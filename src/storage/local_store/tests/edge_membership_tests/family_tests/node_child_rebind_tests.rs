//! A real NODE child rebind must retain the existing physical generation.

use super::{metagraph::with_replacement, *};
use crate::{
	datastore::OccurrenceState,
	storage::graph_pages::{GraphArenaBindings, decode_subgraph_directory, read_node_hot_slot},
};

/// Sync changes the accepted child without rebuilding unchanged graph files.
#[test]
fn node_child_rebind_sync_retains_files_and_old_arena() {
	rebind(IoMode::Sync);
}

/// NativeAsync preserves the same arena and directory publication contract.
#[cfg(feature = "native-async-io")]
#[test]
fn node_child_rebind_native_async_retains_files_and_old_arena() {
	rebind(IoMode::NativeAsync);
}

/// Precreated nonempty leaves distinguish a real rebind from a counter-only
/// update.
fn rebind(mode: IoMode) {
	rebind_with_family(mode, GraphKind::Metagraph);
}

/// Runs the same fixture contract for the selected public parent family.
pub(super) fn rebind_with_family(mode: IoMode, family: GraphKind) {
	let (fixture, _) = super::metagraph::container_fixture(mode, family);
	let (mut fixture, replacement) = with_replacement(fixture);
	let record = "node:container".parse::<RecordId>().assert_ok();
	// Give the source a real WAL-backed Hot version before retaining its pin.
	let mut control = fixture.store.begin();
	control
		.set_occurrence_label(fixture.graph.id(), &record, Some("container-source".into()))
		.assert_ok();
	control.commit().assert_ok();
	assert_bulk_image(&fixture);
	let source = fixture.store.inner.current_root();
	let global = source.resolve_atom(&record).assert_some();
	let owner = source.membership(fixture.graph.id(), global).assert_some();
	let old_child =
		source.occurrence(fixture.graph.id(), owner).assert_some().child_graph().assert_some();
	assert_ne!(old_child, replacement);
	let store = local(&fixture.store);
	let state = store.state.lock();
	let files = state.file_catalog.files.clone();
	let binding = state
		.bindings
		.graphs
		.iter()
		.find(|binding| binding.graph_id == fixture.graph.id())
		.assert_some();
	let arena = binding.arena.clone();
	let slot = arena.direct_child_slot(&source, fixture.graph.id(), owner).assert_ok();
	let mut graphs = arena.graph_by_slot().clone();
	assert_eq!(graphs.insert(slot, replacement), Some(old_child));
	let expected_arena = GraphArenaBindings::new(graphs, arena.scope_by_slot().clone());
	let (number, within) = hot_slot_address(owner.slot()).assert_ok();
	let hot_key = PageKey::new(binding.files.space(), binding.files.nodes, number);
	let old_hot =
		read_node_hot_slot(&store.read_update_page(&state, hot_key).assert_ok(), number, within)
			.assert_ok();
	let directory_key =
		PageKey::new(binding.files.space(), binding.files.subgraphs, PageNo::new(0));
	let (next, mut entries) = decode_subgraph_directory(
		&store.read_update_page(&state, directory_key).assert_ok(),
		PageNo::new(0),
	)
	.assert_ok();
	assert_eq!(next, None);
	assert_eq!(entries.len(), 2);
	let entry = entries.iter_mut().find(|entry| entry.slot == slot).assert_some();
	assert_eq!((entry.node_count, entry.edge_count), (1, 0));
	entry.node_capacity = 2;
	entry.edge_capacity = 1;
	entry.node_count = 2;
	entry.edge_count = 1;
	let header = state.graph_superblocks.get(&fixture.graph.id()).assert_some();
	let ceilings = (header.subgraph_capacity, header.subgraph_count, header.subgraphs_pages);
	assert_eq!(header.subgraphs_pages, 1);
	let home = state.database_superblock.checkpoint_lsn;
	let boundary = store.wal.snapshot().visible_lsn();
	assert!(boundary > home);
	drop(state);
	drop(source);
	let mut old_read = fixture.store.begin();
	let old_snapshot = old_read.query_snapshot().assert_ok();
	let mut write = fixture.store.begin();
	write
		.replace_occurrence_state(
			fixture.graph.id(),
			&record,
			OccurrenceState::node(Some(replacement)),
		)
		.assert_ok();
	write.commit().assert_ok();
	let assert_current = |fixture: &Fixture| {
		let root = fixture.store.inner.current_root();
		assert_eq!(root.resolve_atom(&record), Some(global));
		assert_eq!(root.membership(fixture.graph.id(), global), Some(owner));
		let occurrence = root.occurrence(fixture.graph.id(), owner).assert_some();
		assert_eq!(occurrence.state(), &OccurrenceState::node(Some(replacement)));
		assert_eq!(occurrence.label().map(|label| label.as_str()), Some("container-source"));
		let mut read = fixture.store.begin();
		let snapshot = read.query_snapshot().assert_ok();
		assert_eq!(
			read.query_physical_inner_graph(&snapshot, &fixture.graph, owner)
				.assert_ok()
				.assert_some()
				.graph,
			Some(replacement)
		);
		let store = local(&fixture.store);
		let state = store.state.lock();
		let binding = state
			.bindings
			.graphs
			.iter()
			.find(|binding| binding.graph_id == fixture.graph.id())
			.assert_some();
		assert_eq!(binding.arena, expected_arena);
		assert_eq!(
			state
				._database_catalog
				.graphs
				.iter()
				.find(|graph| graph.id == fixture.graph.id())
				.assert_some()
				.arena,
			expected_arena
		);
		let key = PageKey::new(binding.files.space(), binding.files.nodes, number);
		assert_eq!(
			read_node_hot_slot(&store.read_update_page(&state, key).assert_ok(), number, within)
				.assert_ok(),
			old_hot
		);
		let key = PageKey::new(binding.files.space(), binding.files.subgraphs, PageNo::new(0));
		assert_eq!(
			decode_subgraph_directory(
				&store.read_update_page(&state, key).assert_ok(),
				PageNo::new(0)
			)
			.assert_ok(),
			(None, entries.clone())
		);
		let header = state.graph_superblocks.get(&fixture.graph.id()).assert_some();
		assert_eq!(
			(header.subgraph_capacity, header.subgraph_count, header.subgraphs_pages),
			ceilings
		);
	};
	assert_current(&fixture);
	eprintln!("NODE_CHILD_REBIND_LOGICAL_PASS {mode:?}");
	assert_retained(&fixture, &files, false);
	eprintln!("NODE_CHILD_REBIND_WARM_RETAINED {mode:?}");
	assert_bulk_image(&fixture);
	assert!(fixture.store.checkpoint(CheckpointMode::Passive).assert_ok());
	let advanced = store.state.lock().database_superblock.checkpoint_lsn;
	assert!(advanced > home && advanced <= boundary);
	// The original query pin first resolves its physical child after advancing
	// backfill.
	assert_eq!(
		old_read
			.query_physical_inner_graph(&old_snapshot, &fixture.graph, owner)
			.assert_ok()
			.assert_some()
			.graph,
		Some(old_child)
	);
	assert_eq!(arena.graph_by_slot().get(&slot), Some(&old_child));
	drop(old_snapshot);
	drop(old_read);
	fixture.store.checkpoint(CheckpointMode::Restart).assert_ok();
	fixture = reopen(fixture);
	assert_eq!(fixture.store.local_storage_status().assert_some().options().io_mode, mode);
	assert_current(&fixture);
	assert_retained(&fixture, &files, false);
	assert_bulk_image(&fixture);
}

/// Captures the selected full NODE owner and its accepted child.
pub(super) fn node_owner(
	fixture: &Fixture,
) -> (crate::datastore::GlobalAtomId, AtomId, crate::graph::GraphId) {
	let root = fixture.store.inner.current_root();
	let record = "node:container".parse().assert_ok();
	let global = root.resolve_atom(&record).assert_some();
	let local = root.membership(fixture.graph.id(), global).assert_some();
	let child =
		root.occurrence(fixture.graph.id(), local).assert_some().child_graph().assert_some();
	(global, local, child)
}

/// The production writer changes only the selected child's identity.
pub(super) fn node_change(fixture: &Fixture, child: crate::graph::GraphId) -> Transaction {
	let mut write = fixture.store.begin();
	write
		.replace_occurrence_state(
			fixture.graph.id(),
			&"node:container".parse().assert_ok(),
			OccurrenceState::node(Some(child)),
		)
		.assert_ok();
	write
}

/// Logical and physical readers must resolve the same full selected owner.
pub(super) fn assert_node_state(
	fixture: &Fixture,
	global: crate::datastore::GlobalAtomId,
	owner: AtomId,
	child: crate::graph::GraphId,
) {
	let root = fixture.store.inner.current_root();
	assert_eq!(root.resolve_atom(&"node:container".parse().assert_ok()), Some(global));
	assert_eq!(root.membership(fixture.graph.id(), global), Some(owner));
	assert_eq!(
		root.occurrence(fixture.graph.id(), owner).assert_some().state(),
		&OccurrenceState::node(Some(child))
	);
	let mut read = fixture.store.begin();
	let snapshot = read.query_snapshot().assert_ok();
	assert_eq!(
		read.query_physical_inner_graph(&snapshot, &fixture.graph, owner)
			.assert_ok()
			.assert_some()
			.graph,
		Some(child)
	);
	let store = local(&fixture.store);
	let state = store.state.lock();
	let binding = state
		.bindings
		.graphs
		.iter()
		.find(|binding| binding.graph_id == fixture.graph.id())
		.assert_some();
	let entry = state
		._database_catalog
		.graphs
		.iter()
		.find(|entry| entry.id == fixture.graph.id())
		.assert_some();
	assert!(binding.arena.same_owner(&entry.arena));
	let slot = binding.arena.direct_child_slot(&root, fixture.graph.id(), owner).assert_ok();
	assert_eq!(binding.arena.graph_by_slot().get(&slot), Some(&child));
}

/// Produces the final singleton incidence replay candidate without publishing.
pub(super) fn node_roots(
	fixture: &Fixture,
	child: crate::graph::GraphId,
) -> (
	crate::datastore::DatabaseRoot,
	crate::datastore::DatabaseRoot,
	crate::datastore::PublicationDelta,
) {
	let (_, owner, _) = node_owner(fixture);
	super::metagraph::leaf_roots(fixture, owner, OccurrenceState::node(Some(child)))
}
