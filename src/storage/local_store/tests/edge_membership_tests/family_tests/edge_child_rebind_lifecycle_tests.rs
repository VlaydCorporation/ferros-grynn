//! Shared leaves and final replay retain complete EDGE source ownership.

use super::{
	edge_child_rebind_support::{Source, edge_change},
	metagraph::with_replacement,
	transition_rewire_tests::{portal, rewrite},
	*,
};
use crate::datastore::OccurrenceState;

/// Sync changes equal-counter child identities twice without changing data
/// pages.
#[test]
fn edge_child_rebind_sync_shared_child_and_repeated_rebind() {
	repeated(IoMode::Sync);
}

/// NativeAsync keeps both earlier immutable owners through cold WAL reopen.
#[cfg(feature = "native-async-io")]
#[test]
fn edge_child_rebind_native_async_shared_child_and_repeated_rebind() {
	repeated(IoMode::NativeAsync);
}

/// A shared GraphId still has a distinct canonical slot for each owner path.
fn repeated(mode: IoMode) {
	let (mut fixture, _) = family_fixture(mode, GraphKind::Metagraph);
	let source = Source::capture(&fixture);
	let (_, _, shared) = super::node_child_rebind_tests::node_owner(&fixture);
	assert_ne!(source.child, shared);
	let mut previous = source.arena.clone();
	let mut previous_child = source.child;
	for child in [shared, source.child] {
		edge_change(&fixture, child).commit().assert_ok();
		source.assert_current(&fixture, child, (1, 0, 1, 0));
		let store = local(&fixture.store);
		let state = store.state.lock();
		let current = &state
			.bindings
			.graphs
			.iter()
			.find(|binding| binding.graph_id == fixture.graph.id())
			.assert_some()
			.arena;
		assert!(!current.same_owner(&previous));
		assert_eq!(previous.graph_by_slot().get(&source.slot), Some(&previous_child));
		assert_eq!(
			current.graph_by_slot().values().filter(|graph| **graph == shared).count(),
			if child == shared {
				2
			} else {
				1
			}
		);
		previous = current.clone();
		previous_child = child;
		drop(state);
		assert_bulk_image(&fixture);
	}
	assert_eq!(previous, source.arena);
	assert!(!previous.same_owner(&source.arena));
	let store = local(&fixture.store);
	assert!(
		store.wal.snapshot().visible_lsn() > store.state.lock().database_superblock.checkpoint_lsn
	);
	fixture = reopen(fixture);
	assert_eq!(fixture.store.local_storage_status().assert_some().options().io_mode, mode);
	source.assert_current(&fixture, source.child, (1, 0, 1, 0));
	assert_bulk_image(&fixture);
}

/// Sync replays the leaf change over an independently accepted EDGE attach.
#[test]
fn edge_child_rebind_sync_replay_preserves_independent_commit_and_metadata() {
	replay(IoMode::Sync);
}

/// NativeAsync cold recovery restores final replay state without checkpoint.
#[cfg(feature = "native-async-io")]
#[test]
fn edge_child_rebind_native_async_replay_preserves_independent_commit_and_metadata() {
	replay(IoMode::NativeAsync);
}

/// The source witness is captured after the independent commit, before replay.
fn replay(mode: IoMode) {
	let (fixture, key) = family_fixture(mode, GraphKind::Metagraph);
	let (mut fixture, replacement) = with_replacement(fixture);
	let (_, _, old_child) = portal(&fixture);
	let roles = EdgeTopology::directed([fixture.nodes[0]], [fixture.nodes[1]]);
	let mut metadata =
		rewrite(&fixture, OccurrenceState::edge(roles, Some(13_i64.into()), Some(old_child)));
	metadata
		.set_occurrence_label(
			fixture.graph.id(),
			&"edge:portal".parse().assert_ok(),
			Some("replay-source".into()),
		)
		.assert_ok();
	metadata
		.set_overlay(fixture.graph.id(), &"edge:portal".parse().assert_ok(), key, 789_i64.into())
		.assert_ok();
	metadata.commit().assert_ok();
	let late = edge_change(&fixture, replacement);
	let mut independent = fixture.store.begin();
	let other = independent
		.attach_edge(
			fixture.graph.id(),
			&fixture.records[2],
			None,
			EdgeTopology::directed([fixture.nodes[1]], [fixture.nodes[0]]),
			None,
			None,
		)
		.assert_ok();
	independent.commit().assert_ok();
	let accepted = fixture.store.inner.current_root();
	let other_state = accepted.occurrence(fixture.graph.id(), other).assert_some().clone();
	let stamp = accepted.commit_ts;
	let source = Source::capture(&fixture);
	late.commit().assert_ok();
	let current = fixture.store.inner.current_root();
	assert_eq!(current.commit_ts, stamp + 1);
	assert_eq!(current.occurrence(fixture.graph.id(), other), Some(&other_state));
	drop(current);
	source.assert_current(&fixture, replacement, (2, 1, 2, 1));
	assert_metadata(&fixture, source.owner.1, key);
	assert_bulk_image_with_overlays(&fixture, &[source.owner.1]);
	drop(accepted);
	fixture = reopen(fixture);
	assert_eq!(fixture.store.local_storage_status().assert_some().options().io_mode, mode);
	assert_eq!(
		fixture.store.inner.current_root().occurrence(fixture.graph.id(), other),
		Some(&other_state)
	);
	source.assert_current(&fixture, replacement, (2, 1, 2, 1));
	assert_metadata(&fixture, source.owner.1, key);
	assert_bulk_image_with_overlays(&fixture, &[source.owner.1]);
}

/// Exact physical payload and logical metadata remain independent of bulk
/// offsets.
fn assert_metadata(fixture: &Fixture, owner: AtomId, key: PropertyKeyId) {
	let root = fixture.store.inner.current_root();
	let occurrence = root.occurrence(fixture.graph.id(), owner).assert_some();
	assert_eq!(occurrence.label().map(|label| label.as_str()), Some("replay-source"));
	let mut read = fixture.store.begin();
	let snapshot = read.query_snapshot().assert_ok();
	let fields =
		read.query_physical_fields(&snapshot, &fixture.graph, owner).assert_ok().assert_some();
	assert_eq!(fields.properties.len(), 1);
	assert_eq!(fields.properties.get(key), Some(&crate::values::Value::from(789_i64)));
}
