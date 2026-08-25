use std::borrow::Cow;

use super::{
	AtomId,
	Dag,
	EdgeDirection,
	EdgeRead,
	Forest,
	GraphKind,
	GraphRead,
	RootedTree,
	Tree,
	TreeOrientation,
};
use crate::{
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	values::runtime::{PropertyMap, Uuid},
};

macro_rules! define_family_view {
	($name:ident, $kind:expr) => {
		pub struct $name<'graph, R: GraphRead + ?Sized> {
			source: &'graph R,
		}

		impl<'graph, R: GraphRead + ?Sized> $name<'graph, R> {
			pub(crate) const fn new(source: &'graph R) -> Self {
				Self {
					source,
				}
			}

			pub const fn source(&self) -> &'graph R {
				self.source
			}
		}

		impl<R: GraphRead + ?Sized> GraphRead for $name<'_, R> {
			type Edge<'a>
				= R::Edge<'a>
			where
				Self: 'a;
			type EdgeIds<'a>
				= R::EdgeIds<'a>
			where
				Self: 'a;
			type Node<'a>
				= R::Node<'a>
			where
				Self: 'a;
			type NodeIds<'a>
				= R::NodeIds<'a>
			where
				Self: 'a;

			fn identity(&self) -> Uuid {
				self.source.identity()
			}

			fn revision(&self) -> u64 {
				self.source.revision()
			}

			fn kind(&self) -> GraphKind {
				$kind
			}

			fn properties(&self) -> FerrosGrynnResult<Cow<'_, PropertyMap>> {
				self.source.properties()
			}

			fn node_ids(&self) -> FerrosGrynnResult<Self::NodeIds<'_>> {
				self.source.node_ids()
			}

			fn edge_ids(&self) -> FerrosGrynnResult<Self::EdgeIds<'_>> {
				self.source.edge_ids()
			}

			fn node(&self, id: AtomId) -> FerrosGrynnResult<Option<Self::Node<'_>>> {
				self.source.node(id)
			}

			fn edge(&self, id: AtomId) -> FerrosGrynnResult<Option<Self::Edge<'_>>> {
				self.source.edge(id)
			}

			fn storage_mode(&self) -> super::GraphStorageMode {
				self.source.storage_mode()
			}

			fn snapshot_id(&self) -> super::SnapshotId {
				self.source.snapshot_id()
			}

			fn graph_ref(&self) -> Option<&super::GraphRef> {
				self.source.graph_ref()
			}
		}
	};
}

define_family_view!(GraphView, GraphKind::Graph);
define_family_view!(ForestView, GraphKind::Forest);
define_family_view!(TreeView, GraphKind::Tree);
define_family_view!(DagView, GraphKind::Dag);

impl<State> RootedTree<State> {
	pub const fn as_tree_view(&self) -> TreeView<'_, Self> {
		TreeView::new(self)
	}

	pub const fn as_forest_view(&self) -> ForestView<'_, Self> {
		ForestView::new(self)
	}

	pub const fn as_graph_view(&self) -> GraphView<'_, Self> {
		GraphView::new(self)
	}

	pub fn as_dag_view(&self) -> FerrosGrynnResult<DagView<'_, Self>> {
		directed_forest_view(self.orientation(), self)
	}
}

impl<State> Tree<State> {
	pub const fn as_forest_view(&self) -> ForestView<'_, Self> {
		ForestView::new(self)
	}

	pub const fn as_graph_view(&self) -> GraphView<'_, Self> {
		GraphView::new(self)
	}

	pub fn as_dag_view(&self) -> FerrosGrynnResult<DagView<'_, Self>> {
		directed_forest_view(self.orientation(), self)
	}
}

impl<State> Forest<State> {
	pub const fn as_graph_view(&self) -> GraphView<'_, Self> {
		GraphView::new(self)
	}

	pub fn as_dag_view(&self) -> FerrosGrynnResult<DagView<'_, Self>> {
		directed_forest_view(self.orientation(), self)
	}
}

impl<State> Dag<State> {
	pub const fn as_graph_view(&self) -> GraphView<'_, Self> {
		GraphView::new(self)
	}
}

impl<State> super::Hypergraph<State> {
	pub const fn as_graph_view(&self) -> GraphView<'_, Self> {
		GraphView::new(self)
	}
}

impl<State> super::Metagraph<State> {
	pub const fn as_graph_view(&self) -> GraphView<'_, Self> {
		GraphView::new(self)
	}
}

impl<'graph, R: GraphRead + ?Sized> DagView<'graph, R> {
	pub fn try_new(source: &'graph R) -> FerrosGrynnResult<Self> {
		for id in source.edge_ids()? {
			let id = id?;
			let edge = source
				.edge(id)?
				.ok_or_else(|| FerrosGrynnError::atom_does_not_exist("edge", id.to_string()))?;
			if !matches!(edge.topology()?.local_binary(), Some((_, _, EdgeDirection::Directed))) {
				return Err(FerrosGrynnError::graph_constraint_violation(
					"dag view",
					"all edges must be local binary directed edges",
				));
			}
		}
		Ok(Self::new(source))
	}
}

fn directed_forest_view<R: GraphRead + ?Sized>(
	orientation: TreeOrientation,
	source: &R,
) -> FerrosGrynnResult<DagView<'_, R>> {
	if orientation == TreeOrientation::Undirected {
		return Err(FerrosGrynnError::graph_constraint_violation(
			"dag view",
			"an undirected forest has no implicit DAG orientation",
		));
	}
	Ok(DagView::new(source))
}

#[cfg(test)]
mod tests {
	use super::GraphRead;
	use crate::values::{
		PropertyMap,
		runtime::graph::{Mutable, RootedTree, TreeOrientation},
	};

	#[test]
	fn widening_views_do_not_rebuild_or_change_identity() {
		let tree =
			RootedTree::<Mutable>::new(TreeOrientation::OutArborescence, None, PropertyMap::new())
				.unwrap();
		let graph = tree.as_graph_view();
		let dag = tree.as_dag_view().unwrap();

		assert_eq!(graph.identity(), tree.identity());
		assert_eq!(graph.kind(), super::GraphKind::Graph);
		assert_eq!(dag.kind(), super::GraphKind::Dag);
	}
}
