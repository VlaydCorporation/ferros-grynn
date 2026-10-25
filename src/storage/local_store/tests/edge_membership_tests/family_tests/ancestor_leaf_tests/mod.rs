//! Shared middle graphs require one arena update per full ancestor path.

use std::sync::Arc;

use super::{metagraph::with_replacement, *};
use crate::{
	datastore::{OccurrenceState, QuerySnapshot},
	graph::{EdgeEndpoint, GraphId, QualifiedAtomId},
	storage::{
		graph_pages::{
			GraphArenaBindings,
			build_graph_image,
			decode_statistics_root,
			decode_subgraph_directory,
		},
		page::PageLsn,
	},
	values::{Value, storage::EncodeContext},
};

/// One extra ancestor level needs complete qualified scope publication.
mod depth_tests;
/// Known and uncertain failures retain complete old or final arena bundles.
mod recovery_tests;
/// One shared logical envelope must also reject the actual local commit.
mod resource_tests;
/// False cache owners and checksum-valid parent pages must reject privately.
mod source_tests;

/// Runtime and catalog authorities are captured separately to detect early
/// install.
type ArenaSources = [(GraphId, GraphArenaBindings, GraphArenaBindings); 3];

/// Captures owners under the caller's existing publication lock.
fn arena_sources(routes: &Fanout, state: &crate::storage::local_store::LocalState) -> ArenaSources {
	[routes.fixture.graph.id(), routes.outer[0].id(), routes.outer[1].id()].map(|id| {
		let runtime = state
			.bindings
			.graphs
			.iter()
			.find(|entry| entry.graph_id == id)
			.assert_some()
			.arena
			.clone();
		let catalog = state
			._database_catalog
			.graphs
			.iter()
			.find(|entry| entry.id == id)
			.assert_some()
			.arena
			.clone();
		(id, runtime, catalog)
	})
}

/// Failed preparation must not install even a value-equal replacement owner.
fn assert_sources(state: &crate::storage::local_store::LocalState, expected: &ArenaSources) {
	for (id, runtime, catalog) in expected {
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

/// The same state constructor serves preparation and public transaction tests.
fn final_state(routes: &Fanout) -> crate::datastore::OccurrenceState {
	let root = routes.fixture.store.inner.current_root();
	child_presence_tests::with_child(
		root.occurrence(routes.fixture.graph.id(), routes.selected).assert_some().state(),
		Some(routes.children[1].id()),
	)
}

/// Builds a singleton final replay candidate without physical publication.
fn roots(
	routes: &Fanout,
) -> (
	crate::datastore::DatabaseRoot,
	crate::datastore::DatabaseRoot,
	crate::datastore::PublicationDelta,
) {
	metagraph::leaf_roots(&routes.fixture, routes.selected, final_state(routes))
}

/// Actual commits exercise the same public owner mutation as the core matrix.
fn change(routes: &Fanout) -> Transaction {
	let mut write = routes.fixture.store.begin();
	write
		.replace_occurrence_state(
			routes.fixture.graph.id(),
			&child_presence_tests::record(routes.selected.kind().assert_some()),
			final_state(routes),
		)
		.assert_ok();
	write
}

/// Sync NODE replacement updates both shared parent arenas and keeps aliases.
#[test]
fn ancestor_leaf_sync_node_replacement_retains_files_and_reopens() {
	replace(IoMode::Sync, AtomKind::Node, false);
}

/// NativeAsync NODE replacement keeps the same catalog publication boundary.
#[cfg(feature = "native-async-io")]
#[test]
fn ancestor_leaf_native_async_node_replacement_retains_files_and_reopens() {
	replace(IoMode::NativeAsync, AtomKind::Node, false);
}

/// Sync EDGE replacement preserves every unchanged qualified connector.
#[test]
fn ancestor_leaf_sync_edge_replacement_retains_files_and_reopens() {
	replace(IoMode::Sync, AtomKind::Edge, false);
}

/// NativeAsync EDGE replacement retains old physical pins through backfill.
#[cfg(feature = "native-async-io")]
#[test]
fn ancestor_leaf_native_async_edge_replacement_retains_files_and_reopens() {
	replace(IoMode::NativeAsync, AtomKind::Edge, false);
}

/// A route through the changed leaf cannot silently accept a new GraphId.
#[test]
fn ancestor_leaf_sync_unanchored_replacement_publishes_nothing() {
	for kind in [AtomKind::Node, AtomKind::Edge] {
		replace(IoMode::Sync, kind, true);
	}
}

/// NativeAsync rejects the same ancestor terminal retarget before publication.
#[cfg(feature = "native-async-io")]
#[test]
fn ancestor_leaf_native_async_unanchored_replacement_publishes_nothing() {
	for kind in [AtomKind::Node, AtomKind::Edge] {
		replace(IoMode::NativeAsync, kind, true);
	}
}

/// Two outer roots share M; R reaches it through both NODE and EDGE owners.
struct Fanout {
	/// Owns the accepted local store and isolated directory through reopen.
	fixture: Fixture,
	/// Public durable leaves have equal terminal bits but different contents.
	children: [GraphRef; 2],
	/// R and S are immutable logical parents of the changed middle graph.
	outer: [GraphRef; 2],
	/// R's NODE and EDGE owners share M; its other NODE aliases the old leaf.
	r_owners: [AtomId; 3],
	/// S supplies a separate physical arena for the same changed occurrence.
	s_owner: AtomId,
	/// Exactly this full generation-aware M owner changes its child.
	selected: AtomId,
	/// Both old and new leaves deliberately use the same local NODE handle.
	terminal: AtomId,
	/// The terminal property proves which leaf was physically read.
	key: PropertyKeyId,
}

/// Setup may rebuild; every acceptance witness is captured after cold reopen.
fn fanout(mode: IoMode, kind: AtomKind, unanchored: bool) -> Fanout {
	fanout_with_families(mode, kind, unanchored, [GraphKind::Metagraph; 2])
}

/// The same literal paths exercise both public containment families.
fn fanout_with_families(
	mode: IoMode,
	kind: AtomKind,
	unanchored: bool,
	families: [GraphKind; 2],
) -> Fanout {
	fanout_with_parent(mode, kind, unanchored, families, GraphKind::Metagraph)
}

/// Runs the unchanged ancestor gates for the selected public owner family.
fn fanout_with_parent(
	mode: IoMode,
	kind: AtomKind,
	unanchored: bool,
	families: [GraphKind; 2],
	parent_family: GraphKind,
) -> Fanout {
	let (fixture, key) = metagraph::container_fixture(mode, parent_family);
	let (mut fixture, replacement) = with_replacement(fixture);
	let record = child_presence_tests::record(kind);
	let source = fixture.store.inner.current_root();
	let selected = source
		.membership(fixture.graph.id(), source.resolve_atom(&record).assert_some())
		.assert_some();
	let child =
		source.occurrence(fixture.graph.id(), selected).assert_some().child_graph().assert_some();
	let terminal = source.graph(child).assert_some().occurrences().next().assert_some().local_id();
	let sibling_record = child_presence_tests::record(if kind == AtomKind::Node {
		AtomKind::Edge
	} else {
		AtomKind::Node
	});
	let sibling = source
		.membership(fixture.graph.id(), source.resolve_atom(&sibling_record).assert_some())
		.assert_some();
	let sibling_state =
		source.occurrence(fixture.graph.id(), sibling).assert_some().state().clone();
	assert!(terminal.is_node());
	assert_eq!(
		source.graph(replacement).assert_some().occurrences().next().assert_some().local_id(),
		terminal
	);
	drop(source);
	let mut setup = fixture.store.begin();
	// A GraphId-wide replacement would corrupt this untouched M owner too.
	setup
		.replace_occurrence_state(
			fixture.graph.id(),
			&sibling_record,
			child_presence_tests::with_child(&sibling_state, Some(child)),
		)
		.assert_ok();
	let outer = [("ancestor_r", families[0]), ("ancestor_s", families[1])]
		.map(|(name, family)| setup.define_graph(name, family).assert_ok());
	let r_node = setup
		.attach_node(outer[0].id(), &"node:a".parse().assert_ok(), None, Some(fixture.graph.id()))
		.assert_ok();
	let alias = setup
		.attach_node(outer[0].id(), &"node:b".parse().assert_ok(), None, Some(child))
		.assert_ok();
	let r_edge = setup
		.attach_edge(
			outer[0].id(),
			&fixture.records[2],
			None,
			EdgeTopology::directed([r_node], [alias]),
			None,
			Some(fixture.graph.id()),
		)
		.assert_ok();
	let s_owner = setup
		.attach_node(outer[1].id(), &"node:a".parse().assert_ok(), None, Some(fixture.graph.id()))
		.assert_ok();
	let nested = |scope: Vec<AtomId>| {
		EdgeEndpoint::Nested(QualifiedAtomId::new(scope, terminal).assert_ok())
	};
	let via_middle = if unanchored {
		selected
	} else {
		sibling
	};
	let topology =
		EdgeTopology::directed([nested(vec![alias]), nested(vec![r_node, via_middle])], [
			EdgeEndpoint::Local(r_node),
		]);
	setup
		.replace_occurrence_state(
			outer[0].id(),
			&fixture.records[2],
			OccurrenceState::edge(topology, None, Some(fixture.graph.id())),
		)
		.assert_ok();
	setup.commit().assert_ok();
	fixture.store.checkpoint(CheckpointMode::Restart).assert_ok();
	fixture = reopen(fixture);
	let root = fixture.store.inner.current_root();
	let accepted = |graph, name: &RecordId| {
		root.membership(graph, root.resolve_atom(name).assert_some()).assert_some()
	};
	assert_eq!(accepted(fixture.graph.id(), &record), selected);
	assert_eq!(accepted(outer[0].id(), &"node:a".parse().assert_ok()), r_node);
	assert_eq!(accepted(outer[0].id(), &fixture.records[2]), r_edge);
	assert_eq!(accepted(outer[0].id(), &"node:b".parse().assert_ok()), alias);
	assert_eq!(accepted(outer[1].id(), &"node:a".parse().assert_ok()), s_owner);
	let children =
		[child, replacement].map(|id| root.schema.graph(id).assert_some().reference().clone());
	drop(root);
	Fanout {
		fixture,
		children,
		outer,
		r_owners: [r_node, r_edge, alias],
		s_owner,
		selected,
		terminal,
		key,
	}
}

/// The same snapshot must compose both containment hops and physical data.
fn physical(routes: &Fanout, read: &Transaction, query: &QuerySnapshot, replacement: bool) {
	let middle = &routes.fixture.graph;
	for (graph, owner) in [
		(&routes.outer[0], routes.r_owners[0]),
		(&routes.outer[0], routes.r_owners[1]),
		(&routes.outer[1], routes.s_owner),
	] {
		assert_eq!(
			read.query_physical_inner_graph(query, graph, owner).assert_ok().assert_some().graph,
			Some(middle.id())
		);
	}
	let child = &routes.children[usize::from(replacement)];
	assert_eq!(
		read.query_physical_inner_graph(query, middle, routes.selected)
			.assert_ok()
			.assert_some()
			.graph,
		Some(child.id())
	);
	assert_eq!(
		read.query_physical_inner_graph(query, &routes.outer[0], routes.r_owners[2])
			.assert_ok()
			.assert_some()
			.graph,
		Some(routes.children[0].id())
	);
	let fields =
		read.query_physical_fields(query, child, routes.terminal).assert_ok().assert_some();
	let (name, value) = if replacement {
		("node:d", 40_i64)
	} else if routes.selected.is_node() {
		("node:child_node", 91)
	} else {
		("node:child_edge", 92)
	};
	assert_eq!(fields.record_id, name.parse::<RecordId>().assert_ok());
	assert_eq!(fields.properties.get(routes.key), Some(&Value::from(value)));
}

/// Full scopes, directory counters and independent bulk images must agree.
fn current(routes: &Fanout, arenas: &[(GraphId, GraphArenaBindings)]) {
	let root = routes.fixture.store.inner.current_root();
	let store = local(&routes.fixture.store);
	let state = store.state.lock();
	let changed = [
		(routes.fixture.graph.id(), vec![routes.selected]),
		(routes.outer[0].id(), vec![routes.r_owners[0], routes.selected]),
		(routes.outer[0].id(), vec![routes.r_owners[1], routes.selected]),
		(routes.outer[1].id(), vec![routes.s_owner, routes.selected]),
	];
	for (graph, old) in arenas {
		let binding =
			state.bindings.graphs.iter().find(|entry| entry.graph_id == *graph).assert_some();
		let mut expected = old.graph_by_slot().clone();
		for (_, scope) in changed.iter().filter(|(id, _)| id == graph) {
			let slot = *old
				.scope_by_slot()
				.iter()
				.find(|(_, stored)| stored.as_ref() == scope.as_slice())
				.assert_some()
				.0;
			assert_eq!(
				expected.insert(slot, routes.children[1].id()),
				Some(routes.children[0].id())
			);
		}
		assert_eq!(binding.arena.graph_by_slot(), &expected);
		assert_eq!(binding.arena.scope_by_slot(), old.scope_by_slot());
		let catalog =
			state._database_catalog.graphs.iter().find(|entry| entry.id == *graph).assert_some();
		assert!(catalog.arena.same_owner(&binding.arena));
		let mut context = EncodeContext::new(root.dictionary.as_ref().clone());
		let bulk = build_graph_image(&root, *graph, &mut context, PageLsn::ZERO).assert_ok();
		assert_eq!(binding.arena, bulk.arena);
		assert_eq!(bulk.subgraphs.len(), 1);
		let number = PageNo::new(0);
		let key = PageKey::new(binding.files.space(), binding.files.subgraphs, number);
		assert_eq!(
			decode_subgraph_directory(&store.read_update_page(&state, key).assert_ok(), number)
				.assert_ok(),
			decode_subgraph_directory(&bulk.subgraphs[0], number).assert_ok()
		);
		let statistics_file = crate::storage::local_store::LocalStore::statistics_file(
			&state,
			binding.files.space(),
			DataFileKind::Statistics,
		)
		.assert_ok();
		let label_file = crate::storage::local_store::LocalStore::statistics_file(
			&state,
			binding.files.space(),
			DataFileKind::LabelStatistics,
		)
		.assert_ok();
		let property_file = crate::storage::local_store::LocalStore::statistics_file(
			&state,
			binding.files.space(),
			DataFileKind::PropertyStatistics,
		)
		.assert_ok();
		let statistics_key = PageKey::new(binding.files.space(), statistics_file, number);
		let (statistics, _) = decode_statistics_root(
			&store.read_update_page(&state, statistics_key).assert_ok(),
			store.visible_pages(&state, binding.files.space(), statistics_file).assert_ok(),
			store.visible_pages(&state, binding.files.space(), label_file).assert_ok(),
			store.visible_pages(&state, binding.files.space(), property_file).assert_ok(),
		)
		.assert_ok();
		assert_eq!(statistics.revision, root.statistics.graph(*graph).assert_some().revision);
	}
	drop(state);
	let mut read = routes.fixture.store.begin();
	let query = read.query_snapshot().assert_ok();
	physical(routes, &read, &query, true);
	assert_bulk_image(&routes.fixture);
}

/// Watches accepted graph pages with the intentional changed-root statistics
/// exception.
fn watched_pages(
	store: &crate::storage::local_store::LocalStore,
	state: &crate::storage::local_store::LocalState,
	changed: GraphId,
) -> (Vec<PageKey>, PageKey) {
	let mut keys = Vec::new();
	for file in &state.file_catalog.files {
		if !matches!(file.space_id, DataSpaceId::Graph(_))
			|| (file.space_id == DataSpaceId::Graph(changed)
				&& matches!(
					file.file_id.kind(),
					Some(DataFileKind::GraphSuperblock | DataFileKind::Subgraphs)
				)) {
			continue;
		}
		if let Some(count) = super::super::watched_page_count(store, state, file) {
			keys.extend(
				(0..count)
					.map(|number| PageKey::new(file.space_id, file.file_id, PageNo::new(number))),
			);
		}
	}
	assert!(!keys.is_empty());
	let statistics_file = crate::storage::local_store::LocalStore::statistics_file(
		state,
		DataSpaceId::Graph(changed),
		DataFileKind::Statistics,
	)
	.assert_ok();
	let statistics_key = PageKey::new(DataSpaceId::Graph(changed), statistics_file, PageNo::new(0));
	(keys, statistics_key)
}

/// Canonical maps may change; unchanged graph pages and file identities may
/// not.
fn replace(mode: IoMode, kind: AtomKind, unanchored: bool) {
	let mut routes = fanout(mode, kind, unanchored);
	let mut warm = routes.fixture.store.begin();
	warm.intern_property("ancestor_leaf_pin_source").assert_ok();
	warm.commit().assert_ok();
	let source = routes.fixture.store.inner.current_root();
	let store = local(&routes.fixture.store);
	let state = store.state.lock();
	let files = state.file_catalog.files.clone();
	let arenas =
		[routes.fixture.graph.id(), routes.outer[0].id(), routes.outer[1].id()].map(|graph| {
			let arena = state
				.bindings
				.graphs
				.iter()
				.find(|entry| entry.graph_id == graph)
				.assert_some()
				.arena
				.clone();
			(graph, arena)
		});
	assert_eq!(arenas.each_ref().map(|(_, arena)| arena.graph_by_slot().len()), [3, 8, 4]);
	let (keys, statistics_key) = watched_pages(store, &state, routes.fixture.graph.id());
	let home = state.database_superblock.checkpoint_lsn;
	let boundary = store.wal.snapshot().visible_lsn();
	assert!(boundary > home);
	drop(state);
	let mut old = routes.fixture.store.begin();
	let old_query = old.query_snapshot().assert_ok();
	let record = child_presence_tests::record(kind);
	let occurrence = source.occurrence(routes.fixture.graph.id(), routes.selected).assert_some();
	let final_state =
		child_presence_tests::with_child(occurrence.state(), Some(routes.children[1].id()));
	let mut write = routes.fixture.store.begin();
	write.replace_occurrence_state(routes.fixture.graph.id(), &record, final_state).assert_ok();
	if unanchored {
		let error = write.commit().assert_err();
		assert!(matches!(
			error.status().status(),
			crate::gql_status::Status::GraphConstraintViolation { .. }
		));
		assert!(Arc::ptr_eq(&source, &routes.fixture.store.inner.current_root()));
		assert_eq!(store.wal.snapshot().visible_lsn(), boundary);
		assert_eq!(store.state.lock().file_catalog.files, files);
		eprintln!("ANCESTOR_LEAF_UNANCHORED_PASS {mode:?} {kind:?}");
		return;
	}
	write.commit().assert_ok();
	let published = routes.fixture.store.inner.current_root();
	for graph in &routes.outer {
		assert!(Arc::ptr_eq(
			source.graphs.get(&graph.id()).assert_some(),
			published.graphs.get(&graph.id()).assert_some()
		));
	}
	current(&routes, &arenas);
	eprintln!("ANCESTOR_LEAF_LOGICAL_PASS {mode:?} {kind:?}");
	assert_retained(&routes.fixture, &files, false);
	let frames = store.wal.visible_frames(&keys, store.wal.snapshot());
	let changed = keys
		.iter()
		.zip(frames.frames())
		.filter_map(|(key, frame)| {
			frame.as_ref().filter(|frame| frame.frame_lsn() > boundary).map(|_frame| *key)
		})
		.collect::<Vec<_>>();
	// The M statistics root must advance its revision with the incidence
	// commit.
	assert_eq!(changed, vec![statistics_key], "ANCESTOR_UNEXPECTED_GRAPH_WAL_FRAMES");
	eprintln!("ANCESTOR_LEAF_STATS_FRAME {mode:?} {kind:?} {changed:?}");
	// Checkpoint needs the WAL prune write lock held read-only by frames.
	drop(frames);
	eprintln!("ANCESTOR_LEAF_RETAINED_PASS {mode:?} {kind:?}");
	assert!(routes.fixture.store.checkpoint(CheckpointMode::Passive).assert_ok());
	let advanced = store.state.lock().database_superblock.checkpoint_lsn;
	assert!(advanced > home && advanced <= boundary);
	// These are the first old physical reads; cached reads do not prove a miss.
	physical(&routes, &old, &old_query, false);
	assert_eq!(
		arenas.each_ref().map(|(_, arena)| arena
			.graph_by_slot()
			.values()
			.filter(|id| **id == routes.children[0].id())
			.count()),
		[2, 5, 2]
	);
	eprintln!("ANCESTOR_LEAF_PIN_PASS {mode:?} {kind:?}");
	drop(old_query);
	drop(old);
	drop(source);
	drop(published);
	routes.fixture = reopen(routes.fixture);
	for checkpoint in [false, true] {
		if checkpoint {
			routes.fixture.store.checkpoint(CheckpointMode::Restart).assert_ok();
			routes.fixture = reopen(routes.fixture);
		}
		current(&routes, &arenas);
		assert_retained(&routes.fixture, &files, false);
	}
	eprintln!("ANCESTOR_LEAF_COLD_PASS {mode:?} {kind:?}");
}
