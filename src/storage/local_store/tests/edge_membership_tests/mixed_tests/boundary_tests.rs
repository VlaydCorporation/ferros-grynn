//! Mixed lifecycle preserves all generations across one shared Hot page pass.

use super::*;

/// New pages, old pages, CASCADE, reuse and transient slots share one commit.
#[test]
fn mixed_membership_sync_boundaries_cascade_and_reuse_match_bulk() {
	boundaries(IoMode::Sync);
}

/// NativeAsync remains selected through both boundary variants and reopen.
#[cfg(feature = "native-async-io")]
#[test]
fn mixed_membership_native_async_boundaries_cascade_and_reuse_match_bulk() {
	boundaries(IoMode::NativeAsync);
}

/// Empty and 101-slot sources exercise both same-page merging and page append.
fn boundaries(io_mode: IoMode) {
	for existing in [0, 99] {
		let (mut fixture, records) = mixed_fixture(io_mode, existing);
		let files = local(&fixture.store).state.lock().file_catalog.files.clone();
		let mut old = fixture.store.begin();
		let old_snapshot = old.query_snapshot().assert_ok();
		let old_pairs = pairs(&old, &old_snapshot, &fixture, fixture.nodes[0]);
		let mut append = fixture.store.begin();
		let first =
			append.attach_node(fixture.graph.id(), &records[existing], None, None).assert_ok();
		let second =
			append.attach_node(fixture.graph.id(), &records[existing + 1], None, None).assert_ok();
		assert_eq!(
			(first.slot(), second.slot()),
			(u64::try_from(existing + 2).assert_ok(), u64::try_from(existing + 3).assert_ok())
		);
		let topologies = |node| {
			[
				EdgeTopology::directed(std::iter::repeat_n(node, 5), [fixture.nodes[0]]),
				EdgeTopology::undirected([node, second, fixture.nodes[1]]),
				EdgeTopology::bidirectional([node], [node]),
				EdgeTopology::directed([second], [node]),
			]
		};
		for (index, topology) in topologies(first).into_iter().enumerate() {
			append
				.attach_edge(
					fixture.graph.id(),
					&fixture.records[index],
					None,
					topology,
					None,
					None,
				)
				.assert_ok();
		}
		append.commit().assert_ok();
		assert_bulk_image(&fixture);
		assert_fixed_data(&fixture, &files);
		let mut retained = fixture.store.begin();
		let retained_snapshot = retained.query_snapshot().assert_ok();
		// NODE frontier expansion deliberately falls back for n-ary EDGE roles.
		assert!(
			retained
				.query_physical_traversal(
					&retained_snapshot,
					&fixture.graph,
					first,
					true,
					false,
					false
				)
				.assert_ok()
				.is_none()
		);
		let retained_pairs = pairs(&retained, &retained_snapshot, &fixture, second);
		assert!(!retained_pairs.is_empty());
		let mut reuse = fixture.store.begin();
		reuse.detach_occurrence(fixture.graph.id(), &records[existing], true).assert_ok();
		let replacement =
			reuse.attach_node(fixture.graph.id(), &records[existing], None, None).assert_ok();
		assert_eq!((replacement.slot(), replacement.generation()), (first.slot(), 1));
		let mut reused_slots = BTreeSet::new();
		for (index, topology) in topologies(replacement).into_iter().enumerate() {
			let edge = reuse
				.attach_edge(
					fixture.graph.id(),
					&fixture.records[index],
					None,
					topology,
					None,
					None,
				)
				.assert_ok();
			assert_eq!(edge.generation(), 1);
			assert!(reused_slots.insert(edge.slot()));
		}
		// Reuse order belongs to the allocator; every freed slot must be
		// reused.
		assert_eq!(reused_slots, (0..4_u64).collect::<BTreeSet<_>>());
		reuse.commit().assert_ok();
		assert_bulk_image(&fixture);
		assert_fixed_data(&fixture, &files);
		assert_eq!(pairs(&retained, &retained_snapshot, &fixture, second), retained_pairs);
		let mut detach = fixture.store.begin();
		detach.detach_occurrence(fixture.graph.id(), &records[existing], true).assert_ok();
		detach.commit().assert_ok();
		assert_bulk_image(&fixture);
		assert_fixed_data(&fixture, &files);
		let mut transient = fixture.store.begin();
		let temporary = transient
			.attach_node(fixture.graph.id(), &records[existing + 2], None, None)
			.assert_ok();
		assert_eq!((temporary.slot(), temporary.generation()), (first.slot(), 2));
		transient
			.attach_edge(
				fixture.graph.id(),
				&fixture.records[3],
				None,
				EdgeTopology::directed([temporary], [second]),
				None,
				None,
			)
			.assert_ok();
		transient.detach_occurrence(fixture.graph.id(), &records[existing + 2], true).assert_ok();
		transient.commit().assert_ok();
		assert_bulk_image(&fixture);
		assert_fixed_data(&fixture, &files);
		fixture.store.checkpoint(CheckpointMode::Passive).assert_ok();
		assert_eq!(pairs(&old, &old_snapshot, &fixture, fixture.nodes[0]), old_pairs);
		assert_eq!(pairs(&retained, &retained_snapshot, &fixture, second), retained_pairs);
		assert_bulk_image(&fixture);
		drop(old_snapshot);
		drop(old);
		drop(retained_snapshot);
		drop(retained);
		fixture.store.checkpoint(CheckpointMode::Restart).assert_ok();
		fixture = reopen(fixture);
		assert_bulk_image(&fixture);
		assert_fixed_data(&fixture, &files);
	}
}

/// Full replay remaps both allocations and the EDGE's new NODE endpoint.
#[test]
fn mixed_membership_late_writer_remaps_node_edge_and_topology() {
	for io_mode in [IoMode::Sync, IoMode::NativeAsync] {
		if io_mode == IoMode::NativeAsync && !cfg!(feature = "native-async-io") {
			continue;
		}
		let (mut fixture, records) = mixed_fixture(io_mode, 0);
		let files = local(&fixture.store).state.lock().file_catalog.files.clone();
		let mut late = fixture.store.begin();
		let private_node =
			late.attach_node(fixture.graph.id(), &records[0], None, None).assert_ok();
		let private_edge = late
			.attach_edge(
				fixture.graph.id(),
				&fixture.records[0],
				None,
				EdgeTopology::directed([private_node], [fixture.nodes[0]]),
				None,
				None,
			)
			.assert_ok();
		assert_eq!((private_node.slot(), private_edge.slot()), (2, 0));
		mixed_candidate(&fixture, &records[1], 1).commit().assert_ok();
		late.commit().assert_ok();
		assert_replayed_topology(&fixture, &records[0]);
		assert_bulk_image(&fixture);
		assert_fixed_data(&fixture, &files);
		fixture.store.checkpoint(CheckpointMode::Restart).assert_ok();
		fixture = reopen(fixture);
		assert_replayed_topology(&fixture, &records[0]);
		assert_bulk_image(&fixture);
		assert_fixed_data(&fixture, &files);
	}
}

/// Bulk images alone cannot detect a valid but wrongly remapped logical
/// endpoint.
fn assert_replayed_topology(fixture: &Fixture, record: &RecordId) {
	let root = fixture.store.inner.current_root();
	let node =
		root.membership(fixture.graph.id(), root.resolve_atom(record).assert_some()).assert_some();
	let edge = root
		.membership(fixture.graph.id(), root.resolve_atom(&fixture.records[0]).assert_some())
		.assert_some();
	assert_eq!((node.slot(), edge.slot()), (3, 1));
	let expected = EdgeTopology::directed([node], [fixture.nodes[0]]);
	assert!(matches!(root.occurrence(fixture.graph.id(), edge).assert_some().state().view(),
		crate::datastore::OccurrenceStateRef::Edge { topology, .. } if *topology == expected));
}
