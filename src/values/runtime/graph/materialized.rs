use std::{
	borrow::Cow,
	cmp::Ordering,
	hash::{Hash, Hasher},
};

use super::{
	AtomId,
	AtomKind,
	Dag,
	EdgeView,
	Forest,
	Frozen,
	Graph,
	GraphKind,
	GraphRead,
	Hypergraph,
	Metagraph,
	NodeView,
	RootedTree,
	Subgraph,
	Tree,
};
use crate::{
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	values::runtime::{PropertyMap, Uuid},
};

/// Transport sum type for immutable graph query results.
///
/// Operators with a fixed contract still return their concrete Rust type.
/// This enum is used only when a graph must travel through generic [`Value`]
/// transport, and its discriminant is always available through [`Self::kind`].
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum MaterializedGraph {
	Graph(Graph<Frozen>),
	Tree(Tree<Frozen>),
	RootedTree(RootedTree<Frozen>),
	Forest(Forest<Frozen>),
	Dag(Dag<Frozen>),
	Hypergraph(Hypergraph<Frozen>),
	Metagraph(Metagraph<Frozen>),
	Subgraph(Subgraph<Frozen>),
}

impl GraphRead for MaterializedGraph {
	type Edge<'a>
		= EdgeView<'a>
	where
		Self: 'a;
	type EdgeIds<'a>
		= Box<dyn Iterator<Item = FerrosGrynnResult<AtomId>> + 'a>
	where
		Self: 'a;
	type Node<'a>
		= NodeView<'a>
	where
		Self: 'a;
	type NodeIds<'a>
		= Box<dyn Iterator<Item = FerrosGrynnResult<AtomId>> + 'a>
	where
		Self: 'a;

	fn identity(&self) -> Uuid {
		self.identity()
	}

	fn revision(&self) -> u64 {
		self.revision()
	}

	fn kind(&self) -> GraphKind {
		self.kind()
	}

	fn properties(&self) -> FerrosGrynnResult<Cow<'_, PropertyMap>> {
		Ok(Cow::Borrowed(Self::properties(self)))
	}

	fn node_ids(&self) -> FerrosGrynnResult<Self::NodeIds<'_>> {
		match self {
			Self::Graph(value) => Ok(Box::new(GraphRead::node_ids(value)?)),
			Self::Tree(value) => Ok(Box::new(GraphRead::node_ids(value)?)),
			Self::RootedTree(value) => Ok(Box::new(GraphRead::node_ids(value)?)),
			Self::Forest(value) => Ok(Box::new(GraphRead::node_ids(value)?)),
			Self::Dag(value) => Ok(Box::new(GraphRead::node_ids(value)?)),
			Self::Hypergraph(value) => Ok(Box::new(GraphRead::node_ids(value)?)),
			Self::Metagraph(value) => Ok(Box::new(GraphRead::node_ids(value)?)),
			Self::Subgraph(value) => Ok(Box::new(GraphRead::node_ids(value)?)),
		}
	}

	fn edge_ids(&self) -> FerrosGrynnResult<Self::EdgeIds<'_>> {
		match self {
			Self::Graph(value) => Ok(Box::new(GraphRead::edge_ids(value)?)),
			Self::Tree(value) => Ok(Box::new(GraphRead::edge_ids(value)?)),
			Self::RootedTree(value) => Ok(Box::new(GraphRead::edge_ids(value)?)),
			Self::Forest(value) => Ok(Box::new(GraphRead::edge_ids(value)?)),
			Self::Dag(value) => Ok(Box::new(GraphRead::edge_ids(value)?)),
			Self::Hypergraph(value) => Ok(Box::new(GraphRead::edge_ids(value)?)),
			Self::Metagraph(value) => Ok(Box::new(GraphRead::edge_ids(value)?)),
			Self::Subgraph(value) => Ok(Box::new(GraphRead::edge_ids(value)?)),
		}
	}

	fn node(&self, id: AtomId) -> FerrosGrynnResult<Option<Self::Node<'_>>> {
		Ok(Self::node(self, id))
	}

	fn edge(&self, id: AtomId) -> FerrosGrynnResult<Option<Self::Edge<'_>>> {
		Ok(Self::edge(self, id))
	}
}

impl MaterializedGraph {
	pub(crate) fn restore_free_slot(
		&mut self,
		kind: AtomKind,
		slot: u64,
		generation: u32,
	) -> FerrosGrynnResult<()> {
		match self {
			Self::Graph(value) => value.restore_free_slot(kind, slot, generation),
			Self::Tree(value) => value.restore_free_slot(kind, slot, generation),
			Self::RootedTree(value) => value.restore_free_slot(kind, slot, generation),
			Self::Forest(value) => value.restore_free_slot(kind, slot, generation),
			Self::Dag(value) => value.restore_free_slot(kind, slot, generation),
			Self::Hypergraph(value) => value.restore_free_slot(kind, slot, generation),
			Self::Metagraph(value) => value.restore_free_slot(kind, slot, generation),
			Self::Subgraph(value) => value.restore_free_slot(kind, slot, generation),
		}
	}

	pub(crate) fn slot_capacity(&self, kind: AtomKind) -> usize {
		match self {
			Self::Graph(value) => value.slot_capacity(kind),
			Self::Tree(value) => value.slot_capacity(kind),
			Self::RootedTree(value) => value.slot_capacity(kind),
			Self::Forest(value) => value.slot_capacity(kind),
			Self::Dag(value) => value.slot_capacity(kind),
			Self::Hypergraph(value) => value.slot_capacity(kind),
			Self::Metagraph(value) => value.slot_capacity(kind),
			Self::Subgraph(value) => value.slot_capacity(kind),
		}
	}

	pub(crate) fn slot_generation(&self, kind: AtomKind, slot: usize) -> Option<u32> {
		match self {
			Self::Graph(value) => value.slot_generation(kind, slot),
			Self::Tree(value) => value.slot_generation(kind, slot),
			Self::RootedTree(value) => value.slot_generation(kind, slot),
			Self::Forest(value) => value.slot_generation(kind, slot),
			Self::Dag(value) => value.slot_generation(kind, slot),
			Self::Hypergraph(value) => value.slot_generation(kind, slot),
			Self::Metagraph(value) => value.slot_generation(kind, slot),
			Self::Subgraph(value) => value.slot_generation(kind, slot),
		}
	}

	pub fn node_count(&self) -> usize {
		match self {
			Self::Graph(value) => value.node_count(),
			Self::Tree(value) => value.node_count(),
			Self::RootedTree(value) => value.node_count(),
			Self::Forest(value) => value.node_count(),
			Self::Dag(value) => value.node_count(),
			Self::Hypergraph(value) => value.node_count(),
			Self::Metagraph(value) => value.node_count(),
			Self::Subgraph(value) => value.as_graph().node_count(),
		}
	}

	pub fn edge_count(&self) -> usize {
		match self {
			Self::Graph(value) => value.edge_count(),
			Self::Tree(value) => value.edge_count(),
			Self::RootedTree(value) => value.edge_count(),
			Self::Forest(value) => value.edge_count(),
			Self::Dag(value) => value.edge_count(),
			Self::Hypergraph(value) => value.edge_count(),
			Self::Metagraph(value) => value.edge_count(),
			Self::Subgraph(value) => value.as_graph().edge_count(),
		}
	}

	pub fn contains(&self, id: AtomId) -> bool {
		match id.kind() {
			Some(super::AtomKind::Node) => self.node(id).is_some(),
			Some(super::AtomKind::Edge) => self.edge(id).is_some(),
			None => false,
		}
	}

	pub fn properties(&self) -> &PropertyMap {
		match self {
			Self::Graph(value) => value.properties(),
			Self::Tree(value) => value.properties(),
			Self::RootedTree(value) => value.properties(),
			Self::Forest(value) => value.properties(),
			Self::Dag(value) => value.properties(),
			Self::Hypergraph(value) => value.properties(),
			Self::Metagraph(value) => value.as_graph().properties(),
			Self::Subgraph(value) => value.as_graph().properties(),
		}
	}

	pub fn node(&self, id: AtomId) -> Option<NodeView<'_>> {
		match self {
			Self::Graph(value) => value.node(id),
			Self::Tree(value) => value.node(id),
			Self::RootedTree(value) => value.node(id),
			Self::Forest(value) => value.node(id),
			Self::Dag(value) => value.node(id),
			Self::Hypergraph(value) => value.node(id),
			Self::Metagraph(value) => value.node(id),
			Self::Subgraph(value) => value.as_graph().node(id),
		}
	}

	pub fn edge(&self, id: AtomId) -> Option<EdgeView<'_>> {
		match self {
			Self::Graph(value) => value.edge(id),
			Self::Tree(value) => value.edge(id),
			Self::RootedTree(value) => value.edge(id),
			Self::Forest(value) => value.edge(id),
			Self::Dag(value) => value.edge(id),
			Self::Hypergraph(value) => value.edge(id),
			Self::Metagraph(value) => value.edge(id),
			Self::Subgraph(value) => value.as_graph().edge(id),
		}
	}

	pub const fn kind(&self) -> GraphKind {
		match self {
			Self::Graph(_) => GraphKind::Graph,
			Self::Tree(_) => GraphKind::Tree,
			Self::RootedTree(_) => GraphKind::RootedTree,
			Self::Forest(_) => GraphKind::Forest,
			Self::Dag(_) => GraphKind::Dag,
			Self::Hypergraph(_) => GraphKind::Hypergraph,
			Self::Metagraph(_) => GraphKind::Metagraph,
			Self::Subgraph(_) => GraphKind::Subgraph,
		}
	}

	pub const fn identity(&self) -> Uuid {
		match self {
			Self::Graph(value) => value.identity(),
			Self::Tree(value) => value.identity(),
			Self::RootedTree(value) => value.identity(),
			Self::Forest(value) => value.identity(),
			Self::Dag(value) => value.identity(),
			Self::Hypergraph(value) => value.identity(),
			Self::Metagraph(value) => value.identity(),
			Self::Subgraph(value) => value.identity(),
		}
	}

	pub const fn revision(&self) -> u64 {
		match self {
			Self::Graph(value) => value.revision(),
			Self::Tree(value) => value.revision(),
			Self::RootedTree(value) => value.revision(),
			Self::Forest(value) => value.revision(),
			Self::Dag(value) => value.revision(),
			Self::Hypergraph(value) => value.revision(),
			Self::Metagraph(value) => value.revision(),
			Self::Subgraph(value) => value.revision(),
		}
	}

	pub fn into_general(self) -> FerrosGrynnResult<Graph<Frozen>> {
		match self {
			Self::Graph(value) => Ok(value),
			Self::Tree(value) => value.into_graph(),
			Self::RootedTree(value) => value.into_graph(),
			Self::Forest(value) => value.into_graph(),
			Self::Dag(value) => value.into_graph(),
			Self::Hypergraph(value) => value.into_graph(),
			Self::Metagraph(value) => Ok(value.into_graph()),
			Self::Subgraph(value) => Ok(value.into_graph()),
		}
	}

	pub const fn as_general(&self) -> Option<&Graph<Frozen>> {
		match self {
			Self::Graph(value) => Some(value),
			_ => None,
		}
	}
}

macro_rules! impl_exact_access {
	($type:ty, $variant:ident, $as_method:ident, $into_method:ident) => {
		impl MaterializedGraph {
			pub const fn $as_method(&self) -> Option<&$type> {
				match self {
					Self::$variant(value) => Some(value),
					_ => None,
				}
			}

			pub fn $into_method(self) -> FerrosGrynnResult<$type> {
				<$type>::try_from(self)
			}
		}

		impl TryFrom<MaterializedGraph> for $type {
			type Error = FerrosGrynnError;

			fn try_from(value: MaterializedGraph) -> Result<Self, Self::Error> {
				match value {
					MaterializedGraph::$variant(value) => Ok(value),
					value => Err(FerrosGrynnError::conversion_error(
						value.kind().name(),
						stringify!($type),
					)),
				}
			}
		}
	};
}

impl_exact_access!(Graph<Frozen>, Graph, as_graph, into_graph);
impl_exact_access!(Tree<Frozen>, Tree, as_tree, into_tree);
impl_exact_access!(RootedTree<Frozen>, RootedTree, as_rooted_tree, into_rooted_tree);
impl_exact_access!(Forest<Frozen>, Forest, as_forest, into_forest);
impl_exact_access!(Dag<Frozen>, Dag, as_dag, into_dag);
impl_exact_access!(Hypergraph<Frozen>, Hypergraph, as_hypergraph, into_hypergraph);
impl_exact_access!(Metagraph<Frozen>, Metagraph, as_metagraph, into_metagraph);
impl_exact_access!(Subgraph<Frozen>, Subgraph, as_subgraph, into_subgraph);

impl PartialEq for MaterializedGraph {
	fn eq(&self, other: &Self) -> bool {
		(self.identity(), self.revision()) == (other.identity(), other.revision())
	}
}

impl Eq for MaterializedGraph {}

impl PartialOrd for MaterializedGraph {
	fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
		Some(self.cmp(other))
	}
}

impl Ord for MaterializedGraph {
	fn cmp(&self, other: &Self) -> Ordering {
		(self.identity(), self.revision()).cmp(&(other.identity(), other.revision()))
	}
}

impl Hash for MaterializedGraph {
	fn hash<H: Hasher>(&self, state: &mut H) {
		self.identity().hash(state);
		self.revision().hash(state);
	}
}

macro_rules! impl_from_graph_family {
	($type:ty, $variant:ident) => {
		impl From<$type> for MaterializedGraph {
			fn from(value: $type) -> Self {
				Self::$variant(value)
			}
		}
	};
}

impl_from_graph_family!(Graph<Frozen>, Graph);
impl_from_graph_family!(Tree<Frozen>, Tree);
impl_from_graph_family!(RootedTree<Frozen>, RootedTree);
impl_from_graph_family!(Forest<Frozen>, Forest);
impl_from_graph_family!(Dag<Frozen>, Dag);
impl_from_graph_family!(Hypergraph<Frozen>, Hypergraph);
impl_from_graph_family!(Metagraph<Frozen>, Metagraph);
impl_from_graph_family!(Subgraph<Frozen>, Subgraph);

#[cfg(test)]
mod tests {
	use super::MaterializedGraph;
	use crate::values::runtime::graph::{Graph, Mutable};

	#[test]
	fn preserves_exact_discriminant_in_generic_transport() {
		let graph = Graph::<Mutable>::new().freeze();
		let identity = graph.identity();
		let value = MaterializedGraph::from(graph);

		assert_eq!(value.kind(), super::GraphKind::Graph);
		assert_eq!(value.identity(), identity);
		assert_eq!(value.into_general().unwrap().node_count(), 0);
	}
}
