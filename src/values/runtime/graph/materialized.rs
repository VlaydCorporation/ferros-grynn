use std::{
	cmp::Ordering,
	hash::{Hash, Hasher},
};

use super::{
	Dag,
	Forest,
	Frozen,
	Graph,
	GraphKind,
	Hypergraph,
	Metagraph,
	RootedTree,
	Subgraph,
	Tree,
};
use crate::{
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	values::runtime::Uuid,
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

impl MaterializedGraph {
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
