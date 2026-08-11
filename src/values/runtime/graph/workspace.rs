use std::fmt::{self, Debug, Formatter};

use super::{AtomId, EdgeView, GraphKind, GraphMut, GraphRead, NodeView};
use crate::{
	ecs::WorldComponents,
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	values::runtime::{PropertyMap, Uuid},
};

/// ECS sidecar bound to one graph identity and revision.
pub struct GraphWorkspace<G> {
	graph: G,
	components: WorldComponents,
	derived: WorldComponents,
	bound_identity: Uuid,
	bound_revision: u64,
}

impl<G: GraphRead> GraphWorkspace<G> {
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

	pub const fn graph(&self) -> &G {
		&self.graph
	}

	pub const fn bound_identity(&self) -> Uuid {
		self.bound_identity
	}

	pub const fn bound_revision(&self) -> u64 {
		self.bound_revision
	}

	pub fn components(&self) -> FerrosGrynnResult<&WorldComponents> {
		self.validate_binding()?;
		Ok(&self.components)
	}

	pub fn derived_components(&self) -> FerrosGrynnResult<&WorldComponents> {
		self.validate_binding()?;
		Ok(&self.derived)
	}

	pub fn insert_component<T>(&mut self, id: AtomId, component: T) -> FerrosGrynnResult<Option<T>>
	where
		T: 'static + Send + Sync,
	{
		self.validate_atom(id)?;
		self.components.insert(id, component)
	}

	pub fn component<T>(&self, id: AtomId) -> FerrosGrynnResult<Option<&T>>
	where
		T: 'static + Send + Sync,
	{
		self.validate_atom(id)?;
		Ok(self.components.get(id))
	}

	pub fn component_mut<T>(&mut self, id: AtomId) -> FerrosGrynnResult<Option<&mut T>>
	where
		T: 'static + Send + Sync,
	{
		self.validate_atom(id)?;
		Ok(self.components.get_mut(id))
	}

	pub fn insert_derived<T>(&mut self, id: AtomId, component: T) -> FerrosGrynnResult<Option<T>>
	where
		T: 'static + Send + Sync,
	{
		self.validate_atom(id)?;
		self.derived.insert(id, component)
	}

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
	pub fn mutate<T, MutFn>(&mut self, mutation: MutFn) -> FerrosGrynnResult<T>
	where
		G: GraphMut,
		MutFn: FnOnce(&mut G) -> FerrosGrynnResult<T>,
	{
		self.validate_binding()?;
		let result = mutation(&mut self.graph);
		self.synchronize_after_mutation();
		result
	}

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

	pub fn into_parts(self) -> (G, WorldComponents) {
		(self.graph, self.components)
	}

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

	fn validate_atom(&self, id: AtomId) -> FerrosGrynnResult<()> {
		self.validate_binding()?;
		if !self.graph.contains(id) {
			return Err(FerrosGrynnError::atom_does_not_exist(
				id.kind().map_or_else(|| "null".to_owned(), |kind| kind.to_string()),
				id.to_string(),
			));
		}
		Ok(())
	}

	fn synchronize_after_mutation(&mut self) {
		let identity = self.graph.identity();
		let revision = self.graph.revision();
		if identity != self.bound_identity {
			self.components.clear();
		} else if revision != self.bound_revision {
			let graph = &self.graph;
			self.components.retain_atoms(|id| graph.contains(id));
		}
		if identity != self.bound_identity || revision != self.bound_revision {
			self.derived.clear();
			self.bound_identity = identity;
			self.bound_revision = revision;
		}
	}
}

pub trait IntoGraphWorkspace: GraphRead + Sized {
	fn into_workspace(self) -> GraphWorkspace<Self> {
		GraphWorkspace::new(self)
	}
}

impl<G: GraphRead> IntoGraphWorkspace for G {}

impl<G: GraphRead> GraphRead for GraphWorkspace<G> {
	type NodeIds<'a>
		= G::NodeIds<'a>
	where
		Self: 'a;
	type EdgeIds<'a>
		= G::EdgeIds<'a>
	where
		Self: 'a;

	fn identity(&self) -> Uuid {
		self.graph.identity()
	}

	fn revision(&self) -> u64 {
		self.graph.revision()
	}

	fn kind(&self) -> GraphKind {
		self.graph.kind()
	}

	fn properties(&self) -> &PropertyMap {
		self.graph.properties()
	}

	fn node_ids(&self) -> Self::NodeIds<'_> {
		self.graph.node_ids()
	}

	fn edge_ids(&self) -> Self::EdgeIds<'_> {
		self.graph.edge_ids()
	}

	fn node(&self, id: AtomId) -> Option<NodeView<'_>> {
		self.graph.node(id)
	}

	fn edge(&self, id: AtomId) -> Option<EdgeView<'_>> {
		self.graph.edge(id)
	}

	fn storage_mode(&self) -> super::GraphStorageMode {
		self.graph.storage_mode()
	}

	fn snapshot_id(&self) -> super::SnapshotId {
		self.graph.snapshot_id()
	}

	fn graph_ref(&self) -> Option<&super::GraphRef> {
		self.graph.graph_ref()
	}
}

impl<G: GraphRead + Debug> Debug for GraphWorkspace<G> {
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
mod tests {
	use super::{GraphWorkspace, IntoGraphWorkspace};
	use crate::values::{
		PropertyMap,
		runtime::graph::{Graph, Mutable},
	};

	#[derive(Debug, PartialEq)]
	struct Position(u32);

	#[derive(Debug, PartialEq)]
	struct PageRank(f64);

	#[test]
	fn component_mutation_does_not_change_graph_revision() {
		let mut graph = Graph::<Mutable>::new();
		let node = graph.add_node(None, PropertyMap::new()).unwrap();
		let revision = graph.revision();
		let mut workspace = graph.into_workspace();

		workspace.insert_component(node, Position(7)).unwrap();
		assert_eq!(workspace.bound_revision(), revision);
		assert_eq!(workspace.component::<Position>(node).unwrap(), Some(&Position(7)));
	}

	#[test]
	fn canonical_mutation_invalidates_derived_and_removes_dead_components() {
		let mut graph = Graph::<Mutable>::new();
		let node = graph.add_node(None, PropertyMap::new()).unwrap();
		let mut workspace = GraphWorkspace::new(graph);
		workspace.insert_component(node, Position(1)).unwrap();
		workspace.insert_derived(node, PageRank(0.5)).unwrap();

		workspace.mutate(|graph| graph.remove_atom(node)).unwrap();
		assert!(workspace.components().unwrap().get::<Position>(node).is_none());
		assert_eq!(workspace.derived_components().unwrap().component_pool_count(), 0);
	}

	#[test]
	fn map_graph_moves_user_components_but_discards_derived_pools() {
		let mut graph = Graph::<Mutable>::new();
		let node = graph.add_node(None, PropertyMap::new()).unwrap();
		let mut workspace = graph.into_workspace();
		workspace.insert_component(node, Position(3)).unwrap();
		workspace.insert_derived(node, PageRank(0.8)).unwrap();

		let workspace = workspace.map_graph(Graph::freeze);
		assert_eq!(workspace.component::<Position>(node).unwrap(), Some(&Position(3)));
		assert_eq!(workspace.derived_components().unwrap().component_pool_count(), 0);
	}
}
