//! Existing META EDGE rewires must retain the same real transition graph.

use super::*;
use crate::datastore::OccurrenceState;

/// Sync preserves the accepted arena when only a META EDGE topology changes.
#[test]
fn metagraph_transition_rewire_sync_retains_data_files() {
	transition_rewire(IoMode::Sync);
}

/// NativeAsync keeps the same child directory and cold transition binding.
#[cfg(feature = "native-async-io")]
#[test]
fn metagraph_transition_rewire_native_async_retains_data_files() {
	transition_rewire(IoMode::NativeAsync);
}

/// The complete production commit runs before the physical retention assertion.
fn transition_rewire(mode: IoMode) {
	let (mut fixture, _) = family_fixture(mode, GraphKind::Metagraph);
	let witness = metagraph::Witness::capture(&fixture);
	let root = fixture.store.inner.current_root();
	let child = root.schema.named_graph("child_edge").assert_some().reference().id();
	let record = "edge:portal".parse::<RecordId>().assert_ok();
	let global = root.resolve_atom(&record).assert_some();
	let edge = root.membership(fixture.graph.id(), global).assert_some();
	assert_eq!(root.occurrence(fixture.graph.id(), edge).assert_some().child_graph(), Some(child));
	let topology = EdgeTopology::directed([fixture.nodes[0]], [fixture.nodes[1]]);
	let expected = OccurrenceState::edge(topology, None, Some(child));
	assert_ne!(root.occurrence(fixture.graph.id(), edge).assert_some().state(), &expected);
	drop(root);
	let files = local(&fixture.store).state.lock().file_catalog.files.clone();
	let mut write = fixture.store.begin();
	write.replace_occurrence_state(fixture.graph.id(), &record, expected.clone()).assert_ok();
	write.commit().assert_ok();
	let root = fixture.store.inner.current_root();
	assert_eq!(root.membership(fixture.graph.id(), global), Some(edge));
	assert_eq!(root.occurrence(fixture.graph.id(), edge).assert_some().state(), &expected);
	drop(root);
	assert_retained(&fixture, &files, false);
	witness.assert_structure(&fixture);
	assert_bulk_image(&fixture);
	fixture.store.checkpoint(CheckpointMode::Restart).assert_ok();
	fixture = reopen(fixture);
	assert_eq!(fixture.store.local_storage_status().assert_some().options().io_mode, mode);
	assert_retained(&fixture, &files, false);
	witness.assert_structure(&fixture);
	assert_bulk_image(&fixture);
	let root = fixture.store.inner.current_root();
	assert_eq!(root.membership(fixture.graph.id(), global), Some(edge));
	assert_eq!(root.occurrence(fixture.graph.id(), edge).assert_some().state(), &expected);
}

/// Captures full owner identities before any candidate or replayed write.
pub(super) fn portal(
	fixture: &Fixture,
) -> (crate::datastore::GlobalAtomId, AtomId, crate::graph::GraphId) {
	let root = fixture.store.inner.current_root();
	let global = root.resolve_atom(&"edge:portal".parse().assert_ok()).assert_some();
	let edge = root.membership(fixture.graph.id(), global).assert_some();
	let child = root.occurrence(fixture.graph.id(), edge).assert_some().child_graph().assert_some();
	(global, edge, child)
}

/// Only the accepted portal is rewritten; callers supply the explicit child.
pub(super) fn rewrite(fixture: &Fixture, state: OccurrenceState) -> Transaction {
	let mut write = fixture.store.begin();
	write
		.replace_occurrence_state(fixture.graph.id(), &"edge:portal".parse().assert_ok(), state)
		.assert_ok();
	write
}

/// Checks both full identities and the exact state independently of page
/// codecs.
pub(super) fn assert_state(
	fixture: &Fixture,
	global: crate::datastore::GlobalAtomId,
	edge: AtomId,
	expected: &OccurrenceState,
) {
	let root = fixture.store.inner.current_root();
	assert_eq!(root.resolve_atom(&"edge:portal".parse().assert_ok()), Some(global));
	assert_eq!(root.membership(fixture.graph.id(), global), Some(edge));
	let occurrence = root.occurrence(fixture.graph.id(), edge).assert_some();
	assert_eq!(occurrence.global_id(), global);
	assert_eq!(occurrence.state(), expected);
	let mut read = fixture.store.begin();
	let snapshot = read.query_snapshot().assert_ok();
	assert_eq!(
		read.query_physical_inner_graph(&snapshot, &fixture.graph, edge)
			.assert_ok()
			.assert_some()
			.graph,
		occurrence.child_graph()
	);
}
