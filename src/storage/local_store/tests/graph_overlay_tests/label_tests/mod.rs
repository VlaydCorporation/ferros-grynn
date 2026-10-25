//! Physical label postings must match the accepted root, not only decode.

use std::{collections::BTreeMap, sync::Arc};

use super::*;
use crate::{
	datastore::{DataSpaceId, DatabaseRoot, OccurrenceState},
	graph::{AtomId, GraphId, GraphRef},
	storage::{
		catalog::FileCatalogEntry,
		file_registry::RegisteredDataFile,
		graph_index::{LabelDictionary, LabelKind, build_label_index, visit_label_index_pages},
		local_store::LocalStore,
		scoped_index_image::local_address,
		types::DataFileKind,
	},
	values::storage::DiskAtomRef,
};

/// Namespace transitions, selected-file failures and resource admission.
mod namespace_publication_tests;
/// Namespace shifts preserve surviving bulk distributions and old owners.
mod namespace_tests;

/// Shared existing-slot fixture with separate Node and Edge label namespaces.
struct LabelFixture {
	/// Closes files before the directory field is dropped.
	store: Datastore,
	/// Mode required for the cold-open checks.
	options: LocalOptions,
	/// Graph whose label files are changed.
	graph: GraphRef,
	/// Indexed value and unindexed payload keys.
	keys: [PropertyKeyId; 2],
	/// Three nodes followed by two edges.
	records: [RecordId; 5],
	/// Existing graph-local node slots.
	nodes: [AtomId; 3],
	/// Must outlive every database handle above.
	directory: TestDirectory,
}

/// Sync swaps must preserve unrelated physical file identities.
#[test]
fn label_point_sync_swap_preserves_files_postings_and_reopens() {
	stable_label_publication(IoMode::Sync);
}

/// Actual NativeAsync publishes the same complete label/overlay/weight image.
#[cfg(feature = "native-async-io")]
#[test]
fn label_point_native_async_swap_preserves_files_postings_and_reopens() {
	stable_label_publication(IoMode::NativeAsync);
}

/// Reuses the scalar failure fixture and adds a second live name per namespace.
fn label_fixture(io_mode: IoMode) -> LabelFixture {
	let (directory, store, graph, keys, nodes) = overlay_failure_fixture(io_mode);
	let records = [
		nodes[0].clone(),
		nodes[1].clone(),
		"item:c".parse().assert_ok(),
		"link:a".parse().assert_ok(),
		"link:b".parse().assert_ok(),
	];
	let mut seed = store.begin();
	seed.define_class(ClassSchema::new("link", AtomKind::Edge, []).assert_ok()).assert_ok();
	let mut properties = PropertyMap::new();
	properties.insert(keys[0], 2_i64);
	properties.insert(keys[1], "base");
	seed.create_atom(records[2].clone(), properties).assert_ok();
	seed.attach_node(graph.id(), &records[2], Some("M".into()), None).assert_ok();
	let nodes: [AtomId; 3] = records[..3]
		.iter()
		.map(|record| seed.membership(graph.id(), record).assert_ok().assert_some())
		.collect::<Vec<_>>()
		.try_into()
		.assert_ok();
	let topology = EdgeTopology::directed([nodes[0]], [nodes[1]]);
	for (record, label) in records[3..].iter().zip(["N", "M"]) {
		seed.create_atom(record.clone(), PropertyMap::new()).assert_ok();
		seed.attach_edge(graph.id(), record, Some(label.into()), topology.clone(), None, None)
			.assert_ok();
	}
	// A third name proves the publisher does not replace unrelated label
	// arrays.
	let untouched = "item:untouched".parse::<RecordId>().assert_ok();
	let mut properties = PropertyMap::new();
	properties.insert(keys[0], 3_i64);
	properties.insert(keys[1], "base");
	seed.create_atom(untouched.clone(), properties).assert_ok();
	seed.attach_node(graph.id(), &untouched, Some("Z".into()), None).assert_ok();
	let untouched = "link:untouched".parse::<RecordId>().assert_ok();
	seed.create_atom(untouched.clone(), PropertyMap::new()).assert_ok();
	seed.attach_edge(graph.id(), &untouched, Some("Z".into()), topology, None, None).assert_ok();
	seed.commit().assert_ok();
	store.checkpoint(CheckpointMode::Restart).assert_ok();
	drop(store);
	let options = LocalOptions {
		io_mode,
		buffer_pool_pages: 64,
		..LocalOptions::default()
	};
	let store = Datastore::open_local(&directory.0, options).assert_ok();
	assert_eq!(store.local_storage_status().assert_some().options().io_mode, io_mode);
	store.inner.maintenance.shutdown();
	LabelFixture {
		store,
		options,
		graph,
		keys,
		records,
		nodes,
		directory,
	}
}

/// Combines both namespace swaps with scalar presence and edge weight changes.
fn label_candidate(fixture: &LabelFixture) -> crate::datastore::Transaction {
	let mut write = fixture.store.begin();
	for (offset, label) in [(0, "M"), (2, "N"), (3, "M"), (4, "N")] {
		write
			.set_occurrence_label(fixture.graph.id(), &fixture.records[offset], Some(label.into()))
			.assert_ok();
	}
	write
		.set_overlay(fixture.graph.id(), &fixture.records[0], fixture.keys[0], 100.into())
		.assert_ok();
	write.hide_property(fixture.graph.id(), &fixture.records[2], fixture.keys[1]).assert_ok();
	write
		.replace_occurrence_state(
			fixture.graph.id(),
			&fixture.records[3],
			OccurrenceState::edge(
				EdgeTopology::directed([fixture.nodes[0]], [fixture.nodes[1]]),
				Some(3_i64.into()),
				None,
			),
		)
		.assert_ok();
	write
}

/// Reads physical immutable arrays and compares every slot with logical
/// postings.
pub(in super::super) fn assert_label_arrays(store: &Datastore, graph: GraphId) {
	let root = store.inner.current_root();
	let graph_root = root.graph(graph).assert_some();
	let labels = LabelDictionary::from_labels(graph_root.occurrences().filter_map(|occurrence| {
		let kind = match occurrence.local_id().kind()? {
			AtomKind::Node => LabelKind::Node,
			AtomKind::Edge => LabelKind::Edge,
		};
		Some((kind, occurrence.label()?.as_str()))
	}))
	.assert_ok();
	let local = local_store(store);
	let state = local.state.lock();
	let mut names = BTreeMap::new();
	for ((kind, name), atoms) in &root.indexes.graphs[&graph].labels {
		let namespace = match kind {
			AtomKind::Node => LabelKind::Node,
			AtomKind::Edge => LabelKind::Edge,
		};
		let id = labels.id(namespace, name.as_str()).assert_some();
		let kind_name = match kind {
			AtomKind::Node => "node",
			AtomKind::Edge => "edge",
		};
		let suffix = format!("/idx/lbl_{kind_name}_{id}.fgidx");
		let file = state
			.file_catalog
			.files
			.iter()
			.find(|entry| {
				entry.space_id == DataSpaceId::Graph(graph)
					&& entry.path.as_str().ends_with(&suffix)
			})
			.assert_some();
		assert!(names.insert(file.file_id, ()).is_none());
		let mut expected =
			atoms.iter().map(|atom| local_address(*atom).assert_ok()).collect::<Vec<_>>();
		expected.sort_unstable_by_key(|atom| atom.slot());
		let mut actual = Vec::new();
		visit_label_index_pages::<DiskAtomRef>(
			|number, page| {
				local.read_update_page_into(
					&state,
					crate::storage::types::PageKey::new(file.space_id, file.file_id, number),
					page,
				)
			},
			*kind,
			|atom| {
				actual.push(atom);
				Ok(())
			},
		)
		.assert_ok();
		assert_eq!(actual, expected);
	}
	let count = state
		.file_catalog
		.files
		.iter()
		.filter(|entry| {
			entry.space_id == DataSpaceId::Graph(graph) && entry.path.as_str().contains("/idx/lbl_")
		})
		.count();
	assert_eq!(count, names.len());
}

/// Selects the existing local backend without silently accepting another mode.
fn local_store(store: &Datastore) -> &LocalStore {
	match &store.inner.backend {
		StoreBackend::Local(local) => Some(local),
		StoreBackend::InMemory => None,
	}
	.assert_some()
}

/// Uses the root independently of its physical array oracle.
fn assert_labels(root: &DatabaseRoot, fixture: &LabelFixture, swapped: bool) {
	let labels = if swapped {
		["M", "N", "N", "M", "N"]
	} else {
		["N", "N", "M", "N", "M"]
	};
	for (record, label) in fixture.records.iter().zip(labels) {
		let global = root.resolve_atom(record).assert_some();
		let local = root.membership(fixture.graph.id(), global).assert_some();
		assert_eq!(
			root.occurrence(fixture.graph.id(), local)
				.assert_some()
				.label()
				.map(|value| value.as_str()),
			Some(label)
		);
	}
}

/// Only affected label arrays may receive new files in a stable namespace.
fn assert_unaffected_files(store: &Datastore, graph: GraphId, before: &[FileCatalogEntry]) {
	let local = local_store(store);
	let state = local.state.lock();
	for entry in before {
		if entry.space_id == DataSpaceId::Graph(graph)
			&& entry.file_id.kind() == Some(DataFileKind::Index)
			&& ["node", "edge"].iter().any(|kind| {
				[0, 1]
					.iter()
					.any(|id| entry.path.as_str().ends_with(&format!("/idx/lbl_{kind}_{id}.fgidx")))
			}) {
			continue;
		}
		assert!(
			state.file_catalog.files.contains(entry),
			"unrelated file was replaced: {:?}",
			entry.path
		);
	}
}

/// Exercises final swaps, exact counters, old roots and a durable cold reopen.
fn stable_label_publication(io_mode: IoMode) {
	let mut fixture = label_fixture(io_mode);
	assert_label_arrays(&fixture.store, fixture.graph.id());
	let old = fixture.store.inner.current_root();
	let old_arrays = capture_label_arrays(&fixture.store, fixture.graph.id());
	let old_pin =
		local_store(&fixture.store).capture_query_snapshot(old.commit_ts).assert_ok().assert_some();
	let files = local_store(&fixture.store).state.lock().file_catalog.files.clone();
	label_candidate(&fixture).commit().assert_ok();
	assert_unaffected_files(&fixture.store, fixture.graph.id(), &files);
	assert_labels(&fixture.store.inner.current_root(), &fixture, true);
	assert_labels(&old, &fixture, false);
	assert_label_arrays(&fixture.store, fixture.graph.id());
	assert_overlay_label_statistic(&fixture.store, fixture.graph.id(), AtomKind::Node, "M", 1, 2);
	assert_overlay_label_statistic(&fixture.store, fixture.graph.id(), AtomKind::Node, "N", 2, 3);
	// Removing a label remains sparse while another occurrence keeps its name
	// live.
	let mut none = fixture.store.begin();
	none.set_occurrence_label(fixture.graph.id(), &fixture.records[2], None).assert_ok();
	none.commit().assert_ok();
	assert_unaffected_files(&fixture.store, fixture.graph.id(), &files);
	assert_label_arrays(&fixture.store, fixture.graph.id());
	assert_overlay_label_statistic(&fixture.store, fixture.graph.id(), AtomKind::Node, "N", 1, 2);
	let mut some = fixture.store.begin();
	some.set_occurrence_label(fixture.graph.id(), &fixture.records[2], Some("N".into()))
		.assert_ok();
	some.commit().assert_ok();
	for swapped in [false, true] {
		let mut write = fixture.store.begin();
		let names = if swapped {
			["M", "N", "M", "N"]
		} else {
			["N", "M", "N", "M"]
		};
		for (record, name) in [0, 2, 3, 4].into_iter().zip(names) {
			write
				.set_occurrence_label(
					fixture.graph.id(),
					&fixture.records[record],
					Some(name.into()),
				)
				.assert_ok();
		}
		write.commit().assert_ok();
		assert_labels(&fixture.store.inner.current_root(), &fixture, swapped);
		assert_label_arrays(&fixture.store, fixture.graph.id());
		assert_unaffected_files(&fixture.store, fixture.graph.id(), &files);
	}
	fixture.store.checkpoint(CheckpointMode::Passive).assert_ok();
	for array in &old_arrays {
		array.assert_unchanged();
	}
	assert_labels(&old, &fixture, false);
	drop(old_arrays);
	drop(old_pin);
	drop(old);
	drop(fixture.store);
	fixture.store = Datastore::open_local(&fixture.directory.0, fixture.options).assert_ok();
	assert_eq!(fixture.store.local_storage_status().assert_some().options().io_mode, io_mode);
	assert_unaffected_files(&fixture.store, fixture.graph.id(), &files);
	assert_labels(&fixture.store.inner.current_root(), &fixture, true);
	assert_label_arrays(&fixture.store, fixture.graph.id());
	assert_overlay_label_statistic(&fixture.store, fixture.graph.id(), AtomKind::Node, "M", 1, 2);
	assert_overlay_label_statistic(&fixture.store, fixture.graph.id(), AtomKind::Node, "N", 2, 3);
}

/// Exact old file owner remains usable after selected catalog retirement.
struct LabelArraySnapshot {
	/// Direct-I/O owner pins the immutable retired bytes.
	file: Arc<RegisteredDataFile>,
	/// Namespace checked by the array codec.
	kind: AtomKind,
	/// Small test oracle independent of the new runtime lookup.
	atoms: Vec<DiskAtomRef>,
}

impl LabelArraySnapshot {
	/// Reads the old home file after checkpoint and compares the full stream.
	fn assert_unchanged(&self) {
		let mut actual = Vec::new();
		visit_label_index_pages::<DiskAtomRef>(
			|number, page| self.file.file()?.read_page_raw(number, page),
			self.kind,
			|atom| {
				actual.push(atom);
				Ok(())
			},
		)
		.assert_ok();
		assert_eq!(actual, self.atoms);
	}
}

/// Captures real file owners rather than only immutable logical roots.
fn capture_label_arrays(store: &Datastore, graph: GraphId) -> Vec<LabelArraySnapshot> {
	let local = local_store(store);
	let state = local.state.lock();
	state
		.file_catalog
		.files
		.iter()
		.filter(|entry| {
			entry.space_id == DataSpaceId::Graph(graph) && entry.path.as_str().contains("/idx/lbl_")
		})
		.map(|entry| {
			let kind = if entry.path.as_str().contains("/idx/lbl_node_") {
				AtomKind::Node
			} else {
				AtomKind::Edge
			};
			let file = state.registry.resolve_file(entry.space_id, entry.file_id).assert_ok();
			let mut atoms = Vec::new();
			visit_label_index_pages::<DiskAtomRef>(
				|number, page| file.file()?.read_page_raw(number, page),
				kind,
				|atom| {
					atoms.push(atom);
					Ok(())
				},
			)
			.assert_ok();
			LabelArraySnapshot {
				file,
				kind,
				atoms,
			}
		})
		.collect()
}

/// Well-formed secondary arrays cannot publish false or omitted source
/// postings.
#[test]
fn label_point_checksum_valid_source_mismatch_publishes_nothing() {
	for omitted in [false, true] {
		let mut fixture = label_fixture(IoMode::Sync);
		let local = local_store(&fixture.store);
		let state = local.state.lock();
		let entry = state
			.file_catalog
			.files
			.iter()
			.find(|entry| {
				entry.space_id == DataSpaceId::Graph(fixture.graph.id())
					&& entry.path.as_str().ends_with("/idx/lbl_node_1.fgidx")
			})
			.assert_some();
		let wrong = if omitted {
			Vec::new()
		} else {
			vec![local_address(fixture.nodes[2]).assert_ok()]
		};
		let pages = build_label_index(&wrong, AtomKind::Node, crate::storage::page::PageLsn::ZERO)
			.assert_ok();
		state
			.registry
			.resolve_file(entry.space_id, entry.file_id)
			.assert_ok()
			.file()
			.assert_ok()
			.write_page_raw(crate::storage::page::PageNo::new(0), &pages[0])
			.assert_ok();
		drop(state);
		// Reopen discards resident pages so validation sees the corrupted
		// secondary.
		drop(fixture.store);
		fixture.store = Datastore::open_local(&fixture.directory.0, fixture.options).assert_ok();
		fixture.store.inner.maintenance.shutdown();
		let stamp = fixture.store.inner.current_root().commit_ts;
		let boundary = local_store(&fixture.store).wal.snapshot().visible_lsn();
		let files = local_store(&fixture.store).state.lock().file_catalog.files.clone();
		label_candidate(&fixture).commit().assert_err();
		assert_eq!(fixture.store.inner.current_root().commit_ts, stamp);
		assert_eq!(local_store(&fixture.store).wal.snapshot().visible_lsn(), boundary);
		assert_eq!(local_store(&fixture.store).state.lock().file_catalog.files, files);
		assert_labels(&fixture.store.inner.current_root(), &fixture, false);
	}
}

/// Sync failures retain either the complete old or recovered new label image.
#[test]
fn label_point_sync_failures_recover_complete_selected_files() {
	label_failures(IoMode::Sync);
}

/// Actual NativeAsync shares the selected-file uncertainty fence.
#[cfg(feature = "native-async-io")]
#[test]
fn label_point_native_async_failures_recover_complete_selected_files() {
	label_failures(IoMode::NativeAsync);
}

/// Checks all three coupled projections after failure or recovery.
fn assert_label_image(fixture: &LabelFixture, published: bool) {
	assert_labels(&fixture.store.inner.current_root(), fixture, published);
	assert_label_arrays(&fixture.store, fixture.graph.id());
	assert_overlay_label_statistic(&fixture.store, fixture.graph.id(), AtomKind::Node, "M", 1, 2);
	assert_overlay_label_statistic(
		&fixture.store,
		fixture.graph.id(),
		AtomKind::Node,
		"N",
		2,
		if published {
			3
		} else {
			4
		},
	);
}

/// Known failures remove unpublished files; uncertain WAL is recovered on
/// reopen.
fn label_failures(io_mode: IoMode) {
	use crate::storage::wal::{CommitFailureGuard, CommitFailurePoint};
	{
		let fixture = label_fixture(io_mode);
		let local = local_store(&fixture.store);
		let stamp = fixture.store.inner.current_root().commit_ts;
		let boundary = local.wal.snapshot().visible_lsn();
		let files = local.state.lock().file_catalog.files.clone();
		let generations = generation_directories(&fixture.directory.0);
		let header = local.state.lock().graph_superblocks[&fixture.graph.id()];
		for point in [
			GenerationFailurePoint::AfterBuild,
			GenerationFailurePoint::AfterFileSync,
			GenerationFailurePoint::AfterCatalogStage,
			GenerationFailurePoint::BeforeWalCommit,
		] {
			let write = label_candidate(&fixture);
			{
				let _failure = GenerationFailureGuard::set(point);
				write.commit().assert_err();
			}
			assert_eq!(fixture.store.inner.current_root().commit_ts, stamp);
			assert_eq!(local.wal.snapshot().visible_lsn(), boundary);
			assert_eq!(local.state.lock().file_catalog.files, files);
			assert_eq!(local.state.lock().graph_superblocks[&fixture.graph.id()], header);
			assert_eq!(generation_directories(&fixture.directory.0), generations);
			assert_label_image(&fixture, false);
		}
		label_candidate(&fixture).commit().assert_ok();
		assert_unaffected_files(&fixture.store, fixture.graph.id(), &files);
		assert_label_image(&fixture, true);
	}
	for point in [
		CommitFailurePoint::BeforeAppend,
		CommitFailurePoint::AfterAppend,
		CommitFailurePoint::AfterFlush,
		CommitFailurePoint::AfterSync,
	] {
		let mut fixture = label_fixture(io_mode);
		let stamp = fixture.store.inner.current_root().commit_ts;
		let files = local_store(&fixture.store).state.lock().file_catalog.files.clone();
		let write = label_candidate(&fixture);
		{
			let _failure = CommitFailureGuard::set(point);
			write.commit().assert_err();
		}
		assert_eq!(fixture.store.inner.current_root().commit_ts, stamp);
		assert_eq!(local_store(&fixture.store).state.lock().file_catalog.files, files);
		assert_label_image(&fixture, false);
		label_candidate(&fixture).commit().assert_err();
		drop(fixture.store);
		fixture.store = Datastore::open_local(&fixture.directory.0, fixture.options).assert_ok();
		assert_eq!(fixture.store.local_storage_status().assert_some().options().io_mode, io_mode);
		let published = point != CommitFailurePoint::BeforeAppend;
		assert_eq!(fixture.store.inner.current_root().commit_ts, stamp + u64::from(published));
		assert_unaffected_files(&fixture.store, fixture.graph.id(), &files);
		assert_label_image(&fixture, published);
		fixture.store.checkpoint(CheckpointMode::Restart).assert_ok();
		drop(fixture.store);
		fixture.store = Datastore::open_local(&fixture.directory.0, fixture.options).assert_ok();
		assert_eq!(fixture.store.local_storage_status().assert_some().options().io_mode, io_mode);
		assert_label_image(&fixture, published);
	}
}

/// Final replay can retain a label name removed by the private candidate.
#[test]
fn label_point_replay_uses_final_memberships_after_independent_commit() {
	let fixture = label_fixture(IoMode::Sync);
	let files = local_store(&fixture.store).state.lock().file_catalog.files.clone();
	let mut first = fixture.store.begin();
	let mut second = fixture.store.begin();
	first
		.set_occurrence_label(fixture.graph.id(), &fixture.records[0], Some("M".into()))
		.assert_ok();
	second
		.set_occurrence_label(fixture.graph.id(), &fixture.records[2], Some("N".into()))
		.assert_ok();
	first.commit().assert_ok();
	second.commit().assert_ok();
	assert_unaffected_files(&fixture.store, fixture.graph.id(), &files);
	assert_label_arrays(&fixture.store, fixture.graph.id());
	assert_overlay_label_statistic(&fixture.store, fixture.graph.id(), AtomKind::Node, "M", 1, 2);
	assert_overlay_label_statistic(&fixture.store, fixture.graph.id(), AtomKind::Node, "N", 2, 4);
}
