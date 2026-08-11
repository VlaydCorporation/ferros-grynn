use std::{
	cmp::Ordering,
	collections::BTreeSet,
	fmt::{self, Debug, Formatter},
	hash::{Hash, Hasher},
	marker::PhantomData,
};

use super::{
	AtomId,
	AtomIds,
	AtomKind,
	DirtyTarget,
	EdgeEndpoint,
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
	general::{atom_does_not_exist, atom_kind_mismatch},
	storage::{EdgeColumns, GraphMetadata, NodeColumns, ensure_unique_record, generated_record_id},
};
use crate::{
	collections::cow_vec::CowVec,
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	values::runtime::{Number, PropertyMap, RecordId, Text, Uuid},
};

#[derive(Debug, Clone)]
struct HyperStorage {
	nodes: NodeColumns,
	edges: EdgeColumns,
	incidence: CowVec<Vec<AtomId>>,
}

impl HyperStorage {
	fn new() -> Self {
		Self {
			nodes: NodeColumns::new(),
			edges: EdgeColumns::new(),
			incidence: CowVec::new(),
		}
	}

	fn insert_node_new(
		&mut self,
		record_id: RecordId,
		label: Option<Text>,
		properties: PropertyMap,
	) -> FerrosGrynnResult<AtomId> {
		ensure_unique_record(&self.nodes, &self.edges, &record_id)?;
		let id = self.nodes.insert_new(record_id, label, properties)?;
		self.ensure_node_slot(id);
		Ok(id)
	}

	fn insert_node_at(
		&mut self,
		id: AtomId,
		record_id: RecordId,
		label: Option<Text>,
		properties: PropertyMap,
	) -> FerrosGrynnResult<()> {
		ensure_unique_record(&self.nodes, &self.edges, &record_id)?;
		self.nodes.insert_at(id, record_id, label, properties)?;
		self.ensure_node_slot(id);
		Ok(())
	}

	fn insert_edge_new(
		&mut self,
		topology: EdgeTopology,
		record_id: RecordId,
		label: Option<Text>,
		properties: PropertyMap,
		weight: Option<Number>,
	) -> FerrosGrynnResult<AtomId> {
		self.validate_topology(&topology)?;
		ensure_unique_record(&self.nodes, &self.edges, &record_id)?;
		let id = self.edges.insert_new(record_id, topology, label, properties, weight)?;
		self.register(id);
		Ok(id)
	}

	fn insert_edge_at(
		&mut self,
		id: AtomId,
		topology: EdgeTopology,
		record_id: RecordId,
		label: Option<Text>,
		properties: PropertyMap,
		weight: Option<Number>,
	) -> FerrosGrynnResult<()> {
		self.validate_topology(&topology)?;
		ensure_unique_record(&self.nodes, &self.edges, &record_id)?;
		self.edges.insert_at(id, record_id, topology, label, properties, weight)?;
		self.register(id);
		Ok(())
	}

	fn validate_topology(&self, topology: &EdgeTopology) -> FerrosGrynnResult<()> {
		if topology.endpoint_count() == 0 {
			return Err(FerrosGrynnError::edge_creation_error()
				.with_cause(FerrosGrynnError::empty_edge_endpoints()));
		}
		for group in [topology.source(), topology.target()] {
			let mut seen = BTreeSet::new();
			for endpoint in group {
				let EdgeEndpoint::Local(id) = endpoint else {
					return Err(constraint("cross-level incidence is forbidden"));
				};
				if id.kind() != Some(AtomKind::Node) {
					return Err(constraint("hyperedge endpoints must be vertices"));
				}
				if !self.nodes.contains(*id) {
					return Err(atom_does_not_exist(*id));
				}
				if !seen.insert(*id) {
					return Err(constraint("a hyperedge incidence role cannot repeat an endpoint"));
				}
			}
		}
		Ok(())
	}

	fn register(&mut self, edge: AtomId) {
		if let Some(topology) = self.edges.topology(edge) {
			for node in topology.endpoints().filter_map(EdgeEndpoint::local_id) {
				self.incidence[node.slot() as usize].push(edge);
			}
		}
	}

	fn remove_edge(&mut self, id: AtomId) -> bool {
		let Some(topology) = self.edges.topology(id).cloned() else {
			return false;
		};
		for node in topology.endpoints().filter_map(EdgeEndpoint::local_id) {
			self.incidence[node.slot() as usize].retain(|edge| *edge != id);
		}
		self.edges.remove(id)
	}

	fn remove_node(&mut self, id: AtomId) -> bool {
		if !self.nodes.contains(id) {
			return false;
		}
		for edge in self.incident_edges(id).to_vec() {
			self.remove_edge(edge);
		}
		self.incidence[id.slot() as usize].clear();
		self.nodes.remove(id)
	}

	fn incident_edges(&self, id: AtomId) -> &[AtomId] {
		if !self.nodes.contains(id) {
			return &[];
		}
		self.incidence.get(id.slot() as usize).map_or(&[], Vec::as_slice)
	}

	fn ensure_node_slot(&mut self, id: AtomId) {
		let slot = id.slot() as usize;
		while self.incidence.len() <= slot {
			self.incidence.push(Vec::new());
		}
	}
}

pub struct Hypergraph<State = Frozen> {
	metadata: GraphMetadata,
	storage: HyperStorage,
	state: PhantomData<State>,
}

pub type MutableHypergraph = Hypergraph<Mutable>;
pub type FrozenHypergraph = Hypergraph<Frozen>;

impl Hypergraph<Mutable> {
	pub fn new() -> Self {
		Self {
			metadata: GraphMetadata::new(),
			storage: HyperStorage::new(),
			state: PhantomData,
		}
	}

	pub fn add_node(
		&mut self,
		label: Option<Text>,
		properties: PropertyMap,
	) -> FerrosGrynnResult<AtomId> {
		self.metadata.ensure_can_bump()?;
		let id = self.storage.insert_node_new(
			generated_record_id(AtomKind::Node)?,
			label,
			properties,
		)?;
		self.metadata.bump(Some(DirtyTarget::Atom(id)))?;
		Ok(id)
	}

	pub fn add_edge(
		&mut self,
		topology: EdgeTopology,
		label: Option<Text>,
		properties: PropertyMap,
	) -> FerrosGrynnResult<AtomId> {
		self.metadata.ensure_can_bump()?;
		let id = self.storage.insert_edge_new(
			topology,
			generated_record_id(AtomKind::Edge)?,
			label,
			properties,
			None,
		)?;
		self.metadata.bump(Some(DirtyTarget::Atom(id)))?;
		Ok(id)
	}

	pub fn freeze(self) -> Hypergraph<Frozen> {
		Hypergraph {
			metadata: self.metadata.into_frozen(),
			storage: self.storage,
			state: PhantomData,
		}
	}

	pub fn flush<GWB: GraphWriteBack<Self>>(&mut self, writer: &mut GWB) -> FerrosGrynnResult<()> {
		let origin = self
			.metadata
			.origin()
			.cloned()
			.ok_or_else(FerrosGrynnError::graph_write_back_unavailable)?;
		let dirty = self.metadata.dirty().cloned().collect::<Vec<_>>();
		writer.flush(self, &origin, &dirty)?;
		self.metadata.clear_dirty();
		Ok(())
	}

	pub(crate) fn transaction_dirty(&self) -> Vec<DirtyTarget> {
		self.metadata.dirty().cloned().collect()
	}

	pub(crate) fn transaction_snapshot(&self) -> Hypergraph<Frozen> {
		Hypergraph {
			metadata: self.metadata.clone().into_frozen(),
			storage: self.storage.clone(),
			state: PhantomData,
		}
	}
}

impl Default for Hypergraph<Mutable> {
	fn default() -> Self {
		Self::new()
	}
}

impl Hypergraph<Frozen> {
	pub fn thaw(self) -> Hypergraph<Mutable> {
		Hypergraph {
			metadata: self.metadata,
			storage: self.storage,
			state: PhantomData,
		}
	}
}

impl Clone for Hypergraph<Frozen> {
	fn clone(&self) -> Self {
		Self {
			metadata: self.metadata.clone(),
			storage: self.storage.clone(),
			state: PhantomData,
		}
	}
}

impl<State> Hypergraph<State> {
	pub const fn identity(&self) -> Uuid {
		self.metadata.identity()
	}

	pub const fn revision(&self) -> u64 {
		self.metadata.revision()
	}

	pub const fn properties(&self) -> &PropertyMap {
		self.metadata.properties()
	}

	pub const fn node_count(&self) -> usize {
		self.storage.nodes.len()
	}

	pub const fn edge_count(&self) -> usize {
		self.storage.edges.len()
	}

	pub fn node(&self, id: AtomId) -> Option<NodeView<'_>> {
		<Self as GraphRead>::node(self, id)
	}

	pub fn edge(&self, id: AtomId) -> Option<EdgeView<'_>> {
		<Self as GraphRead>::edge(self, id)
	}

	pub fn incident_edges(&self, id: AtomId) -> &[AtomId] {
		self.storage.incident_edges(id)
	}

	pub fn into_graph(self) -> FerrosGrynnResult<Graph<State>> {
		let converted = Graph::<Mutable>::from_read_preserving(&self)?;
		let (_, storage) = converted.into_raw();
		Ok(Graph::from_raw(self.metadata, storage))
	}
}

impl<State> GraphRead for Hypergraph<State> {
	type EdgeIds<'a>
		= AtomIds<'a>
	where
		Self: 'a;
	type NodeIds<'a>
		= AtomIds<'a>
	where
		Self: 'a;

	fn identity(&self) -> Uuid {
		self.identity()
	}

	fn revision(&self) -> u64 {
		self.revision()
	}

	fn kind(&self) -> GraphKind {
		GraphKind::Hypergraph
	}

	fn properties(&self) -> &PropertyMap {
		self.properties()
	}

	fn node_ids(&self) -> Self::NodeIds<'_> {
		AtomIds::new(self.storage.nodes.ids().iter())
	}

	fn edge_ids(&self) -> Self::EdgeIds<'_> {
		AtomIds::new(self.storage.edges.ids().iter())
	}

	fn node(&self, id: AtomId) -> Option<NodeView<'_>> {
		Some(NodeView::new(
			id,
			self.storage.nodes.record_id(id)?,
			self.storage.nodes.label(id)?,
			self.storage.nodes.properties(id)?,
			None,
		))
	}

	fn edge(&self, id: AtomId) -> Option<EdgeView<'_>> {
		Some(EdgeView::new(
			id,
			self.storage.edges.record_id(id)?,
			self.storage.edges.topology(id)?,
			self.storage.edges.label(id)?,
			self.storage.edges.properties(id)?,
			self.storage.edges.weight(id)?,
			None,
		))
	}

	fn storage_mode(&self) -> super::GraphStorageMode {
		self.metadata.storage_mode()
	}

	fn graph_ref(&self) -> Option<&super::GraphRef> {
		self.metadata.graph_ref()
	}
}

impl GraphMut for Hypergraph<Mutable> {
	fn properties_mut(&mut self) -> FerrosGrynnResult<&mut PropertyMap> {
		self.metadata.properties_mut()
	}

	fn node_properties_mut(&mut self, id: AtomId) -> FerrosGrynnResult<&mut PropertyMap> {
		if id.kind() != Some(AtomKind::Node) {
			return Err(atom_kind_mismatch(id, AtomKind::Node));
		}
		if !self.storage.nodes.contains(id) {
			return Err(atom_does_not_exist(id));
		}
		self.metadata.bump(Some(DirtyTarget::Atom(id)))?;
		self.storage.nodes.properties_mut(id).ok_or_else(|| atom_does_not_exist(id))
	}

	fn edge_properties_mut(&mut self, id: AtomId) -> FerrosGrynnResult<&mut PropertyMap> {
		if id.kind() != Some(AtomKind::Edge) {
			return Err(atom_kind_mismatch(id, AtomKind::Edge));
		}
		if !self.storage.edges.contains(id) {
			return Err(atom_does_not_exist(id));
		}
		self.metadata.bump(Some(DirtyTarget::Atom(id)))?;
		self.storage.edges.properties_mut(id).ok_or_else(|| atom_does_not_exist(id))
	}

	fn remove_atom(&mut self, id: AtomId) -> FerrosGrynnResult<()> {
		self.metadata.ensure_can_bump()?;
		let dependent_edges = if id.is_node() {
			self.storage.incident_edges(id).to_vec()
		} else {
			Vec::new()
		};
		let removed = match id.kind() {
			Some(AtomKind::Node) => self.storage.remove_node(id),
			Some(AtomKind::Edge) => self.storage.remove_edge(id),
			None => false,
		};
		if !removed {
			return Err(atom_does_not_exist(id));
		}
		for edge in dependent_edges {
			self.metadata.mark_dirty(DirtyTarget::Atom(edge));
		}
		self.metadata.bump(Some(DirtyTarget::Atom(id)))
	}
}

impl<State> TryFrom<Graph<State>> for Hypergraph<State> {
	type Error = FerrosGrynnError;

	fn try_from(value: Graph<State>) -> Result<Self, Self::Error> {
		let storage = build_storage(&value)?;
		let (metadata, _) = value.into_raw();
		Ok(Self {
			metadata,
			storage,
			state: PhantomData,
		})
	}
}

impl QueryMaterialize for Hypergraph<Mutable> {
	fn query_from<R: GraphRead>(source: &R) -> FerrosGrynnResult<Self> {
		Self::try_from(Graph::<Mutable>::query_from(source)?)
	}
}

impl QueryMaterialize for Hypergraph<Frozen> {
	fn query_from<R: GraphRead>(source: &R) -> FerrosGrynnResult<Self> {
		Hypergraph::<Mutable>::query_from(source).map(Hypergraph::freeze)
	}
}

impl<State> Debug for Hypergraph<State> {
	fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
		formatter
			.debug_struct("Hypergraph")
			.field("identity", &self.identity())
			.field("revision", &self.revision())
			.field("nodes", &self.node_count())
			.field("edges", &self.edge_count())
			.finish()
	}
}

impl<State> PartialEq for Hypergraph<State> {
	fn eq(&self, other: &Self) -> bool {
		(self.identity(), self.revision()) == (other.identity(), other.revision())
	}
}

impl<State> Eq for Hypergraph<State> {}

impl<State> PartialOrd for Hypergraph<State> {
	fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
		Some(self.cmp(other))
	}
}

impl<State> Ord for Hypergraph<State> {
	fn cmp(&self, other: &Self) -> Ordering {
		(self.identity(), self.revision()).cmp(&(other.identity(), other.revision()))
	}
}

impl<State> Hash for Hypergraph<State> {
	fn hash<H: Hasher>(&self, state: &mut H) {
		self.identity().hash(state);
		self.revision().hash(state);
	}
}

fn build_storage<R: GraphRead>(source: &R) -> FerrosGrynnResult<HyperStorage> {
	let mut storage = HyperStorage::new();
	for id in source.node_ids() {
		let node = source.node(id).ok_or_else(|| atom_does_not_exist(id))?;
		if node.is_meta() {
			return Err(constraint("hypergraph vertices cannot contain nested graphs"));
		}
		storage.insert_node_at(
			id,
			node.record_id().clone(),
			node.label_text().cloned(),
			node.properties().clone(),
		)?;
	}
	for id in source.edge_ids() {
		let edge = source.edge(id).ok_or_else(|| atom_does_not_exist(id))?;
		if edge.is_meta() {
			return Err(constraint("hypergraph edges cannot contain transition graphs"));
		}
		storage.insert_edge_at(
			id,
			edge.topology().clone(),
			edge.record_id().clone(),
			edge.label_text().cloned(),
			edge.properties().clone(),
			edge.weight(),
		)?;
	}
	Ok(storage)
}

fn constraint(reason: impl Into<String>) -> FerrosGrynnError {
	FerrosGrynnError::graph_constraint_violation("hypergraph", reason.into())
}

#[cfg(test)]
mod tests {
	use super::Hypergraph;
	use crate::values::{
		PropertyMap,
		runtime::graph::{DirtyTarget, EdgeTopology, Graph, GraphMut, Mutable},
	};

	#[test]
	fn stores_vertex_hyperedge_incidence() {
		let mut graph = Hypergraph::<Mutable>::new();
		let first = graph.add_node(None, PropertyMap::new()).unwrap();
		let second = graph.add_node(None, PropertyMap::new()).unwrap();
		let third = graph.add_node(None, PropertyMap::new()).unwrap();
		let edge = graph
			.add_edge(EdgeTopology::directed([first, second], [third]), None, PropertyMap::new())
			.unwrap();

		assert_eq!(graph.incident_edges(first), [edge]);
		assert_eq!(graph.incident_edges(third), [edge]);
	}

	#[test]
	fn permits_a_vertex_in_both_directed_incidence_roles() {
		let mut graph = Hypergraph::<Mutable>::new();
		let node = graph.add_node(None, PropertyMap::new()).unwrap();
		let edge = graph
			.add_edge(EdgeTopology::directed([node], [node]), None, PropertyMap::new())
			.unwrap();
		assert_eq!(graph.incident_edges(node), &[edge, edge]);
	}

	#[test]
	fn graph_conversion_rejects_edge_endpoints() {
		let mut graph = Graph::<Mutable>::new();
		let left = graph.add_node(None, PropertyMap::new()).unwrap();
		let right = graph.add_node(None, PropertyMap::new()).unwrap();
		let edge = graph
			.add_edge(EdgeTopology::directed([left], [right]), None, PropertyMap::new())
			.unwrap();
		graph.add_edge(EdgeTopology::directed([edge], [right]), None, PropertyMap::new()).unwrap();

		let error = Hypergraph::try_from(graph).unwrap_err();
		assert_eq!(error.status().code().to_string(), "FG-G2F04");
	}

	#[test]
	fn vertex_removal_marks_its_hyperedge_dirty_once() {
		let mut graph = Hypergraph::<Mutable>::new();
		let removed = graph.add_node(None, PropertyMap::new()).unwrap();
		let peer = graph.add_node(None, PropertyMap::new()).unwrap();
		let edge = graph
			.add_edge(EdgeTopology::directed([removed], [removed, peer]), None, PropertyMap::new())
			.unwrap();
		graph.metadata.clear_dirty();

		graph.remove_atom(removed).unwrap();

		let dirty = graph.metadata.dirty().cloned().collect::<Vec<_>>();
		assert_eq!(dirty.len(), 2);
		assert!(dirty.contains(&DirtyTarget::Atom(removed)));
		assert!(dirty.contains(&DirtyTarget::Atom(edge)));
	}
}
