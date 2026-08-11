use std::{
	collections::{BTreeMap, BTreeSet},
	sync::{
		Arc,
		atomic::{AtomicU64, Ordering},
	},
	time::{Duration, Instant},
};

use parking_lot::{Mutex, RwLock};

use crate::{
	collections::cow_vec::CowVec,
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	values::runtime::{
		PropertyDictionary,
		PropertyKeyId,
		PropertyMap,
		RecordId,
		Text,
		Type,
		Uuid,
		Value,
		graph::{
			AtomId,
			AtomKind,
			Dag,
			DirtyTarget,
			EdgeView,
			Forest,
			Frozen,
			Graph,
			GraphKind,
			GraphRead,
			GraphRef,
			GraphRefReader,
			GraphStorageMode,
			Hypergraph,
			MaterializedGraph,
			Metagraph,
			Mutable,
			NodeView,
			RootedTree,
			SnapshotId,
			Tree,
			merge_transaction_graph,
		},
	},
};

const DEFAULT_MAX_SNAPSHOT_AGE: Duration = Duration::from_secs(300);
const DEFAULT_MAX_WRITE_SET_BYTES: usize = 256 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct InMemoryOptions {
	pub max_snapshot_age: Duration,
	pub max_write_set_bytes: usize,
}

impl Default for InMemoryOptions {
	fn default() -> Self {
		Self {
			max_snapshot_age: DEFAULT_MAX_SNAPSHOT_AGE,
			max_write_set_bytes: DEFAULT_MAX_WRITE_SET_BYTES,
		}
	}
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum IsolationLevel {
	ReadCommitted,
	Snapshot,
	#[default]
	Serializable,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum TransactionAccess {
	#[default]
	ReadWrite,
	ReadOnly,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct TransactionOptions {
	pub isolation: IsolationLevel,
	pub access: TransactionAccess,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum TableKind {
	Any,
	Normal,
	Relation,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TableField {
	name: Text,
	value_type: Type,
	key: Option<PropertyKeyId>,
}

impl TableField {
	pub fn new<IntoText: Into<Text>>(name: IntoText, value_type: Type) -> Self {
		Self {
			name: name.into(),
			value_type,
			key: None,
		}
	}

	pub const fn name(&self) -> &Text {
		&self.name
	}

	pub const fn value_type(&self) -> &Type {
		&self.value_type
	}
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TableSchema {
	name: Text,
	graph: GraphRef,
	kind: TableKind,
	fields: Vec<TableField>,
}

impl TableSchema {
	pub fn new<IntoText, Fields>(
		name: IntoText,
		graph: GraphRef,
		kind: TableKind,
		fields: Fields,
	) -> FerrosGrynnResult<Self>
	where IntoText: Into<Text>, Fields: IntoIterator<Item = TableField>
	{
		let name = name.into();
		if name.as_str().is_empty() {
			return Err(FerrosGrynnError::invalid_argument("table name must not be empty"));
		}
		let fields = fields.into_iter().collect::<Vec<_>>();
		let mut names = BTreeSet::new();
		if fields.iter().any(|field| {
			field.name.as_str().is_empty()
				|| field.name.as_str() == "id"
				|| !names.insert(field.name.as_str().to_owned())
		}) {
			return Err(FerrosGrynnError::invalid_argument(
				"table fields must have unique non-empty names and must not redefine implicit 'id'",
			));
		}
		Ok(Self {
			name,
			graph,
			kind,
			fields,
		})
	}

	pub const fn name(&self) -> &Text {
		&self.name
	}

	pub const fn graph(&self) -> &GraphRef {
		&self.graph
	}

	pub const fn kind(&self) -> TableKind {
		self.kind
	}

	pub fn fields(&self) -> impl ExactSizeIterator<Item = &TableField> {
		self.fields.iter()
	}
}

#[derive(Clone)]
pub struct Datastore {
	inner: Arc<StoreInner>,
}

struct StoreInner {
	options: InMemoryOptions,
	next_transaction: AtomicU64,
	current: RwLock<Arc<DatabaseRoot>>,
	commit: Mutex<()>,
	active: Mutex<BTreeMap<u64, ActiveSnapshot>>,
}

struct ActiveSnapshot {
	start_ts: u64,
	started: Instant,
}

#[derive(Clone)]
struct DatabaseRoot {
	commit_ts: u64,
	graphs: BTreeMap<RecordId, StoredGraph>,
	graph_catalog_versions: BTreeMap<RecordId, u64>,
	tables: BTreeMap<String, Arc<TableSchema>>,
	table_versions: BTreeMap<String, u64>,
	dictionary: Arc<PropertyDictionary>,
}

#[derive(Clone)]
struct StoredGraph {
	graph: Arc<MaterializedGraph>,
	last_commit_ts: u64,
	properties_version: u64,
	node_set_version: u64,
	edge_set_version: u64,
	atom_versions: AtomVersions,
	changes: CowVec<(u64, AtomId)>,
}

#[derive(Clone, Default)]
struct AtomVersions {
	nodes: CowVec<VersionHistory>,
	edges: CowVec<VersionHistory>,
}

#[derive(Clone, Copy)]
struct VersionEntry {
	generation: u32,
	commit_ts: u64,
}

#[allow(clippy::rc_buffer, reason = "the size changes dynamically during insertion")]
#[derive(Clone, Default)]
struct VersionHistory {
	latest_generation: u32,
	latest_commit_ts: u64,
	older: Option<Arc<Vec<VersionEntry>>>,
}

impl VersionHistory {
	fn get(&self, generation: u32) -> u64 {
		if self.latest_commit_ts != 0 && self.latest_generation == generation {
			return self.latest_commit_ts;
		}
		self.older
			.as_deref()
			.and_then(|entries| entries.iter().rev().find(|entry| entry.generation == generation))
			.map_or(0, |entry| entry.commit_ts)
	}

	fn insert(&mut self, generation: u32, commit_ts: u64) {
		if self.latest_commit_ts == 0 || self.latest_generation == generation {
			self.latest_generation = generation;
			self.latest_commit_ts = commit_ts;
			return;
		}
		Arc::make_mut(self.older.get_or_insert_with(|| Arc::new(Vec::new()))).push(VersionEntry {
			generation: self.latest_generation,
			commit_ts: self.latest_commit_ts,
		});
		self.latest_generation = generation;
		self.latest_commit_ts = commit_ts;
	}
}

impl AtomVersions {
	fn get(&self, atom: AtomId) -> u64 {
		let Ok(slot) = usize::try_from(atom.slot()) else {
			return 0;
		};
		match atom.kind() {
			Some(AtomKind::Node) => self.nodes.get(slot),
			Some(AtomKind::Edge) => self.edges.get(slot),
			None => None,
		}
		.map_or(0, |history| history.get(atom.generation()))
	}

	fn insert(&mut self, atom: AtomId, commit_ts: u64) {
		let Ok(slot) = usize::try_from(atom.slot()) else {
			return;
		};
		let Some(required_len) = slot.checked_add(1) else {
			return;
		};
		let slots = match atom.kind() {
			Some(AtomKind::Node) => &mut self.nodes,
			Some(AtomKind::Edge) => &mut self.edges,
			None => return,
		};
		if slots.len() < required_len {
			slots.resize_with(required_len, VersionHistory::default);
		}
		slots[slot].insert(atom.generation(), commit_ts);
	}
}

impl Default for DatabaseRoot {
	fn default() -> Self {
		Self {
			commit_ts: 0,
			graphs: BTreeMap::new(),
			graph_catalog_versions: BTreeMap::new(),
			tables: BTreeMap::new(),
			table_versions: BTreeMap::new(),
			dictionary: Arc::new(PropertyDictionary::new()),
		}
	}
}

impl DatabaseRoot {
	fn version(&self, key: &ReadKey) -> u64 {
		match key {
			ReadKey::GraphCatalog(graph) => {
				self.graph_catalog_versions.get(graph).copied().unwrap_or(0)
			}
			ReadKey::GraphProperties(graph) => self.graphs.get(graph).map_or_else(
				|| self.graph_catalog_versions.get(graph).copied().unwrap_or(0),
				|stored| stored.properties_version,
			),
			ReadKey::NodeSet(graph) => self.graphs.get(graph).map_or_else(
				|| self.graph_catalog_versions.get(graph).copied().unwrap_or(0),
				|stored| stored.node_set_version,
			),
			ReadKey::EdgeSet(graph) => self.graphs.get(graph).map_or_else(
				|| self.graph_catalog_versions.get(graph).copied().unwrap_or(0),
				|stored| stored.edge_set_version,
			),
			ReadKey::Atom(graph, atom) => self.graphs.get(graph).map_or_else(
				|| self.graph_catalog_versions.get(graph).copied().unwrap_or(0),
				|stored| stored.atom_versions.get(*atom),
			),
			ReadKey::Table(table) => self.table_versions.get(table).copied().unwrap_or(0),
		}
	}
}

impl Datastore {
	pub fn in_memory() -> Self {
		Self::in_memory_with(InMemoryOptions::default())
	}

	pub fn in_memory_with(options: InMemoryOptions) -> Self {
		Self {
			inner: Arc::new(StoreInner {
				options,
				next_transaction: AtomicU64::new(1),
				current: RwLock::new(Arc::new(DatabaseRoot::default())),
				commit: Mutex::new(()),
				active: Mutex::new(BTreeMap::new()),
			}),
		}
	}

	pub fn options(&self) -> InMemoryOptions {
		self.inner.options
	}

	pub fn begin(&self) -> Transaction {
		self.begin_with(TransactionOptions::default())
	}

	pub fn begin_with(&self, options: TransactionOptions) -> Transaction {
		let started = Instant::now();
		let current = self.inner.current.read();
		let root = current.clone();
		let id = self.inner.next_transaction.fetch_add(1, Ordering::Relaxed);
		if options.isolation == IsolationLevel::Serializable
			&& options.access == TransactionAccess::ReadWrite
		{
			self.inner.active.lock().insert(id, ActiveSnapshot {
				start_ts: root.commit_ts,
				started,
			});
		}
		drop(current);
		Transaction {
			store: self.clone(),
			id,
			start_ts: root.commit_ts,
			started,
			options,
			root,
			reads: Mutex::new(BTreeMap::new()),
			predicates: Mutex::new(Vec::new()),
			graphs: BTreeMap::new(),
			tables: BTreeMap::new(),
		}
	}

	pub fn table(&self, name: &str) -> Option<Arc<TableSchema>> {
		self.current_root().tables.get(name).cloned()
	}

	/// Interns an append-only property name before it is used in graph values
	/// or table schemas.
	pub fn intern_property<IntoText: Into<Text>>(&self, name: IntoText) -> FerrosGrynnResult<PropertyKeyId> {
		let name = name.into();
		if name.as_str().is_empty() {
			return Err(FerrosGrynnError::invalid_argument("property name must not be empty"));
		}
		let _commit = self.inner.commit.lock();
		let current = self.current_root();
		if let Some(key) = current.dictionary.get(name.as_str()) {
			return Ok(key);
		}
		let operation = self.inner.next_transaction.fetch_add(1, Ordering::Relaxed);
		let commit_ts = current.commit_ts.checked_add(1).ok_or_else(|| {
			FerrosGrynnError::serialization_failure(operation, "the commit clock is exhausted")
		})?;
		let mut next = (*current).clone();
		let key = Arc::make_mut(&mut next.dictionary).intern(name)?;
		next.commit_ts = commit_ts;
		*self.inner.current.write() = Arc::new(next);
		Ok(key)
	}

	pub fn property_dictionary(&self) -> Arc<PropertyDictionary> {
		self.current_root().dictionary.clone()
	}

	pub fn execute(
		&self,
		source: &str,
		variables: &crate::values::Object,
	) -> FerrosGrynnResult<Vec<crate::gql::QueryResult>> {
		self.execute_with_settings(source, variables, crate::gql::ParserSettings::default())
	}

	pub fn execute_with_settings(
		&self,
		source: &str,
		variables: &crate::values::Object,
		settings: crate::gql::ParserSettings,
	) -> FerrosGrynnResult<Vec<crate::gql::QueryResult>> {
		let transaction = self.begin_with(TransactionOptions {
			isolation: IsolationLevel::Serializable,
			access: TransactionAccess::ReadOnly,
		});
		let results = transaction.execute_with_settings(source, variables, settings)?;
		if results.iter().any(|result| result.result.is_err()) {
			return Ok(results);
		}
		transaction.commit()?;
		Ok(results)
	}

	fn current_root(&self) -> Arc<DatabaseRoot> {
		self.inner.current.read().clone()
	}
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CommitResult {
	transaction: u64,
	commit_ts: u64,
}

impl CommitResult {
	pub const fn transaction(self) -> u64 {
		self.transaction
	}

	pub const fn commit_ts(self) -> u64 {
		self.commit_ts
	}
}

pub struct Transaction {
	store: Datastore,
	id: u64,
	start_ts: u64,
	started: Instant,
	options: TransactionOptions,
	root: Arc<DatabaseRoot>,
	reads: Mutex<BTreeMap<ReadKey, u64>>,
	predicates: Mutex<Vec<PredicateRead>>,
	graphs: BTreeMap<RecordId, GraphWrite>,
	tables: BTreeMap<String, Option<TableSchema>>,
}

impl Drop for Transaction {
	fn drop(&mut self) {
		self.store.inner.active.lock().remove(&self.id);
	}
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
enum ReadKey {
	GraphCatalog(RecordId),
	GraphProperties(RecordId),
	NodeSet(RecordId),
	EdgeSet(RecordId),
	Atom(RecordId, AtomId),
	Table(String),
}

pub(crate) struct QueryStorageRow {
	pub(crate) record_id: RecordId,
	pub(crate) atom_id: AtomId,
	pub(crate) values: Vec<Value>,
}

pub(crate) struct QuerySnapshot {
	root: Arc<DatabaseRoot>,
}

impl QuerySnapshot {
	pub(crate) fn commit_ts(&self) -> u64 {
		self.root.commit_ts
	}
}

pub(crate) trait PredicateEvaluator {
	fn matches(&self, row: &QueryStorageRow) -> FerrosGrynnResult<bool>;
}

struct PredicateRead {
	table: String,
	root: Arc<DatabaseRoot>,
	matcher: PredicateMatcher,
}

enum PredicateMatcher {
	Query(Arc<dyn PredicateEvaluator>),
	Record(RecordId),
}

impl PredicateMatcher {
	fn matches(&self, row: &QueryStorageRow) -> FerrosGrynnResult<bool> {
		match self {
			Self::Query(evaluator) => evaluator.matches(row),
			Self::Record(record) => Ok(record == &row.record_id),
		}
	}
}

struct GraphWrite {
	base: Option<Arc<MaterializedGraph>>,
	base_ts: u64,
	value: Option<MutableGraphValue>,
}

enum MutableGraphValue {
	Graph(Graph<Mutable>),
	Tree(Tree<Mutable>),
	RootedTree(RootedTree<Mutable>),
	Forest(Forest<Mutable>),
	Dag(Dag<Mutable>),
	Hypergraph(Hypergraph<Mutable>),
	Metagraph(Metagraph<Mutable>),
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
	fn from_frozen(value: &MaterializedGraph) -> FerrosGrynnResult<Self> {
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

	const fn kind(&self) -> GraphKind {
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

	fn dirty(&self) -> Vec<DirtyTarget> {
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

	fn snapshot(&self) -> MaterializedGraph {
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

	fn freeze(self) -> MaterializedGraph {
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

impl Transaction {
	pub const fn id(&self) -> u64 {
		self.id
	}

	pub fn snapshot_id(&self) -> SnapshotId {
		SnapshotId::new(self.read_root().commit_ts, self.id)
	}

	pub const fn options(&self) -> TransactionOptions {
		self.options
	}

	pub fn execute(
		&self,
		source: &str,
		variables: &crate::values::Object,
	) -> FerrosGrynnResult<Vec<crate::gql::QueryResult>> {
		self.execute_with_settings(source, variables, crate::gql::ParserSettings::default())
	}

	pub fn execute_with_settings(
		&self,
		source: &str,
		variables: &crate::values::Object,
		settings: crate::gql::ParserSettings,
	) -> FerrosGrynnResult<Vec<crate::gql::QueryResult>> {
		crate::gql::execute_transaction(self, source, variables, settings)
	}

	pub fn create_graph(
		&mut self,
		id: RecordId,
		graph: MaterializedGraph,
	) -> FerrosGrynnResult<GraphRef> {
		self.ensure_writable()?;
		self.ensure_live()?;
		if self.visible_graph(&id).is_some() {
			return Err(FerrosGrynnError::graph_already_exists(id.to_string()));
		}
		let kind = graph.kind();
		let value = MutableGraphValue::from_frozen(&graph)?;
		self.graphs.insert(id.clone(), GraphWrite {
			base: None,
			base_ts: self.read_root().commit_ts,
			value: Some(value),
		});
		Ok(GraphRef::new(id, kind, GraphStorageMode::InMemory))
	}

	pub fn remove_graph(&mut self, reference: &GraphRef) -> FerrosGrynnResult<()> {
		self.ensure_writable()?;
		self.ensure_live()?;
		ensure_in_memory_reference(reference)?;
		if self.graphs.get(reference.id()).is_some_and(|write| write.base.is_none()) {
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
		// ponytail: raw family views avoid mirroring every graph mutation API; add
		// tracked family guards if callers cannot follow the explicit read-before-write
		// contract.
		self.ensure_writable()?;
		self.ensure_live()?;
		self.ensure_graph_write(reference)?;
		self.graphs
			.get_mut(reference.id())
			.and_then(|write| write.value.as_mut())
			.map(MutableGraphValue::view)
			.ok_or_else(|| FerrosGrynnError::graph_does_not_exist(reference.id().to_string()))
	}

	pub fn define_table(&mut self, mut schema: TableSchema) -> FerrosGrynnResult<()> {
		self.ensure_writable()?;
		self.ensure_live()?;
		ensure_in_memory_reference(&schema.graph)?;
		if self.visible_table(schema.name.as_str()).is_some() {
			return Err(FerrosGrynnError::table_already_exists(schema.name.as_str()));
		}
		let graph = self
			.visible_graph(schema.graph.id())
			.ok_or_else(|| FerrosGrynnError::graph_does_not_exist(schema.graph.id().to_string()))?;
		if graph.kind() != schema.graph.kind() {
			return Err(FerrosGrynnError::conversion_error(
				schema.graph.kind().name(),
				graph.kind().name(),
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
		self.tables.insert(schema.name.as_str().to_owned(), Some(schema));
		Ok(())
	}

	pub fn remove_table(&mut self, name: &str) -> FerrosGrynnResult<()> {
		self.ensure_writable()?;
		self.ensure_live()?;
		if self.visible_table(name).is_none() {
			return Err(FerrosGrynnError::table_does_not_exist(name));
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

	pub fn commit(mut self) -> FerrosGrynnResult<CommitResult> {
		self.ensure_live()?;
		let estimated = self.estimated_write_set_bytes();
		if estimated > self.store.inner.options.max_write_set_bytes {
			return Err(FerrosGrynnError::transaction_write_set_limit_exceeded(
				self.id,
				estimated as u64,
				self.store.inner.options.max_write_set_bytes as u64,
			));
		}
		if self.options.access == TransactionAccess::ReadOnly
			&& (!self.graphs.is_empty() || !self.tables.is_empty())
		{
			return Err(FerrosGrynnError::read_only_transaction(self.id));
		}

		let _commit = self.store.inner.commit.lock();
		self.ensure_live()?;
		let current = self.store.current_root();
		self.validate_writes(&current)?;
		if self.options.isolation == IsolationLevel::Serializable
			&& self.options.access != TransactionAccess::ReadOnly
		{
			self.validate_serializable(&current)?;
		}
		if self.graphs.is_empty() && self.tables.is_empty() {
			return Ok(CommitResult {
				transaction: self.id,
				commit_ts: current.commit_ts,
			});
		}

		let commit_ts = current.commit_ts.checked_add(1).ok_or_else(|| {
			FerrosGrynnError::serialization_failure(self.id, "the commit clock is exhausted")
		})?;
		// ponytail: cloning the shallow catalog is O(graphs + tables); use a persistent
		// map only after catalog size appears in profiles.
		let mut next = (*current).clone();
		for (id, write) in std::mem::take(&mut self.graphs) {
			self.apply_graph_write(&mut next, &current, id, write, commit_ts)?;
		}
		for (name, schema) in std::mem::take(&mut self.tables) {
			match schema {
				Some(schema) => {
					if !next.graphs.contains_key(schema.graph.id()) {
						return Err(FerrosGrynnError::graph_does_not_exist(
							schema.graph.id().to_string(),
						));
					}
					next.tables.insert(name.clone(), Arc::new(schema));
				}
				None => {
					next.tables.remove(&name);
				}
			}
			next.table_versions.insert(name, commit_ts);
		}
		for schema in next.tables.values() {
			let stored = next.graphs.get(schema.graph.id()).ok_or_else(|| {
				FerrosGrynnError::graph_constraint_violation(
					"catalog",
					format!(
						"table '{}' references missing graph '{}'",
						schema.name,
						schema.graph.id()
					),
				)
			})?;
			if stored.graph.kind() != schema.graph.kind() {
				return Err(FerrosGrynnError::graph_constraint_violation(
					"catalog",
					format!(
						"table '{}' expects graph family '{}' but found '{}'",
						schema.name,
						schema.graph.kind().name(),
						stored.graph.kind().name()
					),
				));
			}
		}
		let mut published = self.store.inner.current.write();
		let mut active = self.store.inner.active.lock();
		active.retain(|_, snapshot| {
			snapshot.started.elapsed() <= self.store.inner.options.max_snapshot_age
		});
		let oldest_snapshot =
			active.values().map(|snapshot| snapshot.start_ts).min().unwrap_or(commit_ts);
		drop(active);
		for stored in next.graphs.values_mut() {
			if stored.changes.len() > 4_096 {
				stored.changes.retain(|(timestamp, _)| *timestamp > oldest_snapshot);
			}
		}
		next.commit_ts = commit_ts;
		*published = Arc::new(next);
		Ok(CommitResult {
			transaction: self.id,
			commit_ts,
		})
	}

	pub fn abort(self) {}

	fn ensure_writable(&self) -> FerrosGrynnResult<()> {
		if self.options.access == TransactionAccess::ReadOnly {
			return Err(FerrosGrynnError::read_only_transaction(self.id));
		}
		Ok(())
	}

	fn ensure_live(&self) -> FerrosGrynnResult<()> {
		if self.started.elapsed() > self.store.inner.options.max_snapshot_age {
			return Err(FerrosGrynnError::snapshot_expired(
				self.id,
				self.store.inner.options.max_snapshot_age.as_secs(),
			));
		}
		Ok(())
	}

	fn read_root(&self) -> Arc<DatabaseRoot> {
		if self.options.isolation == IsolationLevel::ReadCommitted {
			self.store.current_root()
		} else {
			self.root.clone()
		}
	}

	fn visible_graph(&self, id: &RecordId) -> Option<Arc<MaterializedGraph>> {
		if let Some(write) = self.graphs.get(id) {
			return write.value.as_ref().map(|value| Arc::new(value.snapshot()));
		}
		self.read_root().graphs.get(id).map(|stored| stored.graph.clone())
	}

	fn visible_table(&self, name: &str) -> Option<Arc<TableSchema>> {
		match self.tables.get(name) {
			Some(Some(schema)) => Some(Arc::new(schema.clone())),
			Some(None) => None,
			None => self.read_root().tables.get(name).cloned(),
		}
	}

	fn ensure_graph_write(&mut self, reference: &GraphRef) -> FerrosGrynnResult<()> {
		ensure_in_memory_reference(reference)?;
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
		self.graphs.insert(reference.id().clone(), GraphWrite {
			base: Some(stored.graph.clone()),
			base_ts: root.commit_ts,
			value: Some(MutableGraphValue::from_frozen(&stored.graph)?),
		});
		Ok(())
	}

	fn record_read(&self, root: &DatabaseRoot, key: ReadKey) {
		if self.options.access == TransactionAccess::ReadOnly {
			return;
		}
		let version = root.version(&key);
		self.reads.lock().entry(key).or_insert(version);
	}

	fn validate_serializable(&self, current: &DatabaseRoot) -> FerrosGrynnResult<()> {
		// ponytail: conservative SSI aborts every outgoing rw-antidependency. It is
		// serializable; retain dangerous-structure history only if measured abort
		// rates justify the machinery.
		for (key, observed) in self.reads.lock().iter() {
			if current.version(key) > *observed {
				return Err(FerrosGrynnError::serialization_failure(
					self.id,
					"a value read from the snapshot was concurrently changed",
				));
			}
		}
		for predicate in self.predicates.lock().iter() {
			self.validate_predicate(current, predicate)?;
		}
		Ok(())
	}

	fn validate_predicate(
		&self,
		current: &DatabaseRoot,
		predicate: &PredicateRead,
	) -> FerrosGrynnResult<()> {
		let schema = match self.tables.get(&predicate.table) {
			Some(Some(schema)) => schema.clone(),
			Some(None) => return Ok(()),
			None => current
				.tables
				.get(&predicate.table)
				.map(|schema| schema.as_ref().clone())
				.ok_or_else(|| FerrosGrynnError::table_does_not_exist(&predicate.table))?,
		};
		let Some(stored) = current.graphs.get(schema.graph.id()) else {
			return Ok(());
		};
		let changed = stored
			.changes
			.iter()
			.filter_map(|(timestamp, atom)| {
				(*timestamp > predicate.root.commit_ts).then_some(*atom)
			})
			.collect::<BTreeSet<_>>();
		for atom in changed {
			let before = self.predicate_row(&predicate.root, &schema, atom);
			let after = self.predicate_row(current, &schema, atom);
			let before_matches = before
				.as_ref()
				.map(|row| predicate.matcher.matches(row))
				.transpose()?
				.unwrap_or(false);
			let after_matches = after
				.as_ref()
				.map(|row| predicate.matcher.matches(row))
				.transpose()?
				.unwrap_or(false);
			if before_matches != after_matches {
				return Err(FerrosGrynnError::serialization_failure(
					self.id,
					"a row entered or left a previously evaluated predicate",
				));
			}
		}
		Ok(())
	}

	fn predicate_row(
		&self,
		root: &DatabaseRoot,
		schema: &TableSchema,
		atom: AtomId,
	) -> Option<QueryStorageRow> {
		if let Some(write) = self.graphs.get(schema.graph.id()) {
			let dirty = write
				.value
				.as_ref()
				.is_some_and(|value| value.dirty().contains(&DirtyTarget::Atom(atom)));
			if dirty {
				return write
					.value
					.as_ref()
					.and_then(|value| materialize_atom(&value.snapshot(), schema, atom));
			}
		}
		root.graphs
			.get(schema.graph.id())
			.and_then(|stored| materialize_atom(&stored.graph, schema, atom))
	}

	fn validate_writes(&self, current: &DatabaseRoot) -> FerrosGrynnResult<()> {
		for (id, write) in &self.graphs {
			match (&write.base, &write.value) {
				(None, Some(_)) => {
					if current.graphs.contains_key(id)
						|| current.graph_catalog_versions.get(id).copied().unwrap_or(0)
							> write.base_ts
					{
						return Err(self.write_conflict("a graph name was concurrently created"));
					}
				}
				(Some(_), None) => {
					let stored = current
						.graphs
						.get(id)
						.ok_or_else(|| self.write_conflict("the graph was concurrently removed"))?;
					if stored.last_commit_ts > write.base_ts {
						return Err(self.write_conflict("the graph was concurrently changed"));
					}
				}
				(Some(base), Some(value)) => {
					let stored = current
						.graphs
						.get(id)
						.ok_or_else(|| self.write_conflict("the graph was concurrently removed"))?;
					for target in value.dirty() {
						let version = match target {
							DirtyTarget::Graph => stored.properties_version,
							DirtyTarget::Atom(atom) => stored.atom_versions.get(atom),
						};
						if version > write.base_ts {
							return Err(
								self.write_conflict("the same graph atom was concurrently changed")
							);
						}
					}
					if base.kind() != stored.graph.kind() {
						return Err(
							self.write_conflict("the graph family was concurrently changed")
						);
					}
				}
				(None, None) => {}
			}
		}
		for name in self.tables.keys() {
			if current.table_versions.get(name).copied().unwrap_or(0) > self.start_ts {
				return Err(self.write_conflict("the same table schema was concurrently changed"));
			}
		}
		Ok(())
	}

	fn write_conflict(&self, reason: &'static str) -> FerrosGrynnError {
		FerrosGrynnError::serialization_failure(self.id, reason)
	}

	fn apply_graph_write(
		&self,
		next: &mut DatabaseRoot,
		current: &DatabaseRoot,
		id: RecordId,
		write: GraphWrite,
		commit_ts: u64,
	) -> FerrosGrynnResult<()> {
		let created = write.base.is_none();
		let Some(value) = write.value else {
			next.graphs.remove(&id);
			next.graph_catalog_versions.insert(id, commit_ts);
			return Ok(());
		};
		let dirty = if write.base.is_some() {
			value.dirty()
		} else {
			all_dirty(&value.snapshot())
		};
		if dirty.is_empty() && write.base.is_some() {
			return Ok(());
		}

		let graph = match (&write.base, current.graphs.get(&id)) {
			(Some(base), Some(latest)) if latest.last_commit_ts > write.base_ts => {
				merge_materialized(self.id, base, value, &latest.graph, &dirty)?
			}
			_ => value.freeze(),
		};
		let graph = Arc::new(graph);
		let mut stored = current.graphs.get(&id).cloned().unwrap_or_else(|| StoredGraph {
			graph: graph.clone(),
			last_commit_ts: 0,
			properties_version: 0,
			node_set_version: 0,
			edge_set_version: 0,
			atom_versions: AtomVersions::default(),
			changes: CowVec::new(),
		});
		for target in &dirty {
			match target {
				DirtyTarget::Graph => stored.properties_version = commit_ts,
				DirtyTarget::Atom(atom) => {
					stored.atom_versions.insert(*atom, commit_ts);
					stored.changes.push((commit_ts, *atom));
					let before = write.base.as_ref().is_some_and(|base| base.contains(*atom));
					let after = graph.contains(*atom);
					if before != after {
						match atom.kind() {
							Some(AtomKind::Node) => stored.node_set_version = commit_ts,
							Some(AtomKind::Edge) => stored.edge_set_version = commit_ts,
							None => {}
						}
					}
				}
			}
		}
		if created {
			stored.properties_version = commit_ts;
			stored.node_set_version = commit_ts;
			stored.edge_set_version = commit_ts;
		}
		stored.graph = graph;
		stored.last_commit_ts = commit_ts;
		next.graphs.insert(id.clone(), stored);
		if created {
			next.graph_catalog_versions.insert(id, commit_ts);
		}
		Ok(())
	}

	pub(crate) fn query_snapshot(&self) -> FerrosGrynnResult<QuerySnapshot> {
		self.ensure_live()?;
		Ok(QuerySnapshot {
			root: self.read_root(),
		})
	}

	pub(crate) fn query_table(
		&self,
		snapshot: &QuerySnapshot,
		name: &str,
	) -> FerrosGrynnResult<Arc<TableSchema>> {
		self.ensure_live()?;
		self.record_read(&snapshot.root, ReadKey::Table(name.to_owned()));
		match self.tables.get(name) {
			Some(Some(schema)) => Ok(Arc::new(schema.clone())),
			Some(None) => Err(FerrosGrynnError::table_does_not_exist(name)),
			None => snapshot
				.root
				.tables
				.get(name)
				.cloned()
				.ok_or_else(|| FerrosGrynnError::table_does_not_exist(name)),
		}
	}

	pub(crate) fn query_rows(
		&self,
		snapshot: &QuerySnapshot,
		table: &str,
	) -> FerrosGrynnResult<Vec<QueryStorageRow>> {
		self.ensure_live()?;
		let schema = self.query_table(snapshot, table)?;
		self.record_read(&snapshot.root, ReadKey::GraphCatalog(schema.graph.id().clone()));
		let graph = self
			.graphs
			.get(schema.graph.id())
			.map_or_else(
				|| snapshot.root.graphs.get(schema.graph.id()).map(|stored| stored.graph.clone()),
				|write| write.value.as_ref().map(|value| Arc::new(value.snapshot())),
			)
			.ok_or_else(|| FerrosGrynnError::graph_does_not_exist(schema.graph.id().to_string()))?;
		Ok(materialize_rows(&graph, &schema))
	}

	pub(crate) fn register_query_predicate(
		&self,
		snapshot: &QuerySnapshot,
		table: &str,
		rows: &[QueryStorageRow],
		evaluator: Arc<dyn PredicateEvaluator>,
	) -> FerrosGrynnResult<()> {
		if self.options.isolation != IsolationLevel::Serializable
			|| self.options.access == TransactionAccess::ReadOnly
		{
			return Ok(());
		}
		let schema = self.query_table(snapshot, table)?;
		for row in rows {
			if evaluator.matches(row)? {
				self.record_read(
					&snapshot.root,
					ReadKey::Atom(schema.graph.id().clone(), row.atom_id),
				);
			}
		}
		self.predicates.lock().push(PredicateRead {
			table: table.to_owned(),
			root: snapshot.root.clone(),
			matcher: PredicateMatcher::Query(evaluator),
		});
		Ok(())
	}

	pub(crate) fn register_query_point(
		&self,
		snapshot: &QuerySnapshot,
		table: &str,
		record: &RecordId,
		row: Option<&QueryStorageRow>,
	) -> FerrosGrynnResult<()> {
		if self.options.isolation != IsolationLevel::Serializable
			|| self.options.access == TransactionAccess::ReadOnly
		{
			return Ok(());
		}
		let schema = self.query_table(snapshot, table)?;
		if let Some(row) = row {
			self.record_read(&snapshot.root, ReadKey::Atom(schema.graph.id().clone(), row.atom_id));
		}
		self.predicates.lock().push(PredicateRead {
			table: table.to_owned(),
			root: snapshot.root.clone(),
			matcher: PredicateMatcher::Record(record.clone()),
		});
		Ok(())
	}

	fn estimated_write_set_bytes(&self) -> usize {
		// ponytail: this is a logical dirty-target budget, not recursive heap
		// accounting; enforce allocation-time bytes in tracked family guards if large
		// values become a problem.
		let graphs = self.graphs.values().map(|write| {
			write.value.as_ref().map_or(64, |value| {
				let snapshot = value.snapshot();
				let targets = if write.base.is_some() {
					value.dirty().len()
				} else {
					snapshot.node_count() + snapshot.edge_count() + 1
				};
				targets.saturating_mul(256)
			})
		});
		graphs
			.chain(
				self.tables.values().map(|schema| {
					schema.as_ref().map_or(64, |schema| 128 + schema.fields.len() * 64)
				}),
			)
			.fold(0_usize, usize::saturating_add)
	}
}

impl GraphRefReader for Transaction {
	type View<'tx>
		= TransactionGraphView<'tx>
	where
		Self: 'tx;

	fn read_graph(&self, reference: &GraphRef) -> FerrosGrynnResult<Self::View<'_>> {
		self.ensure_live()?;
		ensure_in_memory_reference(reference)?;
		let root = self.read_root();
		self.record_read(&root, ReadKey::GraphCatalog(reference.id().clone()));
		let graph =
			if let Some(write) = self.graphs.get(reference.id()) {
				write.value.as_ref().map(|value| Arc::new(value.snapshot())).ok_or_else(|| {
					FerrosGrynnError::graph_does_not_exist(reference.id().to_string())
				})?
			} else {
				root.graphs.get(reference.id()).map(|stored| stored.graph.clone()).ok_or_else(
					|| FerrosGrynnError::graph_does_not_exist(reference.id().to_string()),
				)?
			};
		if graph.kind() != reference.kind() {
			return Err(FerrosGrynnError::conversion_error(
				reference.kind().name(),
				graph.kind().name(),
			));
		}
		Ok(TransactionGraphView {
			graph,
			reference: reference.clone(),
			snapshot: SnapshotId::new(root.commit_ts, self.id),
			root,
			reads: &self.reads,
		})
	}
}

pub struct TransactionGraphView<'tx> {
	graph: Arc<MaterializedGraph>,
	reference: GraphRef,
	snapshot: SnapshotId,
	root: Arc<DatabaseRoot>,
	reads: &'tx Mutex<BTreeMap<ReadKey, u64>>,
}

impl TransactionGraphView<'_> {
	fn record(&self, key: ReadKey) {
		let version = self.root.version(&key);
		self.reads.lock().entry(key).or_insert(version);
	}
}

impl GraphRead for TransactionGraphView<'_> {
	type EdgeIds<'a>
		= Box<dyn Iterator<Item = AtomId> + 'a>
	where
		Self: 'a;
	type NodeIds<'a>
		= Box<dyn Iterator<Item = AtomId> + 'a>
	where
		Self: 'a;

	fn identity(&self) -> Uuid {
		self.graph.identity()
	}

	fn revision(&self) -> u64 {
		self.graph.revision()
	}

	fn kind(&self) -> GraphKind {
		self.graph.kind()
	}

	fn properties(&self) -> &PropertyMap {
		self.record(ReadKey::GraphProperties(self.reference.id().clone()));
		self.graph.properties()
	}

	fn node_ids(&self) -> Self::NodeIds<'_> {
		self.record(ReadKey::NodeSet(self.reference.id().clone()));
		Box::new(self.graph.node_ids())
	}

	fn edge_ids(&self) -> Self::EdgeIds<'_> {
		self.record(ReadKey::EdgeSet(self.reference.id().clone()));
		Box::new(self.graph.edge_ids())
	}

	fn node(&self, id: AtomId) -> Option<NodeView<'_>> {
		self.record(ReadKey::Atom(self.reference.id().clone(), id));
		self.graph.node(id)
	}

	fn edge(&self, id: AtomId) -> Option<EdgeView<'_>> {
		self.record(ReadKey::Atom(self.reference.id().clone(), id));
		self.graph.edge(id)
	}

	fn snapshot_id(&self) -> SnapshotId {
		self.snapshot
	}

	fn graph_ref(&self) -> Option<&GraphRef> {
		Some(&self.reference)
	}
}

fn all_dirty(graph: &MaterializedGraph) -> Vec<DirtyTarget> {
	std::iter::once(DirtyTarget::Graph)
		.chain(graph.node_ids().map(DirtyTarget::Atom))
		.chain(graph.edge_ids().map(DirtyTarget::Atom))
		.collect()
}

fn ensure_in_memory_reference(reference: &GraphRef) -> FerrosGrynnResult<()> {
	if reference.storage_mode() != GraphStorageMode::InMemory {
		return Err(FerrosGrynnError::invalid_argument(
			"the in-memory datastore accepts only in-memory graph references",
		));
	}
	Ok(())
}

fn materialize_rows(graph: &MaterializedGraph, schema: &TableSchema) -> Vec<QueryStorageRow> {
	let mut rows = Vec::new();
	if matches!(schema.kind, TableKind::Any | TableKind::Normal) {
		rows.extend(graph.node_ids().filter_map(|id| {
			graph
				.node(id)
				.filter(|node| node.record_id().table() == schema.name.as_str())
				.map(|node| row_from_properties(id, node.record_id(), node.properties(), schema))
		}));
	}
	if matches!(schema.kind, TableKind::Any | TableKind::Relation) {
		rows.extend(graph.edge_ids().filter_map(|id| {
			graph
				.edge(id)
				.filter(|edge| edge.record_id().table() == schema.name.as_str())
				.map(|edge| row_from_properties(id, edge.record_id(), edge.properties(), schema))
		}));
	}
	rows.sort_unstable_by(|left, right| left.record_id.cmp(&right.record_id));
	rows
}

fn materialize_atom(
	graph: &MaterializedGraph,
	schema: &TableSchema,
	atom: AtomId,
) -> Option<QueryStorageRow> {
	match atom.kind()? {
		AtomKind::Node if matches!(schema.kind, TableKind::Any | TableKind::Normal) => {
			let node = graph.node(atom)?;
			if node.record_id().table() != schema.name.as_str() {
				return None;
			}
			Some(row_from_properties(atom, node.record_id(), node.properties(), schema))
		}
		AtomKind::Edge if matches!(schema.kind, TableKind::Any | TableKind::Relation) => {
			let edge = graph.edge(atom)?;
			if edge.record_id().table() != schema.name.as_str() {
				return None;
			}
			Some(row_from_properties(atom, edge.record_id(), edge.properties(), schema))
		}
		AtomKind::Node | AtomKind::Edge => None,
	}
}

fn row_from_properties(
	atom_id: AtomId,
	record_id: &RecordId,
	properties: &PropertyMap,
	schema: &TableSchema,
) -> QueryStorageRow {
	let mut values = Vec::with_capacity(schema.fields.len() + 1);
	values.push(Value::Record(Box::new(record_id.clone())));
	values.extend(schema.fields.iter().map(|field| {
		field.key.and_then(|key| properties.get(key)).cloned().unwrap_or(Value::None)
	}));
	QueryStorageRow {
		record_id: record_id.clone(),
		atom_id,
		values,
	}
}

fn merge_materialized(
	transaction: u64,
	base: &MaterializedGraph,
	changed: MutableGraphValue,
	latest: &MaterializedGraph,
	dirty: &[DirtyTarget],
) -> FerrosGrynnResult<MaterializedGraph> {
	if changed.kind() != base.kind() || base.kind() != latest.kind() {
		return Err(FerrosGrynnError::serialization_failure(
			transaction,
			"the graph family changed while merging a transaction",
		));
	}
	let base = base.clone().into_general()?;
	let latest = latest.clone().into_general()?;
	match changed {
		MutableGraphValue::Graph(value) => {
			Ok(merge_transaction_graph(&base, value, latest, dirty)?.into())
		}
		MutableGraphValue::Tree(value) => {
			let merged = merge_transaction_graph(&base, value.into_graph()?, latest, dirty)?;
			Ok(Tree::<Frozen>::try_from(merged)?.into())
		}
		MutableGraphValue::RootedTree(value) => {
			let merged = merge_transaction_graph(&base, value.into_graph()?, latest, dirty)?;
			Ok(RootedTree::<Frozen>::try_from(merged)?.into())
		}
		MutableGraphValue::Forest(value) => {
			let merged = merge_transaction_graph(&base, value.into_graph()?, latest, dirty)?;
			Ok(Forest::<Frozen>::try_from(merged)?.into())
		}
		MutableGraphValue::Dag(value) => {
			let merged = merge_transaction_graph(&base, value.into_graph()?, latest, dirty)?;
			Ok(Dag::<Frozen>::try_from(merged)?.into())
		}
		MutableGraphValue::Hypergraph(value) => {
			let merged = merge_transaction_graph(&base, value.into_graph()?, latest, dirty)?;
			Ok(Hypergraph::<Frozen>::try_from(merged)?.into())
		}
		MutableGraphValue::Metagraph(value) => {
			let merged = merge_transaction_graph(&base, value.into_graph(), latest, dirty)?;
			Ok(Metagraph::<Frozen>::try_from(merged)?.into())
		}
	}
}

#[cfg(test)]
#[allow(
	clippy::expect_used,
	clippy::unwrap_used,
	reason = "tests fail immediately when compact setup or assertions return an unexpected error"
)]
mod tests {
	use super::*;
	use crate::values::runtime::{Value, graph::GraphMut};

	fn graph_id(name: &str) -> RecordId {
		format!("graph:{name}").parse().expect("test graph ID must be valid")
	}

	fn seeded_store() -> (Datastore, GraphRef, AtomId, AtomId) {
		let store = Datastore::in_memory();
		let mut graph = Graph::<Mutable>::new();
		let left = graph.add_node(None, PropertyMap::new()).unwrap();
		let right = graph.add_node(None, PropertyMap::new()).unwrap();
		let mut tx = store.begin();
		let reference = tx.create_graph(graph_id("main"), graph.freeze().into()).unwrap();
		tx.commit().unwrap();
		(store, reference, left, right)
	}

	fn seeded_table() -> (Datastore, GraphRef, AtomId) {
		let store = Datastore::in_memory();
		let mut graph = Graph::<Mutable>::new();
		let name = store.intern_property("name").unwrap();
		let age = store.intern_property("age").unwrap();
		for (id, person_name, person_age) in
			[("carol", "Carol", 17_i64), ("alice", "Alice", 30), ("bob", "Bob", 20)]
		{
			let mut properties = PropertyMap::new();
			properties.insert(name, person_name);
			properties.insert(age, person_age);
			graph
				.add_node_with_id(format!("person:{id}").parse().unwrap(), None, properties)
				.unwrap();
		}
		let mut unrelated = PropertyMap::new();
		unrelated.insert(name, "Mallory");
		unrelated.insert(age, 99_i64);
		graph.add_node_with_id("account:mallory".parse().unwrap(), None, unrelated).unwrap();
		let carol = graph
			.node_ids()
			.find(|id| graph.node(*id).unwrap().record_id().to_string() == "person:carol")
			.unwrap();
		let mut tx = store.begin();
		let reference = tx.create_graph(graph_id("people"), graph.freeze().into()).unwrap();
		tx.define_table(
			TableSchema::new("person", reference.clone(), TableKind::Normal, [
				TableField::new("name", Type::String),
				TableField::new("age", Type::Int),
			])
			.unwrap(),
		)
		.unwrap();
		tx.commit().unwrap();
		(store, reference, carol)
	}

	#[test]
	fn snapshots_use_commit_time_not_begin_time() {
		let store = Datastore::in_memory();
		let old = store.begin();
		let mut late = store.begin();
		let reference =
			late.create_graph(graph_id("late"), Graph::<Mutable>::new().freeze().into()).unwrap();
		late.commit().unwrap();

		assert!(reference.read(&old).is_err());
		assert!(reference.read(&store.begin()).is_ok());
	}

	#[test]
	fn atom_versions_keep_reused_slot_generations_distinct() {
		let old = AtomId::from_parts(AtomKind::Node, 7, 0).unwrap();
		let reused = AtomId::from_parts(AtomKind::Node, 7, 1).unwrap();
		let mut versions = AtomVersions::default();
		versions.insert(old, 3);
		versions.insert(reused, 5);
		assert_eq!(versions.get(old), 3);
		assert_eq!(versions.get(reused), 5);
	}

	#[test]
	fn read_committed_query_uses_one_statement_snapshot() {
		let (store, reference, carol) = seeded_table();
		let reader = store.begin_with(TransactionOptions {
			isolation: IsolationLevel::ReadCommitted,
			access: TransactionAccess::ReadOnly,
		});
		let statement = reader.query_snapshot().unwrap();

		let mut writer = store.begin();
		if let GraphWriteView::Graph(graph) = writer.edit_graph(&reference).unwrap() {
			graph.node_properties_mut(carol).unwrap().insert(1_u32.into(), 21_i64);
		}
		writer.commit().unwrap();

		let old_rows = reader.query_rows(&statement, "person").unwrap();
		let old_carol =
			old_rows.iter().find(|row| row.record_id.to_string() == "person:carol").unwrap();
		assert_eq!(old_carol.values[2], Value::from(17_i64));

		let fresh = reader.query_snapshot().unwrap();
		let fresh_rows = reader.query_rows(&fresh, "person").unwrap();
		let fresh_carol =
			fresh_rows.iter().find(|row| row.record_id.to_string() == "person:carol").unwrap();
		assert_eq!(fresh_carol.values[2], Value::from(21_i64));
	}

	#[test]
	fn graph_removal_preserves_table_catalog_integrity() {
		let (store, reference, _) = seeded_table();
		let mut blocked = store.begin();
		blocked.remove_graph(&reference).unwrap();
		assert!(blocked.commit().is_err());

		let mut removal = store.begin();
		removal.remove_table("person").unwrap();
		removal.remove_graph(&reference).unwrap();
		removal.commit().unwrap();
		assert!(store.table("person").is_none());
		assert!(reference.read(&store.begin()).is_err());
	}

	#[test]
	fn disjoint_atom_writes_merge() {
		let (store, reference, left, right) = seeded_store();
		let mut first = store.begin_with(TransactionOptions {
			isolation: IsolationLevel::Snapshot,
			access: TransactionAccess::ReadWrite,
		});
		let mut second = store.begin_with(first.options());
		let key = PropertyKeyId::new(0);
		if let GraphWriteView::Graph(graph) = first.edit_graph(&reference).unwrap() {
			graph.node_properties_mut(left).unwrap().insert(key, Value::from(1_i64));
		}
		if let GraphWriteView::Graph(graph) = second.edit_graph(&reference).unwrap() {
			graph.node_properties_mut(right).unwrap().insert(key, Value::from(2_i64));
		}
		first.commit().unwrap();
		second.commit().unwrap();

		let tx = store.begin();
		let view = reference.read(&tx).unwrap();
		assert_eq!(view.node(left).unwrap().properties().get(key), Some(&Value::from(1_i64)));
		assert_eq!(view.node(right).unwrap().properties().get(key), Some(&Value::from(2_i64)));
	}

	#[test]
	fn serializable_reader_aborts_after_concurrent_change() {
		let (store, reference, left, _) = seeded_store();
		let reader = store.begin();
		assert!(reference.read(&reader).unwrap().node(left).is_some());

		let mut writer = store.begin();
		if let GraphWriteView::Graph(graph) = writer.edit_graph(&reference).unwrap() {
			graph.node_properties_mut(left).unwrap().insert(0_u32.into(), 1_i64);
		}
		writer.commit().unwrap();

		let error = reader.commit().unwrap_err();
		assert_eq!(error.status().code().to_string(), "FG-40001");
	}

	#[test]
	fn read_only_snapshot_is_safe_from_serialization_aborts() {
		let (store, reference, left, _) = seeded_store();
		let reader = store.begin_with(TransactionOptions {
			isolation: IsolationLevel::Serializable,
			access: TransactionAccess::ReadOnly,
		});
		assert!(reference.read(&reader).unwrap().node(left).is_some());
		let mut writer = store.begin();
		if let GraphWriteView::Graph(graph) = writer.edit_graph(&reference).unwrap() {
			graph.node_properties_mut(left).unwrap().insert(0_u32.into(), 1_i64);
		}
		writer.commit().unwrap();
		reader.commit().unwrap();
	}

	#[test]
	fn select_uses_explicit_schema_and_record_id_order() {
		let (store, _, _) = seeded_table();
		let results = store
			.execute("SELECT VALUE name FROM person WHERE age >= 18", &crate::values::Object::new())
			.unwrap();
		let Value::Array(rows) = results[0].result.as_ref().unwrap().value() else {
			panic!("SELECT must return an array");
		};
		assert_eq!(rows.as_slice(), &[Value::from("Alice"), Value::from("Bob")]);
	}

	#[test]
	fn predicate_validation_ignores_non_matching_changes_but_catches_phantoms() {
		let (store, reference, carol) = seeded_table();
		let reader = store.begin();
		reader
			.execute("SELECT VALUE name FROM person WHERE age >= 18", &crate::values::Object::new())
			.unwrap();
		let mut writer = store.begin();
		if let GraphWriteView::Graph(graph) = writer.edit_graph(&reference).unwrap() {
			graph.node_properties_mut(carol).unwrap().insert(1_u32.into(), 17_i64);
		}
		writer.commit().unwrap();
		reader.commit().unwrap();

		let reader = store.begin();
		reader
			.execute("SELECT VALUE name FROM person WHERE age >= 18", &crate::values::Object::new())
			.unwrap();
		let mut writer = store.begin();
		if let GraphWriteView::Graph(graph) = writer.edit_graph(&reference).unwrap() {
			graph.node_properties_mut(carol).unwrap().insert(1_u32.into(), 21_i64);
		}
		writer.commit().unwrap();
		assert_eq!(reader.commit().unwrap_err().status().code().to_string(), "FG-40001");
	}

	#[test]
	fn predicate_validation_includes_own_uncommitted_writes() {
		let (store, reference, carol) = seeded_table();
		let mut transaction = store.begin();
		if let GraphWriteView::Graph(graph) = transaction.edit_graph(&reference).unwrap() {
			graph.node_properties_mut(carol).unwrap().insert(1_u32.into(), 21_i64);
		}
		transaction
			.execute("SELECT VALUE name FROM person WHERE age >= 18", &crate::values::Object::new())
			.unwrap();
		transaction.commit().unwrap();
	}

	#[test]
	fn configured_transaction_limits_have_domain_statuses() {
		let store = Datastore::in_memory_with(InMemoryOptions {
			max_snapshot_age: Duration::from_secs(300),
			max_write_set_bytes: 0,
		});
		let mut transaction = store.begin();
		transaction
			.create_graph(graph_id("limited"), Graph::<Mutable>::new().freeze().into())
			.unwrap();
		assert_eq!(transaction.commit().unwrap_err().status().code().to_string(), "FG-54F07");

		let store = Datastore::in_memory_with(InMemoryOptions {
			max_snapshot_age: Duration::ZERO,
			max_write_set_bytes: DEFAULT_MAX_WRITE_SET_BYTES,
		});
		assert_eq!(store.begin().commit().unwrap_err().status().code().to_string(), "FG-25F01");
	}
}
