use std::{
	borrow::Cow,
	cmp::Ordering,
	hash::{Hash, Hasher},
	mem::{size_of, size_of_val},
};

use super::{
	AtomId,
	AtomKind,
	Dag,
	EdgeEndpoint,
	EdgeTopology,
	EdgeView,
	Forest,
	Frozen,
	Graph,
	GraphKind,
	GraphRead,
	GraphTopologyConstraints,
	Hypergraph,
	Metagraph,
	NodeView,
	RootedTree,
	Subgraph,
	Tree,
};
use crate::{
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	values::runtime::{PropertyMap, RecordId, Text, Uuid},
};

/// Transport sum type for immutable graph query results.
///
/// Operators with a fixed contract still return their concrete Rust type.
/// This enum is used only when a graph must travel through generic
/// [`crate::values::Value`] transport, and its discriminant is always available
/// through [`Self::kind`].
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum MaterializedGraph {
	/// General graph family.
	Graph(
		/// General graph family.
		Graph<Frozen>,
	),
	/// Connected acyclic tree family.
	Tree(
		/// Connected acyclic tree family.
		Tree<Frozen>,
	),
	/// Tree family with an explicitly selected root.
	RootedTree(
		/// Tree family with an explicitly selected root.
		RootedTree<Frozen>,
	),
	/// Acyclic forest family allowing disconnected components.
	Forest(
		/// Acyclic forest family allowing disconnected components.
		Forest<Frozen>,
	),
	/// Directed acyclic graph family.
	Dag(
		/// Directed acyclic graph family.
		Dag<Frozen>,
	),
	/// Hypergraph family with multi-endpoint incidences.
	Hypergraph(
		/// Hypergraph family with multi-endpoint incidences.
		Hypergraph<Frozen>,
	),
	/// Metagraph family retaining graph containment.
	Metagraph(
		/// Metagraph family retaining graph containment.
		Metagraph<Frozen>,
	),
	/// Subgraph family selecting contents from a containing graph.
	Subgraph(
		/// Subgraph family selecting contents from a containing graph.
		Subgraph<Frozen>,
	),
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

	/// Returns identity for `MaterializedGraph`.
	fn identity(&self) -> Uuid {
		self.identity()
	}

	/// Returns revision for `MaterializedGraph`.
	fn revision(&self) -> u64 {
		self.revision()
	}

	/// Returns kind for `MaterializedGraph`.
	fn kind(&self) -> GraphKind {
		self.kind()
	}

	/// Returns properties for `MaterializedGraph`.
	fn properties(&self) -> FerrosGrynnResult<Cow<'_, PropertyMap>> {
		Ok(Cow::Borrowed(Self::properties(self)))
	}

	/// Returns topology constraints for `MaterializedGraph`.
	fn topology_constraints(&self) -> GraphTopologyConstraints {
		Self::topology_constraints(self)
	}

	/// Returns node ids for `MaterializedGraph`.
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

	/// Returns edge ids for `MaterializedGraph`.
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

	/// Returns node for `MaterializedGraph`.
	fn node(&self, id: AtomId) -> FerrosGrynnResult<Option<Self::Node<'_>>> {
		Ok(Self::node(self, id))
	}

	/// Returns edge for `MaterializedGraph`.
	fn edge(&self, id: AtomId) -> FerrosGrynnResult<Option<Self::Edge<'_>>> {
		Ok(Self::edge(self, id))
	}

	/// Returns materialization origin for `MaterializedGraph`.
	fn materialization_origin(&self) -> Option<&super::MaterializationOrigin> {
		match self {
			Self::Graph(graph) => GraphRead::materialization_origin(graph),
			Self::Tree(graph) => GraphRead::materialization_origin(graph),
			Self::RootedTree(graph) => GraphRead::materialization_origin(graph),
			Self::Forest(graph) => GraphRead::materialization_origin(graph),
			Self::Dag(graph) => GraphRead::materialization_origin(graph),
			Self::Hypergraph(graph) => GraphRead::materialization_origin(graph),
			Self::Metagraph(graph) => GraphRead::materialization_origin(graph),
			Self::Subgraph(graph) => GraphRead::materialization_origin(graph),
		}
	}
}

impl MaterializedGraph {
	/// Converts one owned general candidate through the existing concrete
	/// family validators.
	pub(crate) fn from_general(
		graph: Graph<Frozen>,
		family: GraphKind,
		root: Option<AtomId>,
	) -> FerrosGrynnResult<Self> {
		let validators = graph.topology_constraints();
		let result = match family {
			GraphKind::Graph => Self::Graph(graph),
			GraphKind::Tree => Self::Tree(Tree::try_from(graph)?),
			GraphKind::RootedTree => Self::RootedTree(match root {
				Some(root) => RootedTree::try_from_graph_with_root(graph, root)?,
				None => RootedTree::try_from(graph)?,
			}),
			GraphKind::Forest => Self::Forest(Forest::try_from(graph)?),
			GraphKind::Dag => Self::Dag(Dag::try_from(graph)?),
			GraphKind::Hypergraph => Self::Hypergraph(Hypergraph::try_from(graph)?),
			GraphKind::Metagraph => Self::Metagraph(Metagraph::try_from(graph)?),
			GraphKind::Subgraph => Self::Subgraph(Subgraph::from_graph(graph)),
		};
		super::validate_topology_constraints(&result, validators)?;
		Ok(result)
	}

	/// Estimates owned heap bytes, rejecting a transaction on accounting
	/// failure.
	pub(crate) fn estimated_heap_bytes(&self) -> usize {
		match estimated_graph_heap_bytes(
			self,
			self.slot_capacity(AtomKind::Node),
			self.slot_capacity(AtomKind::Edge),
		) {
			Ok(bytes) => bytes,
			Err(_error) => usize::MAX,
		}
	}

	/// Returns the slot capacity result.
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

	/// Returns node count for `MaterializedGraph`.
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

	/// Returns edge count for `MaterializedGraph`.
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

	/// Reports whether the contains condition holds for `MaterializedGraph`.
	pub fn contains(&self, id: AtomId) -> bool {
		match id.kind() {
			Some(super::AtomKind::Node) => self.node(id).is_some(),
			Some(super::AtomKind::Edge) => self.edge(id).is_some(),
			None => false,
		}
	}

	/// Returns properties for `MaterializedGraph`.
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

	/// Returns topology constraints for `MaterializedGraph`.
	pub const fn topology_constraints(&self) -> GraphTopologyConstraints {
		match self {
			Self::Graph(value) => value.topology_constraints(),
			Self::Tree(value) => value.topology_constraints(),
			Self::RootedTree(value) => value.topology_constraints(),
			Self::Forest(value) => value.topology_constraints(),
			Self::Dag(value) => value.topology_constraints(),
			Self::Hypergraph(value) => value.topology_constraints(),
			Self::Metagraph(value) => value.topology_constraints(),
			Self::Subgraph(value) => value.topology_constraints(),
		}
	}

	/// Returns node for `MaterializedGraph`.
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

	/// Returns edge for `MaterializedGraph`.
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

	/// Returns kind for `MaterializedGraph`.
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

	/// Returns identity for `MaterializedGraph`.
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

	/// Returns revision for `MaterializedGraph`.
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

	/// Converts this value into general for `MaterializedGraph`.
	///
	/// # Errors
	///
	/// Propagates validation or execution failures from the underlying
	/// operation.
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

	/// Borrows general for `MaterializedGraph`.
	pub const fn as_general(&self) -> Option<&Graph<Frozen>> {
		match self {
			Self::Graph(value) => Some(value),
			_ => None,
		}
	}
}

/// Conservatively estimates heap bytes owned by one nested general graph.
pub fn estimated_nested_graph_heap_bytes(graph: &Graph<Frozen>) -> FerrosGrynnResult<usize> {
	estimated_graph_heap_bytes(
		graph,
		graph.slot_capacity(AtomKind::Node),
		graph.slot_capacity(AtomKind::Edge),
	)
}

/// Returns the estimated graph fixed heap bytes result.
pub fn estimated_graph_fixed_heap_bytes(
	container_bytes: usize,
	node_capacity: usize,
	edge_capacity: usize,
	properties: &PropertyMap,
) -> usize {
	let node_slot_bytes = size_of::<Option<RecordId>>()
		.saturating_add(size_of::<Option<Text>>())
		.saturating_add(size_of::<PropertyMap>())
		.saturating_add(size_of::<Option<usize>>())
		.saturating_add(size_of::<u32>());
	let edge_slot_bytes = node_slot_bytes
		.saturating_add(size_of::<Option<EdgeTopology>>())
		.saturating_add(size_of::<Option<usize>>());
	container_bytes
		.saturating_add(node_capacity.saturating_mul(node_slot_bytes))
		.saturating_add(edge_capacity.saturating_mul(edge_slot_bytes))
		.saturating_add(properties.estimated_heap_bytes())
}

/// Returns the estimated node fields heap bytes result.
pub fn estimated_node_fields_heap_bytes(
	record_id: &RecordId,
	label: Option<&Text>,
	properties: &PropertyMap,
	inner: Option<&Graph<Frozen>>,
) -> FerrosGrynnResult<usize> {
	let mut bytes = record_id
		.estimated_heap_bytes()
		.saturating_add(label.map_or(0, Text::estimated_heap_bytes))
		.saturating_add(properties.estimated_heap_bytes());
	if let Some(inner) = inner {
		bytes = bytes.saturating_add(estimated_nested_graph_heap_bytes(inner)?);
	}
	Ok(bytes)
}

/// Returns the estimated edge fields heap bytes result.
pub fn estimated_edge_fields_heap_bytes(
	record_id: &RecordId,
	label: Option<&Text>,
	properties: &PropertyMap,
	topology: &EdgeTopology,
	transition: Option<&Graph<Frozen>>,
) -> FerrosGrynnResult<usize> {
	let mut bytes = record_id
		.estimated_heap_bytes()
		.saturating_add(label.map_or(0, Text::estimated_heap_bytes))
		.saturating_add(properties.estimated_heap_bytes())
		.saturating_add(estimated_topology_heap_bytes(topology));
	if let Some(transition) = transition {
		bytes = bytes.saturating_add(estimated_nested_graph_heap_bytes(transition)?);
	}
	Ok(bytes)
}

/// Accounts retained layouts once even when child bindings share physical
/// storage.
fn estimated_graph_heap_bytes<'a, Reader>(
	graph: &'a Reader,
	node_capacity: usize,
	edge_capacity: usize,
) -> FerrosGrynnResult<usize>
where
	Reader: GraphRead<Node<'a> = NodeView<'a>, Edge<'a> = EdgeView<'a>> + 'a,
{
	let mut bytes = 0_usize;
	visit_graph_heap_bytes(graph, node_capacity, edge_capacity, &mut |additional, _work| {
		bytes = bytes.saturating_add(additional);
		Ok(())
	})?;
	Ok(bytes)
}

/// Admits the borrowed runtime image before its COW snapshot clones metadata.
pub fn admit_graph_snapshot<'a, Reader>(
	graph: &'a Reader,
	admit: &mut dyn FnMut(usize, u64) -> FerrosGrynnResult<()>,
) -> FerrosGrynnResult<()>
where
	Reader: GraphRead<Node<'a> = NodeView<'a>, Edge<'a> = EdgeView<'a>> + 'a,
{
	visit_graph_heap_bytes(graph, 0, 0, admit)
}

/// Visits each physical graph once through the shared retained-memory ledger.
fn visit_graph_heap_bytes<'a, Reader>(
	graph: &'a Reader,
	node_capacity: usize,
	edge_capacity: usize,
	admit: &mut dyn FnMut(usize, u64) -> FerrosGrynnResult<()>,
) -> FerrosGrynnResult<()>
where
	Reader: GraphRead<Node<'a> = NodeView<'a>, Edge<'a> = EdgeView<'a>> + 'a,
{
	let mut pending = Vec::new();
	let mut visited = std::collections::BTreeSet::new();
	visit_graph_fields(graph, node_capacity, edge_capacity, &mut pending, admit)?;
	while let Some(child) = pending.pop() {
		admit(0, 1)?;
		let identity = std::ptr::from_ref(child) as usize;
		if visited.contains(&identity) {
			continue;
		}
		admit(128, 1)?;
		visited.insert(identity);
		visit_graph_fields(
			child,
			child.slot_capacity(AtomKind::Node),
			child.slot_capacity(AtomKind::Edge),
			&mut pending,
			admit,
		)?;
	}
	Ok(())
}

/// Charges the graph fields and schedules its immutable child layouts.
fn visit_graph_fields<'a, Reader>(
	graph: &'a Reader,
	node_capacity: usize,
	edge_capacity: usize,
	pending: &mut Vec<&'a Graph<Frozen>>,
	admit: &mut dyn FnMut(usize, u64) -> FerrosGrynnResult<()>,
) -> FerrosGrynnResult<()>
where
	Reader: GraphRead<Node<'a> = NodeView<'a>, Edge<'a> = EdgeView<'a>> + 'a,
{
	let properties = graph.properties()?;
	let bytes = estimated_graph_fixed_heap_bytes(
		size_of_val(graph),
		node_capacity,
		edge_capacity,
		properties.as_ref(),
	);
	admit(bytes, 1)?;
	if let Some(origin) = graph.materialization_origin() {
		admit(origin.estimated_heap_bytes(), 1)?;
	}
	for id in graph.node_ids()? {
		admit(0, 1)?;
		let id = id?;
		let node = graph.node(id)?.ok_or_else(|| {
			FerrosGrynnError::invalid_storage_file(
				"nested graph node disappeared during accounting",
			)
		})?;
		let record_id = node.record_id();
		let label = node.label_text();
		let properties = node.properties();
		let inner = node.inner_graph();
		admit(estimated_node_fields_heap_bytes(record_id, label, properties, None)?, 0)?;
		if let Some(inner) = inner {
			admit(128, 1)?;
			pending.push(inner);
		}
	}
	for id in graph.edge_ids()? {
		admit(0, 1)?;
		let id = id?;
		let edge = graph.edge(id)?.ok_or_else(|| {
			FerrosGrynnError::invalid_storage_file(
				"nested graph edge disappeared during accounting",
			)
		})?;
		let record_id = edge.record_id();
		let label = edge.label_text();
		let properties = edge.properties();
		let topology = edge.topology();
		let transition = edge.transition_graph();
		admit(estimated_edge_fields_heap_bytes(record_id, label, properties, topology, None)?, 0)?;
		if let Some(transition) = transition {
			admit(128, 1)?;
			pending.push(transition);
		}
	}
	Ok(())
}

/// Estimates the recursively owned endpoint buffers of one topology.
pub fn estimated_topology_heap_bytes(topology: &EdgeTopology) -> usize {
	let mut bytes = topology
		.retained_endpoint_slots()
		.saturating_mul(size_of::<EdgeEndpoint>().saturating_add(size_of::<AtomId>()));
	for endpoint in topology.endpoints() {
		if let EdgeEndpoint::Nested(qualified) = endpoint {
			bytes = bytes.saturating_add(
				qualified.scope().len().saturating_mul(size_of::<AtomId>()).saturating_mul(2),
			);
		}
	}
	bytes
}

/// Expands the impl exact access declarations used by this module.
macro_rules! impl_exact_access {
	($type:ty, $variant:ident, $as_method:ident, $into_method:ident) => {
		impl MaterializedGraph {
			/// Borrows this graph family when the materialized variant matches.
			pub const fn $as_method(&self) -> Option<&$type> {
				match self {
					Self::$variant(value) => Some(value),
					_ => None,
				}
			}

			/// Extracts the concrete graph family from this owned result.
			///
			/// # Errors
			///
			/// Returns a conversion error if the stored graph has another family.
			pub fn $into_method(self) -> FerrosGrynnResult<$type> {
				<$type>::try_from(self)
			}
		}

		impl TryFrom<MaterializedGraph> for $type {
			type Error = FerrosGrynnError;

			/// Checks the source variant and converts its payload to the requested
			/// type.
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
	/// Compares this `MaterializedGraph` with another value for equality.
	fn eq(&self, other: &Self) -> bool {
		(self.identity(), self.revision()) == (other.identity(), other.revision())
	}
}

impl Eq for MaterializedGraph {}

impl PartialOrd for MaterializedGraph {
	/// Compares this `MaterializedGraph` with another value when they are
	/// ordered.
	fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
		Some(self.cmp(other))
	}
}

impl Ord for MaterializedGraph {
	/// Compares this `MaterializedGraph` with another value using its ordering
	/// contract.
	fn cmp(&self, other: &Self) -> Ordering {
		(self.identity(), self.revision()).cmp(&(other.identity(), other.revision()))
	}
}

impl Hash for MaterializedGraph {
	/// Feeds this `MaterializedGraph` into the supplied hasher.
	fn hash<H: Hasher>(&self, state: &mut H) {
		self.identity().hash(state);
		self.revision().hash(state);
	}
}

/// Expands the impl from graph family declarations used by this module.
macro_rules! impl_from_graph_family {
	($type:ty, $variant:ident) => {
		impl From<$type> for MaterializedGraph {
			/// Converts the source payload into its corresponding value variant.
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
/// Regression tests for materialized behavior.
mod tests {
	use super::MaterializedGraph;
	#[cfg(test)]
	use crate::test_assertions::AssertResult as _;
	use crate::values::runtime::graph::{Graph, Mutable};

	/// Verifies preserves exact discriminant in generic transport.
	#[test]
	fn preserves_exact_discriminant_in_generic_transport() {
		let graph = Graph::<Mutable>::new().freeze();
		let identity = graph.identity();
		let value = MaterializedGraph::from(graph);

		assert_eq!(value.kind(), super::GraphKind::Graph);
		assert_eq!(value.identity(), identity);
		assert_eq!(value.into_general().assert_ok().node_count(), 0);
	}
}
