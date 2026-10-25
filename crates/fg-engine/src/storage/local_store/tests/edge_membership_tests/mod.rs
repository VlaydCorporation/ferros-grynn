//! Ordinary EDGE membership publication keeps unrelated generation identities.
/// Full bulk-image oracle across slot, multiplicity and overflow boundaries.
mod boundary_tests;
/// Direct/shared parent directory pages share the child WAL publication.
mod child_directory_tests;
/// Child membership growth also preserves every parent directory.
mod child_graph_tests;
/// Existing local EDGE endpoint capabilities use selected physical publication.
mod endpoint_publication_tests;
/// Plain graph families retain physical identities across final replay.
mod family_tests;
/// Attached global EDGE lifecycle shares Base and incidence publication.
mod global_lifecycle_tests;
/// Combined NODE and EDGE slots must share one physical page publisher.
mod mixed_tests;
/// Known rejection, uncertain WAL recovery and malformed physical sources.
mod recovery_tests;
/// Retained old multi-page EDGE roles survive shrink and advancing backfill.
mod rewire_pin_tests;
/// Late replay, resource rejection and publication failures retain accepted
/// images.
mod rewire_recovery_tests;
/// Checksummed old EDGE endpoint faults cannot become publication fallback.
mod rewire_source_tests;
/// Same-owner topology replacement retains the existing incidence files.
mod rewire_tests;
/// Shared seeded sources and an independent bulk image oracle.
mod support;

use std::collections::BTreeSet;

use super::*;
use crate::{
	datastore::{DataSpaceId, IoMode, QuerySnapshot, Transaction},
	graph::{AtomId, GraphId, GraphRef},
	storage::{
		file_registry::DataFileRole,
		local_store::LocalStore,
		page::PageNo,
		types::{DataFileKind, PageKey},
	},
};

/// Attaching an existing Base edge changes only its graph slice.
#[test]
fn edge_membership_sync_attach_retains_unrelated_files() {
	edge_membership(IoMode::Sync, false);
}
/// Detaching an edge leaves its other occurrence and Base owner intact.
#[test]
fn edge_membership_sync_detach_retains_unrelated_files() {
	edge_membership(IoMode::Sync, true);
}
/// NativeAsync remains selected for an attach and cold reopen.
#[cfg(feature = "native-async-io")]
#[test]
fn edge_membership_native_async_attach_retains_unrelated_files() {
	edge_membership(IoMode::NativeAsync, false);
}
/// NativeAsync detachment preserves the independent graph occurrence.
#[cfg(feature = "native-async-io")]
#[test]
fn edge_membership_native_async_detach_retains_unrelated_files() {
	edge_membership(IoMode::NativeAsync, true);
}

/// Characterizes both sides of a standalone graph's ordinary EDGE membership.
fn edge_membership(io_mode: IoMode, detach: bool) {
	let directory = TestDirectory::new();
	let options = LocalOptions {
		io_mode,
		..LocalOptions::default()
	};
	let store = Datastore::create_local(&directory.0, options).assert_ok();
	assert_eq!(store.local_storage_status().assert_some().options().io_mode, io_mode);
	let mut seed = store.begin();
	seed.define_class(ClassSchema::new("person", AtomKind::Node, []).assert_ok()).assert_ok();
	seed.define_class(ClassSchema::new("link", AtomKind::Edge, []).assert_ok()).assert_ok();
	let graph = seed.define_graph("g", GraphKind::Graph).assert_ok();
	let other = seed.define_graph("other", GraphKind::Graph).assert_ok();
	let records = ["person:a", "person:b", "link:ab"].map(|s| s.parse::<RecordId>().assert_ok());
	for record in &records {
		seed.create_atom(record.clone(), PropertyMap::new()).assert_ok();
	}
	let nodes = records[..2]
		.iter()
		.map(|record| seed.attach_node(graph.id(), record, None, None).assert_ok())
		.collect::<Vec<_>>();
	let other_nodes = records[..2]
		.iter()
		.map(|record| seed.attach_node(other.id(), record, None, None).assert_ok())
		.collect::<Vec<_>>();
	seed.attach_edge(
		other.id(),
		&records[2],
		None,
		EdgeTopology::directed([other_nodes[0]], [other_nodes[1]]),
		None,
		None,
	)
	.assert_ok();
	if detach {
		seed.attach_edge(
			graph.id(),
			&records[2],
			None,
			EdgeTopology::directed([nodes[0]], [nodes[1]]),
			None,
			None,
		)
		.assert_ok();
	}
	seed.commit().assert_ok();
	store.checkpoint(CheckpointMode::Restart).assert_ok();
	drop(store);
	let store = Datastore::open_local(&directory.0, options).assert_ok();
	assert_eq!(store.local_storage_status().assert_some().options().io_mode, io_mode);
	store.inner.maintenance.shutdown();
	let local = match &store.inner.backend {
		StoreBackend::Local(local) => Some(local),
		StoreBackend::InMemory => None,
	}
	.assert_some();
	let files = local.state.lock().file_catalog.files.clone();
	let (watched, expected_pages) = watch_data_pages(local, graph.id(), Some(other.id()));
	let boundary = local.wal.snapshot().visible_lsn();
	let old = store.inner.current_root();
	let mut old_read = store.begin();
	let old_snapshot = old_read.query_snapshot().assert_ok();
	let global = old.resolve_atom(&records[2]).assert_some();
	assert_eq!(old.membership(graph.id(), global).is_some(), detach);
	assert_adjacency(&old_read, &old_snapshot, &graph, &nodes, old.membership(graph.id(), global));
	let mut write = store.begin();
	if detach {
		write.detach_occurrence(graph.id(), &records[2], false).assert_ok();
	} else {
		write
			.attach_edge(
				graph.id(),
				&records[2],
				None,
				EdgeTopology::directed([nodes[0]], [nodes[1]]),
				None,
				None,
			)
			.assert_ok();
	}
	write.commit().assert_ok();
	let changed_pages = {
		let frames = local.wal.visible_frames(&watched, local.wal.snapshot());
		frames
			.frames()
			.iter()
			.flatten()
			.filter(|frame| frame.frame_lsn() > boundary)
			.map(|frame| frame.key())
			.collect::<BTreeSet<_>>()
	};
	assert_eq!(changed_pages, expected_pages);
	assert_adjacency(&old_read, &old_snapshot, &graph, &nodes, old.membership(graph.id(), global));
	let mut current_read = store.begin();
	let current_snapshot = current_read.query_snapshot().assert_ok();
	assert_adjacency(
		&current_read,
		&current_snapshot,
		&graph,
		&nodes,
		store.inner.current_root().membership(graph.id(), global),
	);
	store.checkpoint(CheckpointMode::Passive).assert_ok();
	assert_adjacency(&old_read, &old_snapshot, &graph, &nodes, old.membership(graph.id(), global));
	assert_adjacency(
		&current_read,
		&current_snapshot,
		&graph,
		&nodes,
		store.inner.current_root().membership(graph.id(), global),
	);
	drop(current_snapshot);
	drop(current_read);
	for entry in &files {
		let selected = (entry.space_id == DataSpaceId::Graph(graph.id())
			&& matches!(
				entry.file_id.kind(),
				Some(DataFileKind::Index | DataFileKind::Statistics)
			))
			|| (entry.space_id == DataSpaceId::Database
				&& entry.file_id.kind() == Some(DataFileKind::Index)
				&& entry.path.as_str().ends_with("/memberships.fgidx"));
		if !selected {
			assert!(
				local.state.lock().file_catalog.files.contains(entry),
				"edge membership rebuilt unrelated file: {}",
				entry.path.as_str()
			);
		}
	}
	assert_eq!(old.membership(graph.id(), global).is_some(), detach);
	assert_eq!(store.inner.current_root().membership(graph.id(), global).is_some(), !detach);
	assert!(store.inner.current_root().membership(other.id(), global).is_some());
	drop(old_snapshot);
	drop(old_read);
	drop(old);
	store.checkpoint(CheckpointMode::Restart).assert_ok();
	drop(store);
	let store = Datastore::open_local(&directory.0, options).assert_ok();
	assert_eq!(store.local_storage_status().assert_some().options().io_mode, io_mode);
	assert_eq!(store.inner.current_root().membership(graph.id(), global).is_some(), !detach);
	assert!(store.inner.current_root().membership(other.id(), global).is_some());
	let mut read = store.begin();
	let snapshot = read.query_snapshot().assert_ok();
	assert_adjacency(
		&read,
		&snapshot,
		&graph,
		&nodes,
		store.inner.current_root().membership(graph.id(), global),
	);
}

/// Watches graph data and all unrelated Base pages; selected membership/index
/// metadata and the database catalog have their own publication obligations.
fn watch_data_pages(
	store: &LocalStore,
	graph: GraphId,
	other: Option<GraphId>,
) -> (Vec<PageKey>, BTreeSet<PageKey>) {
	let state = store.state.lock();
	let target =
		state.bindings.graphs.iter().find(|binding| binding.graph_id == graph).assert_some().files;
	let retained = other.map(|other| {
		state.bindings.graphs.iter().find(|binding| binding.graph_id == other).assert_some().files
	});
	let expected = [target.nodes, target.edges, target.superblock]
		.map(|file| PageKey::new(DataSpaceId::Graph(graph), file, PageNo::new(0)))
		.into_iter()
		.collect::<BTreeSet<_>>();
	let mut keys = expected.clone();
	for entry in &state.file_catalog.files {
		let target_data = entry.space_id == DataSpaceId::Graph(graph)
			&& entry.file_id != target.membership
			&& target.regular_files().contains(&entry.file_id);
		let other_data = other.is_some_and(|other| entry.space_id == DataSpaceId::Graph(other))
			&& retained.is_some_and(|retained| {
				retained.regular_files().contains(&entry.file_id)
					|| entry.file_id == retained.superblock
			});
		let base_data = entry.space_id == DataSpaceId::Database
			&& entry.path.as_str().starts_with("atoms/")
			&& !entry.path.as_str().ends_with("/memberships.fgidx");
		if target_data || other_data || base_data {
			let Some(pages) = watched_page_count(store, &state, entry) else {
				continue;
			};
			for number in 0..pages {
				keys.insert(PageKey::new(entry.space_id, entry.file_id, PageNo::new(number)));
			}
		}
	}
	(keys.into_iter().collect(), expected)
}
/// Only positional WAL files and singleton superblocks have watchable page
/// keys.
fn watched_page_count(
	store: &LocalStore,
	state: &super::super::LocalState,
	entry: &crate::storage::catalog::FileCatalogEntry,
) -> Option<u64> {
	Some(match entry.role {
		DataFileRole::WalBackfilled => {
			store.visible_pages(state, entry.space_id, entry.file_id).assert_ok()
		}
		DataFileRole::Superblock => {
			let file = state.registry.resolve_file(entry.space_id, entry.file_id).assert_ok();
			assert_eq!(file.page_count(), 1);
			1
		}
		DataFileRole::MetadataCow | DataFileRole::AuxiliaryImmutable => return None,
	})
}

/// Both direction axes must use pinned physical pages, never materialized
/// fallback.
fn assert_adjacency(
	read: &Transaction,
	snapshot: &QuerySnapshot,
	graph: &GraphRef,
	nodes: &[AtomId],
	edge: Option<AtomId>,
) {
	for (source, target, forward, backward) in
		[(nodes[0], nodes[1], true, false), (nodes[1], nodes[0], false, true)]
	{
		let physical = read
			.query_physical_traversal(snapshot, graph, source, forward, backward, false)
			.assert_ok()
			.assert_some();
		let actual = physical
			.candidates
			.iter()
			.map(|candidate| (candidate.edge, candidate.target))
			.collect::<Vec<_>>();
		let expected = edge.into_iter().map(|edge| (edge, target)).collect::<Vec<_>>();
		assert_eq!(actual, expected);
	}
}
