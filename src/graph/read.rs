use std::{
	borrow::Cow,
	collections::BTreeMap,
	fmt::{self, Display, Formatter},
};

use super::{AtomId, AtomKind, EdgeTopology, Frozen, Graph, GraphId};
use crate::{
	collections::cow_vec,
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	values::runtime::{Number, PropertyMap, RecordId, Text, Uuid},
};

/// Stable iterator over atom handles in a graph snapshot.
pub struct AtomIds<'a> {
	inner: cow_vec::Iter<'a, AtomId>,
}

impl<'a> AtomIds<'a> {
	pub(crate) const fn new(inner: cow_vec::Iter<'a, AtomId>) -> Self {
		Self {
			inner,
		}
	}
}

impl Iterator for AtomIds<'_> {
	type Item = AtomId;

	fn next(&mut self) -> Option<Self::Item> {
		self.inner.next().copied()
	}

	fn size_hint(&self) -> (usize, Option<usize>) {
		self.inner.size_hint()
	}
}

impl ExactSizeIterator for AtomIds<'_> {}
impl std::iter::FusedIterator for AtomIds<'_> {}

/// Adapts an infallible in-memory atom iterator to the fallible storage
/// contract without allocating or boxing it.
pub struct FallibleAtomIds<Inner> {
	inner: Inner,
}

impl<Inner> FallibleAtomIds<Inner> {
	pub(crate) const fn new(inner: Inner) -> Self {
		Self {
			inner,
		}
	}
}

impl<Inner: Iterator<Item = AtomId>> Iterator for FallibleAtomIds<Inner> {
	type Item = FerrosGrynnResult<AtomId>;

	fn next(&mut self) -> Option<Self::Item> {
		self.inner.next().map(Ok)
	}

	fn size_hint(&self) -> (usize, Option<usize>) {
		self.inner.size_hint()
	}
}

impl<Inner: ExactSizeIterator<Item = AtomId>> ExactSizeIterator for FallibleAtomIds<Inner> {}
impl<Inner: std::iter::FusedIterator<Item = AtomId>> std::iter::FusedIterator
	for FallibleAtomIds<Inner>
{
}

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
	id: GraphId,
	name: Text,
	kind: GraphKind,
	storage_mode: GraphStorageMode,
}

impl GraphRef {
	/// Creates a catalog graph reference.
	///
	/// # Errors
	///
	/// Returns an invalid-argument error when the name is empty, contains NUL,
	/// or exceeds 128 UTF-8 bytes.
	pub fn new<IntoText: Into<Text>>(
		id: GraphId,
		name: IntoText,
		kind: GraphKind,
		storage_mode: GraphStorageMode,
	) -> FerrosGrynnResult<Self> {
		let name = name.into();
		if name.is_empty() || name.as_str().contains('\0') || name.len() > 128 {
			return Err(FerrosGrynnError::invalid_argument(
				"graph name must contain 1..=128 UTF-8 bytes and must not contain NUL",
			));
		}
		Ok(Self {
			id,
			name,
			kind,
			storage_mode,
		})
	}

	pub const fn id(&self) -> GraphId {
		self.id
	}

	pub const fn name(&self) -> &Text {
		&self.name
	}

	pub const fn kind(&self) -> GraphKind {
		self.kind
	}

	pub const fn storage_mode(&self) -> GraphStorageMode {
		self.storage_mode
	}

	pub fn read<'tx, T: GraphRefReader + ?Sized>(
		&self,
		transaction: &'tx T,
	) -> FerrosGrynnResult<T::View<'tx>> {
		transaction.read_graph(self)
	}
}

impl Display for GraphRef {
	fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
		Display::fmt(&self.name, formatter)
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

/// Read-only node projection whose cold fields may require storage I/O.
pub trait NodeRead {
	fn id(&self) -> AtomId;
	fn record_id(&self) -> FerrosGrynnResult<Cow<'_, RecordId>>;
	fn label_text(&self) -> FerrosGrynnResult<Option<Cow<'_, Text>>>;
	fn properties(&self) -> FerrosGrynnResult<Cow<'_, PropertyMap>>;
	fn inner_graph_ref(&self) -> FerrosGrynnResult<Option<Cow<'_, GraphRef>>>;

	fn inner_graph(&self) -> FerrosGrynnResult<Option<Cow<'_, Graph<Frozen>>>> {
		if self.inner_graph_ref()?.is_some() {
			return Err(FerrosGrynnError::invalid_argument(
				"this graph reader exposes nested metadata but not nested graph contents",
			));
		}
		Ok(None)
	}

	fn atom_ref(&self) -> FerrosGrynnResult<AtomRef> {
		Ok(AtomRef::new(AtomKind::Node, self.record_id()?.into_owned()))
	}

	fn label(&self) -> FerrosGrynnResult<Option<Cow<'_, str>>> {
		Ok(self.label_text()?.map(|label| match label {
			Cow::Borrowed(label) => Cow::Borrowed(label.as_str()),
			Cow::Owned(label) => Cow::Owned(label.as_str().to_owned()),
		}))
	}

	fn is_meta(&self) -> FerrosGrynnResult<bool> {
		Ok(self.inner_graph_ref()?.is_some())
	}
}

impl NodeRead for NodeView<'_> {
	fn id(&self) -> AtomId {
		self.id
	}

	fn record_id(&self) -> FerrosGrynnResult<Cow<'_, RecordId>> {
		Ok(Cow::Borrowed(self.record_id))
	}

	fn label_text(&self) -> FerrosGrynnResult<Option<Cow<'_, Text>>> {
		Ok(self.label.map(Cow::Borrowed))
	}

	fn properties(&self) -> FerrosGrynnResult<Cow<'_, PropertyMap>> {
		Ok(Cow::Borrowed(self.properties))
	}

	fn inner_graph_ref(&self) -> FerrosGrynnResult<Option<Cow<'_, GraphRef>>> {
		Ok(self.inner_graph.and_then(GraphRead::graph_ref).map(Cow::Borrowed))
	}

	fn inner_graph(&self) -> FerrosGrynnResult<Option<Cow<'_, Graph<Frozen>>>> {
		Ok(self.inner_graph.map(Cow::Borrowed))
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

/// Read-only edge projection whose topology and cold fields may require
/// storage I/O.
pub trait EdgeRead {
	fn id(&self) -> AtomId;
	fn record_id(&self) -> FerrosGrynnResult<Cow<'_, RecordId>>;
	fn topology(&self) -> FerrosGrynnResult<Cow<'_, EdgeTopology>>;
	fn label_text(&self) -> FerrosGrynnResult<Option<Cow<'_, Text>>>;
	fn properties(&self) -> FerrosGrynnResult<Cow<'_, PropertyMap>>;
	fn weight(&self) -> Option<Number>;
	fn transition_graph_ref(&self) -> FerrosGrynnResult<Option<Cow<'_, GraphRef>>>;

	fn transition_graph(&self) -> FerrosGrynnResult<Option<Cow<'_, Graph<Frozen>>>> {
		if self.transition_graph_ref()?.is_some() {
			return Err(FerrosGrynnError::invalid_argument(
				"this graph reader exposes transition metadata but not transition graph contents",
			));
		}
		Ok(None)
	}

	fn atom_ref(&self) -> FerrosGrynnResult<AtomRef> {
		Ok(AtomRef::new(AtomKind::Edge, self.record_id()?.into_owned()))
	}

	fn label(&self) -> FerrosGrynnResult<Option<Cow<'_, str>>> {
		Ok(self.label_text()?.map(|label| match label {
			Cow::Borrowed(label) => Cow::Borrowed(label.as_str()),
			Cow::Owned(label) => Cow::Owned(label.as_str().to_owned()),
		}))
	}

	fn is_meta(&self) -> FerrosGrynnResult<bool> {
		Ok(self.transition_graph_ref()?.is_some())
	}
}

impl EdgeRead for EdgeView<'_> {
	fn id(&self) -> AtomId {
		self.id
	}

	fn record_id(&self) -> FerrosGrynnResult<Cow<'_, RecordId>> {
		Ok(Cow::Borrowed(self.record_id))
	}

	fn topology(&self) -> FerrosGrynnResult<Cow<'_, EdgeTopology>> {
		Ok(Cow::Borrowed(self.topology))
	}

	fn label_text(&self) -> FerrosGrynnResult<Option<Cow<'_, Text>>> {
		Ok(self.label.map(Cow::Borrowed))
	}

	fn properties(&self) -> FerrosGrynnResult<Cow<'_, PropertyMap>> {
		Ok(Cow::Borrowed(self.properties))
	}

	fn weight(&self) -> Option<Number> {
		self.weight
	}

	fn transition_graph_ref(&self) -> FerrosGrynnResult<Option<Cow<'_, GraphRef>>> {
		Ok(self.transition_graph.and_then(GraphRead::graph_ref).map(Cow::Borrowed))
	}

	fn transition_graph(&self) -> FerrosGrynnResult<Option<Cow<'_, Graph<Frozen>>>> {
		Ok(self.transition_graph.map(Cow::Borrowed))
	}
}

pub trait GraphRead {
	type Edge<'a>: EdgeRead + 'a
	where
		Self: 'a;
	type EdgeIds<'a>: Iterator<Item = FerrosGrynnResult<AtomId>> + 'a
	where
		Self: 'a;
	type Node<'a>: NodeRead + 'a
	where
		Self: 'a;
	type NodeIds<'a>: Iterator<Item = FerrosGrynnResult<AtomId>> + 'a
	where
		Self: 'a;

	fn identity(&self) -> Uuid;
	fn revision(&self) -> u64;
	fn kind(&self) -> GraphKind;
	fn properties(&self) -> FerrosGrynnResult<Cow<'_, PropertyMap>>;
	fn node_ids(&self) -> FerrosGrynnResult<Self::NodeIds<'_>>;
	fn edge_ids(&self) -> FerrosGrynnResult<Self::EdgeIds<'_>>;
	fn node(&self, id: AtomId) -> FerrosGrynnResult<Option<Self::Node<'_>>>;
	fn edge(&self, id: AtomId) -> FerrosGrynnResult<Option<Self::Edge<'_>>>;

	fn storage_mode(&self) -> GraphStorageMode {
		GraphStorageMode::InMemory
	}

	fn snapshot_id(&self) -> SnapshotId {
		SnapshotId::in_memory(self.revision())
	}

	fn graph_ref(&self) -> Option<&GraphRef> {
		None
	}

	fn contains(&self, id: AtomId) -> FerrosGrynnResult<bool> {
		Ok(match id.kind() {
			Some(AtomKind::Node) => self.node(id)?.is_some(),
			Some(AtomKind::Edge) => self.edge(id)?.is_some(),
			None => false,
		})
	}

	fn node_count(&self) -> FerrosGrynnResult<usize> {
		self.node_ids()?.try_fold(0_usize, |count, id| id.map(|_id| count + 1))
	}

	fn edge_count(&self) -> FerrosGrynnResult<usize> {
		self.edge_ids()?.try_fold(0_usize, |count, id| id.map(|_id| count + 1))
	}

	fn atom_ref(&self, id: AtomId) -> FerrosGrynnResult<Option<AtomRef>> {
		match id.kind() {
			Some(AtomKind::Node) => self.node(id)?.map(|node| node.atom_ref()).transpose(),
			Some(AtomKind::Edge) => self.edge(id)?.map(|edge| edge.atom_ref()).transpose(),
			None => Ok(None),
		}
	}

	fn atom_id(&self, reference: &AtomRef) -> FerrosGrynnResult<Option<AtomId>> {
		let ids = match reference.kind() {
			AtomKind::Node => self.node_ids()?,
			AtomKind::Edge => return self.edge_id(reference),
		};
		for id in ids {
			let id = id?;
			if let Some(node) = self.node(id)?
				&& node.record_id()?.as_ref() == reference.id()
			{
				return Ok(Some(id));
			}
		}
		Ok(None)
	}

	fn edge_id(&self, reference: &AtomRef) -> FerrosGrynnResult<Option<AtomId>> {
		for id in self.edge_ids()? {
			let id = id?;
			if let Some(edge) = self.edge(id)?
				&& edge.record_id()?.as_ref() == reference.id()
			{
				return Ok(Some(id));
			}
		}
		Ok(None)
	}
}

pub trait GraphMut: GraphRead {
	fn properties_mut(&mut self) -> FerrosGrynnResult<&mut PropertyMap>;
	fn node_label_mut(&mut self, id: AtomId) -> FerrosGrynnResult<&mut Option<Text>>;
	fn node_properties_mut(&mut self, id: AtomId) -> FerrosGrynnResult<&mut PropertyMap>;
	fn edge_label_mut(&mut self, id: AtomId) -> FerrosGrynnResult<&mut Option<Text>>;
	fn edge_properties_mut(&mut self, id: AtomId) -> FerrosGrynnResult<&mut PropertyMap>;
	fn remove_atom(&mut self, id: AtomId) -> FerrosGrynnResult<()>;
}

#[derive(Debug, Clone, PartialOrd, PartialEq, Eq, Ord, Hash)]
#[non_exhaustive]
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

	pub(crate) fn source_atoms(&self) -> impl ExactSizeIterator<Item = (AtomId, &AtomRef)> {
		self.source_atoms.iter().map(|(local, atom)| (*local, atom))
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
	type Edge<'a>
		= R::Edge<'a>
	where
		Self: 'a;
	type EdgeIds<'a>
		= R::EdgeIds<'a>
	where
		Self: 'a;
	type Node<'a>
		= R::Node<'a>
	where
		Self: 'a;
	type NodeIds<'a>
		= R::NodeIds<'a>
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

	fn properties(&self) -> FerrosGrynnResult<Cow<'_, PropertyMap>> {
		self.source.properties()
	}

	fn node_ids(&self) -> FerrosGrynnResult<Self::NodeIds<'_>> {
		self.source.node_ids()
	}

	fn edge_ids(&self) -> FerrosGrynnResult<Self::EdgeIds<'_>> {
		self.source.edge_ids()
	}

	fn node(&self, id: AtomId) -> FerrosGrynnResult<Option<Self::Node<'_>>> {
		self.source.node(id)
	}

	fn edge(&self, id: AtomId) -> FerrosGrynnResult<Option<Self::Edge<'_>>> {
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
	type Edge<'a>
		= R::Edge<'a>
	where
		Self: 'a;
	type EdgeIds<'a>
		= R::EdgeIds<'a>
	where
		Self: 'a;
	type Node<'a>
		= R::Node<'a>
	where
		Self: 'a;
	type NodeIds<'a>
		= R::NodeIds<'a>
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

	fn properties(&self) -> FerrosGrynnResult<Cow<'_, PropertyMap>> {
		self.view.properties()
	}

	fn node_ids(&self) -> FerrosGrynnResult<Self::NodeIds<'_>> {
		self.view.node_ids()
	}

	fn edge_ids(&self) -> FerrosGrynnResult<Self::EdgeIds<'_>> {
		self.view.edge_ids()
	}

	fn node(&self, id: AtomId) -> FerrosGrynnResult<Option<Self::Node<'_>>> {
		self.view.node(id)
	}

	fn edge(&self, id: AtomId) -> FerrosGrynnResult<Option<Self::Edge<'_>>> {
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
		DirtyTarget,
		DiskGraphView,
		GraphId,
		GraphKind,
		GraphMut,
		GraphRead,
		GraphRef,
		GraphRefReader,
		GraphStorageMode,
		GraphWriteBack,
		MaterializationOrigin,
		NodeRead,
		SnapshotId,
		query_to_memory,
	};
	use crate::{
		gql_status::{FerrosGrynnError, FerrosGrynnResult},
		values::runtime::{
			PropertyMap,
			Value,
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
			GraphId::new(1).unwrap(),
			"example",
			GraphKind::Graph,
			GraphStorageMode::DiskBacked,
		)
		.unwrap();
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
		let hybrid_root = hybrid.node_ids().unwrap().next().unwrap().unwrap();
		assert_eq!(stable_bfs(&hybrid, hybrid_root), source_order);
		assert!(hybrid.graph_ref().is_some());
	}

	#[test]
	fn mutable_hybrid_flush_uses_origin_mapping_and_dirty_set() {
		let (source, _) = sample_graph();
		let source_identity = source.identity();
		let reference = GraphRef::new(
			GraphId::new(2).unwrap(),
			"mutable",
			GraphKind::Graph,
			GraphStorageMode::DiskBacked,
		)
		.unwrap();
		let transaction = FakeTransaction {
			reference: reference.clone(),
			graph: source,
			snapshot: SnapshotId::new(52, 3),
			_lifetime: Arc::new(()),
		};
		let view = reference.read(&transaction).unwrap();
		let mut hybrid = query_to_memory::<Graph<Mutable>, _>(&view).unwrap();
		let local = hybrid.node_ids().unwrap().next().unwrap().unwrap();
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
		let removed_node = hybrid.node_ids().unwrap().nth(1).unwrap().unwrap();
		let removed_edges =
			hybrid.edge_ids().unwrap().collect::<FerrosGrynnResult<Vec<_>>>().unwrap();

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
			.map(|id| graph.node(id).unwrap().unwrap().record_id().unwrap().to_string())
			.collect()
	}

	fn assert_snapshot_released(snapshot: &Weak<()>) {
		assert!(snapshot.upgrade().is_none());
	}
}
