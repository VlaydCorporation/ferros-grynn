use std::{
	borrow::Cow,
	cmp::Ordering,
	fmt::{self, Debug, Formatter},
	hash::{Hash, Hasher},
};

use super::{
	AtomId,
	AtomKind,
	Frozen,
	Graph,
	GraphKind,
	GraphMut,
	GraphRead,
	GraphTopologyConstraints,
	GraphWriteBack,
	Mutable,
	QueryMaterialize,
};
use crate::{
	gql_status::FerrosGrynnResult,
	values::runtime::{PropertyMap, Text, Uuid},
};

/// Self-contained materialized query result.
///
/// A subgraph is a result boundary, not an additional storage family, so it
/// deliberately reuses the general graph layout.
pub struct Subgraph<State = Frozen> {
	/// Graph associated with this subgraph.
	graph: Graph<State>,
}

pub type MutableSubgraph = Subgraph<Mutable>;
pub type FrozenSubgraph = Subgraph<Frozen>;

impl Subgraph<Mutable> {
	/// Constructs `Subgraph<Mutable>` from the supplied initial state.
	pub fn new() -> Self {
		Self {
			graph: Graph::new(),
		}
	}

	/// Returns freeze for `Subgraph<Mutable>`.
	pub fn freeze(self) -> Subgraph<Frozen> {
		Subgraph {
			graph: self.graph.freeze(),
		}
	}

	/// Clones the COW layout while retaining identity, revision and dirty
	/// metadata.
	pub(crate) fn clone_preserving_state(&self) -> Self {
		Self {
			graph: self.graph.clone_preserving_state(),
		}
	}

	/// Adopts an accepted remote watermark without creating a local edit.
	pub(crate) fn adopt_external_revision(&mut self, revision: u64) {
		self.graph.adopt_external_revision(revision);
	}

	/// Clears local dirty metadata after accepting the remote base.
	pub(crate) fn clear_live_dirty(&mut self) {
		self.graph.clear_live_dirty();
	}

	/// Performs the flush operation for `Subgraph<Mutable>`.
	///
	/// # Errors
	///
	/// Propagates errors returned by the underlying operation.
	pub fn flush<GWB: GraphWriteBack<Self>>(&mut self, writer: &mut GWB) -> FerrosGrynnResult<()> {
		let (origin, dirty) = self.graph.writeback_context()?;
		writer.flush(self, &origin, &dirty)?;
		self.graph.finish_writeback();
		Ok(())
	}
}

impl Default for Subgraph<Mutable> {
	/// Constructs the default initial state of `Subgraph<Mutable>`.
	fn default() -> Self {
		Self::new()
	}
}

impl Subgraph<Frozen> {
	/// Returns thaw for `Subgraph<Frozen>`.
	pub fn thaw(self) -> Subgraph<Mutable> {
		Subgraph {
			graph: self.graph.thaw(),
		}
	}
}

impl Clone for Subgraph<Frozen> {
	/// Returns an owned clone of this `Subgraph<Frozen>`.
	fn clone(&self) -> Self {
		Self {
			graph: self.graph.clone(),
		}
	}
}

impl<State> Subgraph<State> {
	/// Returns the slot capacity result.
	pub(crate) fn slot_capacity(&self, kind: AtomKind) -> usize {
		self.graph.slot_capacity(kind)
	}

	/// Constructs from graph for `Subgraph<State>`.
	pub const fn from_graph(graph: Graph<State>) -> Self {
		Self {
			graph,
		}
	}

	/// Returns the stored graph of this `Subgraph<State>`.
	pub fn into_graph(self) -> Graph<State> {
		self.graph
	}

	/// Borrows the stored graph of this `Subgraph<State>`.
	pub const fn as_graph(&self) -> &Graph<State> {
		&self.graph
	}

	/// Returns identity for `Subgraph<State>`.
	pub const fn identity(&self) -> Uuid {
		self.graph.identity()
	}

	/// Returns revision for `Subgraph<State>`.
	pub const fn revision(&self) -> u64 {
		self.graph.revision()
	}

	/// Returns topology constraints for `Subgraph<State>`.
	pub const fn topology_constraints(&self) -> GraphTopologyConstraints {
		self.graph.topology_constraints()
	}
}

impl<State> GraphRead for Subgraph<State> {
	type Edge<'a>
		= <Graph<State> as GraphRead>::Edge<'a>
	where
		Self: 'a;
	type EdgeIds<'a>
		= <Graph<State> as GraphRead>::EdgeIds<'a>
	where
		Self: 'a;
	type Node<'a>
		= <Graph<State> as GraphRead>::Node<'a>
	where
		Self: 'a;
	type NodeIds<'a>
		= <Graph<State> as GraphRead>::NodeIds<'a>
	where
		Self: 'a;

	/// Returns identity for `Subgraph<State>`.
	fn identity(&self) -> Uuid {
		self.graph.identity()
	}

	/// Returns revision for `Subgraph<State>`.
	fn revision(&self) -> u64 {
		self.graph.revision()
	}

	/// Returns kind for `Subgraph<State>`.
	fn kind(&self) -> GraphKind {
		GraphKind::Subgraph
	}

	/// Returns properties for `Subgraph<State>`.
	fn properties(&self) -> FerrosGrynnResult<Cow<'_, PropertyMap>> {
		GraphRead::properties(&self.graph)
	}

	/// Returns topology constraints for `Subgraph<State>`.
	fn topology_constraints(&self) -> GraphTopologyConstraints {
		self.graph.topology_constraints()
	}

	/// Returns node ids for `Subgraph<State>`.
	fn node_ids(&self) -> FerrosGrynnResult<Self::NodeIds<'_>> {
		GraphRead::node_ids(&self.graph)
	}

	/// Returns edge ids for `Subgraph<State>`.
	fn edge_ids(&self) -> FerrosGrynnResult<Self::EdgeIds<'_>> {
		GraphRead::edge_ids(&self.graph)
	}

	/// Returns node for `Subgraph<State>`.
	fn node(&self, id: AtomId) -> FerrosGrynnResult<Option<Self::Node<'_>>> {
		GraphRead::node(&self.graph, id)
	}

	/// Returns edge for `Subgraph<State>`.
	fn edge(&self, id: AtomId) -> FerrosGrynnResult<Option<Self::Edge<'_>>> {
		GraphRead::edge(&self.graph, id)
	}

	/// Returns storage mode for `Subgraph<State>`.
	fn storage_mode(&self) -> super::GraphStorageMode {
		self.graph.storage_mode()
	}

	/// Returns snapshot id for `Subgraph<State>`.
	fn snapshot_id(&self) -> super::SnapshotId {
		self.graph.snapshot_id()
	}

	/// Returns graph ref for `Subgraph<State>`.
	fn graph_ref(&self) -> Option<&super::GraphRef> {
		self.graph.graph_ref()
	}

	/// Returns materialization origin for `Subgraph<State>`.
	fn materialization_origin(&self) -> Option<&super::MaterializationOrigin> {
		self.graph.materialization_origin()
	}
}

impl GraphMut for Subgraph<Mutable> {
	/// Returns properties mut for `Subgraph<Mutable>`.
	fn properties_mut(&mut self) -> FerrosGrynnResult<&mut PropertyMap> {
		self.graph.properties_mut()
	}

	/// Returns node label mut for `Subgraph<Mutable>`.
	fn node_label_mut(&mut self, id: AtomId) -> FerrosGrynnResult<&mut Option<Text>> {
		self.graph.node_label_mut(id)
	}

	/// Returns node properties mut for `Subgraph<Mutable>`.
	fn node_properties_mut(&mut self, id: AtomId) -> FerrosGrynnResult<&mut PropertyMap> {
		self.graph.node_properties_mut(id)
	}

	/// Returns edge label mut for `Subgraph<Mutable>`.
	fn edge_label_mut(&mut self, id: AtomId) -> FerrosGrynnResult<&mut Option<Text>> {
		self.graph.edge_label_mut(id)
	}

	/// Returns edge properties mut for `Subgraph<Mutable>`.
	fn edge_properties_mut(&mut self, id: AtomId) -> FerrosGrynnResult<&mut PropertyMap> {
		self.graph.edge_properties_mut(id)
	}

	/// Removes atom for `Subgraph<Mutable>`.
	fn remove_atom(&mut self, id: AtomId) -> FerrosGrynnResult<()> {
		self.graph.remove_atom(id)
	}
}

impl QueryMaterialize for Subgraph<Mutable> {
	/// Returns query from for `Subgraph<Mutable>`.
	fn query_from<R: GraphRead>(source: &R) -> FerrosGrynnResult<Self> {
		Graph::<Mutable>::query_from(source).map(Self::from_graph)
	}
}

impl QueryMaterialize for Subgraph<Frozen> {
	/// Returns query from for `Subgraph<Frozen>`.
	fn query_from<R: GraphRead>(source: &R) -> FerrosGrynnResult<Self> {
		Subgraph::<Mutable>::query_from(source).map(Subgraph::freeze)
	}
}

impl<State> Debug for Subgraph<State> {
	/// Formats `Subgraph<State>` using the requested formatter.
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
	/// Compares this `Subgraph<State>` with another value for equality.
	fn eq(&self, other: &Self) -> bool {
		(self.identity(), self.revision()) == (other.identity(), other.revision())
	}
}

impl<State> Eq for Subgraph<State> {}

impl<State> PartialOrd for Subgraph<State> {
	/// Compares this `Subgraph<State>` with another value when they are
	/// ordered.
	fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
		Some(self.cmp(other))
	}
}

impl<State> Ord for Subgraph<State> {
	/// Compares this `Subgraph<State>` with another value using its ordering
	/// contract.
	fn cmp(&self, other: &Self) -> Ordering {
		(self.identity(), self.revision()).cmp(&(other.identity(), other.revision()))
	}
}

impl<State> Hash for Subgraph<State> {
	/// Feeds this `Subgraph<State>` into the supplied hasher.
	fn hash<H: Hasher>(&self, state: &mut H) {
		self.identity().hash(state);
		self.revision().hash(state);
	}
}

#[cfg(test)]
/// Regression tests for subgraph behavior.
mod tests {
	use super::Subgraph;
	#[cfg(test)]
	use crate::test_assertions::AssertResult as _;
	use crate::values::{
		PropertyMap,
		runtime::graph::{GraphKind, GraphRead, Mutable},
	};

	/// Verifies marks materialized result without new storage family.
	#[test]
	fn marks_materialized_result_without_new_storage_family() {
		let mut graph = Subgraph::<Mutable>::new();
		graph.graph.add_node(None, PropertyMap::new()).assert_ok();
		assert_eq!(graph.kind(), GraphKind::Subgraph);
		assert_eq!(graph.node_count().assert_ok(), 1);
	}
}
