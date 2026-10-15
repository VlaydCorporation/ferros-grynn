use std::{
	borrow::Cow,
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
	EdgeRead,
	EdgeTopology,
	EdgeView,
	FallibleAtomIds,
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
	general::{atom_does_not_exist, atom_kind_mismatch},
	storage::{EdgeColumns, GraphMetadata, NodeColumns, ensure_unique_record, generated_record_id},
};
use crate::{
	collections::cow_vec::CowVec,
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	values::runtime::{Number, PropertyMap, RecordId, Text, Uuid},
};

#[derive(Debug, Clone)]
/// Hyper storage state.
struct HyperStorage {
	/// Nodes associated with this hyper storage.
	nodes: NodeColumns,
	/// Edges associated with this hyper storage.
	edges: EdgeColumns,
	/// Incidence associated with this hyper storage.
	incidence: CowVec<Vec<AtomId>>,
}

impl HyperStorage {
	/// Creates a new `HyperStorage` value.
	fn new() -> Self {
		Self {
			nodes: NodeColumns::new(),
			edges: EdgeColumns::new(),
			incidence: CowVec::new(),
		}
	}

	/// Inserts node new.
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

	/// Inserts node at.
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

	/// Inserts edge new.
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

	/// Inserts edge at.
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

	/// Validates topology.
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

	/// Registers hyper storage.
	fn register(&mut self, edge: AtomId) {
		if let Some(topology) = self.edges.topology(edge) {
			for node in topology.endpoints().filter_map(EdgeEndpoint::local_id) {
				self.incidence[node.slot() as usize].push(edge);
			}
		}
	}

	/// Removes edge.
	fn remove_edge(&mut self, id: AtomId) -> bool {
		let Some(topology) = self.edges.topology(id).cloned() else {
			return false;
		};
		for node in topology.endpoints().filter_map(EdgeEndpoint::local_id) {
			self.incidence[node.slot() as usize].retain(|edge| *edge != id);
		}
		self.edges.remove(id)
	}

	/// Removes node.
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

	/// Returns the incident edges result.
	fn incident_edges(&self, id: AtomId) -> &[AtomId] {
		if !self.nodes.contains(id) {
			return &[];
		}
		self.incidence.get(id.slot() as usize).map_or(&[], Vec::as_slice)
	}

	/// Ensures node slot.
	fn ensure_node_slot(&mut self, id: AtomId) {
		let slot = id.slot() as usize;
		while self.incidence.len() <= slot {
			self.incidence.push(Vec::new());
		}
	}
}

/// Graph-family materialization supporting multi-endpoint edge topology.
pub struct Hypergraph<State = Frozen> {
	/// Metadata associated with this hypergraph.
	metadata: GraphMetadata,
	/// Storage associated with this hypergraph.
	storage: HyperStorage,
	/// State associated with this hypergraph.
	state: PhantomData<State>,
}

pub type MutableHypergraph = Hypergraph<Mutable>;
pub type FrozenHypergraph = Hypergraph<Frozen>;

impl Hypergraph<Mutable> {
	/// Constructs `Hypergraph<Mutable>` from the supplied initial state.
	pub fn new() -> Self {
		Self {
			metadata: GraphMetadata::new(),
			storage: HyperStorage::new(),
			state: PhantomData,
		}
	}

	/// Returns add node for `Hypergraph<Mutable>`.
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

	/// Returns the add node with id result.
	pub(crate) fn add_node_with_id(
		&mut self,
		record_id: RecordId,
		label: Option<Text>,
		properties: PropertyMap,
	) -> FerrosGrynnResult<AtomId> {
		self.metadata.ensure_can_bump()?;
		let id = self.storage.insert_node_new(record_id, label, properties)?;
		self.metadata.bump(Some(DirtyTarget::Atom(id)))?;
		Ok(id)
	}

	/// Returns add edge for `Hypergraph<Mutable>`.
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

	/// Returns the add edge with id result.
	pub(crate) fn add_edge_with_id(
		&mut self,
		record_id: RecordId,
		topology: EdgeTopology,
		label: Option<Text>,
		properties: PropertyMap,
	) -> FerrosGrynnResult<AtomId> {
		self.metadata.ensure_can_bump()?;
		let id = self.storage.insert_edge_new(topology, record_id, label, properties, None)?;
		self.metadata.bump(Some(DirtyTarget::Atom(id)))?;
		Ok(id)
	}

	/// Returns freeze for `Hypergraph<Mutable>`.
	pub fn freeze(self) -> Hypergraph<Frozen> {
		Hypergraph {
			metadata: self.metadata.into_frozen(),
			storage: self.storage,
			state: PhantomData,
		}
	}

	/// Clones the COW layout while retaining identity, revision and dirty
	/// metadata.
	pub(crate) fn clone_preserving_state(&self) -> Self {
		Self {
			metadata: self.metadata.clone(),
			storage: self.storage.clone(),
			state: PhantomData,
		}
	}

	/// Adopts an accepted remote watermark without creating a local edit.
	pub(crate) fn adopt_external_revision(&mut self, revision: u64) {
		self.metadata.adopt_external_revision(revision);
	}

	/// Clears local dirty metadata after accepting the remote base.
	pub(crate) fn clear_live_dirty(&mut self) {
		self.metadata.clear_dirty();
	}

	/// Performs the flush operation for `Hypergraph<Mutable>`.
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

	/// Returns the transaction dirty result.
	#[cfg(test)]
	pub(crate) fn transaction_dirty(&self) -> Vec<DirtyTarget> {
		self.metadata.dirty().cloned().collect()
	}
}

impl Default for Hypergraph<Mutable> {
	/// Constructs the default initial state of `Hypergraph<Mutable>`.
	fn default() -> Self {
		Self::new()
	}
}

impl Hypergraph<Frozen> {
	/// Returns thaw for `Hypergraph<Frozen>`.
	pub fn thaw(self) -> Hypergraph<Mutable> {
		Hypergraph {
			metadata: self.metadata,
			storage: self.storage,
			state: PhantomData,
		}
	}
}

impl Clone for Hypergraph<Frozen> {
	/// Returns an owned clone of this `Hypergraph<Frozen>`.
	fn clone(&self) -> Self {
		Self {
			metadata: self.metadata.clone(),
			storage: self.storage.clone(),
			state: PhantomData,
		}
	}
}

impl<State> Hypergraph<State> {
	/// Returns the slot capacity result.
	pub(crate) fn slot_capacity(&self, kind: AtomKind) -> usize {
		match kind {
			AtomKind::Node => self.storage.nodes.slot_capacity(),
			AtomKind::Edge => self.storage.edges.slot_capacity(),
		}
	}

	/// Returns identity for `Hypergraph<State>`.
	pub const fn identity(&self) -> Uuid {
		self.metadata.identity()
	}

	/// Returns revision for `Hypergraph<State>`.
	pub const fn revision(&self) -> u64 {
		self.metadata.revision()
	}

	/// Returns properties for `Hypergraph<State>`.
	pub const fn properties(&self) -> &PropertyMap {
		self.metadata.properties()
	}

	/// Returns topology constraints for `Hypergraph<State>`.
	pub const fn topology_constraints(&self) -> GraphTopologyConstraints {
		self.metadata.topology_constraints()
	}

	/// Returns node count for `Hypergraph<State>`.
	pub const fn node_count(&self) -> usize {
		self.storage.nodes.len()
	}

	/// Returns edge count for `Hypergraph<State>`.
	pub const fn edge_count(&self) -> usize {
		self.storage.edges.len()
	}

	/// Returns node for `Hypergraph<State>`.
	pub fn node(&self, id: AtomId) -> Option<NodeView<'_>> {
		Some(NodeView::new(
			id,
			self.storage.nodes.record_id(id)?,
			self.storage.nodes.label(id)?.as_ref(),
			self.storage.nodes.properties(id)?,
			None,
		))
	}

	/// Returns edge for `Hypergraph<State>`.
	pub fn edge(&self, id: AtomId) -> Option<EdgeView<'_>> {
		Some(EdgeView::new(
			id,
			self.storage.edges.record_id(id)?,
			self.storage.edges.topology(id)?,
			self.storage.edges.label(id)?.as_ref(),
			self.storage.edges.properties(id)?,
			self.storage.edges.weight(id)?.as_ref().copied(),
			None,
		))
	}

	/// Returns incident edges for `Hypergraph<State>`.
	pub fn incident_edges(&self, id: AtomId) -> &[AtomId] {
		self.storage.incident_edges(id)
	}

	/// Converts this value into graph for `Hypergraph<State>`.
	///
	/// # Errors
	///
	/// Propagates errors returned by the underlying operation.
	pub fn into_graph(self) -> FerrosGrynnResult<Graph<State>> {
		let converted = Graph::<Mutable>::from_read_preserving(&self)?;
		let (_, storage) = converted.into_raw();
		Ok(Graph::from_raw(self.metadata, storage))
	}
}

impl<State> GraphRead for Hypergraph<State> {
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

	/// Returns identity for `Hypergraph<State>`.
	fn identity(&self) -> Uuid {
		self.identity()
	}

	/// Returns revision for `Hypergraph<State>`.
	fn revision(&self) -> u64 {
		self.revision()
	}

	/// Returns kind for `Hypergraph<State>`.
	fn kind(&self) -> GraphKind {
		GraphKind::Hypergraph
	}

	/// Returns properties for `Hypergraph<State>`.
	fn properties(&self) -> FerrosGrynnResult<Cow<'_, PropertyMap>> {
		Ok(Cow::Borrowed(self.properties()))
	}

	/// Returns topology constraints for `Hypergraph<State>`.
	fn topology_constraints(&self) -> GraphTopologyConstraints {
		self.topology_constraints()
	}

	/// Returns node ids for `Hypergraph<State>`.
	fn node_ids(&self) -> FerrosGrynnResult<Self::NodeIds<'_>> {
		Ok(FallibleAtomIds::new(AtomIds::new(self.storage.nodes.ids().iter())))
	}

	/// Returns edge ids for `Hypergraph<State>`.
	fn edge_ids(&self) -> FerrosGrynnResult<Self::EdgeIds<'_>> {
		Ok(FallibleAtomIds::new(AtomIds::new(self.storage.edges.ids().iter())))
	}

	/// Returns node for `Hypergraph<State>`.
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
		Ok(Some(NodeView::new(id, record_id, label.as_ref(), properties, None)))
	}

	/// Returns edge for `Hypergraph<State>`.
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
			None,
		)))
	}

	/// Returns storage mode for `Hypergraph<State>`.
	fn storage_mode(&self) -> super::GraphStorageMode {
		self.metadata.storage_mode()
	}

	/// Returns graph ref for `Hypergraph<State>`.
	fn graph_ref(&self) -> Option<&super::GraphRef> {
		self.metadata.graph_ref()
	}

	/// Returns materialization origin for `Hypergraph<State>`.
	fn materialization_origin(&self) -> Option<&super::MaterializationOrigin> {
		self.metadata.origin()
	}
}

impl GraphMut for Hypergraph<Mutable> {
	/// Returns properties mut for `Hypergraph<Mutable>`.
	fn properties_mut(&mut self) -> FerrosGrynnResult<&mut PropertyMap> {
		self.metadata.properties_mut()
	}

	/// Returns node label mut for `Hypergraph<Mutable>`.
	fn node_label_mut(&mut self, id: AtomId) -> FerrosGrynnResult<&mut Option<Text>> {
		if id.kind() != Some(AtomKind::Node) {
			return Err(atom_kind_mismatch(id, AtomKind::Node));
		}
		if !self.storage.nodes.contains(id) {
			return Err(atom_does_not_exist(id));
		}
		self.metadata.bump(Some(DirtyTarget::Atom(id)))?;
		self.storage.nodes.label_mut(id).ok_or_else(|| atom_does_not_exist(id))
	}

	/// Returns node properties mut for `Hypergraph<Mutable>`.
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

	/// Returns edge label mut for `Hypergraph<Mutable>`.
	fn edge_label_mut(&mut self, id: AtomId) -> FerrosGrynnResult<&mut Option<Text>> {
		if id.kind() != Some(AtomKind::Edge) {
			return Err(atom_kind_mismatch(id, AtomKind::Edge));
		}
		if !self.storage.edges.contains(id) {
			return Err(atom_does_not_exist(id));
		}
		self.metadata.bump(Some(DirtyTarget::Atom(id)))?;
		self.storage.edges.label_mut(id).ok_or_else(|| atom_does_not_exist(id))
	}

	/// Returns edge properties mut for `Hypergraph<Mutable>`.
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

	/// Removes atom for `Hypergraph<Mutable>`.
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

	/// Converts `Graph<State>` into `Hypergraph<State>` after validating the
	/// conversion.
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
	/// Returns query from for `Hypergraph<Mutable>`.
	fn query_from<R: GraphRead>(source: &R) -> FerrosGrynnResult<Self> {
		Self::try_from(Graph::<Mutable>::query_from(source)?)
	}
}

impl QueryMaterialize for Hypergraph<Frozen> {
	/// Returns query from for `Hypergraph<Frozen>`.
	fn query_from<R: GraphRead>(source: &R) -> FerrosGrynnResult<Self> {
		Hypergraph::<Mutable>::query_from(source).map(Hypergraph::freeze)
	}
}

impl<State> Debug for Hypergraph<State> {
	/// Formats `Hypergraph<State>` using the requested formatter.
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
	/// Compares this `Hypergraph<State>` with another value for equality.
	fn eq(&self, other: &Self) -> bool {
		(self.identity(), self.revision()) == (other.identity(), other.revision())
	}
}

impl<State> Eq for Hypergraph<State> {}

impl<State> PartialOrd for Hypergraph<State> {
	/// Compares this `Hypergraph<State>` with another value when they are
	/// ordered.
	fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
		Some(self.cmp(other))
	}
}

impl<State> Ord for Hypergraph<State> {
	/// Compares this `Hypergraph<State>` with another value using its ordering
	/// contract.
	fn cmp(&self, other: &Self) -> Ordering {
		(self.identity(), self.revision()).cmp(&(other.identity(), other.revision()))
	}
}

impl<State> Hash for Hypergraph<State> {
	/// Feeds this `Hypergraph<State>` into the supplied hasher.
	fn hash<H: Hasher>(&self, state: &mut H) {
		self.identity().hash(state);
		self.revision().hash(state);
	}
}

/// Builds storage.
fn build_storage<R: GraphRead>(source: &R) -> FerrosGrynnResult<HyperStorage> {
	let mut storage = HyperStorage::new();
	for id in source.node_ids()? {
		let id = id?;
		let node = source.node(id)?.ok_or_else(|| atom_does_not_exist(id))?;
		if node.is_meta()? {
			return Err(constraint("hypergraph vertices cannot contain nested graphs"));
		}
		storage.insert_node_at(
			id,
			node.record_id()?.into_owned(),
			node.label_text()?.map(Cow::into_owned),
			node.properties()?.into_owned(),
		)?;
	}
	for id in source.edge_ids()? {
		let id = id?;
		let edge = source.edge(id)?.ok_or_else(|| atom_does_not_exist(id))?;
		if edge.is_meta()? {
			return Err(constraint("hypergraph edges cannot contain transition graphs"));
		}
		storage.insert_edge_at(
			id,
			edge.topology()?.into_owned(),
			edge.record_id()?.into_owned(),
			edge.label_text()?.map(Cow::into_owned),
			edge.properties()?.into_owned(),
			edge.weight(),
		)?;
	}
	Ok(storage)
}

/// Returns the constraint result.
fn constraint(reason: impl Into<String>) -> FerrosGrynnError {
	FerrosGrynnError::graph_constraint_violation("hypergraph", reason.into())
}

#[cfg(test)]
/// Regression tests for hypergraph behavior.
mod tests {
	use super::Hypergraph;
	#[cfg(test)]
	use crate::test_assertions::AssertResult as _;
	use crate::values::{
		PropertyMap,
		runtime::graph::{DirtyTarget, EdgeTopology, Graph, GraphMut, Mutable},
	};

	/// Verifies stores vertex hyperedge incidence.
	#[test]
	fn stores_vertex_hyperedge_incidence() {
		let mut graph = Hypergraph::<Mutable>::new();
		let first = graph.add_node(None, PropertyMap::new()).assert_ok();
		let second = graph.add_node(None, PropertyMap::new()).assert_ok();
		let third = graph.add_node(None, PropertyMap::new()).assert_ok();
		let edge = graph
			.add_edge(EdgeTopology::directed([first, second], [third]), None, PropertyMap::new())
			.assert_ok();

		assert_eq!(graph.incident_edges(first), [edge]);
		assert_eq!(graph.incident_edges(third), [edge]);
	}

	/// Verifies permits a vertex in both directed incidence roles.
	#[test]
	fn permits_a_vertex_in_both_directed_incidence_roles() {
		let mut graph = Hypergraph::<Mutable>::new();
		let node = graph.add_node(None, PropertyMap::new()).assert_ok();
		let edge = graph
			.add_edge(EdgeTopology::directed([node], [node]), None, PropertyMap::new())
			.assert_ok();
		assert_eq!(graph.incident_edges(node), &[edge, edge]);
	}

	/// Verifies graph conversion rejects edge endpoints.
	#[test]
	fn graph_conversion_rejects_edge_endpoints() {
		let mut graph = Graph::<Mutable>::new();
		let left = graph.add_node(None, PropertyMap::new()).assert_ok();
		let right = graph.add_node(None, PropertyMap::new()).assert_ok();
		let edge = graph
			.add_edge(EdgeTopology::directed([left], [right]), None, PropertyMap::new())
			.assert_ok();
		graph
			.add_edge(EdgeTopology::directed([edge], [right]), None, PropertyMap::new())
			.assert_ok();

		let error = Hypergraph::try_from(graph).assert_err();
		assert_eq!(error.status().code().to_string(), "FG-G2F04");
	}

	/// Verifies vertex removal marks its hyperedge dirty once.
	#[test]
	fn vertex_removal_marks_its_hyperedge_dirty_once() {
		let mut graph = Hypergraph::<Mutable>::new();
		let removed = graph.add_node(None, PropertyMap::new()).assert_ok();
		let peer = graph.add_node(None, PropertyMap::new()).assert_ok();
		let edge = graph
			.add_edge(EdgeTopology::directed([removed], [removed, peer]), None, PropertyMap::new())
			.assert_ok();
		graph.metadata.clear_dirty();

		graph.remove_atom(removed).assert_ok();

		let dirty = graph.metadata.dirty().cloned().collect::<Vec<_>>();
		assert_eq!(dirty.len(), 2);
		assert!(dirty.contains(&DirtyTarget::Atom(removed)));
		assert!(dirty.contains(&DirtyTarget::Atom(edge)));
	}
}
