//! Sparse EDGE slots and NODE adjacency agree with an independent bulk image.

use super::{support::*, *};

/// Covers Hot boundaries, slot reuse, transient state and multi-page overflow.
#[test]
fn edge_membership_sync_boundaries_match_bulk_image() {
	boundaries(IoMode::Sync);
}

/// Actual NativeAsync uses the same canonical image through checkpoint/reopen.
#[cfg(feature = "native-async-io")]
#[test]
fn edge_membership_native_async_boundaries_match_bulk_image() {
	boundaries(IoMode::NativeAsync);
}

/// Keeps an old physical traversal pinned while final generations change order.
fn boundaries(io_mode: IoMode) {
	let mut fixture = fixture(io_mode, 107, 101);
	let retained = local(&fixture.store)
		.state
		.lock()
		.file_catalog
		.files
		.iter()
		.filter(|entry| {
			entry.space_id == DataSpaceId::Database
				&& entry.file_id.kind() != Some(DataFileKind::Index)
		})
		.cloned()
		.collect::<Vec<_>>();
	let mut old = fixture.store.begin();
	let snapshot = old.query_snapshot().assert_ok();
	let pinned = physical_pairs(&old, &snapshot, &fixture);
	assert_bulk_image(&fixture);
	let mut append = fixture.store.begin();
	for index in 101..103 {
		let local = append
			.attach_edge(
				fixture.graph.id(),
				&fixture.records[index],
				None,
				EdgeTopology::directed([fixture.nodes[0]], [fixture.nodes[1]]),
				None,
				None,
			)
			.assert_ok();
		assert_eq!((local.slot(), local.generation()), (u64::try_from(index).assert_ok(), 0));
	}
	append.commit().assert_ok();
	assert_bulk_image(&fixture);
	let mut reuse = fixture.store.begin();
	reuse.detach_occurrence(fixture.graph.id(), &fixture.records[0], false).assert_ok();
	let reused = reuse
		.attach_edge(
			fixture.graph.id(),
			&fixture.records[0],
			None,
			EdgeTopology::directed([fixture.nodes[0]], [fixture.nodes[1]]),
			None,
			None,
		)
		.assert_ok();
	assert_eq!((reused.slot(), reused.generation()), (0, 1));
	reuse.commit().assert_ok();
	assert_bulk_image(&fixture);
	let mut transient = candidate(&fixture, 103);
	transient.detach_occurrence(fixture.graph.id(), &fixture.records[103], false).assert_ok();
	transient.commit().assert_ok();
	assert_bulk_image(&fixture);
	let mut special = fixture.store.begin();
	for (index, topology) in [
		(103, EdgeTopology::directed([fixture.nodes[0]], [fixture.nodes[0]])),
		(
			104,
			EdgeTopology::directed(
				std::iter::repeat_n(fixture.nodes[0], 2_050),
				[fixture.nodes[1]],
			),
		),
		(105, EdgeTopology::bidirectional([fixture.nodes[0]], [fixture.nodes[0]])),
		(106, EdgeTopology::undirected(fixture.nodes)),
	] {
		special
			.attach_edge(fixture.graph.id(), &fixture.records[index], None, topology, None, None)
			.assert_ok();
	}
	special.commit().assert_ok();
	assert_bulk_image(&fixture);
	assert_eq!(physical_pairs(&old, &snapshot, &fixture), pinned);
	fixture.store.checkpoint(CheckpointMode::Passive).assert_ok();
	assert_eq!(physical_pairs(&old, &snapshot, &fixture), pinned);
	assert_bulk_image(&fixture);
	let mut shrink = fixture.store.begin();
	shrink.detach_occurrence(fixture.graph.id(), &fixture.records[104], false).assert_ok();
	shrink.commit().assert_ok();
	assert_bulk_image(&fixture);
	assert_eq!(physical_pairs(&old, &snapshot, &fixture), pinned);
	for entry in retained {
		assert!(local(&fixture.store).state.lock().file_catalog.files.contains(&entry));
	}
	drop(snapshot);
	drop(old);
	fixture.store.checkpoint(CheckpointMode::Restart).assert_ok();
	fixture = reopen(fixture);
	assert_bulk_image(&fixture);
	let graph = fixture.store.inner.current_root().graph(fixture.graph.id()).assert_some().clone();
	assert_eq!(graph.slot_capacity(AtomKind::Edge), 107);
}

/// A pinned SDK query must still select exact physical pages after later
/// commits.
fn physical_pairs(
	read: &Transaction,
	snapshot: &QuerySnapshot,
	fixture: &Fixture,
) -> Vec<(AtomId, AtomId)> {
	read.query_physical_traversal(snapshot, &fixture.graph, fixture.nodes[0], true, false, false)
		.assert_ok()
		.assert_some()
		.candidates
		.iter()
		.map(|candidate| (candidate.edge, candidate.target))
		.collect()
}
