//! An unchanged qualified connector follows a surviving direct child scope.

use super::*;
use crate::{
	datastore::GraphOccurrence,
	graph::{EdgeEndpoint, QualifiedAtomId},
	storage::graph_pages::{EdgeSlotFlags, EndpointSide},
	values::storage::DiskAtomRef,
};

/// Sync NODE attach preserves the untouched qualified connector.
#[test]
fn qualified_child_scope_sync_node_attach_retains_data_files() {
	remap(IoMode::Sync, AtomKind::Node, true);
}

/// NativeAsync NODE attach preserves the untouched qualified connector.
#[cfg(feature = "native-async-io")]
#[test]
fn qualified_child_scope_native_async_node_attach_retains_data_files() {
	remap(IoMode::NativeAsync, AtomKind::Node, true);
}

/// Sync NODE detach preserves the untouched qualified connector.
#[test]
fn qualified_child_scope_sync_node_detach_retains_data_files() {
	remap(IoMode::Sync, AtomKind::Node, false);
}

/// NativeAsync NODE detach preserves the untouched qualified connector.
#[cfg(feature = "native-async-io")]
#[test]
fn qualified_child_scope_native_async_node_detach_retains_data_files() {
	remap(IoMode::NativeAsync, AtomKind::Node, false);
}

/// Sync EDGE attach preserves the untouched qualified connector.
#[test]
fn qualified_child_scope_sync_edge_attach_retains_data_files() {
	remap(IoMode::Sync, AtomKind::Edge, true);
}

/// NativeAsync EDGE attach preserves the untouched qualified connector.
#[cfg(feature = "native-async-io")]
#[test]
fn qualified_child_scope_native_async_edge_attach_retains_data_files() {
	remap(IoMode::NativeAsync, AtomKind::Edge, true);
}

/// Sync EDGE detach preserves the untouched qualified connector.
#[test]
fn qualified_child_scope_sync_edge_detach_retains_data_files() {
	remap(IoMode::Sync, AtomKind::Edge, false);
}

/// NativeAsync EDGE detach preserves the untouched qualified connector.
#[cfg(feature = "native-async-io")]
#[test]
fn qualified_child_scope_native_async_edge_detach_retains_data_files() {
	remap(IoMode::NativeAsync, AtomKind::Edge, false);
}

/// Input identities and literal dense slots do not depend on prepared output.
pub(super) fn assert_inline(
	fixture: &Fixture,
	owner: &GraphOccurrence,
	terminal: AtomId,
	scope: u32,
) {
	let root = fixture.store.inner.current_root();
	assert_eq!(root.occurrence(fixture.graph.id(), owner.local_id()), Some(owner));
	let store = local(&fixture.store);
	let state = store.state.lock();
	let binding = state
		.bindings
		.graphs
		.iter()
		.find(|entry| entry.graph_id == fixture.graph.id())
		.assert_some();
	let (number, within) = hot_slot_address(owner.local_id().slot()).assert_ok();
	let key = PageKey::new(binding.files.space(), binding.files.edges, number);
	let hot = read_edge_hot_slot(&store.read_update_page(&state, key).assert_ok(), number, within)
		.assert_ok();
	assert_eq!(hot.global_atom, Some(owner.global_id()));
	assert_eq!(hot.generation, owner.local_id().generation());
	assert!(hot.flags.contains(EdgeSlotFlags::CROSS_LEVEL));
	assert!(!hot.flags.contains(EdgeSlotFlags::META));
	assert_eq!((hot.source_count, hot.target_count), (1, 1));
	assert_eq!(hot.source_inline, [
		Some(DiskAtomRef::from_parts(AtomKind::Node, terminal.slot()).assert_ok()),
		None
	]);
	assert_eq!(hot.target_inline, [
		Some(DiskAtomRef::from_parts(AtomKind::Node, fixture.nodes[1].slot()).assert_ok()),
		None
	]);
	assert_eq!(hot.endpoint_graph_slot(EndpointSide::Source), Some(scope));
	assert_eq!(hot.endpoint_graph_slot(EndpointSide::Target), Some(0));
	assert_eq!(hot.source_overflow, None);
	assert_eq!(hot.target_overflow, None);
	assert_eq!(hot.source_subgraph_overflow, None);
	assert_eq!(hot.target_subgraph_overflow, None);
	assert_eq!(hot.transition_subgraph_slot, None);
}

/// All ordinary remap assertions also apply when a role keeps its logical path.
fn remap(mode: IoMode, kind: AtomKind, attach: bool) {
	let (mut fixture, records, children) = child_scope_remap_tests::fixture(mode, kind, attach);
	let root = fixture.store.inner.current_root();
	let portal_global = root.resolve_atom(&records[1]).assert_some();
	let portal = root.membership(fixture.graph.id(), portal_global).assert_some();
	let leaf = root.graph(children[1]).assert_some();
	let terminals =
		leaf.occurrences().filter(|owner| owner.local_id().is_node()).collect::<Vec<_>>();
	assert_eq!(terminals.len(), 1);
	let terminal = terminals[0].local_id();
	let connector_record = fixture.records[3].clone();
	let global = root.resolve_atom(&connector_record).assert_some();
	assert_eq!(root.membership(fixture.graph.id(), global), None);
	let nested = EdgeEndpoint::Nested(QualifiedAtomId::new([portal], terminal).assert_ok());
	let topology = EdgeTopology::directed([nested], [EdgeEndpoint::Local(fixture.nodes[1])]);
	drop(terminals);
	drop(root);
	let mut setup = fixture.store.begin();
	setup
		.attach_edge(fixture.graph.id(), &connector_record, None, topology, None, None)
		.assert_ok();
	setup.commit().assert_ok();
	fixture.store.checkpoint(CheckpointMode::Restart).assert_ok();
	fixture = reopen(fixture);
	let root = fixture.store.inner.current_root();
	let connector = root.membership(fixture.graph.id(), global).assert_some();
	let owner = root.occurrence(fixture.graph.id(), connector).assert_some().clone();
	drop(root);
	assert_inline(
		&fixture,
		&owner,
		terminal,
		if attach {
			1
		} else {
			2
		},
	);
	assert_bulk_image(&fixture);
	eprintln!("QUALIFIED_CHILD_SCOPE_SOURCE_PASS {mode:?} {kind:?} {attach}");
	// The shared runner distinguishes logical success from file-retention RED.
	let fixture = child_scope_remap_tests::publish(fixture, records, children, attach);
	assert_inline(
		&fixture,
		&owner,
		terminal,
		if attach {
			2
		} else {
			1
		},
	);
	eprintln!("QUALIFIED_CHILD_SCOPE_COLD_PASS {mode:?} {kind:?} {attach}");
}
