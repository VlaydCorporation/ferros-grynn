//! Owned accepted EDGE evidence is independent of the candidate publisher.

use super::{
	transition_rewire_tests::{assert_state, portal, rewrite},
	*,
};
use crate::{
	datastore::{GlobalAtomId, GraphOccurrence, OccurrenceState, OccurrenceStateRef},
	graph::{EdgeEndpoint, GraphId},
	storage::{
		catalog::FileCatalogEntry,
		graph_pages::{EdgeHotSlot, GraphArenaBindings, SubgraphEntry, decode_subgraph_directory},
		graph_store::GraphFileSet,
		page::AlignedPage,
		types::{DataFileId, Lsn},
	},
};

/// Copies only the unchanged EDGE state fields and substitutes its child.
pub(super) fn edge_state(fixture: &Fixture, child: GraphId) -> OccurrenceState {
	let (_, owner, _) = portal(fixture);
	let root = fixture.store.inner.current_root();
	let state = root.occurrence(fixture.graph.id(), owner).assert_some().state();
	with_child(state, child)
}

/// Keeps the expected roles and weight anchored in the captured source.
fn with_child(state: &OccurrenceState, child: GraphId) -> OccurrenceState {
	let projected = if let OccurrenceStateRef::Edge {
		topology,
		weight,
		..
	} = state.view()
	{
		Some(OccurrenceState::edge(topology.clone(), *weight, Some(child)))
	} else {
		None
	};
	projected.assert_some()
}

/// Uses the actual production transaction without a retry or extra metadata.
pub(super) fn edge_change(fixture: &Fixture, child: GraphId) -> Transaction {
	rewrite(fixture, edge_state(fixture, child))
}

/// Full source identities and all parent data pages remain retained.
pub(super) struct Source {
	/// Source logical metadata is independent of the post-commit actual state.
	occurrence: GraphOccurrence,
	/// Full Base and local owners never change during a child-only rebind.
	pub(super) owner: (GlobalAtomId, AtomId),
	/// Accepted child and immutable canonical scope maps.
	pub(super) child: GraphId,
	/// Numeric slot remains the same for every tested leaf replacement.
	pub(super) slot: u32,
	/// Old pins retain this accepted immutable owner.
	pub(super) arena: GraphArenaBindings,
	/// No graph or AtomStore file is replaced by the candidate.
	pub(super) files: Vec<FileCatalogEntry>,
	/// Concrete parent data files define the retained byte keys.
	pub(super) graph_files: GraphFileSet,
	/// Complete decoded source Hot, including metadata and both role layouts.
	pub(super) hot: EdgeHotSlot,
	/// Ordered direct directory is compared independently of the bulk oracle.
	directory: Vec<SubgraphEntry>,
	/// Containment capacity/count/page ceiling cannot move for a leaf rebind.
	ceilings: (u64, u64, u64),
	/// Hot, adjacency and overlay pages retain their exact checksummed bytes.
	pages: Vec<(PageKey, Box<AlignedPage>)>,
	/// These source keys must receive no newer frames during publication.
	boundary: Lsn,
}

impl Source {
	/// A role pointer must resolve inside the captured accepted page set.
	pub(super) fn page(&self, key: PageKey) -> &AlignedPage {
		self.pages.iter().find(|(candidate, _)| *candidate == key).assert_some().1.as_ref()
	}

	/// Captures genuine accepted data after all fixture and capability setup.
	pub(super) fn capture(fixture: &Fixture) -> Self {
		let (global, owner, child) = portal(fixture);
		let root = fixture.store.inner.current_root();
		let store = local(&fixture.store);
		let state = store.state.lock();
		let binding = state
			.bindings
			.graphs
			.iter()
			.find(|binding| binding.graph_id == fixture.graph.id())
			.assert_some();
		let graph_files = binding.files;
		let header = state.graph_superblocks.get(&fixture.graph.id()).assert_some();
		let (number, within) = hot_slot_address(owner.slot()).assert_ok();
		let hot = read_edge_hot_slot(
			&store
				.read_update_page(
					&state,
					PageKey::new(graph_files.space(), graph_files.edges, number),
				)
				.assert_ok(),
			number,
			within,
		)
		.assert_ok();
		let mut pages = Vec::new();
		for (file, count) in [
			(graph_files.nodes, header.node_hot_pages),
			(graph_files.edges, header.edge_hot_pages),
			(graph_files.adjacency, header.adjacency_overflow_pages),
			(graph_files.cross_level_adjacency, header.cross_level_adjacency_overflow_pages),
			(graph_files.overlays, header.overlays_pages),
		] {
			for number in 0..count {
				let key = PageKey::new(graph_files.space(), file, PageNo::new(number));
				pages.push((key, store.read_update_page(&state, key).assert_ok()));
			}
		}
		let (next, directory) = decode_subgraph_directory(
			&store
				.read_update_page(
					&state,
					PageKey::new(graph_files.space(), graph_files.subgraphs, PageNo::new(0)),
				)
				.assert_ok(),
			PageNo::new(0),
		)
		.assert_ok();
		assert_eq!(next, None);
		assert_eq!(directory.len(), 2);
		let slot = binding.arena.direct_child_slot(&root, fixture.graph.id(), owner).assert_ok();
		let source = Self {
			occurrence: root.occurrence(fixture.graph.id(), owner).assert_some().clone(),
			owner: (global, owner),
			child,
			slot,
			arena: binding.arena.clone(),
			files: state.file_catalog.files.clone(),
			graph_files,
			hot,
			directory,
			ceilings: (header.subgraph_capacity, header.subgraph_count, header.subgraphs_pages),
			pages,
			boundary: store.wal.snapshot().visible_lsn(),
		};
		assert_eq!(source.ceilings.2, 1);
		source
	}

	/// Requires exact retained bytes, scope owners and explicit final counters.
	pub(super) fn assert_current(
		&self,
		fixture: &Fixture,
		child: GraphId,
		counts: (u64, u64, u32, u32),
	) {
		assert_state(
			fixture,
			self.owner.0,
			self.owner.1,
			&with_child(self.occurrence.state(), child),
		);
		let root = fixture.store.inner.current_root();
		let owner = root.occurrence(fixture.graph.id(), self.owner.1).assert_some();
		assert_eq!(owner.label(), self.occurrence.label());
		assert_eq!(owner.overlay(), self.occurrence.overlay());
		let store = local(&fixture.store);
		let state = store.state.lock();
		let binding = state
			.bindings
			.graphs
			.iter()
			.find(|binding| binding.graph_id == fixture.graph.id())
			.assert_some();
		assert_eq!(state.file_catalog.files, self.files);
		assert_eq!(file_ids(binding.files), file_ids(self.graph_files));
		assert_eq!(
			(binding.files.graph_id, binding.files.membership_root),
			(self.graph_files.graph_id, self.graph_files.membership_root)
		);
		let mut graphs = self.arena.graph_by_slot().clone();
		assert_eq!(graphs.insert(self.slot, child), Some(self.child));
		assert_eq!(binding.arena.graph_by_slot(), &graphs);
		assert_eq!(binding.arena.scope_by_slot(), self.arena.scope_by_slot());
		let catalog = state
			._database_catalog
			.graphs
			.iter()
			.find(|graph| graph.id == fixture.graph.id())
			.assert_some();
		assert!(binding.arena.same_owner(&catalog.arena));
		let mut directory = self.directory.clone();
		let selected = directory.iter_mut().find(|entry| entry.slot == self.slot).assert_some();
		(
			selected.node_capacity,
			selected.edge_capacity,
			selected.node_count,
			selected.edge_count,
		) = counts;
		let key = PageKey::new(binding.files.space(), binding.files.subgraphs, PageNo::new(0));
		assert_eq!(
			decode_subgraph_directory(
				&store.read_update_page(&state, key).assert_ok(),
				PageNo::new(0)
			)
			.assert_ok(),
			(None, directory)
		);
		let header = state.graph_superblocks.get(&fixture.graph.id()).assert_some();
		assert_eq!(
			(header.subgraph_capacity, header.subgraph_count, header.subgraphs_pages),
			self.ceilings
		);
		for (key, page) in &self.pages {
			assert_eq!(
				store.read_update_page(&state, *key).assert_ok().as_bytes(),
				page.as_bytes()
			);
		}
		let keys = self.pages.iter().map(|(key, _)| *key).collect::<Vec<_>>();
		assert!(
			store
				.wal
				.visible_frames(&keys, store.wal.snapshot())
				.frames()
				.iter()
				.flatten()
				.all(|frame| frame.frame_lsn() <= self.boundary)
		);
		assert_eq!(self.arena.graph_by_slot().get(&self.slot), Some(&self.child));
	}
}

/// The storage file-set deliberately has no semantic equality implementation.
fn file_ids(files: GraphFileSet) -> [DataFileId; 9] {
	[
		files.superblock,
		files.nodes,
		files.edges,
		files.adjacency,
		files.cross_level_adjacency,
		files.overlays,
		files.subgraphs,
		files.labels,
		files.membership,
	]
}

/// Resolves only literal single-owner fixture paths, without production
/// encoding.
pub(super) fn endpoint_row(source: &Source, endpoint: &EdgeEndpoint) -> (u32, DiskAtomRef) {
	let (scope, atom) = if let EdgeEndpoint::Nested(route) = endpoint {
		assert_eq!(route.scope().len(), 1);
		let slot = *source
			.arena
			.scope_by_slot()
			.iter()
			.find(|(_, scope)| scope.as_ref() == route.scope())
			.assert_some()
			.0;
		(slot, route.atom())
	} else {
		(0, endpoint.local_id().assert_some())
	};
	(scope, DiskAtomRef::from_parts(atom.kind().assert_some(), atom.slot()).assert_ok())
}
