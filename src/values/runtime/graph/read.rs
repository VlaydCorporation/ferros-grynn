use std::{
	collections::BTreeMap,
	fmt::{self, Display, Formatter},
};

use super::{AtomId, AtomKind, EdgeTopology, Frozen, Graph};
use crate::{
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	values::runtime::{Number, PropertyMap, RecordId, Text, Uuid},
};

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
	Subgraph,
}

impl GraphKind {
	pub const fn name(self) -> &'static str {
		match self {
			Self::Graph => "graph",
			Self::Tree => "tree",
			Self::RootedTree => "rooted_tree",
			Self::Forest => "forest",
			Self::Dag => "dag",
			Self::Hypergraph => "hypergraph",
			Self::Metagraph => "metagraph",
			Self::Subgraph => "subgraph",
		}
	}
}

impl Display for GraphKind {
	fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
		formatter.write_str(self.name())
	}
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
	pub const fn new(id: RecordId, kind: GraphKind, mode: GraphStorageMode) -> Self {
		Self {
			id,
			kind,
			mode,
		}
	}

	pub const fn id(&self) -> &RecordId {
		&self.id
	}

	pub const fn kind(&self) -> GraphKind {
		self.kind
	}

	pub const fn storage_mode(&self) -> GraphStorageMode {
		self.mode
	}

	pub fn read<'tx, T: GraphRefReader + ?Sized>(
		&self,
		transaction: &'tx T,
	) -> FerrosGrynnResult<T::View<'tx>> {
		transaction.read_graph(self)
	}
}

/// Storage/query transaction boundary used by [`GraphRef::read`].
pub trait GraphRefReader {
	type View<'tx>: GraphRead + 'tx
	where
		Self: 'tx;

	fn read_graph(&self, reference: &GraphRef) -> FerrosGrynnResult<Self::View<'_>>;
}

/// Identity of one MVCC read version; it does not own graph data.
#[derive(Debug, Default, Clone, Copy, PartialOrd, PartialEq, Eq, Ord, Hash)]
pub struct SnapshotId {
	timestamp: u64,
	transaction: u64,
}

impl SnapshotId {
	pub const fn new(timestamp: u64, transaction: u64) -> Self {
		Self {
			timestamp,
			transaction,
		}
	}

	pub const fn in_memory(revision: u64) -> Self {
		Self::new(revision, 0)
	}

	pub const fn timestamp(self) -> u64 {
		self.timestamp
	}

	pub const fn transaction(self) -> u64 {
		self.transaction
	}
}

impl Display for SnapshotId {
	fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
		write!(formatter, "{}:{}", self.timestamp, self.transaction)
	}
}

#[derive(Debug, Clone, PartialOrd, PartialEq, Eq, Ord, Hash)]
pub struct AtomRef {
	kind: AtomKind,
	id: RecordId,
}

impl AtomRef {
	pub const fn new(kind: AtomKind, id: RecordId) -> Self {
		Self {
			kind,
			id,
		}
	}

	pub const fn kind(&self) -> AtomKind {
		self.kind
	}

	pub const fn id(&self) -> &RecordId {
		&self.id
	}
}

#[derive(Debug, Clone, Copy)]
pub struct NodeView<'a> {
	id: AtomId,
	record_id: &'a RecordId,
	label: Option<&'a Text>,
	properties: &'a PropertyMap,
	inner_graph: Option<&'a Graph<Frozen>>,
}

impl<'a> NodeView<'a> {
	pub(crate) const fn new(
		id: AtomId,
		record_id: &'a RecordId,
		label: Option<&'a Text>,
		properties: &'a PropertyMap,
		inner_graph: Option<&'a Graph<Frozen>>,
	) -> Self {
		Self {
			id,
			record_id,
			label,
			properties,
			inner_graph,
		}
	}

	pub const fn id(self) -> AtomId {
		self.id
	}

	pub const fn record_id(self) -> &'a RecordId {
		self.record_id
	}

	pub fn atom_ref(self) -> AtomRef {
		AtomRef::new(AtomKind::Node, self.record_id.clone())
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

	pub const fn inner_graph(self) -> Option<&'a Graph<Frozen>> {
		self.inner_graph
	}

	pub const fn is_meta(self) -> bool {
		self.inner_graph.is_some()
	}
}

#[derive(Debug, Clone, Copy)]
pub struct EdgeView<'a> {
	id: AtomId,
	record_id: &'a RecordId,
	topology: &'a EdgeTopology,
	label: Option<&'a Text>,
	properties: &'a PropertyMap,
	weight: Option<Number>,
	transition_graph: Option<&'a Graph<Frozen>>,
}

impl<'a> EdgeView<'a> {
	pub(crate) const fn new(
		id: AtomId,
		record_id: &'a RecordId,
		topology: &'a EdgeTopology,
		label: Option<&'a Text>,
		properties: &'a PropertyMap,
		weight: Option<Number>,
		transition_graph: Option<&'a Graph<Frozen>>,
	) -> Self {
		Self {
			id,
			record_id,
			topology,
			label,
			properties,
			weight,
			transition_graph,
		}
	}

	pub const fn id(self) -> AtomId {
		self.id
	}

	pub const fn record_id(self) -> &'a RecordId {
		self.record_id
	}

	pub fn atom_ref(self) -> AtomRef {
		AtomRef::new(AtomKind::Edge, self.record_id.clone())
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

	pub const fn weight(self) -> Option<Number> {
		self.weight
	}

	pub const fn transition_graph(self) -> Option<&'a Graph<Frozen>> {
		self.transition_graph
	}

	pub const fn is_meta(self) -> bool {
		self.transition_graph.is_some()
	}
}

pub trait GraphRead {
	type NodeIds<'a>: Iterator<Item = AtomId> + 'a
	where
		Self: 'a;
	type EdgeIds<'a>: Iterator<Item = AtomId> + 'a
	where
		Self: 'a;

	fn identity(&self) -> Uuid;
	fn revision(&self) -> u64;
	fn kind(&self) -> GraphKind;
	fn properties(&self) -> &PropertyMap;
	fn node_ids(&self) -> Self::NodeIds<'_>;
	fn edge_ids(&self) -> Self::EdgeIds<'_>;
	fn node(&self, id: AtomId) -> Option<NodeView<'_>>;
	fn edge(&self, id: AtomId) -> Option<EdgeView<'_>>;

	fn storage_mode(&self) -> GraphStorageMode {
		GraphStorageMode::InMemory
	}

	fn snapshot_id(&self) -> SnapshotId {
		SnapshotId::in_memory(self.revision())
	}

	fn graph_ref(&self) -> Option<&GraphRef> {
		None
	}

	fn contains(&self, id: AtomId) -> bool {
		match id.kind() {
			Some(AtomKind::Node) => self.node(id).is_some(),
			Some(AtomKind::Edge) => self.edge(id).is_some(),
			None => false,
		}
	}

	fn node_count(&self) -> usize {
		self.node_ids().count()
	}

	fn edge_count(&self) -> usize {
		self.edge_ids().count()
	}

	fn atom_ref(&self, id: AtomId) -> Option<AtomRef> {
		match id.kind()? {
			AtomKind::Node => self.node(id).map(NodeView::atom_ref),
			AtomKind::Edge => self.edge(id).map(EdgeView::atom_ref),
		}
	}

	fn atom_id(&self, reference: &AtomRef) -> Option<AtomId> {
		match reference.kind() {
			AtomKind::Node => self
				.node_ids()
				.find(|id| self.node(*id).is_some_and(|node| node.record_id() == reference.id())),
			AtomKind::Edge => self
				.edge_ids()
				.find(|id| self.edge(*id).is_some_and(|edge| edge.record_id() == reference.id())),
		}
	}
}

pub trait GraphMut: GraphRead {
	fn properties_mut(&mut self) -> FerrosGrynnResult<&mut PropertyMap>;
	fn node_properties_mut(&mut self, id: AtomId) -> FerrosGrynnResult<&mut PropertyMap>;
	fn edge_properties_mut(&mut self, id: AtomId) -> FerrosGrynnResult<&mut PropertyMap>;
	fn remove_atom(&mut self, id: AtomId) -> FerrosGrynnResult<()>;
}

#[derive(Debug, Clone, PartialOrd, PartialEq, Eq, Ord, Hash)]
pub enum DirtyTarget {
	Graph,
	Atom(AtomId),
}

#[derive(Debug, Clone)]
pub struct MaterializationOrigin {
	identity: Uuid,
	snapshot: SnapshotId,
	graph_ref: Option<GraphRef>,
	source_atoms: BTreeMap<AtomId, AtomRef>,
}

impl MaterializationOrigin {
	pub(crate) fn new<R: GraphRead>(source: &R, source_atoms: BTreeMap<AtomId, AtomRef>) -> Self {
		Self {
			identity: source.identity(),
			snapshot: source.snapshot_id(),
			graph_ref: source.graph_ref().cloned(),
			source_atoms,
		}
	}

	pub const fn identity(&self) -> Uuid {
		self.identity
	}

	pub const fn snapshot(&self) -> SnapshotId {
		self.snapshot
	}

	pub const fn graph_ref(&self) -> Option<&GraphRef> {
		self.graph_ref.as_ref()
	}

	pub fn source_atom(&self, local: AtomId) -> Option<&AtomRef> {
		self.source_atoms.get(&local)
	}
}

pub trait GraphWriteBack<G> {
	fn flush(
		&mut self,
		graph: &G,
		origin: &MaterializationOrigin,
		dirty: &[DirtyTarget],
	) -> FerrosGrynnResult<()>;
}

pub trait QueryMaterialize: Sized {
	fn query_from<R: GraphRead>(source: &R) -> FerrosGrynnResult<Self>;
}

pub fn query_to_memory<T, R>(source: &R) -> FerrosGrynnResult<T>
where
	T: QueryMaterialize,
	R: GraphRead,
{
	T::query_from(source)
}

pub struct GraphReadView<'tx, R: GraphRead + ?Sized> {
	source: &'tx R,
	snapshot: SnapshotId,
}

impl<'tx, R: GraphRead + ?Sized> GraphReadView<'tx, R> {
	pub const fn new(source: &'tx R, snapshot: SnapshotId) -> Self {
		Self {
			source,
			snapshot,
		}
	}

	pub const fn source(&self) -> &'tx R {
		self.source
	}
}

impl<R: GraphRead + ?Sized> GraphRead for GraphReadView<'_, R> {
	type NodeIds<'a>
		= R::NodeIds<'a>
	where
		Self: 'a;
	type EdgeIds<'a>
		= R::EdgeIds<'a>
	where
		Self: 'a;

	fn identity(&self) -> Uuid {
		self.source.identity()
	}

	fn revision(&self) -> u64 {
		self.source.revision()
	}

	fn kind(&self) -> GraphKind {
		self.source.kind()
	}

	fn properties(&self) -> &PropertyMap {
		self.source.properties()
	}

	fn node_ids(&self) -> Self::NodeIds<'_> {
		self.source.node_ids()
	}

	fn edge_ids(&self) -> Self::EdgeIds<'_> {
		self.source.edge_ids()
	}

	fn node(&self, id: AtomId) -> Option<NodeView<'_>> {
		self.source.node(id)
	}

	fn edge(&self, id: AtomId) -> Option<EdgeView<'_>> {
		self.source.edge(id)
	}

	fn storage_mode(&self) -> GraphStorageMode {
		self.source.storage_mode()
	}

	fn snapshot_id(&self) -> SnapshotId {
		self.snapshot
	}

	fn graph_ref(&self) -> Option<&GraphRef> {
		self.source.graph_ref()
	}
}

/// A transaction-bound disk adapter view.
///
/// The source is supplied by the storage layer and may perform page reads; the
/// view itself never owns or materializes the graph.
pub struct DiskGraphView<'tx, R: GraphRead + ?Sized> {
	graph_ref: &'tx GraphRef,
	view: GraphReadView<'tx, R>,
}

impl<'tx, R: GraphRead + ?Sized> DiskGraphView<'tx, R> {
	pub fn new(
		graph_ref: &'tx GraphRef,
		source: &'tx R,
		snapshot: SnapshotId,
	) -> FerrosGrynnResult<Self> {
		if graph_ref.kind() != source.kind() {
			return Err(FerrosGrynnError::graph_constraint_violation(
				graph_ref.kind().name(),
				format!("the storage adapter exposes {}", source.kind().name()),
			));
		}
		Ok(Self {
			graph_ref,
			view: GraphReadView::new(source, snapshot),
		})
	}
}

impl<R: GraphRead + ?Sized> GraphRead for DiskGraphView<'_, R> {
	type NodeIds<'a>
		= R::NodeIds<'a>
	where
		Self: 'a;
	type EdgeIds<'a>
		= R::EdgeIds<'a>
	where
		Self: 'a;

	fn identity(&self) -> Uuid {
		self.view.identity()
	}

	fn revision(&self) -> u64 {
		self.view.revision()
	}

	fn kind(&self) -> GraphKind {
		self.graph_ref.kind()
	}

	fn properties(&self) -> &PropertyMap {
		self.view.properties()
	}

	fn node_ids(&self) -> Self::NodeIds<'_> {
		self.view.node_ids()
	}

	fn edge_ids(&self) -> Self::EdgeIds<'_> {
		self.view.edge_ids()
	}

	fn node(&self, id: AtomId) -> Option<NodeView<'_>> {
		self.view.node(id)
	}

	fn edge(&self, id: AtomId) -> Option<EdgeView<'_>> {
		self.view.edge(id)
	}

	fn storage_mode(&self) -> GraphStorageMode {
		GraphStorageMode::DiskBacked
	}

	fn snapshot_id(&self) -> SnapshotId {
		self.view.snapshot_id()
	}

	fn graph_ref(&self) -> Option<&GraphRef> {
		Some(self.graph_ref)
	}
}

#[cfg(test)]
mod tests {
	use std::sync::{Arc, Weak};

	use super::{
		DirtyTarget, DiskGraphView, GraphKind, GraphMut, GraphRead, GraphRef, GraphRefReader,
		GraphStorageMode, GraphWriteBack, MaterializationOrigin, SnapshotId, query_to_memory,
	};
	use crate::{
		gql_status::{FerrosGrynnError, FerrosGrynnResult},
		values::runtime::{
			PropertyMap, Value,
			graph::{AtomId, EdgeTopology, Frozen, Graph, Mutable, TraversalDirection, bfs},
		},
	};

	struct FakeTransaction {
		reference: GraphRef,
		graph: Graph<Frozen>,
		snapshot: SnapshotId,
		_lifetime: Arc<()>,
	}

	impl GraphRefReader for FakeTransaction {
		type View<'tx> = DiskGraphView<'tx, Graph<Frozen>>;

		fn read_graph(&self, reference: &GraphRef) -> FerrosGrynnResult<Self::View<'_>> {
			if reference != &self.reference {
				return Err(FerrosGrynnError::graph_constraint_violation(
					"graph reference",
					"the transaction does not contain the requested graph",
				));
			}
			DiskGraphView::new(&self.reference, &self.graph, self.snapshot)
		}
	}

	#[derive(Default)]
	struct RecordingWriter {
		origin_identity: Option<crate::values::runtime::Uuid>,
		dirty: Vec<DirtyTarget>,
	}

	impl GraphWriteBack<Graph<Mutable>> for RecordingWriter {
		fn flush(
			&mut self,
			_graph: &Graph<Mutable>,
			origin: &MaterializationOrigin,
			dirty: &[DirtyTarget],
		) -> FerrosGrynnResult<()> {
			self.origin_identity = Some(origin.identity());
			self.dirty = dirty.to_vec();
			Ok(())
		}
	}

	#[test]
	fn disk_view_and_hybrid_materialization_have_the_same_read_result() {
		let (source, root) = sample_graph();
		let source_order = stable_bfs(&source, root);
		let reference = GraphRef::new(
			"graph:example".parse().unwrap(),
			GraphKind::Graph,
			GraphStorageMode::DiskBacked,
		);
		let lifetime = Arc::new(());
		let weak = Arc::downgrade(&lifetime);
		let hybrid = {
			let transaction = FakeTransaction {
				reference: reference.clone(),
				graph: source,
				snapshot: SnapshotId::new(41, 7),
				_lifetime: lifetime,
			};
			let view = reference.read(&transaction).unwrap();
			assert_eq!(view.storage_mode(), GraphStorageMode::DiskBacked);
			assert_eq!(view.snapshot_id(), SnapshotId::new(41, 7));
			assert_eq!(stable_bfs(&view, root), source_order);
			query_to_memory::<Graph<Frozen>, _>(&view).unwrap()
		};

		assert_snapshot_released(&weak);
		assert_eq!(hybrid.storage_mode(), GraphStorageMode::Hybrid);
		let hybrid_root = hybrid.node_ids().next().unwrap();
		assert_eq!(stable_bfs(&hybrid, hybrid_root), source_order);
		assert!(hybrid.graph_ref().is_some());
	}

	#[test]
	fn mutable_hybrid_flush_uses_origin_mapping_and_dirty_set() {
		let (source, _) = sample_graph();
		let source_identity = source.identity();
		let reference = GraphRef::new(
			"graph:mutable".parse().unwrap(),
			GraphKind::Graph,
			GraphStorageMode::DiskBacked,
		);
		let transaction = FakeTransaction {
			reference: reference.clone(),
			graph: source,
			snapshot: SnapshotId::new(52, 3),
			_lifetime: Arc::new(()),
		};
		let view = reference.read(&transaction).unwrap();
		let mut hybrid = query_to_memory::<Graph<Mutable>, _>(&view).unwrap();
		let local = hybrid.node_ids().next().unwrap();
		assert!(
			hybrid.materialization_origin().and_then(|origin| origin.source_atom(local)).is_some()
		);
		GraphMut::node_properties_mut(&mut hybrid, local)
			.unwrap()
			.insert(1_u32.into(), Value::from("changed"));

		let mut writer = RecordingWriter::default();
		hybrid.flush(&mut writer).unwrap();
		assert_eq!(writer.origin_identity, Some(source_identity));
		assert_eq!(writer.dirty, vec![DirtyTarget::Atom(local)]);

		hybrid.flush(&mut writer).unwrap();
		assert!(writer.dirty.is_empty());
	}

	#[test]
	fn cascade_removal_marks_removed_incident_edges_dirty() {
		let (source, _) = sample_graph();
		let mut hybrid = query_to_memory::<Graph<Mutable>, _>(&source).unwrap();
		let removed_node = hybrid.node_ids().nth(1).unwrap();
		let removed_edges = hybrid.edge_ids().collect::<Vec<_>>();

		GraphMut::remove_atom(&mut hybrid, removed_node).unwrap();

		let mut writer = RecordingWriter::default();
		hybrid.flush(&mut writer).unwrap();
		assert_eq!(writer.dirty.len(), removed_edges.len() + 1);
		assert!(writer.dirty.contains(&DirtyTarget::Atom(removed_node)));
		for edge in removed_edges {
			assert!(writer.dirty.contains(&DirtyTarget::Atom(edge)));
		}
	}

	fn sample_graph() -> (Graph<Frozen>, AtomId) {
		let mut graph = Graph::<Mutable>::new();
		let first = graph.add_node(Some("first".into()), PropertyMap::new()).unwrap();
		let second = graph.add_node(Some("second".into()), PropertyMap::new()).unwrap();
		let third = graph.add_node(Some("third".into()), PropertyMap::new()).unwrap();
		graph
			.add_edge(EdgeTopology::directed([first], [second]), None, PropertyMap::new())
			.unwrap();
		graph
			.add_edge(EdgeTopology::directed([second], [third]), None, PropertyMap::new())
			.unwrap();
		(graph.freeze(), first)
	}

	fn stable_bfs<R: GraphRead>(graph: &R, root: AtomId) -> Vec<String> {
		bfs(graph, root, TraversalDirection::Forward, None)
			.unwrap()
			.into_iter()
			.map(|id| graph.node(id).unwrap().record_id().to_string())
			.collect()
	}

	fn assert_snapshot_released(snapshot: &Weak<()>) {
		assert!(snapshot.upgrade().is_none());
	}
}
