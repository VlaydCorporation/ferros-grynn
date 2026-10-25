use std::{
	borrow::Cow,
	cmp::Ordering,
	collections::BTreeSet,
	fmt::{self, Debug, Formatter},
	hash::{Hash, Hasher},
	marker::PhantomData,
	sync::Arc,
};

use super::{
	AtomId,
	AtomIds,
	AtomKind,
	DirtyTarget,
	EdgeDirection,
	EdgeRead,
	EdgeTopology,
	EdgeView,
	FallibleAtomIds,
	Forest,
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
	RootedTree,
	Tree,
	TreeOrientation,
	general::{atom_does_not_exist, atom_kind_mismatch},
	storage::{EdgeColumns, GraphMetadata, NodeColumns, ensure_unique_record, generated_record_id},
};
use crate::{
	collections::cow_vec::CowVec,
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	values::runtime::{Number, PropertyMap, RecordId, Text, Uuid},
};

#[derive(Debug, Clone, Default)]
/// Packed adjacency state.
struct PackedAdjacency {
	/// Offsets associated with this packed adjacency.
	offsets: Vec<usize>,
	/// Edges associated with this packed adjacency.
	edges: Vec<AtomId>,
}

impl PackedAdjacency {
	/// Creates from rows.
	fn from_rows(rows: &CowVec<Vec<AtomId>>) -> Self {
		let mut offsets = Vec::with_capacity(rows.len() + 1);
		let mut edges = Vec::with_capacity(rows.iter().map(Vec::len).sum());
		offsets.push(0);
		for row in rows.iter() {
			edges.extend_from_slice(row);
			offsets.push(edges.len());
		}
		Self {
			offsets,
			edges,
		}
	}

	/// Returns the row result.
	fn row(&self, slot: usize) -> &[AtomId] {
		let Some((&start, &end)) = self.offsets.get(slot).zip(self.offsets.get(slot + 1)) else {
			return &[];
		};
		&self.edges[start..end]
	}
}

#[allow(
	clippy::rc_buffer,
	reason = "the topological order is built incrementally as a Vec and shared without an \
	          additional Vec-to-Arc<[T]> allocation"
)]
#[derive(Debug, Clone)]
/// Dag storage state.
struct DagStorage {
	/// Nodes associated with this dag storage.
	nodes: NodeColumns,
	/// Edges associated with this dag storage.
	edges: EdgeColumns,
	/// Outgoing associated with this dag storage.
	outgoing: CowVec<Vec<AtomId>>,
	/// Incoming associated with this dag storage.
	incoming: CowVec<Vec<AtomId>>,
	/// Packed outgoing associated with this dag storage.
	packed_outgoing: Option<Arc<PackedAdjacency>>,
	/// Packed incoming associated with this dag storage.
	packed_incoming: Option<Arc<PackedAdjacency>>,
	/// Topological associated with this dag storage.
	topological: Arc<Vec<AtomId>>,
	/// Rank associated with this dag storage.
	rank: CowVec<Option<usize>>,
}

impl DagStorage {
	/// Creates a new `DagStorage` value.
	fn new() -> Self {
		Self {
			nodes: NodeColumns::new(),
			edges: EdgeColumns::new(),
			outgoing: CowVec::new(),
			incoming: CowVec::new(),
			packed_outgoing: None,
			packed_incoming: None,
			topological: Arc::new(Vec::new()),
			rank: CowVec::new(),
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
		self.rebuild_topological()?;
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
		source: AtomId,
		target: AtomId,
		record_id: RecordId,
		label: Option<Text>,
		properties: PropertyMap,
		weight: Option<Number>,
	) -> FerrosGrynnResult<AtomId> {
		self.validate_edge(source, target)?;
		ensure_unique_record(&self.nodes, &self.edges, &record_id)?;
		let edge = self.edges.insert_new(
			record_id,
			EdgeTopology::directed([source], [target]),
			label,
			properties,
			weight,
		)?;
		self.register(edge, source, target);
		self.rebuild_topological()?;
		Ok(edge)
	}

	/// Inserts edge at.
	fn insert_edge_at(
		&mut self,
		id: AtomId,
		source: AtomId,
		target: AtomId,
		record_id: RecordId,
		topology: EdgeTopology,
		label: Option<Text>,
		properties: PropertyMap,
		weight: Option<Number>,
	) -> FerrosGrynnResult<()> {
		self.validate_edge(source, target)?;
		ensure_unique_record(&self.nodes, &self.edges, &record_id)?;
		self.edges.insert_at(id, record_id, topology, label, properties, weight)?;
		self.register(id, source, target);
		Ok(())
	}

	/// Validates edge.
	fn validate_edge(&self, source: AtomId, target: AtomId) -> FerrosGrynnResult<()> {
		for id in [source, target] {
			if id.kind() != Some(AtomKind::Node) {
				return Err(atom_kind_mismatch(id, AtomKind::Node));
			}
			if !self.nodes.contains(id) {
				return Err(atom_does_not_exist(id));
			}
		}
		if source == target {
			return Err(constraint("self-loops are forbidden"));
		}
		if self.reachable(target, source) {
			return Err(constraint("the edge would create a directed cycle"));
		}
		Ok(())
	}

	/// Returns whether the reachable condition holds.
	fn reachable(&self, start: AtomId, goal: AtomId) -> bool {
		let mut pending = vec![start];
		let mut seen = BTreeSet::new();
		while let Some(node) = pending.pop() {
			if node == goal {
				return true;
			}
			if !seen.insert(node) {
				continue;
			}
			for edge in self.outgoing_edges(node) {
				if let Some((_, target, EdgeDirection::Directed)) =
					self.edges.topology(*edge).and_then(EdgeTopology::local_binary)
				{
					pending.push(target);
				}
			}
		}
		false
	}

	/// Registers dag storage.
	fn register(&mut self, edge: AtomId, source: AtomId, target: AtomId) {
		self.outgoing[source.slot() as usize].push(edge);
		self.incoming[target.slot() as usize].push(edge);
		self.packed_outgoing = None;
		self.packed_incoming = None;
	}

	/// Removes edge.
	fn remove_edge(&mut self, id: AtomId) -> FerrosGrynnResult<bool> {
		let Some((source, target, EdgeDirection::Directed)) =
			self.edges.topology(id).and_then(EdgeTopology::local_binary)
		else {
			return Ok(false);
		};
		self.outgoing[source.slot() as usize].retain(|edge| *edge != id);
		self.incoming[target.slot() as usize].retain(|edge| *edge != id);
		self.edges.remove(id);
		self.packed_outgoing = None;
		self.packed_incoming = None;
		self.rebuild_topological()?;
		Ok(true)
	}

	/// Sets edge.
	#[cfg(any(test, feature = "test-support"))]
	fn set_edge(&mut self, id: AtomId, source: AtomId, target: AtomId) -> FerrosGrynnResult<()> {
		let Some((old_source, old_target, EdgeDirection::Directed)) =
			self.edges.topology(id).and_then(EdgeTopology::local_binary)
		else {
			return Err(atom_does_not_exist(id));
		};
		self.outgoing[old_source.slot() as usize].retain(|edge| *edge != id);
		self.incoming[old_target.slot() as usize].retain(|edge| *edge != id);
		if let Err(error) = self.validate_edge(source, target) {
			self.register(id, old_source, old_target);
			return Err(error);
		}
		*self.edges.topology_mut(id).ok_or_else(|| atom_does_not_exist(id))? =
			EdgeTopology::directed([source], [target]);
		self.register(id, source, target);
		self.rebuild_topological()
	}

	/// Removes node.
	fn remove_node(&mut self, id: AtomId) -> FerrosGrynnResult<bool> {
		if !self.nodes.contains(id) {
			return Ok(false);
		}
		let mut edges = self.outgoing_edges(id).to_vec();
		edges.extend_from_slice(self.incoming_edges(id));
		edges.sort_unstable();
		edges.dedup();
		for edge in edges {
			self.remove_edge(edge)?;
		}
		self.nodes.remove(id);
		let slot = id.slot() as usize;
		self.outgoing[slot].clear();
		self.incoming[slot].clear();
		self.rebuild_topological()?;
		Ok(true)
	}

	/// Returns the outgoing edges result.
	fn outgoing_edges(&self, id: AtomId) -> &[AtomId] {
		if !self.nodes.contains(id) {
			return &[];
		}
		if let Some(packed) = &self.packed_outgoing {
			return packed.row(id.slot() as usize);
		}
		self.outgoing.get(id.slot() as usize).map_or(&[], Vec::as_slice)
	}

	/// Returns the incoming edges result.
	fn incoming_edges(&self, id: AtomId) -> &[AtomId] {
		if !self.nodes.contains(id) {
			return &[];
		}
		if let Some(packed) = &self.packed_incoming {
			return packed.row(id.slot() as usize);
		}
		self.incoming.get(id.slot() as usize).map_or(&[], Vec::as_slice)
	}

	/// Rebuilds topological.
	fn rebuild_topological(&mut self) -> FerrosGrynnResult<()> {
		let mut indegree = vec![0_usize; self.incoming.len()];
		let mut ready = BTreeSet::new();
		for id in self.nodes.ids().iter().copied() {
			indegree[id.slot() as usize] = self.incoming[id.slot() as usize].len();
			if indegree[id.slot() as usize] == 0 {
				ready.insert(id);
			}
		}
		let mut order = Vec::with_capacity(self.nodes.len());
		while let Some(id) = ready.pop_first() {
			order.push(id);
			for edge in &self.outgoing[id.slot() as usize] {
				let Some((_, target, EdgeDirection::Directed)) =
					self.edges.topology(*edge).and_then(EdgeTopology::local_binary)
				else {
					return Err(constraint("DAG storage contains a non-directed edge"));
				};
				let degree = &mut indegree[target.slot() as usize];
				*degree = degree.saturating_sub(1);
				if *degree == 0 {
					ready.insert(target);
				}
			}
		}
		if order.len() != self.nodes.len() {
			return Err(constraint("the topology contains a directed cycle"));
		}
		self.rank.resize(self.outgoing.len(), None);
		self.rank.fill(None);
		for (rank, id) in order.iter().copied().enumerate() {
			self.rank[id.slot() as usize] = Some(rank);
		}
		self.topological = Arc::new(order);
		Ok(())
	}

	/// Performs the freeze layout operation.
	fn freeze_layout(&mut self) {
		self.packed_outgoing = Some(Arc::new(PackedAdjacency::from_rows(&self.outgoing)));
		self.packed_incoming = Some(Arc::new(PackedAdjacency::from_rows(&self.incoming)));
		self.outgoing.clear();
		self.incoming.clear();
	}

	/// Returns the thaw layout result.
	fn thaw_layout(&mut self) -> FerrosGrynnResult<()> {
		if self.packed_outgoing.is_none() {
			return Ok(());
		}
		let slots = self.rank.len();
		self.outgoing = vec![Vec::new(); slots].into();
		self.incoming = vec![Vec::new(); slots].into();
		for edge in self.edges.ids().to_vec() {
			let Some((source, target, EdgeDirection::Directed)) =
				self.edges.topology(edge).and_then(EdgeTopology::local_binary)
			else {
				return Err(constraint("DAG storage contains a non-directed edge"));
			};
			self.outgoing[source.slot() as usize].push(edge);
			self.incoming[target.slot() as usize].push(edge);
		}
		self.packed_outgoing = None;
		self.packed_incoming = None;
		Ok(())
	}

	/// Ensures node slot.
	fn ensure_node_slot(&mut self, id: AtomId) {
		let slot = id.slot() as usize;
		while self.outgoing.len() <= slot {
			self.outgoing.push(Vec::new());
			self.incoming.push(Vec::new());
			self.rank.push(None);
		}
	}
}

/// Graph family whose structural mutations preserve directed acyclicity.
pub struct Dag<State = Frozen> {
	/// Metadata associated with this dag.
	metadata: GraphMetadata,
	/// Storage associated with this dag.
	storage: DagStorage,
	/// State associated with this dag.
	state: PhantomData<State>,
}

pub type MutableDag = Dag<Mutable>;
pub type FrozenDag = Dag<Frozen>;

impl Dag<Mutable> {
	/// Constructs `Dag<Mutable>` from the supplied initial state.
	pub fn new() -> Self {
		Self {
			metadata: GraphMetadata::new(),
			storage: DagStorage::new(),
			state: PhantomData,
		}
	}

	/// Returns add node for `Dag<Mutable>`.
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

	/// Returns add edge for `Dag<Mutable>`.
	///
	/// # Errors
	///
	/// Propagates errors returned by the underlying operation.
	pub fn add_edge(
		&mut self,
		source: AtomId,
		target: AtomId,
		label: Option<Text>,
		properties: PropertyMap,
	) -> FerrosGrynnResult<AtomId> {
		self.add_edge_with_id(
			source,
			target,
			generated_record_id(AtomKind::Edge)?,
			label,
			properties,
		)
	}

	/// Returns the add edge with id result.
	pub(crate) fn add_edge_with_id(
		&mut self,
		source: AtomId,
		target: AtomId,
		record_id: RecordId,
		label: Option<Text>,
		properties: PropertyMap,
	) -> FerrosGrynnResult<AtomId> {
		self.metadata.ensure_can_bump()?;
		let id =
			self.storage.insert_edge_new(source, target, record_id, label, properties, None)?;
		self.metadata.bump(Some(DirtyTarget::Atom(id)))?;
		Ok(id)
	}

	/// Replaces edge incidence.
	#[cfg(any(test, feature = "test-support"))]
	pub(crate) fn replace_edge_incidence(
		&mut self,
		id: AtomId,
		source: AtomId,
		target: AtomId,
	) -> FerrosGrynnResult<()> {
		self.metadata.ensure_can_bump()?;
		self.storage.set_edge(id, source, target)?;
		self.metadata.bump(Some(DirtyTarget::Atom(id)))
	}

	/// Returns freeze for `Dag<Mutable>`.
	pub fn freeze(mut self) -> Dag<Frozen> {
		self.storage.freeze_layout();
		Dag {
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

	/// Performs the flush operation for `Dag<Mutable>`.
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

impl Default for Dag<Mutable> {
	/// Constructs the default initial state of `Dag<Mutable>`.
	fn default() -> Self {
		Self::new()
	}
}

impl Dag<Frozen> {
	/// Returns thaw for `Dag<Frozen>`.
	///
	/// # Errors
	///
	/// Propagates errors returned by the underlying operation.
	pub fn thaw(mut self) -> FerrosGrynnResult<Dag<Mutable>> {
		self.storage.thaw_layout()?;
		Ok(Dag {
			metadata: self.metadata,
			storage: self.storage,
			state: PhantomData,
		})
	}
}

impl Clone for Dag<Frozen> {
	/// Returns an owned clone of this `Dag<Frozen>`.
	fn clone(&self) -> Self {
		Self {
			metadata: self.metadata.clone(),
			storage: self.storage.clone(),
			state: PhantomData,
		}
	}
}

impl<State> Dag<State> {
	/// Returns the slot capacity result.
	pub(crate) fn slot_capacity(&self, kind: AtomKind) -> usize {
		match kind {
			AtomKind::Node => self.storage.nodes.slot_capacity(),
			AtomKind::Edge => self.storage.edges.slot_capacity(),
		}
	}

	/// Returns identity for `Dag<State>`.
	pub const fn identity(&self) -> Uuid {
		self.metadata.identity()
	}

	/// Returns revision for `Dag<State>`.
	pub const fn revision(&self) -> u64 {
		self.metadata.revision()
	}

	/// Returns properties for `Dag<State>`.
	pub const fn properties(&self) -> &PropertyMap {
		self.metadata.properties()
	}

	/// Returns topology constraints for `Dag<State>`.
	pub const fn topology_constraints(&self) -> GraphTopologyConstraints {
		self.metadata.topology_constraints()
	}

	/// Returns node count for `Dag<State>`.
	pub const fn node_count(&self) -> usize {
		self.storage.nodes.len()
	}

	/// Returns edge count for `Dag<State>`.
	pub const fn edge_count(&self) -> usize {
		self.storage.edges.len()
	}

	/// Returns node for `Dag<State>`.
	pub fn node(&self, id: AtomId) -> Option<NodeView<'_>> {
		Some(NodeView::new(
			id,
			self.storage.nodes.record_id(id)?,
			self.storage.nodes.label(id)?.as_ref(),
			self.storage.nodes.properties(id)?,
			None,
		))
	}

	/// Returns edge for `Dag<State>`.
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

	/// Returns outgoing edges for `Dag<State>`.
	pub fn outgoing_edges(&self, id: AtomId) -> &[AtomId] {
		self.storage.outgoing_edges(id)
	}

	/// Returns incoming edges for `Dag<State>`.
	pub fn incoming_edges(&self, id: AtomId) -> &[AtomId] {
		self.storage.incoming_edges(id)
	}

	/// Borrows the stored storage.topological of this `Dag<State>`.
	pub fn topological_order(&self) -> &[AtomId] {
		&self.storage.topological
	}

	/// Returns topological rank for `Dag<State>`.
	pub fn topological_rank(&self, id: AtomId) -> Option<usize> {
		self.storage
			.nodes
			.contains(id)
			.then(|| self.storage.rank.get(id.slot() as usize).copied().flatten())?
	}

	/// Converts this value into graph for `Dag<State>`.
	///
	/// # Errors
	///
	/// Propagates errors returned by the underlying operation.
	pub fn into_graph(self) -> FerrosGrynnResult<Graph<State>> {
		let converted = Graph::<Mutable>::from_read_preserving(&self)?;
		let (_, storage) = converted.into_raw();
		Ok(Graph::from_raw(self.metadata, storage))
	}

	/// Constructs from undirected tree for `Dag<State>`.
	///
	/// # Errors
	///
	/// Propagates errors returned by the underlying operation.
	pub fn from_undirected_tree(
		tree: Tree<State>,
		root: AtomId,
		orientation: TreeOrientation,
	) -> FerrosGrynnResult<Self> {
		if tree.orientation() != TreeOrientation::Undirected {
			return Err(constraint("the source tree is already directed"));
		}
		if orientation == TreeOrientation::Undirected {
			return Err(constraint("a DAG conversion requires an explicit directed orientation"));
		}
		let graph = tree.into_graph()?;
		let storage = build_oriented_tree_storage(&graph, root, orientation)?;
		let (metadata, _) = graph.into_raw();
		Ok(Self {
			metadata,
			storage,
			state: PhantomData,
		})
	}
}

impl<State> GraphRead for Dag<State> {
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

	/// Returns identity for `Dag<State>`.
	fn identity(&self) -> Uuid {
		self.identity()
	}

	/// Returns revision for `Dag<State>`.
	fn revision(&self) -> u64 {
		self.revision()
	}

	/// Returns kind for `Dag<State>`.
	fn kind(&self) -> GraphKind {
		GraphKind::Dag
	}

	/// Returns properties for `Dag<State>`.
	fn properties(&self) -> FerrosGrynnResult<Cow<'_, PropertyMap>> {
		Ok(Cow::Borrowed(self.properties()))
	}

	/// Returns topology constraints for `Dag<State>`.
	fn topology_constraints(&self) -> GraphTopologyConstraints {
		self.topology_constraints()
	}

	/// Returns node ids for `Dag<State>`.
	fn node_ids(&self) -> FerrosGrynnResult<Self::NodeIds<'_>> {
		Ok(FallibleAtomIds::new(AtomIds::new(self.storage.nodes.ids().iter())))
	}

	/// Returns edge ids for `Dag<State>`.
	fn edge_ids(&self) -> FerrosGrynnResult<Self::EdgeIds<'_>> {
		Ok(FallibleAtomIds::new(AtomIds::new(self.storage.edges.ids().iter())))
	}

	/// Returns node for `Dag<State>`.
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

	/// Returns edge for `Dag<State>`.
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

	/// Returns storage mode for `Dag<State>`.
	fn storage_mode(&self) -> super::GraphStorageMode {
		self.metadata.storage_mode()
	}

	/// Returns graph ref for `Dag<State>`.
	fn graph_ref(&self) -> Option<&super::GraphRef> {
		self.metadata.graph_ref()
	}

	/// Returns materialization origin for `Dag<State>`.
	fn materialization_origin(&self) -> Option<&super::MaterializationOrigin> {
		self.metadata.origin()
	}
}

impl GraphMut for Dag<Mutable> {
	/// Returns properties mut for `Dag<Mutable>`.
	fn properties_mut(&mut self) -> FerrosGrynnResult<&mut PropertyMap> {
		self.metadata.properties_mut()
	}

	/// Returns node label mut for `Dag<Mutable>`.
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

	/// Returns node properties mut for `Dag<Mutable>`.
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

	/// Returns edge label mut for `Dag<Mutable>`.
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

	/// Returns edge properties mut for `Dag<Mutable>`.
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

	/// Removes atom for `Dag<Mutable>`.
	fn remove_atom(&mut self, id: AtomId) -> FerrosGrynnResult<()> {
		self.metadata.ensure_can_bump()?;
		let dependent_edges = if id.is_node() {
			let mut edges = self.storage.outgoing_edges(id).to_vec();
			edges.extend_from_slice(self.storage.incoming_edges(id));
			edges.sort_unstable();
			edges.dedup();
			edges
		} else {
			Vec::new()
		};
		let removed = match id.kind() {
			Some(AtomKind::Node) => self.storage.remove_node(id)?,
			Some(AtomKind::Edge) => self.storage.remove_edge(id)?,
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

impl<State> TryFrom<Graph<State>> for Dag<State> {
	type Error = FerrosGrynnError;

	/// Converts `Graph<State>` into `Dag<State>` after validating the
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

impl<State> TryFrom<Dag<State>> for Tree<State> {
	type Error = FerrosGrynnError;

	/// Converts `Dag<State>` into `Tree<State>` after validating the
	/// conversion.
	fn try_from(value: Dag<State>) -> Result<Self, Self::Error> {
		Tree::try_from(value.into_graph()?)
	}
}

/// Expands the impl from forest family declarations used by this module.
macro_rules! impl_from_forest_family {
	($source:ident) => {
		impl TryFrom<$source<Mutable>> for Dag<Mutable> {
			type Error = FerrosGrynnError;

			/// Checks the source variant and converts its payload to the requested
			/// type.
			fn try_from(value: $source<Mutable>) -> Result<Self, Self::Error> {
				if value.orientation() == TreeOrientation::Undirected {
					return Err(constraint(
						"an undirected forest requires an explicit root and orientation",
					));
				}
				let graph = Graph::<Mutable>::from_read_preserving(&value)?;
				Self::try_from(graph)
			}
		}

		impl TryFrom<$source<Frozen>> for Dag<Frozen> {
			type Error = FerrosGrynnError;

			/// Checks the source variant and converts its payload to the requested
			/// type.
			fn try_from(value: $source<Frozen>) -> Result<Self, Self::Error> {
				if value.orientation() == TreeOrientation::Undirected {
					return Err(constraint(
						"an undirected forest requires an explicit root and orientation",
					));
				}
				let graph = Graph::<Mutable>::from_read_preserving(&value)?;
				Dag::<Mutable>::try_from(graph).map(Dag::freeze)
			}
		}
	};
}

impl_from_forest_family!(Forest);
impl_from_forest_family!(Tree);
impl_from_forest_family!(RootedTree);

impl QueryMaterialize for Dag<Mutable> {
	/// Returns query from for `Dag<Mutable>`.
	fn query_from<R: GraphRead>(source: &R) -> FerrosGrynnResult<Self> {
		Self::try_from(Graph::<Mutable>::query_from(source)?)
	}
}

impl QueryMaterialize for Dag<Frozen> {
	/// Returns query from for `Dag<Frozen>`.
	fn query_from<R: GraphRead>(source: &R) -> FerrosGrynnResult<Self> {
		Dag::<Mutable>::query_from(source).map(Dag::freeze)
	}
}

impl<State> Debug for Dag<State> {
	/// Formats `Dag<State>` using the requested formatter.
	fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
		formatter
			.debug_struct("Dag")
			.field("identity", &self.identity())
			.field("revision", &self.revision())
			.field("nodes", &self.node_count())
			.field("edges", &self.edge_count())
			.finish()
	}
}

impl<State> PartialEq for Dag<State> {
	/// Compares this `Dag<State>` with another value for equality.
	fn eq(&self, other: &Self) -> bool {
		(self.identity(), self.revision()) == (other.identity(), other.revision())
	}
}

impl<State> Eq for Dag<State> {}

impl<State> PartialOrd for Dag<State> {
	/// Compares this `Dag<State>` with another value when they are ordered.
	fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
		Some(self.cmp(other))
	}
}

impl<State> Ord for Dag<State> {
	/// Compares this `Dag<State>` with another value using its ordering
	/// contract.
	fn cmp(&self, other: &Self) -> Ordering {
		(self.identity(), self.revision()).cmp(&(other.identity(), other.revision()))
	}
}

impl<State> Hash for Dag<State> {
	/// Feeds this `Dag<State>` into the supplied hasher.
	fn hash<H: Hasher>(&self, state: &mut H) {
		self.identity().hash(state);
		self.revision().hash(state);
	}
}

/// Builds storage.
fn build_storage<R: GraphRead>(source: &R) -> FerrosGrynnResult<DagStorage> {
	let mut storage = DagStorage::new();
	for id in source.node_ids()? {
		let id = id?;
		let node = source.node(id)?.ok_or_else(|| atom_does_not_exist(id))?;
		if node.is_meta()? {
			return Err(constraint("DAG vertices cannot contain nested graphs"));
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
			return Err(constraint("DAG edges cannot contain transition graphs"));
		}
		let topology = edge.topology()?.into_owned();
		let Some((from, to, EdgeDirection::Directed)) = topology.local_binary() else {
			return Err(constraint("DAG edges must be local binary directed vertex edges"));
		};
		storage.insert_edge_at(
			id,
			from,
			to,
			edge.record_id()?.into_owned(),
			topology,
			edge.label_text()?.map(Cow::into_owned),
			edge.properties()?.into_owned(),
			edge.weight(),
		)?;
	}
	storage.rebuild_topological()?;
	Ok(storage)
}

/// Builds oriented tree storage.
fn build_oriented_tree_storage<R: GraphRead>(
	source: &R,
	root: AtomId,
	orientation: TreeOrientation,
) -> FerrosGrynnResult<DagStorage> {
	if source.node(root)?.is_none() {
		return Err(atom_does_not_exist(root));
	}
	let mut adjacency = std::collections::BTreeMap::<AtomId, Vec<(AtomId, AtomId)>>::new();
	for edge_id in source.edge_ids()? {
		let edge_id = edge_id?;
		let edge = source.edge(edge_id)?.ok_or_else(|| atom_does_not_exist(edge_id))?;
		let topology = edge.topology()?;
		let Some((left, right, EdgeDirection::Undirected)) = topology.local_binary() else {
			return Err(constraint("the source tree contains a non-undirected edge"));
		};
		adjacency.entry(left).or_default().push((right, edge_id));
		adjacency.entry(right).or_default().push((left, edge_id));
	}

	let mut storage = DagStorage::new();
	for id in source.node_ids()? {
		let id = id?;
		let node = source.node(id)?.ok_or_else(|| atom_does_not_exist(id))?;
		storage.insert_node_at(
			id,
			node.record_id()?.into_owned(),
			node.label_text()?.map(Cow::into_owned),
			node.properties()?.into_owned(),
		)?;
	}
	let mut seen = BTreeSet::from([root]);
	let mut pending = vec![root];
	while let Some(parent) = pending.pop() {
		for (child, edge_id) in adjacency.get(&parent).into_iter().flatten().copied() {
			if !seen.insert(child) {
				continue;
			}
			let edge = source.edge(edge_id)?.ok_or_else(|| atom_does_not_exist(edge_id))?;
			let (from, to) = match orientation {
				TreeOrientation::OutArborescence => (parent, child),
				TreeOrientation::InArborescence => (child, parent),
				TreeOrientation::Undirected => {
					return Err(constraint("a DAG conversion requires a directed orientation"));
				}
			};
			storage.insert_edge_at(
				edge_id,
				from,
				to,
				edge.record_id()?.into_owned(),
				EdgeTopology::directed([from], [to]),
				edge.label_text()?.map(Cow::into_owned),
				edge.properties()?.into_owned(),
				edge.weight(),
			)?;
			pending.push(child);
		}
	}
	if seen.len() != source.node_count()? {
		return Err(constraint("the selected root does not reach the complete tree"));
	}
	storage.rebuild_topological()?;
	Ok(storage)
}

/// Returns the constraint result.
fn constraint(reason: impl Into<String>) -> FerrosGrynnError {
	FerrosGrynnError::graph_constraint_violation("dag", reason.into())
}

/// Operations shared with the engine and value codecs.
pub trait DagMutableInternal: Sized {
	/// Replaces edge incidence.
	#[cfg(any(test, feature = "test-support"))]
	/// # Errors
	/// Propagates validation, resource-admission or callback errors from the
	/// operation.
	fn replace_edge_incidence(
		&mut self,
		id: AtomId,
		source: AtomId,
		target: AtomId,
	) -> FerrosGrynnResult<()>;
}
impl DagMutableInternal for Dag<Mutable> {
	#[cfg(any(test, feature = "test-support"))]
	fn replace_edge_incidence(
		&mut self,
		id: AtomId,
		source: AtomId,
		target: AtomId,
	) -> FerrosGrynnResult<()> {
		<Dag<Mutable>>::replace_edge_incidence(self, id, source, target)
	}
}

#[cfg(test)]
/// Regression tests for dag behavior.
mod tests {
	use super::Dag;
	#[cfg(test)]
	use crate::test_assertions::AssertResult as _;
	use crate::values::{
		PropertyMap,
		runtime::graph::{DirtyTarget, GraphMut, Mutable},
	};

	/// Verifies rejects cycles transactionally and keeps topological order.
	#[test]
	fn rejects_cycles_transactionally_and_keeps_topological_order() {
		let mut dag = Dag::<Mutable>::new();
		let first = dag.add_node(None, PropertyMap::new()).assert_ok();
		let second = dag.add_node(None, PropertyMap::new()).assert_ok();
		let third = dag.add_node(None, PropertyMap::new()).assert_ok();
		dag.add_edge(first, second, None, PropertyMap::new()).assert_ok();
		dag.add_edge(second, third, None, PropertyMap::new()).assert_ok();
		let revision = dag.revision();
		let error = dag.add_edge(third, first, None, PropertyMap::new()).assert_err();

		assert_eq!(error.status().code().to_string(), "FG-G2F04");
		assert_eq!(dag.revision(), revision);
		assert_eq!(dag.topological_order(), [first, second, third]);
	}

	/// Verifies frozen layout round trips to mutable adjacency.
	#[test]
	fn frozen_layout_round_trips_to_mutable_adjacency() {
		let mut dag = Dag::<Mutable>::new();
		let left = dag.add_node(None, PropertyMap::new()).assert_ok();
		let right = dag.add_node(None, PropertyMap::new()).assert_ok();
		let edge = dag.add_edge(left, right, None, PropertyMap::new()).assert_ok();
		let frozen = dag.freeze();
		assert_eq!(frozen.outgoing_edges(left), [edge]);
		let mut dag = frozen.thaw().assert_ok();
		dag.remove_atom(edge).assert_ok();
		assert!(dag.outgoing_edges(left).is_empty());
	}

	/// Verifies parallel edges are allowed.
	#[test]
	fn parallel_edges_are_allowed() {
		let mut dag = Dag::<Mutable>::new();
		let left = dag.add_node(None, PropertyMap::new()).assert_ok();
		let right = dag.add_node(None, PropertyMap::new()).assert_ok();
		dag.add_edge(left, right, None, PropertyMap::new()).assert_ok();
		dag.add_edge(left, right, None, PropertyMap::new()).assert_ok();
		assert_eq!(dag.edge_count(), 2);
	}

	/// Verifies node removal marks incoming and outgoing edges dirty.
	#[test]
	fn node_removal_marks_incoming_and_outgoing_edges_dirty() {
		let mut dag = Dag::<Mutable>::new();
		let first = dag.add_node(None, PropertyMap::new()).assert_ok();
		let middle = dag.add_node(None, PropertyMap::new()).assert_ok();
		let last = dag.add_node(None, PropertyMap::new()).assert_ok();
		let incoming = dag.add_edge(first, middle, None, PropertyMap::new()).assert_ok();
		let outgoing = dag.add_edge(middle, last, None, PropertyMap::new()).assert_ok();
		dag.metadata.clear_dirty();

		dag.remove_atom(middle).assert_ok();

		let dirty = dag.metadata.dirty().cloned().collect::<Vec<_>>();
		assert_eq!(dirty.len(), 3);
		assert!(dirty.contains(&DirtyTarget::Atom(middle)));
		assert!(dirty.contains(&DirtyTarget::Atom(incoming)));
		assert!(dirty.contains(&DirtyTarget::Atom(outgoing)));
	}
}
