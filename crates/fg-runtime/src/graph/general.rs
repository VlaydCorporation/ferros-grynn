use std::{
	borrow::Cow,
	cmp::Ordering,
	collections::{BTreeMap, BTreeSet},
	fmt::{self, Debug, Formatter},
	hash::{Hash, Hasher},
	marker::PhantomData,
	mem::size_of,
	sync::Arc,
};

use rapidhash::RapidHashMap;
use smallvec::SmallVec;

use super::{
	AtomId,
	AtomIds,
	AtomKind,
	AtomRef,
	DirtyTarget,
	EdgeEndpoint,
	EdgeRead,
	EdgeTopology,
	EdgeView,
	FallibleAtomIds,
	Frozen,
	GraphKind,
	GraphMut,
	GraphRead,
	GraphReadView,
	GraphRef,
	GraphStorageMode,
	GraphTopologyConstraints,
	GraphWriteBack,
	MaterializationOrigin,
	Mutable,
	NodeRead,
	NodeView,
	QualifiedAtomId,
	QueryMaterialize,
	SnapshotId,
	materialized::{
		MaterializedGraph,
		estimated_edge_fields_heap_bytes,
		estimated_graph_fixed_heap_bytes,
		estimated_node_fields_heap_bytes,
	},
	storage::{EdgeColumns, GraphMetadata, NodeColumns, ensure_unique_record, generated_record_id},
};
use crate::{
	collections::cow_vec::CowVec,
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	values::runtime::{Number, PropertyMap, RecordId, Text, Uuid},
};

/// Configured inline incidence value.
const INLINE_INCIDENCE: usize = 4;

#[derive(Debug, Clone)]
/// General storage state.
pub(super) struct GeneralStorage {
	/// Nodes associated with this general storage.
	pub(super) nodes: NodeColumns,
	/// Edges associated with this general storage.
	pub(super) edges: EdgeColumns,
	/// Node inner associated with this general storage.
	node_inner: CowVec<Option<Arc<Graph<Frozen>>>>,
	/// Edge transition associated with this general storage.
	edge_transition: CowVec<Option<Arc<Graph<Frozen>>>>,
	/// Incidence associated with this general storage.
	incidence: Arc<RapidHashMap<EdgeEndpoint, SmallVec<AtomId, INLINE_INCIDENCE>>>,
}

impl GeneralStorage {
	/// Creates a new `GeneralStorage` value.
	fn new() -> Self {
		Self {
			nodes: NodeColumns::new(),
			edges: EdgeColumns::new(),
			node_inner: CowVec::new(),
			edge_transition: CowVec::new(),
			incidence: Arc::new(RapidHashMap::default()),
		}
	}

	/// Configures capacity.
	fn with_capacity(nodes: usize, edges: usize) -> Self {
		Self {
			nodes: NodeColumns::with_capacity(nodes),
			edges: EdgeColumns::with_capacity(edges),
			node_inner: CowVec::with_capacity(nodes),
			edge_transition: CowVec::with_capacity(edges),
			incidence: Arc::new(RapidHashMap::default()),
		}
	}

	/// Inserts node new.
	fn insert_node_new(
		&mut self,
		record_id: RecordId,
		label: Option<Text>,
		properties: PropertyMap,
		inner: Option<Graph<Frozen>>,
	) -> FerrosGrynnResult<AtomId> {
		let id = self.nodes.insert_new(record_id, label, properties)?;
		self.ensure_node_slot(id.slot() as usize);
		self.node_inner[id.slot() as usize] = inner.map(Arc::new);
		Ok(id)
	}

	/// Inserts node at.
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
		self.node_inner[id.slot() as usize] = inner.map(Arc::new);
		Ok(())
	}

	/// Inserts edge new.
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
		self.edge_transition[id.slot() as usize] = transition.map(Arc::new);
		self.register_incidence(id, &topology);
		Ok(id)
	}

	/// Inserts edge at.
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
		self.edge_transition[id.slot() as usize] = transition.map(Arc::new);
		self.register_incidence(id, &topology);
		Ok(())
	}

	/// Removes edge raw.
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

	/// Sets edge topology.
	fn set_edge_topology(&mut self, id: AtomId, topology: EdgeTopology) -> FerrosGrynnResult<()> {
		self.validate_topology(&topology)?;
		let previous = self.edges.topology(id).cloned().ok_or_else(|| atom_does_not_exist(id))?;
		self.unregister_incidence(id, &previous);
		*self.edges.topology_mut(id).ok_or_else(|| atom_does_not_exist(id))? = topology.clone();
		self.register_incidence(id, &topology);
		Ok(())
	}

	/// Returns the node inner result.
	fn node_inner(&self, id: AtomId) -> Option<&Graph<Frozen>> {
		self.nodes.contains(id).then(|| self.node_inner.get(id.slot() as usize)?.as_deref())?
	}

	/// Returns the edge transition result.
	fn edge_transition(&self, id: AtomId) -> Option<&Graph<Frozen>> {
		self.edges.contains(id).then(|| self.edge_transition.get(id.slot() as usize)?.as_deref())?
	}

	/// Validates topology.
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

	/// Checks for endpoint.
	fn contains_endpoint(&self, endpoint: &EdgeEndpoint) -> bool {
		match endpoint {
			EdgeEndpoint::Local(id) => self.contains(*id),
			EdgeEndpoint::Nested(qualified) => self.contains_qualified(qualified),
		}
	}

	/// Checks for qualified.
	fn contains_qualified(&self, qualified: &QualifiedAtomId) -> bool {
		let mut scope = qualified.scope().iter().copied();
		let Some(first) = scope.next() else {
			return false;
		};
		let Some(mut graph) = self.owned_child(first) else {
			return false;
		};
		for container in scope {
			let Some(next) = graph.storage.owned_child(container) else {
				return false;
			};
			graph = next;
		}
		graph.contains(qualified.atom())
	}

	/// Borrows the immutable child bound to this concrete owner.
	fn owned_child(&self, owner: AtomId) -> Option<&Graph<Frozen>> {
		match owner.kind()? {
			AtomKind::Node => self.node_inner(owner),
			AtomKind::Edge => self.edge_transition(owner),
		}
	}

	/// Visits physically retained child layouts for shared-scope accounting.
	fn owned_children(&self) -> impl Iterator<Item = &Graph<Frozen>> {
		self.node_inner.iter().chain(self.edge_transition.iter()).flatten().map(Arc::as_ref)
	}

	/// Validates qualified incidence after restoring owned child bindings.
	fn validate_owned_topology(&self) -> FerrosGrynnResult<()> {
		for id in self.edges.ids().iter() {
			let topology = self.edges.topology(*id).ok_or_else(|| atom_does_not_exist(*id))?;
			self.validate_topology(topology)?;
		}
		Ok(())
	}

	/// Checks for general storage.
	fn contains(&self, id: AtomId) -> bool {
		match id.kind() {
			Some(AtomKind::Node) => self.nodes.contains(id),
			Some(AtomKind::Edge) => self.edges.contains(id),
			None => false,
		}
	}

	/// Returns the incident edges result.
	fn incident_edges(&self, endpoint: &EdgeEndpoint) -> &[AtomId] {
		self.incidence.get(endpoint).map_or(&[], SmallVec::as_slice)
	}

	/// Registers incidence.
	fn register_incidence(&mut self, id: AtomId, topology: &EdgeTopology) {
		let incidence = Arc::make_mut(&mut self.incidence);
		for endpoint in topology.endpoints().cloned() {
			incidence.entry(endpoint).or_default().push(id);
		}
	}

	/// Unregisters incidence.
	fn unregister_incidence(&mut self, id: AtomId, topology: &EdgeTopology) {
		let incidence = Arc::make_mut(&mut self.incidence);
		for endpoint in topology.endpoints() {
			let remove_entry = if let Some(edges) = incidence.get_mut(endpoint) {
				if let Some(index) = edges.iter().position(|candidate| *candidate == id) {
					edges.swap_remove(index);
				}
				edges.is_empty()
			} else {
				false
			};
			if remove_entry {
				incidence.remove(endpoint);
			}
		}
	}

	/// Collects dependent edges.
	fn collect_dependent_edges(&self, id: AtomId) -> BTreeSet<AtomId> {
		let mut pending = Vec::new();
		let mut edges = BTreeSet::new();
		if id.is_edge() {
			pending.push(id);
		}
		pending.extend(self.incident_edges(&EdgeEndpoint::Local(id)).iter().copied());
		if id.is_node() {
			for (endpoint, incident) in self.incidence.iter() {
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

	/// Ensures node slot.
	fn ensure_node_slot(&mut self, index: usize) {
		while self.node_inner.len() <= index {
			self.node_inner.push(None);
		}
	}

	/// Ensures edge slot.
	fn ensure_edge_slot(&mut self, index: usize) {
		while self.edge_transition.len() <= index {
			self.edge_transition.push(None);
		}
	}
}

impl Default for GeneralStorage {
	/// Constructs the default initial state of `GeneralStorage`.
	fn default() -> Self {
		Self::new()
	}
}

/// General graph-family materialization with atom storage and graph metadata.
pub struct Graph<State = Frozen> {
	/// Metadata associated with this graph.
	pub(super) metadata: GraphMetadata,
	/// Storage associated with this graph.
	pub(super) storage: GeneralStorage,
	/// State associated with this graph.
	state: PhantomData<State>,
}

pub type MutableGraph = Graph<Mutable>;
pub type FrozenGraph = Graph<Frozen>;

impl Graph<Mutable> {
	/// Constructs `Graph<Mutable>` from the supplied initial state.
	pub fn new() -> Self {
		Self {
			metadata: GraphMetadata::new(),
			storage: GeneralStorage::new(),
			state: PhantomData,
		}
	}

	/// Creates an empty collection with the requested initial capacity.
	pub fn with_capacity(nodes: usize, edges: usize) -> Self {
		Self {
			metadata: GraphMetadata::new(),
			storage: GeneralStorage::with_capacity(nodes, edges),
			state: PhantomData,
		}
	}

	/// Returns add node for `Graph<Mutable>`.
	///
	/// # Errors
	///
	/// Propagates errors returned by the underlying operation.
	pub fn add_node(
		&mut self,
		label: Option<Text>,
		properties: PropertyMap,
	) -> FerrosGrynnResult<AtomId> {
		self.add_node_with_id(generated_record_id(AtomKind::Node)?, label, properties)
	}

	/// Returns add node with id for `Graph<Mutable>`.
	///
	/// # Errors
	///
	/// Propagates validation or execution failures from the underlying
	/// operation.
	pub fn add_node_with_id(
		&mut self,
		record_id: RecordId,
		label: Option<Text>,
		properties: PropertyMap,
	) -> FerrosGrynnResult<AtomId> {
		self.add_node_raw(record_id, label, properties, None)
	}

	/// Returns add metavertex for `Graph<Mutable>`.
	///
	/// # Errors
	///
	/// Propagates errors returned by the underlying operation.
	pub fn add_metavertex(
		&mut self,
		inner: Graph<Frozen>,
		label: Option<Text>,
		properties: PropertyMap,
	) -> FerrosGrynnResult<AtomId> {
		self.add_node_raw(generated_record_id(AtomKind::Node)?, label, properties, Some(inner))
	}

	/// Returns add edge for `Graph<Mutable>`.
	///
	/// # Errors
	///
	/// Propagates errors returned by the underlying operation.
	pub fn add_edge(
		&mut self,
		topology: EdgeTopology,
		label: Option<Text>,
		properties: PropertyMap,
	) -> FerrosGrynnResult<AtomId> {
		self.add_edge_with_id(generated_record_id(AtomKind::Edge)?, topology, label, properties)
	}

	/// Returns add edge with id for `Graph<Mutable>`.
	///
	/// # Errors
	///
	/// Propagates validation or execution failures from the underlying
	/// operation.
	pub fn add_edge_with_id(
		&mut self,
		record_id: RecordId,
		topology: EdgeTopology,
		label: Option<Text>,
		properties: PropertyMap,
	) -> FerrosGrynnResult<AtomId> {
		self.add_edge_raw(record_id, topology, label, properties, None, None)
	}

	/// Returns add metaedge for `Graph<Mutable>`.
	///
	/// # Errors
	///
	/// Propagates errors returned by the underlying operation.
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

	/// Sets node label for `Graph<Mutable>`.
	///
	/// # Errors
	///
	/// Propagates errors returned by the underlying operation.
	pub fn set_node_label(&mut self, id: AtomId, label: Option<Text>) -> FerrosGrynnResult<()> {
		self.check_node(id)?;
		self.metadata.ensure_can_bump()?;
		if let Some(slot) = self.storage.nodes.label_mut(id) {
			*slot = label;
		}
		self.metadata.bump(Some(DirtyTarget::Atom(id)))
	}

	/// Sets edge label for `Graph<Mutable>`.
	///
	/// # Errors
	///
	/// Propagates errors returned by the underlying operation.
	pub fn set_edge_label(&mut self, id: AtomId, label: Option<Text>) -> FerrosGrynnResult<()> {
		self.check_edge(id)?;
		self.metadata.ensure_can_bump()?;
		if let Some(slot) = self.storage.edges.label_mut(id) {
			*slot = label;
		}
		self.metadata.bump(Some(DirtyTarget::Atom(id)))
	}

	/// Sets edge weight for `Graph<Mutable>`.
	///
	/// # Errors
	///
	/// Propagates errors returned by the underlying operation.
	pub fn set_edge_weight(&mut self, id: AtomId, weight: Option<Number>) -> FerrosGrynnResult<()> {
		self.check_edge(id)?;
		self.metadata.ensure_can_bump()?;
		if let Some(slot) = self.storage.edges.weight_mut(id) {
			*slot = weight.filter(|value| !value.is_nan());
		}
		self.metadata.bump(Some(DirtyTarget::Atom(id)))
	}

	/// Replaces edge topology.
	#[cfg(any(test, feature = "test-support"))]
	pub(crate) fn replace_edge_topology(
		&mut self,
		id: AtomId,
		topology: EdgeTopology,
	) -> FerrosGrynnResult<()> {
		self.check_edge(id)?;
		self.metadata.ensure_can_bump()?;
		self.storage.set_edge_topology(id, topology)?;
		self.metadata.bump(Some(DirtyTarget::Atom(id)))
	}

	/// Replaces inner graph for `Graph<Mutable>`.
	///
	/// # Errors
	///
	/// Propagates errors returned by the underlying operation.
	pub fn replace_inner_graph(
		&mut self,
		id: AtomId,
		inner: Option<Graph<Frozen>>,
	) -> FerrosGrynnResult<Option<Graph<Frozen>>> {
		self.check_node(id)?;
		self.metadata.ensure_can_bump()?;
		let slot = &mut self.storage.node_inner[id.slot() as usize];
		let previous = std::mem::replace(slot, inner.map(Arc::new)).map(Arc::unwrap_or_clone);
		self.metadata.bump(Some(DirtyTarget::Atom(id)))?;
		Ok(previous)
	}

	/// Replaces transition graph for `Graph<Mutable>`.
	///
	/// # Errors
	///
	/// Propagates errors returned by the underlying operation.
	pub fn replace_transition_graph(
		&mut self,
		id: AtomId,
		transition: Option<Graph<Frozen>>,
	) -> FerrosGrynnResult<Option<Graph<Frozen>>> {
		self.check_edge(id)?;
		self.metadata.ensure_can_bump()?;
		let slot = &mut self.storage.edge_transition[id.slot() as usize];
		let previous = std::mem::replace(slot, transition.map(Arc::new)).map(Arc::unwrap_or_clone);
		self.metadata.bump(Some(DirtyTarget::Atom(id)))?;
		Ok(previous)
	}

	/// Removes atom for `Graph<Mutable>`.
	///
	/// # Errors
	///
	/// Propagates errors returned by the underlying operation.
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

	/// Returns freeze for `Graph<Mutable>`.
	pub fn freeze(self) -> Graph<Frozen> {
		Graph {
			metadata: self.metadata.into_frozen(),
			storage: self.storage,
			state: PhantomData,
		}
	}

	/// Performs the flush operation for `Graph<Mutable>`.
	///
	/// # Errors
	///
	/// Propagates errors returned by the underlying operation.
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

	/// Performs the adopt external revision operation.
	pub(crate) fn adopt_external_revision(&mut self, revision: u64) {
		self.metadata.adopt_external_revision(revision);
	}

	/// Clears local dirty metadata after accepting the remote base.
	pub(crate) fn clear_live_dirty(&mut self) {
		self.metadata.clear_dirty();
	}

	/// Returns the clone preserving state result.
	pub(crate) fn clone_preserving_state(&self) -> Self {
		Self {
			metadata: self.metadata.clone(),
			storage: self.storage.clone(),
			state: PhantomData,
		}
	}

	/// Returns the writeback context result.
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

	/// Finishes writeback.
	pub(crate) fn finish_writeback(&mut self) {
		self.metadata.clear_dirty();
	}

	/// Returns the transaction dirty result.
	#[cfg(test)]
	pub(crate) fn transaction_dirty(&self) -> Vec<DirtyTarget> {
		self.metadata.dirty().cloned().collect()
	}

	/// Sets topology constraints.
	pub(crate) fn set_topology_constraints(&mut self, constraints: GraphTopologyConstraints) {
		self.metadata.set_topology_constraints(constraints);
	}

	/// Creates from read preserving.
	pub(crate) fn from_read_preserving<R: GraphRead>(source: &R) -> FerrosGrynnResult<Self> {
		copy_from_read(source, true)
	}

	/// Returns the query flat with heap estimate result.
	pub(crate) fn query_flat_with_heap_estimate<R: GraphRead>(
		source: &R,
	) -> FerrosGrynnResult<Option<(Self, usize)>> {
		copy_from_read_inner(source, false, true)
	}

	/// Starts an owned query result without recording construction as mutation.
	pub(crate) fn begin_query_materialization(
		properties: PropertyMap,
		nodes: usize,
		edges: usize,
	) -> Self {
		Self {
			metadata: GraphMetadata::with_identity(Uuid::now_v7(), 0, properties),
			storage: GeneralStorage::with_capacity(nodes, edges),
			state: PhantomData,
		}
	}

	/// Starts a restored result with its stable incarnation identity and
	/// watermark.
	pub(crate) fn begin_live_materialization(
		identity: Uuid,
		revision: u64,
		properties: PropertyMap,
		nodes: usize,
		edges: usize,
	) -> Self {
		Self {
			metadata: GraphMetadata::with_identity(identity, revision, properties),
			storage: GeneralStorage::with_capacity(nodes, edges),
			state: PhantomData,
		}
	}

	/// Inserts one fully decoded node into a query result.
	pub(crate) fn materialize_query_node(
		&mut self,
		record_id: RecordId,
		label: Option<Text>,
		properties: PropertyMap,
		inner: Option<Graph<Frozen>>,
	) -> FerrosGrynnResult<AtomId> {
		ensure_unique_record(&self.storage.nodes, &self.storage.edges, &record_id)?;
		self.storage.insert_node_new(record_id, label, properties, inner)
	}

	/// Reserves one decoded edge before all remapped endpoints are available.
	pub(crate) fn reserve_query_edge(
		&mut self,
		record_id: RecordId,
		label: Option<Text>,
		properties: PropertyMap,
		weight: Option<Number>,
		transition: Option<Graph<Frozen>>,
	) -> FerrosGrynnResult<AtomId> {
		ensure_unique_record(&self.storage.nodes, &self.storage.edges, &record_id)?;
		self.storage.insert_edge_new(
			record_id,
			EdgeTopology::undirected(std::iter::empty::<AtomId>()),
			label,
			properties,
			weight,
			transition,
		)
	}

	/// Publishes and validates the final topology of a reserved query edge.
	pub(crate) fn materialize_query_edge_topology(
		&mut self,
		id: AtomId,
		topology: EdgeTopology,
	) -> FerrosGrynnResult<()> {
		self.storage.set_edge_topology(id, topology)
	}

	/// Attaches owned logical mappings for global query results and restored
	/// images.
	pub(crate) fn finish_query_materialization_parts(
		&mut self,
		identity: Uuid,
		snapshot: SnapshotId,
		reference: Option<GraphRef>,
		source_atoms: BTreeMap<AtomId, AtomRef>,
	) {
		self.metadata.set_origin(MaterializationOrigin::from_parts(
			identity,
			snapshot,
			reference,
			source_atoms,
		));
	}

	/// Attaches the exact source snapshot and clears construction state.
	pub(crate) fn finish_query_materialization<R: GraphRead>(
		&mut self,
		source: &R,
		source_atoms: BTreeMap<AtomId, AtomRef>,
	) {
		self.metadata.set_origin(MaterializationOrigin::new(source, source_atoms));
	}

	/// Returns the add node raw result.
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

	/// Returns the add edge raw result.
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
	/// Constructs the default initial state of `Graph<Mutable>`.
	fn default() -> Self {
		Self::new()
	}
}

impl Graph<Frozen> {
	/// Returns thaw for `Graph<Frozen>`.
	pub fn thaw(self) -> Graph<Mutable> {
		Graph {
			metadata: self.metadata,
			storage: self.storage,
			state: PhantomData,
		}
	}
}

impl Clone for Graph<Frozen> {
	/// Returns an owned clone of this `Graph<Frozen>`.
	fn clone(&self) -> Self {
		Self {
			metadata: self.metadata.clone(),
			storage: self.storage.clone(),
			state: PhantomData,
		}
	}
}

impl<State> Graph<State> {
	/// Validates owned endpoint routes and every contained graph without
	/// copying layouts.
	pub(crate) fn validate_owned_topology(&self) -> FerrosGrynnResult<()> {
		self.storage.validate_owned_topology()?;
		let mut pending = self.storage.owned_children().collect::<Vec<_>>();
		let mut containment = BTreeMap::from([(
			self.identity(),
			self.storage.owned_children().map(Graph::identity).collect::<BTreeSet<_>>(),
		)]);
		let mut visited = BTreeSet::new();
		while let Some(graph) = pending.pop() {
			if !visited.insert(std::ptr::from_ref(graph)) {
				continue;
			}
			graph.storage.validate_owned_topology()?;
			super::validate_topology_constraints(graph, graph.topology_constraints())?;
			// Union every physical variant's logical links before memoizing
			// traversal.
			containment
				.entry(graph.identity())
				.or_default()
				.extend(graph.storage.owned_children().map(Graph::identity));
			pending.extend(graph.storage.owned_children());
		}
		let mut incoming = containment
			.keys()
			.copied()
			.map(|identity| (identity, 0_usize))
			.collect::<BTreeMap<_, _>>();
		for children in containment.values() {
			for child in children {
				let count = incoming.get_mut(child).ok_or_else(|| {
					FerrosGrynnError::graph_constraint_violation(
						"graph containment",
						"a contained graph was not inspected",
					)
				})?;
				*count = count.checked_add(1).ok_or_else(|| {
					FerrosGrynnError::operation_overflow("graph containment indegree")
				})?;
			}
		}
		let mut ready = incoming
			.iter()
			.filter_map(|(identity, count)| (*count == 0).then_some(*identity))
			.collect::<Vec<_>>();
		while let Some(identity) = ready.pop() {
			incoming.remove(&identity);
			if let Some(children) = containment.get(&identity) {
				for child in children {
					let count = incoming.get_mut(child).ok_or_else(|| {
						FerrosGrynnError::graph_constraint_violation(
							"graph containment",
							"a contained graph was processed before its owner",
						)
					})?;
					*count = count.checked_sub(1).ok_or_else(|| {
						FerrosGrynnError::operation_overflow("graph containment indegree")
					})?;
					if *count == 0 {
						ready.push(*child);
					}
				}
			}
		}
		if !incoming.is_empty() {
			return Err(FerrosGrynnError::graph_constraint_violation(
				"graph containment",
				"a contained graph repeats an ancestor identity",
			));
		}
		Ok(())
	}

	/// Returns the slot capacity result.
	pub(crate) fn slot_capacity(&self, kind: AtomKind) -> usize {
		match kind {
			AtomKind::Node => self.storage.nodes.slot_capacity(),
			AtomKind::Edge => self.storage.edges.slot_capacity(),
		}
	}

	/// Returns identity for `Graph<State>`.
	pub const fn identity(&self) -> Uuid {
		self.metadata.identity()
	}

	/// Returns revision for `Graph<State>`.
	pub const fn revision(&self) -> u64 {
		self.metadata.revision()
	}

	/// Returns kind for `Graph<State>`.
	pub const fn kind(&self) -> GraphKind {
		GraphKind::Graph
	}

	/// Returns properties for `Graph<State>`.
	pub const fn properties(&self) -> &PropertyMap {
		self.metadata.properties()
	}

	/// Returns topology constraints for `Graph<State>`.
	pub const fn topology_constraints(&self) -> GraphTopologyConstraints {
		self.metadata.topology_constraints()
	}

	/// Returns node count for `Graph<State>`.
	pub const fn node_count(&self) -> usize {
		self.storage.nodes.len()
	}

	/// Returns edge count for `Graph<State>`.
	pub const fn edge_count(&self) -> usize {
		self.storage.edges.len()
	}

	/// Reports whether the contains condition holds for `Graph<State>`.
	pub fn contains(&self, id: AtomId) -> bool {
		self.storage.contains(id)
	}

	/// Returns node for `Graph<State>`.
	pub fn node(&self, id: AtomId) -> Option<NodeView<'_>> {
		Some(NodeView::new(
			id,
			self.storage.nodes.record_id(id)?,
			self.storage.nodes.label(id)?.as_ref(),
			self.storage.nodes.properties(id)?,
			self.storage.node_inner(id),
		))
	}

	/// Returns edge for `Graph<State>`.
	pub fn edge(&self, id: AtomId) -> Option<EdgeView<'_>> {
		Some(EdgeView::new(
			id,
			self.storage.edges.record_id(id)?,
			self.storage.edges.topology(id)?,
			self.storage.edges.label(id)?.as_ref(),
			self.storage.edges.properties(id)?,
			self.storage.edges.weight(id)?.as_ref().copied(),
			self.storage.edge_transition(id),
		))
	}

	/// Returns nodes for `Graph<State>`.
	pub fn nodes(&self) -> impl Iterator<Item = NodeView<'_>> {
		self.storage.nodes.ids().iter().filter_map(|id| self.node(*id))
	}

	/// Returns edges for `Graph<State>`.
	pub fn edges(&self) -> impl Iterator<Item = EdgeView<'_>> {
		self.storage.edges.ids().iter().filter_map(|id| self.edge(*id))
	}

	/// Returns incident edges for `Graph<State>`.
	pub fn incident_edges(
		&self,
		endpoint: AtomId,
	) -> std::iter::Copied<std::slice::Iter<'_, AtomId>> {
		self.incident_edges_at(&EdgeEndpoint::Local(endpoint))
	}

	/// Returns incident edges at for `Graph<State>`.
	pub fn incident_edges_at(
		&self,
		endpoint: &EdgeEndpoint,
	) -> std::iter::Copied<std::slice::Iter<'_, AtomId>> {
		self.storage.incident_edges(endpoint).iter().copied()
	}

	/// Reads view for `Graph<State>`.
	pub const fn read_view(&self, snapshot: SnapshotId) -> GraphReadView<'_, Self> {
		GraphReadView::new(self, snapshot)
	}

	/// Returns materialization origin for `Graph<State>`.
	pub const fn materialization_origin(&self) -> Option<&MaterializationOrigin> {
		self.metadata.origin()
	}

	/// Converts this value into raw.
	pub(super) fn into_raw(self) -> (GraphMetadata, GeneralStorage) {
		(self.metadata, self.storage)
	}

	/// Creates from raw.
	pub(super) const fn from_raw(metadata: GraphMetadata, storage: GeneralStorage) -> Self {
		Self {
			metadata,
			storage,
			state: PhantomData,
		}
	}

	/// Checks node.
	fn check_node(&self, id: AtomId) -> FerrosGrynnResult<()> {
		if id.kind() != Some(AtomKind::Node) {
			return Err(atom_kind_mismatch(id, AtomKind::Node));
		}
		self.storage.nodes.contains(id).then_some(()).ok_or_else(|| atom_does_not_exist(id))
	}

	/// Checks edge.
	fn check_edge(&self, id: AtomId) -> FerrosGrynnResult<()> {
		if id.kind() != Some(AtomKind::Edge) {
			return Err(atom_kind_mismatch(id, AtomKind::Edge));
		}
		self.storage.edges.contains(id).then_some(()).ok_or_else(|| atom_does_not_exist(id))
	}
}

impl<State> GraphRead for Graph<State> {
	type Edge<'a>
		= EdgeView<'a>
	where
		Self: 'a;
	type EdgeIds<'a>
		= FallibleAtomIds<AtomIds<'a>>
	where
		Self: 'a;
	type Node<'a>
		= NodeView<'a>
	where
		Self: 'a;
	type NodeIds<'a>
		= FallibleAtomIds<AtomIds<'a>>
	where
		Self: 'a;

	/// Returns identity for `Graph<State>`.
	fn identity(&self) -> Uuid {
		self.identity()
	}

	/// Returns revision for `Graph<State>`.
	fn revision(&self) -> u64 {
		self.revision()
	}

	/// Returns kind for `Graph<State>`.
	fn kind(&self) -> GraphKind {
		GraphKind::Graph
	}

	/// Returns properties for `Graph<State>`.
	fn properties(&self) -> FerrosGrynnResult<Cow<'_, PropertyMap>> {
		Ok(Cow::Borrowed(self.properties()))
	}

	/// Returns topology constraints for `Graph<State>`.
	fn topology_constraints(&self) -> GraphTopologyConstraints {
		self.topology_constraints()
	}

	/// Returns node ids for `Graph<State>`.
	fn node_ids(&self) -> FerrosGrynnResult<Self::NodeIds<'_>> {
		Ok(FallibleAtomIds::new(AtomIds::new(self.storage.nodes.ids().iter())))
	}

	/// Returns edge ids for `Graph<State>`.
	fn edge_ids(&self) -> FerrosGrynnResult<Self::EdgeIds<'_>> {
		Ok(FallibleAtomIds::new(AtomIds::new(self.storage.edges.ids().iter())))
	}

	/// Returns node for `Graph<State>`.
	fn node(&self, id: AtomId) -> FerrosGrynnResult<Option<Self::Node<'_>>> {
		let Some(record_id) = self.storage.nodes.record_id(id) else {
			return Ok(None);
		};
		let Some(label) = self.storage.nodes.label(id) else {
			return Ok(None);
		};
		let Some(properties) = self.storage.nodes.properties(id) else {
			return Ok(None);
		};
		Ok(Some(NodeView::new(
			id,
			record_id,
			label.as_ref(),
			properties,
			self.storage.node_inner(id),
		)))
	}

	/// Returns edge for `Graph<State>`.
	fn edge(&self, id: AtomId) -> FerrosGrynnResult<Option<Self::Edge<'_>>> {
		let Some(record_id) = self.storage.edges.record_id(id) else {
			return Ok(None);
		};
		let Some(topology) = self.storage.edges.topology(id) else {
			return Ok(None);
		};
		let Some(label) = self.storage.edges.label(id) else {
			return Ok(None);
		};
		let Some(properties) = self.storage.edges.properties(id) else {
			return Ok(None);
		};
		let Some(weight) = self.storage.edges.weight(id) else {
			return Ok(None);
		};
		Ok(Some(EdgeView::new(
			id,
			record_id,
			topology,
			label.as_ref(),
			properties,
			weight.as_ref().copied(),
			self.storage.edge_transition(id),
		)))
	}

	/// Returns storage mode for `Graph<State>`.
	fn storage_mode(&self) -> GraphStorageMode {
		self.metadata.storage_mode()
	}

	/// Returns graph ref for `Graph<State>`.
	fn graph_ref(&self) -> Option<&super::GraphRef> {
		self.metadata.graph_ref()
	}

	/// Returns materialization origin for `Graph<State>`.
	fn materialization_origin(&self) -> Option<&MaterializationOrigin> {
		self.metadata.origin()
	}
}

impl GraphMut for Graph<Mutable> {
	/// Returns properties mut for `Graph<Mutable>`.
	fn properties_mut(&mut self) -> FerrosGrynnResult<&mut PropertyMap> {
		self.metadata.properties_mut()
	}

	/// Returns node label mut for `Graph<Mutable>`.
	fn node_label_mut(&mut self, id: AtomId) -> FerrosGrynnResult<&mut Option<Text>> {
		self.check_node(id)?;
		self.metadata.bump(Some(DirtyTarget::Atom(id)))?;
		self.storage.nodes.label_mut(id).ok_or_else(|| atom_does_not_exist(id))
	}

	/// Returns node properties mut for `Graph<Mutable>`.
	fn node_properties_mut(&mut self, id: AtomId) -> FerrosGrynnResult<&mut PropertyMap> {
		self.check_node(id)?;
		self.metadata.bump(Some(DirtyTarget::Atom(id)))?;
		self.storage.nodes.properties_mut(id).ok_or_else(|| atom_does_not_exist(id))
	}

	/// Returns edge label mut for `Graph<Mutable>`.
	fn edge_label_mut(&mut self, id: AtomId) -> FerrosGrynnResult<&mut Option<Text>> {
		self.check_edge(id)?;
		self.metadata.bump(Some(DirtyTarget::Atom(id)))?;
		self.storage.edges.label_mut(id).ok_or_else(|| atom_does_not_exist(id))
	}

	/// Returns edge properties mut for `Graph<Mutable>`.
	fn edge_properties_mut(&mut self, id: AtomId) -> FerrosGrynnResult<&mut PropertyMap> {
		self.check_edge(id)?;
		self.metadata.bump(Some(DirtyTarget::Atom(id)))?;
		self.storage.edges.properties_mut(id).ok_or_else(|| atom_does_not_exist(id))
	}

	/// Removes atom for `Graph<Mutable>`.
	fn remove_atom(&mut self, id: AtomId) -> FerrosGrynnResult<()> {
		Graph::<Mutable>::remove_atom(self, id)
	}
}

impl QueryMaterialize for Graph<Mutable> {
	/// Returns query from for `Graph<Mutable>`.
	fn query_from<R: GraphRead>(source: &R) -> FerrosGrynnResult<Self> {
		copy_from_read(source, false)
	}
}

impl QueryMaterialize for Graph<Frozen> {
	/// Returns query from for `Graph<Frozen>`.
	fn query_from<R: GraphRead>(source: &R) -> FerrosGrynnResult<Self> {
		copy_from_read(source, false).map(Graph::freeze)
	}
}

impl<State> Debug for Graph<State> {
	/// Formats `Graph<State>` using the requested formatter.
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
	/// Compares this `Graph<State>` with another value for equality.
	fn eq(&self, other: &Self) -> bool {
		self.identity() == other.identity() && self.revision() == other.revision()
	}
}

impl<State> Eq for Graph<State> {}

impl<State> PartialOrd for Graph<State> {
	/// Compares this `Graph<State>` with another value when they are ordered.
	fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
		Some(self.cmp(other))
	}
}

impl<State> Ord for Graph<State> {
	/// Compares this `Graph<State>` with another value using its ordering
	/// contract.
	fn cmp(&self, other: &Self) -> Ordering {
		(self.identity(), self.revision()).cmp(&(other.identity(), other.revision()))
	}
}

impl<State> Hash for Graph<State> {
	/// Feeds this `Graph<State>` into the supplied hasher.
	fn hash<H: Hasher>(&self, state: &mut H) {
		self.identity().hash(state);
		self.revision().hash(state);
	}
}

/// Returns the copy from read result.
fn copy_from_read<R: GraphRead>(
	source: &R,
	preserve_ids: bool,
) -> FerrosGrynnResult<Graph<Mutable>> {
	match copy_from_read_inner(source, preserve_ids, false)? {
		Some((graph, _estimated_bytes)) => Ok(graph),
		None => Err(FerrosGrynnError::invalid_argument(
			"nested graph detection escaped the flat-copy path",
		)),
	}
}

/// Returns the copy from read inner result.
fn copy_from_read_inner<R: GraphRead>(
	source: &R,
	preserve_ids: bool,
	reject_nested: bool,
) -> FerrosGrynnResult<Option<(Graph<Mutable>, usize)>> {
	let properties = source.properties()?.into_owned();
	let metadata = if preserve_ids {
		GraphMetadata::with_identity(source.identity(), source.revision(), properties)
	} else {
		GraphMetadata::with_identity(Uuid::now_v7(), 0, properties)
	};
	let mut result = Graph {
		metadata,
		storage: GeneralStorage::with_capacity(source.node_count()?, source.edge_count()?),
		state: PhantomData,
	};
	let mut ids = BTreeMap::new();
	let mut source_atoms = BTreeMap::new();
	let mut variable_bytes = 0_usize;

	for source_id in source.node_ids()? {
		let source_id = source_id?;
		let node = source.node(source_id)?.ok_or_else(|| atom_does_not_exist(source_id))?;
		let inner_graph = if reject_nested {
			if node.inner_graph_ref()?.is_some() {
				return Ok(None);
			}
			None
		} else {
			node.inner_graph()?.map(Cow::into_owned)
		};
		let record_id = node.record_id()?.into_owned();
		let label = node.label_text()?.map(Cow::into_owned);
		let properties = node.properties()?.into_owned();
		if reject_nested {
			variable_bytes = variable_bytes.saturating_add(estimated_node_fields_heap_bytes(
				&record_id,
				label.as_ref(),
				&properties,
				None,
			)?);
		}
		let local_id = if preserve_ids {
			result.storage.insert_node_at(source_id, record_id, label, properties, inner_graph)?;
			source_id
		} else {
			result.storage.insert_node_new(record_id, label, properties, inner_graph)?
		};
		ids.insert(source_id, local_id);
		source_atoms.insert(local_id, node.atom_ref()?);
	}

	for source_id in source.edge_ids()? {
		let source_id = source_id?;
		let edge = source.edge(source_id)?.ok_or_else(|| atom_does_not_exist(source_id))?;
		let transition_graph = if reject_nested {
			if edge.transition_graph_ref()?.is_some() {
				return Ok(None);
			}
			None
		} else {
			edge.transition_graph()?.map(Cow::into_owned)
		};
		let topology = remap_topology(edge.topology()?.as_ref(), &ids)?;
		result.storage.validate_topology(&topology)?;
		let record_id = edge.record_id()?.into_owned();
		let label = edge.label_text()?.map(Cow::into_owned);
		let properties = edge.properties()?.into_owned();
		if reject_nested {
			variable_bytes = variable_bytes.saturating_add(estimated_edge_fields_heap_bytes(
				&record_id,
				label.as_ref(),
				&properties,
				&topology,
				None,
			)?);
		}
		let local_id = if preserve_ids {
			result.storage.insert_edge_at(
				source_id,
				record_id,
				topology,
				label,
				properties,
				edge.weight(),
				transition_graph,
			)?;
			source_id
		} else {
			result.storage.insert_edge_new(
				record_id,
				topology,
				label,
				properties,
				edge.weight(),
				transition_graph,
			)?
		};
		ids.insert(source_id, local_id);
		source_atoms.insert(local_id, edge.atom_ref()?);
	}

	if !preserve_ids {
		result.metadata.set_origin(MaterializationOrigin::new(source, source_atoms));
	}
	let estimated_bytes = if reject_nested {
		estimated_graph_fixed_heap_bytes(
			size_of::<MaterializedGraph>(),
			result.slot_capacity(AtomKind::Node),
			result.slot_capacity(AtomKind::Edge),
			result.properties(),
		)
		.saturating_add(variable_bytes)
		.saturating_add(
			result.materialization_origin().map_or(0, MaterializationOrigin::estimated_heap_bytes),
		)
	} else {
		0
	};
	Ok(Some((result, estimated_bytes)))
}

/// Returns the remap topology result.
///
/// # Errors
/// Propagates validation, resource-admission or callback errors from the
/// operation.
pub fn remap_topology(
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

/// Returns the atom does not exist result.
pub(super) fn atom_does_not_exist(id: AtomId) -> FerrosGrynnError {
	FerrosGrynnError::atom_does_not_exist(
		id.kind().map_or_else(|| "null".to_owned(), |kind| kind.to_string()),
		id.to_string(),
	)
}

/// Returns the atom kind mismatch result.
pub(super) fn atom_kind_mismatch(id: AtomId, expected: AtomKind) -> FerrosGrynnError {
	FerrosGrynnError::atom_kind_mismatch(
		id.to_string(),
		id.kind().map_or_else(|| "null".to_owned(), |kind| kind.to_string()),
		expected.to_string(),
	)
}

/// Returns the endpoint kind result.
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

/// Operations shared with the engine and value codecs.
pub trait GraphMutableInternal: Sized {
	/// Starts a restored live result with its exact identity.
	fn begin_live_materialization(
		identity: Uuid,
		revision: u64,
		properties: PropertyMap,
		nodes: usize,
		edges: usize,
	) -> Self;
	/// Replaces edge topology.
	#[cfg(any(test, feature = "test-support"))]
	/// # Errors
	/// Propagates validation, resource-admission or callback errors from the
	/// operation.
	fn replace_edge_topology(
		&mut self,
		id: AtomId,
		topology: EdgeTopology,
	) -> FerrosGrynnResult<()>;
	/// Sets topology constraints.
	fn set_topology_constraints(&mut self, constraints: GraphTopologyConstraints);
	/// Returns the query flat with heap estimate result.
	///
	/// # Errors
	/// Propagates validation, resource-admission or callback errors from the
	/// operation.
	fn query_flat_with_heap_estimate<R: GraphRead>(
		source: &R,
	) -> FerrosGrynnResult<Option<(Self, usize)>>;
	/// Starts an owned query result without recording construction as mutation.
	fn begin_query_materialization(properties: PropertyMap, nodes: usize, edges: usize) -> Self;
	/// Inserts one fully decoded node into a query result.
	///
	/// # Errors
	/// Propagates validation, resource-admission or callback errors from the
	/// operation.
	fn materialize_query_node(
		&mut self,
		record_id: RecordId,
		label: Option<Text>,
		properties: PropertyMap,
		inner: Option<Graph<Frozen>>,
	) -> FerrosGrynnResult<AtomId>;
	/// Reserves one decoded edge before all remapped endpoints are available.
	///
	/// # Errors
	/// Propagates validation, resource-admission or callback errors from the
	/// operation.
	fn reserve_query_edge(
		&mut self,
		record_id: RecordId,
		label: Option<Text>,
		properties: PropertyMap,
		weight: Option<Number>,
		transition: Option<Graph<Frozen>>,
	) -> FerrosGrynnResult<AtomId>;
	/// Publishes and validates the final topology of a reserved query edge.
	///
	/// # Errors
	/// Propagates validation, resource-admission or callback errors from the
	/// operation.
	fn materialize_query_edge_topology(
		&mut self,
		id: AtomId,
		topology: EdgeTopology,
	) -> FerrosGrynnResult<()>;
	/// Attaches owned logical mappings for global query results and restored
	/// images.
	fn finish_query_materialization_parts(
		&mut self,
		identity: Uuid,
		snapshot: SnapshotId,
		reference: Option<GraphRef>,
		source_atoms: BTreeMap<AtomId, AtomRef>,
	);
	/// Attaches the exact source snapshot and clears construction state.
	fn finish_query_materialization<R: GraphRead>(
		&mut self,
		source: &R,
		source_atoms: BTreeMap<AtomId, AtomRef>,
	);
}
impl GraphMutableInternal for Graph<Mutable> {
	fn begin_live_materialization(
		identity: Uuid,
		revision: u64,
		properties: PropertyMap,
		nodes: usize,
		edges: usize,
	) -> Self {
		Self::begin_live_materialization(identity, revision, properties, nodes, edges)
	}

	#[cfg(any(test, feature = "test-support"))]
	fn replace_edge_topology(
		&mut self,
		id: AtomId,
		topology: EdgeTopology,
	) -> FerrosGrynnResult<()> {
		<Graph<Mutable>>::replace_edge_topology(self, id, topology)
	}

	fn set_topology_constraints(&mut self, constraints: GraphTopologyConstraints) {
		<Graph<Mutable>>::set_topology_constraints(self, constraints);
	}

	fn query_flat_with_heap_estimate<R: GraphRead>(
		source: &R,
	) -> FerrosGrynnResult<Option<(Self, usize)>> {
		<Graph<Mutable>>::query_flat_with_heap_estimate::<R>(source)
	}

	fn begin_query_materialization(properties: PropertyMap, nodes: usize, edges: usize) -> Self {
		<Graph<Mutable>>::begin_query_materialization(properties, nodes, edges)
	}

	fn materialize_query_node(
		&mut self,
		record_id: RecordId,
		label: Option<Text>,
		properties: PropertyMap,
		inner: Option<Graph<Frozen>>,
	) -> FerrosGrynnResult<AtomId> {
		<Graph<Mutable>>::materialize_query_node(self, record_id, label, properties, inner)
	}

	fn reserve_query_edge(
		&mut self,
		record_id: RecordId,
		label: Option<Text>,
		properties: PropertyMap,
		weight: Option<Number>,
		transition: Option<Graph<Frozen>>,
	) -> FerrosGrynnResult<AtomId> {
		<Graph<Mutable>>::reserve_query_edge(self, record_id, label, properties, weight, transition)
	}

	fn materialize_query_edge_topology(
		&mut self,
		id: AtomId,
		topology: EdgeTopology,
	) -> FerrosGrynnResult<()> {
		<Graph<Mutable>>::materialize_query_edge_topology(self, id, topology)
	}

	fn finish_query_materialization_parts(
		&mut self,
		identity: Uuid,
		snapshot: SnapshotId,
		reference: Option<GraphRef>,
		source_atoms: BTreeMap<AtomId, AtomRef>,
	) {
		<Graph<Mutable>>::finish_query_materialization_parts(
			self,
			identity,
			snapshot,
			reference,
			source_atoms,
		);
	}

	fn finish_query_materialization<R: GraphRead>(
		&mut self,
		source: &R,
		source_atoms: BTreeMap<AtomId, AtomRef>,
	) {
		<Graph<Mutable>>::finish_query_materialization::<R>(self, source, source_atoms);
	}
}

/// Validation shared by persistence without exposing graph storage.
pub trait GraphStateInternal {
	/// Validates retained topology and owned child routes.
	///
	/// # Errors
	/// Propagates validation, resource-admission or callback errors from the
	/// operation.
	fn validate_owned_topology(&self) -> FerrosGrynnResult<()>;
}
impl<State> GraphStateInternal for Graph<State> {
	fn validate_owned_topology(&self) -> FerrosGrynnResult<()> {
		Graph::validate_owned_topology(self)
	}
}

#[cfg(test)]
/// Regression tests for general behavior.
mod tests {
	use super::Graph;
	#[cfg(test)]
	use crate::test_assertions::{AssertOption as _, AssertResult as _};
	use crate::{
		gql_status::Domain,
		values::{
			PropertyMap,
			RecordId,
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

	/// Verifies supports node edge edge edge and cross level incidence.
	#[test]
	fn supports_node_edge_edge_edge_and_cross_level_incidence() {
		let mut inner = Graph::<Mutable>::new();
		let inner_node = inner.add_node(Some("inner".into()), PropertyMap::new()).assert_ok();
		let mut graph = Graph::<Mutable>::new();
		let meta = graph
			.add_metavertex(inner.freeze(), Some("meta".into()), PropertyMap::new())
			.assert_ok();
		let right = graph.add_node(Some("right".into()), PropertyMap::new()).assert_ok();
		let edge = graph
			.add_edge(EdgeTopology::directed([meta], [right]), None, PropertyMap::new())
			.assert_ok();
		let connector = graph
			.add_edge(EdgeTopology::directed([edge], [right]), None, PropertyMap::new())
			.assert_ok();
		let nested = QualifiedAtomId::new(vec![meta], inner_node).assert_ok();
		let cross = graph
			.add_edge(
				EdgeTopology::directed([EdgeEndpoint::Nested(nested)], [EdgeEndpoint::Local(
					right,
				)]),
				None,
				PropertyMap::new(),
			)
			.assert_ok();

		assert!(graph.edge(connector).is_some());
		assert!(graph.edge(cross).assert_some().topology().is_cross_level());
		assert_eq!(graph.incident_edges(edge).collect::<Vec<_>>(), [connector]);
	}

	/// Verifies universal graph allows directed self loops.
	#[test]
	fn universal_graph_allows_directed_self_loops() {
		let mut graph = Graph::<Mutable>::new();
		let node = graph.add_node(None, PropertyMap::new()).assert_ok();
		let edge = graph
			.add_edge(EdgeTopology::directed([node], [node]), None, PropertyMap::new())
			.assert_ok();
		assert_eq!(
			graph.edge(edge).assert_some().topology().local_binary(),
			Some((node, node, EdgeDirection::Directed,))
		);
	}

	/// Verifies removal cascades through edge endpoints and reuses generation.
	#[test]
	fn removal_cascades_through_edge_endpoints_and_reuses_generation() {
		let mut graph = Graph::<Mutable>::new();
		let left = graph.add_node(None, PropertyMap::new()).assert_ok();
		let right = graph.add_node(None, PropertyMap::new()).assert_ok();
		let edge = graph
			.add_edge(EdgeTopology::directed([left], [right]), None, PropertyMap::new())
			.assert_ok();
		let connector = graph
			.add_edge(EdgeTopology::directed([edge], [right]), None, PropertyMap::new())
			.assert_ok();

		graph.remove_atom(edge).assert_ok();
		assert!(!graph.contains(edge));
		assert!(!graph.contains(connector));
		let replacement = graph
			.add_edge(EdgeTopology::directed([left], [right]), None, PropertyMap::new())
			.assert_ok();
		assert_eq!(replacement.slot(), connector.slot());
		assert_ne!(replacement.generation(), connector.generation());
	}

	/// Verifies invalid edges are transactional and use graph statuses.
	#[test]
	fn invalid_edges_are_transactional_and_use_graph_statuses() {
		let mut graph = Graph::<Mutable>::new();
		let unknown = super::AtomId::from_raw(42);
		let revision = graph.revision();
		let error = graph
			.add_edge(EdgeTopology::undirected([unknown]), None, PropertyMap::new())
			.assert_err();

		assert_eq!(error.status().code().to_string(), "FG-G2F02");
		assert_eq!(error.status().domain(), Domain::Graph);
		assert_eq!(graph.revision(), revision);
		assert_eq!(graph.edge_count(), 0);
	}

	/// Verifies freeze thaw preserves identity ids and topology.
	#[test]
	fn freeze_thaw_preserves_identity_ids_and_topology() {
		let mut graph = Graph::<Mutable>::new();
		let left = graph.add_node(None, PropertyMap::new()).assert_ok();
		let right = graph.add_node(None, PropertyMap::new()).assert_ok();
		let edge = graph
			.add_edge(EdgeTopology::bidirectional([left], [right]), None, PropertyMap::new())
			.assert_ok();
		let identity = graph.identity();
		let revision = graph.revision();

		let graph: Graph<Frozen> = graph.freeze();
		let graph = graph.thaw();
		assert_eq!(graph.identity(), identity);
		assert_eq!(graph.revision(), revision);
		assert!(graph.contains(left));
		assert!(graph.contains(edge));
	}

	/// Verifies record index rejects cross kind duplicates and releases removed
	/// ids.
	#[test]
	fn record_index_rejects_cross_kind_duplicates_and_releases_removed_ids() {
		let mut graph = Graph::<Mutable>::new();
		let left = graph.add_node(None, PropertyMap::new()).assert_ok();
		let right = graph.add_node(None, PropertyMap::new()).assert_ok();
		let record = "atom:stable".parse::<RecordId>().assert_ok();
		let duplicate =
			graph.add_node_with_id(record.clone(), None, PropertyMap::new()).assert_ok();

		assert!(
			graph
				.add_edge_with_id(
					record.clone(),
					EdgeTopology::directed([left], [right]),
					None,
					PropertyMap::new(),
				)
				.is_err()
		);
		graph.remove_atom(duplicate).assert_ok();
		assert!(
			graph
				.add_edge_with_id(
					record,
					EdgeTopology::directed([left], [right]),
					None,
					PropertyMap::new(),
				)
				.is_ok()
		);
	}
}
