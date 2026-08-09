use std::{
	cmp::Ordering,
	collections::{BTreeMap, BTreeSet},
	fmt::{self, Debug, Formatter},
	hash::{Hash, Hasher},
	marker::PhantomData,
};

use rapidhash::RapidHashMap;
use smallvec::SmallVec;

use super::{
	AtomId,
	AtomKind,
	DirtyTarget,
	EdgeEndpoint,
	EdgeTopology,
	EdgeView,
	Frozen,
	GraphKind,
	GraphMut,
	GraphRead,
	GraphReadView,
	GraphStorageMode,
	GraphWriteBack,
	MaterializationOrigin,
	Mutable,
	NodeView,
	QualifiedAtomId,
	QueryMaterialize,
	SnapshotId,
	storage::{EdgeColumns, GraphMetadata, NodeColumns, ensure_unique_record, generated_record_id},
};
use crate::{
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	values::runtime::{Number, PropertyMap, RecordId, Text, Uuid},
};

const INLINE_INCIDENCE: usize = 4;

#[derive(Debug, Clone)]
pub(super) struct GeneralStorage {
	pub(super) nodes: NodeColumns,
	pub(super) edges: EdgeColumns,
	node_inner: Vec<Option<Box<Graph<Frozen>>>>,
	edge_transition: Vec<Option<Box<Graph<Frozen>>>>,
	incidence: RapidHashMap<EdgeEndpoint, SmallVec<AtomId, INLINE_INCIDENCE>>,
}

impl GeneralStorage {
	fn new() -> Self {
		Self {
			nodes: NodeColumns::new(),
			edges: EdgeColumns::new(),
			node_inner: Vec::new(),
			edge_transition: Vec::new(),
			incidence: RapidHashMap::default(),
		}
	}

	fn with_capacity(nodes: usize, edges: usize) -> Self {
		Self {
			nodes: NodeColumns::with_capacity(nodes),
			edges: EdgeColumns::with_capacity(edges),
			node_inner: Vec::with_capacity(nodes),
			edge_transition: Vec::with_capacity(edges),
			incidence: RapidHashMap::default(),
		}
	}

	fn insert_node_new(
		&mut self,
		record_id: RecordId,
		label: Option<Text>,
		properties: PropertyMap,
		inner: Option<Graph<Frozen>>,
	) -> FerrosGrynnResult<AtomId> {
		let id = self.nodes.insert_new(record_id, label, properties)?;
		self.ensure_node_slot(id.slot() as usize);
		self.node_inner[id.slot() as usize] = inner.map(Box::new);
		Ok(id)
	}

	fn insert_node_at(
		&mut self,
		id: AtomId,
		record_id: RecordId,
		label: Option<Text>,
		properties: PropertyMap,
		inner: Option<Graph<Frozen>>,
	) -> FerrosGrynnResult<()> {
		self.nodes.insert_at(id, record_id, label, properties)?;
		self.ensure_node_slot(id.slot() as usize);
		self.node_inner[id.slot() as usize] = inner.map(Box::new);
		Ok(())
	}

	fn insert_edge_new(
		&mut self,
		record_id: RecordId,
		topology: EdgeTopology,
		label: Option<Text>,
		properties: PropertyMap,
		weight: Option<Number>,
		transition: Option<Graph<Frozen>>,
	) -> FerrosGrynnResult<AtomId> {
		let id = self.edges.insert_new(record_id, topology.clone(), label, properties, weight)?;
		self.ensure_edge_slot(id.slot() as usize);
		self.edge_transition[id.slot() as usize] = transition.map(Box::new);
		self.register_incidence(id, &topology);
		Ok(id)
	}

	fn insert_edge_at(
		&mut self,
		id: AtomId,
		record_id: RecordId,
		topology: EdgeTopology,
		label: Option<Text>,
		properties: PropertyMap,
		weight: Option<Number>,
		transition: Option<Graph<Frozen>>,
	) -> FerrosGrynnResult<()> {
		self.edges.insert_at(id, record_id, topology.clone(), label, properties, weight)?;
		self.ensure_edge_slot(id.slot() as usize);
		self.edge_transition[id.slot() as usize] = transition.map(Box::new);
		self.register_incidence(id, &topology);
		Ok(())
	}

	fn remove_edge_raw(&mut self, id: AtomId) {
		let Some(topology) = self.edges.topology(id).cloned() else {
			return;
		};
		self.unregister_incidence(id, &topology);
		self.edges.remove(id);
		if let Some(transition) = self.edge_transition.get_mut(id.slot() as usize) {
			*transition = None;
		}
	}

	fn node_inner(&self, id: AtomId) -> Option<&Graph<Frozen>> {
		self.nodes.contains(id).then(|| self.node_inner.get(id.slot() as usize)?.as_deref())?
	}

	fn edge_transition(&self, id: AtomId) -> Option<&Graph<Frozen>> {
		self.edges.contains(id).then(|| self.edge_transition.get(id.slot() as usize)?.as_deref())?
	}

	fn validate_topology(&self, topology: &EdgeTopology) -> FerrosGrynnResult<()> {
		if topology.endpoint_count() == 0 {
			return Err(FerrosGrynnError::edge_creation_error()
				.with_cause(FerrosGrynnError::empty_edge_endpoints()));
		}

		for group in [topology.source(), topology.target()] {
			let mut seen = BTreeSet::new();
			for endpoint in group {
				if !seen.insert(endpoint) {
					return Err(FerrosGrynnError::edge_creation_error().with_cause(
						FerrosGrynnError::graph_constraint_violation(
							"graph",
							"an incidence role cannot repeat the same endpoint",
						),
					));
				}
				if !self.contains_endpoint(endpoint) {
					return Err(FerrosGrynnError::edge_creation_error().with_cause(
						FerrosGrynnError::atom_does_not_exist(
							endpoint_kind(endpoint),
							format!("{endpoint:?}"),
						),
					));
				}
			}
		}
		Ok(())
	}

	fn contains_endpoint(&self, endpoint: &EdgeEndpoint) -> bool {
		match endpoint {
			EdgeEndpoint::Local(id) => self.contains(*id),
			EdgeEndpoint::Nested(qualified) => self.contains_qualified(qualified),
		}
	}

	fn contains_qualified(&self, qualified: &QualifiedAtomId) -> bool {
		let mut scope = qualified.scope().iter().copied();
		let Some(first) = scope.next() else {
			return false;
		};
		let Some(mut graph) = self.node_inner(first) else {
			return false;
		};
		for container in scope {
			let Some(next) = graph.node(container).and_then(NodeView::inner_graph) else {
				return false;
			};
			graph = next;
		}
		graph.contains(qualified.atom())
	}

	fn contains(&self, id: AtomId) -> bool {
		match id.kind() {
			Some(AtomKind::Node) => self.nodes.contains(id),
			Some(AtomKind::Edge) => self.edges.contains(id),
			None => false,
		}
	}

	fn incident_edges(&self, endpoint: &EdgeEndpoint) -> &[AtomId] {
		self.incidence.get(endpoint).map_or(&[], SmallVec::as_slice)
	}

	fn register_incidence(&mut self, id: AtomId, topology: &EdgeTopology) {
		for endpoint in topology.endpoints().cloned() {
			self.incidence.entry(endpoint).or_default().push(id);
		}
	}

	fn unregister_incidence(&mut self, id: AtomId, topology: &EdgeTopology) {
		for endpoint in topology.endpoints() {
			let remove_entry = if let Some(edges) = self.incidence.get_mut(endpoint) {
				if let Some(index) = edges.iter().position(|candidate| *candidate == id) {
					edges.swap_remove(index);
				}
				edges.is_empty()
			} else {
				false
			};
			if remove_entry {
				self.incidence.remove(endpoint);
			}
		}
	}

	fn collect_dependent_edges(&self, id: AtomId) -> BTreeSet<AtomId> {
		let mut pending = Vec::new();
		let mut edges = BTreeSet::new();
		if id.is_edge() {
			pending.push(id);
		}
		pending.extend(self.incident_edges(&EdgeEndpoint::Local(id)).iter().copied());
		if id.is_node() {
			for (endpoint, incident) in &self.incidence {
				if matches!(endpoint, EdgeEndpoint::Nested(value) if value.scope().first() == Some(&id))
				{
					pending.extend(incident.iter().copied());
				}
			}
		}

		while let Some(edge) = pending.pop() {
			if !edges.insert(edge) {
				continue;
			}
			pending.extend(self.incident_edges(&EdgeEndpoint::Local(edge)).iter().copied());
		}
		edges
	}

	fn ensure_node_slot(&mut self, index: usize) {
		while self.node_inner.len() <= index {
			self.node_inner.push(None);
		}
	}

	fn ensure_edge_slot(&mut self, index: usize) {
		while self.edge_transition.len() <= index {
			self.edge_transition.push(None);
		}
	}
}

impl Default for GeneralStorage {
	fn default() -> Self {
		Self::new()
	}
}

pub struct Graph<State = Frozen> {
	pub(super) metadata: GraphMetadata,
	pub(super) storage: GeneralStorage,
	state: PhantomData<State>,
}

pub type MutableGraph = Graph<Mutable>;
pub type FrozenGraph = Graph<Frozen>;

impl Graph<Mutable> {
	pub fn new() -> Self {
		Self {
			metadata: GraphMetadata::new(),
			storage: GeneralStorage::new(),
			state: PhantomData,
		}
	}

	pub fn with_capacity(nodes: usize, edges: usize) -> Self {
		Self {
			metadata: GraphMetadata::new(),
			storage: GeneralStorage::with_capacity(nodes, edges),
			state: PhantomData,
		}
	}

	pub fn add_node(
		&mut self,
		label: Option<Text>,
		properties: PropertyMap,
	) -> FerrosGrynnResult<AtomId> {
		self.add_node_with_id(generated_record_id(AtomKind::Node)?, label, properties)
	}

	pub fn add_node_with_id(
		&mut self,
		record_id: RecordId,
		label: Option<Text>,
		properties: PropertyMap,
	) -> FerrosGrynnResult<AtomId> {
		self.add_node_raw(record_id, label, properties, None)
	}

	pub fn add_metavertex(
		&mut self,
		inner: Graph<Frozen>,
		label: Option<Text>,
		properties: PropertyMap,
	) -> FerrosGrynnResult<AtomId> {
		self.add_node_raw(generated_record_id(AtomKind::Node)?, label, properties, Some(inner))
	}

	pub fn add_edge(
		&mut self,
		topology: EdgeTopology,
		label: Option<Text>,
		properties: PropertyMap,
	) -> FerrosGrynnResult<AtomId> {
		self.add_edge_with_id(generated_record_id(AtomKind::Edge)?, topology, label, properties)
	}

	pub fn add_edge_with_id(
		&mut self,
		record_id: RecordId,
		topology: EdgeTopology,
		label: Option<Text>,
		properties: PropertyMap,
	) -> FerrosGrynnResult<AtomId> {
		self.add_edge_raw(record_id, topology, label, properties, None, None)
	}

	pub fn add_metaedge(
		&mut self,
		topology: EdgeTopology,
		transition: Graph<Frozen>,
		label: Option<Text>,
		properties: PropertyMap,
	) -> FerrosGrynnResult<AtomId> {
		self.add_edge_raw(
			generated_record_id(AtomKind::Edge)?,
			topology,
			label,
			properties,
			None,
			Some(transition),
		)
	}

	pub fn set_node_label(&mut self, id: AtomId, label: Option<Text>) -> FerrosGrynnResult<()> {
		self.check_node(id)?;
		self.metadata.ensure_can_bump()?;
		if let Some(slot) = self.storage.nodes.label_mut(id) {
			*slot = label;
		}
		self.metadata.bump(Some(DirtyTarget::Atom(id)))
	}

	pub fn set_edge_label(&mut self, id: AtomId, label: Option<Text>) -> FerrosGrynnResult<()> {
		self.check_edge(id)?;
		self.metadata.ensure_can_bump()?;
		if let Some(slot) = self.storage.edges.label_mut(id) {
			*slot = label;
		}
		self.metadata.bump(Some(DirtyTarget::Atom(id)))
	}

	pub fn set_edge_weight(&mut self, id: AtomId, weight: Option<Number>) -> FerrosGrynnResult<()> {
		self.check_edge(id)?;
		self.metadata.ensure_can_bump()?;
		if let Some(slot) = self.storage.edges.weight_mut(id) {
			*slot = weight.filter(|value| !value.is_nan());
		}
		self.metadata.bump(Some(DirtyTarget::Atom(id)))
	}

	pub fn replace_inner_graph(
		&mut self,
		id: AtomId,
		inner: Option<Graph<Frozen>>,
	) -> FerrosGrynnResult<Option<Graph<Frozen>>> {
		self.check_node(id)?;
		self.metadata.ensure_can_bump()?;
		let slot = &mut self.storage.node_inner[id.slot() as usize];
		let previous = std::mem::replace(slot, inner.map(Box::new)).map(|graph| *graph);
		self.metadata.bump(Some(DirtyTarget::Atom(id)))?;
		Ok(previous)
	}

	pub fn replace_transition_graph(
		&mut self,
		id: AtomId,
		transition: Option<Graph<Frozen>>,
	) -> FerrosGrynnResult<Option<Graph<Frozen>>> {
		self.check_edge(id)?;
		self.metadata.ensure_can_bump()?;
		let slot = &mut self.storage.edge_transition[id.slot() as usize];
		let previous = std::mem::replace(slot, transition.map(Box::new)).map(|graph| *graph);
		self.metadata.bump(Some(DirtyTarget::Atom(id)))?;
		Ok(previous)
	}

	pub fn remove_atom(&mut self, id: AtomId) -> FerrosGrynnResult<()> {
		if !self.storage.contains(id) {
			return Err(atom_does_not_exist(id));
		}
		self.metadata.ensure_can_bump()?;
		let dependent_edges = self.storage.collect_dependent_edges(id);
		for edge in &dependent_edges {
			self.storage.remove_edge_raw(*edge);
		}
		if id.is_node() {
			self.storage.nodes.remove(id);
			self.storage.node_inner[id.slot() as usize] = None;
		}
		for edge in dependent_edges {
			self.metadata.mark_dirty(DirtyTarget::Atom(edge));
		}
		self.metadata.bump(Some(DirtyTarget::Atom(id)))
	}

	pub fn freeze(self) -> Graph<Frozen> {
		Graph {
			metadata: self.metadata.into_frozen(),
			storage: self.storage,
			state: PhantomData,
		}
	}

	pub fn flush(&mut self, writer: &mut impl GraphWriteBack<Self>) -> FerrosGrynnResult<()> {
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

	pub(crate) fn adopt_external_revision(&mut self, revision: u64) {
		self.metadata.adopt_external_revision(revision);
	}

	pub(crate) fn clone_preserving_state(&self) -> Self {
		Self {
			metadata: self.metadata.clone(),
			storage: self.storage.clone(),
			state: PhantomData,
		}
	}

	pub(crate) fn writeback_context(
		&self,
	) -> FerrosGrynnResult<(MaterializationOrigin, Vec<DirtyTarget>)> {
		let origin = self
			.metadata
			.origin()
			.cloned()
			.ok_or_else(FerrosGrynnError::graph_write_back_unavailable)?;
		Ok((origin, self.metadata.dirty().cloned().collect()))
	}

	pub(crate) fn finish_writeback(&mut self) {
		self.metadata.clear_dirty();
	}

	pub(crate) fn from_read_preserving<R: GraphRead>(source: &R) -> FerrosGrynnResult<Self> {
		copy_from_read(source, true)
	}

	fn add_node_raw(
		&mut self,
		record_id: RecordId,
		label: Option<Text>,
		properties: PropertyMap,
		inner: Option<Graph<Frozen>>,
	) -> FerrosGrynnResult<AtomId> {
		ensure_unique_record(&self.storage.nodes, &self.storage.edges, &record_id)?;
		self.metadata.ensure_can_bump()?;
		let id = self.storage.insert_node_new(record_id, label, properties, inner)?;
		self.metadata.bump(Some(DirtyTarget::Atom(id)))?;
		Ok(id)
	}

	fn add_edge_raw(
		&mut self,
		record_id: RecordId,
		topology: EdgeTopology,
		label: Option<Text>,
		properties: PropertyMap,
		weight: Option<Number>,
		transition: Option<Graph<Frozen>>,
	) -> FerrosGrynnResult<AtomId> {
		ensure_unique_record(&self.storage.nodes, &self.storage.edges, &record_id)?;
		self.storage.validate_topology(&topology)?;
		self.metadata.ensure_can_bump()?;
		let id = self
			.storage
			.insert_edge_new(record_id, topology, label, properties, weight, transition)?;
		self.metadata.bump(Some(DirtyTarget::Atom(id)))?;
		Ok(id)
	}
}

impl Default for Graph<Mutable> {
	fn default() -> Self {
		Self::new()
	}
}

impl Graph<Frozen> {
	pub fn thaw(self) -> Graph<Mutable> {
		Graph {
			metadata: self.metadata,
			storage: self.storage,
			state: PhantomData,
		}
	}
}

impl Clone for Graph<Frozen> {
	fn clone(&self) -> Self {
		Self {
			metadata: self.metadata.clone(),
			storage: self.storage.clone(),
			state: PhantomData,
		}
	}
}

impl<State> Graph<State> {
	pub const fn identity(&self) -> Uuid {
		self.metadata.identity()
	}

	pub const fn revision(&self) -> u64 {
		self.metadata.revision()
	}

	pub const fn kind(&self) -> GraphKind {
		GraphKind::Graph
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

	pub fn contains(&self, id: AtomId) -> bool {
		self.storage.contains(id)
	}

	pub fn node(&self, id: AtomId) -> Option<NodeView<'_>> {
		<Self as GraphRead>::node(self, id)
	}

	pub fn edge(&self, id: AtomId) -> Option<EdgeView<'_>> {
		<Self as GraphRead>::edge(self, id)
	}

	pub fn nodes(&self) -> impl Iterator<Item = NodeView<'_>> {
		self.storage.nodes.ids().iter().filter_map(|id| self.node(*id))
	}

	pub fn edges(&self) -> impl Iterator<Item = EdgeView<'_>> {
		self.storage.edges.ids().iter().filter_map(|id| self.edge(*id))
	}

	pub fn incident_edges(
		&self,
		endpoint: AtomId,
	) -> std::iter::Copied<std::slice::Iter<'_, AtomId>> {
		self.incident_edges_at(&EdgeEndpoint::Local(endpoint))
	}

	pub fn incident_edges_at(
		&self,
		endpoint: &EdgeEndpoint,
	) -> std::iter::Copied<std::slice::Iter<'_, AtomId>> {
		self.storage.incident_edges(endpoint).iter().copied()
	}

	pub const fn read_view(&self, snapshot: SnapshotId) -> GraphReadView<'_, Self> {
		GraphReadView::new(self, snapshot)
	}

	pub const fn materialization_origin(&self) -> Option<&MaterializationOrigin> {
		self.metadata.origin()
	}

	pub(super) fn into_raw(self) -> (GraphMetadata, GeneralStorage) {
		(self.metadata, self.storage)
	}

	pub(super) const fn from_raw(metadata: GraphMetadata, storage: GeneralStorage) -> Self {
		Self {
			metadata,
			storage,
			state: PhantomData,
		}
	}

	fn check_node(&self, id: AtomId) -> FerrosGrynnResult<()> {
		if id.kind() != Some(AtomKind::Node) {
			return Err(atom_kind_mismatch(id, AtomKind::Node));
		}
		self.storage.nodes.contains(id).then_some(()).ok_or_else(|| atom_does_not_exist(id))
	}

	fn check_edge(&self, id: AtomId) -> FerrosGrynnResult<()> {
		if id.kind() != Some(AtomKind::Edge) {
			return Err(atom_kind_mismatch(id, AtomKind::Edge));
		}
		self.storage.edges.contains(id).then_some(()).ok_or_else(|| atom_does_not_exist(id))
	}
}

impl<State> GraphRead for Graph<State> {
	type NodeIds<'a>
		= std::iter::Copied<std::slice::Iter<'a, AtomId>>
	where
		Self: 'a;
	type EdgeIds<'a>
		= std::iter::Copied<std::slice::Iter<'a, AtomId>>
	where
		Self: 'a;

	fn identity(&self) -> Uuid {
		self.identity()
	}

	fn revision(&self) -> u64 {
		self.revision()
	}

	fn kind(&self) -> GraphKind {
		GraphKind::Graph
	}

	fn properties(&self) -> &PropertyMap {
		self.properties()
	}

	fn node_ids(&self) -> Self::NodeIds<'_> {
		self.storage.nodes.ids().iter().copied()
	}

	fn edge_ids(&self) -> Self::EdgeIds<'_> {
		self.storage.edges.ids().iter().copied()
	}

	fn node(&self, id: AtomId) -> Option<NodeView<'_>> {
		Some(NodeView::new(
			id,
			self.storage.nodes.record_id(id)?,
			self.storage.nodes.label(id)?,
			self.storage.nodes.properties(id)?,
			self.storage.node_inner(id),
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
			self.storage.edge_transition(id),
		))
	}

	fn storage_mode(&self) -> GraphStorageMode {
		self.metadata.storage_mode()
	}

	fn graph_ref(&self) -> Option<&super::GraphRef> {
		self.metadata.graph_ref()
	}
}

impl GraphMut for Graph<Mutable> {
	fn properties_mut(&mut self) -> FerrosGrynnResult<&mut PropertyMap> {
		self.metadata.properties_mut()
	}

	fn node_properties_mut(&mut self, id: AtomId) -> FerrosGrynnResult<&mut PropertyMap> {
		self.check_node(id)?;
		self.metadata.bump(Some(DirtyTarget::Atom(id)))?;
		self.storage.nodes.properties_mut(id).ok_or_else(|| atom_does_not_exist(id))
	}

	fn edge_properties_mut(&mut self, id: AtomId) -> FerrosGrynnResult<&mut PropertyMap> {
		self.check_edge(id)?;
		self.metadata.bump(Some(DirtyTarget::Atom(id)))?;
		self.storage.edges.properties_mut(id).ok_or_else(|| atom_does_not_exist(id))
	}

	fn remove_atom(&mut self, id: AtomId) -> FerrosGrynnResult<()> {
		Graph::<Mutable>::remove_atom(self, id)
	}
}

impl QueryMaterialize for Graph<Mutable> {
	fn query_from<R: GraphRead>(source: &R) -> FerrosGrynnResult<Self> {
		copy_from_read(source, false)
	}
}

impl QueryMaterialize for Graph<Frozen> {
	fn query_from<R: GraphRead>(source: &R) -> FerrosGrynnResult<Self> {
		copy_from_read(source, false).map(Graph::freeze)
	}
}

impl<State> Debug for Graph<State> {
	fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
		formatter
			.debug_struct("Graph")
			.field("identity", &self.identity())
			.field("revision", &self.revision())
			.field("nodes", &self.node_count())
			.field("edges", &self.edge_count())
			.finish()
	}
}

impl<State> PartialEq for Graph<State> {
	fn eq(&self, other: &Self) -> bool {
		self.identity() == other.identity() && self.revision() == other.revision()
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
		(self.identity(), self.revision()).cmp(&(other.identity(), other.revision()))
	}
}

impl<State> Hash for Graph<State> {
	fn hash<H: Hasher>(&self, state: &mut H) {
		self.identity().hash(state);
		self.revision().hash(state);
	}
}

fn copy_from_read<R: GraphRead>(
	source: &R,
	preserve_ids: bool,
) -> FerrosGrynnResult<Graph<Mutable>> {
	let metadata = if preserve_ids {
		GraphMetadata::with_identity(
			source.identity(),
			source.revision(),
			source.properties().clone(),
		)
	} else {
		GraphMetadata::with_identity(Uuid::now_v7(), 0, source.properties().clone())
	};
	let mut result = Graph {
		metadata,
		storage: GeneralStorage::with_capacity(source.node_count(), source.edge_count()),
		state: PhantomData,
	};
	let mut ids = BTreeMap::new();
	let mut source_atoms = BTreeMap::new();

	for source_id in source.node_ids() {
		let node = source.node(source_id).ok_or_else(|| atom_does_not_exist(source_id))?;
		let local_id = if preserve_ids {
			result.storage.insert_node_at(
				source_id,
				node.record_id().clone(),
				node.label_text().cloned(),
				node.properties().clone(),
				node.inner_graph().cloned(),
			)?;
			source_id
		} else {
			result.storage.insert_node_new(
				node.record_id().clone(),
				node.label_text().cloned(),
				node.properties().clone(),
				node.inner_graph().cloned(),
			)?
		};
		ids.insert(source_id, local_id);
		source_atoms.insert(local_id, node.atom_ref());
	}

	for source_id in source.edge_ids() {
		let edge = source.edge(source_id).ok_or_else(|| atom_does_not_exist(source_id))?;
		let topology = remap_topology(edge.topology(), &ids)?;
		result.storage.validate_topology(&topology)?;
		let local_id = if preserve_ids {
			result.storage.insert_edge_at(
				source_id,
				edge.record_id().clone(),
				topology,
				edge.label_text().cloned(),
				edge.properties().clone(),
				edge.weight(),
				edge.transition_graph().cloned(),
			)?;
			source_id
		} else {
			result.storage.insert_edge_new(
				edge.record_id().clone(),
				topology,
				edge.label_text().cloned(),
				edge.properties().clone(),
				edge.weight(),
				edge.transition_graph().cloned(),
			)?
		};
		ids.insert(source_id, local_id);
		source_atoms.insert(local_id, edge.atom_ref());
	}

	if !preserve_ids {
		result.metadata.set_origin(MaterializationOrigin::new(source, source_atoms));
	}
	Ok(result)
}

fn remap_topology(
	topology: &EdgeTopology,
	ids: &BTreeMap<AtomId, AtomId>,
) -> FerrosGrynnResult<EdgeTopology> {
	topology.try_map_endpoints(|endpoint| match endpoint {
		EdgeEndpoint::Local(id) => {
			ids.get(id).copied().map(EdgeEndpoint::Local).ok_or_else(|| atom_does_not_exist(*id))
		}
		EdgeEndpoint::Nested(qualified) => {
			let mut scope = qualified.scope().to_vec();
			if let Some(first) = scope.first_mut() {
				*first = ids.get(first).copied().ok_or_else(|| atom_does_not_exist(*first))?;
			}
			QualifiedAtomId::new(scope, qualified.atom()).map(EdgeEndpoint::Nested)
		}
	})
}

pub(super) fn atom_does_not_exist(id: AtomId) -> FerrosGrynnError {
	FerrosGrynnError::atom_does_not_exist(
		id.kind().map_or_else(|| "null".to_owned(), |kind| kind.to_string()),
		id.to_string(),
	)
}

pub(super) fn atom_kind_mismatch(id: AtomId, expected: AtomKind) -> FerrosGrynnError {
	FerrosGrynnError::atom_kind_mismatch(
		id.to_string(),
		id.kind().map_or_else(|| "null".to_owned(), |kind| kind.to_string()),
		expected.to_string(),
	)
}

fn endpoint_kind(endpoint: &EdgeEndpoint) -> String {
	match endpoint {
		EdgeEndpoint::Local(id) => {
			id.kind().map_or_else(|| "null".to_owned(), |kind| kind.to_string())
		}
		EdgeEndpoint::Nested(value) => {
			value.atom().kind().map_or_else(|| "null".to_owned(), |kind| format!("nested {kind}"))
		}
	}
}

#[cfg(test)]
mod tests {
	use super::Graph;
	use crate::{
		gql_status::Domain,
		values::{
			PropertyMap,
			runtime::graph::{
				EdgeDirection,
				EdgeEndpoint,
				EdgeTopology,
				Frozen,
				Mutable,
				QualifiedAtomId,
			},
		},
	};

	#[test]
	fn supports_node_edge_edge_edge_and_cross_level_incidence() {
		let mut inner = Graph::<Mutable>::new();
		let inner_node = inner.add_node(Some("inner".into()), PropertyMap::new()).unwrap();
		let mut graph = Graph::<Mutable>::new();
		let meta =
			graph.add_metavertex(inner.freeze(), Some("meta".into()), PropertyMap::new()).unwrap();
		let right = graph.add_node(Some("right".into()), PropertyMap::new()).unwrap();
		let edge = graph
			.add_edge(EdgeTopology::directed([meta], [right]), None, PropertyMap::new())
			.unwrap();
		let connector = graph
			.add_edge(EdgeTopology::directed([edge], [right]), None, PropertyMap::new())
			.unwrap();
		let nested = QualifiedAtomId::new(vec![meta], inner_node).unwrap();
		let cross = graph
			.add_edge(
				EdgeTopology::directed([EdgeEndpoint::Nested(nested)], [EdgeEndpoint::Local(
					right,
				)]),
				None,
				PropertyMap::new(),
			)
			.unwrap();

		assert!(graph.edge(connector).is_some());
		assert!(graph.edge(cross).unwrap().topology().is_cross_level());
		assert_eq!(graph.incident_edges(edge).collect::<Vec<_>>(), [connector]);
	}

	#[test]
	fn universal_graph_allows_directed_self_loops() {
		let mut graph = Graph::<Mutable>::new();
		let node = graph.add_node(None, PropertyMap::new()).unwrap();
		let edge = graph
			.add_edge(EdgeTopology::directed([node], [node]), None, PropertyMap::new())
			.unwrap();
		assert_eq!(
			graph.edge(edge).unwrap().topology().local_binary(),
			Some((node, node, EdgeDirection::Directed,))
		);
	}

	#[test]
	fn removal_cascades_through_edge_endpoints_and_reuses_generation() {
		let mut graph = Graph::<Mutable>::new();
		let left = graph.add_node(None, PropertyMap::new()).unwrap();
		let right = graph.add_node(None, PropertyMap::new()).unwrap();
		let edge = graph
			.add_edge(EdgeTopology::directed([left], [right]), None, PropertyMap::new())
			.unwrap();
		let connector = graph
			.add_edge(EdgeTopology::directed([edge], [right]), None, PropertyMap::new())
			.unwrap();

		graph.remove_atom(edge).unwrap();
		assert!(!graph.contains(edge));
		assert!(!graph.contains(connector));
		let replacement = graph
			.add_edge(EdgeTopology::directed([left], [right]), None, PropertyMap::new())
			.unwrap();
		assert_eq!(replacement.slot(), connector.slot());
		assert_ne!(replacement.generation(), connector.generation());
	}

	#[test]
	fn invalid_edges_are_transactional_and_use_graph_statuses() {
		let mut graph = Graph::<Mutable>::new();
		let unknown = super::AtomId::from_raw(42);
		let revision = graph.revision();
		let error = graph
			.add_edge(EdgeTopology::undirected([unknown]), None, PropertyMap::new())
			.unwrap_err();

		assert_eq!(error.status().code().to_string(), "FG-G2F02");
		assert_eq!(error.status().domain(), Domain::Graph);
		assert_eq!(graph.revision(), revision);
		assert_eq!(graph.edge_count(), 0);
	}

	#[test]
	fn freeze_thaw_preserves_identity_ids_and_topology() {
		let mut graph = Graph::<Mutable>::new();
		let left = graph.add_node(None, PropertyMap::new()).unwrap();
		let right = graph.add_node(None, PropertyMap::new()).unwrap();
		let edge = graph
			.add_edge(EdgeTopology::bidirectional([left], [right]), None, PropertyMap::new())
			.unwrap();
		let identity = graph.identity();
		let revision = graph.revision();

		let graph: Graph<Frozen> = graph.freeze();
		let graph = graph.thaw();
		assert_eq!(graph.identity(), identity);
		assert_eq!(graph.revision(), revision);
		assert!(graph.contains(left));
		assert!(graph.contains(edge));
	}
}
