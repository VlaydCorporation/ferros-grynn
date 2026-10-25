//! True child attach/detach must retain existing NODE/EDGE data files.

use super::*;
use crate::{
	datastore::{OccurrenceState, OccurrenceStateRef},
	graph::GraphId,
	storage::graph_pages::{
		EdgeSlotFlags,
		GraphArenaBindings,
		NodeSlotFlags,
		SubgraphEntry,
		decode_subgraph_directory,
		read_node_hot_slot,
	},
};

/// Sync Node child attach retains data files and snapshot authority.
#[test]
fn child_presence_sync_node_attach_retains_data_files() {
	presence(IoMode::Sync, AtomKind::Node, true);
}

/// NativeAsync Node child attach retains data files and snapshot authority.
#[cfg(feature = "native-async-io")]
#[test]
fn child_presence_native_async_node_attach_retains_data_files() {
	presence(IoMode::NativeAsync, AtomKind::Node, true);
}

/// Sync Node child detach retains data files and snapshot authority.
#[test]
fn child_presence_sync_node_detach_retains_data_files() {
	presence(IoMode::Sync, AtomKind::Node, false);
}

/// NativeAsync Node child detach retains data files and snapshot authority.
#[cfg(feature = "native-async-io")]
#[test]
fn child_presence_native_async_node_detach_retains_data_files() {
	presence(IoMode::NativeAsync, AtomKind::Node, false);
}

/// Sync Edge child attach retains data files and snapshot authority.
#[test]
fn child_presence_sync_edge_attach_retains_data_files() {
	presence(IoMode::Sync, AtomKind::Edge, true);
}

/// NativeAsync Edge child attach retains data files and snapshot authority.
#[cfg(feature = "native-async-io")]
#[test]
fn child_presence_native_async_edge_attach_retains_data_files() {
	presence(IoMode::NativeAsync, AtomKind::Edge, true);
}

/// Sync Edge child detach retains data files and snapshot authority.
#[test]
fn child_presence_sync_edge_detach_retains_data_files() {
	presence(IoMode::Sync, AtomKind::Edge, false);
}

/// NativeAsync Edge child detach retains data files and snapshot authority.
#[cfg(feature = "native-async-io")]
#[test]
fn child_presence_native_async_edge_detach_retains_data_files() {
	presence(IoMode::NativeAsync, AtomKind::Edge, false);
}

/// Existing full owners are stable across setup, publication and reopen.
pub(super) fn record(kind: AtomKind) -> RecordId {
	match kind {
		AtomKind::Node => "node:container",
		AtomKind::Edge => "edge:portal",
	}
	.parse()
	.assert_ok()
}

/// Changes only child presence in an independently captured occurrence.
pub(super) fn with_child(state: &OccurrenceState, child: Option<GraphId>) -> OccurrenceState {
	match state.view() {
		OccurrenceStateRef::Node {
			..
		} => OccurrenceState::node(child),
		OccurrenceStateRef::Edge {
			topology,
			weight,
			..
		} => OccurrenceState::edge(topology.clone(), *weight, child),
	}
}

/// Setup preserves the accepted topology and weight of each container.
fn child_state(fixture: &Fixture, kind: AtomKind, child: Option<GraphId>) -> OccurrenceState {
	let root = fixture.store.inner.current_root();
	let global = root.resolve_atom(&record(kind)).assert_some();
	let owner = root.membership(fixture.graph.id(), global).assert_some();
	with_child(root.occurrence(fixture.graph.id(), owner).assert_some().state(), child)
}

/// Setup leaves only the selected direct child, or a genuine empty page0.
pub(super) fn fixture(
	mode: IoMode,
	kind: AtomKind,
	attach: bool,
	family: GraphKind,
) -> (Fixture, GraphId) {
	let (mut fixture, _) = super::metagraph::container_fixture(mode, family);
	let root = fixture.store.inner.current_root();
	let global = root.resolve_atom(&record(kind)).assert_some();
	let owner = root.membership(fixture.graph.id(), global).assert_some();
	let child =
		root.occurrence(fixture.graph.id(), owner).assert_some().child_graph().assert_some();
	drop(root);
	let mut setup = fixture.store.begin();
	for target in [AtomKind::Node, AtomKind::Edge] {
		if target != kind || attach {
			setup
				.replace_occurrence_state(
					fixture.graph.id(),
					&record(target),
					child_state(&fixture, target, None),
				)
				.assert_ok();
		}
	}
	setup.commit().assert_ok();
	fixture.store.checkpoint(CheckpointMode::Restart).assert_ok();
	fixture = reopen(fixture);
	// A catalog-only publication creates a WAL boundary beyond home without
	// changing the selected source Hot or direct directory.
	let mut warm = fixture.store.begin();
	warm.intern_property("child_presence_pin_source").assert_ok();
	warm.commit().assert_ok();
	assert_bulk_image(&fixture);
	(fixture, child)
}

/// Literal maps distinguish a real absent binding from an empty child graph.
fn arena(graph: GraphId, owner: AtomId, child: Option<GraphId>) -> GraphArenaBindings {
	let mut graphs = std::collections::BTreeMap::from([(0, graph)]);
	let mut scopes = std::collections::BTreeMap::from([(0, Box::<[AtomId]>::default())]);
	if let Some(child) = child {
		graphs.insert(1, child);
		scopes.insert(1, vec![owner].into_boxed_slice());
	}
	GraphArenaBindings::new(graphs, scopes)
}

/// The one-page directory and Hot META/slot agree with the exact runtime root.
pub(super) fn assert_current(
	fixture: &Fixture,
	owner: AtomId,
	global: crate::datastore::GlobalAtomId,
	expected: &OccurrenceState,
	capacity_ceiling: u64,
) {
	let root = fixture.store.inner.current_root();
	assert_eq!(root.membership(fixture.graph.id(), global), Some(owner));
	assert_eq!(root.occurrence(fixture.graph.id(), owner).assert_some().state(), expected);
	let child = match expected.view() {
		OccurrenceStateRef::Node {
			nested_graph,
		} => *nested_graph,
		OccurrenceStateRef::Edge {
			transition_graph,
			..
		} => *transition_graph,
	};
	let mut read = fixture.store.begin();
	let snapshot = read.query_snapshot().assert_ok();
	assert_eq!(
		read.query_physical_inner_graph(&snapshot, &fixture.graph, owner)
			.assert_ok()
			.assert_some()
			.graph,
		child
	);
	let store = local(&fixture.store);
	let state = store.state.lock();
	let binding = state
		.bindings
		.graphs
		.iter()
		.find(|binding| binding.graph_id == fixture.graph.id())
		.assert_some();
	let expected = arena(fixture.graph.id(), owner, child);
	assert_eq!(binding.arena, expected);
	let catalog = state
		._database_catalog
		.graphs
		.iter()
		.find(|entry| entry.id == fixture.graph.id())
		.assert_some();
	assert!(binding.arena.same_owner(&catalog.arena));
	let entries = if child.is_some() {
		vec![SubgraphEntry {
			slot: 1,
			generation: owner.generation(),
			node_capacity: 1,
			edge_capacity: 0,
			node_count: 1,
			edge_count: 0,
		}]
	} else {
		Vec::new()
	};
	let key = PageKey::new(binding.files.space(), binding.files.subgraphs, PageNo::new(0));
	assert_eq!(
		decode_subgraph_directory(&store.read_update_page(&state, key).assert_ok(), PageNo::new(0))
			.assert_ok(),
		(None, entries)
	);
	let header = state.graph_superblocks.get(&fixture.graph.id()).assert_some();
	assert_eq!(header.subgraphs_pages, 1);
	assert_eq!(header.subgraph_count, u64::from(child.is_some()));
	assert!(header.subgraph_capacity >= header.subgraph_count);
	assert!(header.subgraph_capacity <= capacity_ceiling.max(header.subgraph_count));
	let (number, within) = hot_slot_address(owner.slot()).assert_ok();
	match owner.kind().assert_some() {
		AtomKind::Node => {
			let key = PageKey::new(binding.files.space(), binding.files.nodes, number);
			let hot = read_node_hot_slot(
				&store.read_update_page(&state, key).assert_ok(),
				number,
				within,
			)
			.assert_ok();
			assert_eq!(hot.generation, owner.generation());
			assert_eq!(hot.global_atom, Some(global));
			assert_eq!(hot.flags.contains(NodeSlotFlags::META), child.is_some());
			assert_eq!(hot.subgraph_slot, child.map(|_| 1));
		}
		AtomKind::Edge => {
			let key = PageKey::new(binding.files.space(), binding.files.edges, number);
			let hot = read_edge_hot_slot(
				&store.read_update_page(&state, key).assert_ok(),
				number,
				within,
			)
			.assert_ok();
			assert_eq!(hot.generation, owner.generation());
			assert_eq!(hot.global_atom, Some(global));
			assert_eq!(hot.flags.contains(EdgeSlotFlags::META), child.is_some());
			assert_eq!(hot.transition_subgraph_slot, child.map(|_| 1));
		}
	}
}

/// Semantic acceptance precedes retention, old-pin and cold phase markers.
fn presence(mode: IoMode, kind: AtomKind, attach: bool) {
	presence_with_family(mode, kind, attach, GraphKind::Metagraph);
}

/// Runs the same fixture contract for the selected public parent family.
pub(super) fn presence_with_family(mode: IoMode, kind: AtomKind, attach: bool, family: GraphKind) {
	let (mut fixture, child) = fixture(mode, kind, attach, family);
	let root = fixture.store.inner.current_root();
	let global = root.resolve_atom(&record(kind)).assert_some();
	let owner = root.membership(fixture.graph.id(), global).assert_some();
	assert_eq!(owner.kind(), Some(kind));
	assert_eq!(global.kind(), Some(kind));
	let before_state = root.occurrence(fixture.graph.id(), owner).assert_some().state().clone();
	let after_state = with_child(&before_state, attach.then_some(child));
	let before_child = (!attach).then_some(child);
	let store = local(&fixture.store);
	let state = store.state.lock();
	let files = state.file_catalog.files.clone();
	let old_arena = state
		.bindings
		.graphs
		.iter()
		.find(|binding| binding.graph_id == fixture.graph.id())
		.assert_some()
		.arena
		.clone();
	let capacity = state.graph_superblocks.get(&fixture.graph.id()).assert_some().subgraph_capacity;
	let home = state.database_superblock.checkpoint_lsn;
	let boundary = store.wal.snapshot().visible_lsn();
	assert!(boundary > home);
	drop(state);
	drop(root);
	assert_current(&fixture, owner, global, &before_state, capacity);
	assert_eq!(old_arena, arena(fixture.graph.id(), owner, before_child));
	let mut old_read = fixture.store.begin();
	let old_snapshot = old_read.query_snapshot().assert_ok();
	let mut write = fixture.store.begin();
	write
		.replace_occurrence_state(fixture.graph.id(), &record(kind), after_state.clone())
		.assert_ok();
	write.commit().assert_ok();
	assert_current(&fixture, owner, global, &after_state, capacity);
	assert_bulk_image(&fixture);
	eprintln!("CHILD_PRESENCE_LOGICAL_PASS {kind:?} {attach} {mode:?}");
	assert_retained(&fixture, &files, false);
	eprintln!("CHILD_PRESENCE_WARM_RETAINED {kind:?} {attach} {mode:?}");
	assert!(fixture.store.checkpoint(CheckpointMode::Passive).assert_ok());
	let advanced = store.state.lock().database_superblock.checkpoint_lsn;
	assert!(advanced > home && advanced <= boundary);
	assert_eq!(
		old_read
			.query_physical_inner_graph(&old_snapshot, &fixture.graph, owner)
			.assert_ok()
			.assert_some()
			.graph,
		before_child
	);
	assert_eq!(old_arena, arena(fixture.graph.id(), owner, before_child));
	drop(old_snapshot);
	drop(old_read);
	fixture.store.checkpoint(CheckpointMode::Restart).assert_ok();
	fixture = reopen(fixture);
	assert_eq!(fixture.store.local_storage_status().assert_some().options().io_mode, mode);
	assert_current(&fixture, owner, global, &after_state, capacity);
	assert_retained(&fixture, &files, false);
	assert_bulk_image(&fixture);
}
