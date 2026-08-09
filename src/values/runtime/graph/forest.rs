use std::{
	cmp::Ordering,
	collections::{BTreeMap, BTreeSet},
	fmt::{self, Debug, Formatter},
	hash::{Hash, Hasher},
	marker::PhantomData,
};

use super::{
	AtomId,
	AtomKind,
	DirtyTarget,
	EdgeDirection,
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
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	values::runtime::{Number, PropertyMap, RecordId, Text, Uuid},
};

#[derive(Debug, Default, Clone, Copy, PartialOrd, PartialEq, Eq, Ord, Hash)]
pub enum TreeOrientation {
	#[default]
	Undirected,
	OutArborescence,
	InArborescence,
}

#[derive(Debug, Clone)]
struct ForestStorage {
	nodes: NodeColumns,
	edges: EdgeColumns,
	orientation: TreeOrientation,
	parent: Vec<Option<AtomId>>,
	parent_edge: Vec<Option<AtomId>>,
	children: Vec<Vec<AtomId>>,
	roots: BTreeSet<AtomId>,
}

impl ForestStorage {
	const fn new(orientation: TreeOrientation) -> Self {
		Self {
			nodes: NodeColumns::new(),
			edges: EdgeColumns::new(),
			orientation,
			parent: Vec::new(),
			parent_edge: Vec::new(),
			children: Vec::new(),
			roots: BTreeSet::new(),
		}
	}

	fn insert_root_new(
		&mut self,
		record_id: RecordId,
		label: Option<Text>,
		properties: PropertyMap,
	) -> FerrosGrynnResult<AtomId> {
		ensure_unique_record(&self.nodes, &self.edges, &record_id)?;
		let id = self.nodes.insert_new(record_id, label, properties)?;
		self.ensure_node_slot(id);
		self.roots.insert(id);
		Ok(id)
	}

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
		self.roots.insert(id);
		Ok(())
	}

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

	fn attach(&mut self, parent: AtomId, child: AtomId, edge: AtomId) {
		let child_slot = child.slot() as usize;
		self.parent[child_slot] = Some(parent);
		self.parent_edge[child_slot] = Some(edge);
		self.children[parent.slot() as usize].push(child);
		self.roots.remove(&child);
	}

	fn component_root(&self, mut id: AtomId) -> Option<AtomId> {
		if !self.nodes.contains(id) {
			return None;
		}
		while let Some(parent) = self.parent.get(id.slot() as usize).copied().flatten() {
			id = parent;
		}
		Some(id)
	}

	fn parent(&self, id: AtomId) -> Option<AtomId> {
		self.nodes.contains(id).then(|| self.parent.get(id.slot() as usize).copied().flatten())?
	}

	fn children(&self, id: AtomId) -> &[AtomId] {
		if !self.nodes.contains(id) {
			return &[];
		}
		self.children.get(id.slot() as usize).map_or(&[], Vec::as_slice)
	}

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
			if let Some(parent) = self.parent[child_slot].take() {
				if let Some(children) = self.children.get_mut(parent.slot() as usize) {
					children.retain(|candidate| *candidate != child);
				}
			}
			self.parent_edge[child_slot] = None;
			self.roots.insert(child);
		}
		self.edges.remove(edge)
	}

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
		self.roots.remove(&id);
		self.parent[slot] = None;
		self.parent_edge[slot] = None;
		self.children[slot].clear();
		self.nodes.remove(id)
	}

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

	fn ensure_node_slot(&mut self, id: AtomId) {
		let slot = id.slot() as usize;
		while self.parent.len() <= slot {
			self.parent.push(None);
			self.parent_edge.push(None);
			self.children.push(Vec::new());
		}
	}
}

pub struct Forest<State = Frozen> {
	metadata: GraphMetadata,
	storage: ForestStorage,
	state: PhantomData<State>,
}

pub struct Tree<State = Frozen> {
	metadata: GraphMetadata,
	storage: ForestStorage,
	state: PhantomData<State>,
}

pub struct RootedTree<State = Frozen> {
	metadata: GraphMetadata,
	storage: ForestStorage,
	root: AtomId,
	state: PhantomData<State>,
}

pub type MutableForest = Forest<Mutable>;
pub type FrozenForest = Forest<Frozen>;
pub type MutableTree = Tree<Mutable>;
pub type FrozenTree = Tree<Frozen>;
pub type MutableRootedTree = RootedTree<Mutable>;
pub type FrozenRootedTree = RootedTree<Frozen>;

impl Forest<Mutable> {
	pub fn new(orientation: TreeOrientation) -> Self {
		Self {
			metadata: GraphMetadata::new(),
			storage: ForestStorage::new(orientation),
			state: PhantomData,
		}
	}

	pub fn add_root(
		&mut self,
		label: Option<Text>,
		properties: PropertyMap,
	) -> FerrosGrynnResult<AtomId> {
		self.metadata.ensure_can_bump()?;
		let id = self.storage.insert_root_new(
			generated_record_id(AtomKind::Node)?,
			label,
			properties,
		)?;
		self.metadata.bump(Some(DirtyTarget::Atom(id)))?;
		Ok(id)
	}

	pub fn connect(
		&mut self,
		parent: AtomId,
		child: AtomId,
		label: Option<Text>,
		properties: PropertyMap,
	) -> FerrosGrynnResult<AtomId> {
		self.metadata.ensure_can_bump()?;
		let edge = self.storage.connect_new(
			parent,
			child,
			generated_record_id(AtomKind::Edge)?,
			label,
			properties,
			None,
		)?;
		self.metadata.bump(Some(DirtyTarget::Atom(edge)))?;
		Ok(edge)
	}

	pub fn freeze(self) -> Forest<Frozen> {
		Forest {
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
}

impl Default for Forest<Mutable> {
	fn default() -> Self {
		Self::new(TreeOrientation::Undirected)
	}
}

impl Forest<Frozen> {
	pub fn thaw(self) -> Forest<Mutable> {
		Forest {
			metadata: self.metadata,
			storage: self.storage,
			state: PhantomData,
		}
	}
}

impl Clone for Forest<Frozen> {
	fn clone(&self) -> Self {
		Self {
			metadata: self.metadata.clone(),
			storage: self.storage.clone(),
			state: PhantomData,
		}
	}
}

impl Tree<Mutable> {
	pub fn new(
		orientation: TreeOrientation,
		root_label: Option<Text>,
		root_properties: PropertyMap,
	) -> FerrosGrynnResult<Self> {
		let mut forest = Forest::new(orientation);
		forest.add_root(root_label, root_properties)?;
		Self::try_from(forest)
	}

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

	pub fn freeze(self) -> Tree<Frozen> {
		Tree {
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
}

impl Tree<Frozen> {
	pub fn thaw(self) -> Tree<Mutable> {
		Tree {
			metadata: self.metadata,
			storage: self.storage,
			state: PhantomData,
		}
	}
}

impl Clone for Tree<Frozen> {
	fn clone(&self) -> Self {
		Self {
			metadata: self.metadata.clone(),
			storage: self.storage.clone(),
			state: PhantomData,
		}
	}
}

impl RootedTree<Mutable> {
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

	pub fn freeze(self) -> RootedTree<Frozen> {
		RootedTree {
			metadata: self.metadata.into_frozen(),
			storage: self.storage,
			root: self.root,
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
}

impl RootedTree<Frozen> {
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
	fn clone(&self) -> Self {
		Self {
			metadata: self.metadata.clone(),
			storage: self.storage.clone(),
			root: self.root,
			state: PhantomData,
		}
	}
}

macro_rules! impl_forest_read {
	($type:ident, $kind:expr) => {
		impl<State> GraphRead for $type<State> {
			type EdgeIds<'a>
				= std::iter::Copied<std::slice::Iter<'a, AtomId>>
			where
				Self: 'a;
			type NodeIds<'a>
				= std::iter::Copied<std::slice::Iter<'a, AtomId>>
			where
				Self: 'a;

			fn identity(&self) -> Uuid {
				self.metadata.identity()
			}

			fn revision(&self) -> u64 {
				self.metadata.revision()
			}

			fn kind(&self) -> GraphKind {
				$kind
			}

			fn properties(&self) -> &PropertyMap {
				self.metadata.properties()
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
	};
}

impl_forest_read!(Forest, GraphKind::Forest);
impl_forest_read!(Tree, GraphKind::Tree);
impl_forest_read!(RootedTree, GraphKind::RootedTree);

macro_rules! impl_forest_common {
	($type:ident) => {
		impl<State> $type<State> {
			pub const fn identity(&self) -> Uuid {
				self.metadata.identity()
			}

			pub const fn revision(&self) -> u64 {
				self.metadata.revision()
			}

			pub const fn properties(&self) -> &PropertyMap {
				self.metadata.properties()
			}

			pub const fn orientation(&self) -> TreeOrientation {
				self.storage.orientation
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

			pub fn parent(&self, id: AtomId) -> Option<AtomId> {
				self.storage.parent(id)
			}

			pub fn children(&self, id: AtomId) -> &[AtomId] {
				self.storage.children(id)
			}

			pub fn roots(&self) -> impl ExactSizeIterator<Item = AtomId> + '_ {
				self.storage.roots.iter().copied()
			}

			pub fn into_graph(self) -> FerrosGrynnResult<Graph<State>> {
				let converted = Graph::<Mutable>::from_read_preserving(&self)?;
				let (_, storage) = converted.into_raw();
				Ok(Graph::from_raw(self.metadata, storage))
			}
		}

		impl<State> Debug for $type<State> {
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
			fn eq(&self, other: &Self) -> bool {
				(self.identity(), self.revision()) == (other.identity(), other.revision())
			}
		}
		impl<State> Eq for $type<State> {}
		impl<State> PartialOrd for $type<State> {
			fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
				Some(self.cmp(other))
			}
		}
		impl<State> Ord for $type<State> {
			fn cmp(&self, other: &Self) -> Ordering {
				(self.identity(), self.revision()).cmp(&(other.identity(), other.revision()))
			}
		}
		impl<State> Hash for $type<State> {
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
	pub const fn root(&self) -> AtomId {
		self.root
	}
}

impl GraphMut for Forest<Mutable> {
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

macro_rules! impl_tree_mut {
	($type:ident $(, $root:ident)?) => {
		impl GraphMut for $type<Mutable> {
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
	fn from(value: RootedTree<State>) -> Self {
		Self {
			metadata: value.metadata,
			storage: value.storage,
			state: PhantomData,
		}
	}
}

impl<State> From<RootedTree<State>> for Forest<State> {
	fn from(value: RootedTree<State>) -> Self {
		Tree::from(value).into()
	}
}

impl<State> From<Tree<State>> for Forest<State> {
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

	fn try_from(value: Graph<State>) -> Result<Self, Self::Error> {
		Forest::try_from(value).and_then(Tree::try_from)
	}
}

impl QueryMaterialize for Forest<Mutable> {
	fn query_from<R: GraphRead>(source: &R) -> FerrosGrynnResult<Self> {
		let graph = Graph::<Mutable>::query_from(source)?;
		Self::try_from(graph)
	}
}

impl QueryMaterialize for Forest<Frozen> {
	fn query_from<R: GraphRead>(source: &R) -> FerrosGrynnResult<Self> {
		Forest::<Mutable>::query_from(source).map(Forest::freeze)
	}
}

impl QueryMaterialize for Tree<Mutable> {
	fn query_from<R: GraphRead>(source: &R) -> FerrosGrynnResult<Self> {
		Tree::try_from(Graph::<Mutable>::query_from(source)?)
	}
}

impl QueryMaterialize for Tree<Frozen> {
	fn query_from<R: GraphRead>(source: &R) -> FerrosGrynnResult<Self> {
		Tree::<Mutable>::query_from(source).map(Tree::freeze)
	}
}

fn build_storage<R: GraphRead>(source: &R) -> FerrosGrynnResult<ForestStorage> {
	let mut raw_edges = Vec::with_capacity(source.edge_count());
	let mut direction = None;
	let mut nodes = BTreeMap::new();
	for (index, id) in source.node_ids().enumerate() {
		nodes.insert(id, index);
	}
	for id in source.edge_ids() {
		let edge = source.edge(id).ok_or_else(|| atom_does_not_exist(id))?;
		let (left, right, edge_direction) = edge.topology().local_binary().ok_or_else(|| {
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
		raw_edges.push((id, left, right, edge));
	}

	validate_acyclic(&nodes, &raw_edges)?;
	let orientation = match direction {
		None | Some(EdgeDirection::Undirected) => TreeOrientation::Undirected,
		Some(EdgeDirection::Directed) => infer_directed_orientation(&nodes, &raw_edges)?,
		Some(EdgeDirection::Bidirectional) => {
			return Err(constraint("bidirectional edges are not supported"));
		}
	};
	let parent_map = build_parent_map(&nodes, &raw_edges, orientation)?;
	let mut storage = ForestStorage::new(orientation);
	for id in source.node_ids() {
		let node = source.node(id).ok_or_else(|| atom_does_not_exist(id))?;
		storage.insert_root_at(
			id,
			node.record_id().clone(),
			node.label_text().cloned(),
			node.properties().clone(),
		)?;
	}
	for (id, left, right, edge) in raw_edges {
		let (parent, child) = parent_map.get(&id).copied().unwrap_or((left, right));
		storage.connect_at(
			id,
			parent,
			child,
			edge.record_id().clone(),
			edge.topology().clone(),
			edge.label_text().cloned(),
			edge.properties().clone(),
			edge.weight(),
		)?;
	}
	Ok(storage)
}

fn validate_acyclic(
	nodes: &BTreeMap<AtomId, usize>,
	edges: &[(AtomId, AtomId, AtomId, EdgeView<'_>)],
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

fn find_root(parent: &mut [usize], mut index: usize) -> usize {
	while parent[index] != index {
		parent[index] = parent[parent[index]];
		index = parent[index];
	}
	index
}

fn infer_directed_orientation(
	nodes: &BTreeMap<AtomId, usize>,
	edges: &[(AtomId, AtomId, AtomId, EdgeView<'_>)],
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

fn build_parent_map(
	nodes: &BTreeMap<AtomId, usize>,
	edges: &[(AtomId, AtomId, AtomId, EdgeView<'_>)],
	orientation: TreeOrientation,
) -> FerrosGrynnResult<BTreeMap<AtomId, (AtomId, AtomId)>> {
	if orientation == TreeOrientation::OutArborescence {
		return Ok(edges.iter().map(|(id, source, target, _)| (*id, (*source, *target))).collect());
	}
	if orientation == TreeOrientation::InArborescence {
		return Ok(edges.iter().map(|(id, source, target, _)| (*id, (*target, *source))).collect());
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
	Ok(result)
}

fn topology_for(orientation: TreeOrientation, parent: AtomId, child: AtomId) -> EdgeTopology {
	match orientation {
		TreeOrientation::Undirected => EdgeTopology::undirected([parent, child]),
		TreeOrientation::OutArborescence => EdgeTopology::directed([parent], [child]),
		TreeOrientation::InArborescence => EdgeTopology::directed([child], [parent]),
	}
}

fn constraint(reason: impl Into<String>) -> FerrosGrynnError {
	FerrosGrynnError::graph_constraint_violation("forest", reason.into())
}

#[cfg(test)]
mod tests {
	use super::{Forest, RootedTree, Tree, TreeOrientation};
	use crate::values::{
		PropertyMap,
		runtime::graph::{DirtyTarget, Graph, GraphMut, Mutable},
	};

	#[test]
	fn rooted_tree_tree_forest_conversions_reuse_atom_ids() {
		let mut rooted = RootedTree::<Mutable>::new(
			TreeOrientation::OutArborescence,
			Some("root".into()),
			PropertyMap::new(),
		)
		.unwrap();
		let root = rooted.root();
		let (child, _) =
			rooted.add_child(root, None, PropertyMap::new(), None, PropertyMap::new()).unwrap();
		let tree: Tree<Mutable> = rooted.into();
		let forest: Forest<Mutable> = tree.into();

		assert_eq!(forest.parent(child), Some(root));
		assert_eq!(forest.roots().collect::<Vec<_>>(), [root]);
	}

	#[test]
	fn forest_rejects_cycles_without_mutation() {
		let mut forest = Forest::<Mutable>::new(TreeOrientation::Undirected);
		let left = forest.add_root(None, PropertyMap::new()).unwrap();
		let right = forest.add_root(None, PropertyMap::new()).unwrap();
		forest.connect(left, right, None, PropertyMap::new()).unwrap();
		let revision = forest.revision();
		let error = forest.connect(right, left, None, PropertyMap::new()).unwrap_err();

		assert_eq!(error.status().code().to_string(), "FG-G2F04");
		assert_eq!(forest.revision(), revision);
		assert_eq!(forest.edge_count(), 1);
	}

	#[test]
	fn graph_to_tree_validates_classic_topology() {
		let mut graph = Graph::<Mutable>::new();
		let left = graph.add_node(None, PropertyMap::new()).unwrap();
		let right = graph.add_node(None, PropertyMap::new()).unwrap();
		graph
			.add_edge(super::EdgeTopology::directed([left], [right]), None, PropertyMap::new())
			.unwrap();
		let tree = Tree::try_from(graph).unwrap();
		assert_eq!(tree.orientation(), TreeOrientation::OutArborescence);
		assert_eq!(tree.node_count(), 2);
	}

	#[test]
	fn tree_mutation_cannot_break_connectivity() {
		let mut tree =
			Tree::<Mutable>::new(TreeOrientation::Undirected, None, PropertyMap::new()).unwrap();
		let root = tree.roots().next().unwrap();
		let (child, edge) =
			tree.add_child(root, None, PropertyMap::new(), None, PropertyMap::new()).unwrap();
		assert!(tree.remove_atom(edge).is_err());
		tree.metadata.clear_dirty();
		tree.remove_atom(child).unwrap();
		assert_eq!(tree.node_count(), 1);
		let dirty = tree.metadata.dirty().cloned().collect::<Vec<_>>();
		assert_eq!(dirty.len(), 2);
		assert!(dirty.contains(&DirtyTarget::Atom(child)));
		assert!(dirty.contains(&DirtyTarget::Atom(edge)));
	}

	#[test]
	fn forest_node_removal_marks_every_incident_edge_dirty() {
		let mut forest = Forest::<Mutable>::new(TreeOrientation::Undirected);
		let first = forest.add_root(None, PropertyMap::new()).unwrap();
		let middle = forest.add_root(None, PropertyMap::new()).unwrap();
		let last = forest.add_root(None, PropertyMap::new()).unwrap();
		let first_edge = forest.connect(first, middle, None, PropertyMap::new()).unwrap();
		let last_edge = forest.connect(middle, last, None, PropertyMap::new()).unwrap();
		forest.metadata.clear_dirty();

		forest.remove_atom(middle).unwrap();

		let dirty = forest.metadata.dirty().cloned().collect::<Vec<_>>();
		assert_eq!(dirty.len(), 3);
		assert!(dirty.contains(&DirtyTarget::Atom(middle)));
		assert!(dirty.contains(&DirtyTarget::Atom(first_edge)));
		assert!(dirty.contains(&DirtyTarget::Atom(last_edge)));
	}
}
