use std::{
	borrow::Cow,
	cmp::Ordering,
	fmt::{self, Debug, Formatter},
	hash::{Hash, Hasher},
};

use super::{
	AtomId,
	AtomKind,
	EdgeRead,
	EdgeTopology,
	EdgeView,
	Frozen,
	Graph,
	GraphKind,
	GraphMut,
	GraphRead,
	GraphTopologyConstraints,
	GraphWriteBack,
	Mutable,
	NodeRead,
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
	/// Graph associated with this metagraph.
	graph: Graph<State>,
}

pub type MutableMetagraph = Metagraph<Mutable>;
pub type FrozenMetagraph = Metagraph<Frozen>;

impl Metagraph<Mutable> {
	/// Constructs `Metagraph<Mutable>` from the supplied initial state.
	pub fn new() -> Self {
		Self {
			graph: Graph::new(),
		}
	}

	/// Returns add node for `Metagraph<Mutable>`.
	///
	/// # Errors
	///
	/// Propagates validation or execution failures from the underlying
	/// operation.
	pub fn add_node(
		&mut self,
		label: Option<Text>,
		properties: PropertyMap,
	) -> FerrosGrynnResult<AtomId> {
		self.graph.add_node(label, properties)
	}

	/// Returns add metavertex for `Metagraph<Mutable>`.
	///
	/// # Errors
	///
	/// Propagates validation or execution failures from the underlying
	/// operation.
	pub fn add_metavertex(
		&mut self,
		inner: Graph<Frozen>,
		label: Option<Text>,
		properties: PropertyMap,
	) -> FerrosGrynnResult<AtomId> {
		self.graph.add_metavertex(inner, label, properties)
	}

	/// Returns add edge for `Metagraph<Mutable>`.
	///
	/// # Errors
	///
	/// Propagates validation or execution failures from the underlying
	/// operation.
	pub fn add_edge(
		&mut self,
		topology: EdgeTopology,
		label: Option<Text>,
		properties: PropertyMap,
	) -> FerrosGrynnResult<AtomId> {
		self.graph.add_edge(topology, label, properties)
	}

	/// Returns add metaedge for `Metagraph<Mutable>`.
	///
	/// # Errors
	///
	/// Propagates validation or execution failures from the underlying
	/// operation.
	pub fn add_metaedge(
		&mut self,
		topology: EdgeTopology,
		transition: Graph<Frozen>,
		label: Option<Text>,
		properties: PropertyMap,
	) -> FerrosGrynnResult<AtomId> {
		self.graph.add_metaedge(topology, transition, label, properties)
	}

	/// Returns freeze for `Metagraph<Mutable>`.
	pub fn freeze(self) -> Metagraph<Frozen> {
		Metagraph {
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

	/// Performs the flush operation for `Metagraph<Mutable>`.
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

	/// Returns the transaction dirty result.
	#[cfg(test)]
	pub(crate) fn transaction_dirty(&self) -> Vec<super::DirtyTarget> {
		self.graph.transaction_dirty()
	}
}

impl Default for Metagraph<Mutable> {
	/// Constructs the default initial state of `Metagraph<Mutable>`.
	fn default() -> Self {
		Self::new()
	}
}

impl Metagraph<Frozen> {
	/// Returns thaw for `Metagraph<Frozen>`.
	pub fn thaw(self) -> Metagraph<Mutable> {
		Metagraph {
			graph: self.graph.thaw(),
		}
	}
}

impl Clone for Metagraph<Frozen> {
	/// Returns an owned clone of this `Metagraph<Frozen>`.
	fn clone(&self) -> Self {
		Self {
			graph: self.graph.clone(),
		}
	}
}

impl<State> Metagraph<State> {
	/// Returns the slot capacity result.
	pub(crate) fn slot_capacity(&self, kind: AtomKind) -> usize {
		self.graph.slot_capacity(kind)
	}

	/// Returns kind for `Metagraph<State>`.
	pub const fn kind(&self) -> GraphKind {
		GraphKind::Metagraph
	}

	/// Returns identity for `Metagraph<State>`.
	pub const fn identity(&self) -> Uuid {
		self.graph.identity()
	}

	/// Returns revision for `Metagraph<State>`.
	pub const fn revision(&self) -> u64 {
		self.graph.revision()
	}

	/// Returns topology constraints for `Metagraph<State>`.
	pub const fn topology_constraints(&self) -> GraphTopologyConstraints {
		self.graph.topology_constraints()
	}

	/// Returns node count for `Metagraph<State>`.
	pub const fn node_count(&self) -> usize {
		self.graph.node_count()
	}

	/// Returns edge count for `Metagraph<State>`.
	pub const fn edge_count(&self) -> usize {
		self.graph.edge_count()
	}

	/// Returns node for `Metagraph<State>`.
	pub fn node(&self, id: AtomId) -> Option<NodeView<'_>> {
		self.graph.node(id)
	}

	/// Returns edge for `Metagraph<State>`.
	pub fn edge(&self, id: AtomId) -> Option<EdgeView<'_>> {
		self.graph.edge(id)
	}

	/// Borrows the stored graph of this `Metagraph<State>`.
	pub const fn as_graph(&self) -> &Graph<State> {
		&self.graph
	}

	/// Returns the stored graph of this `Metagraph<State>`.
	pub fn into_graph(self) -> Graph<State> {
		self.graph
	}

	/// Attempts from graph for `Metagraph<State>`.
	///
	/// # Errors
	///
	/// Propagates errors returned by the underlying operation.
	pub fn try_from_graph(graph: Graph<State>) -> FerrosGrynnResult<Self> {
		validate_nested_references(&graph)?;
		Ok(Self {
			graph,
		})
	}
}

impl<State> GraphRead for Metagraph<State> {
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

	/// Returns identity for `Metagraph<State>`.
	fn identity(&self) -> Uuid {
		self.graph.identity()
	}

	/// Returns revision for `Metagraph<State>`.
	fn revision(&self) -> u64 {
		self.graph.revision()
	}

	/// Returns kind for `Metagraph<State>`.
	fn kind(&self) -> GraphKind {
		GraphKind::Metagraph
	}

	/// Returns properties for `Metagraph<State>`.
	fn properties(&self) -> FerrosGrynnResult<Cow<'_, PropertyMap>> {
		GraphRead::properties(&self.graph)
	}

	/// Returns topology constraints for `Metagraph<State>`.
	fn topology_constraints(&self) -> GraphTopologyConstraints {
		self.graph.topology_constraints()
	}

	/// Returns node ids for `Metagraph<State>`.
	fn node_ids(&self) -> FerrosGrynnResult<Self::NodeIds<'_>> {
		GraphRead::node_ids(&self.graph)
	}

	/// Returns edge ids for `Metagraph<State>`.
	fn edge_ids(&self) -> FerrosGrynnResult<Self::EdgeIds<'_>> {
		GraphRead::edge_ids(&self.graph)
	}

	/// Returns node for `Metagraph<State>`.
	fn node(&self, id: AtomId) -> FerrosGrynnResult<Option<Self::Node<'_>>> {
		GraphRead::node(&self.graph, id)
	}

	/// Returns edge for `Metagraph<State>`.
	fn edge(&self, id: AtomId) -> FerrosGrynnResult<Option<Self::Edge<'_>>> {
		GraphRead::edge(&self.graph, id)
	}

	/// Returns storage mode for `Metagraph<State>`.
	fn storage_mode(&self) -> super::GraphStorageMode {
		self.graph.storage_mode()
	}

	/// Returns snapshot id for `Metagraph<State>`.
	fn snapshot_id(&self) -> super::SnapshotId {
		self.graph.snapshot_id()
	}

	/// Returns graph ref for `Metagraph<State>`.
	fn graph_ref(&self) -> Option<&super::GraphRef> {
		self.graph.graph_ref()
	}

	/// Returns materialization origin for `Metagraph<State>`.
	fn materialization_origin(&self) -> Option<&super::MaterializationOrigin> {
		self.graph.materialization_origin()
	}
}

impl GraphMut for Metagraph<Mutable> {
	/// Returns properties mut for `Metagraph<Mutable>`.
	fn properties_mut(&mut self) -> FerrosGrynnResult<&mut PropertyMap> {
		self.graph.properties_mut()
	}

	/// Returns node label mut for `Metagraph<Mutable>`.
	fn node_label_mut(&mut self, id: AtomId) -> FerrosGrynnResult<&mut Option<Text>> {
		self.graph.node_label_mut(id)
	}

	/// Returns node properties mut for `Metagraph<Mutable>`.
	fn node_properties_mut(&mut self, id: AtomId) -> FerrosGrynnResult<&mut PropertyMap> {
		self.graph.node_properties_mut(id)
	}

	/// Returns edge label mut for `Metagraph<Mutable>`.
	fn edge_label_mut(&mut self, id: AtomId) -> FerrosGrynnResult<&mut Option<Text>> {
		self.graph.edge_label_mut(id)
	}

	/// Returns edge properties mut for `Metagraph<Mutable>`.
	fn edge_properties_mut(&mut self, id: AtomId) -> FerrosGrynnResult<&mut PropertyMap> {
		self.graph.edge_properties_mut(id)
	}

	/// Removes atom for `Metagraph<Mutable>`.
	fn remove_atom(&mut self, id: AtomId) -> FerrosGrynnResult<()> {
		self.graph.remove_atom(id)
	}
}

impl<State> TryFrom<Graph<State>> for Metagraph<State> {
	type Error = crate::gql_status::FerrosGrynnError;

	/// Converts `Graph<State>` into `Metagraph<State>` after validating the
	/// conversion.
	fn try_from(value: Graph<State>) -> Result<Self, Self::Error> {
		Self::try_from_graph(value)
	}
}

impl QueryMaterialize for Metagraph<Mutable> {
	/// Returns query from for `Metagraph<Mutable>`.
	fn query_from<R: GraphRead>(source: &R) -> FerrosGrynnResult<Self> {
		Self::try_from_graph(Graph::<Mutable>::query_from(source)?)
	}
}

impl QueryMaterialize for Metagraph<Frozen> {
	/// Returns query from for `Metagraph<Frozen>`.
	fn query_from<R: GraphRead>(source: &R) -> FerrosGrynnResult<Self> {
		Metagraph::<Mutable>::query_from(source).map(Metagraph::freeze)
	}
}

impl<State> Debug for Metagraph<State> {
	/// Formats `Metagraph<State>` using the requested formatter.
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
	/// Compares this `Metagraph<State>` with another value for equality.
	fn eq(&self, other: &Self) -> bool {
		(self.identity(), self.revision()) == (other.identity(), other.revision())
	}
}

impl<State> Eq for Metagraph<State> {}

impl<State> PartialOrd for Metagraph<State> {
	/// Compares this `Metagraph<State>` with another value when they are
	/// ordered.
	fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
		Some(self.cmp(other))
	}
}

impl<State> Ord for Metagraph<State> {
	/// Compares this `Metagraph<State>` with another value using its ordering
	/// contract.
	fn cmp(&self, other: &Self) -> Ordering {
		(self.identity(), self.revision()).cmp(&(other.identity(), other.revision()))
	}
}

impl<State> Hash for Metagraph<State> {
	/// Feeds this `Metagraph<State>` into the supplied hasher.
	fn hash<H: Hasher>(&self, state: &mut H) {
		self.identity().hash(state);
		self.revision().hash(state);
	}
}

/// Validates nested references.
///
/// # Errors
/// Propagates validation, resource-admission or callback errors from the
/// operation.
pub fn validate_nested_references<R: GraphRead>(graph: &R) -> FerrosGrynnResult<()> {
	for id in graph.edge_ids()? {
		let id = id?;
		let edge = graph.edge(id)?.ok_or_else(|| {
			crate::gql_status::FerrosGrynnError::atom_does_not_exist("edge", id.to_string())
		})?;
		let topology = edge.topology()?;
		for endpoint in topology.endpoints() {
			if let super::EdgeEndpoint::Nested(qualified) = endpoint {
				let Some(first) = qualified.scope().first().copied() else {
					return Err(crate::gql_status::FerrosGrynnError::graph_constraint_violation(
						"metagraph",
						"a qualified endpoint has an empty scope",
					));
				};
				let is_meta = match first.kind() {
					Some(super::AtomKind::Node) => match graph.node(first)? {
						Some(node) => node.is_meta()?,
						None => false,
					},
					Some(super::AtomKind::Edge) => match graph.edge(first)? {
						Some(edge) => edge.is_meta()?,
						None => false,
					},
					None => false,
				};
				if !is_meta {
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
/// Regression tests for metagraph behavior.
mod tests {
	use super::Metagraph;
	#[cfg(test)]
	use crate::test_assertions::{AssertOption as _, AssertResult as _};
	use crate::values::{
		PropertyMap,
		runtime::graph::{EdgeEndpoint, EdgeTopology, Graph, Mutable, QualifiedAtomId},
	};

	/// Verifies exposes explicit meta atom api over general layout.
	#[test]
	fn exposes_explicit_meta_atom_api_over_general_layout() {
		let mut inner = Graph::<Mutable>::new();
		let nested = inner.add_node(None, PropertyMap::new()).assert_ok();
		let mut graph = Metagraph::<Mutable>::new();
		let meta = graph.add_metavertex(inner.freeze(), None, PropertyMap::new()).assert_ok();
		let outer = graph.add_node(None, PropertyMap::new()).assert_ok();
		let nested = QualifiedAtomId::new([meta], nested).assert_ok();
		let edge = graph
			.add_edge(
				EdgeTopology::directed([EdgeEndpoint::Nested(nested)], [EdgeEndpoint::Local(
					outer,
				)]),
				None,
				PropertyMap::new(),
			)
			.assert_ok();

		assert!(graph.edge(edge).assert_some().topology().is_cross_level());
		assert_eq!(graph.kind(), super::GraphKind::Metagraph);
	}

	/// Verifies owned anonymous children are meta through generic read traits.
	#[test]
	fn owned_anonymous_children_are_meta_through_generic_read_traits() {
		use crate::test_assertions::AssertResult as _;

		let child = Graph::<Mutable>::new().freeze();
		let mut graph = Metagraph::<Mutable>::new();
		let node = graph.add_metavertex(child.clone(), None, PropertyMap::new()).assert_ok();
		let edge = graph
			.add_metaedge(EdgeTopology::directed([node], [node]), child, None, PropertyMap::new())
			.assert_ok();
		let node = graph
			.node(node)
			.ok_or_else(|| {
				crate::gql_status::FerrosGrynnError::invalid_argument("missing meta node")
			})
			.assert_ok();
		let edge = graph
			.edge(edge)
			.ok_or_else(|| {
				crate::gql_status::FerrosGrynnError::invalid_argument("missing meta edge")
			})
			.assert_ok();
		assert!(crate::graph::NodeRead::inner_graph_ref(&node).assert_ok().is_none());
		assert!(crate::graph::EdgeRead::transition_graph_ref(&edge).assert_ok().is_none());
		assert!(crate::graph::NodeRead::is_meta(&node).assert_ok());
		assert!(crate::graph::EdgeRead::is_meta(&edge).assert_ok());
	}
}
