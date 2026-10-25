//! A surviving EDGE container must follow the dense child-slot remap.

use std::collections::BTreeMap;

use super::*;
use crate::{
	datastore::{GraphOccurrence, OccurrenceState},
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

/// Sync removal shifts the surviving portal transition from slot2 to slot1.
#[test]
fn child_scope_remap_sync_node_detach_retains_files() {
	remap(IoMode::Sync, false);
}

/// NativeAsync removal preserves the complete public child identities.
#[cfg(feature = "native-async-io")]
#[test]
fn child_scope_remap_native_async_node_detach_retains_files() {
	remap(IoMode::NativeAsync, false);
}

/// Sync insertion shifts the surviving portal transition from slot1 to slot2.
#[test]
fn child_scope_remap_sync_node_attach_retains_files() {
	remap(IoMode::Sync, true);
}

/// NativeAsync insertion preserves old pins and final cold authority.
#[cfg(feature = "native-async-io")]
#[test]
fn child_scope_remap_native_async_node_attach_retains_files() {
	remap(IoMode::NativeAsync, true);
}

/// Sync EDGE removal shifts another EDGE on the same physical Hot page.
#[test]
fn child_scope_remap_sync_edge_detach_retains_files() {
	edge_remap(IoMode::Sync, false);
}

/// NativeAsync EDGE removal preserves both complete source roles and weights.
#[cfg(feature = "native-async-io")]
#[test]
fn child_scope_remap_native_async_edge_detach_retains_files() {
	edge_remap(IoMode::NativeAsync, false);
}

/// Sync EDGE insertion coalesces both changed slots into one retained frame.
#[test]
fn child_scope_remap_sync_edge_attach_retains_files() {
	edge_remap(IoMode::Sync, true);
}

/// NativeAsync EDGE insertion retains old scopes and final cold authority.
#[cfg(feature = "native-async-io")]
#[test]
fn child_scope_remap_native_async_edge_attach_retains_files() {
	edge_remap(IoMode::NativeAsync, true);
}

/// Literal full-owner paths distinguish direct remap from leaf replacement.
fn arena(
	graph: GraphId,
	owners: &[GraphOccurrence; 2],
	children: [GraphId; 2],
	present: bool,
) -> GraphArenaBindings {
	let mut graphs = BTreeMap::from([(0, graph)]);
	let mut scopes = BTreeMap::from([(0, Box::<[AtomId]>::default())]);
	if present {
		graphs.insert(1, children[0]);
		scopes.insert(1, vec![owners[0].local_id()].into_boxed_slice());
	}
	let portal_slot = if present {
		2
	} else {
		1
	};
	graphs.insert(portal_slot, children[1]);
	scopes.insert(portal_slot, vec![owners[1].local_id()].into_boxed_slice());
	GraphArenaBindings::new(graphs, scopes)
}

/// Every source/final slot is specified independently of the bulk planner.
pub(super) fn assert_current(
	fixture: &Fixture,
	owners: &[GraphOccurrence; 2],
	children: [GraphId; 2],
	present: bool,
	capacity: u64,
) {
	assert_current_with_counts(fixture, owners, children, present, capacity, [(1, 0, 1, 0); 2]);
}

/// Literal child counters also cover surviving leaves containing EDGE
/// terminals.
pub(super) fn assert_current_with_counts(
	fixture: &Fixture,
	owners: &[GraphOccurrence; 2],
	children: [GraphId; 2],
	present: bool,
	capacity: u64,
	counts: [(u64, u64, u32, u32); 2],
) {
	let root = fixture.store.inner.current_root();
	for owner in owners {
		assert_eq!(root.membership(fixture.graph.id(), owner.global_id()), Some(owner.local_id()));
	}
	let node = root.occurrence(fixture.graph.id(), owners[0].local_id()).assert_some();
	assert_eq!(node.global_id(), owners[0].global_id());
	assert_eq!(node.label(), owners[0].label());
	assert_eq!(node.overlay(), owners[0].overlay());
	assert_eq!(
		node.state(),
		&child_presence_tests::with_child(owners[0].state(), present.then_some(children[0]))
	);
	assert_eq!(root.occurrence(fixture.graph.id(), owners[1].local_id()), Some(&owners[1]));
	let mut read = fixture.store.begin();
	let snapshot = read.query_snapshot().assert_ok();
	for (owner, expected) in owners.iter().zip([present.then_some(children[0]), Some(children[1])])
	{
		assert_eq!(
			read.query_physical_inner_graph(&snapshot, &fixture.graph, owner.local_id())
				.assert_ok()
				.assert_some()
				.graph,
			expected
		);
	}
	let store = local(&fixture.store);
	let state = store.state.lock();
	let binding = state
		.bindings
		.graphs
		.iter()
		.find(|binding| binding.graph_id == fixture.graph.id())
		.assert_some();
	assert_eq!(binding.arena, arena(fixture.graph.id(), owners, children, present));
	assert!(
		binding.arena.same_owner(
			&state
				._database_catalog
				.graphs
				.iter()
				.find(|entry| entry.id == fixture.graph.id())
				.assert_some()
				.arena
		)
	);
	let portal_slot = if present {
		2
	} else {
		1
	};
	let entry =
		|slot, owner: &GraphOccurrence, (node_capacity, edge_capacity, node_count, edge_count)| {
			SubgraphEntry {
				slot,
				generation: owner.local_id().generation(),
				node_capacity,
				edge_capacity,
				node_count,
				edge_count,
			}
		};
	let mut entries = Vec::new();
	if present {
		entries.push(entry(1, &owners[0], counts[0]));
	}
	entries.push(entry(portal_slot, &owners[1], counts[1]));
	let key = PageKey::new(binding.files.space(), binding.files.subgraphs, PageNo::new(0));
	assert_eq!(
		decode_subgraph_directory(&store.read_update_page(&state, key).assert_ok(), PageNo::new(0))
			.assert_ok(),
		(None, entries)
	);
	let header = state.graph_superblocks.get(&fixture.graph.id()).assert_some();
	assert_eq!(header.subgraphs_pages, 1);
	assert_eq!(header.subgraph_count, 1 + u64::from(present));
	assert!(header.subgraph_capacity >= header.subgraph_count);
	assert!(header.subgraph_capacity <= capacity.max(header.subgraph_count));
	drop(state);
	assert_hot(fixture, owners, present, portal_slot);
}

/// Hot identities and child references are checked separately from catalog
/// pages.
fn assert_hot(fixture: &Fixture, owners: &[GraphOccurrence; 2], present: bool, portal_slot: u32) {
	let store = local(&fixture.store);
	let state = store.state.lock();
	let binding = state
		.bindings
		.graphs
		.iter()
		.find(|binding| binding.graph_id == fixture.graph.id())
		.assert_some();
	let (number, within) = hot_slot_address(owners[0].local_id().slot()).assert_ok();
	if owners[0].local_id().is_node() {
		let key = PageKey::new(binding.files.space(), binding.files.nodes, number);
		let node =
			read_node_hot_slot(&store.read_update_page(&state, key).assert_ok(), number, within)
				.assert_ok();
		assert_eq!(node.generation, owners[0].local_id().generation());
		assert_eq!(node.global_atom, Some(owners[0].global_id()));
		assert_eq!(node.flags.contains(NodeSlotFlags::META), present);
		assert_eq!(node.subgraph_slot, present.then_some(1));
	} else {
		let key = PageKey::new(binding.files.space(), binding.files.edges, number);
		let edge =
			read_edge_hot_slot(&store.read_update_page(&state, key).assert_ok(), number, within)
				.assert_ok();
		assert_eq!(edge.generation, owners[0].local_id().generation());
		assert_eq!(edge.global_atom, Some(owners[0].global_id()));
		assert_eq!(edge.flags.contains(EdgeSlotFlags::META), present);
		assert_eq!(edge.transition_subgraph_slot, present.then_some(1));
	}
	let (number, within) = hot_slot_address(owners[1].local_id().slot()).assert_ok();
	let key = PageKey::new(binding.files.space(), binding.files.edges, number);
	let edge = read_edge_hot_slot(&store.read_update_page(&state, key).assert_ok(), number, within)
		.assert_ok();
	assert_eq!(edge.generation, owners[1].local_id().generation());
	assert_eq!(edge.global_atom, Some(owners[1].global_id()));
	assert!(edge.flags.contains(EdgeSlotFlags::META));
	assert_eq!(edge.transition_subgraph_slot, Some(portal_slot));
}

/// The selected NODE remains an endpoint; only its optional child is changed.
fn remap(mode: IoMode, attach: bool) {
	let (fixture, records, children) = fixture(mode, AtomKind::Node, attach);
	publish(fixture, records, children, attach);
}

/// Both accepted owner kinds use the same publication and failure runners.
pub(super) fn fixture(
	mode: IoMode,
	kind: AtomKind,
	attach: bool,
) -> (Fixture, [RecordId; 2], [GraphId; 2]) {
	fixture_with_family(mode, kind, attach, GraphKind::Metagraph)
}

/// Runs the same fixture contract for the selected public parent family.
pub(super) fn fixture_with_family(
	mode: IoMode,
	kind: AtomKind,
	attach: bool,
	family: GraphKind,
) -> (Fixture, [RecordId; 2], [GraphId; 2]) {
	match kind {
		AtomKind::Node => node_fixture(mode, attach, family),
		AtomKind::Edge => edge_fixture(mode, attach, family),
	}
}

/// Keeps the original NODE setup and its surviving EDGE portal unchanged.
fn node_fixture(
	mode: IoMode,
	attach: bool,
	family: GraphKind,
) -> (Fixture, [RecordId; 2], [GraphId; 2]) {
	let (mut fixture, _) = super::metagraph::container_fixture(mode, family);
	let records =
		["node:container", "edge:portal"].map(|name| name.parse::<RecordId>().assert_ok());
	let root = fixture.store.inner.current_root();
	let children = records.each_ref().map(|record| {
		let global = root.resolve_atom(record).assert_some();
		let owner = root.membership(fixture.graph.id(), global).assert_some();
		root.occurrence(fixture.graph.id(), owner).assert_some().child_graph().assert_some()
	});
	drop(root);
	if attach {
		let mut setup = fixture.store.begin();
		setup
			.replace_occurrence_state(fixture.graph.id(), &records[0], OccurrenceState::node(None))
			.assert_ok();
		setup.commit().assert_ok();
		fixture.store.checkpoint(CheckpointMode::Restart).assert_ok();
		fixture = reopen(fixture);
	}
	(fixture, records, children)
}

/// Two EDGE children make a same-page source and final transition remap.
fn edge_remap(mode: IoMode, attach: bool) {
	let (fixture, records, children) = fixture(mode, AtomKind::Edge, attach);
	publish(fixture, records, children, attach);
}

/// Keeps the repeated roles, weight and same-page survivor source unchanged.
fn edge_fixture(
	mode: IoMode,
	attach: bool,
	family: GraphKind,
) -> (Fixture, [RecordId; 2], [GraphId; 2]) {
	let (mut fixture, _) = super::metagraph::container_fixture(mode, family);
	let node = "node:container".parse::<RecordId>().assert_ok();
	let portal = "edge:portal".parse::<RecordId>().assert_ok();
	let root = fixture.store.inner.current_root();
	let node_owner =
		root.membership(fixture.graph.id(), root.resolve_atom(&node).assert_some()).assert_some();
	let selected =
		root.membership(fixture.graph.id(), root.resolve_atom(&portal).assert_some()).assert_some();
	let selected = root.occurrence(fixture.graph.id(), selected).assert_some().clone();
	let children = [
		selected.child_graph().assert_some(),
		root.occurrence(fixture.graph.id(), node_owner).assert_some().child_graph().assert_some(),
	];
	drop(root);
	let survivor = fixture.records[2].clone();
	let mut setup = fixture.store.begin();
	setup
		.replace_occurrence_state(fixture.graph.id(), &node, OccurrenceState::node(None))
		.assert_ok();
	if attach {
		setup
			.replace_occurrence_state(
				fixture.graph.id(),
				&portal,
				child_presence_tests::with_child(selected.state(), None),
			)
			.assert_ok();
	}
	let survivor_id = setup
		.attach_edge(
			fixture.graph.id(),
			&survivor,
			Some("survivor".into()),
			EdgeTopology::directed([fixture.nodes[0], fixture.nodes[0]], [fixture.nodes[1]]),
			Some(13_i64.into()),
			Some(children[1]),
		)
		.assert_ok();
	assert!(selected.local_id().raw() < survivor_id.raw());
	setup.commit().assert_ok();
	fixture.store.checkpoint(CheckpointMode::Restart).assert_ok();
	fixture = reopen(fixture);
	(fixture, [portal, survivor], children)
}

/// Full expected owners are captured before the tested commit for both kinds.
pub(super) fn publish(
	mut fixture: Fixture,
	records: [RecordId; 2],
	children: [GraphId; 2],
	attach: bool,
) -> Fixture {
	let mode = fixture.options.io_mode;
	// A catalog-only boundary permits Passive to advance while the old pin
	// lives.
	let mut warm = fixture.store.begin();
	warm.intern_property("scope_remap_pin_source").assert_ok();
	warm.commit().assert_ok();
	let root = fixture.store.inner.current_root();
	let owners = records.each_ref().map(|record| {
		let global = root.resolve_atom(record).assert_some();
		let owner = root.membership(fixture.graph.id(), global).assert_some();
		root.occurrence(fixture.graph.id(), owner).assert_some().clone()
	});
	assert_eq!(owners[1].local_id().kind(), Some(AtomKind::Edge));
	assert!(owners[0].local_id().raw() < owners[1].local_id().raw());
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
	assert_current(&fixture, &owners, children, !attach, capacity);
	assert_bulk_image(&fixture);
	assert_eq!(old_arena, arena(fixture.graph.id(), &owners, children, !attach));
	let mut old_read = fixture.store.begin();
	let old_snapshot = old_read.query_snapshot().assert_ok();
	let mut write = fixture.store.begin();
	write
		.replace_occurrence_state(
			fixture.graph.id(),
			&records[0],
			child_presence_tests::with_child(owners[0].state(), attach.then_some(children[0])),
		)
		.assert_ok();
	write.commit().assert_ok();
	assert_current(&fixture, &owners, children, attach, capacity);
	assert_bulk_image(&fixture);
	eprintln!("CHILD_SCOPE_REMAP_LOGICAL_PASS {mode:?} {attach}");
	assert_retained(&fixture, &files, false);
	eprintln!("CHILD_SCOPE_REMAP_WARM_RETAINED {mode:?} {attach}");
	let state = store.state.lock();
	assert!(
		!state
			.bindings
			.graphs
			.iter()
			.find(|binding| binding.graph_id == fixture.graph.id())
			.assert_some()
			.arena
			.same_owner(&old_arena)
	);
	drop(state);
	assert!(fixture.store.checkpoint(CheckpointMode::Passive).assert_ok());
	let advanced = store.state.lock().database_superblock.checkpoint_lsn;
	assert!(advanced > home && advanced <= boundary);
	assert!(advanced < store.wal.snapshot().visible_lsn());
	for (owner, expected) in
		owners.iter().zip([(!attach).then_some(children[0]), Some(children[1])])
	{
		assert_eq!(
			old_read
				.query_physical_inner_graph(&old_snapshot, &fixture.graph, owner.local_id())
				.assert_ok()
				.assert_some()
				.graph,
			expected
		);
	}
	assert_eq!(old_arena, arena(fixture.graph.id(), &owners, children, !attach));
	drop(old_snapshot);
	drop(old_read);
	// The final commit remains in WAL beyond the old pin's backfill boundary.
	fixture = reopen(fixture);
	assert_eq!(fixture.store.local_storage_status().assert_some().options().io_mode, mode);
	assert_current(&fixture, &owners, children, attach, capacity);
	assert_retained(&fixture, &files, false);
	assert_bulk_image(&fixture);
	fixture
}
