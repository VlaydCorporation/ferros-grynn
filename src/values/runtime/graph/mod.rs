use std::{
	cmp::Ordering,
	fmt::{self, Debug, Display, Formatter},
	hash::{Hash, Hasher},
	marker::PhantomData,
};

use super::{PropertyMap, RecordId, Text, Uuid};
use crate::gql_status::{FerrosGrynnError, FerrosGrynnResult};

const KIND_SHIFT: u32 = 40;
const GENERATION_SHIFT: u32 = 41;
const KIND_BIT: u64 = 1 << KIND_SHIFT;
const SLOT_MASK: u64 = (1 << KIND_SHIFT) - 1;

#[derive(Debug, Default, Clone, Copy, PartialOrd, PartialEq, Eq, Ord, Hash)]
pub struct Mutable;

#[derive(Debug, Default, Clone, Copy, PartialOrd, PartialEq, Eq, Ord, Hash)]
pub struct Frozen;

#[derive(Debug, Clone, Copy, PartialOrd, PartialEq, Eq, Ord, Hash)]
#[non_exhaustive]
pub enum GraphKind {
	Graph,
	Tree,
	RootedTree,
	Forest,
	Dag,
	Hypergraph,
	Metagraph,
}

#[derive(Debug, Clone, Copy, PartialOrd, PartialEq, Eq, Ord, Hash)]
#[non_exhaustive]
pub enum GraphStorageMode {
	InMemory,
	DiskBacked,
	Hybrid,
}

#[derive(Debug, Clone, PartialOrd, PartialEq, Eq, Ord, Hash)]
pub struct GraphRef {
	id: RecordId,
	kind: GraphKind,
	mode: GraphStorageMode,
}

impl GraphRef {
	pub fn new(id: RecordId, kind: GraphKind, mode: GraphStorageMode) -> Self {
		Self {
			id,
			kind,
			mode,
		}
	}

	pub fn id(&self) -> &RecordId {
		&self.id
	}

	pub const fn kind(&self) -> GraphKind {
		self.kind
	}

	pub const fn storage_mode(&self) -> GraphStorageMode {
		self.mode
	}
}

#[derive(Debug, Clone, Copy, PartialOrd, PartialEq, Eq, Ord, Hash)]
pub enum AtomKind {
	Node,
	Edge,
}

impl Display for AtomKind {
	fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
		formatter.write_str(match self {
			Self::Node => "node",
			Self::Edge => "edge",
		})
	}
}

#[derive(Clone, Copy, PartialOrd, PartialEq, Eq, Ord, Hash)]
#[repr(transparent)]
pub struct AtomId(u64);

impl AtomId {
	pub const NULL: Self = Self(u64::MAX);

	const fn node(slot: u64) -> Self {
		Self(slot)
	}

	const fn edge(slot: u64) -> Self {
		Self(KIND_BIT | slot)
	}

	pub const fn from_raw(value: u64) -> Self {
		Self(value)
	}

	pub const fn raw(self) -> u64 {
		self.0
	}

	pub const fn is_null(self) -> bool {
		self.0 == Self::NULL.0
	}

	pub const fn kind(self) -> Option<AtomKind> {
		if self.is_null() {
			None
		} else if self.0 & KIND_BIT == 0 {
			Some(AtomKind::Node)
		} else {
			Some(AtomKind::Edge)
		}
	}

	pub const fn slot(self) -> u64 {
		self.0 & SLOT_MASK
	}

	pub const fn generation(self) -> u32 {
		(self.0 >> GENERATION_SHIFT) as u32
	}
}

impl Debug for AtomId {
	fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
		match self.kind() {
			Some(kind) => formatter
				.debug_struct("AtomId")
				.field("kind", &kind)
				.field("slot", &self.slot())
				.field("generation", &self.generation())
				.finish(),
			None => formatter.write_str("AtomId::NULL"),
		}
	}
}

impl Display for AtomId {
	fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
		match self.kind() {
			Some(AtomKind::Node) => write!(formatter, "node:{}", self.slot()),
			Some(AtomKind::Edge) => write!(formatter, "edge:{}", self.slot()),
			None => formatter.write_str("null"),
		}
	}
}

#[derive(Debug, Clone, PartialOrd, PartialEq, Eq, Ord, Hash)]
#[non_exhaustive]
pub enum EdgeTopology {
	Directed {
		inbound: Vec<AtomId>,
		outbound: Vec<AtomId>,
	},
	Undirected {
		endpoints: Vec<AtomId>,
	},
}

impl EdgeTopology {
	pub fn directed(inbound: impl Into<Vec<AtomId>>, outbound: impl Into<Vec<AtomId>>) -> Self {
		Self::Directed {
			inbound: inbound.into(),
			outbound: outbound.into(),
		}
	}

	pub fn undirected(endpoints: impl Into<Vec<AtomId>>) -> Self {
		Self::Undirected {
			endpoints: endpoints.into(),
		}
	}

	pub fn endpoints(&self) -> impl Iterator<Item = AtomId> + '_ {
		match self {
			Self::Directed {
				inbound,
				outbound,
			} => itertools::Either::Left(inbound.iter().chain(outbound).copied()),
			Self::Undirected {
				endpoints,
			} => itertools::Either::Right(endpoints.iter().copied()),
		}
	}
}

#[derive(Debug, Default, Clone)]
struct GeneralStorage {
	node_labels: Vec<Option<Text>>,
	node_properties: Vec<PropertyMap>,
	edge_topologies: Vec<EdgeTopology>,
	edge_labels: Vec<Option<Text>>,
	edge_properties: Vec<PropertyMap>,
}

#[derive(Clone, Copy)]
pub struct NodeView<'a> {
	id: AtomId,
	label: Option<&'a Text>,
	properties: &'a PropertyMap,
}

impl<'a> NodeView<'a> {
	pub const fn id(self) -> AtomId {
		self.id
	}

	pub fn label(self) -> Option<&'a str> {
		self.label.map(Text::as_str)
	}

	pub const fn label_text(self) -> Option<&'a Text> {
		self.label
	}

	pub const fn properties(self) -> &'a PropertyMap {
		self.properties
	}
}

impl Debug for NodeView<'_> {
	fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
		formatter
			.debug_struct("NodeView")
			.field("id", &self.id)
			.field("label", &self.label)
			.field("properties", &self.properties)
			.finish()
	}
}

#[derive(Clone, Copy)]
pub struct EdgeView<'a> {
	id: AtomId,
	topology: &'a EdgeTopology,
	label: Option<&'a Text>,
	properties: &'a PropertyMap,
}

impl<'a> EdgeView<'a> {
	pub const fn id(self) -> AtomId {
		self.id
	}

	pub const fn topology(self) -> &'a EdgeTopology {
		self.topology
	}

	pub fn label(self) -> Option<&'a str> {
		self.label.map(Text::as_str)
	}

	pub const fn label_text(self) -> Option<&'a Text> {
		self.label
	}

	pub const fn properties(self) -> &'a PropertyMap {
		self.properties
	}
}

impl Debug for EdgeView<'_> {
	fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
		formatter
			.debug_struct("EdgeView")
			.field("id", &self.id)
			.field("topology", &self.topology)
			.field("label", &self.label)
			.field("properties", &self.properties)
			.finish()
	}
}

pub struct Graph<State = Frozen> {
	identity: Uuid,
	revision: u64,
	properties: PropertyMap,
	storage: GeneralStorage,
	state: PhantomData<State>,
}

pub type MutableGraph = Graph<Mutable>;
pub type FrozenGraph = Graph<Frozen>;

impl Graph<Mutable> {
	pub fn new() -> Self {
		Self {
			identity: Uuid::now_v7(),
			revision: 0,
			properties: PropertyMap::new(),
			storage: GeneralStorage::default(),
			state: PhantomData,
		}
	}

	pub fn with_capacity(nodes: usize, edges: usize) -> Self {
		let mut graph = Self::new();
		graph.storage.node_labels.reserve(nodes);
		graph.storage.node_properties.reserve(nodes);
		graph.storage.edge_topologies.reserve(edges);
		graph.storage.edge_labels.reserve(edges);
		graph.storage.edge_properties.reserve(edges);
		graph
	}

	pub fn properties_mut(&mut self) -> &mut PropertyMap {
		self.bump_revision();
		&mut self.properties
	}

	pub fn add_node(
		&mut self,
		label: Option<Text>,
		properties: PropertyMap,
	) -> FerrosGrynnResult<AtomId> {
		if self.storage.node_labels.len() as u64 > SLOT_MASK {
			return Err(FerrosGrynnError::graph_capacity_exceeded(AtomKind::Node.to_string()));
		}
		let slot = self.storage.node_labels.len() as u64;
		self.storage.node_labels.push(label);
		self.storage.node_properties.push(properties);
		self.bump_revision();
		Ok(AtomId::node(slot))
	}

	pub fn add_edge(
		&mut self,
		topology: EdgeTopology,
		label: Option<Text>,
		properties: PropertyMap,
	) -> FerrosGrynnResult<AtomId> {
		let endpoints = topology.endpoints().collect::<Vec<_>>();
		if endpoints.is_empty() {
			return Err(FerrosGrynnError::edge_creation_error()
				.with_cause(FerrosGrynnError::empty_edge_endpoints()));
		}
		if let Some(endpoint) = endpoints.into_iter().find(|endpoint| !self.contains(*endpoint)) {
			return Err(
				FerrosGrynnError::edge_creation_error().with_cause(atom_does_not_exist(endpoint))
			);
		}
		if self.storage.edge_topologies.len() as u64 > SLOT_MASK {
			return Err(FerrosGrynnError::graph_capacity_exceeded(AtomKind::Edge.to_string()));
		}

		let slot = self.storage.edge_topologies.len() as u64;
		self.storage.edge_topologies.push(topology);
		self.storage.edge_labels.push(label);
		self.storage.edge_properties.push(properties);
		self.bump_revision();
		Ok(AtomId::edge(slot))
	}

	pub fn node_properties_mut(&mut self, id: AtomId) -> FerrosGrynnResult<&mut PropertyMap> {
		if id.kind() != Some(AtomKind::Node) {
			return Err(atom_kind_mismatch(id, AtomKind::Node));
		}
		if id.generation() != 0 {
			return Err(atom_does_not_exist(id));
		}
		let index = id.slot() as usize;
		if index >= self.storage.node_properties.len() {
			return Err(atom_does_not_exist(id));
		}
		self.bump_revision();
		Ok(&mut self.storage.node_properties[index])
	}

	pub fn edge_properties_mut(&mut self, id: AtomId) -> FerrosGrynnResult<&mut PropertyMap> {
		if id.kind() != Some(AtomKind::Edge) {
			return Err(atom_kind_mismatch(id, AtomKind::Edge));
		}
		if id.generation() != 0 {
			return Err(atom_does_not_exist(id));
		}
		let index = id.slot() as usize;
		if index >= self.storage.edge_properties.len() {
			return Err(atom_does_not_exist(id));
		}
		self.bump_revision();
		Ok(&mut self.storage.edge_properties[index])
	}

	pub fn freeze(self) -> Graph<Frozen> {
		Graph {
			identity: self.identity,
			revision: self.revision,
			properties: self.properties,
			storage: self.storage,
			state: PhantomData,
		}
	}

	fn bump_revision(&mut self) {
		if let Some(revision) = self.revision.checked_add(1) {
			self.revision = revision;
		} else {
			self.identity = Uuid::now_v7();
			self.revision = 0;
		}
	}
}

fn atom_kind_name(kind: Option<AtomKind>) -> String {
	kind.map_or_else(|| "null".to_owned(), |kind| kind.to_string())
}

fn atom_does_not_exist(id: AtomId) -> FerrosGrynnError {
	FerrosGrynnError::atom_does_not_exist(atom_kind_name(id.kind()), id.to_string())
}

fn atom_kind_mismatch(id: AtomId, expected: AtomKind) -> FerrosGrynnError {
	FerrosGrynnError::atom_kind_mismatch(
		id.to_string(),
		atom_kind_name(id.kind()),
		expected.to_string(),
	)
}

impl Default for Graph<Mutable> {
	fn default() -> Self {
		Self::new()
	}
}

impl Graph<Frozen> {
	pub fn thaw(self) -> Graph<Mutable> {
		Graph {
			identity: self.identity,
			revision: self.revision,
			properties: self.properties,
			storage: self.storage,
			state: PhantomData,
		}
	}
}

impl Clone for Graph<Frozen> {
	fn clone(&self) -> Self {
		Self {
			identity: self.identity,
			revision: self.revision,
			properties: self.properties.clone(),
			storage: self.storage.clone(),
			state: PhantomData,
		}
	}
}

impl<State> Graph<State> {
	pub const fn identity(&self) -> Uuid {
		self.identity
	}

	pub const fn revision(&self) -> u64 {
		self.revision
	}

	pub const fn kind(&self) -> GraphKind {
		GraphKind::Graph
	}

	pub fn properties(&self) -> &PropertyMap {
		&self.properties
	}

	pub fn node_count(&self) -> usize {
		self.storage.node_labels.len()
	}

	pub fn edge_count(&self) -> usize {
		self.storage.edge_topologies.len()
	}

	pub fn contains(&self, id: AtomId) -> bool {
		if id.generation() != 0 {
			return false;
		}
		match id.kind() {
			Some(AtomKind::Node) => (id.slot() as usize) < self.storage.node_labels.len(),
			Some(AtomKind::Edge) => (id.slot() as usize) < self.storage.edge_topologies.len(),
			None => false,
		}
	}

	pub fn node(&self, id: AtomId) -> Option<NodeView<'_>> {
		if id.kind() != Some(AtomKind::Node) || id.generation() != 0 {
			return None;
		}
		let index = id.slot() as usize;
		Some(NodeView {
			id,
			label: self.storage.node_labels.get(index)?.as_ref(),
			properties: self.storage.node_properties.get(index)?,
		})
	}

	pub fn edge(&self, id: AtomId) -> Option<EdgeView<'_>> {
		if id.kind() != Some(AtomKind::Edge) || id.generation() != 0 {
			return None;
		}
		let index = id.slot() as usize;
		Some(EdgeView {
			id,
			topology: self.storage.edge_topologies.get(index)?,
			label: self.storage.edge_labels.get(index)?.as_ref(),
			properties: self.storage.edge_properties.get(index)?,
		})
	}

	pub fn nodes(&self) -> impl Iterator<Item = NodeView<'_>> {
		self.storage.node_labels.iter().zip(&self.storage.node_properties).enumerate().map(
			|(slot, (label, properties))| NodeView {
				id: AtomId::node(slot as u64),
				label: label.as_ref(),
				properties,
			},
		)
	}

	pub fn edges(&self) -> impl Iterator<Item = EdgeView<'_>> {
		self.storage
			.edge_topologies
			.iter()
			.zip(&self.storage.edge_labels)
			.zip(&self.storage.edge_properties)
			.enumerate()
			.map(|(slot, ((topology, label), properties))| EdgeView {
				id: AtomId::edge(slot as u64),
				topology,
				label: label.as_ref(),
				properties,
			})
	}
}

impl<State> Debug for Graph<State> {
	fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
		formatter
			.debug_struct("Graph")
			.field("identity", &self.identity)
			.field("revision", &self.revision)
			.field("nodes", &self.node_count())
			.field("edges", &self.edge_count())
			.finish()
	}
}

impl<State> PartialEq for Graph<State> {
	fn eq(&self, other: &Self) -> bool {
		self.identity == other.identity && self.revision == other.revision
	}
}

impl<State> Eq for Graph<State> {}

impl<State> PartialOrd for Graph<State> {
	fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
		Some(self.cmp(other))
	}
}

impl<State> Ord for Graph<State> {
	fn cmp(&self, other: &Self) -> Ordering {
		(self.identity, self.revision).cmp(&(other.identity, other.revision))
	}
}

impl<State> Hash for Graph<State> {
	fn hash<H: Hasher>(&self, state: &mut H) {
		self.identity.hash(state);
		self.revision.hash(state);
	}
}

#[cfg(test)]
mod tests {
	use super::{AtomId, AtomKind, EdgeTopology, Graph, Mutable};
	use crate::{gql_status::Domain, values::PropertyMap};

	#[test]
	fn atom_id_uses_u40_slots_and_u23_generations() {
		let slot = (1_u64 << 39) + 42;
		let generation = 7_u64;
		let node = AtomId::from_raw((generation << 41) | slot);
		let edge = AtomId::from_raw((generation << 41) | (1_u64 << 40) | slot);

		assert_eq!(node.kind(), Some(AtomKind::Node));
		assert_eq!(edge.kind(), Some(AtomKind::Edge));
		assert_eq!(node.slot(), slot);
		assert_eq!(edge.slot(), slot);
		assert_eq!(node.generation(), generation as u32);
		assert_eq!(edge.generation(), generation as u32);
	}

	#[test]
	fn supports_node_edge_and_edge_edge_incidence() {
		let mut graph = Graph::<Mutable>::new();
		let left = graph.add_node(Some("left".into()), PropertyMap::new()).unwrap();
		let right = graph.add_node(Some("right".into()), PropertyMap::new()).unwrap();
		let edge = graph
			.add_edge(EdgeTopology::directed(vec![left], vec![right]), None, PropertyMap::new())
			.unwrap();
		let meta_edge = graph
			.add_edge(EdgeTopology::directed(vec![edge], vec![right]), None, PropertyMap::new())
			.unwrap();

		assert_eq!(graph.node_count(), 2);
		assert_eq!(graph.edge_count(), 2);
		assert_eq!(graph.edge(meta_edge).unwrap().topology().endpoints().count(), 2,);
	}

	#[test]
	fn rejects_missing_endpoints_without_mutation() {
		let mut graph = Graph::<Mutable>::new();
		let unknown = super::AtomId::from_raw(42);
		let error = graph
			.add_edge(EdgeTopology::undirected(vec![unknown]), None, PropertyMap::new())
			.unwrap_err();
		assert_graph_error(&error, "FG-G2F02");
		assert_eq!(error.root().status().code().to_string(), "FG-G1F01");
		assert_eq!(graph.edge_count(), 0);

		let node = graph.add_node(None, PropertyMap::new()).unwrap();
		let stale_generation = super::AtomId::from_raw((1_u64 << 41) | node.raw());
		assert!(!graph.contains(stale_generation));
		let error = graph
			.add_edge(EdgeTopology::undirected(vec![stale_generation]), None, PropertyMap::new())
			.unwrap_err();
		assert_graph_error(&error, "FG-G2F02");
		assert_eq!(error.root().status().code().to_string(), "FG-G1F01");
	}

	#[test]
	fn reports_empty_edges_and_atom_kind_mismatches() {
		let mut graph = Graph::<Mutable>::new();
		let empty =
			graph.add_edge(EdgeTopology::undirected([]), None, PropertyMap::new()).unwrap_err();
		assert_graph_error(&empty, "FG-G2F02");
		assert_eq!(empty.root().status().code().to_string(), "FG-G2F01");

		let node = graph.add_node(None, PropertyMap::new()).unwrap();
		let mismatch = graph.edge_properties_mut(node).unwrap_err();
		assert_graph_error(&mismatch, "FG-G2F03");
	}

	#[test]
	fn frozen_identity_order_ignores_graph_content() {
		let graph = Graph::<Mutable>::new().freeze();
		let clone = graph.clone();
		assert_eq!(graph, clone);
		assert_eq!(graph.cmp(&clone), std::cmp::Ordering::Equal);
	}

	fn assert_graph_error(error: &crate::gql_status::FerrosGrynnError, code: &str) {
		assert_eq!(error.status().code().to_string(), code);
		assert_eq!(error.status().domain(), Domain::Graph);
	}
}
