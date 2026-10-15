use std::borrow::Cow;

use super::{
	AtomId,
	Dag,
	EdgeDirection,
	EdgeRead,
	Forest,
	GraphKind,
	GraphRead,
	GraphTopologyConstraints,
	RootedTree,
	Tree,
	TreeOrientation,
};
use crate::{
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	values::runtime::{PropertyMap, Uuid},
};

/// Expands the define family view declarations used by this module.
macro_rules! define_family_view {
	($name:ident, $kind:expr) => {
		/// Borrowed graph-family view forwarding reads to its underlying graph reader.
		pub struct $name<'graph, R: GraphRead + ?Sized> {
			/// Borrowed graph reader retaining the topology and snapshot represented by
			/// this view.
			source: &'graph R,
		}

		impl<'graph, R: GraphRead + ?Sized> $name<'graph, R> {
			/// Constructs the borrowed view or join from its supplied source.
			pub(crate) const fn new(source: &'graph R) -> Self {
				Self {
					source,
				}
			}

			/// Borrows the reader underlying this graph-family view.
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

			/// Returns the graph identity shared by snapshots of this graph.
			fn identity(&self) -> Uuid {
				self.source.identity()
			}

			/// Returns the revision represented by this graph snapshot.
			fn revision(&self) -> u64 {
				self.source.revision()
			}

			/// Returns the structural graph family represented by this implementation.
			fn kind(&self) -> GraphKind {
				$kind
			}

			/// Borrows the graph properties through the shared read surface.
			fn properties(&self) -> FerrosGrynnResult<Cow<'_, PropertyMap>> {
				self.source.properties()
			}

			/// Returns the structural constraints enforced by this graph family.
			fn topology_constraints(&self) -> GraphTopologyConstraints {
				self.source.topology_constraints()
			}

			/// Iterates over live node identifiers in the current graph.
			fn node_ids(&self) -> FerrosGrynnResult<Self::NodeIds<'_>> {
				self.source.node_ids()
			}

			/// Iterates over live edge identifiers in the current graph.
			fn edge_ids(&self) -> FerrosGrynnResult<Self::EdgeIds<'_>> {
				self.source.edge_ids()
			}

			/// Looks up a node view after validating its local identifier.
			fn node(&self, id: AtomId) -> FerrosGrynnResult<Option<Self::Node<'_>>> {
				self.source.node(id)
			}

			/// Looks up an edge view after validating its local identifier.
			fn edge(&self, id: AtomId) -> FerrosGrynnResult<Option<Self::Edge<'_>>> {
				self.source.edge(id)
			}

			/// Returns the storage mode of the underlying graph reader.
			fn storage_mode(&self) -> super::GraphStorageMode {
				self.source.storage_mode()
			}

			/// Returns the snapshot identity used by the underlying reader.
			fn snapshot_id(&self) -> super::SnapshotId {
				self.source.snapshot_id()
			}

			/// Borrows the durable graph reference when this reader has one.
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
	/// Returns the tree view payload when this value has the matching variant.
	pub const fn as_tree_view(&self) -> TreeView<'_, Self> {
		TreeView::new(self)
	}

	/// Returns the forest view payload when this value has the matching
	/// variant.
	pub const fn as_forest_view(&self) -> ForestView<'_, Self> {
		ForestView::new(self)
	}

	/// Returns the graph view payload when this value has the matching variant.
	pub const fn as_graph_view(&self) -> GraphView<'_, Self> {
		GraphView::new(self)
	}

	/// Returns the dag view payload when this value has the matching variant.
	///
	/// # Errors
	///
	/// Propagates validation or execution failures from the underlying
	/// operation.
	pub fn as_dag_view(&self) -> FerrosGrynnResult<DagView<'_, Self>> {
		directed_forest_view(self.orientation(), self)
	}
}

impl<State> Tree<State> {
	/// Returns the forest view payload when this value has the matching
	/// variant.
	pub const fn as_forest_view(&self) -> ForestView<'_, Self> {
		ForestView::new(self)
	}

	/// Returns the graph view payload when this value has the matching variant.
	pub const fn as_graph_view(&self) -> GraphView<'_, Self> {
		GraphView::new(self)
	}

	/// Returns the dag view payload when this value has the matching variant.
	///
	/// # Errors
	///
	/// Propagates validation or execution failures from the underlying
	/// operation.
	pub fn as_dag_view(&self) -> FerrosGrynnResult<DagView<'_, Self>> {
		directed_forest_view(self.orientation(), self)
	}
}

impl<State> Forest<State> {
	/// Returns the graph view payload when this value has the matching variant.
	pub const fn as_graph_view(&self) -> GraphView<'_, Self> {
		GraphView::new(self)
	}

	/// Returns the dag view payload when this value has the matching variant.
	///
	/// # Errors
	///
	/// Propagates validation or execution failures from the underlying
	/// operation.
	pub fn as_dag_view(&self) -> FerrosGrynnResult<DagView<'_, Self>> {
		directed_forest_view(self.orientation(), self)
	}
}

impl<State> Dag<State> {
	/// Returns the graph view payload when this value has the matching variant.
	pub const fn as_graph_view(&self) -> GraphView<'_, Self> {
		GraphView::new(self)
	}
}

impl<State> super::Hypergraph<State> {
	/// Returns the graph view payload when this value has the matching variant.
	pub const fn as_graph_view(&self) -> GraphView<'_, Self> {
		GraphView::new(self)
	}
}

impl<State> super::Metagraph<State> {
	/// Returns the graph view payload when this value has the matching variant.
	pub const fn as_graph_view(&self) -> GraphView<'_, Self> {
		GraphView::new(self)
	}
}

impl<'graph, R: GraphRead + ?Sized> DagView<'graph, R> {
	/// Attempts new for `DagView<'graph, R>`.
	///
	/// # Errors
	///
	/// Returns `atom_does_not_exist`, `graph_constraint_violation` diagnostics
	/// and propagates failures from called operations.
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

/// Returns the directed forest view result.
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
/// Regression tests for view behavior.
mod tests {
	use super::GraphRead;
	#[cfg(test)]
	use crate::test_assertions::AssertResult as _;
	use crate::values::{
		PropertyMap,
		runtime::graph::{Mutable, RootedTree, TreeOrientation},
	};

	/// Verifies widening views do not rebuild or change identity.
	#[test]
	fn widening_views_do_not_rebuild_or_change_identity() {
		let tree =
			RootedTree::<Mutable>::new(TreeOrientation::OutArborescence, None, PropertyMap::new())
				.assert_ok();
		let graph = tree.as_graph_view();
		let dag = tree.as_dag_view().assert_ok();

		assert_eq!(graph.identity(), tree.identity());
		assert_eq!(graph.kind(), super::GraphKind::Graph);
		assert_eq!(dag.kind(), super::GraphKind::Dag);
	}
}
