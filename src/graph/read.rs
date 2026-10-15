use std::{
	borrow::Cow,
	cmp::Ordering,
	collections::BTreeMap,
	fmt::{self, Display, Formatter},
	hash::{Hash, Hasher},
	sync::Arc,
};

use super::{AtomId, AtomKind, EdgeTopology, Frozen, Graph, GraphId};
use crate::{
	collections::cow_vec,
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	values::runtime::{Number, PropertyMap, RecordId, Text, Uuid},
};

/// Resolves containment metadata without materializing the child graph.
///
/// # Errors
/// Propagates reader errors and rejects missing or invalid owners.
#[allow(clippy::redundant_pub_crate, reason = "Re-exported at crate scope for snapshot adapters")]
pub(crate) fn nested_graph_reference<G: GraphRead>(
	graph: &G,
	owner: AtomId,
) -> FerrosGrynnResult<Option<GraphRef>> {
	match owner.kind() {
		Some(AtomKind::Node) => graph
			.node(owner)?
			.ok_or_else(|| FerrosGrynnError::atom_does_not_exist("node", owner.to_string()))?
			.inner_graph_ref()
			.map(|value| value.map(Cow::into_owned)),
		Some(AtomKind::Edge) => graph
			.edge(owner)?
			.ok_or_else(|| FerrosGrynnError::atom_does_not_exist("edge", owner.to_string()))?
			.transition_graph_ref()
			.map(|value| value.map(Cow::into_owned)),
		None => Err(FerrosGrynnError::invalid_storage_file(
			"MATCH containment owner has an invalid atom kind",
		)),
	}
}

/// Stable iterator over atom handles in a graph snapshot.
pub struct AtomIds<'a> {
	/// Inner associated with this atom ids.
	inner: cow_vec::Iter<'a, AtomId>,
}

impl<'a> AtomIds<'a> {
	/// Creates a new `AtomIds<'a>` value.
	pub(crate) const fn new(inner: cow_vec::Iter<'a, AtomId>) -> Self {
		Self {
			inner,
		}
	}
}

impl Iterator for AtomIds<'_> {
	type Item = AtomId;

	/// Advances this `AtomIds<'_>` and returns its next element, if any.
	fn next(&mut self) -> Option<Self::Item> {
		self.inner.next().copied()
	}

	/// Returns lower and optional upper bounds on the remaining element count.
	fn size_hint(&self) -> (usize, Option<usize>) {
		self.inner.size_hint()
	}
}

impl ExactSizeIterator for AtomIds<'_> {}
impl std::iter::FusedIterator for AtomIds<'_> {}

/// Adapts an infallible in-memory atom iterator to the fallible storage
/// contract without allocating or boxing it.
pub struct FallibleAtomIds<Inner> {
	/// Inner associated with this fallible atom ids.
	inner: Inner,
}

impl<Inner> FallibleAtomIds<Inner> {
	/// Creates a new `FallibleAtomIds<Inner>` value.
	pub(crate) const fn new(inner: Inner) -> Self {
		Self {
			inner,
		}
	}
}

impl<Inner: Iterator<Item = AtomId>> Iterator for FallibleAtomIds<Inner> {
	type Item = FerrosGrynnResult<AtomId>;

	/// Advances this `FallibleAtomIds<Inner>` and returns its next element, if
	/// any.
	fn next(&mut self) -> Option<Self::Item> {
		self.inner.next().map(Ok)
	}

	/// Returns lower and optional upper bounds on the remaining element count.
	fn size_hint(&self) -> (usize, Option<usize>) {
		self.inner.size_hint()
	}
}

impl<Inner: ExactSizeIterator<Item = AtomId>> ExactSizeIterator for FallibleAtomIds<Inner> {}
impl<Inner: std::iter::FusedIterator<Item = AtomId>> std::iter::FusedIterator
	for FallibleAtomIds<Inner>
{
}

/// Structural graph family identified by a graph reference or reader.
#[derive(Debug, Clone, Copy, PartialOrd, PartialEq, Eq, Ord, Hash)]
#[non_exhaustive]
pub enum GraphKind {
	/// General graph family.
	Graph,
	/// Connected acyclic tree family.
	Tree,
	/// Tree family with an explicitly selected root.
	RootedTree,
	/// Acyclic forest family allowing disconnected components.
	Forest,
	/// Directed acyclic graph family.
	Dag,
	/// Hypergraph family with multi-endpoint incidences.
	Hypergraph,
	/// Metagraph family retaining graph containment.
	Metagraph,
	/// Subgraph family selecting contents from a containing graph.
	Subgraph,
}

impl GraphKind {
	/// Returns name for `GraphKind`.
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
	/// Formats `GraphKind` using the requested formatter.
	fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
		formatter.write_str(self.name())
	}
}

/// Orthogonal topology validator attached to a graph contract.
#[derive(Debug, Clone, Copy, PartialOrd, PartialEq, Eq, Ord, Hash)]
#[non_exhaustive]
pub enum GraphTopologyConstraint {
	/// Rejects self-loops and repeated unordered endpoint pairs.
	Simple,
	/// Requires a two-colorable binary node projection.
	Bipartite,
	/// Requires an edge for every unordered pair of distinct nodes.
	Complete,
}

impl GraphTopologyConstraint {
	/// Returns the bit result.
	const fn bit(self) -> u16 {
		match self {
			Self::Simple => 1 << 0,
			Self::Bipartite => 1 << 1,
			Self::Complete => 1 << 2,
		}
	}
}

/// Compact durable set of graph topology validators.
#[derive(Debug, Default, Clone, Copy, PartialOrd, PartialEq, Eq, Ord, Hash)]
#[repr(transparent)]
pub struct GraphTopologyConstraints(
	/// Bit set selecting the durable SIMPLE, BIPARTITE and COMPLETE validators.
	u16,
);

impl GraphTopologyConstraints {
	/// Configured known bits value.
	pub(crate) const KNOWN_BITS: u16 = (1 << 3) - 1;

	/// Returns a graph contract without additional topology validators.
	pub const fn empty() -> Self {
		Self(0)
	}

	/// Returns whether this contract contains the requested validator.
	pub const fn contains(self, constraint: GraphTopologyConstraint) -> bool {
		self.0 & constraint.bit() != 0
	}

	/// Iterates validators in their canonical order.
	pub fn iter(self) -> impl Iterator<Item = GraphTopologyConstraint> {
		[
			GraphTopologyConstraint::Simple,
			GraphTopologyConstraint::Bipartite,
			GraphTopologyConstraint::Complete,
		]
		.into_iter()
		.filter(move |constraint| self.contains(*constraint))
	}

	/// Inserts graph topology constraints.
	pub(crate) const fn insert(&mut self, constraint: GraphTopologyConstraint) -> bool {
		let bit = constraint.bit();
		let inserted = self.0 & bit == 0;
		self.0 |= bit;
		inserted
	}

	/// Returns the bits result.
	pub(crate) const fn bits(self) -> u16 {
		self.0
	}

	/// Creates from bits.
	pub(crate) const fn from_bits(bits: u16) -> Option<Self> {
		if bits & !Self::KNOWN_BITS == 0 {
			Some(Self(bits))
		} else {
			None
		}
	}
}

/// Whether graph data is materialized in memory, read from disk or combined.
#[derive(Debug, Clone, Copy, PartialOrd, PartialEq, Eq, Ord, Hash)]
#[non_exhaustive]
pub enum GraphStorageMode {
	/// Graph contents are materialized in memory.
	InMemory,
	/// Graph contents are read through the local storage backend.
	DiskBacked,
	/// Graph reads combine resident materialization and local storage.
	Hybrid,
}

/// Owned graph identity and metadata resolved through a transaction reader.
#[derive(Debug, Clone, PartialOrd, PartialEq, Eq, Ord, Hash)]
pub struct GraphRef {
	/// Identifier used to locate this value within its owning subsystem.
	id: GraphId,
	/// Name associated with this graph ref.
	name: Text,
	/// Kind associated with this graph ref.
	kind: GraphKind,
	/// Storage mode associated with this graph ref.
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

	/// Returns the stored id of this `GraphRef`.
	pub const fn id(&self) -> GraphId {
		self.id
	}

	/// Borrows the stored name of this `GraphRef`.
	pub const fn name(&self) -> &Text {
		&self.name
	}

	/// Returns the stored kind of this `GraphRef`.
	pub const fn kind(&self) -> GraphKind {
		self.kind
	}

	/// Returns the stored storage mode of this `GraphRef`.
	pub const fn storage_mode(&self) -> GraphStorageMode {
		self.storage_mode
	}

	/// Resolves this graph reference through the supplied graph-reference
	/// reader.
	///
	/// # Errors
	///
	/// Propagates validation or execution failures from the underlying
	/// operation.
	pub fn read<'tx, T: GraphRefReader + ?Sized>(
		&self,
		transaction: &'tx T,
	) -> FerrosGrynnResult<T::View<'tx>> {
		transaction.read_graph(self)
	}
}

impl Display for GraphRef {
	/// Formats `GraphRef` using the requested formatter.
	fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
		Display::fmt(&self.name, formatter)
	}
}

/// Storage/query transaction boundary used by [`GraphRef::read`].
pub trait GraphRefReader {
	/// Transaction-bound graph view returned when resolving an owned graph
	/// reference.
	type View<'tx>: GraphRead + 'tx
	where
		Self: 'tx;

	/// Reads graph for `GraphRefReader`.
	///
	/// # Errors
	///
	/// Propagates validation or execution failures from the underlying
	/// operation.
	fn read_graph(&self, reference: &GraphRef) -> FerrosGrynnResult<Self::View<'_>>;
}

/// Identity of one MVCC read version; it does not own graph data.
#[derive(Debug, Default, Clone, Copy, PartialOrd, PartialEq, Eq, Ord, Hash)]
pub struct SnapshotId {
	/// Timestamp associated with this snapshot id.
	timestamp: u64,
	/// Transaction associated with this snapshot id.
	transaction: u64,
}

impl SnapshotId {
	/// Constructs `SnapshotId` from the supplied initial state.
	pub const fn new(timestamp: u64, transaction: u64) -> Self {
		Self {
			timestamp,
			transaction,
		}
	}

	/// Returns in memory for `SnapshotId`.
	pub const fn in_memory(revision: u64) -> Self {
		Self::new(revision, 0)
	}

	/// Returns the stored timestamp of this `SnapshotId`.
	pub const fn timestamp(self) -> u64 {
		self.timestamp
	}

	/// Returns the stored transaction of this `SnapshotId`.
	pub const fn transaction(self) -> u64 {
		self.transaction
	}
}

impl Display for SnapshotId {
	/// Formats `SnapshotId` using the requested formatter.
	fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
		write!(formatter, "{}:{}", self.timestamp, self.transaction)
	}
}

/// Owned record identity paired with its node-or-edge kind.
#[derive(Debug, Clone, PartialOrd, PartialEq, Eq, Ord, Hash)]
pub struct AtomRef {
	/// Kind associated with this atom ref.
	kind: AtomKind,
	/// Identifier used to locate this value within its owning subsystem.
	id: RecordId,
}

/// One stable atom identity in one catalog graph occurrence.
///
/// [`AtomRef`] identifies the database-global atom, while this type supplies
/// the graph scope required for graph-local labels, properties, and topology.
#[derive(Debug, Clone)]
pub struct AtomOccurrenceRef {
	/// Atom associated with this atom occurrence ref.
	atom: AtomRef,
	/// Graph associated with this atom occurrence ref.
	graph: Arc<GraphRef>,
}

impl AtomOccurrenceRef {
	/// Creates an occurrence in the supplied catalog graph.
	pub const fn new(atom: AtomRef, graph: Arc<GraphRef>) -> Self {
		Self {
			atom,
			graph,
		}
	}

	/// Returns the database-global atom identity.
	pub const fn atom(&self) -> &AtomRef {
		&self.atom
	}

	/// Returns the graph scope of this occurrence.
	pub fn graph(&self) -> &GraphRef {
		self.graph.as_ref()
	}

	/// Splits this occurrence into its global atom and shared graph reference.
	pub fn into_parts(self) -> (AtomRef, Arc<GraphRef>) {
		(self.atom, self.graph)
	}
}

impl PartialEq for AtomOccurrenceRef {
	/// Compares this `AtomOccurrenceRef` with another value for equality.
	fn eq(&self, other: &Self) -> bool {
		self.cmp(other) == Ordering::Equal
	}
}

impl Eq for AtomOccurrenceRef {}

impl PartialOrd for AtomOccurrenceRef {
	/// Compares this `AtomOccurrenceRef` with another value when they are
	/// ordered.
	fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
		Some(self.cmp(other))
	}
}

impl Ord for AtomOccurrenceRef {
	/// Compares this `AtomOccurrenceRef` with another value using its ordering
	/// contract.
	fn cmp(&self, other: &Self) -> Ordering {
		(self.graph.id(), &self.atom).cmp(&(other.graph.id(), &other.atom))
	}
}

impl Hash for AtomOccurrenceRef {
	/// Feeds this `AtomOccurrenceRef` into the supplied hasher.
	fn hash<H: Hasher>(&self, state: &mut H) {
		self.graph.id().hash(state);
		self.atom.hash(state);
	}
}

impl AtomRef {
	/// Constructs `AtomRef` from the supplied initial state.
	pub const fn new(kind: AtomKind, id: RecordId) -> Self {
		Self {
			kind,
			id,
		}
	}

	/// Returns the stored kind of this `AtomRef`.
	pub const fn kind(&self) -> AtomKind {
		self.kind
	}

	/// Borrows the stored id of this `AtomRef`.
	pub const fn id(&self) -> &RecordId {
		&self.id
	}
}

/// Borrowed node identity, label, properties and optional nested graph.
#[derive(Debug, Clone, Copy)]
pub struct NodeView<'a> {
	/// Identifier used to locate this value within its owning subsystem.
	id: AtomId,
	/// Stable record identifier of the associated atom.
	record_id: &'a RecordId,
	/// Label associated with this node view.
	label: Option<&'a Text>,
	/// Properties associated with this node view.
	properties: &'a PropertyMap,
	/// Inner graph associated with this node view.
	inner_graph: Option<&'a Graph<Frozen>>,
}

impl<'a> NodeView<'a> {
	/// Creates a new `NodeView<'a>` value.
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

	/// Returns the stored id of this `NodeView<'a>`.
	pub const fn id(self) -> AtomId {
		self.id
	}

	/// Returns the stored record id of this `NodeView<'a>`.
	pub const fn record_id(self) -> &'a RecordId {
		self.record_id
	}

	/// Returns atom ref for `NodeView<'a>`.
	pub fn atom_ref(self) -> AtomRef {
		AtomRef::new(AtomKind::Node, self.record_id.clone())
	}

	/// Returns label for `NodeView<'a>`.
	pub fn label(self) -> Option<&'a str> {
		self.label.map(Text::as_str)
	}

	/// Returns the stored label of this `NodeView<'a>`.
	pub const fn label_text(self) -> Option<&'a Text> {
		self.label
	}

	/// Returns the stored properties of this `NodeView<'a>`.
	pub const fn properties(self) -> &'a PropertyMap {
		self.properties
	}

	/// Returns the stored inner graph of this `NodeView<'a>`.
	pub const fn inner_graph(self) -> Option<&'a Graph<Frozen>> {
		self.inner_graph
	}

	/// Reports whether the value is meta for `NodeView<'a>`.
	pub const fn is_meta(self) -> bool {
		self.inner_graph.is_some()
	}
}

/// Read-only node projection whose cold fields may require storage I/O.
pub trait NodeRead {
	/// Returns id for `NodeRead`.
	fn id(&self) -> AtomId;
	/// Records id for `NodeRead`.
	///
	/// # Errors
	///
	/// Propagates validation or execution failures from the underlying
	/// operation.
	fn record_id(&self) -> FerrosGrynnResult<Cow<'_, RecordId>>;
	/// Returns label text for `NodeRead`.
	///
	/// # Errors
	///
	/// Propagates validation or execution failures from the underlying
	/// operation.
	fn label_text(&self) -> FerrosGrynnResult<Option<Cow<'_, Text>>>;
	/// Returns properties for `NodeRead`.
	///
	/// # Errors
	///
	/// Propagates validation or execution failures from the underlying
	/// operation.
	fn properties(&self) -> FerrosGrynnResult<Cow<'_, PropertyMap>>;
	/// Returns inner graph ref for `NodeRead`.
	///
	/// # Errors
	///
	/// Propagates validation or execution failures from the underlying
	/// operation.
	fn inner_graph_ref(&self) -> FerrosGrynnResult<Option<Cow<'_, GraphRef>>>;

	/// Returns inner graph for `NodeRead`.
	///
	/// # Errors
	///
	/// Returns `invalid_argument` diagnostics and propagates failures from
	/// called operations.
	fn inner_graph(&self) -> FerrosGrynnResult<Option<Cow<'_, Graph<Frozen>>>> {
		if self.inner_graph_ref()?.is_some() {
			return Err(FerrosGrynnError::invalid_argument(
				"this graph reader exposes nested metadata but not nested graph contents",
			));
		}
		Ok(None)
	}

	/// Returns atom ref for `NodeRead`.
	///
	/// # Errors
	///
	/// Propagates validation or execution failures from the underlying
	/// operation.
	fn atom_ref(&self) -> FerrosGrynnResult<AtomRef> {
		Ok(AtomRef::new(AtomKind::Node, self.record_id()?.into_owned()))
	}

	/// Returns label for `NodeRead`.
	///
	/// # Errors
	///
	/// Propagates validation or execution failures from the underlying
	/// operation.
	fn label(&self) -> FerrosGrynnResult<Option<Cow<'_, str>>> {
		Ok(self.label_text()?.map(|label| match label {
			Cow::Borrowed(label) => Cow::Borrowed(label.as_str()),
			Cow::Owned(label) => Cow::Owned(label.as_str().to_owned()),
		}))
	}

	/// Reports whether the value is meta for `NodeRead`.
	///
	/// # Errors
	///
	/// Propagates validation or execution failures from the underlying
	/// operation.
	fn is_meta(&self) -> FerrosGrynnResult<bool> {
		Ok(self.inner_graph_ref()?.is_some())
	}
}

impl NodeRead for NodeView<'_> {
	/// Returns the stored id of this `NodeView<'_>`.
	fn id(&self) -> AtomId {
		self.id
	}

	/// Records id for `NodeView<'_>`.
	fn record_id(&self) -> FerrosGrynnResult<Cow<'_, RecordId>> {
		Ok(Cow::Borrowed(self.record_id))
	}

	/// Returns label text for `NodeView<'_>`.
	fn label_text(&self) -> FerrosGrynnResult<Option<Cow<'_, Text>>> {
		Ok(self.label.map(Cow::Borrowed))
	}

	/// Returns properties for `NodeView<'_>`.
	fn properties(&self) -> FerrosGrynnResult<Cow<'_, PropertyMap>> {
		Ok(Cow::Borrowed(self.properties))
	}

	/// Returns inner graph ref for `NodeView<'_>`.
	fn inner_graph_ref(&self) -> FerrosGrynnResult<Option<Cow<'_, GraphRef>>> {
		Ok(self.inner_graph.and_then(GraphRead::graph_ref).map(Cow::Borrowed))
	}

	/// Returns inner graph for `NodeView<'_>`.
	fn inner_graph(&self) -> FerrosGrynnResult<Option<Cow<'_, Graph<Frozen>>>> {
		Ok(self.inner_graph.map(Cow::Borrowed))
	}

	/// Reports whether the value is meta for `NodeView<'_>`.
	fn is_meta(&self) -> FerrosGrynnResult<bool> {
		Ok(self.inner_graph.is_some())
	}
}

/// Borrowed edge identity, topology, properties and optional transition graph.
#[derive(Debug, Clone, Copy)]
pub struct EdgeView<'a> {
	/// Identifier used to locate this value within its owning subsystem.
	id: AtomId,
	/// Stable record identifier of the associated atom.
	record_id: &'a RecordId,
	/// Topology associated with this edge view.
	topology: &'a EdgeTopology,
	/// Label associated with this edge view.
	label: Option<&'a Text>,
	/// Properties associated with this edge view.
	properties: &'a PropertyMap,
	/// Weight associated with this edge view.
	weight: Option<Number>,
	/// Transition graph associated with this edge view.
	transition_graph: Option<&'a Graph<Frozen>>,
}

impl<'a> EdgeView<'a> {
	/// Creates a new `EdgeView<'a>` value.
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

	/// Returns the stored id of this `EdgeView<'a>`.
	pub const fn id(self) -> AtomId {
		self.id
	}

	/// Returns the stored record id of this `EdgeView<'a>`.
	pub const fn record_id(self) -> &'a RecordId {
		self.record_id
	}

	/// Returns atom ref for `EdgeView<'a>`.
	pub fn atom_ref(self) -> AtomRef {
		AtomRef::new(AtomKind::Edge, self.record_id.clone())
	}

	/// Returns the stored topology of this `EdgeView<'a>`.
	pub const fn topology(self) -> &'a EdgeTopology {
		self.topology
	}

	/// Returns label for `EdgeView<'a>`.
	pub fn label(self) -> Option<&'a str> {
		self.label.map(Text::as_str)
	}

	/// Returns the stored label of this `EdgeView<'a>`.
	pub const fn label_text(self) -> Option<&'a Text> {
		self.label
	}

	/// Returns the stored properties of this `EdgeView<'a>`.
	pub const fn properties(self) -> &'a PropertyMap {
		self.properties
	}

	/// Returns the stored weight of this `EdgeView<'a>`.
	pub const fn weight(self) -> Option<Number> {
		self.weight
	}

	/// Returns the stored transition graph of this `EdgeView<'a>`.
	pub const fn transition_graph(self) -> Option<&'a Graph<Frozen>> {
		self.transition_graph
	}

	/// Reports whether the value is meta for `EdgeView<'a>`.
	pub const fn is_meta(self) -> bool {
		self.transition_graph.is_some()
	}
}

/// Read-only edge projection whose topology and cold fields may require
/// storage I/O.
pub trait EdgeRead {
	/// Returns id for `EdgeRead`.
	fn id(&self) -> AtomId;
	/// Records id for `EdgeRead`.
	///
	/// # Errors
	///
	/// Propagates validation or execution failures from the underlying
	/// operation.
	fn record_id(&self) -> FerrosGrynnResult<Cow<'_, RecordId>>;
	/// Returns topology for `EdgeRead`.
	///
	/// # Errors
	///
	/// Propagates validation or execution failures from the underlying
	/// operation.
	fn topology(&self) -> FerrosGrynnResult<Cow<'_, EdgeTopology>>;
	/// Returns label text for `EdgeRead`.
	///
	/// # Errors
	///
	/// Propagates validation or execution failures from the underlying
	/// operation.
	fn label_text(&self) -> FerrosGrynnResult<Option<Cow<'_, Text>>>;
	/// Returns properties for `EdgeRead`.
	///
	/// # Errors
	///
	/// Propagates validation or execution failures from the underlying
	/// operation.
	fn properties(&self) -> FerrosGrynnResult<Cow<'_, PropertyMap>>;
	/// Returns weight for `EdgeRead`.
	fn weight(&self) -> Option<Number>;
	/// Returns transition graph ref for `EdgeRead`.
	///
	/// # Errors
	///
	/// Propagates validation or execution failures from the underlying
	/// operation.
	fn transition_graph_ref(&self) -> FerrosGrynnResult<Option<Cow<'_, GraphRef>>>;

	/// Returns transition graph for `EdgeRead`.
	///
	/// # Errors
	///
	/// Returns `invalid_argument` diagnostics and propagates failures from
	/// called operations.
	fn transition_graph(&self) -> FerrosGrynnResult<Option<Cow<'_, Graph<Frozen>>>> {
		if self.transition_graph_ref()?.is_some() {
			return Err(FerrosGrynnError::invalid_argument(
				"this graph reader exposes transition metadata but not transition graph contents",
			));
		}
		Ok(None)
	}

	/// Returns atom ref for `EdgeRead`.
	///
	/// # Errors
	///
	/// Propagates validation or execution failures from the underlying
	/// operation.
	fn atom_ref(&self) -> FerrosGrynnResult<AtomRef> {
		Ok(AtomRef::new(AtomKind::Edge, self.record_id()?.into_owned()))
	}

	/// Returns label for `EdgeRead`.
	///
	/// # Errors
	///
	/// Propagates validation or execution failures from the underlying
	/// operation.
	fn label(&self) -> FerrosGrynnResult<Option<Cow<'_, str>>> {
		Ok(self.label_text()?.map(|label| match label {
			Cow::Borrowed(label) => Cow::Borrowed(label.as_str()),
			Cow::Owned(label) => Cow::Owned(label.as_str().to_owned()),
		}))
	}

	/// Reports whether the value is meta for `EdgeRead`.
	///
	/// # Errors
	///
	/// Propagates validation or execution failures from the underlying
	/// operation.
	fn is_meta(&self) -> FerrosGrynnResult<bool> {
		Ok(self.transition_graph_ref()?.is_some())
	}
}

impl EdgeRead for EdgeView<'_> {
	/// Returns the stored id of this `EdgeView<'_>`.
	fn id(&self) -> AtomId {
		self.id
	}

	/// Records id for `EdgeView<'_>`.
	fn record_id(&self) -> FerrosGrynnResult<Cow<'_, RecordId>> {
		Ok(Cow::Borrowed(self.record_id))
	}

	/// Returns topology for `EdgeView<'_>`.
	fn topology(&self) -> FerrosGrynnResult<Cow<'_, EdgeTopology>> {
		Ok(Cow::Borrowed(self.topology))
	}

	/// Returns label text for `EdgeView<'_>`.
	fn label_text(&self) -> FerrosGrynnResult<Option<Cow<'_, Text>>> {
		Ok(self.label.map(Cow::Borrowed))
	}

	/// Returns properties for `EdgeView<'_>`.
	fn properties(&self) -> FerrosGrynnResult<Cow<'_, PropertyMap>> {
		Ok(Cow::Borrowed(self.properties))
	}

	/// Returns the stored weight of this `EdgeView<'_>`.
	fn weight(&self) -> Option<Number> {
		self.weight
	}

	/// Returns transition graph ref for `EdgeView<'_>`.
	fn transition_graph_ref(&self) -> FerrosGrynnResult<Option<Cow<'_, GraphRef>>> {
		Ok(self.transition_graph.and_then(GraphRead::graph_ref).map(Cow::Borrowed))
	}

	/// Returns transition graph for `EdgeView<'_>`.
	fn transition_graph(&self) -> FerrosGrynnResult<Option<Cow<'_, Graph<Frozen>>>> {
		Ok(self.transition_graph.map(Cow::Borrowed))
	}

	/// Reports whether the value is meta for `EdgeView<'_>`.
	fn is_meta(&self) -> FerrosGrynnResult<bool> {
		Ok(self.transition_graph.is_some())
	}
}

/// Shared fallible read contract for in-memory, disk-backed and hybrid graph
/// views.
pub trait GraphRead {
	/// Borrowed edge view produced by this graph reader.
	type Edge<'a>: EdgeRead + 'a
	where
		Self: 'a;
	/// Fallible iterator over edge identifiers visible to this reader.
	type EdgeIds<'a>: Iterator<Item = FerrosGrynnResult<AtomId>> + 'a
	where
		Self: 'a;
	/// Borrowed node view produced by this graph reader.
	type Node<'a>: NodeRead + 'a
	where
		Self: 'a;
	/// Fallible iterator over node identifiers visible to this reader.
	type NodeIds<'a>: Iterator<Item = FerrosGrynnResult<AtomId>> + 'a
	where
		Self: 'a;

	/// Returns the graph identity checked when resolving graph-qualified paths.
	fn identity(&self) -> Uuid;
	/// Returns the graph revision exposed by this reader.
	fn revision(&self) -> u64;
	/// Identifies the structural graph family exposed by this reader.
	fn kind(&self) -> GraphKind;
	/// Reads graph-level properties as a borrowed or owned property map.
	///
	/// # Errors
	///
	/// Propagates validation or execution failures from the underlying
	/// operation.
	fn properties(&self) -> FerrosGrynnResult<Cow<'_, PropertyMap>>;

	/// Returns durable topology validators attached to this graph.
	fn topology_constraints(&self) -> GraphTopologyConstraints {
		GraphTopologyConstraints::empty()
	}
	/// Returns a fallible iterator over visible materialization-local node
	/// handles.
	///
	/// # Errors
	///
	/// Propagates validation or execution failures from the underlying
	/// operation.
	fn node_ids(&self) -> FerrosGrynnResult<Self::NodeIds<'_>>;
	/// Returns a fallible iterator over visible materialization-local edge
	/// handles.
	///
	/// # Errors
	///
	/// Propagates validation or execution failures from the underlying
	/// operation.
	fn edge_ids(&self) -> FerrosGrynnResult<Self::EdgeIds<'_>>;
	/// Returns exact connector identities for a qualified edge endpoint when
	/// indexed.
	///
	/// # Errors
	/// Returns snapshot, incidence, or resource errors from the graph reader.
	fn edge_endpoint_connectors(
		&self,
		_endpoint: &crate::graph::EdgeEndpoint,
	) -> FerrosGrynnResult<Option<Vec<AtomId>>> {
		Ok(None)
	}
	/// Looks up a node view using a materialization-local atom handle.
	///
	/// # Errors
	///
	/// Propagates validation or execution failures from the underlying
	/// operation.
	fn node(&self, id: AtomId) -> FerrosGrynnResult<Option<Self::Node<'_>>>;
	/// Looks up an edge view using a materialization-local atom handle.
	///
	/// # Errors
	///
	/// Propagates validation or execution failures from the underlying
	/// operation.
	fn edge(&self, id: AtomId) -> FerrosGrynnResult<Option<Self::Edge<'_>>>;

	/// Identifies whether this reader exposes in-memory, disk-backed or hybrid
	/// graph storage.
	fn storage_mode(&self) -> GraphStorageMode {
		GraphStorageMode::InMemory
	}

	/// Returns the snapshot token checked when resolving snapshot-qualified
	/// paths.
	fn snapshot_id(&self) -> SnapshotId {
		SnapshotId::in_memory(self.revision())
	}

	/// Borrows the durable graph reference when one is associated with this
	/// reader.
	fn graph_ref(&self) -> Option<&GraphRef> {
		None
	}

	/// Returns owned write-back provenance when this reader is a
	/// materialization.
	fn materialization_origin(&self) -> Option<&MaterializationOrigin> {
		None
	}

	/// Tests whether a node or edge with this handle is present in the reader.
	/// # Errors
	///
	/// Propagates failures from the node or edge lookup.
	fn contains(&self, id: AtomId) -> FerrosGrynnResult<bool> {
		Ok(match id.kind() {
			Some(AtomKind::Node) => self.node(id)?.is_some(),
			Some(AtomKind::Edge) => self.edge(id)?.is_some(),
			None => false,
		})
	}

	/// Returns the number of nodes visible through this reader.
	///
	/// # Errors
	///
	/// Propagates errors returned by the underlying operation.
	fn node_count(&self) -> FerrosGrynnResult<usize> {
		self.node_ids()?.try_fold(0_usize, |count, id| id.map(|_id| count + 1))
	}

	/// Returns the number of edges visible through this reader.
	///
	/// # Errors
	///
	/// Propagates errors returned by the underlying operation.
	fn edge_count(&self) -> FerrosGrynnResult<usize> {
		self.edge_ids()?.try_fold(0_usize, |count, id| id.map(|_id| count + 1))
	}

	/// Resolves a local node-or-edge handle to an owned, record-based atom
	/// reference.
	///
	/// # Errors
	///
	/// Propagates errors returned by the underlying operation.
	fn atom_ref(&self, id: AtomId) -> FerrosGrynnResult<Option<AtomRef>> {
		match id.kind() {
			Some(AtomKind::Node) => self.node(id)?.map(|node| node.atom_ref()).transpose(),
			Some(AtomKind::Edge) => self.edge(id)?.map(|edge| edge.atom_ref()).transpose(),
			None => Ok(None),
		}
	}

	/// Resolves a record-based atom reference to a visible
	/// materialization-local handle.
	///
	/// # Errors
	///
	/// Propagates errors returned by the underlying operation.
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

	/// Finds a visible edge handle matching the supplied reference's record
	/// identity.
	///
	/// # Errors
	///
	/// Propagates errors returned by the underlying operation.
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

/// Mutation contract for graph properties, atom properties and atom removal.
pub trait GraphMut: GraphRead {
	/// Returns properties mut for `GraphMut`.
	///
	/// # Errors
	///
	/// Propagates validation or execution failures from the underlying
	/// operation.
	fn properties_mut(&mut self) -> FerrosGrynnResult<&mut PropertyMap>;
	/// Returns node label mut for `GraphMut`.
	///
	/// # Errors
	///
	/// Propagates validation or execution failures from the underlying
	/// operation.
	fn node_label_mut(&mut self, id: AtomId) -> FerrosGrynnResult<&mut Option<Text>>;
	/// Returns node properties mut for `GraphMut`.
	///
	/// # Errors
	///
	/// Propagates validation or execution failures from the underlying
	/// operation.
	fn node_properties_mut(&mut self, id: AtomId) -> FerrosGrynnResult<&mut PropertyMap>;
	/// Returns edge label mut for `GraphMut`.
	///
	/// # Errors
	///
	/// Propagates validation or execution failures from the underlying
	/// operation.
	fn edge_label_mut(&mut self, id: AtomId) -> FerrosGrynnResult<&mut Option<Text>>;
	/// Returns edge properties mut for `GraphMut`.
	///
	/// # Errors
	///
	/// Propagates validation or execution failures from the underlying
	/// operation.
	fn edge_properties_mut(&mut self, id: AtomId) -> FerrosGrynnResult<&mut PropertyMap>;
	/// Removes atom for `GraphMut`.
	///
	/// # Errors
	///
	/// Propagates validation or execution failures from the underlying
	/// operation.
	fn remove_atom(&mut self, id: AtomId) -> FerrosGrynnResult<()>;
}

/// Graph-level or atom-level target changed in a writable materialization.
#[derive(Debug, Clone, PartialOrd, PartialEq, Eq, Ord, Hash)]
#[non_exhaustive]
pub enum DirtyTarget {
	/// Graph-level metadata or properties have changed.
	Graph,
	/// A particular atom has changed.
	Atom(
		/// A particular atom has changed.
		AtomId,
	),
}

/// Snapshot provenance and stable atom references retained for graph
/// write-back.
#[derive(Debug, Clone)]
pub struct MaterializationOrigin {
	/// Identity associated with this materialization origin.
	identity: Uuid,
	/// Snapshot associated with this materialization origin.
	snapshot: SnapshotId,
	/// Graph ref associated with this materialization origin.
	graph_ref: Option<GraphRef>,
	/// Maps local handles to qualified logical source atoms for automatic
	/// updates.
	source_atoms: Arc<BTreeMap<AtomId, AtomRef>>,
}

impl MaterializationOrigin {
	/// Restores logical source mappings without a live storage reader.
	pub(crate) fn from_parts(
		identity: Uuid,
		snapshot: SnapshotId,
		graph_ref: Option<GraphRef>,
		source_atoms: BTreeMap<AtomId, AtomRef>,
	) -> Self {
		Self {
			identity,
			snapshot,
			graph_ref,
			source_atoms: Arc::new(source_atoms),
		}
	}

	/// Creates a new `MaterializationOrigin` value.
	pub(crate) fn new<R: GraphRead>(source: &R, source_atoms: BTreeMap<AtomId, AtomRef>) -> Self {
		Self {
			identity: source.identity(),
			snapshot: source.snapshot_id(),
			graph_ref: source.graph_ref().cloned(),
			source_atoms: Arc::new(source_atoms),
		}
	}

	/// Returns the stored identity of this `MaterializationOrigin`.
	pub const fn identity(&self) -> Uuid {
		self.identity
	}

	/// Returns the stored snapshot of this `MaterializationOrigin`.
	pub const fn snapshot(&self) -> SnapshotId {
		self.snapshot
	}

	/// Returns graph ref for `MaterializationOrigin`.
	pub const fn graph_ref(&self) -> Option<&GraphRef> {
		self.graph_ref.as_ref()
	}

	/// Returns source atom for `MaterializationOrigin`.
	pub fn source_atom(&self, local: AtomId) -> Option<&AtomRef> {
		self.source_atoms.get(&local)
	}

	/// Returns the source atoms result.
	pub(crate) fn source_atoms(&self) -> impl ExactSizeIterator<Item = (AtomId, &AtomRef)> {
		self.source_atoms.iter().map(|(local, atom)| (*local, atom))
	}

	/// Conservative retained bytes; immutable origin maps share storage across
	/// COW clones.
	pub(crate) fn estimated_heap_bytes(&self) -> usize {
		self.source_atoms.values().fold(
			std::mem::size_of::<BTreeMap<AtomId, AtomRef>>()
				+ 2 * std::mem::size_of::<usize>()
				+ self
					.graph_ref
					.as_ref()
					.map_or(0, |graph| graph.name().len() + std::mem::size_of::<GraphRef>()),
			|bytes, atom| {
				bytes.saturating_add(
					128 + std::mem::size_of::<AtomId>()
						+ std::mem::size_of::<AtomRef>()
						+ atom.id().estimated_heap_bytes(),
				)
			},
		)
	}
}

/// Transaction boundary for publishing changes from a graph materialization.
pub trait GraphWriteBack<G> {
	/// Performs the flush operation for `GraphWriteBack`.
	///
	/// # Errors
	///
	/// Propagates validation or execution failures from the underlying
	/// operation.
	fn flush(
		&mut self,
		graph: &G,
		origin: &MaterializationOrigin,
		dirty: &[DirtyTarget],
	) -> FerrosGrynnResult<()>;
}

/// Transaction boundary for producing an owned graph-family query result.
pub trait QueryMaterialize: Sized {
	/// Returns query from for `QueryMaterialize`.
	///
	/// # Errors
	///
	/// Propagates validation or execution failures from the underlying
	/// operation.
	fn query_from<R: GraphRead>(source: &R) -> FerrosGrynnResult<Self>;
}

/// Returns query to memory for this module.
///
/// # Errors
///
/// Propagates validation or execution failures from the underlying operation.
pub fn query_to_memory<T, R>(source: &R) -> FerrosGrynnResult<T>
where
	T: QueryMaterialize,
	R: GraphRead,
{
	T::query_from(source)
}

/// Borrowed graph reader bound to the snapshot from which it was obtained.
pub struct GraphReadView<'tx, R: GraphRead + ?Sized> {
	/// Source associated with this graph read view.
	source: &'tx R,
	/// Snapshot associated with this graph read view.
	snapshot: SnapshotId,
}

impl<'tx, R: GraphRead + ?Sized> GraphReadView<'tx, R> {
	/// Constructs `GraphReadView<'tx, R>` from the supplied initial state.
	pub const fn new(source: &'tx R, snapshot: SnapshotId) -> Self {
		Self {
			source,
			snapshot,
		}
	}

	/// Returns the stored source of this `GraphReadView<'tx, R>`.
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

	/// Returns identity for `GraphReadView<'_, R>`.
	fn identity(&self) -> Uuid {
		self.source.identity()
	}

	/// Returns revision for `GraphReadView<'_, R>`.
	fn revision(&self) -> u64 {
		self.source.revision()
	}

	/// Returns kind for `GraphReadView<'_, R>`.
	fn kind(&self) -> GraphKind {
		self.source.kind()
	}

	/// Returns properties for `GraphReadView<'_, R>`.
	fn properties(&self) -> FerrosGrynnResult<Cow<'_, PropertyMap>> {
		self.source.properties()
	}

	/// Returns topology constraints for `GraphReadView<'_, R>`.
	fn topology_constraints(&self) -> GraphTopologyConstraints {
		self.source.topology_constraints()
	}

	/// Returns node ids for `GraphReadView<'_, R>`.
	fn node_ids(&self) -> FerrosGrynnResult<Self::NodeIds<'_>> {
		self.source.node_ids()
	}

	/// Returns edge ids for `GraphReadView<'_, R>`.
	fn edge_ids(&self) -> FerrosGrynnResult<Self::EdgeIds<'_>> {
		self.source.edge_ids()
	}

	/// Returns node for `GraphReadView<'_, R>`.
	fn node(&self, id: AtomId) -> FerrosGrynnResult<Option<Self::Node<'_>>> {
		self.source.node(id)
	}

	/// Returns edge for `GraphReadView<'_, R>`.
	fn edge(&self, id: AtomId) -> FerrosGrynnResult<Option<Self::Edge<'_>>> {
		self.source.edge(id)
	}

	/// Returns storage mode for `GraphReadView<'_, R>`.
	fn storage_mode(&self) -> GraphStorageMode {
		self.source.storage_mode()
	}

	/// Returns the stored snapshot of this `GraphReadView<'_, R>`.
	fn snapshot_id(&self) -> SnapshotId {
		self.snapshot
	}

	/// Returns graph ref for `GraphReadView<'_, R>`.
	fn graph_ref(&self) -> Option<&GraphRef> {
		self.source.graph_ref()
	}
}

/// A transaction-bound disk adapter view.
///
/// The source is supplied by the storage layer and may perform page reads; the
/// view itself never owns or materializes the graph.
pub struct DiskGraphView<'tx, R: GraphRead + ?Sized> {
	/// Graph ref associated with this disk graph view.
	graph_ref: &'tx GraphRef,
	/// View associated with this disk graph view.
	view: GraphReadView<'tx, R>,
}

impl<'tx, R: GraphRead + ?Sized> DiskGraphView<'tx, R> {
	/// Constructs `DiskGraphView<'tx, R>` from the supplied initial state.
	///
	/// # Errors
	///
	/// Returns `graph_constraint_violation` diagnostics.
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

	/// Returns identity for `DiskGraphView<'_, R>`.
	fn identity(&self) -> Uuid {
		self.view.identity()
	}

	/// Returns revision for `DiskGraphView<'_, R>`.
	fn revision(&self) -> u64 {
		self.view.revision()
	}

	/// Returns kind for `DiskGraphView<'_, R>`.
	fn kind(&self) -> GraphKind {
		self.graph_ref.kind()
	}

	/// Returns properties for `DiskGraphView<'_, R>`.
	fn properties(&self) -> FerrosGrynnResult<Cow<'_, PropertyMap>> {
		self.view.properties()
	}

	/// Returns topology constraints for `DiskGraphView<'_, R>`.
	fn topology_constraints(&self) -> GraphTopologyConstraints {
		self.view.topology_constraints()
	}

	/// Returns node ids for `DiskGraphView<'_, R>`.
	fn node_ids(&self) -> FerrosGrynnResult<Self::NodeIds<'_>> {
		self.view.node_ids()
	}

	/// Returns edge ids for `DiskGraphView<'_, R>`.
	fn edge_ids(&self) -> FerrosGrynnResult<Self::EdgeIds<'_>> {
		self.view.edge_ids()
	}

	/// Returns node for `DiskGraphView<'_, R>`.
	fn node(&self, id: AtomId) -> FerrosGrynnResult<Option<Self::Node<'_>>> {
		self.view.node(id)
	}

	/// Returns edge for `DiskGraphView<'_, R>`.
	fn edge(&self, id: AtomId) -> FerrosGrynnResult<Option<Self::Edge<'_>>> {
		self.view.edge(id)
	}

	/// Returns storage mode for `DiskGraphView<'_, R>`.
	fn storage_mode(&self) -> GraphStorageMode {
		GraphStorageMode::DiskBacked
	}

	/// Returns snapshot id for `DiskGraphView<'_, R>`.
	fn snapshot_id(&self) -> SnapshotId {
		self.view.snapshot_id()
	}

	/// Returns graph ref for `DiskGraphView<'_, R>`.
	fn graph_ref(&self) -> Option<&GraphRef> {
		Some(self.graph_ref)
	}
}

#[cfg(test)]
/// Regression tests for read behavior.
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
	#[cfg(test)]
	use crate::test_assertions::AssertOption as _;
	#[cfg(test)]
	use crate::test_assertions::AssertResult as _;
	use crate::{
		gql_status::{FerrosGrynnError, FerrosGrynnResult},
		values::runtime::{
			PropertyMap,
			Value,
			graph::{AtomId, EdgeTopology, Frozen, Graph, Mutable, TraversalDirection, bfs},
		},
	};

	/// Transaction test double exposing one graph and its snapshot identity.
	struct FakeTransaction {
		/// Owned graph reference identifying the operation scope.
		reference: GraphRef,
		/// Graph materialization or physical graph files retained by the
		/// operation.
		graph: Graph<Frozen>,
		/// Snapshot identity or retained read snapshot used by the operation.
		snapshot: SnapshotId,
		/// Shared lifetime token used to verify borrowed snapshot ownership.
		_lifetime: Arc<()>,
	}

	impl GraphRefReader for FakeTransaction {
		type View<'tx> = DiskGraphView<'tx, Graph<Frozen>>;

		/// Reads graph for `FakeTransaction`.
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

	/// Write-back test double retaining the accepted origin and dirty targets.
	#[derive(Default)]
	struct RecordingWriter {
		/// Origin graph identity recorded by the write-back test double.
		origin_identity: Option<crate::values::runtime::Uuid>,
		/// Changed targets recorded by the write-back test double.
		dirty: Vec<DirtyTarget>,
	}

	impl GraphWriteBack<Graph<Mutable>> for RecordingWriter {
		/// Performs the flush operation for `RecordingWriter`.
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

	/// Verifies disk view and hybrid materialization have the same read result.
	#[test]
	fn disk_view_and_hybrid_materialization_have_the_same_read_result() {
		let (source, root) = sample_graph();
		let source_order = stable_bfs(&source, root);
		let reference = GraphRef::new(
			GraphId::new(1).assert_some(),
			"example",
			GraphKind::Graph,
			GraphStorageMode::DiskBacked,
		)
		.assert_ok();
		let lifetime = Arc::new(());
		let weak = Arc::downgrade(&lifetime);
		let hybrid = {
			let transaction = FakeTransaction {
				reference: reference.clone(),
				graph: source,
				snapshot: SnapshotId::new(41, 7),
				_lifetime: lifetime,
			};
			let view = reference.read(&transaction).assert_ok();
			assert_eq!(view.storage_mode(), GraphStorageMode::DiskBacked);
			assert_eq!(view.snapshot_id(), SnapshotId::new(41, 7));
			assert_eq!(stable_bfs(&view, root), source_order);
			query_to_memory::<Graph<Frozen>, _>(&view).assert_ok()
		};

		assert_snapshot_released(&weak);
		assert_eq!(hybrid.storage_mode(), GraphStorageMode::Hybrid);
		let hybrid_root = hybrid.node_ids().assert_ok().next().assert_some().assert_ok();
		assert_eq!(stable_bfs(&hybrid, hybrid_root), source_order);
		assert!(hybrid.graph_ref().is_some());
	}

	/// Verifies mutable hybrid flush uses origin mapping and dirty set.
	#[test]
	fn mutable_hybrid_flush_uses_origin_mapping_and_dirty_set() {
		let (source, _) = sample_graph();
		let source_identity = source.identity();
		let reference = GraphRef::new(
			GraphId::new(2).assert_some(),
			"mutable",
			GraphKind::Graph,
			GraphStorageMode::DiskBacked,
		)
		.assert_ok();
		let transaction = FakeTransaction {
			reference: reference.clone(),
			graph: source,
			snapshot: SnapshotId::new(52, 3),
			_lifetime: Arc::new(()),
		};
		let view = reference.read(&transaction).assert_ok();
		let mut hybrid = query_to_memory::<Graph<Mutable>, _>(&view).assert_ok();
		let local = hybrid.node_ids().assert_ok().next().assert_some().assert_ok();
		assert!(
			hybrid.materialization_origin().and_then(|origin| origin.source_atom(local)).is_some()
		);
		GraphMut::node_properties_mut(&mut hybrid, local)
			.assert_ok()
			.insert(1_u32.into(), Value::from("changed"));

		let mut writer = RecordingWriter::default();
		hybrid.flush(&mut writer).assert_ok();
		assert_eq!(writer.origin_identity, Some(source_identity));
		assert_eq!(writer.dirty, vec![DirtyTarget::Atom(local)]);

		hybrid.flush(&mut writer).assert_ok();
		assert!(writer.dirty.is_empty());
	}

	/// Verifies cascade removal marks removed incident edges dirty.
	#[test]
	fn cascade_removal_marks_removed_incident_edges_dirty() {
		let (source, _) = sample_graph();
		let mut hybrid = query_to_memory::<Graph<Mutable>, _>(&source).assert_ok();
		let removed_node = hybrid.node_ids().assert_ok().nth(1).assert_some().assert_ok();
		let removed_edges =
			hybrid.edge_ids().assert_ok().collect::<FerrosGrynnResult<Vec<_>>>().assert_ok();

		GraphMut::remove_atom(&mut hybrid, removed_node).assert_ok();

		let mut writer = RecordingWriter::default();
		hybrid.flush(&mut writer).assert_ok();
		assert_eq!(writer.dirty.len(), removed_edges.len() + 1);
		assert!(writer.dirty.contains(&DirtyTarget::Atom(removed_node)));
		for edge in removed_edges {
			assert!(writer.dirty.contains(&DirtyTarget::Atom(edge)));
		}
	}

	/// Returns sample graph for this module.
	fn sample_graph() -> (Graph<Frozen>, AtomId) {
		let mut graph = Graph::<Mutable>::new();
		let first = graph.add_node(Some("first".into()), PropertyMap::new()).assert_ok();
		let second = graph.add_node(Some("second".into()), PropertyMap::new()).assert_ok();
		let third = graph.add_node(Some("third".into()), PropertyMap::new()).assert_ok();
		graph
			.add_edge(EdgeTopology::directed([first], [second]), None, PropertyMap::new())
			.assert_ok();
		graph
			.add_edge(EdgeTopology::directed([second], [third]), None, PropertyMap::new())
			.assert_ok();
		(graph.freeze(), first)
	}

	/// Returns stable bfs for this module.
	fn stable_bfs<R: GraphRead>(graph: &R, root: AtomId) -> Vec<String> {
		bfs(graph, root, TraversalDirection::Forward, None)
			.assert_ok()
			.into_iter()
			.map(|id| graph.node(id).assert_ok().assert_some().record_id().assert_ok().to_string())
			.collect()
	}

	/// Returns assert snapshot released for this module.
	fn assert_snapshot_released(snapshot: &Weak<()>) {
		assert!(snapshot.upgrade().is_none());
	}
}
