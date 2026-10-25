//! One extra ancestor level preserves every repeated full scope and old pin.

use super::*;
use crate::storage::local_store::tests::generation_directory_paths;

/// Fourth-owner source validation has its own adversarial witnesses.
mod source_tests;

/// Opened maintenance policy bounds the complete four-owner commit.
mod resource_tests;

/// Failure and recovery cannot expose a mixed four-owner bundle.
mod recovery_tests;

/// General Graph ancestors share the existing physical containment contract.
mod graph_family_tests;

/// Sync NODE replacement must retain the added fourth physical arena.
#[test]
fn ancestor_depth_sync_node_replacement_retains_files_and_reopens() {
	replace_depth(IoMode::Sync, AtomKind::Node, false);
}

/// NativeAsync keeps the same fourth-owner publication boundary.
#[cfg(feature = "native-async-io")]
#[test]
fn ancestor_depth_native_async_node_replacement_retains_files_and_reopens() {
	replace_depth(IoMode::NativeAsync, AtomKind::Node, false);
}

/// Sync EDGE replacement changes four deep paths without changing aliases.
#[test]
fn ancestor_depth_sync_edge_replacement_retains_files_and_reopens() {
	replace_depth(IoMode::Sync, AtomKind::Edge, false);
}

/// NativeAsync EDGE replacement survives WAL-only and checkpoint reopen.
#[cfg(feature = "native-async-io")]
#[test]
fn ancestor_depth_native_async_edge_replacement_retains_files_and_reopens() {
	replace_depth(IoMode::NativeAsync, AtomKind::Edge, false);
}

/// An unanchored endpoint in T cannot silently retarget through R and M.
#[test]
fn ancestor_depth_sync_unanchored_replacement_publishes_nothing() {
	for kind in [AtomKind::Node, AtomKind::Edge] {
		replace_depth(IoMode::Sync, kind, true);
	}
}

/// NativeAsync rejects both selected-owner kinds before publication.
#[cfg(feature = "native-async-io")]
#[test]
fn ancestor_depth_native_async_unanchored_replacement_publishes_nothing() {
	for kind in [AtomKind::Node, AtomKind::Edge] {
		replace_depth(IoMode::NativeAsync, kind, true);
	}
}

/// T adds NODE and EDGE routes to R without widening the three-owner fixtures.
struct Depth {
	/// Existing M/R/S aliases and selected leaf replacement.
	routes: Fanout,
	/// A separate public root contains R twice.
	top: GraphRef,
	/// Both full container identities must be retained.
	owners: [AtomId; 2],
}

/// Every owner in the four-arena bundle has separate runtime and catalog pins.
type DepthSources = [(GraphId, GraphArenaBindings, GraphArenaBindings); 4];

/// Captures T without changing the established three-owner fixture.
fn depth_sources(case: &Depth, state: &crate::storage::local_store::LocalState) -> DepthSources {
	let [middle, outer_r, outer_s] = arena_sources(&case.routes, state);
	let id = case.top.id();
	let binding = state.bindings.graphs.iter().find(|entry| entry.graph_id == id).assert_some();
	let catalog = state._database_catalog.graphs.iter().find(|entry| entry.id == id).assert_some();
	[middle, outer_r, outer_s, (id, binding.arena.clone(), catalog.arena.clone())]
}

/// A rejected attempt cannot replace any prepared runtime or catalog owner.
fn assert_depth_sources(state: &crate::storage::local_store::LocalState, sources: &DepthSources) {
	for (id, runtime, catalog) in sources {
		assert!(
			state
				.bindings
				.graphs
				.iter()
				.find(|entry| entry.graph_id == *id)
				.assert_some()
				.arena
				.same_owner(runtime)
		);
		assert!(
			state
				._database_catalog
				.graphs
				.iter()
				.find(|entry| entry.id == *id)
				.assert_some()
				.arena
				.same_owner(catalog)
		);
	}
}

/// Cold reconstruction keeps values while creating legitimate new Arc owners.
fn old_depth(case: &Depth, sources: &DepthSources) {
	let store = local(&case.routes.fixture.store);
	let state = store.state.lock();
	for (id, runtime, _) in sources {
		let binding =
			state.bindings.graphs.iter().find(|entry| entry.graph_id == *id).assert_some();
		let catalog =
			state._database_catalog.graphs.iter().find(|entry| entry.id == *id).assert_some();
		assert_eq!(&binding.arena, runtime);
		assert!(binding.arena.same_owner(&catalog.arena));
	}
	drop(state);
	let mut read = case.routes.fixture.store.begin();
	let query = read.query_snapshot().assert_ok();
	physical_depth(case, &read, &query, false);
	assert_bulk_image(&case.routes.fixture);
}

/// M/R/S and all four changed full T paths form one final bundle.
fn final_depth(case: &Depth, sources: &DepthSources) {
	let old = sources[..3].iter().map(|(id, arena, _)| (*id, arena.clone())).collect::<Vec<_>>();
	current(&case.routes, &old);
	current_depth(case, &sources[3].1);
}

/// R, S and T keep their declared family through publication and recovery.
fn depth_with_parent(
	mode: IoMode,
	kind: AtomKind,
	unanchored: bool,
	families: [GraphKind; 3],
	parent_family: GraphKind,
) -> Depth {
	let mut routes =
		fanout_with_parent(mode, kind, false, [families[0], families[1]], parent_family);
	let root = routes.fixture.store.inner.current_root();
	let sibling_record = child_presence_tests::record(if kind == AtomKind::Node {
		AtomKind::Edge
	} else {
		AtomKind::Node
	});
	let sibling = root
		.membership(routes.fixture.graph.id(), root.resolve_atom(&sibling_record).assert_some())
		.assert_some();
	drop(root);
	let mut setup = routes.fixture.store.begin();
	let top = setup.define_graph("ancestor_top", families[2]).assert_ok();
	let node = setup
		.attach_node(top.id(), &"node:a".parse().assert_ok(), None, Some(routes.outer[0].id()))
		.assert_ok();
	let via_middle = if unanchored {
		routes.selected
	} else {
		sibling
	};
	let endpoint = EdgeEndpoint::Nested(
		QualifiedAtomId::new(vec![node, routes.r_owners[0], via_middle], routes.terminal)
			.assert_ok(),
	);
	let edge = setup
		.attach_edge(
			top.id(),
			&routes.fixture.records[2],
			None,
			EdgeTopology::directed([endpoint], [EdgeEndpoint::Local(node)]),
			None,
			Some(routes.outer[0].id()),
		)
		.assert_ok();
	setup.commit().assert_ok();
	routes.fixture.store.checkpoint(CheckpointMode::Restart).assert_ok();
	routes.fixture = reopen(routes.fixture);
	Depth {
		routes,
		top,
		owners: [node, edge],
	}
}

/// Each physical first hop composes with the accepted R/M/leaf hops.
fn physical_depth(case: &Depth, read: &Transaction, query: &QuerySnapshot, changed: bool) {
	let root = case.routes.fixture.store.inner.current_root();
	for graph in
		[&case.routes.fixture.graph, &case.routes.outer[0], &case.routes.outer[1], &case.top]
	{
		assert_eq!(root.schema.graph(graph.id()).assert_some().reference().kind(), graph.kind());
	}
	for owner in case.owners {
		assert_eq!(
			read.query_physical_inner_graph(query, &case.top, owner)
				.assert_ok()
				.assert_some()
				.graph,
			Some(case.routes.outer[0].id())
		);
	}
	physical(&case.routes, read, query, changed);
}

/// Literal path updates and a separate bulk image protect untouched aliases.
fn current_depth(case: &Depth, old: &GraphArenaBindings) {
	let root = case.routes.fixture.store.inner.current_root();
	let store = local(&case.routes.fixture.store);
	let state = store.state.lock();
	let binding =
		state.bindings.graphs.iter().find(|entry| entry.graph_id == case.top.id()).assert_some();
	let catalog =
		state._database_catalog.graphs.iter().find(|entry| entry.id == case.top.id()).assert_some();
	let mut expected = old.graph_by_slot().clone();
	for top_owner in case.owners {
		for parent_owner in case.routes.r_owners[..2].iter().copied() {
			let path = [top_owner, parent_owner, case.routes.selected];
			let slot = *old
				.scope_by_slot()
				.iter()
				.find(|(_, stored)| stored.as_ref() == path.as_slice())
				.assert_some()
				.0;
			assert_eq!(
				expected.insert(slot, case.routes.children[1].id()),
				Some(case.routes.children[0].id())
			);
		}
	}
	assert_eq!(binding.arena.graph_by_slot(), &expected);
	assert_eq!(binding.arena.scope_by_slot(), old.scope_by_slot());
	assert!(catalog.arena.same_owner(&binding.arena));
	let mut context = EncodeContext::new(root.dictionary.as_ref().clone());
	let bulk = build_graph_image(&root, case.top.id(), &mut context, PageLsn::ZERO).assert_ok();
	assert_eq!(binding.arena, bulk.arena);
	assert_eq!(bulk.subgraphs.len(), 1);
	let number = PageNo::new(0);
	let key = PageKey::new(binding.files.space(), binding.files.subgraphs, number);
	let actual = store.read_update_page(&state, key).assert_ok();
	assert_eq!(
		decode_subgraph_directory(&actual, number).assert_ok(),
		decode_subgraph_directory(&bulk.subgraphs[0], number).assert_ok()
	);
	drop(state);
	let mut read = case.routes.fixture.store.begin();
	let query = read.query_snapshot().assert_ok();
	physical_depth(case, &read, &query, true);
}

/// The RED witness follows successful logical and complete physical checks.
fn replace_depth(mode: IoMode, kind: AtomKind, unanchored: bool) {
	replace_depth_with_families(mode, kind, unanchored, [GraphKind::Metagraph; 3]);
}

/// Family parity uses every existing logical, physical, retained-file and cold
/// gate.
fn replace_depth_with_families(
	mode: IoMode,
	kind: AtomKind,
	unanchored: bool,
	families: [GraphKind; 3],
) {
	replace_depth_with_parent(mode, kind, unanchored, families, GraphKind::Metagraph);
}

/// Runs the unchanged ancestor gates for the selected public owner family.
fn replace_depth_with_parent(
	mode: IoMode,
	kind: AtomKind,
	unanchored: bool,
	families: [GraphKind; 3],
	parent_family: GraphKind,
) {
	let mut case = depth_with_parent(mode, kind, unanchored, families, parent_family);
	let file_ids = |entries: &[crate::storage::catalog::FileCatalogEntry]| {
		entries.iter().map(|entry| (entry.space_id, entry.file_id)).collect::<Vec<_>>()
	};
	let mut warm = case.routes.fixture.store.begin();
	warm.intern_property("ancestor_depth_pin_source").assert_ok();
	warm.commit().assert_ok();
	let source = case.routes.fixture.store.inner.current_root();
	let store = local(&case.routes.fixture.store);
	let state = store.state.lock();
	let files = state.file_catalog.files.clone();
	let sources = arena_sources(&case.routes, &state);
	let arenas = sources.iter().map(|(id, arena, _)| (*id, arena.clone())).collect::<Vec<_>>();
	let top = state
		.bindings
		.graphs
		.iter()
		.find(|entry| entry.graph_id == case.top.id())
		.assert_some()
		.arena
		.clone();
	let top_catalog = state
		._database_catalog
		.graphs
		.iter()
		.find(|entry| entry.id == case.top.id())
		.assert_some()
		.arena
		.clone();
	assert!(top.same_owner(&top_catalog));
	assert_eq!(arenas.iter().map(|(_, arena)| arena.graph_by_slot().len()).collect::<Vec<_>>(), [
		3, 8, 4
	]);
	assert_eq!(top.graph_by_slot().len(), 17);
	assert_eq!(
		top.graph_by_slot().values().filter(|id| **id == case.routes.children[0].id()).count(),
		10
	);
	let (keys, statistics_key) = watched_pages(store, &state, case.routes.fixture.graph.id());
	let home = state.database_superblock.checkpoint_lsn;
	let boundary = store.wal.snapshot().visible_lsn();
	assert!(boundary > home);
	let paths = generation_directory_paths(&case.routes.fixture.directory.0);
	drop(state);
	let mut old = case.routes.fixture.store.begin();
	let old_query = old.query_snapshot().assert_ok();
	let outcome = change(&case.routes).commit();
	if unanchored {
		let error = outcome.assert_err();
		assert!(matches!(
			error.status().status(),
			crate::gql_status::Status::GraphConstraintViolation { .. }
		));
		assert!(Arc::ptr_eq(&source, &case.routes.fixture.store.inner.current_root()));
		assert_eq!(store.wal.snapshot().visible_lsn(), boundary);
		let state = store.state.lock();
		assert_sources(&state, &sources);
		assert!(
			state
				.bindings
				.graphs
				.iter()
				.find(|entry| entry.graph_id == case.top.id())
				.assert_some()
				.arena
				.same_owner(&top)
		);
		assert!(
			state
				._database_catalog
				.graphs
				.iter()
				.find(|entry| entry.id == case.top.id())
				.assert_some()
				.arena
				.same_owner(&top_catalog)
		);
		assert_eq!(state.file_catalog.files, files);
		assert_eq!(generation_directory_paths(&case.routes.fixture.directory.0), paths);
		eprintln!("ANCESTOR_DEPTH_UNANCHORED_PASS {mode:?} {kind:?}");
		return;
	}
	outcome.assert_ok();
	let published = case.routes.fixture.store.inner.current_root();
	for graph in [case.routes.outer[0].id(), case.routes.outer[1].id(), case.top.id()] {
		assert!(Arc::ptr_eq(
			source.graphs.get(&graph).assert_some(),
			published.graphs.get(&graph).assert_some()
		));
	}
	current(&case.routes, &arenas);
	current_depth(&case, &top);
	eprintln!("ANCESTOR_DEPTH_LOGICAL_PASS {mode:?} {kind:?}");
	assert_retained(&case.routes.fixture, &files, false);
	assert_eq!(
		file_ids(&local(&case.routes.fixture.store).state.lock().file_catalog.files),
		file_ids(&files)
	);
	let frames = store.wal.visible_frames(&keys, store.wal.snapshot());
	let changed = keys
		.iter()
		.zip(frames.frames())
		.filter_map(|(key, frame)| {
			frame.as_ref().filter(|frame| frame.frame_lsn() > boundary).map(|_frame| *key)
		})
		.collect::<Vec<_>>();
	assert_eq!(changed, vec![statistics_key], "ANCESTOR_DEPTH_UNEXPECTED_GRAPH_WAL_FRAMES");
	// Release the prune read guard before the checkpoint writer waits for it.
	drop(frames);
	assert!(case.routes.fixture.store.checkpoint(CheckpointMode::Passive).assert_ok());
	let advanced = store.state.lock().database_superblock.checkpoint_lsn;
	assert!(advanced > home && advanced <= boundary);
	physical_depth(&case, &old, &old_query, false);
	assert_eq!(
		top.graph_by_slot().values().filter(|id| **id == case.routes.children[0].id()).count(),
		10
	);
	eprintln!("ANCESTOR_DEPTH_PIN_PASS {mode:?} {kind:?}");
	drop(old_query);
	drop(old);
	drop(source);
	drop(published);
	case.routes.fixture = reopen(case.routes.fixture);
	for checkpoint in [false, true] {
		if checkpoint {
			case.routes.fixture.store.checkpoint(CheckpointMode::Restart).assert_ok();
			case.routes.fixture = reopen(case.routes.fixture);
		}
		current(&case.routes, &arenas);
		current_depth(&case, &top);
		assert_retained(&case.routes.fixture, &files, false);
		assert_eq!(
			file_ids(&local(&case.routes.fixture.store).state.lock().file_catalog.files),
			file_ids(&files)
		);
	}
	eprintln!("ANCESTOR_DEPTH_COLD_PASS {mode:?} {kind:?}");
}

/// Graph owners reuse the same source and publication contracts.
mod graph_owner_tests;
