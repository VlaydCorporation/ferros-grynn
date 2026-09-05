use rapidhash::RapidHashMap;

#[allow(
	clippy::wildcard_imports,
	reason = "the facade owns imports during the move-only split"
)]
use super::*;

pub(in crate::datastore) struct GraphWrite {
	/// Catalog name published with this graph write.
	pub(super) name: String,
	pub(super) base: Option<Arc<MaterializedGraph>>,
	pub(super) local: Option<LocalGraphWrite>,
	pub(super) base_ts: u64,
	pub(super) value: Option<MutableGraphValue>,
	/// Rebuilds durable secondary indexes even when graph pages are unchanged.
	pub(super) force_persist: bool,
}

pub(in crate::datastore) struct LocalGraphWrite {
	pub(super) base: Arc<LocalGraph>,
	pub(super) snapshot: Arc<LocalReadTransaction>,
	pub(super) delta: LocalGraphDelta,
}

pub(in crate::datastore) struct LocalGraphDelta {
	kind: GraphKind,
	next_node_slot: u64,
	next_revision: u64,
	nodes: RapidHashMap<RecordId, LocalCreatedNode>,
}

pub(in crate::datastore) struct LocalCreatedNode {
	pub(super) id: AtomId,
	pub(super) label: Option<Text>,
	pub(super) properties: PropertyMap,
}

impl LocalGraphDelta {
	fn new(kind: GraphKind, next_node_slot: u64, next_revision: u64) -> Self {
		Self {
			kind,
			next_node_slot,
			next_revision,
			nodes: RapidHashMap::default(),
		}
	}

	fn create_node(
		&mut self,
		base: &LocalGraphView<'_>,
		record_id: RecordId,
		label: Option<Text>,
		properties: PropertyMap,
	) -> FerrosGrynnResult<AtomRef> {
		if matches!(self.kind, GraphKind::Tree | GraphKind::RootedTree) {
			return Err(FerrosGrynnError::graph_constraint_violation(
				self.kind.name(),
				"an isolated node would violate the single-tree invariant",
			));
		}
		if self.nodes.contains_key(&record_id)
			|| (base.node_count()? != 0 && base.contains_record_id(&record_id)?)
		{
			return Err(FerrosGrynnError::graph_constraint_violation(
				"record id",
				format!("duplicate RecordId '{record_id}' in one graph"),
			));
		}
		let id = AtomId::from_parts(AtomKind::Node, self.next_node_slot, 0)?;
		let next_node_slot = self
			.next_node_slot
			.checked_add(1)
			.ok_or_else(|| FerrosGrynnError::graph_capacity_exceeded(AtomKind::Node.to_string()))?;
		let next_revision = self
			.next_revision
			.checked_add(1)
			.ok_or_else(|| FerrosGrynnError::graph_capacity_exceeded("revision".to_owned()))?;
		self.next_node_slot = next_node_slot;
		self.next_revision = next_revision;
		let reference = AtomRef::new(AtomKind::Node, record_id.clone());
		self.nodes.insert(record_id, LocalCreatedNode {
			id,
			label,
			properties,
		});
		Ok(reference)
	}

	pub(super) fn dirty(&self) -> impl ExactSizeIterator<Item = DirtyTarget> + '_ {
		self.nodes.values().map(|node| DirtyTarget::Atom(node.id))
	}

	pub(super) fn estimated_write_set_bytes(&self) -> usize {
		self.nodes
			.iter()
			.map(|(record_id, node)| {
				DIRTY_TARGET_WRITE_BYTES
					.saturating_add(std::mem::size_of::<LocalCreatedNode>())
					.saturating_add(record_id.estimated_heap_bytes())
					.saturating_add(node.label.as_ref().map_or(0, Text::len))
					.saturating_add(node.properties.estimated_heap_bytes())
			})
			.fold(0_usize, usize::saturating_add)
	}

	pub(super) fn created_node(&self, id: AtomId) -> Option<(&RecordId, &LocalCreatedNode)> {
		self.nodes.iter().find(|(_record_id, node)| node.id == id)
	}

	pub(super) fn is_empty(&self) -> bool {
		self.nodes.is_empty()
	}

	pub(super) fn into_commit_nodes(self) -> Vec<crate::storage::LocalCommitNode> {
		let mut nodes = self
			.nodes
			.into_iter()
			.map(|(record_id, node)| crate::storage::LocalCommitNode {
				id: node.id,
				record_id,
				label: node.label,
				properties: node.properties,
			})
			.collect::<Vec<_>>();
		nodes.sort_unstable_by_key(|node| node.id);
		nodes
	}

	fn apply_to(&self, value: &mut MutableGraphValue) -> FerrosGrynnResult<()> {
		for (record_id, node) in &self.nodes {
			value.create_node(record_id.clone(), node.label.clone(), node.properties.clone())?;
		}
		Ok(())
	}
}

impl GraphWrite {
	pub(in crate::datastore) const fn is_delta_backed(&self) -> bool {
		self.local.is_some() && self.value.is_none()
	}

	pub(super) const fn exists(&self) -> bool {
		self.value.is_some() || self.local.is_some()
	}

	pub(super) const fn is_created(&self) -> bool {
		self.base.is_none() && self.local.is_none()
	}

	pub(super) fn kind(&self) -> Option<GraphKind> {
		self.value
			.as_ref()
			.map(MutableGraphValue::kind)
			.or_else(|| self.local.as_ref().map(|local| local.delta.kind))
			.or_else(|| self.base.as_ref().map(|base| base.kind()))
	}

	pub(super) fn snapshot(&self, store: &Datastore) -> FerrosGrynnResult<MaterializedGraph> {
		if let Some(value) = &self.value {
			return Ok(value.snapshot());
		}
		let local = self
			.local
			.as_ref()
			.ok_or_else(|| FerrosGrynnError::graph_does_not_exist(self.name.clone()))?;
		let storage = store.inner.backend.local().ok_or_else(|| {
			FerrosGrynnError::invalid_storage_file(
				"local graph is attached to an in-memory backend",
			)
		})?;
		let base = storage.materialize_graph(local.snapshot.clone(), &local.base)?;
		let mut value = MutableGraphValue::from_frozen(&base)?;
		local.delta.apply_to(&mut value)?;
		Ok(value.snapshot())
	}

	pub(super) fn ensure_materialized(&mut self, store: &Datastore) -> FerrosGrynnResult<()> {
		let Some(local) = self.local.as_ref() else {
			return Ok(());
		};
		let storage = store.inner.backend.local().ok_or_else(|| {
			FerrosGrynnError::invalid_storage_file(
				"local graph is attached to an in-memory backend",
			)
		})?;
		let base = Arc::new(storage.materialize_graph(local.snapshot.clone(), &local.base)?);
		let mut value = MutableGraphValue::from_frozen(&base)?;
		local.delta.apply_to(&mut value)?;
		self.base = Some(base);
		self.value = Some(value);
		self.local = None;
		Ok(())
	}
}

pub(super) enum MutableGraphValue {
	Graph(Graph<Mutable>),
	Tree(Tree<Mutable>),
	RootedTree(RootedTree<Mutable>),
	Forest(Forest<Mutable>),
	Dag(Dag<Mutable>),
	Hypergraph(Hypergraph<Mutable>),
	Metagraph(Metagraph<Mutable>),
}

/// Endpoint incidence addressed by stable public atom references.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum AtomRefTopology {
	/// Directed incidence from `source` to `target`.
	Directed {
		/// Source endpoints.
		source: Vec<AtomRef>,
		/// Target endpoints.
		target: Vec<AtomRef>,
	},
	/// Bidirectional incidence between the source and target groups.
	Bidirectional {
		/// First endpoint group.
		source: Vec<AtomRef>,
		/// Second endpoint group.
		target: Vec<AtomRef>,
	},
	/// Undirected incidence over one endpoint group.
	Undirected {
		/// Unordered endpoints.
		endpoints: Vec<AtomRef>,
	},
}

/// Backend-neutral transaction-private point-mutation view.
///
/// Atom identity crosses the API boundary only as [`AtomRef`]. Local operations
/// retain an atom delta while the selected operation has a page-delta path;
/// unsupported operations materialize the exact transaction snapshot.
pub struct GraphMutationView<'a> {
	store: Datastore,
	kind: GraphKind,
	write: &'a mut GraphWrite,
}

impl GraphMutationView<'_> {
	/// Returns the structural family of the graph being changed.
	pub const fn kind(&self) -> GraphKind {
		self.kind
	}

	fn materialized(&mut self) -> FerrosGrynnResult<&mut MutableGraphValue> {
		self.write.ensure_materialized(&self.store)?;
		self.write
			.value
			.as_mut()
			.ok_or_else(|| FerrosGrynnError::graph_does_not_exist(self.write.name.clone()))
	}

	/// Creates a node with a caller-selected stable record ID.
	///
	/// # Errors
	///
	/// Returns a graph constraint error for a duplicate record ID or for graph
	/// families that cannot add an isolated node without violating invariants.
	pub fn create_node(
		&mut self,
		record_id: RecordId,
		label: Option<Text>,
		properties: PropertyMap,
	) -> FerrosGrynnResult<AtomRef> {
		if let Some(local) = self.write.local.as_mut() {
			let storage = self.store.inner.backend.local().ok_or_else(|| {
				FerrosGrynnError::invalid_storage_file(
					"local graph is attached to an in-memory backend",
				)
			})?;
			let base = storage.graph_view(local.snapshot.clone(), local.base.clone());
			return local.delta.create_node(&base, record_id, label, properties);
		}
		self.materialized()?.create_node(record_id, label, properties)
	}

	/// Creates an edge with stable record IDs for both the edge and endpoints.
	///
	/// # Errors
	///
	/// Returns an error when an endpoint is absent, the record ID is already in
	/// use, or the incidence violates the graph-family constraints.
	pub fn create_edge(
		&mut self,
		record_id: RecordId,
		topology: &AtomRefTopology,
		label: Option<Text>,
		properties: PropertyMap,
	) -> FerrosGrynnResult<AtomRef> {
		self.materialized()?.create_edge(record_id, topology, label, properties)
	}

	/// Replaces an atom label and returns the previous value.
	///
	/// # Errors
	///
	/// Returns an error when the referenced atom does not exist.
	pub fn set_label(
		&mut self,
		atom: &AtomRef,
		label: Option<Text>,
	) -> FerrosGrynnResult<Option<Text>> {
		self.materialized()?.set_atom_label(atom, label)
	}

	/// Inserts or replaces one atom property and returns the previous value.
	///
	/// # Errors
	///
	/// Returns an error when the referenced atom does not exist.
	pub fn set_property(
		&mut self,
		atom: &AtomRef,
		key: PropertyKeyId,
		value: Value,
	) -> FerrosGrynnResult<Option<Value>> {
		Ok(self.materialized()?.atom_properties_mut(atom)?.insert(key, value))
	}

	/// Removes one atom property and returns its previous value when present.
	///
	/// # Errors
	///
	/// Returns an error when the referenced atom does not exist.
	pub fn remove_property(
		&mut self,
		atom: &AtomRef,
		key: PropertyKeyId,
	) -> FerrosGrynnResult<Option<Value>> {
		Ok(self.materialized()?.atom_properties_mut(atom)?.remove(key))
	}

	/// Replaces the complete incidence of an existing edge.
	///
	/// # Errors
	///
	/// Returns an error when an atom is absent or the new incidence violates
	/// the graph-family constraints.
	pub fn replace_incidence(
		&mut self,
		edge: &AtomRef,
		topology: &AtomRefTopology,
	) -> FerrosGrynnResult<()> {
		self.materialized()?.replace_incidence(edge, topology)
	}

	/// Removes an atom. Removing a node also removes dependent edges.
	///
	/// # Errors
	///
	/// Returns an error when the atom is absent or removal violates a family
	/// invariant.
	pub fn remove_atom(&mut self, atom: &AtomRef) -> FerrosGrynnResult<()> {
		self.materialized()?.remove_atom(atom)
	}
}

/// A transaction-private family-specific graph write view.
///
/// This is a blind-write surface. Reads that influence application decisions
/// in a serializable transaction must be performed through [`GraphRef::read`]
/// before borrowing this view so they enter the MVCC read set.
#[non_exhaustive]
pub enum GraphWriteView<'a> {
	Graph(&'a mut Graph<Mutable>),
	Tree(&'a mut Tree<Mutable>),
	RootedTree(&'a mut RootedTree<Mutable>),
	Forest(&'a mut Forest<Mutable>),
	Dag(&'a mut Dag<Mutable>),
	Hypergraph(&'a mut Hypergraph<Mutable>),
	Metagraph(&'a mut Metagraph<Mutable>),
}

impl GraphWriteView<'_> {
	pub const fn kind(&self) -> GraphKind {
		match self {
			Self::Graph(_) => GraphKind::Graph,
			Self::Tree(_) => GraphKind::Tree,
			Self::RootedTree(_) => GraphKind::RootedTree,
			Self::Forest(_) => GraphKind::Forest,
			Self::Dag(_) => GraphKind::Dag,
			Self::Hypergraph(_) => GraphKind::Hypergraph,
			Self::Metagraph(_) => GraphKind::Metagraph,
		}
	}

	pub const fn as_graph_mut(&mut self) -> Option<&mut Graph<Mutable>> {
		match self {
			Self::Graph(value) => Some(value),
			_ => None,
		}
	}

	pub const fn as_tree_mut(&mut self) -> Option<&mut Tree<Mutable>> {
		match self {
			Self::Tree(value) => Some(value),
			_ => None,
		}
	}

	pub const fn as_rooted_tree_mut(&mut self) -> Option<&mut RootedTree<Mutable>> {
		match self {
			Self::RootedTree(value) => Some(value),
			_ => None,
		}
	}

	pub const fn as_forest_mut(&mut self) -> Option<&mut Forest<Mutable>> {
		match self {
			Self::Forest(value) => Some(value),
			_ => None,
		}
	}

	pub const fn as_dag_mut(&mut self) -> Option<&mut Dag<Mutable>> {
		match self {
			Self::Dag(value) => Some(value),
			_ => None,
		}
	}

	pub const fn as_hypergraph_mut(&mut self) -> Option<&mut Hypergraph<Mutable>> {
		match self {
			Self::Hypergraph(value) => Some(value),
			_ => None,
		}
	}

	pub const fn as_metagraph_mut(&mut self) -> Option<&mut Metagraph<Mutable>> {
		match self {
			Self::Metagraph(value) => Some(value),
			_ => None,
		}
	}
}

impl MutableGraphValue {
	pub(super) fn from_frozen(value: &MaterializedGraph) -> FerrosGrynnResult<Self> {
		match value {
			MaterializedGraph::Graph(value) => Ok(Self::Graph(value.clone().thaw())),
			MaterializedGraph::Tree(value) => Ok(Self::Tree(value.clone().thaw())),
			MaterializedGraph::RootedTree(value) => Ok(Self::RootedTree(value.clone().thaw())),
			MaterializedGraph::Forest(value) => Ok(Self::Forest(value.clone().thaw())),
			MaterializedGraph::Dag(value) => Ok(Self::Dag(value.clone().thaw()?)),
			MaterializedGraph::Hypergraph(value) => Ok(Self::Hypergraph(value.clone().thaw())),
			MaterializedGraph::Metagraph(value) => Ok(Self::Metagraph(value.clone().thaw())),
			MaterializedGraph::Subgraph(_) => Err(FerrosGrynnError::invalid_argument(
				"a materialized subgraph cannot be stored as a named graph",
			)),
		}
	}

	pub(super) const fn kind(&self) -> GraphKind {
		match self {
			Self::Graph(_) => GraphKind::Graph,
			Self::Tree(_) => GraphKind::Tree,
			Self::RootedTree(_) => GraphKind::RootedTree,
			Self::Forest(_) => GraphKind::Forest,
			Self::Dag(_) => GraphKind::Dag,
			Self::Hypergraph(_) => GraphKind::Hypergraph,
			Self::Metagraph(_) => GraphKind::Metagraph,
		}
	}

	fn atom_id(&self, reference: &AtomRef) -> FerrosGrynnResult<AtomId> {
		let id = match self {
			Self::Graph(value) => value.atom_id(reference)?,
			Self::Tree(value) => value.atom_id(reference)?,
			Self::RootedTree(value) => value.atom_id(reference)?,
			Self::Forest(value) => value.atom_id(reference)?,
			Self::Dag(value) => value.atom_id(reference)?,
			Self::Hypergraph(value) => value.atom_id(reference)?,
			Self::Metagraph(value) => value.atom_id(reference)?,
		};
		id.ok_or_else(|| {
			FerrosGrynnError::atom_does_not_exist(
				reference.kind().to_string(),
				reference.id().to_string(),
			)
		})
	}

	fn resolve_topology(&self, topology: &AtomRefTopology) -> FerrosGrynnResult<EdgeTopology> {
		let mut resolve = |reference: &AtomRef| self.atom_id(reference).map(EdgeEndpoint::Local);
		Ok(match topology {
			AtomRefTopology::Directed {
				source,
				target,
			} => EdgeTopology::directed(
				source.iter().map(&mut resolve).collect::<FerrosGrynnResult<Vec<_>>>()?,
				target.iter().map(&mut resolve).collect::<FerrosGrynnResult<Vec<_>>>()?,
			),
			AtomRefTopology::Bidirectional {
				source,
				target,
			} => EdgeTopology::bidirectional(
				source.iter().map(&mut resolve).collect::<FerrosGrynnResult<Vec<_>>>()?,
				target.iter().map(&mut resolve).collect::<FerrosGrynnResult<Vec<_>>>()?,
			),
			AtomRefTopology::Undirected {
				endpoints,
			} => EdgeTopology::undirected(
				endpoints.iter().map(resolve).collect::<FerrosGrynnResult<Vec<_>>>()?,
			),
		})
	}

	fn create_node(
		&mut self,
		record_id: RecordId,
		label: Option<Text>,
		properties: PropertyMap,
	) -> FerrosGrynnResult<AtomRef> {
		let reference = AtomRef::new(AtomKind::Node, record_id.clone());
		match self {
			Self::Graph(value) => value.add_node_with_id(record_id, label, properties)?,
			Self::Forest(value) => value.add_root_with_id(record_id, label, properties)?,
			Self::Dag(value) => value.add_node_with_id(record_id, label, properties)?,
			Self::Hypergraph(value) => value.add_node_with_id(record_id, label, properties)?,
			Self::Metagraph(value) => value.add_node_with_id(record_id, label, properties)?,
			Self::Tree(_) | Self::RootedTree(_) => {
				return Err(FerrosGrynnError::graph_constraint_violation(
					self.kind().name(),
					"an isolated node would violate the single-tree invariant",
				));
			}
		};
		Ok(reference)
	}

	fn create_edge(
		&mut self,
		record_id: RecordId,
		topology: &AtomRefTopology,
		label: Option<Text>,
		properties: PropertyMap,
	) -> FerrosGrynnResult<AtomRef> {
		let resolved = self.resolve_topology(topology)?;
		let reference = AtomRef::new(AtomKind::Edge, record_id.clone());
		match self {
			Self::Graph(value) => {
				value.add_edge_with_id(record_id, resolved, label, properties)?;
			}
			Self::Dag(value) => {
				let (source, target) = directed_pair(&resolved, GraphKind::Dag)?;
				value.add_edge_with_id(source, target, record_id, label, properties)?;
			}
			Self::Hypergraph(value) => {
				value.add_edge_with_id(record_id, resolved, label, properties)?;
			}
			Self::Metagraph(value) => {
				value.add_edge_with_id(record_id, resolved, label, properties)?;
			}
			Self::Forest(value) => {
				let (parent, child) = forest_pair(&resolved, value.orientation())?;
				value.connect_with_id(parent, child, record_id, label, properties)?;
			}
			Self::Tree(_) | Self::RootedTree(_) => {
				return Err(FerrosGrynnError::graph_constraint_violation(
					self.kind().name(),
					"use the family write view to add a child and edge atomically",
				));
			}
		}
		Ok(reference)
	}

	fn atom_properties_mut(&mut self, reference: &AtomRef) -> FerrosGrynnResult<&mut PropertyMap> {
		let id = self.atom_id(reference)?;
		match self {
			Self::Graph(value) => atom_properties_mut(value, id),
			Self::Tree(value) => atom_properties_mut(value, id),
			Self::RootedTree(value) => atom_properties_mut(value, id),
			Self::Forest(value) => atom_properties_mut(value, id),
			Self::Dag(value) => atom_properties_mut(value, id),
			Self::Hypergraph(value) => atom_properties_mut(value, id),
			Self::Metagraph(value) => atom_properties_mut(value, id),
		}
	}

	fn set_atom_label(
		&mut self,
		reference: &AtomRef,
		label: Option<Text>,
	) -> FerrosGrynnResult<Option<Text>> {
		let id = self.atom_id(reference)?;
		let current = match self {
			Self::Graph(value) => atom_label_mut(value, id),
			Self::Tree(value) => atom_label_mut(value, id),
			Self::RootedTree(value) => atom_label_mut(value, id),
			Self::Forest(value) => atom_label_mut(value, id),
			Self::Dag(value) => atom_label_mut(value, id),
			Self::Hypergraph(value) => atom_label_mut(value, id),
			Self::Metagraph(value) => atom_label_mut(value, id),
		}?;
		Ok(std::mem::replace(current, label))
	}

	fn replace_incidence(
		&mut self,
		reference: &AtomRef,
		topology: &AtomRefTopology,
	) -> FerrosGrynnResult<()> {
		if reference.kind() != AtomKind::Edge {
			return Err(FerrosGrynnError::atom_kind_mismatch(
				reference.id().to_string(),
				reference.kind().to_string(),
				AtomKind::Edge.to_string(),
			));
		}
		let id = self.atom_id(reference)?;
		let resolved = self.resolve_topology(topology)?;
		match self {
			Self::Graph(value) => value.replace_edge_topology(id, resolved),
			Self::Dag(value) => {
				let (source, target) = directed_pair(&resolved, GraphKind::Dag)?;
				value.replace_edge_incidence(id, source, target)
			}
			Self::Hypergraph(value) => value.replace_edge_incidence(id, resolved),
			Self::Metagraph(value) => value.replace_edge_incidence(id, resolved),
			Self::Tree(_) | Self::RootedTree(_) | Self::Forest(_) => {
				Err(FerrosGrynnError::graph_constraint_violation(
					self.kind().name(),
					"generic incidence replacement cannot preserve tree parent invariants",
				))
			}
		}
	}

	fn remove_atom(&mut self, reference: &AtomRef) -> FerrosGrynnResult<()> {
		let id = self.atom_id(reference)?;
		match self {
			Self::Graph(value) => GraphMut::remove_atom(value, id),
			Self::Tree(value) => GraphMut::remove_atom(value, id),
			Self::RootedTree(value) => GraphMut::remove_atom(value, id),
			Self::Forest(value) => GraphMut::remove_atom(value, id),
			Self::Dag(value) => GraphMut::remove_atom(value, id),
			Self::Hypergraph(value) => GraphMut::remove_atom(value, id),
			Self::Metagraph(value) => GraphMut::remove_atom(value, id),
		}
	}

	pub(super) fn dirty(&self) -> Vec<DirtyTarget> {
		match self {
			Self::Graph(value) => value.transaction_dirty(),
			Self::Tree(value) => value.transaction_dirty(),
			Self::RootedTree(value) => value.transaction_dirty(),
			Self::Forest(value) => value.transaction_dirty(),
			Self::Dag(value) => value.transaction_dirty(),
			Self::Hypergraph(value) => value.transaction_dirty(),
			Self::Metagraph(value) => value.transaction_dirty(),
		}
	}

	pub(super) fn snapshot(&self) -> MaterializedGraph {
		match self {
			Self::Graph(value) => value.transaction_snapshot().into(),
			Self::Tree(value) => value.transaction_snapshot().into(),
			Self::RootedTree(value) => value.transaction_snapshot().into(),
			Self::Forest(value) => value.transaction_snapshot().into(),
			Self::Dag(value) => value.transaction_snapshot().into(),
			Self::Hypergraph(value) => value.transaction_snapshot().into(),
			Self::Metagraph(value) => value.transaction_snapshot().into(),
		}
	}

	pub(super) fn freeze(self) -> MaterializedGraph {
		match self {
			Self::Graph(value) => value.freeze().into(),
			Self::Tree(value) => value.freeze().into(),
			Self::RootedTree(value) => value.freeze().into(),
			Self::Forest(value) => value.freeze().into(),
			Self::Dag(value) => value.freeze().into(),
			Self::Hypergraph(value) => value.freeze().into(),
			Self::Metagraph(value) => value.freeze().into(),
		}
	}

	const fn view(&mut self) -> GraphWriteView<'_> {
		match self {
			Self::Graph(value) => GraphWriteView::Graph(value),
			Self::Tree(value) => GraphWriteView::Tree(value),
			Self::RootedTree(value) => GraphWriteView::RootedTree(value),
			Self::Forest(value) => GraphWriteView::Forest(value),
			Self::Dag(value) => GraphWriteView::Dag(value),
			Self::Hypergraph(value) => GraphWriteView::Hypergraph(value),
			Self::Metagraph(value) => GraphWriteView::Metagraph(value),
		}
	}
}

fn atom_properties_mut(
	graph: &mut impl GraphMut,
	id: AtomId,
) -> FerrosGrynnResult<&mut PropertyMap> {
	match id.kind() {
		Some(AtomKind::Node) => graph.node_properties_mut(id),
		Some(AtomKind::Edge) => graph.edge_properties_mut(id),
		None => Err(FerrosGrynnError::atom_does_not_exist("null", id.to_string())),
	}
}

fn atom_label_mut(graph: &mut impl GraphMut, id: AtomId) -> FerrosGrynnResult<&mut Option<Text>> {
	match id.kind() {
		Some(AtomKind::Node) => graph.node_label_mut(id),
		Some(AtomKind::Edge) => graph.edge_label_mut(id),
		None => Err(FerrosGrynnError::atom_does_not_exist("null", id.to_string())),
	}
}

fn directed_pair(topology: &EdgeTopology, kind: GraphKind) -> FerrosGrynnResult<(AtomId, AtomId)> {
	match topology.local_binary() {
		Some((source, target, EdgeDirection::Directed)) => Ok((source, target)),
		_ => Err(FerrosGrynnError::graph_constraint_violation(
			kind.name(),
			"the graph family requires one directed local source and target",
		)),
	}
}

fn forest_pair(
	topology: &EdgeTopology,
	orientation: TreeOrientation,
) -> FerrosGrynnResult<(AtomId, AtomId)> {
	let Some((source, target, direction)) = topology.local_binary() else {
		return Err(FerrosGrynnError::graph_constraint_violation(
			GraphKind::Forest.name(),
			"a forest edge requires two local endpoints",
		));
	};
	match (orientation, direction) {
		(TreeOrientation::Undirected, EdgeDirection::Undirected)
		| (TreeOrientation::OutArborescence, EdgeDirection::Directed) => Ok((source, target)),
		(TreeOrientation::InArborescence, EdgeDirection::Directed) => Ok((target, source)),
		_ => Err(FerrosGrynnError::graph_constraint_violation(
			GraphKind::Forest.name(),
			"edge direction disagrees with the forest orientation",
		)),
	}
}

#[allow(
	clippy::multiple_inherent_impl,
	reason = "transaction responsibilities are split into cohesive modules"
)]
impl Transaction {
	/// Creates a graph using its record value as the default catalog name.
	///
	/// # Errors
	///
	/// Returns an error under the same conditions as
	/// [`Self::create_named_graph`].
	pub fn create_graph(
		&mut self,
		id: RecordId,
		graph: MaterializedGraph,
	) -> FerrosGrynnResult<GraphRef> {
		let name = id.id().to_string();
		self.create_named_graph(name, id, graph)
	}

	/// Creates a graph with an explicit case-sensitive catalog name.
	///
	/// # Errors
	///
	/// Returns an error when the transaction is not writable, the record or
	/// name already exists, or the name violates the named-graph bounds.
	pub fn create_named_graph<IntoText: Into<Text>>(
		&mut self,
		name: IntoText,
		id: RecordId,
		graph: MaterializedGraph,
	) -> FerrosGrynnResult<GraphRef> {
		self.ensure_writable()?;
		self.ensure_live()?;
		let name = name.into();
		validate_graph_name(name.as_str())?;
		if id.table() == INTERNAL_NESTED_GRAPH_TABLE {
			return Err(FerrosGrynnError::invalid_argument(format!(
				"RecordId table '{INTERNAL_NESTED_GRAPH_TABLE}' is reserved for nested graphs"
			)));
		}
		if self.visible_graph_kind(&id).is_some() {
			return Err(FerrosGrynnError::graph_already_exists(id.to_string()));
		}
		if self.graphs.iter().any(|(record_id, write)| {
			record_id != &id && write.exists() && write.name == name.as_str()
		}) {
			return Err(FerrosGrynnError::graph_already_exists(name.as_str()));
		}
		let root = self.read_root();
		if let Some(record_id) = root.graph_names.get(name.as_str())
			&& record_id != &id
			&& !self.graphs.get(record_id).is_some_and(|write| !write.exists())
		{
			return Err(FerrosGrynnError::graph_already_exists(name.as_str()));
		}
		let kind = graph.kind();
		let value = MutableGraphValue::from_frozen(&graph)?;
		self.graphs.insert(id.clone(), GraphWrite {
			name: name.as_str().to_owned(),
			base: None,
			local: None,
			base_ts: root.commit_ts,
			value: Some(value),
			force_persist: false,
		});
		Ok(GraphRef::new(id, kind, self.store.inner.graph_storage_mode()))
	}

	pub fn remove_graph(&mut self, reference: &GraphRef) -> FerrosGrynnResult<()> {
		self.ensure_writable()?;
		self.ensure_live()?;
		ensure_store_reference(reference, self.store.inner.graph_storage_mode())?;
		if self.graphs.get(reference.id()).is_some_and(GraphWrite::is_created) {
			self.graphs.remove(reference.id());
			return Ok(());
		}
		self.ensure_graph_write(reference)?;
		if let Some(write) = self.graphs.get_mut(reference.id()) {
			write.value = None;
		}
		Ok(())
	}

	pub fn edit_graph(&mut self, reference: &GraphRef) -> FerrosGrynnResult<GraphWriteView<'_>> {
		self.ensure_writable()?;
		self.ensure_live()?;
		self.ensure_graph_write(reference)?;
		self.graphs
			.get_mut(reference.id())
			.and_then(|write| write.value.as_mut())
			.map(MutableGraphValue::view)
			.ok_or_else(|| FerrosGrynnError::graph_does_not_exist(reference.id().to_string()))
	}

	/// Opens the backend-neutral point-mutation surface for one graph.
	///
	/// This is a blind-write surface. Reads that influence serializable
	/// decisions must be performed through [`GraphRef::read`] before this
	/// mutable borrow.
	///
	/// # Errors
	///
	/// Returns an error when the transaction is read-only, expired or
	/// cancelled, the graph reference belongs to another backend, or the graph
	/// is absent.
	pub fn mutate_graph(
		&mut self,
		reference: &GraphRef,
	) -> FerrosGrynnResult<GraphMutationView<'_>> {
		self.ensure_writable()?;
		self.ensure_live()?;
		self.ensure_graph_mutation_write(reference)?;
		let store = self.store.clone();
		let write = self
			.graphs
			.get_mut(reference.id())
			.ok_or_else(|| FerrosGrynnError::graph_does_not_exist(reference.id().to_string()))?;
		let kind = write
			.kind()
			.ok_or_else(|| FerrosGrynnError::graph_does_not_exist(reference.id().to_string()))?;
		Ok(GraphMutationView {
			store,
			kind,
			write,
		})
	}

	pub fn define_table(&mut self, mut schema: TableSchema) -> FerrosGrynnResult<()> {
		self.ensure_writable()?;
		self.ensure_live()?;
		ensure_store_reference(&schema.graph, self.store.inner.graph_storage_mode())?;
		if self.visible_table(schema.name.as_str()).is_some() {
			return Err(FerrosGrynnError::table_already_exists(schema.name.as_str()));
		}
		let graph_kind = self
			.visible_graph_kind(schema.graph.id())
			.ok_or_else(|| FerrosGrynnError::graph_does_not_exist(schema.graph.id().to_string()))?;
		if graph_kind != schema.graph.kind() {
			return Err(FerrosGrynnError::conversion_error(
				schema.graph.kind().name(),
				graph_kind.name(),
			));
		}
		let dictionary = self.read_root().dictionary.clone();
		for field in &mut schema.fields {
			field.key = Some(dictionary.get(field.name.as_str()).ok_or_else(|| {
				FerrosGrynnError::invalid_argument(format!(
					"property '{}' must be interned before defining its table schema",
					field.name
				))
			})?);
		}
		if !schema.indexes.is_empty() {
			self.ensure_graph_write(&schema.graph)?;
			if let Some(write) = self.graphs.get_mut(schema.graph.id()) {
				write.force_persist = true;
			}
		}
		self.tables.insert(schema.name.as_str().to_owned(), Some(schema));
		Ok(())
	}

	pub fn remove_table(&mut self, name: &str) -> FerrosGrynnResult<()> {
		self.ensure_writable()?;
		self.ensure_live()?;
		let schema =
			self.visible_table(name).ok_or_else(|| FerrosGrynnError::table_does_not_exist(name))?;
		if !schema.indexes.is_empty() {
			self.ensure_graph_write(&schema.graph)?;
			if let Some(write) = self.graphs.get_mut(schema.graph.id()) {
				write.force_persist = true;
			}
		}
		self.tables.insert(name.to_owned(), None);
		Ok(())
	}

	pub fn table(&self, name: &str) -> FerrosGrynnResult<Arc<TableSchema>> {
		self.ensure_live()?;
		let root = self.read_root();
		self.record_read(&root, ReadKey::Table(name.to_owned()));
		match self.tables.get(name) {
			Some(Some(schema)) => Ok(Arc::new(schema.clone())),
			Some(None) => Err(FerrosGrynnError::table_does_not_exist(name)),
			None => root
				.tables
				.get(name)
				.cloned()
				.ok_or_else(|| FerrosGrynnError::table_does_not_exist(name)),
		}
	}

	fn ensure_writable(&self) -> FerrosGrynnResult<()> {
		if self.options.access == TransactionAccess::ReadOnly {
			return Err(FerrosGrynnError::read_only_transaction(self.id));
		}
		Ok(())
	}

	fn ensure_graph_write(&mut self, reference: &GraphRef) -> FerrosGrynnResult<()> {
		ensure_store_reference(reference, self.store.inner.graph_storage_mode())?;
		if let Some(write) = self.graphs.get_mut(reference.id()) {
			write.ensure_materialized(&self.store)?;
			return Ok(());
		}
		let root = self.read_root();
		let stored = root
			.graphs
			.get(reference.id())
			.ok_or_else(|| FerrosGrynnError::graph_does_not_exist(reference.id().to_string()))?;
		if stored.graph.kind() != reference.kind() {
			return Err(FerrosGrynnError::conversion_error(
				reference.kind().name(),
				stored.graph.kind().name(),
			));
		}
		let base = match &stored.graph {
			StoredGraphValue::Materialized(graph) => graph.clone(),
			StoredGraphValue::Local(graph) => {
				let local = self.store.inner.backend.local().ok_or_else(|| {
					FerrosGrynnError::invalid_storage_file(
						"local graph is attached to an in-memory backend",
					)
				})?;
				Arc::new(local.materialize_graph(self.local_snapshot()?, graph)?)
			}
		};
		self.graphs.insert(reference.id().clone(), GraphWrite {
			name: stored.name.clone(),
			base: Some(base.clone()),
			local: None,
			base_ts: root.commit_ts,
			value: Some(MutableGraphValue::from_frozen(&base)?),
			force_persist: false,
		});
		Ok(())
	}

	fn ensure_graph_mutation_write(&mut self, reference: &GraphRef) -> FerrosGrynnResult<()> {
		ensure_store_reference(reference, self.store.inner.graph_storage_mode())?;
		if self.graphs.contains_key(reference.id()) {
			return Ok(());
		}
		let root = self.read_root();
		let stored = root
			.graphs
			.get(reference.id())
			.ok_or_else(|| FerrosGrynnError::graph_does_not_exist(reference.id().to_string()))?;
		if stored.graph.kind() != reference.kind() {
			return Err(FerrosGrynnError::conversion_error(
				reference.kind().name(),
				stored.graph.kind().name(),
			));
		}
		let (base, local, value) = match &stored.graph {
			StoredGraphValue::Materialized(graph) => {
				(Some(graph.clone()), None, Some(MutableGraphValue::from_frozen(graph)?))
			}
			StoredGraphValue::Local(graph) => (
				None,
				Some(LocalGraphWrite {
					base: graph.clone(),
					snapshot: self.local_snapshot()?,
					delta: LocalGraphDelta::new(
						graph.kind(),
						graph.node_capacity(),
						graph.revision(),
					),
				}),
				None,
			),
		};
		self.graphs.insert(reference.id().clone(), GraphWrite {
			name: stored.name.clone(),
			base,
			local,
			base_ts: root.commit_ts,
			value,
			force_persist: false,
		});
		Ok(())
	}
}

/// Validates the public catalog-name bounds.
pub(super) fn ensure_store_reference(
	reference: &GraphRef,
	expected: GraphStorageMode,
) -> FerrosGrynnResult<()> {
	if reference.storage_mode() != expected {
		return Err(FerrosGrynnError::invalid_argument(format!(
			"graph reference mode {:?} does not belong to this {:?} datastore",
			reference.storage_mode(),
			expected
		)));
	}
	Ok(())
}
