use std::{
	borrow::Cow,
	cmp::Ordering,
	collections::{BTreeMap, BTreeSet, VecDeque},
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

/// Undirected or arborescence orientation of a tree-family topology.
#[derive(Debug, Default, Clone, Copy, PartialOrd, PartialEq, Eq, Ord, Hash)]
#[non_exhaustive]
pub enum TreeOrientation {
	/// Tree edges have undirected incidence.
	#[default]
	Undirected,
	/// Tree edges point from a parent toward its children.
	OutArborescence,
	/// Tree edges point from children toward their parent.
	InArborescence,
}

#[derive(Debug, Clone)]
/// Forest storage state.
struct ForestStorage {
	/// Nodes associated with this forest storage.
	nodes: NodeColumns,
	/// Edges associated with this forest storage.
	edges: EdgeColumns,
	/// Orientation associated with this forest storage.
	orientation: TreeOrientation,
	/// Parent associated with this forest storage.
	parent: CowVec<Option<AtomId>>,
	/// Parent edge associated with this forest storage.
	parent_edge: CowVec<Option<AtomId>>,
	/// Children associated with this forest storage.
	children: CowVec<Vec<AtomId>>,
	/// Roots associated with this forest storage.
	roots: Arc<BTreeSet<AtomId>>,
}

impl ForestStorage {
	/// Creates a new `ForestStorage` value.
	fn new(orientation: TreeOrientation) -> Self {
		Self {
			nodes: NodeColumns::new(),
			edges: EdgeColumns::new(),
			orientation,
			parent: CowVec::new(),
			parent_edge: CowVec::new(),
			children: CowVec::new(),
			roots: Arc::new(BTreeSet::new()),
		}
	}

	/// Returns the reroot undirected result.
	fn reroot_undirected(&mut self, root: AtomId) -> FerrosGrynnResult<()> {
		if self.orientation != TreeOrientation::Undirected {
			return Err(constraint("only an undirected tree can be re-rooted"));
		}
		if !self.nodes.contains(root) {
			return Err(atom_does_not_exist(root));
		}
		let mut adjacency = BTreeMap::<AtomId, Vec<(AtomId, AtomId)>>::new();
		for edge in self.edges.ids().iter().copied() {
			let topology = self.edges.topology(edge).ok_or_else(|| atom_does_not_exist(edge))?;
			let (left, right, direction) = topology
				.local_binary()
				.ok_or_else(|| constraint("a rooted tree requires local binary edges"))?;
			if direction != EdgeDirection::Undirected {
				return Err(constraint("an undirected tree contains a directed edge"));
			}
			adjacency.entry(left).or_default().push((right, edge));
			adjacency.entry(right).or_default().push((left, edge));
		}
		for node in self.nodes.ids().iter().copied() {
			let slot = node.slot() as usize;
			self.parent[slot] = None;
			self.parent_edge[slot] = None;
			self.children[slot].clear();
		}
		let mut seen = BTreeSet::from([root]);
		let mut queue = VecDeque::from([root]);
		while let Some(parent) = queue.pop_front() {
			for (child, edge) in adjacency.get(&parent).into_iter().flatten().copied() {
				if !seen.insert(child) {
					continue;
				}
				self.parent[child.slot() as usize] = Some(parent);
				self.parent_edge[child.slot() as usize] = Some(edge);
				self.children[parent.slot() as usize].push(child);
				queue.push_back(child);
			}
		}
		if seen.len() != self.nodes.len() {
			return Err(constraint("a rooted tree must contain one connected component"));
		}
		self.roots = Arc::new(BTreeSet::from([root]));
		Ok(())
	}

	/// Inserts root new.
	fn insert_root_new(
		&mut self,
		record_id: RecordId,
		label: Option<Text>,
		properties: PropertyMap,
	) -> FerrosGrynnResult<AtomId> {
		ensure_unique_record(&self.nodes, &self.edges, &record_id)?;
		let id = self.nodes.insert_new(record_id, label, properties)?;
		self.ensure_node_slot(id);
		Arc::make_mut(&mut self.roots).insert(id);
		Ok(id)
	}

	/// Inserts root at.
	fn insert_root_at(
		&mut self,
		id: AtomId,
		record_id: RecordId,
		label: Option<Text>,
		properties: PropertyMap,
	) -> FerrosGrynnResult<()> {
		ensure_unique_record(&self.nodes, &self.edges, &record_id)?;
		self.nodes.insert_at(id, record_id, label, properties)?;
		self.ensure_node_slot(id);
		Arc::make_mut(&mut self.roots).insert(id);
		Ok(())
	}

	/// Returns the connect new result.
	fn connect_new(
		&mut self,
		parent: AtomId,
		child: AtomId,
		record_id: RecordId,
		label: Option<Text>,
		properties: PropertyMap,
		weight: Option<Number>,
	) -> FerrosGrynnResult<AtomId> {
		self.validate_connection(parent, child)?;
		ensure_unique_record(&self.nodes, &self.edges, &record_id)?;
		let topology = topology_for(self.orientation, parent, child);
		let edge = self.edges.insert_new(record_id, topology, label, properties, weight)?;
		self.attach(parent, child, edge);
		Ok(edge)
	}

	/// Returns the connect at result.
	fn connect_at(
		&mut self,
		id: AtomId,
		parent: AtomId,
		child: AtomId,
		record_id: RecordId,
		topology: EdgeTopology,
		label: Option<Text>,
		properties: PropertyMap,
		weight: Option<Number>,
	) -> FerrosGrynnResult<()> {
		self.validate_connection(parent, child)?;
		ensure_unique_record(&self.nodes, &self.edges, &record_id)?;
		self.edges.insert_at(id, record_id, topology, label, properties, weight)?;
		self.attach(parent, child, id);
		Ok(())
	}

	/// Validates connection.
	fn validate_connection(&self, parent: AtomId, child: AtomId) -> FerrosGrynnResult<()> {
		for id in [parent, child] {
			if id.kind() != Some(AtomKind::Node) {
				return Err(atom_kind_mismatch(id, AtomKind::Node));
			}
			if !self.nodes.contains(id) {
				return Err(atom_does_not_exist(id));
			}
		}
		if parent == child {
			return Err(constraint("self-loops are forbidden"));
		}
		if !self.roots.contains(&child) {
			return Err(constraint("the child endpoint must be a component root"));
		}
		if self.component_root(parent) == Some(child) {
			return Err(constraint("the edge would create a cycle"));
		}
		Ok(())
	}

	/// Attaches forest storage.
	fn attach(&mut self, parent: AtomId, child: AtomId, edge: AtomId) {
		let child_slot = child.slot() as usize;
		self.parent[child_slot] = Some(parent);
		self.parent_edge[child_slot] = Some(edge);
		self.children[parent.slot() as usize].push(child);
		Arc::make_mut(&mut self.roots).remove(&child);
	}

	/// Returns the component root result.
	fn component_root(&self, mut id: AtomId) -> Option<AtomId> {
		if !self.nodes.contains(id) {
			return None;
		}
		while let Some(parent) = self.parent.get(id.slot() as usize).copied().flatten() {
			id = parent;
		}
		Some(id)
	}

	/// Returns the parent result.
	fn parent(&self, id: AtomId) -> Option<AtomId> {
		self.nodes.contains(id).then(|| self.parent.get(id.slot() as usize).copied().flatten())?
	}

	/// Returns the children result.
	fn children(&self, id: AtomId) -> &[AtomId] {
		if !self.nodes.contains(id) {
			return &[];
		}
		self.children.get(id.slot() as usize).map_or(&[], Vec::as_slice)
	}

	/// Detaches edge.
	fn detach_edge(&mut self, edge: AtomId) -> bool {
		if !self.edges.contains(edge) {
			return false;
		}
		let child =
			self.nodes.ids().iter().copied().find(|id| {
				self.parent_edge.get(id.slot() as usize).copied().flatten() == Some(edge)
			});
		if let Some(child) = child {
			let child_slot = child.slot() as usize;
			if let Some(parent) = self.parent[child_slot].take()
				&& let Some(children) = self.children.get_mut(parent.slot() as usize)
			{
				children.retain(|candidate| *candidate != child);
			}
			self.parent_edge[child_slot] = None;
			Arc::make_mut(&mut self.roots).insert(child);
		}
		self.edges.remove(edge)
	}

	/// Removes node.
	fn remove_node(&mut self, id: AtomId) -> bool {
		if !self.nodes.contains(id) {
			return false;
		}
		let slot = id.slot() as usize;
		let mut incident = self.children[slot]
			.iter()
			.filter_map(|child| self.parent_edge[child.slot() as usize])
			.collect::<Vec<_>>();
		if let Some(edge) = self.parent_edge[slot] {
			incident.push(edge);
		}
		for edge in incident {
			self.detach_edge(edge);
		}
		Arc::make_mut(&mut self.roots).remove(&id);
		self.parent[slot] = None;
		self.parent_edge[slot] = None;
		self.children[slot].clear();
		self.nodes.remove(id)
	}

	/// Returns the incident edges result.
	fn incident_edges(&self, id: AtomId) -> Vec<AtomId> {
		if !self.nodes.contains(id) {
			return Vec::new();
		}
		let slot = id.slot() as usize;
		let mut incident = self.children[slot]
			.iter()
			.filter_map(|child| self.parent_edge[child.slot() as usize])
			.collect::<Vec<_>>();
		if let Some(edge) = self.parent_edge[slot] {
			incident.push(edge);
		}
		incident
	}

	/// Ensures node slot.
	fn ensure_node_slot(&mut self, id: AtomId) {
		let slot = id.slot() as usize;
		while self.parent.len() <= slot {
			self.parent.push(None);
			self.parent_edge.push(None);
			self.children.push(Vec::new());
		}
	}
}

/// Tree-family graph allowing several disconnected acyclic components.
pub struct Forest<State = Frozen> {
	/// Metadata associated with this forest.
	metadata: GraphMetadata,
	/// Storage associated with this forest.
	storage: ForestStorage,
	/// State associated with this forest.
	state: PhantomData<State>,
}

/// Connected acyclic graph with a selected tree orientation.
pub struct Tree<State = Frozen> {
	/// Metadata associated with this tree.
	metadata: GraphMetadata,
	/// Storage associated with this tree.
	storage: ForestStorage,
	/// State associated with this tree.
	state: PhantomData<State>,
}

/// Tree-family graph retaining an explicitly selected root atom.
pub struct RootedTree<State = Frozen> {
	/// Metadata associated with this rooted tree.
	metadata: GraphMetadata,
	/// Storage associated with this rooted tree.
	storage: ForestStorage,
	/// Root associated with this rooted tree.
	root: AtomId,
	/// State associated with this rooted tree.
	state: PhantomData<State>,
}

pub type MutableForest = Forest<Mutable>;
pub type FrozenForest = Forest<Frozen>;
pub type MutableTree = Tree<Mutable>;
pub type FrozenTree = Tree<Frozen>;
pub type MutableRootedTree = RootedTree<Mutable>;
pub type FrozenRootedTree = RootedTree<Frozen>;

/// Expands the impl slot state declarations used by this module.
macro_rules! impl_slot_state {
	($type:ident) => {
		impl<State> $type<State> {
			/// Returns the allocated slot count for the selected atom kind.
			pub(crate) fn slot_capacity(&self, kind: AtomKind) -> usize {
				match kind {
					AtomKind::Node => self.storage.nodes.slot_capacity(),
					AtomKind::Edge => self.storage.edges.slot_capacity(),
				}
			}
		}
	};
}

impl_slot_state!(Forest);
impl_slot_state!(Tree);
impl_slot_state!(RootedTree);

impl Forest<Mutable> {
	/// Constructs `Forest<Mutable>` from the supplied initial state.
	pub fn new(orientation: TreeOrientation) -> Self {
		Self {
			metadata: GraphMetadata::new(),
			storage: ForestStorage::new(orientation),
			state: PhantomData,
		}
	}

	/// Returns add root for `Forest<Mutable>`.
	///
	/// # Errors
	///
	/// Propagates errors returned by the underlying operation.
	pub fn add_root(
		&mut self,
		label: Option<Text>,
		properties: PropertyMap,
	) -> FerrosGrynnResult<AtomId> {
		self.add_root_with_id(generated_record_id(AtomKind::Node)?, label, properties)
	}

	/// Returns the add root with id result.
	pub(crate) fn add_root_with_id(
		&mut self,
		record_id: RecordId,
		label: Option<Text>,
		properties: PropertyMap,
	) -> FerrosGrynnResult<AtomId> {
		self.metadata.ensure_can_bump()?;
		let id = self.storage.insert_root_new(record_id, label, properties)?;
		self.metadata.bump(Some(DirtyTarget::Atom(id)))?;
		Ok(id)
	}

	/// Returns connect for `Forest<Mutable>`.
	///
	/// # Errors
	///
	/// Propagates errors returned by the underlying operation.
	pub fn connect(
		&mut self,
		parent: AtomId,
		child: AtomId,
		label: Option<Text>,
		properties: PropertyMap,
	) -> FerrosGrynnResult<AtomId> {
		self.connect_with_id(parent, child, generated_record_id(AtomKind::Edge)?, label, properties)
	}

	/// Returns the connect with id result.
	pub(crate) fn connect_with_id(
		&mut self,
		parent: AtomId,
		child: AtomId,
		record_id: RecordId,
		label: Option<Text>,
		properties: PropertyMap,
	) -> FerrosGrynnResult<AtomId> {
		self.metadata.ensure_can_bump()?;
		let edge = self.storage.connect_new(parent, child, record_id, label, properties, None)?;
		self.metadata.bump(Some(DirtyTarget::Atom(edge)))?;
		Ok(edge)
	}

	/// Returns freeze for `Forest<Mutable>`.
	pub fn freeze(self) -> Forest<Frozen> {
		Forest {
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

	/// Performs the flush operation for `Forest<Mutable>`.
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

impl Default for Forest<Mutable> {
	/// Constructs the default initial state of `Forest<Mutable>`.
	fn default() -> Self {
		Self::new(TreeOrientation::Undirected)
	}
}

impl Forest<Frozen> {
	/// Returns thaw for `Forest<Frozen>`.
	pub fn thaw(self) -> Forest<Mutable> {
		Forest {
			metadata: self.metadata,
			storage: self.storage,
			state: PhantomData,
		}
	}
}

impl Clone for Forest<Frozen> {
	/// Returns an owned clone of this `Forest<Frozen>`.
	fn clone(&self) -> Self {
		Self {
			metadata: self.metadata.clone(),
			storage: self.storage.clone(),
			state: PhantomData,
		}
	}
}

impl Tree<Mutable> {
	/// Constructs `Tree<Mutable>` from the supplied initial state.
	///
	/// # Errors
	///
	/// Propagates errors returned by the underlying operation.
	pub fn new(
		orientation: TreeOrientation,
		root_label: Option<Text>,
		root_properties: PropertyMap,
	) -> FerrosGrynnResult<Self> {
		let mut forest = Forest::new(orientation);
		forest.add_root(root_label, root_properties)?;
		Self::try_from(forest)
	}

	/// Returns add child for `Tree<Mutable>`.
	///
	/// # Errors
	///
	/// Propagates errors returned by the underlying operation.
	pub fn add_child(
		&mut self,
		parent: AtomId,
		node_label: Option<Text>,
		node_properties: PropertyMap,
		edge_label: Option<Text>,
		edge_properties: PropertyMap,
	) -> FerrosGrynnResult<(AtomId, AtomId)> {
		if !self.storage.nodes.contains(parent) {
			return Err(atom_does_not_exist(parent));
		}
		self.metadata.ensure_can_bump()?;
		let child_record = generated_record_id(AtomKind::Node)?;
		let edge_record = generated_record_id(AtomKind::Edge)?;
		let child = self.storage.insert_root_new(child_record, node_label, node_properties)?;
		let edge = match self.storage.connect_new(
			parent,
			child,
			edge_record,
			edge_label,
			edge_properties,
			None,
		) {
			Ok(edge) => edge,
			Err(error) => {
				self.storage.remove_node(child);
				return Err(error);
			}
		};
		self.metadata.bump(Some(DirtyTarget::Atom(child)))?;
		Ok((child, edge))
	}

	/// Returns freeze for `Tree<Mutable>`.
	pub fn freeze(self) -> Tree<Frozen> {
		Tree {
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

	/// Performs the flush operation for `Tree<Mutable>`.
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

impl Tree<Frozen> {
	/// Returns thaw for `Tree<Frozen>`.
	pub fn thaw(self) -> Tree<Mutable> {
		Tree {
			metadata: self.metadata,
			storage: self.storage,
			state: PhantomData,
		}
	}
}

impl Clone for Tree<Frozen> {
	/// Returns an owned clone of this `Tree<Frozen>`.
	fn clone(&self) -> Self {
		Self {
			metadata: self.metadata.clone(),
			storage: self.storage.clone(),
			state: PhantomData,
		}
	}
}

impl RootedTree<Mutable> {
	/// Creates a rooted tree with one initial root node.
	/// # Errors
	///
	/// Propagates tree-construction failures and rejects a tree without a root.
	pub fn new(
		orientation: TreeOrientation,
		root_label: Option<Text>,
		root_properties: PropertyMap,
	) -> FerrosGrynnResult<Self> {
		let tree = Tree::new(orientation, root_label, root_properties)?;
		let root = tree
			.storage
			.roots
			.first()
			.copied()
			.ok_or_else(|| constraint("a rooted tree must contain one root"))?;
		Ok(Self {
			metadata: tree.metadata,
			storage: tree.storage,
			root,
			state: PhantomData,
		})
	}

	/// Returns add child for `RootedTree<Mutable>`.
	///
	/// # Errors
	///
	/// Propagates validation or execution failures from the underlying
	/// operation.
	pub fn add_child(
		&mut self,
		parent: AtomId,
		node_label: Option<Text>,
		node_properties: PropertyMap,
		edge_label: Option<Text>,
		edge_properties: PropertyMap,
	) -> FerrosGrynnResult<(AtomId, AtomId)> {
		let mut tree = Tree {
			metadata: std::mem::take(&mut self.metadata),
			storage: std::mem::replace(
				&mut self.storage,
				ForestStorage::new(TreeOrientation::Undirected),
			),
			state: PhantomData,
		};
		let result =
			tree.add_child(parent, node_label, node_properties, edge_label, edge_properties);
		self.metadata = tree.metadata;
		self.storage = tree.storage;
		result
	}

	/// Returns freeze for `RootedTree<Mutable>`.
	pub fn freeze(self) -> RootedTree<Frozen> {
		RootedTree {
			metadata: self.metadata.into_frozen(),
			storage: self.storage,
			root: self.root,
			state: PhantomData,
		}
	}

	/// Clones the COW layout while retaining identity, revision and dirty
	/// metadata.
	pub(crate) fn clone_preserving_state(&self) -> Self {
		Self {
			metadata: self.metadata.clone(),
			storage: self.storage.clone(),
			root: self.root,
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

	/// Performs the flush operation for `RootedTree<Mutable>`.
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

impl RootedTree<Frozen> {
	/// Returns thaw for `RootedTree<Frozen>`.
	pub fn thaw(self) -> RootedTree<Mutable> {
		RootedTree {
			metadata: self.metadata,
			storage: self.storage,
			root: self.root,
			state: PhantomData,
		}
	}
}

impl Clone for RootedTree<Frozen> {
	/// Returns an owned clone of this `RootedTree<Frozen>`.
	fn clone(&self) -> Self {
		Self {
			metadata: self.metadata.clone(),
			storage: self.storage.clone(),
			root: self.root,
			state: PhantomData,
		}
	}
}

/// Expands the impl forest read declarations used by this module.
macro_rules! impl_forest_read {
	($type:ident, $kind:expr) => {
		impl<State> GraphRead for $type<State> {
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

			/// Returns the graph identity shared by snapshots of this graph.
			fn identity(&self) -> Uuid {
				self.metadata.identity()
			}

			/// Returns the revision represented by this graph snapshot.
			fn revision(&self) -> u64 {
				self.metadata.revision()
			}

			/// Returns the structural graph family represented by this implementation.
			fn kind(&self) -> GraphKind {
				$kind
			}

			/// Borrows the graph properties through the shared read surface.
			fn properties(&self) -> FerrosGrynnResult<Cow<'_, PropertyMap>> {
				Ok(Cow::Borrowed(self.metadata.properties()))
			}

			/// Returns the structural constraints enforced by this graph family.
			fn topology_constraints(&self) -> GraphTopologyConstraints {
				self.metadata.topology_constraints()
			}

			/// Iterates over live node identifiers in the current graph.
			fn node_ids(&self) -> FerrosGrynnResult<Self::NodeIds<'_>> {
				Ok(FallibleAtomIds::new(AtomIds::new(self.storage.nodes.ids().iter())))
			}

			/// Iterates over live edge identifiers in the current graph.
			fn edge_ids(&self) -> FerrosGrynnResult<Self::EdgeIds<'_>> {
				Ok(FallibleAtomIds::new(AtomIds::new(self.storage.edges.ids().iter())))
			}

			/// Looks up a node view after validating its local identifier.
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

			/// Looks up an edge view after validating its local identifier.
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

			/// Returns the storage mode of the underlying graph reader.
			fn storage_mode(&self) -> super::GraphStorageMode {
				self.metadata.storage_mode()
			}

			/// Borrows the durable graph reference when this reader has one.
			fn graph_ref(&self) -> Option<&super::GraphRef> {
				self.metadata.graph_ref()
			}

			/// Borrows the origin retained for explicit graph write-back.
			fn materialization_origin(&self) -> Option<&super::MaterializationOrigin> {
				self.metadata.origin()
			}
		}
	};
}

impl_forest_read!(Forest, GraphKind::Forest);
impl_forest_read!(Tree, GraphKind::Tree);
impl_forest_read!(RootedTree, GraphKind::RootedTree);

/// Expands the impl forest common declarations used by this module.
macro_rules! impl_forest_common {
	($type:ident) => {
		impl<State> $type<State> {
			/// Returns the graph identity shared by snapshots of this graph.
			pub const fn identity(&self) -> Uuid {
				self.metadata.identity()
			}

			/// Returns the revision represented by this graph snapshot.
			pub const fn revision(&self) -> u64 {
				self.metadata.revision()
			}

			/// Borrows the graph properties through the shared read surface.
			pub const fn properties(&self) -> &PropertyMap {
				self.metadata.properties()
			}

			/// Returns the structural constraints enforced by this graph family.
			pub const fn topology_constraints(&self) -> GraphTopologyConstraints {
				self.metadata.topology_constraints()
			}

			/// Returns the orientation enforced by this tree family.
			pub const fn orientation(&self) -> TreeOrientation {
				self.storage.orientation
			}

			/// Returns the number of live nodes in this graph.
			pub const fn node_count(&self) -> usize {
				self.storage.nodes.len()
			}

			/// Returns the number of live edges in this graph.
			pub const fn edge_count(&self) -> usize {
				self.storage.edges.len()
			}

			/// Looks up a node view after validating its local identifier.
			pub fn node(&self, id: AtomId) -> Option<NodeView<'_>> {
				Some(NodeView::new(
					id,
					self.storage.nodes.record_id(id)?,
					self.storage.nodes.label(id)?.as_ref(),
					self.storage.nodes.properties(id)?,
					None,
				))
			}

			/// Looks up an edge view after validating its local identifier.
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

			/// Returns the parent of the requested node, if present.
			pub fn parent(&self, id: AtomId) -> Option<AtomId> {
				self.storage.parent(id)
			}

			/// Borrows the child identifiers of the requested node.
			pub fn children(&self, id: AtomId) -> &[AtomId] {
				self.storage.children(id)
			}

			/// Iterates over roots in this forest or tree.
			pub fn roots(&self) -> impl ExactSizeIterator<Item = AtomId> + '_ {
				self.storage.roots.iter().copied()
			}

			/// Copies this family's topology into a general graph.
			///
			/// # Errors
			///
			/// Propagates failures while reading and validating the source graph.
			pub fn into_graph(self) -> FerrosGrynnResult<Graph<State>> {
				let converted = Graph::<Mutable>::from_read_preserving(&self)?;
				let (_, storage) = converted.into_raw();
				Ok(Graph::from_raw(self.metadata, storage))
			}
		}

		impl<State> Debug for $type<State> {
			/// Formats graph metadata and cardinalities for debugging.
			fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
				formatter
					.debug_struct(stringify!($type))
					.field("identity", &self.identity())
					.field("revision", &self.revision())
					.field("orientation", &self.orientation())
					.field("nodes", &self.node_count())
					.field("edges", &self.edge_count())
					.finish()
			}
		}

		impl<State> PartialEq for $type<State> {
			/// Compares graph identity and revision without comparing contents.
			fn eq(&self, other: &Self) -> bool {
				(self.identity(), self.revision()) == (other.identity(), other.revision())
			}
		}
		impl<State> Eq for $type<State> {}
		impl<State> PartialOrd for $type<State> {
			/// Returns the total graph identity and revision ordering.
			fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
				Some(self.cmp(other))
			}
		}
		impl<State> Ord for $type<State> {
			/// Orders graphs by identity and revision.
			fn cmp(&self, other: &Self) -> Ordering {
				(self.identity(), self.revision()).cmp(&(other.identity(), other.revision()))
			}
		}
		impl<State> Hash for $type<State> {
			/// Feeds graph identity and revision into the supplied hasher.
			fn hash<H: Hasher>(&self, state: &mut H) {
				self.identity().hash(state);
				self.revision().hash(state);
			}
		}
	};
}

impl_forest_common!(Forest);
impl_forest_common!(Tree);
impl_forest_common!(RootedTree);

impl<State> RootedTree<State> {
	/// Returns the stored root of this `RootedTree<State>`.
	pub const fn root(&self) -> AtomId {
		self.root
	}

	/// Builds a rooted tree with an explicit authoritative root.
	///
	/// # Errors
	///
	/// Returns a graph constraint error when the source is not a tree, the root
	/// is absent, or a directed source has a different structural root.
	pub fn try_from_graph_with_root(value: Graph<State>, root: AtomId) -> FerrosGrynnResult<Self> {
		Self::try_from_tree_with_root(Tree::try_from(value)?, root)
	}

	/// Attempts from tree with root.
	fn try_from_tree_with_root(mut tree: Tree<State>, root: AtomId) -> FerrosGrynnResult<Self> {
		if !tree.storage.nodes.contains(root) {
			return Err(atom_does_not_exist(root));
		}
		if tree.storage.orientation == TreeOrientation::Undirected {
			tree.storage.reroot_undirected(root)?;
		} else if tree.storage.roots.first().copied() != Some(root) {
			return Err(constraint("the explicit root differs from the directed structural root"));
		}
		Ok(Self {
			metadata: tree.metadata,
			storage: tree.storage,
			root,
			state: PhantomData,
		})
	}
}

impl GraphMut for Forest<Mutable> {
	/// Returns properties mut for `Forest<Mutable>`.
	fn properties_mut(&mut self) -> FerrosGrynnResult<&mut PropertyMap> {
		self.metadata.properties_mut()
	}

	/// Returns node label mut for `Forest<Mutable>`.
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

	/// Returns node properties mut for `Forest<Mutable>`.
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

	/// Returns edge label mut for `Forest<Mutable>`.
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

	/// Returns edge properties mut for `Forest<Mutable>`.
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

	/// Removes atom for `Forest<Mutable>`.
	fn remove_atom(&mut self, id: AtomId) -> FerrosGrynnResult<()> {
		self.metadata.ensure_can_bump()?;
		let dependent_edges = if id.is_node() {
			self.storage.incident_edges(id)
		} else {
			Vec::new()
		};
		let removed = match id.kind() {
			Some(AtomKind::Node) => self.storage.remove_node(id),
			Some(AtomKind::Edge) => self.storage.detach_edge(id),
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

/// Expands the impl tree mut declarations used by this module.
macro_rules! impl_tree_mut {
	($type:ident $(, $root:ident)?) => {
		impl GraphMut for $type<Mutable> {
			/// Borrows graph properties for a checked mutation.
			fn properties_mut(&mut self) -> FerrosGrynnResult<&mut PropertyMap> {
				self.metadata.properties_mut()
			}

			/// Borrows a live node label for mutation.
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

			/// Borrows live node properties for mutation.
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

			/// Borrows a live edge label for mutation.
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

			/// Borrows live edge properties for mutation.
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

			/// Removes a live atom while preserving this graph family's invariants.
			fn remove_atom(&mut self, id: AtomId) -> FerrosGrynnResult<()> {
				if id.kind() != Some(AtomKind::Node) || self.storage.children(id).len() != 0 {
					return Err(constraint("only a leaf vertex can be removed without breaking the tree invariant"));
				}
				$(
					if self.$root == id {
						return Err(constraint("the root vertex cannot be removed"));
					}
				)?
				if self.storage.nodes.len() <= 1 {
					return Err(constraint("a tree cannot become empty"));
				}
				self.metadata.ensure_can_bump()?;
				let dependent_edges = self.storage.incident_edges(id);
				if !self.storage.remove_node(id) {
					return Err(atom_does_not_exist(id));
				}
				for edge in dependent_edges {
					self.metadata.mark_dirty(DirtyTarget::Atom(edge));
				}
				self.metadata.bump(Some(DirtyTarget::Atom(id)))
			}
		}
	};
}

impl_tree_mut!(Tree);
impl_tree_mut!(RootedTree, root);

impl<State> From<RootedTree<State>> for Tree<State> {
	/// Converts `RootedTree<State>` into `Tree<State>`.
	fn from(value: RootedTree<State>) -> Self {
		Self {
			metadata: value.metadata,
			storage: value.storage,
			state: PhantomData,
		}
	}
}

impl<State> From<RootedTree<State>> for Forest<State> {
	/// Converts `RootedTree<State>` into `Forest<State>`.
	fn from(value: RootedTree<State>) -> Self {
		Tree::from(value).into()
	}
}

impl<State> From<Tree<State>> for Forest<State> {
	/// Converts `Tree<State>` into `Forest<State>`.
	fn from(value: Tree<State>) -> Self {
		Self {
			metadata: value.metadata,
			storage: value.storage,
			state: PhantomData,
		}
	}
}

impl<State> TryFrom<Forest<State>> for Tree<State> {
	type Error = FerrosGrynnError;

	/// Converts `Forest<State>` into `Tree<State>` after validating the
	/// conversion.
	fn try_from(value: Forest<State>) -> Result<Self, Self::Error> {
		if value.storage.nodes.len() == 0 || value.storage.roots.len() != 1 {
			return Err(constraint("a tree must contain exactly one non-empty component"));
		}
		Ok(Self {
			metadata: value.metadata,
			storage: value.storage,
			state: PhantomData,
		})
	}
}

impl<State> TryFrom<Graph<State>> for Forest<State> {
	type Error = FerrosGrynnError;

	/// Converts `Graph<State>` into `Forest<State>` after validating the
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

impl<State> TryFrom<Graph<State>> for Tree<State> {
	type Error = FerrosGrynnError;

	/// Converts `Graph<State>` into `Tree<State>` after validating the
	/// conversion.
	fn try_from(value: Graph<State>) -> Result<Self, Self::Error> {
		Forest::try_from(value).and_then(Tree::try_from)
	}
}

impl<State> TryFrom<Graph<State>> for RootedTree<State> {
	type Error = FerrosGrynnError;

	/// Converts `Graph<State>` into `RootedTree<State>` after validating the
	/// conversion.
	fn try_from(value: Graph<State>) -> Result<Self, Self::Error> {
		let tree = Tree::try_from(value)?;
		if tree.storage.orientation == TreeOrientation::Undirected && tree.storage.nodes.len() > 1 {
			return Err(constraint("an undirected rooted tree requires an explicit root"));
		}
		let root = tree
			.storage
			.roots
			.first()
			.copied()
			.ok_or_else(|| constraint("a rooted tree must contain one root"))?;
		Self::try_from_tree_with_root(tree, root)
	}
}

impl QueryMaterialize for Forest<Mutable> {
	/// Returns query from for `Forest<Mutable>`.
	fn query_from<R: GraphRead>(source: &R) -> FerrosGrynnResult<Self> {
		let graph = Graph::<Mutable>::query_from(source)?;
		Self::try_from(graph)
	}
}

impl QueryMaterialize for Forest<Frozen> {
	/// Returns query from for `Forest<Frozen>`.
	fn query_from<R: GraphRead>(source: &R) -> FerrosGrynnResult<Self> {
		Forest::<Mutable>::query_from(source).map(Forest::freeze)
	}
}

impl QueryMaterialize for Tree<Mutable> {
	/// Returns query from for `Tree<Mutable>`.
	fn query_from<R: GraphRead>(source: &R) -> FerrosGrynnResult<Self> {
		Tree::try_from(Graph::<Mutable>::query_from(source)?)
	}
}

impl QueryMaterialize for Tree<Frozen> {
	/// Returns query from for `Tree<Frozen>`.
	fn query_from<R: GraphRead>(source: &R) -> FerrosGrynnResult<Self> {
		Tree::<Mutable>::query_from(source).map(Tree::freeze)
	}
}

/// Materialized edge state.
struct MaterializedEdge {
	/// Stable record identifier of the associated atom.
	record_id: RecordId,
	/// Topology associated with this materialized edge.
	topology: EdgeTopology,
	/// Label associated with this materialized edge.
	label: Option<Text>,
	/// Properties associated with this materialized edge.
	properties: PropertyMap,
	/// Weight associated with this materialized edge.
	weight: Option<Number>,
}

/// Builds storage.
fn build_storage<R: GraphRead>(source: &R) -> FerrosGrynnResult<ForestStorage> {
	let mut raw_edges = Vec::with_capacity(source.edge_count()?);
	let mut direction = None;
	let mut nodes = BTreeMap::new();
	for (index, id) in source.node_ids()?.enumerate() {
		let id = id?;
		let node = source.node(id)?.ok_or_else(|| atom_does_not_exist(id))?;
		if node.is_meta()? {
			return Err(constraint("forest nodes cannot contain child graphs"));
		}
		nodes.insert(id, index);
	}
	for id in source.edge_ids()? {
		let id = id?;
		let edge = source.edge(id)?.ok_or_else(|| atom_does_not_exist(id))?;
		if edge.is_meta()? {
			return Err(constraint("forest edges cannot contain transition graphs"));
		}
		let topology = edge.topology()?.into_owned();
		let (left, right, edge_direction) = topology.local_binary().ok_or_else(|| {
			constraint("forest edges must be local binary vertex-to-vertex edges")
		})?;
		if !nodes.contains_key(&left) || !nodes.contains_key(&right) {
			return Err(constraint("forest edges can only reference vertices"));
		}
		if edge_direction == EdgeDirection::Bidirectional {
			return Err(constraint("bidirectional edges are not a tree orientation"));
		}
		if let Some(expected) = direction {
			if expected != edge_direction {
				return Err(constraint("a forest cannot mix directed and undirected edges"));
			}
		} else {
			direction = Some(edge_direction);
		}
		raw_edges.push((id, left, right, MaterializedEdge {
			record_id: edge.record_id()?.into_owned(),
			topology,
			label: edge.label_text()?.map(Cow::into_owned),
			properties: edge.properties()?.into_owned(),
			weight: edge.weight(),
		}));
	}

	validate_acyclic(&nodes, &raw_edges)?;
	let orientation = match direction {
		None | Some(EdgeDirection::Undirected) => TreeOrientation::Undirected,
		Some(EdgeDirection::Directed) => infer_directed_orientation(&nodes, &raw_edges)?,
		Some(EdgeDirection::Bidirectional) => {
			return Err(constraint("bidirectional edges are not supported"));
		}
	};
	let parent_map = build_parent_map(&nodes, &raw_edges, orientation);
	let mut storage = ForestStorage::new(orientation);
	for id in source.node_ids()? {
		let id = id?;
		let node = source.node(id)?.ok_or_else(|| atom_does_not_exist(id))?;
		storage.insert_root_at(
			id,
			node.record_id()?.into_owned(),
			node.label_text()?.map(Cow::into_owned),
			node.properties()?.into_owned(),
		)?;
	}
	for (id, left, right, edge) in raw_edges {
		let (parent, child) = parent_map.get(&id).copied().unwrap_or((left, right));
		storage.connect_at(
			id,
			parent,
			child,
			edge.record_id,
			edge.topology,
			edge.label,
			edge.properties,
			edge.weight,
		)?;
	}
	Ok(storage)
}

/// Validates acyclic.
fn validate_acyclic(
	nodes: &BTreeMap<AtomId, usize>,
	edges: &[(AtomId, AtomId, AtomId, MaterializedEdge)],
) -> FerrosGrynnResult<()> {
	let mut parent = (0..nodes.len()).collect::<Vec<_>>();
	let mut pairs = BTreeSet::new();
	for (_, left, right, _) in edges {
		if left == right {
			return Err(constraint("self-loops are forbidden"));
		}
		let pair = if left < right {
			(*left, *right)
		} else {
			(*right, *left)
		};
		if !pairs.insert(pair) {
			return Err(constraint("parallel edges are forbidden"));
		}
		let left_index = nodes[left];
		let right_index = nodes[right];
		let left_root = find_root(&mut parent, left_index);
		let right_root = find_root(&mut parent, right_index);
		if left_root == right_root {
			return Err(constraint("the topology contains a cycle"));
		}
		parent[right_root] = left_root;
	}
	Ok(())
}

/// Finds root.
fn find_root(parent: &mut [usize], mut index: usize) -> usize {
	while parent[index] != index {
		parent[index] = parent[parent[index]];
		index = parent[index];
	}
	index
}

/// Returns the infer directed orientation result.
fn infer_directed_orientation(
	nodes: &BTreeMap<AtomId, usize>,
	edges: &[(AtomId, AtomId, AtomId, MaterializedEdge)],
) -> FerrosGrynnResult<TreeOrientation> {
	let mut inbound = vec![0_u32; nodes.len()];
	let mut outbound = vec![0_u32; nodes.len()];
	for (_, source, target, _) in edges {
		outbound[nodes[source]] += 1;
		inbound[nodes[target]] += 1;
	}
	if inbound.iter().all(|degree| *degree <= 1) {
		return Ok(TreeOrientation::OutArborescence);
	}
	if outbound.iter().all(|degree| *degree <= 1) {
		return Ok(TreeOrientation::InArborescence);
	}
	Err(constraint("directed forest edges do not form consistently oriented arborescences"))
}

/// Builds parent map.
fn build_parent_map(
	nodes: &BTreeMap<AtomId, usize>,
	edges: &[(AtomId, AtomId, AtomId, MaterializedEdge)],
	orientation: TreeOrientation,
) -> BTreeMap<AtomId, (AtomId, AtomId)> {
	if orientation == TreeOrientation::OutArborescence {
		return edges.iter().map(|(id, source, target, _)| (*id, (*source, *target))).collect();
	}
	if orientation == TreeOrientation::InArborescence {
		return edges.iter().map(|(id, source, target, _)| (*id, (*target, *source))).collect();
	}

	let mut adjacency = BTreeMap::<AtomId, Vec<(AtomId, AtomId)>>::new();
	for (edge, left, right, _) in edges {
		adjacency.entry(*left).or_default().push((*right, *edge));
		adjacency.entry(*right).or_default().push((*left, *edge));
	}
	let mut seen = BTreeSet::new();
	let mut result = BTreeMap::new();
	for root in nodes.keys().copied() {
		if !seen.insert(root) {
			continue;
		}
		let mut stack = vec![root];
		while let Some(parent) = stack.pop() {
			for (child, edge) in adjacency.get(&parent).into_iter().flatten().copied() {
				if seen.insert(child) {
					result.insert(edge, (parent, child));
					stack.push(child);
				}
			}
		}
	}
	result
}

/// Returns the topology for result.
fn topology_for(orientation: TreeOrientation, parent: AtomId, child: AtomId) -> EdgeTopology {
	match orientation {
		TreeOrientation::Undirected => EdgeTopology::undirected([parent, child]),
		TreeOrientation::OutArborescence => EdgeTopology::directed([parent], [child]),
		TreeOrientation::InArborescence => EdgeTopology::directed([child], [parent]),
	}
}

/// Returns the constraint result.
fn constraint(reason: impl Into<String>) -> FerrosGrynnError {
	FerrosGrynnError::graph_constraint_violation("forest", reason.into())
}

#[cfg(test)]
/// Regression tests for forest behavior.
mod tests {
	use super::{Forest, RootedTree, Tree, TreeOrientation};
	#[cfg(test)]
	use crate::test_assertions::{AssertOption as _, AssertResult as _};
	use crate::values::{
		PropertyMap,
		runtime::graph::{DirtyTarget, Graph, GraphMut, Mutable},
	};

	/// Verifies rooted tree tree forest conversions reuse atom ids.
	#[test]
	fn rooted_tree_tree_forest_conversions_reuse_atom_ids() {
		let mut rooted = RootedTree::<Mutable>::new(
			TreeOrientation::OutArborescence,
			Some("root".into()),
			PropertyMap::new(),
		)
		.assert_ok();
		let root = rooted.root();
		let (child, _) =
			rooted.add_child(root, None, PropertyMap::new(), None, PropertyMap::new()).assert_ok();
		let tree: Tree<Mutable> = rooted.into();
		let forest: Forest<Mutable> = tree.into();

		assert_eq!(forest.parent(child), Some(root));
		assert_eq!(forest.roots().collect::<Vec<_>>(), [root]);
	}

	/// Verifies forest rejects cycles without mutation.
	#[test]
	fn forest_rejects_cycles_without_mutation() {
		let mut forest = Forest::<Mutable>::new(TreeOrientation::Undirected);
		let left = forest.add_root(None, PropertyMap::new()).assert_ok();
		let right = forest.add_root(None, PropertyMap::new()).assert_ok();
		forest.connect(left, right, None, PropertyMap::new()).assert_ok();
		let revision = forest.revision();
		let error = forest.connect(right, left, None, PropertyMap::new()).assert_err();

		assert_eq!(error.status().code().to_string(), "FG-G2F04");
		assert_eq!(forest.revision(), revision);
		assert_eq!(forest.edge_count(), 1);
	}

	/// Verifies graph to tree validates classic topology.
	#[test]
	fn graph_to_tree_validates_classic_topology() {
		let mut graph = Graph::<Mutable>::new();
		let left = graph.add_node(None, PropertyMap::new()).assert_ok();
		let right = graph.add_node(None, PropertyMap::new()).assert_ok();
		graph
			.add_edge(super::EdgeTopology::directed([left], [right]), None, PropertyMap::new())
			.assert_ok();
		let tree = Tree::try_from(graph).assert_ok();
		assert_eq!(tree.orientation(), TreeOrientation::OutArborescence);
		assert_eq!(tree.node_count(), 2);
	}

	/// Verifies undirected rooted tree requires and applies explicit root.
	#[test]
	fn undirected_rooted_tree_requires_and_applies_explicit_root() {
		let mut graph = Graph::<Mutable>::new();
		let left = graph.add_node(None, PropertyMap::new()).assert_ok();
		let right = graph.add_node(None, PropertyMap::new()).assert_ok();
		graph
			.add_edge(super::EdgeTopology::undirected([left, right]), None, PropertyMap::new())
			.assert_ok();
		let rooted = RootedTree::try_from_graph_with_root(graph, right).assert_ok();

		assert_eq!(rooted.root(), right);
		assert_eq!(rooted.parent(left), Some(right));
	}

	/// Verifies tree mutation cannot break connectivity.
	#[test]
	fn tree_mutation_cannot_break_connectivity() {
		let mut tree =
			Tree::<Mutable>::new(TreeOrientation::Undirected, None, PropertyMap::new()).assert_ok();
		let root = tree.roots().next().assert_some();
		let (child, edge) =
			tree.add_child(root, None, PropertyMap::new(), None, PropertyMap::new()).assert_ok();
		assert!(tree.remove_atom(edge).is_err());
		tree.metadata.clear_dirty();
		tree.remove_atom(child).assert_ok();
		assert_eq!(tree.node_count(), 1);
		let dirty = tree.metadata.dirty().cloned().collect::<Vec<_>>();
		assert_eq!(dirty.len(), 2);
		assert!(dirty.contains(&DirtyTarget::Atom(child)));
		assert!(dirty.contains(&DirtyTarget::Atom(edge)));
	}

	/// Verifies forest node removal marks every incident edge dirty.
	#[test]
	fn forest_node_removal_marks_every_incident_edge_dirty() {
		let mut forest = Forest::<Mutable>::new(TreeOrientation::Undirected);
		let first = forest.add_root(None, PropertyMap::new()).assert_ok();
		let middle = forest.add_root(None, PropertyMap::new()).assert_ok();
		let last = forest.add_root(None, PropertyMap::new()).assert_ok();
		let first_edge = forest.connect(first, middle, None, PropertyMap::new()).assert_ok();
		let last_edge = forest.connect(middle, last, None, PropertyMap::new()).assert_ok();
		forest.metadata.clear_dirty();

		forest.remove_atom(middle).assert_ok();

		let dirty = forest.metadata.dirty().cloned().collect::<Vec<_>>();
		assert_eq!(dirty.len(), 3);
		assert!(dirty.contains(&DirtyTarget::Atom(middle)));
		assert!(dirty.contains(&DirtyTarget::Atom(first_edge)));
		assert!(dirty.contains(&DirtyTarget::Atom(last_edge)));
	}

	/// Verifies forest conversion rejects containment before discarding it.
	#[test]
	fn forest_conversion_rejects_containment_before_discarding_it() {
		let mut child = Graph::<Mutable>::new();
		child.add_node(None, PropertyMap::new()).assert_ok();
		let child = child.freeze();
		let mut source = Graph::<Mutable>::new();
		let owner = source.add_node(None, PropertyMap::new()).assert_ok();
		source.replace_inner_graph(owner, Some(child.clone())).assert_ok();
		assert!(Forest::try_from(source.freeze()).is_err());
		let mut source = Graph::<Mutable>::new();
		let left = source.add_node(None, PropertyMap::new()).assert_ok();
		let right = source.add_node(None, PropertyMap::new()).assert_ok();
		let edge = source
			.add_edge(super::EdgeTopology::undirected([left, right]), None, PropertyMap::new())
			.assert_ok();
		source.replace_transition_graph(edge, Some(child)).assert_ok();
		assert!(Tree::try_from(source.freeze()).is_err());
	}
}
