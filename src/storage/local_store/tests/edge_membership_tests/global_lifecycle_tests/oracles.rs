//! Physical Hot frames and checkpoint tuples are checked outside the builders.

use super::*;
use crate::{
	storage::{graph_index::visit_label_index_pages, types::Lsn},
	values::storage::DiskAtomRef,
};

/// Class arrays encode local slots, so expected sets are explicit and sorted.
pub(super) fn assert_class_slots(fixture: &support::Fixture, name: &str, expected: &[u64]) {
	assert_class_slots_in(fixture, fixture.graph.id(), name, expected);
}

/// Each graph owns a distinct class array even for the same Base identity.
pub(super) fn assert_class_slots_in(
	fixture: &support::Fixture,
	graph: GraphId,
	name: &str,
	expected: &[u64],
) {
	let root = fixture.store.inner.current_root();
	let class = root.schema.class(name).assert_some();
	let suffix = format!("/idx/cls_{}.fgidx", class.id().assert_some().get());
	let local = support::local(&fixture.store);
	let state = local.state.lock();
	let mut files = state.file_catalog.files.iter().filter(|entry| {
		entry.space_id == DataSpaceId::Graph(graph) && entry.path.as_str().ends_with(&suffix)
	});
	let file = files.next().assert_some().file_id;
	assert!(files.next().is_none());
	let mut actual = Vec::new();
	visit_label_index_pages::<DiskAtomRef>(
		|number, page| {
			local.read_update_page_into(
				&state,
				PageKey::new(DataSpaceId::Graph(graph), file, number),
				page,
			)
		},
		class.kind(),
		|atom| {
			actual.push(atom.slot());
			Ok(())
		},
	)
	.assert_ok();
	assert_eq!(actual, expected);
}

/// One commit stages each key once and touches only its explicit graph domain.
pub(super) fn assert_hot_frames(
	fixture: &support::Fixture,
	boundary: Lsn,
	global_pages: &[u64],
	edge_pages: &[u64],
) {
	assert_hot_frames_including_nodes(fixture, boundary, global_pages, &[0], edge_pages);
}

/// Transient EDGE tombstones must not stage unchanged NODE adjacency pages.
pub(super) fn assert_hot_frames_including_nodes(
	fixture: &support::Fixture,
	boundary: Lsn,
	global_pages: &[u64],
	node_pages: &[u64],
	edge_pages: &[u64],
) {
	let local = support::local(&fixture.store);
	let frames = local
		.wal
		.recovery_frames()
		.into_iter()
		.filter(|frame| frame.frame_lsn() > boundary)
		.collect::<Vec<_>>();
	let keys = frames.iter().map(|frame| frame.key()).collect::<BTreeSet<_>>();
	assert_eq!(keys.len(), frames.len());
	assert!(!frames.is_empty());
	assert!(keys.iter().all(|key| key.space_id() == DataSpaceId::Database
		|| key.space_id() == DataSpaceId::Graph(fixture.graph.id())));
	let state = local.state.lock();
	let atoms = state.bindings.atoms;
	let graph = state
		.bindings
		.graphs
		.iter()
		.find(|binding| binding.graph_id == fixture.graph.id())
		.assert_some()
		.files;
	for (kind, space, file, pages) in [
		(DataFileKind::AtomsHot, DataSpaceId::Database, atoms.atoms_hot, global_pages),
		(DataFileKind::NodesHot, graph.space(), graph.nodes, node_pages),
		(DataFileKind::EdgesHot, graph.space(), graph.edges, edge_pages),
	] {
		let actual = keys
			.iter()
			.filter(|key| key.file_id().kind() == Some(kind))
			.copied()
			.collect::<BTreeSet<_>>();
		let expected = pages
			.iter()
			.map(|page| PageKey::new(space, file, PageNo::new(*page)))
			.collect::<BTreeSet<_>>();
		assert_eq!(actual, expected);
	}
	let header = state.graph_superblocks[&fixture.graph.id()];
	for (generation, backfilled, record) in [
		(header.checkpoint_generation, header.backfilled_lsn, header.checkpoint_record_lsn),
		(
			state.atom_superblock.checkpoint_generation,
			state.atom_superblock.backfilled_lsn,
			state.atom_superblock.checkpoint_record_lsn,
		),
	] {
		assert_eq!(generation, state.database_superblock.generation);
		assert_eq!(backfilled, state.database_superblock.checkpoint_lsn);
		assert_eq!(record, state.database_superblock.checkpoint_record_lsn);
	}
}
