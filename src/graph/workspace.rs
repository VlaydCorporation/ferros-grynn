use std::{
	borrow::Cow,
	collections::BTreeSet,
	fmt::{self, Debug, Formatter},
};

use super::{AtomId, GraphKind, GraphMut, GraphRead};
use crate::{
	ecs::WorldComponents,
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	values::runtime::{PropertyMap, Uuid},
};

/// ECS sidecar bound to one graph identity and revision.
pub struct GraphWorkspace<G> {
	/// Graph associated with this graph workspace.
	graph: G,
	/// Components associated with this graph workspace.
	components: WorldComponents,
	/// Derived associated with this graph workspace.
	derived: WorldComponents,
	/// Bound identity associated with this graph workspace.
	bound_identity: Uuid,
	/// Bound revision associated with this graph workspace.
	bound_revision: u64,
}

impl<G: GraphRead> GraphWorkspace<G> {
	/// Constructs `GraphWorkspace<G>` from the supplied initial state.
	pub fn new(graph: G) -> Self {
		let bound_identity = graph.identity();
		let bound_revision = graph.revision();
		Self {
			graph,
			components: WorldComponents::new(),
			derived: WorldComponents::new(),
			bound_identity,
			bound_revision,
		}
	}

	/// Borrows the stored graph of this `GraphWorkspace<G>`.
	pub const fn graph(&self) -> &G {
		&self.graph
	}

	/// Returns the stored bound identity of this `GraphWorkspace<G>`.
	pub const fn bound_identity(&self) -> Uuid {
		self.bound_identity
	}

	/// Returns the stored bound revision of this `GraphWorkspace<G>`.
	pub const fn bound_revision(&self) -> u64 {
		self.bound_revision
	}

	/// Returns components for `GraphWorkspace<G>`.
	///
	/// # Errors
	///
	/// Propagates errors returned by the underlying operation.
	pub fn components(&self) -> FerrosGrynnResult<&WorldComponents> {
		self.validate_binding()?;
		Ok(&self.components)
	}

	/// Returns derived components for `GraphWorkspace<G>`.
	///
	/// # Errors
	///
	/// Propagates errors returned by the underlying operation.
	pub fn derived_components(&self) -> FerrosGrynnResult<&WorldComponents> {
		self.validate_binding()?;
		Ok(&self.derived)
	}

	/// Inserts component for `GraphWorkspace<G>`.
	///
	/// # Errors
	///
	/// Propagates errors returned by the underlying operation.
	pub fn insert_component<T>(&mut self, id: AtomId, component: T) -> FerrosGrynnResult<Option<T>>
	where
		T: 'static + Send + Sync,
	{
		self.validate_atom(id)?;
		self.components.insert(id, component)
	}

	/// Returns component for `GraphWorkspace<G>`.
	///
	/// # Errors
	///
	/// Propagates errors returned by the underlying operation.
	pub fn component<T>(&self, id: AtomId) -> FerrosGrynnResult<Option<&T>>
	where
		T: 'static + Send + Sync,
	{
		self.validate_atom(id)?;
		Ok(self.components.get(id))
	}

	/// Returns component mut for `GraphWorkspace<G>`.
	///
	/// # Errors
	///
	/// Propagates errors returned by the underlying operation.
	pub fn component_mut<T>(&mut self, id: AtomId) -> FerrosGrynnResult<Option<&mut T>>
	where
		T: 'static + Send + Sync,
	{
		self.validate_atom(id)?;
		Ok(self.components.get_mut(id))
	}

	/// Inserts derived for `GraphWorkspace<G>`.
	///
	/// # Errors
	///
	/// Propagates errors returned by the underlying operation.
	pub fn insert_derived<T>(&mut self, id: AtomId, component: T) -> FerrosGrynnResult<Option<T>>
	where
		T: 'static + Send + Sync,
	{
		self.validate_atom(id)?;
		self.derived.insert(id, component)
	}

	/// Returns derived for `GraphWorkspace<G>`.
	///
	/// # Errors
	///
	/// Propagates errors returned by the underlying operation.
	pub fn derived<T>(&self, id: AtomId) -> FerrosGrynnResult<Option<&T>>
	where
		T: 'static + Send + Sync,
	{
		self.validate_atom(id)?;
		Ok(self.derived.get(id))
	}

	/// Runs a canonical graph mutation and synchronizes the ECS sidecar.
	///
	/// Derived pools are revision-specific and are invalidated. Ordinary user
	/// components survive while their complete generation-aware `AtomId`
	/// exists.
	///
	/// # Errors
	///
	/// Propagates errors returned by the underlying operation.
	pub fn mutate<T, MutFn>(&mut self, mutation: MutFn) -> FerrosGrynnResult<T>
	where
		G: GraphMut,
		MutFn: FnOnce(&mut G) -> FerrosGrynnResult<T>,
	{
		self.validate_binding()?;
		let result = mutation(&mut self.graph);
		self.synchronize_after_mutation()?;
		result
	}

	/// Returns map graph for `GraphWorkspace<G>`.
	pub fn map_graph<H, MapFn>(self, map: MapFn) -> GraphWorkspace<H>
	where
		H: GraphRead,
		MapFn: FnOnce(G) -> H,
	{
		let graph = map(self.graph);
		let same_identity = graph.identity() == self.bound_identity;
		let mut components = self.components;
		if !same_identity {
			components.clear();
		}
		GraphWorkspace {
			bound_identity: graph.identity(),
			bound_revision: graph.revision(),
			graph,
			components,
			derived: WorldComponents::new(),
		}
	}

	/// Converts this value into parts for `GraphWorkspace<G>`.
	pub fn into_parts(self) -> (G, WorldComponents) {
		(self.graph, self.components)
	}

	/// Validates binding for `GraphWorkspace<G>`.
	///
	/// # Errors
	///
	/// Returns `graph_workspace_stale` diagnostics.
	pub fn validate_binding(&self) -> FerrosGrynnResult<()> {
		let identity = self.graph.identity();
		let revision = self.graph.revision();
		if identity != self.bound_identity || revision != self.bound_revision {
			return Err(FerrosGrynnError::graph_workspace_stale(
				self.bound_identity.to_string(),
				self.bound_revision,
				identity.to_string(),
				revision,
			));
		}
		Ok(())
	}

	/// Validates atom.
	fn validate_atom(&self, id: AtomId) -> FerrosGrynnResult<()> {
		self.validate_binding()?;
		if !self.graph.contains(id)? {
			return Err(FerrosGrynnError::atom_does_not_exist(
				id.kind().map_or_else(|| "null".to_owned(), |kind| kind.to_string()),
				id.to_string(),
			));
		}
		Ok(())
	}

	/// Returns the synchronize after mutation result.
	fn synchronize_after_mutation(&mut self) -> FerrosGrynnResult<()> {
		let identity = self.graph.identity();
		let revision = self.graph.revision();
		if identity != self.bound_identity {
			self.components.clear();
		} else if revision != self.bound_revision {
			let live = self
				.graph
				.node_ids()?
				.chain(self.graph.edge_ids()?)
				.collect::<FerrosGrynnResult<BTreeSet<_>>>()?;
			self.components.retain_atoms(|id| live.contains(&id));
		}
		if identity != self.bound_identity || revision != self.bound_revision {
			self.derived.clear();
			self.bound_identity = identity;
			self.bound_revision = revision;
		}
		Ok(())
	}
}

/// Conversion into an owned graph workspace with associated ECS components.
pub trait IntoGraphWorkspace: GraphRead + Sized {
	/// Converts this value into workspace for `IntoGraphWorkspace`.
	fn into_workspace(self) -> GraphWorkspace<Self> {
		GraphWorkspace::new(self)
	}
}

impl<G: GraphRead> IntoGraphWorkspace for G {}

impl<G: GraphRead> GraphRead for GraphWorkspace<G> {
	type Edge<'a>
		= G::Edge<'a>
	where
		Self: 'a;
	type EdgeIds<'a>
		= G::EdgeIds<'a>
	where
		Self: 'a;
	type Node<'a>
		= G::Node<'a>
	where
		Self: 'a;
	type NodeIds<'a>
		= G::NodeIds<'a>
	where
		Self: 'a;

	/// Returns identity for `GraphWorkspace<G>`.
	fn identity(&self) -> Uuid {
		self.graph.identity()
	}

	/// Returns revision for `GraphWorkspace<G>`.
	fn revision(&self) -> u64 {
		self.graph.revision()
	}

	/// Returns kind for `GraphWorkspace<G>`.
	fn kind(&self) -> GraphKind {
		self.graph.kind()
	}

	/// Returns properties for `GraphWorkspace<G>`.
	fn properties(&self) -> FerrosGrynnResult<Cow<'_, PropertyMap>> {
		self.graph.properties()
	}

	/// Returns topology constraints for `GraphWorkspace<G>`.
	fn topology_constraints(&self) -> super::GraphTopologyConstraints {
		self.graph.topology_constraints()
	}

	/// Returns node ids for `GraphWorkspace<G>`.
	fn node_ids(&self) -> FerrosGrynnResult<Self::NodeIds<'_>> {
		self.graph.node_ids()
	}

	/// Returns edge ids for `GraphWorkspace<G>`.
	fn edge_ids(&self) -> FerrosGrynnResult<Self::EdgeIds<'_>> {
		self.graph.edge_ids()
	}

	/// Returns node for `GraphWorkspace<G>`.
	fn node(&self, id: AtomId) -> FerrosGrynnResult<Option<Self::Node<'_>>> {
		self.graph.node(id)
	}

	/// Returns edge for `GraphWorkspace<G>`.
	fn edge(&self, id: AtomId) -> FerrosGrynnResult<Option<Self::Edge<'_>>> {
		self.graph.edge(id)
	}

	/// Returns storage mode for `GraphWorkspace<G>`.
	fn storage_mode(&self) -> super::GraphStorageMode {
		self.graph.storage_mode()
	}

	/// Returns snapshot id for `GraphWorkspace<G>`.
	fn snapshot_id(&self) -> super::SnapshotId {
		self.graph.snapshot_id()
	}

	/// Returns graph ref for `GraphWorkspace<G>`.
	fn graph_ref(&self) -> Option<&super::GraphRef> {
		self.graph.graph_ref()
	}
}

impl<G: GraphRead + Debug> Debug for GraphWorkspace<G> {
	/// Formats `GraphWorkspace<G>` using the requested formatter.
	fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
		formatter
			.debug_struct("GraphWorkspace")
			.field("graph", &self.graph)
			.field("identity", &self.bound_identity)
			.field("revision", &self.bound_revision)
			.field("component_pools", &self.components.component_pool_count())
			.field("derived_pools", &self.derived.component_pool_count())
			.finish()
	}
}

#[cfg(test)]
/// Regression tests for workspace behavior.
mod tests {
	use super::{GraphWorkspace, IntoGraphWorkspace};
	#[cfg(test)]
	use crate::test_assertions::AssertResult as _;
	use crate::values::{
		PropertyMap,
		runtime::graph::{Graph, Mutable},
	};

	/// Test component retaining a position inside one graph workspace.
	#[derive(Debug, PartialEq)]
	struct Position(
		/// Coordinate values in the runtime position representation.
		u32,
	);

	/// Test component retaining a rank estimate inside one graph workspace.
	#[derive(Debug, PartialEq)]
	struct PageRank(
		/// Rank estimate attached to an atom by the workspace test.
		f64,
	);

	/// Verifies component mutation does not change graph revision.
	#[test]
	fn component_mutation_does_not_change_graph_revision() {
		let mut graph = Graph::<Mutable>::new();
		let node = graph.add_node(None, PropertyMap::new()).assert_ok();
		let revision = graph.revision();
		let mut workspace = graph.into_workspace();

		workspace.insert_component(node, Position(7)).assert_ok();
		assert_eq!(workspace.bound_revision(), revision);
		assert_eq!(workspace.component::<Position>(node).assert_ok(), Some(&Position(7)));
	}

	/// Verifies canonical mutation invalidates derived and removes dead
	/// components.
	#[test]
	fn canonical_mutation_invalidates_derived_and_removes_dead_components() {
		let mut graph = Graph::<Mutable>::new();
		let node = graph.add_node(None, PropertyMap::new()).assert_ok();
		let mut workspace = GraphWorkspace::new(graph);
		workspace.insert_component(node, Position(1)).assert_ok();
		workspace.insert_derived(node, PageRank(0.5)).assert_ok();

		workspace.mutate(|graph| graph.remove_atom(node)).assert_ok();
		assert!(workspace.components().assert_ok().get::<Position>(node).is_none());
		assert_eq!(workspace.derived_components().assert_ok().component_pool_count(), 0);
	}

	/// Verifies map graph moves user components but discards derived pools.
	#[test]
	fn map_graph_moves_user_components_but_discards_derived_pools() {
		let mut graph = Graph::<Mutable>::new();
		let node = graph.add_node(None, PropertyMap::new()).assert_ok();
		let mut workspace = graph.into_workspace();
		workspace.insert_component(node, Position(3)).assert_ok();
		workspace.insert_derived(node, PageRank(0.8)).assert_ok();

		let workspace = workspace.map_graph(Graph::freeze);
		assert_eq!(workspace.component::<Position>(node).assert_ok(), Some(&Position(3)));
		assert_eq!(workspace.derived_components().assert_ok().component_pool_count(), 0);
	}
}
