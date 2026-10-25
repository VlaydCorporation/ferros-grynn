//! Root-arena scopes distinguish deep shared routes through NODE and EDGE
//! owners.

use super::*;
use crate::{
	datastore::GraphOccurrence,
	graph::GraphRead,
	storage::{catalog::FileCatalogEntry, graph_pages::GraphArenaBindings, types::Lsn},
	values::storage::DiskAtomRef,
};

/// Sync publishes both deep paths without rebuilding descendants or
/// containment.
#[test]
fn qualified_sync_deep_shared_node_edge_routes_match_bulk() {
	deep_routes(IoMode::Sync);
}

/// NativeAsync preserves scoped EDGE postings, duplicates and cold recovery.
#[cfg(feature = "native-async-io")]
#[test]
fn qualified_native_async_deep_shared_node_edge_routes_match_bulk() {
	deep_routes(IoMode::NativeAsync);
}

/// Accepted handles are resolved again after the setup commit and cold reopen.
struct DeepRoutes {
	/// Owns the production store and its isolated directory.
	fixture: Fixture,
	/// Distinct NODE and EDGE containers share one middle graph.
	owners: [AtomId; 2],
	/// A real EDGE container connects the middle graph to the leaf.
	portal: AtomId,
	/// Both kinds deliberately share terminal slot zero.
	terminals: [AtomId; 2],
	/// Both full root scopes must resolve to this same graph.
	leaf: GraphId,
}

/// Source evidence includes every descendant occurrence and physical page
/// owner.
struct DeepWitness {
	/// Equal child GraphIds must not collapse full-path arena slots.
	arena: GraphArenaBindings,
	/// All descendant logical contents stay unchanged.
	contents: Vec<(GraphId, Vec<GraphOccurrence>)>,
	/// Every descendant file retains its exact catalog identity.
	files: Vec<FileCatalogEntry>,
	/// Descendant pages and the root directory receive no new WAL frames.
	keys: Vec<PageKey>,
	/// The accepted source boundary precedes all candidate writes.
	boundary: Lsn,
}

impl DeepWitness {
	/// Captures a full accepted root arena, not a child-local slot namespace.
	fn capture(fixture: &Fixture) -> Self {
		let root = fixture.store.inner.current_root();
		let store = local(&fixture.store);
		let state = store.state.lock();
		let binding = state
			.bindings
			.graphs
			.iter()
			.find(|binding| binding.graph_id == fixture.graph.id())
			.assert_some();
		let arena = binding.arena.clone();
		let descendants = arena
			.graph_by_slot()
			.values()
			.copied()
			.filter(|id| *id != fixture.graph.id())
			.collect::<BTreeSet<_>>();
		assert_eq!(descendants.len(), 4);
		let contents = descendants
			.iter()
			.map(|id| (*id, root.graph(*id).assert_some().occurrences().cloned().collect()))
			.collect();
		let files = state
			.file_catalog
			.files
			.iter()
			.filter(|entry| descendants.iter().any(|id| entry.space_id == DataSpaceId::Graph(*id)))
			.cloned()
			.collect::<Vec<_>>();
		let ceiling =
			state.graph_superblocks.get(&fixture.graph.id()).assert_some().subgraphs_pages;
		let mut keys = (0..ceiling)
			.map(|number| {
				PageKey::new(binding.files.space(), binding.files.subgraphs, PageNo::new(number))
			})
			.collect::<Vec<_>>();
		for file in &files {
			if let Some(count) = super::super::super::watched_page_count(store, &state, file) {
				keys.extend(
					(0..count).map(|number| {
						PageKey::new(file.space_id, file.file_id, PageNo::new(number))
					}),
				);
			}
		}
		assert!(!keys.is_empty());
		Self {
			arena,
			contents,
			files,
			keys,
			boundary: store.wal.snapshot().visible_lsn(),
		}
	}

	/// Checks logical equality, catalog ownership and absence of descendant
	/// writes.
	fn assert_unchanged(&self, fixture: &Fixture) {
		let root = fixture.store.inner.current_root();
		for (id, contents) in &self.contents {
			assert_eq!(
				root.graph(*id).assert_some().occurrences().cloned().collect::<Vec<_>>(),
				*contents
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
		assert_eq!(binding.arena, self.arena);
		for entry in &self.files {
			assert!(state.file_catalog.files.contains(entry));
		}
		drop(state);
		let frames = store.wal.visible_frames(&self.keys, store.wal.snapshot());
		assert!(frames.frames().iter().flatten().all(|frame| frame.frame_lsn() <= self.boundary));
	}
}

/// Fixture setup may rebuild; all acceptance boundaries are captured
/// afterwards.
fn deep_fixture(mode: IoMode) -> DeepRoutes {
	let (mut fixture, key) = family_fixture(mode, GraphKind::Metagraph);
	let mut setup = fixture.store.begin();
	let middle = setup.define_graph("middle", GraphKind::Metagraph).assert_ok().id();
	let leaf = setup.define_graph("leaf", GraphKind::Graph).assert_ok().id();
	for name in ["node:deep_owner", "node:deep_mid", "node:deep_leaf"] {
		let mut properties = PropertyMap::new();
		properties.insert(key, 99);
		setup.create_atom(name.parse().assert_ok(), properties).assert_ok();
	}
	for name in ["edge:deep_owner", "edge:deep_portal", "edge:deep_leaf"] {
		setup.create_atom(name.parse().assert_ok(), PropertyMap::new()).assert_ok();
	}
	setup
		.attach_node(fixture.graph.id(), &"node:deep_owner".parse().assert_ok(), None, Some(middle))
		.assert_ok();
	setup
		.attach_edge(
			fixture.graph.id(),
			&"edge:deep_owner".parse().assert_ok(),
			None,
			EdgeTopology::directed([fixture.nodes[0]], [fixture.nodes[1]]),
			None,
			Some(middle),
		)
		.assert_ok();
	let mid =
		setup.attach_node(middle, &"node:deep_mid".parse().assert_ok(), None, None).assert_ok();
	setup
		.attach_edge(
			middle,
			&"edge:deep_portal".parse().assert_ok(),
			None,
			EdgeTopology::directed([mid], [mid]),
			None,
			Some(leaf),
		)
		.assert_ok();
	let node =
		setup.attach_node(leaf, &"node:deep_leaf".parse().assert_ok(), None, None).assert_ok();
	setup
		.attach_edge(
			leaf,
			&"edge:deep_leaf".parse().assert_ok(),
			None,
			EdgeTopology::directed([node], [node]),
			None,
			None,
		)
		.assert_ok();
	setup.commit().assert_ok();
	fixture.store.checkpoint(CheckpointMode::Restart).assert_ok();
	fixture = reopen(fixture);
	let mut read = fixture.store.begin();
	let owners = ["node:deep_owner", "edge:deep_owner"].map(|name| {
		read.membership(fixture.graph.id(), &name.parse().assert_ok()).assert_ok().assert_some()
	});
	let portal =
		read.membership(middle, &"edge:deep_portal".parse().assert_ok()).assert_ok().assert_some();
	let terminals = ["node:deep_leaf", "edge:deep_leaf"]
		.map(|name| read.membership(leaf, &name.parse().assert_ok()).assert_ok().assert_some());
	drop(read);
	assert_eq!(owners[0].kind(), Some(AtomKind::Node));
	assert_eq!(owners[1].kind(), Some(AtomKind::Edge));
	assert_eq!(terminals.map(AtomId::slot), [0, 0]);
	DeepRoutes {
		fixture,
		owners,
		portal,
		terminals,
		leaf,
	}
}

/// Every shape checks both paths before rewire and removes the connector
/// afterwards.
fn deep_routes(mode: IoMode) {
	let mut routes = deep_fixture(mode);
	let witness = DeepWitness::capture(&routes.fixture);
	let files = local(&routes.fixture.store).state.lock().file_catalog.files.clone();
	let scopes = routes.owners.map(|owner| {
		let scope = [owner, routes.portal];
		let slot = *witness
			.arena
			.scope_by_slot()
			.iter()
			.find(|(_, stored)| stored.as_ref() == scope.as_slice())
			.assert_some()
			.0;
		assert_ne!(slot, 0);
		assert_eq!(witness.arena.graph_by_slot().get(&slot), Some(&routes.leaf));
		slot
	});
	assert_ne!(scopes[0], scopes[1]);
	let endpoints = |terminal| {
		routes.owners.map(|owner| {
			EdgeEndpoint::Nested(QualifiedAtomId::new([owner, routes.portal], terminal).assert_ok())
		})
	};
	let node = endpoints(routes.terminals[0]);
	let edge = endpoints(routes.terminals[1]);
	let source =
		vec![node[0].clone(), edge[1].clone(), node[1].clone(), edge[0].clone(), edge[1].clone()];
	let topology = EdgeTopology::directed(source, [EdgeEndpoint::Local(routes.fixture.nodes[1])]);
	let (write, connector) = attachment(&routes.fixture, topology);
	write.commit().assert_ok();
	assert_retained(&routes.fixture, &files, false);
	assert_bulk_image(&routes.fixture);
	witness.assert_unchanged(&routes.fixture);
	let raw = |atom: AtomId| {
		DiskAtomRef::from_parts(atom.kind().assert_some(), atom.slot()).assert_ok().raw()
	};
	let mut rows =
		scopes.map(|scope| (scope, raw(routes.terminals[1]), raw(connector), 0)).to_vec();
	rows.sort();
	super::super::super::endpoint_publication_tests::assert_postings(&routes.fixture, &rows);
	for endpoint in &edge {
		let read = routes.fixture.store.begin();
		assert_eq!(
			routes
				.fixture
				.graph
				.read(&read)
				.assert_ok()
				.edge_endpoint_connectors(endpoint)
				.assert_ok(),
			Some(vec![connector])
		);
	}
	let topology = EdgeTopology::directed([EdgeEndpoint::Local(routes.fixture.nodes[0])], [
		edge[1].clone(),
		node[0].clone(),
		edge[0].clone(),
	]);
	rewrite(&routes.fixture, topology).commit().assert_ok();
	for row in &mut rows {
		row.3 = 1;
	}
	super::super::super::endpoint_publication_tests::assert_postings(&routes.fixture, &rows);
	assert_retained(&routes.fixture, &files, false);
	assert_bulk_image(&routes.fixture);
	witness.assert_unchanged(&routes.fixture);
	routes.fixture.store.checkpoint(CheckpointMode::Restart).assert_ok();
	routes.fixture = reopen(routes.fixture);
	assert_eq!(routes.fixture.store.local_storage_status().assert_some().options().io_mode, mode);
	super::super::super::endpoint_publication_tests::assert_postings(&routes.fixture, &rows);
	witness.assert_unchanged(&routes.fixture);
	assert_bulk_image(&routes.fixture);
	let mut remove = routes.fixture.store.begin();
	remove
		.detach_occurrence(routes.fixture.graph.id(), &routes.fixture.records[2], false)
		.assert_ok();
	remove.commit().assert_ok();
	super::super::super::endpoint_publication_tests::assert_postings(&routes.fixture, &[]);
	assert_retained(&routes.fixture, &files, false);
	assert_bulk_image(&routes.fixture);
	witness.assert_unchanged(&routes.fixture);
	routes.fixture.store.checkpoint(CheckpointMode::Restart).assert_ok();
	routes.fixture = reopen(routes.fixture);
	assert_eq!(routes.fixture.store.local_storage_status().assert_some().options().io_mode, mode);
	assert_retained(&routes.fixture, &files, false);
	assert_bulk_image(&routes.fixture);
	witness.assert_unchanged(&routes.fixture);
}

/// Sync combines a real META owner with deep NODE/EDGE qualified terminals.
#[test]
fn transition_rewire_sync_deep_shared_edge_terminals_match_bulk() {
	transition_deep_routes(IoMode::Sync);
}

/// NativeAsync retains full scopes, selected postings and the original child.
#[cfg(feature = "native-async-io")]
#[test]
fn transition_rewire_native_async_deep_shared_edge_terminals_match_bulk() {
	transition_deep_routes(IoMode::NativeAsync);
}

/// The root EDGE retains its middle child while its roles traverse both routes.
fn transition_deep_routes(mode: IoMode) {
	let mut routes = deep_fixture(mode);
	let witness = DeepWitness::capture(&routes.fixture);
	let files = local(&routes.fixture.store).state.lock().file_catalog.files.clone();
	let root = routes.fixture.store.inner.current_root();
	let record = "edge:deep_owner".parse::<RecordId>().assert_ok();
	let global = root.resolve_atom(&record).assert_some();
	let owner = routes.owners[1];
	let child =
		root.occurrence(routes.fixture.graph.id(), owner).assert_some().child_graph().assert_some();
	drop(root);
	let endpoints = routes.owners.map(|container| {
		EdgeEndpoint::Nested(
			QualifiedAtomId::new([container, routes.portal], routes.terminals[1]).assert_ok(),
		)
	});
	let node = EdgeEndpoint::Nested(
		QualifiedAtomId::new([routes.owners[0], routes.portal], routes.terminals[0]).assert_ok(),
	);
	let scopes = routes.owners.map(|container| {
		*witness
			.arena
			.scope_by_slot()
			.iter()
			.find(|(_, path)| path.as_ref() == [container, routes.portal].as_slice())
			.assert_some()
			.0
	});
	assert_ne!(scopes[0], scopes[1]);
	let raw = |atom: AtomId| {
		DiskAtomRef::from_parts(atom.kind().assert_some(), atom.slot()).assert_ok().raw()
	};
	let mut expected = OccurrenceState::edge(
		EdgeTopology::directed(
			[endpoints[1].clone(), node.clone(), endpoints[0].clone(), endpoints[1].clone()],
			[EdgeEndpoint::Local(routes.fixture.nodes[0])],
		),
		None,
		Some(child),
	);
	for side in 0..2 {
		if side == 1 {
			expected = OccurrenceState::edge(
				EdgeTopology::directed([EdgeEndpoint::Local(routes.fixture.nodes[1])], [
					endpoints[0].clone(),
					node.clone(),
					endpoints[1].clone(),
				]),
				None,
				Some(child),
			);
		}
		let mut write = routes.fixture.store.begin();
		write
			.replace_occurrence_state(routes.fixture.graph.id(), &record, expected.clone())
			.assert_ok();
		write.commit().assert_ok();
		let mut rows =
			scopes.map(|scope| (scope, raw(routes.terminals[1]), raw(owner), side)).to_vec();
		rows.sort();
		super::super::super::endpoint_publication_tests::assert_postings(&routes.fixture, &rows);
		assert_retained(&routes.fixture, &files, false);
		witness.assert_unchanged(&routes.fixture);
		assert_bulk_image(&routes.fixture);
		routes.fixture.store.checkpoint(CheckpointMode::Restart).assert_ok();
		routes.fixture = reopen(routes.fixture);
		assert_eq!(
			routes.fixture.store.local_storage_status().assert_some().options().io_mode,
			mode
		);
		let current = routes.fixture.store.inner.current_root();
		assert_eq!(current.membership(routes.fixture.graph.id(), global), Some(owner));
		assert_eq!(
			current.occurrence(routes.fixture.graph.id(), owner).assert_some().state(),
			&expected
		);
		super::super::super::endpoint_publication_tests::assert_postings(&routes.fixture, &rows);
		assert_retained(&routes.fixture, &files, false);
		witness.assert_unchanged(&routes.fixture);
		assert_bulk_image(&routes.fixture);
	}
}
