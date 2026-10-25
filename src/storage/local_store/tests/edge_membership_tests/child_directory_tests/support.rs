//! Concrete containment fixtures shared by directory boundary and recovery
//! tests.

use super::*;
use crate::{
	datastore::{DatabaseRoot, PublicationDelta},
	gql_status::FerrosGrynnResult,
	storage::{
		graph_pages::{SubgraphEntry, build_graph_image, decode_subgraph_directory},
		graph_store::GraphFileSet,
		local_store::{point_graph::GraphUpdatePlan, point_namespace::LabelPlan},
		page::{PageLsn, PageNo},
	},
	values::storage::EncodeContext,
};

/// Owns shared children, direct parents and a deep-only ancestor through
/// reopen.
pub(super) struct DirectoryFixture {
	/// Closed before deleting its directory.
	pub(super) store: Datastore,
	/// Reused and checked after each cold reopen.
	pub(super) options: LocalOptions,
	/// Plain membership targets share existing Base owners.
	pub(super) children: [GraphRef; 2],
	/// The first parent spans the requested page boundary.
	pub(super) parents: [GraphRef; 2],
	/// Holds A directly and appears many times below the first parent.
	pub(super) intermediary: GraphRef,
	/// Contains descendants without copying their counters directly.
	pub(super) ancestor: GraphRef,
	/// Both owners exist before any tested membership commit.
	pub(super) pending: [RecordId; 2],
	/// Outlives the datastore and all exact-version file handles.
	pub(super) directory: TestDirectory,
}

/// A occupies the first two direct slots; B is the final EDGE-owned entry.
pub(super) fn fixture(io_mode: IoMode, entries: usize) -> DirectoryFixture {
	assert!(entries >= 3);
	let directory = TestDirectory::new();
	let options = LocalOptions {
		io_mode,
		..LocalOptions::default()
	};
	let store = Datastore::create_local(&directory.0, options).assert_ok();
	store.inner.maintenance.shutdown();
	assert_eq!(store.local_storage_status().assert_some().options().io_mode, io_mode);
	let mut seed = store.begin();
	seed.define_class(ClassSchema::new("node", AtomKind::Node, []).assert_ok()).assert_ok();
	seed.define_class(ClassSchema::new("edge", AtomKind::Edge, []).assert_ok()).assert_ok();
	let children = ["a", "b"].map(|name| seed.define_graph(name, GraphKind::Graph).assert_ok());
	let parents = ["parent", "other_parent"]
		.map(|name| seed.define_graph(name, GraphKind::Metagraph).assert_ok());
	let intermediary = seed.define_graph("intermediary", GraphKind::Metagraph).assert_ok();
	let ancestor = seed.define_graph("ancestor", GraphKind::Metagraph).assert_ok();
	let existing = "node:existing".parse::<RecordId>().assert_ok();
	let pending =
		["node:pending0", "node:pending1"].map(|name| name.parse::<RecordId>().assert_ok());
	for record in [&existing, &pending[0], &pending[1]] {
		seed.create_atom(record.clone(), PropertyMap::new()).assert_ok();
	}
	for child in &children {
		seed.attach_node(child.id(), &existing, None, None).assert_ok();
	}
	seed.attach_node(intermediary.id(), &existing, None, Some(children[0].id())).assert_ok();
	seed.attach_node(parents[1].id(), &existing, None, Some(children[0].id())).assert_ok();
	let mut first = None;
	for index in 0..entries - 1 {
		let record = format!("node:owner{index}").parse::<RecordId>().assert_ok();
		seed.create_atom(record.clone(), PropertyMap::new()).assert_ok();
		let child = if index < 2 {
			&children[0]
		} else {
			&intermediary
		};
		let local = seed.attach_node(parents[0].id(), &record, None, Some(child.id())).assert_ok();
		first.get_or_insert(local);
	}
	let connector = "edge:container".parse::<RecordId>().assert_ok();
	seed.create_atom(connector.clone(), PropertyMap::new()).assert_ok();
	let endpoint = first.assert_some();
	seed.attach_edge(
		parents[0].id(),
		&connector,
		None,
		EdgeTopology::directed([endpoint], [endpoint]),
		None,
		Some(children[1].id()),
	)
	.assert_ok();
	seed.attach_node(ancestor.id(), &existing, None, Some(parents[0].id())).assert_ok();
	seed.commit().assert_ok();
	store.checkpoint(CheckpointMode::Restart).assert_ok();
	reopen(DirectoryFixture {
		store,
		options,
		children,
		parents,
		intermediary,
		ancestor,
		pending,
		directory,
	})
}

/// Restores the same concrete backend and excludes unrelated background work.
pub(super) fn reopen(mut fixture: DirectoryFixture) -> DirectoryFixture {
	drop(fixture.store);
	fixture.store = Datastore::open_local(&fixture.directory.0, fixture.options).assert_ok();
	fixture.store.inner.maintenance.shutdown();
	assert_eq!(
		fixture.store.local_storage_status().assert_some().options().io_mode,
		fixture.options.io_mode
	);
	fixture
}

/// Looks up current file authority without permitting a materialized fallback.
pub(super) fn files(fixture: &DirectoryFixture, graph: GraphId) -> GraphFileSet {
	local(&fixture.store)
		.state
		.lock()
		.bindings
		.graphs
		.iter()
		.find(|binding| binding.graph_id == graph)
		.assert_some()
		.files
}

/// Reads the complete current directory as ordered page/link/entry tuples.
pub(super) fn directory(
	fixture: &DirectoryFixture,
	graph: GraphId,
) -> Vec<(Option<PageNo>, Vec<SubgraphEntry>)> {
	let store = local(&fixture.store);
	let state = store.state.lock();
	let files =
		state.bindings.graphs.iter().find(|binding| binding.graph_id == graph).assert_some().files;
	let ceiling = state.graph_superblocks.get(&graph).assert_some().subgraphs_pages;
	(0..ceiling)
		.map(|number| {
			let number = PageNo::new(number);
			let page = store
				.read_update_page(&state, PageKey::new(files.space(), files.subgraphs, number))
				.assert_ok();
			decode_subgraph_directory(&page, number).assert_ok()
		})
		.collect()
}

/// The independent bulk writer computes all four counters from accepted data.
pub(super) fn assert_directories(fixture: &DirectoryFixture) {
	let root = fixture.store.inner.current_root();
	for graph in fixture.parents.iter().chain([&fixture.intermediary, &fixture.ancestor]) {
		let mut context = EncodeContext::new(root.dictionary.as_ref().clone());
		let expected =
			build_graph_image(&root, graph.id(), &mut context, PageLsn::ZERO).assert_ok();
		let expected = expected
			.subgraphs
			.iter()
			.enumerate()
			.map(|(number, page)| {
				decode_subgraph_directory(page, PageNo::new(u64::try_from(number).assert_ok()))
					.assert_ok()
			})
			.collect::<Vec<_>>();
		assert_eq!(directory(fixture, graph.id()), expected);
	}
}

/// Prepares a real candidate independently of commit so duplicate page keys are
/// visible.
pub(super) fn prepared(fixture: &DirectoryFixture, targets: &[(usize, usize)]) -> GraphUpdatePlan {
	let (before, after, delta) = roots(fixture, targets);
	prepare(local(&fixture.store), &before, &after, &delta).assert_ok()
}

/// Builds explicit final replay inputs without publishing or bypassing shared
/// budgets.
pub(super) fn roots(
	fixture: &DirectoryFixture,
	targets: &[(usize, usize)],
) -> (DatabaseRoot, DatabaseRoot, PublicationDelta) {
	let store = local(&fixture.store);
	let mut before = fixture.store.inner.current_root().as_ref().clone();
	store.hydrate_statistics(&mut before).assert_ok();
	let mut after = before.clone();
	let mut delta = PublicationDelta {
		records: BTreeSet::new(),
		global_slots: BTreeSet::new(),
		overlays: BTreeSet::new(),
		incidence: BTreeSet::new(),
		memberships: BTreeSet::new(),
		point_graph_only: true,
	};
	for &(child, record) in targets {
		let graph = fixture.children[child].id();
		let record = &fixture.pending[record];
		let global = before.resolve_atom(record).assert_some();
		let local = after.attach_node(graph, global, None, None).assert_ok();
		delta.records.insert(record.clone());
		delta.memberships.insert((graph, global));
		delta.incidence.insert((graph, local));
	}
	(before, after, delta)
}

/// Shares the production label coordinator and final replay roots in
/// direct-plan tests.
///
/// # Errors
/// Preserves production admission, source codec and preparation failures.
pub(super) fn prepare(
	store: &LocalStore,
	before: &DatabaseRoot,
	after: &DatabaseRoot,
	delta: &PublicationDelta,
) -> FerrosGrynnResult<GraphUpdatePlan> {
	let state = store.state.lock();
	let mut labels = LabelPlan::build(store, &state, before, after, delta)?;
	let mut context = crate::storage::atom_store_image::value_context(
		&after.schema,
		after.dictionary.as_ref().clone(),
	)?;
	GraphUpdatePlan::build(store, &state, before, after, delta, &mut labels, &mut context)
}

/// Commits the same explicit targets used by the direct-plan oracle.
pub(super) fn candidate(fixture: &DirectoryFixture, targets: &[(usize, usize)]) -> Transaction {
	let mut write = fixture.store.begin();
	for &(child, record) in targets {
		write
			.attach_node(fixture.children[child].id(), &fixture.pending[record], None, None)
			.assert_ok();
	}
	write
}
