use std::{collections::BTreeSet, sync::Arc};

use rapidhash::{HashSetExt, RapidHashSet};

use super::{
	AtomId,
	AtomKind,
	DirtyTarget,
	EdgeTopology,
	GraphRef,
	GraphStorageMode,
	GraphTopologyConstraints,
	MaterializationOrigin,
	id::SlotAllocator,
};
use crate::{
	collections::cow_vec::CowVec,
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	values::runtime::{Number, PropertyMap, RecordId, RecordIdPart, Text, Ulid, Uuid},
};

#[derive(Debug, Clone)]
/// Graph metadata state.
pub(super) struct GraphMetadata {
	/// Identity associated with this graph metadata.
	identity: Uuid,
	/// Revision identifying this version of the data.
	revision: u64,
	/// Properties associated with this graph metadata.
	properties: PropertyMap,
	/// Topology constraints associated with this graph metadata.
	topology_constraints: GraphTopologyConstraints,
	/// Origin associated with this graph metadata.
	origin: Option<MaterializationOrigin>,
	/// Dirty associated with this graph metadata.
	dirty: BTreeSet<DirtyTarget>,
}

impl GraphMetadata {
	/// Creates a new `GraphMetadata` value.
	pub(crate) fn new() -> Self {
		Self {
			identity: Uuid::now_v7(),
			revision: 0,
			properties: PropertyMap::new(),
			topology_constraints: GraphTopologyConstraints::empty(),
			origin: None,
			dirty: BTreeSet::new(),
		}
	}

	/// Configures identity.
	pub(crate) const fn with_identity(
		identity: Uuid,
		revision: u64,
		properties: PropertyMap,
	) -> Self {
		Self {
			identity,
			revision,
			properties,
			topology_constraints: GraphTopologyConstraints::empty(),
			origin: None,
			dirty: BTreeSet::new(),
		}
	}

	/// Returns the identity result.
	pub(crate) const fn identity(&self) -> Uuid {
		self.identity
	}

	/// Returns the revision result.
	pub(crate) const fn revision(&self) -> u64 {
		self.revision
	}

	/// Returns the properties result.
	pub(crate) const fn properties(&self) -> &PropertyMap {
		&self.properties
	}

	/// Returns the topology constraints result.
	pub(crate) const fn topology_constraints(&self) -> GraphTopologyConstraints {
		self.topology_constraints
	}

	/// Sets topology constraints.
	pub(crate) const fn set_topology_constraints(&mut self, constraints: GraphTopologyConstraints) {
		self.topology_constraints = constraints;
	}

	/// Returns the properties mut result.
	pub(crate) fn properties_mut(&mut self) -> FerrosGrynnResult<&mut PropertyMap> {
		self.bump(Some(DirtyTarget::Graph))?;
		Ok(&mut self.properties)
	}

	/// Ensures can bump.
	pub(crate) fn ensure_can_bump(&self) -> FerrosGrynnResult<()> {
		self.revision
			.checked_add(1)
			.map(|_| ())
			.ok_or_else(|| FerrosGrynnError::graph_capacity_exceeded("revision".to_owned()))
	}

	/// Returns the bump result.
	pub(crate) fn bump(&mut self, target: Option<DirtyTarget>) -> FerrosGrynnResult<()> {
		self.ensure_can_bump()?;
		self.revision += 1;
		if let Some(target) = target {
			self.dirty.insert(target);
		}
		Ok(())
	}

	/// Marks dirty.
	pub(crate) fn mark_dirty(&mut self, target: DirtyTarget) {
		self.dirty.insert(target);
	}

	/// Sets origin.
	pub(crate) fn set_origin(&mut self, origin: MaterializationOrigin) {
		self.origin = Some(origin);
		self.dirty.clear();
	}

	/// Returns the origin result.
	pub(crate) const fn origin(&self) -> Option<&MaterializationOrigin> {
		self.origin.as_ref()
	}

	/// Returns the dirty result.
	pub(crate) fn dirty(&self) -> impl ExactSizeIterator<Item = &DirtyTarget> {
		self.dirty.iter()
	}

	/// Clears dirty.
	pub(crate) fn clear_dirty(&mut self) {
		self.dirty.clear();
	}

	/// Returns the storage mode result.
	pub(crate) const fn storage_mode(&self) -> GraphStorageMode {
		if self.origin.is_some() {
			GraphStorageMode::Hybrid
		} else {
			GraphStorageMode::InMemory
		}
	}

	/// Returns the graph ref result.
	pub(crate) const fn graph_ref(&self) -> Option<&GraphRef> {
		match &self.origin {
			Some(origin) => origin.graph_ref(),
			None => None,
		}
	}

	/// Performs the adopt external revision operation.
	pub(crate) fn adopt_external_revision(&mut self, revision: u64) {
		self.revision = revision;
		self.dirty.clear();
	}

	/// Converts this value into frozen.
	pub(crate) fn into_frozen(mut self) -> Self {
		self.dirty.clear();
		self
	}
}

impl Default for GraphMetadata {
	/// Constructs the default initial state of `GraphMetadata`.
	fn default() -> Self {
		Self::new()
	}
}

#[derive(Debug, Clone)]
/// Node columns state.
pub(super) struct NodeColumns {
	/// Allocator associated with this node columns.
	allocator: SlotAllocator,
	/// Live associated with this node columns.
	live: CowVec<AtomId>,
	/// Dense positions associated with this node columns.
	dense_positions: CowVec<Option<usize>>,
	/// Record ids associated with this node columns.
	record_ids: CowVec<Option<RecordId>>,
	/// Record index associated with this node columns.
	record_index: Arc<RapidHashSet<RecordId>>,
	/// Labels associated with this node columns.
	labels: CowVec<Option<Text>>,
	/// Properties associated with this node columns.
	properties: CowVec<PropertyMap>,
}

impl NodeColumns {
	/// Creates a new `NodeColumns` value.
	pub(crate) fn new() -> Self {
		Self {
			allocator: SlotAllocator::new(AtomKind::Node),
			live: CowVec::new(),
			dense_positions: CowVec::new(),
			record_ids: CowVec::new(),
			record_index: Arc::new(RapidHashSet::default()),
			labels: CowVec::new(),
			properties: CowVec::new(),
		}
	}

	/// Configures capacity.
	pub(crate) fn with_capacity(capacity: usize) -> Self {
		Self {
			allocator: SlotAllocator::with_capacity(AtomKind::Node, capacity),
			live: CowVec::with_capacity(capacity),
			dense_positions: CowVec::with_capacity(capacity),
			record_ids: CowVec::with_capacity(capacity),
			record_index: Arc::new(RapidHashSet::with_capacity(capacity)),
			labels: CowVec::with_capacity(capacity),
			properties: CowVec::with_capacity(capacity),
		}
	}

	/// Inserts new.
	pub(crate) fn insert_new(
		&mut self,
		record_id: RecordId,
		label: Option<Text>,
		properties: PropertyMap,
	) -> FerrosGrynnResult<AtomId> {
		let id = self.allocator.alloc()?;
		self.populate(id, record_id, label, properties);
		Ok(id)
	}

	/// Inserts at.
	pub(crate) fn insert_at(
		&mut self,
		id: AtomId,
		record_id: RecordId,
		label: Option<Text>,
		properties: PropertyMap,
	) -> FerrosGrynnResult<()> {
		self.allocator.restore(id)?;
		self.populate(id, record_id, label, properties);
		Ok(())
	}

	/// Returns the slot capacity result.
	pub(crate) fn slot_capacity(&self) -> usize {
		self.allocator.capacity()
	}

	/// Removes node columns.
	pub(crate) fn remove(&mut self, id: AtomId) -> bool {
		if !self.allocator.free(id) {
			return false;
		}
		let index = id.slot() as usize;
		if let Some(record_id) = self.record_ids[index].take() {
			Arc::make_mut(&mut self.record_index).remove(&record_id);
		}
		self.labels[index] = None;
		self.properties[index].clear();
		let Some(dense) = self.dense_positions[index].take() else {
			return true;
		};
		self.live.swap_remove(dense);
		if let Some(moved) = self.live.get(dense) {
			self.dense_positions[moved.slot() as usize] = Some(dense);
		}
		true
	}

	/// Checks for node columns.
	pub(crate) fn contains(&self, id: AtomId) -> bool {
		self.allocator.contains(id)
	}

	/// Returns the ids result.
	pub(crate) const fn ids(&self) -> &CowVec<AtomId> {
		&self.live
	}

	/// Returns the number of logical elements.
	pub(crate) const fn len(&self) -> usize {
		self.allocator.live_len()
	}

	/// Records id.
	pub(crate) fn record_id(&self, id: AtomId) -> Option<&RecordId> {
		self.contains(id).then(|| self.record_ids.get(id.slot() as usize)?.as_ref())?
	}

	/// Borrows the label slot of a live node; `None` means a stale or foreign
	/// ID. A borrowed `None` slot means the live node has no label.
	pub(crate) fn label(&self, id: AtomId) -> Option<&Option<Text>> {
		self.contains(id).then(|| self.labels.get(id.slot() as usize))?
	}

	/// Returns the properties result.
	pub(crate) fn properties(&self, id: AtomId) -> Option<&PropertyMap> {
		self.contains(id).then(|| self.properties.get(id.slot() as usize))?
	}

	/// Returns the properties mut result.
	pub(crate) fn properties_mut(&mut self, id: AtomId) -> Option<&mut PropertyMap> {
		self.contains(id).then(|| self.properties.get_mut(id.slot() as usize))?
	}

	/// Returns the label mut result.
	pub(crate) fn label_mut(&mut self, id: AtomId) -> Option<&mut Option<Text>> {
		self.contains(id).then(|| self.labels.get_mut(id.slot() as usize))?
	}

	/// Checks for record.
	pub(crate) fn contains_record(&self, record_id: &RecordId) -> bool {
		self.record_index.contains(record_id)
	}

	/// Performs the populate operation.
	fn populate(
		&mut self,
		id: AtomId,
		record_id: RecordId,
		label: Option<Text>,
		properties: PropertyMap,
	) {
		self.ensure_slot(id.slot() as usize);
		let index = id.slot() as usize;
		let inserted = Arc::make_mut(&mut self.record_index).insert(record_id.clone());
		debug_assert!(inserted, "record ID uniqueness must be checked before insertion");
		self.record_ids[index] = Some(record_id);
		self.labels[index] = label;
		self.properties[index] = properties;
		self.dense_positions[index] = Some(self.live.len());
		self.live.push(id);
	}

	/// Ensures slot.
	fn ensure_slot(&mut self, index: usize) {
		while self.record_ids.len() <= index {
			self.record_ids.push(None);
			self.labels.push(None);
			self.properties.push(PropertyMap::new());
			self.dense_positions.push(None);
		}
	}
}

impl Default for NodeColumns {
	/// Constructs the default initial state of `NodeColumns`.
	fn default() -> Self {
		Self::new()
	}
}

#[derive(Debug, Clone)]
/// Edge columns state.
pub(super) struct EdgeColumns {
	/// Allocator associated with this edge columns.
	allocator: SlotAllocator,
	/// Live associated with this edge columns.
	live: CowVec<AtomId>,
	/// Dense positions associated with this edge columns.
	dense_positions: CowVec<Option<usize>>,
	/// Record ids associated with this edge columns.
	record_ids: CowVec<Option<RecordId>>,
	/// Record index associated with this edge columns.
	record_index: Arc<RapidHashSet<RecordId>>,
	/// Topologies associated with this edge columns.
	topologies: CowVec<EdgeTopology>,
	/// Labels associated with this edge columns.
	labels: CowVec<Option<Text>>,
	/// Properties associated with this edge columns.
	properties: CowVec<PropertyMap>,
	/// Weights associated with this edge columns.
	weights: CowVec<Option<Number>>,
}

impl EdgeColumns {
	/// Creates a new `EdgeColumns` value.
	pub(crate) fn new() -> Self {
		Self {
			allocator: SlotAllocator::new(AtomKind::Edge),
			live: CowVec::new(),
			dense_positions: CowVec::new(),
			record_ids: CowVec::new(),
			record_index: Arc::new(RapidHashSet::default()),
			topologies: CowVec::new(),
			labels: CowVec::new(),
			properties: CowVec::new(),
			weights: CowVec::new(),
		}
	}

	/// Configures capacity.
	pub(crate) fn with_capacity(capacity: usize) -> Self {
		Self {
			allocator: SlotAllocator::with_capacity(AtomKind::Edge, capacity),
			live: CowVec::with_capacity(capacity),
			dense_positions: CowVec::with_capacity(capacity),
			record_ids: CowVec::with_capacity(capacity),
			record_index: Arc::new(RapidHashSet::with_capacity(capacity)),
			topologies: CowVec::with_capacity(capacity),
			labels: CowVec::with_capacity(capacity),
			properties: CowVec::with_capacity(capacity),
			weights: CowVec::with_capacity(capacity),
		}
	}

	/// Inserts new.
	pub(crate) fn insert_new(
		&mut self,
		record_id: RecordId,
		topology: EdgeTopology,
		label: Option<Text>,
		properties: PropertyMap,
		weight: Option<Number>,
	) -> FerrosGrynnResult<AtomId> {
		let id = self.allocator.alloc()?;
		self.populate(id, record_id, topology, label, properties, weight);
		Ok(id)
	}

	/// Inserts at.
	pub(crate) fn insert_at(
		&mut self,
		id: AtomId,
		record_id: RecordId,
		topology: EdgeTopology,
		label: Option<Text>,
		properties: PropertyMap,
		weight: Option<Number>,
	) -> FerrosGrynnResult<()> {
		self.allocator.restore(id)?;
		self.populate(id, record_id, topology, label, properties, weight);
		Ok(())
	}

	/// Returns the slot capacity result.
	pub(crate) fn slot_capacity(&self) -> usize {
		self.allocator.capacity()
	}

	/// Removes edge columns.
	pub(crate) fn remove(&mut self, id: AtomId) -> bool {
		if !self.allocator.free(id) {
			return false;
		}
		let index = id.slot() as usize;
		if let Some(record_id) = self.record_ids[index].take() {
			Arc::make_mut(&mut self.record_index).remove(&record_id);
		}
		self.topologies[index] = empty_topology();
		self.labels[index] = None;
		self.properties[index].clear();
		self.weights[index] = None;
		let Some(dense) = self.dense_positions[index].take() else {
			return true;
		};
		self.live.swap_remove(dense);
		if let Some(moved) = self.live.get(dense) {
			self.dense_positions[moved.slot() as usize] = Some(dense);
		}
		true
	}

	/// Checks for edge columns.
	pub(crate) fn contains(&self, id: AtomId) -> bool {
		self.allocator.contains(id)
	}

	/// Returns the ids result.
	pub(crate) const fn ids(&self) -> &CowVec<AtomId> {
		&self.live
	}

	/// Returns the number of logical elements.
	pub(crate) const fn len(&self) -> usize {
		self.allocator.live_len()
	}

	/// Records id.
	pub(crate) fn record_id(&self, id: AtomId) -> Option<&RecordId> {
		self.contains(id).then(|| self.record_ids.get(id.slot() as usize)?.as_ref())?
	}

	/// Returns the topology result.
	pub(crate) fn topology(&self, id: AtomId) -> Option<&EdgeTopology> {
		self.contains(id).then(|| self.topologies.get(id.slot() as usize))?
	}

	/// Returns the topology mut result.
	pub(crate) fn topology_mut(&mut self, id: AtomId) -> Option<&mut EdgeTopology> {
		self.contains(id).then(|| self.topologies.get_mut(id.slot() as usize))?
	}

	/// Borrows the label slot of a live edge; `None` means a stale or foreign
	/// ID. A borrowed `None` slot means the live edge has no label.
	pub(crate) fn label(&self, id: AtomId) -> Option<&Option<Text>> {
		self.contains(id).then(|| self.labels.get(id.slot() as usize))?
	}

	/// Returns the properties result.
	pub(crate) fn properties(&self, id: AtomId) -> Option<&PropertyMap> {
		self.contains(id).then(|| self.properties.get(id.slot() as usize))?
	}

	/// Returns the properties mut result.
	pub(crate) fn properties_mut(&mut self, id: AtomId) -> Option<&mut PropertyMap> {
		self.contains(id).then(|| self.properties.get_mut(id.slot() as usize))?
	}

	/// Returns the optional label slot only while `id` identifies a live edge.
	pub(crate) fn label_mut(&mut self, id: AtomId) -> Option<&mut Option<Text>> {
		self.contains(id).then(|| self.labels.get_mut(id.slot() as usize))?
	}

	/// Borrows the weight slot of a live edge; `None` means a stale or foreign
	/// ID. A borrowed `None` slot means the live edge has no stored weight.
	pub(crate) fn weight(&self, id: AtomId) -> Option<&Option<Number>> {
		self.contains(id).then(|| self.weights.get(id.slot() as usize))?
	}

	/// Returns the optional weight slot only while `id` identifies a live edge.
	pub(crate) fn weight_mut(&mut self, id: AtomId) -> Option<&mut Option<Number>> {
		self.contains(id).then(|| self.weights.get_mut(id.slot() as usize))?
	}

	/// Checks for record.
	pub(crate) fn contains_record(&self, record_id: &RecordId) -> bool {
		self.record_index.contains(record_id)
	}

	/// Performs the populate operation.
	fn populate(
		&mut self,
		id: AtomId,
		record_id: RecordId,
		topology: EdgeTopology,
		label: Option<Text>,
		properties: PropertyMap,
		weight: Option<Number>,
	) {
		self.ensure_slot(id.slot() as usize);
		let index = id.slot() as usize;
		let inserted = Arc::make_mut(&mut self.record_index).insert(record_id.clone());
		debug_assert!(inserted, "record ID uniqueness must be checked before insertion");
		self.record_ids[index] = Some(record_id);
		self.topologies[index] = topology;
		self.labels[index] = label;
		self.properties[index] = properties;
		self.weights[index] = weight;
		self.dense_positions[index] = Some(self.live.len());
		self.live.push(id);
	}

	/// Ensures slot.
	fn ensure_slot(&mut self, index: usize) {
		while self.record_ids.len() <= index {
			self.record_ids.push(None);
			self.topologies.push(empty_topology());
			self.labels.push(None);
			self.properties.push(PropertyMap::new());
			self.weights.push(None);
			self.dense_positions.push(None);
		}
	}
}

impl Default for EdgeColumns {
	/// Constructs the default initial state of `EdgeColumns`.
	fn default() -> Self {
		Self::new()
	}
}

/// Returns the generated record id result.
pub(super) fn generated_record_id(kind: AtomKind) -> FerrosGrynnResult<RecordId> {
	RecordId::new(kind.to_string(), RecordIdPart::Ulid(Ulid::new()))
}

/// Ensures unique record.
pub(super) fn ensure_unique_record(
	nodes: &NodeColumns,
	edges: &EdgeColumns,
	record_id: &RecordId,
) -> FerrosGrynnResult<()> {
	if nodes.contains_record(record_id) || edges.contains_record(record_id) {
		return Err(FerrosGrynnError::graph_constraint_violation(
			"graph",
			format!("record ID {record_id} is already assigned to another atom"),
		));
	}
	Ok(())
}

/// Returns the empty topology result.
fn empty_topology() -> EdgeTopology {
	EdgeTopology::undirected(Vec::<AtomId>::new())
}
