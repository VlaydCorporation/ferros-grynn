//! Independently captured child state shared by resource and recovery checks.

use super::{
	super::{
		child_presence_tests::{assert_current, fixture, record, with_child},
		child_scope_remap_tests,
	},
	*,
};
use crate::{
	datastore::{GlobalAtomId, GraphOccurrence, OccurrenceState, Transaction},
	graph::{EdgeEndpoint, GraphId, QualifiedAtomId},
	storage::page::AlignedPage,
};

/// Every operation starts from independent accepted topology and weight.
pub(super) struct Case {
	/// A reopened public parent retains the accepted direct-child layout.
	pub(super) fixture: Fixture,
	/// Public record selection stays separate from local numeric identity.
	pub(super) kind: AtomKind,
	/// Full global and local owner identities never change during this edit.
	pub(super) owner: (GlobalAtomId, AtomId),
	/// Only the optional child differs between the accepted and final states.
	pub(super) states: (OccurrenceState, OccurrenceState),
	/// Existing capacity is an upper bound unless one new entry is required.
	pub(super) capacity: u64,
	/// Literal precommit owners distinguish survivor remap from zero/one
	/// presence.
	pub(super) survivors: Option<([GraphOccurrence; 2], [GraphId; 2])>,
	/// Captured full connector state independently specifies both EDGE-side
	/// postings.
	pub(super) qualified_edge: Option<GraphOccurrence>,
}

impl Case {
	/// Captures the source once, before creating or committing the write.
	pub(super) fn new(mode: IoMode, kind: AtomKind, attach: bool) -> Self {
		Self::new_with_family(mode, kind, attach, GraphKind::Metagraph)
	}

	/// Runs the same fixture contract for the selected public parent family.
	pub(super) fn new_with_family(
		mode: IoMode,
		kind: AtomKind,
		attach: bool,
		family: GraphKind,
	) -> Self {
		let (fixture, child) = fixture(mode, kind, attach, family);
		Self::capture(fixture, kind, attach, child, None)
	}

	/// Captures both full owners before the tested transaction can exist.
	pub(super) fn surviving(mode: IoMode, kind: AtomKind, attach: bool) -> Self {
		Self::surviving_with_family(mode, kind, attach, GraphKind::Metagraph)
	}

	/// Runs the same fixture contract for the selected public parent family.
	pub(super) fn surviving_with_family(
		mode: IoMode,
		kind: AtomKind,
		attach: bool,
		family: GraphKind,
	) -> Self {
		let (fixture, records, children) =
			child_scope_remap_tests::fixture_with_family(mode, kind, attach, family);
		assert_eq!(records[0], record(kind));
		let root = fixture.store.inner.current_root();
		let owners = records.each_ref().map(|record| {
			let global = root.resolve_atom(record).assert_some();
			let owner = root.membership(fixture.graph.id(), global).assert_some();
			root.occurrence(fixture.graph.id(), owner).assert_some().clone()
		});
		drop(root);
		Self::capture(fixture, kind, attach, children[0], Some((owners, children)))
	}

	/// The original NODE-terminal fixture keeps the same accepted topology.
	pub(super) fn qualified(mode: IoMode, kind: AtomKind, attach: bool) -> Self {
		Self::qualified_with_family(mode, kind, attach, GraphKind::Metagraph)
	}

	/// Runs the same fixture contract for the selected public parent family.
	pub(super) fn qualified_with_family(
		mode: IoMode,
		kind: AtomKind,
		attach: bool,
		family: GraphKind,
	) -> Self {
		Self::qualified_terminal(mode, kind, attach, AtomKind::Node, family)
	}

	/// One Base EDGE belongs to both the public leaf and its root connector.
	pub(super) fn qualified_edge(mode: IoMode, kind: AtomKind, attach: bool) -> Self {
		Self::qualified_edge_with_family(mode, kind, attach, GraphKind::Metagraph)
	}

	/// Runs the same fixture contract for the selected public parent family.
	pub(super) fn qualified_edge_with_family(
		mode: IoMode,
		kind: AtomKind,
		attach: bool,
		family: GraphKind,
	) -> Self {
		Self::qualified_terminal(mode, kind, attach, AtomKind::Edge, family)
	}

	/// Both terminal kinds preserve records[3] for the independent replay
	/// writer.
	fn qualified_terminal(
		mode: IoMode,
		kind: AtomKind,
		attach: bool,
		terminal_kind: AtomKind,
		family: GraphKind,
	) -> Self {
		let mut case = Self::surviving_with_family(mode, kind, attach, family);
		let (owners, children) = case.survivors.as_ref().assert_some();
		let survivor = owners[1].local_id();
		let child = children[1];
		let root = case.fixture.store.inner.current_root();
		let terminals = root
			.graph(child)
			.assert_some()
			.occurrences()
			.filter(|owner| owner.local_id().is_node())
			.map(GraphOccurrence::local_id)
			.collect::<Vec<_>>();
		assert_eq!(terminals.len(), 1);
		drop(root);
		let record = case.fixture.records[4].clone();
		let mut setup = case.fixture.store.begin();
		let terminal = if terminal_kind == AtomKind::Edge {
			setup
				.attach_edge(
					child,
					&record,
					None,
					EdgeTopology::directed([terminals[0]], [terminals[0]]),
					None,
					None,
				)
				.assert_ok()
		} else {
			assert_eq!(terminal_kind, AtomKind::Node);
			terminals[0]
		};
		let nested = EdgeEndpoint::Nested(QualifiedAtomId::new([survivor], terminal).assert_ok());
		let endpoints = (0..2050)
			.map(|index| {
				if index % 2 == 0 {
					nested.clone()
				} else {
					EdgeEndpoint::Local(case.fixture.nodes[0])
				}
			})
			.collect::<Vec<_>>();
		let topology = EdgeTopology::directed(endpoints, [nested.clone(), nested.clone(), nested]);
		let connector = setup
			.attach_edge(
				case.fixture.graph.id(),
				&record,
				None,
				topology,
				Some(17_i64.into()),
				None,
			)
			.assert_ok();
		assert_eq!(
			hot_slot_address(connector.slot()).assert_ok().0,
			hot_slot_address(survivor.slot()).assert_ok().0
		);
		setup.commit().assert_ok();
		case.fixture.store.checkpoint(CheckpointMode::Restart).assert_ok();
		case.fixture = reopen(case.fixture);
		let root = case.fixture.store.inner.current_root();
		let global = root.resolve_atom(&record).assert_some();
		assert_eq!(root.membership(case.fixture.graph.id(), global), Some(connector));
		if terminal_kind == AtomKind::Edge {
			assert_eq!(root.membership(child, global), Some(terminal));
			case.qualified_edge =
				Some(root.occurrence(case.fixture.graph.id(), connector).assert_some().clone());
		}
		drop(root);
		case.check(false);
		case
	}

	/// Shared capture preserves the existing presence inputs and selected
	/// states.
	fn capture(
		fixture: Fixture,
		kind: AtomKind,
		attach: bool,
		child: GraphId,
		survivors: Option<([GraphOccurrence; 2], [GraphId; 2])>,
	) -> Self {
		let root = fixture.store.inner.current_root();
		let global = root.resolve_atom(&record(kind)).assert_some();
		let owner = root.membership(fixture.graph.id(), global).assert_some();
		let before = root.occurrence(fixture.graph.id(), owner).assert_some().state().clone();
		let after = with_child(&before, attach.then_some(child));
		let capacity = local(&fixture.store)
			.state
			.lock()
			.graph_superblocks
			.get(&fixture.graph.id())
			.assert_some()
			.subgraph_capacity;
		drop(root);
		Self {
			fixture,
			kind,
			owner: (global, owner),
			states: (before, after),
			capacity,
			survivors,
			qualified_edge: None,
		}
	}

	/// Submits an independent copy so later assertions cannot read the
	/// candidate.
	pub(super) fn change(&self) -> Transaction {
		let mut write = self.fixture.store.begin();
		write
			.replace_occurrence_state(
				self.fixture.graph.id(),
				&record(self.kind),
				self.states.1.clone(),
			)
			.assert_ok();
		write
	}

	/// Checks literal arena/directory/Hot state and an independent bulk image.
	pub(super) fn check(&self, accepted: bool) {
		if let Some((owners, children)) = &self.survivors {
			let source_present = owners[0].child_graph().is_some();
			let present = if accepted {
				!source_present
			} else {
				source_present
			};
			if self.qualified_edge.is_some() {
				child_scope_remap_tests::assert_current_with_counts(
					&self.fixture,
					owners,
					*children,
					present,
					self.capacity,
					[(1, 0, 1, 0), (1, 1, 1, 1)],
				);
				self.assert_endpoint(owners, present);
			} else {
				child_scope_remap_tests::assert_current(
					&self.fixture,
					owners,
					*children,
					present,
					self.capacity,
				);
			}
		} else {
			assert_current(
				&self.fixture,
				self.owner.1,
				self.owner.0,
				if accepted {
					&self.states.1
				} else {
					&self.states.0
				},
				self.capacity,
			);
		}
		assert_bulk_image(&self.fixture);
	}

	/// Literal two-row keys are independent of the bulk graph-image oracle.
	fn assert_endpoint(&self, owners: &[GraphOccurrence; 2], present: bool) {
		let captured = self.qualified_edge.as_ref().assert_some();
		let root = self.fixture.store.inner.current_root();
		assert_eq!(root.occurrence(self.fixture.graph.id(), captured.local_id()), Some(captured));
		let topology = match captured.state().view() {
			crate::datastore::OccurrenceStateRef::Edge {
				topology,
				..
			} => Some(topology),
			crate::datastore::OccurrenceStateRef::Node {
				..
			} => None,
		}
		.assert_some();
		assert_eq!((topology.source().len(), topology.target().len()), (2050, 3));
		let route = match topology.source().first().assert_some() {
			EdgeEndpoint::Nested(route) => Some(route),
			EdgeEndpoint::Local(_) => None,
		}
		.assert_some();
		assert_eq!(route.scope(), &[owners[1].local_id()]);
		assert_eq!(route.atom().kind(), Some(AtomKind::Edge));
		let scope = if present {
			2
		} else {
			1
		};
		let terminal =
			crate::values::storage::DiskAtomRef::from_parts(AtomKind::Edge, route.atom().slot())
				.assert_ok();
		let connector = crate::values::storage::DiskAtomRef::from_parts(
			AtomKind::Edge,
			captured.local_id().slot(),
		)
		.assert_ok();
		super::super::super::endpoint_publication_tests::assert_postings(&self.fixture, &[
			(scope, terminal.raw(), connector.raw(), 0),
			(scope, terminal.raw(), connector.raw(), 1),
		]);
		let store = local(&self.fixture.store);
		let state = store.state.lock();
		let binding = state
			.bindings
			.graphs
			.iter()
			.find(|binding| binding.graph_id == self.fixture.graph.id())
			.assert_some()
			.edge_endpoint
			.assert_some();
		let space = crate::datastore::DataSpaceId::Graph(self.fixture.graph.id());
		let capability = root
			.schema
			.graph(self.fixture.graph.id())
			.assert_some()
			.edge_endpoint_index()
			.assert_some();
		super::super::qualified_edge_scope_tests::assert_lookup(
			|number, page| {
				store.read_update_page_into(
					&state,
					PageKey::new(space, binding.file_id, number),
					page,
				)
			},
			self.fixture.graph.id(),
			capability,
			root.commit_ts,
			store.visible_pages(&state, space, binding.file_id).assert_ok(),
			(scope, terminal, &[(connector, false), (connector, true)]),
		);
	}

	/// Captures the immutable owner used by both runtime physical authorities.
	pub(super) fn arena(&self) -> GraphArenaBindings {
		local(&self.fixture.store)
			.state
			.lock()
			.bindings
			.graphs
			.iter()
			.find(|binding| binding.graph_id == self.fixture.graph.id())
			.assert_some()
			.arena
			.clone()
	}

	/// An unsuccessful preparation may not replace either accepted owner.
	pub(super) fn assert_owners(&self, source: &GraphArenaBindings) {
		let state = local(&self.fixture.store).state.lock();
		assert!(
			state
				.bindings
				.graphs
				.iter()
				.find(|binding| binding.graph_id == self.fixture.graph.id())
				.assert_some()
				.arena
				.same_owner(source)
		);
		assert!(
			state
				._database_catalog
				.graphs
				.iter()
				.find(|entry| entry.id == self.fixture.graph.id())
				.assert_some()
				.arena
				.same_owner(source)
		);
	}

	/// Exact Hot and page0 bytes protect the source independently of map
	/// equality.
	pub(super) fn pages(&self) -> [(PageKey, Box<AlignedPage>); 2] {
		let store = local(&self.fixture.store);
		let state = store.state.lock();
		let files = state
			.bindings
			.graphs
			.iter()
			.find(|binding| binding.graph_id == self.fixture.graph.id())
			.assert_some()
			.files;
		let file = if self.kind == AtomKind::Node {
			files.nodes
		} else {
			files.edges
		};
		[
			PageKey::new(files.space(), file, hot_slot_address(self.owner.1.slot()).assert_ok().0),
			PageKey::new(files.space(), files.subgraphs, PageNo::new(0)),
		]
		.map(|key| (key, store.read_update_page(&state, key).assert_ok()))
	}

	/// Every captured source page remains byte-identical after rejection.
	pub(super) fn assert_pages(&self, previous: &[(PageKey, Box<AlignedPage>)]) {
		let current = self.all_pages();
		assert_eq!(current.len(), previous.len());
		for ((old_key, old), (key, page)) in previous.iter().zip(&current) {
			assert_eq!(key, old_key);
			assert_eq!(page.as_bytes(), old.as_bytes());
		}
	}

	/// Rejected preparation preserves every coalesced source Hot page, not just
	/// the selected owner.
	pub(super) fn all_pages(&self) -> Vec<(PageKey, Box<AlignedPage>)> {
		let mut pages = self.pages().into_iter().collect::<Vec<_>>();
		if let Some((owners, _)) = &self.survivors {
			let store = local(&self.fixture.store);
			let state = store.state.lock();
			let binding = state
				.bindings
				.graphs
				.iter()
				.find(|binding| binding.graph_id == self.fixture.graph.id())
				.assert_some();
			let files = binding.files;
			let number = hot_slot_address(owners[1].local_id().slot()).assert_ok().0;
			let key = PageKey::new(files.space(), files.edges, number);
			if !pages.iter().any(|(existing, _)| *existing == key) {
				pages.push((key, store.read_update_page(&state, key).assert_ok()));
			}
			// Qualified sources retain all accepted overflow bytes on
			// rejection.
			let count = state
				.graph_superblocks
				.get(&self.fixture.graph.id())
				.assert_some()
				.cross_level_adjacency_overflow_pages;
			for number in 0..count {
				let key =
					PageKey::new(files.space(), files.cross_level_adjacency, PageNo::new(number));
				pages.push((key, store.read_update_page(&state, key).assert_ok()));
			}
			if let Some(endpoint) = binding.edge_endpoint {
				let count =
					store.visible_pages(&state, files.space(), endpoint.file_id).assert_ok();
				// Both literal connector sides fit one leaf behind the source
				// header.
				assert_eq!(count, 2);
				for number in 0..count {
					let key = PageKey::new(files.space(), endpoint.file_id, PageNo::new(number));
					pages.push((key, store.read_update_page(&state, key).assert_ok()));
				}
			}
		}
		pages
	}
}
