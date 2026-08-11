use std::{
	cmp::Ordering,
	fmt::{self, Debug, Formatter},
	hash::{Hash, Hasher},
};

use super::{
	AtomId,
	EdgeTopology,
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
	values::runtime::{PropertyMap, Text, Uuid},
};

/// General-layout graph with explicit meta-atom construction APIs.
///
/// Metagraph intentionally shares the physical layout with [`Graph`]: unlike
/// tree, DAG and hypergraph families, its invariant does not enable a narrower
/// adjacency representation.
pub struct Metagraph<State = Frozen> {
	graph: Graph<State>,
}

pub type MutableMetagraph = Metagraph<Mutable>;
pub type FrozenMetagraph = Metagraph<Frozen>;

impl Metagraph<Mutable> {
	pub fn new() -> Self {
		Self {
			graph: Graph::new(),
		}
	}

	pub fn add_node(
		&mut self,
		label: Option<Text>,
		properties: PropertyMap,
	) -> FerrosGrynnResult<AtomId> {
		self.graph.add_node(label, properties)
	}

	pub fn add_metavertex(
		&mut self,
		inner: Graph<Frozen>,
		label: Option<Text>,
		properties: PropertyMap,
	) -> FerrosGrynnResult<AtomId> {
		self.graph.add_metavertex(inner, label, properties)
	}

	pub fn add_edge(
		&mut self,
		topology: EdgeTopology,
		label: Option<Text>,
		properties: PropertyMap,
	) -> FerrosGrynnResult<AtomId> {
		self.graph.add_edge(topology, label, properties)
	}

	pub fn add_metaedge(
		&mut self,
		topology: EdgeTopology,
		transition: Graph<Frozen>,
		label: Option<Text>,
		properties: PropertyMap,
	) -> FerrosGrynnResult<AtomId> {
		self.graph.add_metaedge(topology, transition, label, properties)
	}

	pub fn freeze(self) -> Metagraph<Frozen> {
		Metagraph {
			graph: self.graph.freeze(),
		}
	}

	pub fn flush<GWB: GraphWriteBack<Self>>(&mut self, writer: &mut GWB) -> FerrosGrynnResult<()> {
		let (origin, dirty) = self.graph.writeback_context()?;
		writer.flush(self, &origin, &dirty)?;
		self.graph.finish_writeback();
		Ok(())
	}

	pub(crate) fn transaction_dirty(&self) -> Vec<super::DirtyTarget> {
		self.graph.transaction_dirty()
	}

	pub(crate) fn transaction_snapshot(&self) -> Metagraph<Frozen> {
		Metagraph {
			graph: self.graph.transaction_snapshot(),
		}
	}
}

impl Default for Metagraph<Mutable> {
	fn default() -> Self {
		Self::new()
	}
}

impl Metagraph<Frozen> {
	pub fn thaw(self) -> Metagraph<Mutable> {
		Metagraph {
			graph: self.graph.thaw(),
		}
	}
}

impl Clone for Metagraph<Frozen> {
	fn clone(&self) -> Self {
		Self {
			graph: self.graph.clone(),
		}
	}
}

impl<State> Metagraph<State> {
	pub const fn kind(&self) -> GraphKind {
		GraphKind::Metagraph
	}

	pub const fn identity(&self) -> Uuid {
		self.graph.identity()
	}

	pub const fn revision(&self) -> u64 {
		self.graph.revision()
	}

	pub const fn node_count(&self) -> usize {
		self.graph.node_count()
	}

	pub const fn edge_count(&self) -> usize {
		self.graph.edge_count()
	}

	pub fn node(&self, id: AtomId) -> Option<NodeView<'_>> {
		self.graph.node(id)
	}

	pub fn edge(&self, id: AtomId) -> Option<EdgeView<'_>> {
		self.graph.edge(id)
	}

	pub const fn as_graph(&self) -> &Graph<State> {
		&self.graph
	}

	pub fn into_graph(self) -> Graph<State> {
		self.graph
	}

	pub fn try_from_graph(graph: Graph<State>) -> FerrosGrynnResult<Self> {
		validate_nested_references(&graph)?;
		Ok(Self {
			graph,
		})
	}
}

impl<State> GraphRead for Metagraph<State> {
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
		GraphKind::Metagraph
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

impl GraphMut for Metagraph<Mutable> {
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

impl<State> TryFrom<Graph<State>> for Metagraph<State> {
	type Error = crate::gql_status::FerrosGrynnError;

	fn try_from(value: Graph<State>) -> Result<Self, Self::Error> {
		Self::try_from_graph(value)
	}
}

impl QueryMaterialize for Metagraph<Mutable> {
	fn query_from<R: GraphRead>(source: &R) -> FerrosGrynnResult<Self> {
		Self::try_from_graph(Graph::<Mutable>::query_from(source)?)
	}
}

impl QueryMaterialize for Metagraph<Frozen> {
	fn query_from<R: GraphRead>(source: &R) -> FerrosGrynnResult<Self> {
		Metagraph::<Mutable>::query_from(source).map(Metagraph::freeze)
	}
}

impl<State> Debug for Metagraph<State> {
	fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
		formatter
			.debug_struct("Metagraph")
			.field("identity", &self.identity())
			.field("revision", &self.revision())
			.field("nodes", &self.node_count())
			.field("edges", &self.edge_count())
			.finish()
	}
}

impl<State> PartialEq for Metagraph<State> {
	fn eq(&self, other: &Self) -> bool {
		(self.identity(), self.revision()) == (other.identity(), other.revision())
	}
}

impl<State> Eq for Metagraph<State> {}

impl<State> PartialOrd for Metagraph<State> {
	fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
		Some(self.cmp(other))
	}
}

impl<State> Ord for Metagraph<State> {
	fn cmp(&self, other: &Self) -> Ordering {
		(self.identity(), self.revision()).cmp(&(other.identity(), other.revision()))
	}
}

impl<State> Hash for Metagraph<State> {
	fn hash<H: Hasher>(&self, state: &mut H) {
		self.identity().hash(state);
		self.revision().hash(state);
	}
}

fn validate_nested_references<R: GraphRead>(graph: &R) -> FerrosGrynnResult<()> {
	for id in graph.edge_ids() {
		let edge = graph.edge(id).ok_or_else(|| {
			crate::gql_status::FerrosGrynnError::atom_does_not_exist("edge", id.to_string())
		})?;
		for endpoint in edge.topology().endpoints() {
			if let super::EdgeEndpoint::Nested(qualified) = endpoint {
				let Some(first) = qualified.scope().first().copied() else {
					return Err(crate::gql_status::FerrosGrynnError::graph_constraint_violation(
						"metagraph",
						"a qualified endpoint has an empty scope",
					));
				};
				if !graph.node(first).is_some_and(NodeView::is_meta) {
					return Err(crate::gql_status::FerrosGrynnError::graph_constraint_violation(
						"metagraph",
						format!("qualified endpoint scope starts at non-meta atom {first}"),
					));
				}
			}
		}
	}
	Ok(())
}

#[cfg(test)]
mod tests {
	use super::Metagraph;
	use crate::values::{
		PropertyMap,
		runtime::graph::{EdgeEndpoint, EdgeTopology, Graph, Mutable, QualifiedAtomId},
	};

	#[test]
	fn exposes_explicit_meta_atom_api_over_general_layout() {
		let mut inner = Graph::<Mutable>::new();
		let nested = inner.add_node(None, PropertyMap::new()).unwrap();
		let mut graph = Metagraph::<Mutable>::new();
		let meta = graph.add_metavertex(inner.freeze(), None, PropertyMap::new()).unwrap();
		let outer = graph.add_node(None, PropertyMap::new()).unwrap();
		let nested = QualifiedAtomId::new([meta], nested).unwrap();
		let edge = graph
			.add_edge(
				EdgeTopology::directed([EdgeEndpoint::Nested(nested)], [EdgeEndpoint::Local(
					outer,
				)]),
				None,
				PropertyMap::new(),
			)
			.unwrap();

		assert!(graph.edge(edge).unwrap().topology().is_cross_level());
		assert_eq!(graph.kind(), super::GraphKind::Metagraph);
	}
}
