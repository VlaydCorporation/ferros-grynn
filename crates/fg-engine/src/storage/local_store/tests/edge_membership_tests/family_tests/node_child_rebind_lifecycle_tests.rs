//! Shared-child, repeated and replayed NODE bindings retain existing files.

use super::{
	metagraph::with_replacement,
	node_child_rebind_tests::{assert_node_state, node_change, node_owner},
	*,
};
use crate::storage::graph_pages::{decode_subgraph_directory, read_node_hot_slot};

/// Sync changes equal-counter child identities twice without touching
/// Hot/directory.
#[test]
fn node_child_rebind_sync_equal_counters_shared_child_and_repeated_rebind() {
	equal_counters(IoMode::Sync);
}

/// NativeAsync retains both previous immutable owners through repeated commits.
#[cfg(feature = "native-async-io")]
#[test]
fn node_child_rebind_native_async_equal_counters_shared_child_and_repeated_rebind() {
	equal_counters(IoMode::NativeAsync);
}

/// A shared child is distinguished by its owner path even with identical
/// counters.
fn equal_counters(mode: IoMode) {
	let (mut fixture, _) = family_fixture(mode, GraphKind::Metagraph);
	let (global, owner, original_child) = node_owner(&fixture);
	let (_, _, shared_child) = super::transition_rewire_tests::portal(&fixture);
	assert_ne!(original_child, shared_child);
	let store = local(&fixture.store);
	let state = store.state.lock();
	let files = state.file_catalog.files.clone();
	let binding = state
		.bindings
		.graphs
		.iter()
		.find(|binding| binding.graph_id == fixture.graph.id())
		.assert_some();
	let original = binding.arena.clone();
	let slot = original
		.direct_child_slot(&fixture.store.inner.current_root(), fixture.graph.id(), owner)
		.assert_ok();
	let (number, within) = hot_slot_address(owner.slot()).assert_ok();
	let hot_key = PageKey::new(binding.files.space(), binding.files.nodes, number);
	let directory_key =
		PageKey::new(binding.files.space(), binding.files.subgraphs, PageNo::new(0));
	let hot =
		read_node_hot_slot(&store.read_update_page(&state, hot_key).assert_ok(), number, within)
			.assert_ok();
	let directory = decode_subgraph_directory(
		&store.read_update_page(&state, directory_key).assert_ok(),
		PageNo::new(0),
	)
	.assert_ok();
	assert_eq!(directory.0, None);
	assert_eq!(directory.1.len(), 2);
	for entry in &directory.1 {
		assert_eq!(
			(entry.node_capacity, entry.edge_capacity, entry.node_count, entry.edge_count),
			(1, 0, 1, 0)
		);
	}
	drop(state);
	let boundary = store.wal.snapshot().visible_lsn();
	let mut previous = original.clone();
	let mut previous_child = original_child;
	for child in [shared_child, original_child] {
		node_change(&fixture, child).commit().assert_ok();
		assert_node_state(&fixture, global, owner, child);
		let state = store.state.lock();
		let current = &state
			.bindings
			.graphs
			.iter()
			.find(|binding| binding.graph_id == fixture.graph.id())
			.assert_some()
			.arena;
		let mut expected = original.graph_by_slot().clone();
		expected.insert(slot, child);
		assert_eq!(current.graph_by_slot(), &expected);
		assert_eq!(current.scope_by_slot(), original.scope_by_slot());
		assert!(!current.same_owner(&previous));
		assert_eq!(previous.graph_by_slot().get(&slot), Some(&previous_child));
		assert_eq!(original.graph_by_slot().get(&slot), Some(&original_child));
		assert_eq!(
			read_node_hot_slot(
				&store.read_update_page(&state, hot_key).assert_ok(),
				number,
				within
			)
			.assert_ok(),
			hot
		);
		assert_eq!(
			decode_subgraph_directory(
				&store.read_update_page(&state, directory_key).assert_ok(),
				PageNo::new(0)
			)
			.assert_ok(),
			directory
		);
		previous = current.clone();
		previous_child = child;
		drop(state);
		assert!(
			store
				.wal
				.visible_frames(&[hot_key, directory_key], store.wal.snapshot())
				.frames()
				.iter()
				.flatten()
				.all(|frame| frame.frame_lsn() <= boundary)
		);
		assert_retained(&fixture, &files, false);
		assert_bulk_image(&fixture);
	}
	assert_eq!(previous, original);
	assert!(!previous.same_owner(&original));
	assert!(
		store.wal.snapshot().visible_lsn() > store.state.lock().database_superblock.checkpoint_lsn
	);
	fixture = reopen(fixture);
	assert_eq!(fixture.store.local_storage_status().assert_some().options().io_mode, mode);
	assert_node_state(&fixture, global, owner, original_child);
	assert_retained(&fixture, &files, false);
	assert_bulk_image(&fixture);
}

/// Sync replays after an independent accepted EDGE and preserves source
/// overlay.
#[test]
fn node_child_rebind_sync_replay_preserves_independent_commit_and_overlay() {
	replay(IoMode::Sync);
}

/// NativeAsync cold recovery restores the final replay arena without
/// checkpoint.
#[cfg(feature = "native-async-io")]
#[test]
fn node_child_rebind_native_async_replay_preserves_independent_commit_and_overlay() {
	replay(IoMode::NativeAsync);
}

/// The late transaction must use the accepted arena and unchanged payload
/// owner.
fn replay(mode: IoMode) {
	let (fixture, key) = family_fixture(mode, GraphKind::Metagraph);
	let (mut fixture, replacement) = with_replacement(fixture);
	let (global, owner, _) = node_owner(&fixture);
	let mut metadata = fixture.store.begin();
	metadata
		.set_overlay(fixture.graph.id(), &"node:container".parse().assert_ok(), key, 789_i64.into())
		.assert_ok();
	metadata.commit().assert_ok();
	let overlay = fixture
		.store
		.inner
		.current_root()
		.occurrence(fixture.graph.id(), owner)
		.assert_some()
		.overlay()
		.cloned();
	assert!(overlay.is_some());
	let late = node_change(&fixture, replacement);
	let mut other = fixture.store.begin();
	let independent = other
		.attach_edge(
			fixture.graph.id(),
			&fixture.records[2],
			None,
			EdgeTopology::directed([fixture.nodes[0]], [fixture.nodes[1]]),
			None,
			None,
		)
		.assert_ok();
	other.commit().assert_ok();
	let accepted = fixture.store.inner.current_root();
	let independent_state =
		accepted.occurrence(fixture.graph.id(), independent).assert_some().clone();
	let stamp = accepted.commit_ts;
	let files = local(&fixture.store).state.lock().file_catalog.files.clone();
	late.commit().assert_ok();
	let current = fixture.store.inner.current_root();
	assert_eq!(current.commit_ts, stamp + 1);
	assert_eq!(current.occurrence(fixture.graph.id(), independent), Some(&independent_state));
	assert_eq!(
		current.occurrence(fixture.graph.id(), owner).assert_some().overlay(),
		overlay.as_ref()
	);
	drop(current);
	assert_node_state(&fixture, global, owner, replacement);
	assert_retained(&fixture, &files, false);
	assert_bulk_image(&fixture);
	let store = local(&fixture.store);
	assert!(
		store.wal.snapshot().visible_lsn() > store.state.lock().database_superblock.checkpoint_lsn
	);
	drop(accepted);
	fixture = reopen(fixture);
	assert_eq!(fixture.store.local_storage_status().assert_some().options().io_mode, mode);
	let current = fixture.store.inner.current_root();
	assert_eq!(current.occurrence(fixture.graph.id(), independent), Some(&independent_state));
	assert_eq!(
		current.occurrence(fixture.graph.id(), owner).assert_some().overlay(),
		overlay.as_ref()
	);
	assert_node_state(&fixture, global, owner, replacement);
	assert_retained(&fixture, &files, false);
	assert_bulk_image(&fixture);
}
