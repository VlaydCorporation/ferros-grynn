use std::{
	cmp::Ordering,
	fmt::{self, Debug, Formatter},
	hash::{Hash, Hasher},
};

use super::{
	AtomId,
	EdgeView,
	Frozen,
	Graph,
	GraphKind,
	GraphMut,
	GraphRead,
	GraphWriteBack,
	Mutable,
	NodeView,
	QueryMaterialize,
};
use crate::{
	gql_status::FerrosGrynnResult,
	values::runtime::{PropertyMap, Uuid},
};

/// Self-contained materialized query result.
///
/// A subgraph is a result boundary, not an additional storage family, so it
/// deliberately reuses the general graph layout.
pub struct Subgraph<State = Frozen> {
	graph: Graph<State>,
}

pub type MutableSubgraph = Subgraph<Mutable>;
pub type FrozenSubgraph = Subgraph<Frozen>;

impl Subgraph<Mutable> {
	pub fn new() -> Self {
		Self {
			graph: Graph::new(),
		}
	}

	pub fn freeze(self) -> Subgraph<Frozen> {
		Subgraph {
			graph: self.graph.freeze(),
		}
	}

	pub fn flush<GWB: GraphWriteBack<Self>>(&mut self, writer: &mut GWB) -> FerrosGrynnResult<()> {
		let (origin, dirty) = self.graph.writeback_context()?;
		writer.flush(self, &origin, &dirty)?;
		self.graph.finish_writeback();
		Ok(())
	}
}

impl Default for Subgraph<Mutable> {
	fn default() -> Self {
		Self::new()
	}
}

impl Subgraph<Frozen> {
	pub fn thaw(self) -> Subgraph<Mutable> {
		Subgraph {
			graph: self.graph.thaw(),
		}
	}
}

impl Clone for Subgraph<Frozen> {
	fn clone(&self) -> Self {
		Self {
			graph: self.graph.clone(),
		}
	}
}

impl<State> Subgraph<State> {
	pub const fn from_graph(graph: Graph<State>) -> Self {
		Self {
			graph,
		}
	}

	pub fn into_graph(self) -> Graph<State> {
		self.graph
	}

	pub const fn as_graph(&self) -> &Graph<State> {
		&self.graph
	}

	pub const fn identity(&self) -> Uuid {
		self.graph.identity()
	}

	pub const fn revision(&self) -> u64 {
		self.graph.revision()
	}
}

impl<State> GraphRead for Subgraph<State> {
	type EdgeIds<'a>
		= <Graph<State> as GraphRead>::EdgeIds<'a>
	where
		Self: 'a;
	type NodeIds<'a>
		= <Graph<State> as GraphRead>::NodeIds<'a>
	where
		Self: 'a;

	fn identity(&self) -> Uuid {
		self.graph.identity()
	}

	fn revision(&self) -> u64 {
		self.graph.revision()
	}

	fn kind(&self) -> GraphKind {
		GraphKind::Subgraph
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

impl GraphMut for Subgraph<Mutable> {
	fn properties_mut(&mut self) -> FerrosGrynnResult<&mut PropertyMap> {
		self.graph.properties_mut()
	}

	fn node_properties_mut(&mut self, id: AtomId) -> FerrosGrynnResult<&mut PropertyMap> {
		self.graph.node_properties_mut(id)
	}

	fn edge_properties_mut(&mut self, id: AtomId) -> FerrosGrynnResult<&mut PropertyMap> {
		self.graph.edge_properties_mut(id)
	}

	fn remove_atom(&mut self, id: AtomId) -> FerrosGrynnResult<()> {
		self.graph.remove_atom(id)
	}
}

impl QueryMaterialize for Subgraph<Mutable> {
	fn query_from<R: GraphRead>(source: &R) -> FerrosGrynnResult<Self> {
		Graph::<Mutable>::query_from(source).map(Self::from_graph)
	}
}

impl QueryMaterialize for Subgraph<Frozen> {
	fn query_from<R: GraphRead>(source: &R) -> FerrosGrynnResult<Self> {
		Subgraph::<Mutable>::query_from(source).map(Subgraph::freeze)
	}
}

impl<State> Debug for Subgraph<State> {
	fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
		formatter
			.debug_struct("Subgraph")
			.field("identity", &self.identity())
			.field("revision", &self.revision())
			.field("nodes", &self.graph.node_count())
			.field("edges", &self.graph.edge_count())
			.finish()
	}
}

impl<State> PartialEq for Subgraph<State> {
	fn eq(&self, other: &Self) -> bool {
		(self.identity(), self.revision()) == (other.identity(), other.revision())
	}
}

impl<State> Eq for Subgraph<State> {}

impl<State> PartialOrd for Subgraph<State> {
	fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
		Some(self.cmp(other))
	}
}

impl<State> Ord for Subgraph<State> {
	fn cmp(&self, other: &Self) -> Ordering {
		(self.identity(), self.revision()).cmp(&(other.identity(), other.revision()))
	}
}

impl<State> Hash for Subgraph<State> {
	fn hash<H: Hasher>(&self, state: &mut H) {
		self.identity().hash(state);
		self.revision().hash(state);
	}
}

#[cfg(test)]
mod tests {
	use super::Subgraph;
	use crate::values::{
		PropertyMap,
		runtime::graph::{GraphKind, GraphRead, Mutable},
	};

	#[test]
	fn marks_materialized_result_without_new_storage_family() {
		let mut graph = Subgraph::<Mutable>::new();
		graph.graph.add_node(None, PropertyMap::new()).unwrap();
		assert_eq!(graph.kind(), GraphKind::Subgraph);
		assert_eq!(graph.node_count(), 1);
	}
}
