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
	pub(crate) fn restore_free_slot(
		&mut self,
		kind: AtomKind,
		slot: u64,
		generation: u32,
	) -> FerrosGrynnResult<()> {
		self.graph.restore_free_slot(kind, slot, generation)
	}

	pub(crate) fn slot_capacity(&self, kind: AtomKind) -> usize {
		self.graph.slot_capacity(kind)
	}

	pub(crate) fn slot_generation(&self, kind: AtomKind, slot: usize) -> Option<u32> {
		self.graph.slot_generation(kind, slot)
	}

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

	fn identity(&self) -> Uuid {
		self.graph.identity()
	}

	fn revision(&self) -> u64 {
		self.graph.revision()
	}

	fn kind(&self) -> GraphKind {
		GraphKind::Subgraph
	}

	fn properties(&self) -> FerrosGrynnResult<Cow<'_, PropertyMap>> {
		GraphRead::properties(&self.graph)
	}

	fn node_ids(&self) -> FerrosGrynnResult<Self::NodeIds<'_>> {
		GraphRead::node_ids(&self.graph)
	}

	fn edge_ids(&self) -> FerrosGrynnResult<Self::EdgeIds<'_>> {
		GraphRead::edge_ids(&self.graph)
	}

	fn node(&self, id: AtomId) -> FerrosGrynnResult<Option<Self::Node<'_>>> {
		GraphRead::node(&self.graph, id)
	}

	fn edge(&self, id: AtomId) -> FerrosGrynnResult<Option<Self::Edge<'_>>> {
		GraphRead::edge(&self.graph, id)
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

	fn node_label_mut(&mut self, id: AtomId) -> FerrosGrynnResult<&mut Option<Text>> {
		self.graph.node_label_mut(id)
	}

	fn node_properties_mut(&mut self, id: AtomId) -> FerrosGrynnResult<&mut PropertyMap> {
		self.graph.node_properties_mut(id)
	}

	fn edge_label_mut(&mut self, id: AtomId) -> FerrosGrynnResult<&mut Option<Text>> {
		self.graph.edge_label_mut(id)
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
		assert_eq!(graph.node_count().unwrap(), 1);
	}
}
