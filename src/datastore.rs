use std::{
	borrow::Cow,
	collections::{BTreeMap, BTreeSet},
	num::NonZeroUsize,
	path::Path,
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
	storage::{
		LocalCommitGraph,
		LocalCommitResult,
		LocalCommitTable,
		LocalCommitTableField,
		LocalCommitTableIndex,
		LocalDatabaseSnapshot,
		LocalEdgeRead,
		LocalGraph,
		LocalGraphView,
		LocalNodeRead,
		LocalQueryAtom,
		LocalReadTransaction,
		LocalStore,
	},
	values::{
		runtime::{
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
				AtomRef,
				Dag,
				DirtyTarget,
				EdgeDirection,
				EdgeEndpoint,
				EdgeRead,
				EdgeTopology,
				EdgeView,
				Forest,
				Frozen,
				Graph,
				GraphKind,
				GraphMut,
				GraphRead,
				GraphRef,
				GraphRefReader,
				GraphStorageMode,
				Hypergraph,
				MaterializedGraph,
				Metagraph,
				Mutable,
				NodeRead,
				NodeView,
				RootedTree,
				SnapshotId,
				Tree,
				TreeOrientation,
				merge_transaction_graph,
			},
		},
		storage::validate_index_type,
	},
};

/// Default lifetime of one MVCC snapshot before cooperative cancellation.
const DEFAULT_MAX_SNAPSHOT_AGE: Duration = Duration::from_secs(300);
/// Default logical transaction write-set ceiling.
const DEFAULT_MAX_WRITE_SET_BYTES: usize = 256 * 1024 * 1024;
/// Default 64-MiB pool measured in format-v1 pages.
const DEFAULT_BUFFER_POOL_PAGES: usize = 4_096;
/// Default WAL-only page ratio that triggers a passive checkpoint.
const DEFAULT_CHECKPOINT_DIRTY_PERCENT: u8 = 25;
/// Default maximum interval between passive checkpoint attempts.
const DEFAULT_CHECKPOINT_INTERVAL: Duration = Duration::from_secs(300);
/// Reserved table name used for hidden durable nested-graph identities.
pub(crate) const INTERNAL_NESTED_GRAPH_TABLE: &str = "__fg_nested";

/// Physical I/O strategy selected for one local datastore.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum IoMode {
	/// Synchronous positional Direct I/O.
	#[default]
	Sync,
	/// Native asynchronous I/O implemented by the later async storage stage.
	NativeAsync,
}

/// Checkpoint behavior requested by an explicit maintenance call.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum CheckpointMode {
	/// Advance only to the oldest active storage snapshot.
	#[default]
	Passive,
	/// Cancel active storage snapshots and advance to the captured durable
	/// tail.
	Restart,
}

/// Automatic passive-checkpoint thresholds for a local datastore.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct CheckpointPolicy {
	/// Percentage of the buffer-pool page count allowed to remain WAL-only.
	pub unbackfilled_page_percent: u8,
	/// Maximum time between checkpoint attempts while WAL-only pages exist.
	pub interval: Duration,
}

impl Default for CheckpointPolicy {
	fn default() -> Self {
		Self {
			unbackfilled_page_percent: DEFAULT_CHECKPOINT_DIRTY_PERCENT,
			interval: DEFAULT_CHECKPOINT_INTERVAL,
		}
	}
}

/// Resource limits and I/O selection for a local datastore.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct LocalOptions {
	/// Physical I/O implementation selected at open time.
	pub io_mode: IoMode,
	/// Exact number of 16-KiB frames in the bounded page cache.
	pub buffer_pool_pages: usize,
	/// Automatic passive-checkpoint thresholds.
	pub checkpoint: CheckpointPolicy,
	/// Maximum lifetime of one transaction snapshot.
	pub max_snapshot_age: Duration,
	/// Maximum estimated bytes in one transaction-private write set.
	pub max_write_set_bytes: usize,
}

impl Default for LocalOptions {
	fn default() -> Self {
		Self {
			io_mode: IoMode::Sync,
			buffer_pool_pages: DEFAULT_BUFFER_POOL_PAGES,
			checkpoint: CheckpointPolicy::default(),
			max_snapshot_age: DEFAULT_MAX_SNAPSHOT_AGE,
			max_write_set_bytes: DEFAULT_MAX_WRITE_SET_BYTES,
		}
	}
}

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

/// Backend and resource limits owned by one datastore instance.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum DatastoreOptions {
	/// Volatile in-process storage.
	InMemory(InMemoryOptions),
	/// Exclusively owned local disk storage.
	Local(LocalOptions),
}

impl Default for DatastoreOptions {
	fn default() -> Self {
		Self::InMemory(InMemoryOptions::default())
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

impl TransactionOptions {
	pub fn new(isolation: IsolationLevel, access: TransactionAccess) -> Self {
		Self {
			isolation,
			access,
		}
	}
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum TableKind {
	Any,
	Normal,
	Relation,
}

/// Scalar B+tree uniqueness contract for one table index.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum TableIndexKind {
	/// One key may reference multiple atoms.
	Standard,
	/// One key may reference at most one atom.
	Unique,
}

/// Declarative scalar B+tree attached to one table schema.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TableIndex {
	/// Table-local index name.
	name: Text,
	/// Whether duplicate keys are permitted.
	kind: TableIndexKind,
	/// Indexed fields in composite-key order.
	fields: Vec<Text>,
}

impl TableIndex {
	/// Creates an index definition; field existence is checked by
	/// [`TableSchema::with_index`].
	///
	/// # Errors
	///
	/// Returns an invalid-argument error when the index name or field list is
	/// empty, names the synthetic `id` column, or repeats a field.
	pub fn new<IntoText, Fields>(
		name: IntoText,
		kind: TableIndexKind,
		fields: Fields,
	) -> FerrosGrynnResult<Self>
	where
		IntoText: Into<Text>,
		Fields: IntoIterator,
		Fields::Item: Into<Text>,
	{
		let name = name.into();
		let fields = fields.into_iter().map(Into::into).collect::<Vec<_>>();
		let mut unique = BTreeSet::new();
		if name.is_empty()
			|| fields.is_empty()
			|| fields.iter().any(|field| {
				field.is_empty() || field.as_str() == "id" || !unique.insert(field.clone())
			}) {
			return Err(FerrosGrynnError::invalid_argument(
				"an index requires a non-empty name and unique explicit table fields",
			));
		}
		Ok(Self {
			name,
			kind,
			fields,
		})
	}

	/// Returns the table-local index name.
	pub const fn name(&self) -> &Text {
		&self.name
	}

	/// Returns the standard or unique key contract.
	pub const fn kind(&self) -> TableIndexKind {
		self.kind
	}

	/// Returns fields in composite-key order.
	pub fn fields(&self) -> impl ExactSizeIterator<Item = &Text> {
		self.fields.iter()
	}
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
	/// Scalar indexes in declaration order.
	indexes: Vec<TableIndex>,
}

impl TableSchema {
	pub fn new<IntoText, Fields>(
		name: IntoText,
		graph: GraphRef,
		kind: TableKind,
		fields: Fields,
	) -> FerrosGrynnResult<Self>
	where
		IntoText: Into<Text>,
		Fields: IntoIterator<Item = TableField>,
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
			indexes: Vec::new(),
		})
	}

	/// Adds one validated scalar B+tree definition.
	///
	/// # Errors
	///
	/// Returns an invalid-argument error when the name is duplicated, a field
	/// is absent, or its value type has no stable format-v1 scalar order.
	pub fn with_index(mut self, index: TableIndex) -> FerrosGrynnResult<Self> {
		if self.indexes.iter().any(|existing| existing.name == index.name) {
			return Err(FerrosGrynnError::invalid_argument(format!(
				"table '{}' contains duplicate index '{}'",
				self.name, index.name
			)));
		}
		for name in &index.fields {
			let field = self.fields.iter().find(|field| field.name == *name).ok_or_else(|| {
				FerrosGrynnError::invalid_argument(format!(
					"index '{}' references unknown field '{}'",
					index.name, name
				))
			})?;
			validate_index_type(&field.value_type)?;
		}
		self.indexes.push(index);
		Ok(self)
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

	/// Returns scalar indexes in definition order.
	pub fn indexes(&self) -> impl ExactSizeIterator<Item = &TableIndex> {
		self.indexes.iter()
	}
}

#[derive(Clone)]
pub struct Datastore {
	inner: Arc<StoreInner>,
}

struct StoreInner {
	options: DatastoreOptions,
	backend: StoreBackend,
	next_transaction: AtomicU64,
	current: RwLock<Arc<DatabaseRoot>>,
	commit: Mutex<()>,
	active: Mutex<BTreeMap<u64, ActiveSnapshot>>,
}

/// Storage-specific durability owner behind the shared MVCC root.
enum StoreBackend {
	/// Volatile root with no durability owner.
	InMemory,
	/// Windows-first synchronous local durability owner.
	Local(Box<LocalStore>),
}

impl StoreBackend {
	fn local(&self) -> Option<&LocalStore> {
		match self {
			Self::InMemory => None,
			Self::Local(store) => Some(store),
		}
	}
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
	graph: StoredGraphValue,
	record_index: Arc<BTreeMap<String, AtomId>>,
	last_commit_ts: u64,
	properties_version: u64,
	node_set_version: u64,
	edge_set_version: u64,
	atom_versions: AtomVersions,
	changes: CowVec<(u64, AtomId)>,
}

#[derive(Clone)]
enum StoredGraphValue {
	Materialized(Arc<MaterializedGraph>),
	Local(Arc<LocalGraph>),
}

impl StoredGraphValue {
	fn kind(&self) -> GraphKind {
		match self {
			Self::Materialized(graph) => graph.kind(),
			Self::Local(graph) => graph.kind(),
		}
	}

	fn materialized(&self) -> Option<&Arc<MaterializedGraph>> {
		match self {
			Self::Materialized(graph) => Some(graph),
			Self::Local(_) => None,
		}
	}
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
	/// Rebuilds the in-process MVCC metadata around a recovered durable root.
	fn from_local(snapshot: LocalDatabaseSnapshot) -> FerrosGrynnResult<Self> {
		let LocalDatabaseSnapshot {
			commit_ts,
			property_dictionary,
			graphs,
			tables,
		} = snapshot;
		let dictionary = property_dictionary;
		let mut root = Self {
			commit_ts,
			graphs: BTreeMap::new(),
			graph_catalog_versions: BTreeMap::new(),
			tables: BTreeMap::new(),
			table_versions: BTreeMap::new(),
			dictionary,
		};
		for entry in graphs {
			if root.graphs.contains_key(&entry.record_id) {
				return Err(FerrosGrynnError::invalid_storage_file(
					"durable graph catalog contains duplicate RecordId values",
				));
			}
			root.graph_catalog_versions.insert(entry.record_id.clone(), commit_ts);
			root.graphs.insert(entry.record_id, StoredGraph {
				graph: StoredGraphValue::Local(entry.graph),
				record_index: Arc::new(BTreeMap::new()),
				last_commit_ts: commit_ts,
				properties_version: commit_ts,
				node_set_version: commit_ts,
				edge_set_version: commit_ts,
				atom_versions: AtomVersions::default(),
				changes: CowVec::new(),
			});
		}
		for table in tables {
			let stored = root.graphs.get(&table.graph).ok_or_else(|| {
				FerrosGrynnError::invalid_storage_file(
					"durable table references a graph absent from the recovered root",
				)
			})?;
			let durable_indexes = table.indexes;
			let fields = table
				.fields
				.into_iter()
				.map(|field| {
					let key = PropertyKeyId::new(field.property_key);
					if root.dictionary.get(&field.name) != Some(key) {
						return Err(FerrosGrynnError::invalid_storage_file(
							"durable table field disagrees with the property dictionary",
						));
					}
					Ok(TableField {
						name: field.name.into(),
						value_type: field.value_type,
						key: Some(key),
					})
				})
				.collect::<FerrosGrynnResult<Vec<_>>>()?;
			let name = table.name;
			let mut schema = TableSchema {
				name: name.clone().into(),
				graph: GraphRef::new(
					table.graph,
					stored.graph.kind(),
					GraphStorageMode::DiskBacked,
				),
				kind: table.kind,
				fields,
				indexes: Vec::new(),
			};
			for index in durable_indexes {
				let field_names = index
					.fields
					.iter()
					.map(|property_key| {
						schema
							.fields
							.iter()
							.find(|field| field.key.map(PropertyKeyId::get) == Some(*property_key))
							.map(|field| field.name.clone())
							.ok_or_else(|| {
								FerrosGrynnError::invalid_storage_file(
									"durable scalar index references an unknown table field",
								)
							})
					})
					.collect::<FerrosGrynnResult<Vec<_>>>()?;
				schema =
					schema.with_index(TableIndex::new(index.name, index.kind, field_names)?)?;
			}
			if root.tables.insert(name.clone(), Arc::new(schema)).is_some() {
				return Err(FerrosGrynnError::invalid_storage_file(
					"durable table catalog contains duplicate names",
				));
			}
			root.table_versions.insert(name, commit_ts);
		}
		Ok(root)
	}

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

impl StoreInner {
	/// Returns MVCC limits independent of the selected persistence backend.
	const fn transaction_limits(&self) -> InMemoryOptions {
		match self.options {
			DatastoreOptions::InMemory(options) => options,
			DatastoreOptions::Local(options) => InMemoryOptions {
				max_snapshot_age: options.max_snapshot_age,
				max_write_set_bytes: options.max_write_set_bytes,
			},
		}
	}

	/// Returns the graph-reference mode owned by this datastore.
	const fn graph_storage_mode(&self) -> GraphStorageMode {
		match self.backend {
			StoreBackend::InMemory => GraphStorageMode::InMemory,
			StoreBackend::Local(_) => GraphStorageMode::DiskBacked,
		}
	}

	/// Makes one already validated MVCC root durable before publication.
	fn persist_root(
		&self,
		expected_commit_ts: u64,
		root: &DatabaseRoot,
		changed_graphs: &[RecordId],
	) -> FerrosGrynnResult<Option<LocalCommitResult>> {
		let StoreBackend::Local(local) = &self.backend else {
			return Ok(None);
		};
		let graphs = changed_graphs
			.iter()
			.filter_map(|record_id| root.graphs.get(record_id).map(|stored| (record_id, stored)))
			.map(|(record_id, stored)| {
				stored
					.graph
					.materialized()
					.cloned()
					.map(|graph| LocalCommitGraph {
						record_id: record_id.clone(),
						graph,
					})
					.ok_or_else(|| {
						FerrosGrynnError::invalid_storage_file(
							"changed local graph has no materialized commit image",
						)
					})
			})
			.collect::<FerrosGrynnResult<Vec<_>>>()?;
		let tables = root
			.tables
			.values()
			.map(|schema| {
				let fields = schema
					.fields
					.iter()
					.map(|field| {
						Ok(LocalCommitTableField {
							name: field.name.as_str().to_owned(),
							value_type: field.value_type.clone(),
							property_key: field
								.key
								.ok_or_else(|| {
									FerrosGrynnError::invalid_storage_file(
										"table field has no interned property key",
									)
								})?
								.get(),
						})
					})
					.collect::<FerrosGrynnResult<Vec<_>>>()?;
				Ok(LocalCommitTable {
					name: schema.name.as_str().to_owned(),
					graph: schema.graph.id().clone(),
					kind: schema.kind,
					fields,
					indexes: schema
						.indexes
						.iter()
						.map(|index| {
							let fields = index
								.fields
								.iter()
								.map(|name| {
									schema
										.fields
										.iter()
										.find(|field| field.name == *name)
										.and_then(|field| field.key)
										.map(PropertyKeyId::get)
										.ok_or_else(|| {
											FerrosGrynnError::invalid_storage_file(
												"table index field has no interned property key",
											)
										})
								})
								.collect::<FerrosGrynnResult<Vec<_>>>()?;
							Ok(LocalCommitTableIndex {
								name: index.name.as_str().to_owned(),
								kind: index.kind,
								fields,
							})
						})
						.collect::<FerrosGrynnResult<Vec<_>>>()?,
				})
			})
			.collect::<FerrosGrynnResult<Vec<_>>>()?;
		local
			.commit(expected_commit_ts, &root.dictionary, &graphs, &tables, changed_graphs)
			.map(Some)
	}
}

#[derive(Debug)]
struct ValidatedLocalOptions {
	pool_pages: NonZeroUsize,
	checkpoint_page_threshold: usize,
}

/// Validates only options implemented by the current synchronous local slice.
fn validate_local_options(options: LocalOptions) -> FerrosGrynnResult<ValidatedLocalOptions> {
	if options.io_mode != IoMode::Sync {
		return Err(FerrosGrynnError::io_mode_unavailable(
			"NativeAsync",
			"native async I/O belongs to stage 6 and has no silent sync fallback",
		));
	}
	let pool_pages = NonZeroUsize::new(options.buffer_pool_pages).ok_or_else(|| {
		FerrosGrynnError::invalid_argument("local buffer_pool_pages must be greater than zero")
	})?;
	if !(1..=100).contains(&options.checkpoint.unbackfilled_page_percent) {
		return Err(FerrosGrynnError::invalid_argument(
			"checkpoint unbackfilled_page_percent must be in 1..=100",
		));
	}
	let checkpoint_page_threshold = options
		.buffer_pool_pages
		.checked_mul(usize::from(options.checkpoint.unbackfilled_page_percent))
		.ok_or_else(|| FerrosGrynnError::invalid_argument("checkpoint page threshold overflows"))?
		.div_ceil(100)
		.max(1);
	Ok(ValidatedLocalOptions {
		pool_pages,
		checkpoint_page_threshold,
	})
}

impl Datastore {
	pub fn in_memory() -> Self {
		Self::in_memory_with(InMemoryOptions::default())
	}

	pub fn in_memory_with(options: InMemoryOptions) -> Self {
		Self {
			inner: Arc::new(StoreInner {
				options: DatastoreOptions::InMemory(options),
				backend: StoreBackend::InMemory,
				next_transaction: AtomicU64::new(1),
				current: RwLock::new(Arc::new(DatabaseRoot::default())),
				commit: Mutex::new(()),
				active: Mutex::new(BTreeMap::new()),
			}),
		}
	}

	/// Creates a new exclusively owned local database directory.
	///
	/// # Errors
	///
	/// Returns a typed configuration, ownership, filesystem, Direct-I/O, WAL,
	/// or storage-format error. An existing directory is never overwritten.
	pub fn create_local<PathLike: AsRef<Path>>(
		path: PathLike,
		options: LocalOptions,
	) -> FerrosGrynnResult<Self> {
		let validated = validate_local_options(options)?;
		let path = path.as_ref();
		let database_name = path
			.file_name()
			.and_then(std::ffi::OsStr::to_str)
			.filter(|name| !name.is_empty())
			.ok_or_else(|| {
				FerrosGrynnError::invalid_argument(
					"local database path must end with a non-empty UTF-8 name",
				)
			})?;
		let local = LocalStore::create(
			path,
			database_name,
			validated.pool_pages,
			validated.checkpoint_page_threshold,
			options.checkpoint.interval,
		)?;
		Ok(Self {
			inner: Arc::new(StoreInner {
				options: DatastoreOptions::Local(options),
				backend: StoreBackend::Local(Box::new(local)),
				next_transaction: AtomicU64::new(1),
				current: RwLock::new(Arc::new(DatabaseRoot::default())),
				commit: Mutex::new(()),
				active: Mutex::new(BTreeMap::new()),
			}),
		})
	}

	/// Opens, recovers, and exclusively owns an existing local database.
	///
	/// # Errors
	///
	/// Returns a typed configuration, ownership, filesystem, Direct-I/O, WAL,
	/// recovery, or storage-format error. Existing files are never truncated.
	pub fn open_local<PathLike: AsRef<Path>>(
		path: PathLike,
		options: LocalOptions,
	) -> FerrosGrynnResult<Self> {
		let validated = validate_local_options(options)?;
		let (local, snapshot) = LocalStore::open(
			path,
			validated.pool_pages,
			validated.checkpoint_page_threshold,
			options.checkpoint.interval,
		)?;
		let root = DatabaseRoot::from_local(snapshot)?;
		Ok(Self {
			inner: Arc::new(StoreInner {
				options: DatastoreOptions::Local(options),
				backend: StoreBackend::Local(Box::new(local)),
				next_transaction: AtomicU64::new(1),
				current: RwLock::new(Arc::new(root)),
				commit: Mutex::new(()),
				active: Mutex::new(BTreeMap::new()),
			}),
		})
	}

	/// Returns the selected backend and its resource limits.
	pub fn options(&self) -> DatastoreOptions {
		self.inner.options
	}

	pub fn begin(&self) -> Transaction {
		self.begin_with(TransactionOptions::default())
	}

	pub fn begin_with(&self, options: TransactionOptions) -> Transaction {
		let _commit = self.inner.commit.lock();
		let started = Instant::now();
		let current = self.inner.current.read();
		let root = current.clone();
		let id = self.inner.next_transaction.fetch_add(1, Ordering::Relaxed);
		let limits = self.inner.transaction_limits();
		let (storage_tx, storage_error) = match self.inner.backend.local() {
			Some(local) => match local.begin_read(id, limits.max_snapshot_age) {
				Ok(transaction) => (Some(Arc::new(transaction)), None),
				Err(error) => (None, Some(error.to_string())),
			},
			None => (None, None),
		};
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
			storage_tx: Mutex::new(storage_tx),
			storage_error,
			reads: Mutex::new(BTreeMap::new()),
			predicates: Mutex::new(Vec::new()),
			graphs: BTreeMap::new(),
			tables: BTreeMap::new(),
		}
	}

	pub fn table(&self, name: &str) -> Option<Arc<TableSchema>> {
		self.current_root().tables.get(name).cloned()
	}

	/// Runs local checkpoint maintenance with explicit reader behavior.
	///
	/// `Restart` invalidates active disk snapshots; their next fallible access
	/// returns `FG-25F02`. In-memory datastores have no durable state and treat
	/// checkpoint as a no-op.
	///
	/// # Errors
	///
	/// Returns a WAL, Direct-I/O, atomic-superblock, or prune error.
	pub fn checkpoint(&self, mode: CheckpointMode) -> FerrosGrynnResult<()> {
		let _commit = self.inner.commit.lock();
		match self.inner.backend.local() {
			Some(local) => local.checkpoint(mode),
			None => Ok(()),
		}
	}

	/// Interns an append-only property name before it is used in graph values
	/// or table schemas.
	pub fn intern_property<IntoText: Into<Text>>(
		&self,
		name: IntoText,
	) -> FerrosGrynnResult<PropertyKeyId> {
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
		if let Some(persisted) = self.inner.persist_root(commit_ts, &next, &[])? {
			next.dictionary = Arc::new(persisted.property_dictionary);
		}
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
	storage_tx: Mutex<Option<Arc<LocalReadTransaction>>>,
	storage_error: Option<String>,
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

#[derive(Debug, Clone, Copy)]
pub(crate) enum ScalarComparison {
	/// Exact equality.
	Equal,
	/// Strictly less than.
	Less,
	/// Less than or equal.
	LessOrEqual,
	/// Strictly greater than.
	Greater,
	/// Greater than or equal.
	GreaterOrEqual,
}

/// One scalar predicate that may be satisfied by a durable table index.
#[derive(Debug, Clone)]
pub(crate) struct ScalarIndexConstraint {
	/// Query row column; zero is the synthetic record id.
	pub(crate) column: usize,
	/// Ordered comparison to apply.
	pub(crate) comparison: ScalarComparison,
	/// Runtime scalar encoded into an order-preserving index key.
	pub(crate) value: Value,
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
	/// Rebuilds durable secondary indexes even when graph pages are unchanged.
	force_persist: bool,
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
/// Atom identity crosses the API boundary only as [`AtomRef`]. The view may
/// materialize a disk graph when the first mutation starts, but subsequent
/// operations use the same contract for local and in-memory datastores.
pub struct GraphMutationView<'a> {
	value: &'a mut MutableGraphValue,
}

impl GraphMutationView<'_> {
	/// Returns the structural family of the graph being changed.
	pub const fn kind(&self) -> GraphKind {
		self.value.kind()
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
		self.value.create_node(record_id, label, properties)
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
		self.value.create_edge(record_id, topology, label, properties)
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
		self.value.set_atom_label(atom, label)
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
		Ok(self.value.atom_properties_mut(atom)?.insert(key, value))
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
		Ok(self.value.atom_properties_mut(atom)?.remove(key))
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
		self.value.replace_incidence(edge, topology)
	}

	/// Removes an atom. Removing a node also removes dependent edges.
	///
	/// # Errors
	///
	/// Returns an error when the atom is absent or removal violates a family
	/// invariant.
	pub fn remove_atom(&mut self, atom: &AtomRef) -> FerrosGrynnResult<()> {
		self.value.remove_atom(atom)
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
		if id.table() == INTERNAL_NESTED_GRAPH_TABLE {
			return Err(FerrosGrynnError::invalid_argument(format!(
				"RecordId table '{INTERNAL_NESTED_GRAPH_TABLE}' is reserved for nested graphs"
			)));
		}
		if self.visible_graph_kind(&id).is_some() {
			return Err(FerrosGrynnError::graph_already_exists(id.to_string()));
		}
		let kind = graph.kind();
		let value = MutableGraphValue::from_frozen(&graph)?;
		self.graphs.insert(id.clone(), GraphWrite {
			base: None,
			base_ts: self.read_root().commit_ts,
			value: Some(value),
			force_persist: false,
		});
		Ok(GraphRef::new(id, kind, self.store.inner.graph_storage_mode()))
	}

	pub fn remove_graph(&mut self, reference: &GraphRef) -> FerrosGrynnResult<()> {
		self.ensure_writable()?;
		self.ensure_live()?;
		ensure_store_reference(reference, self.store.inner.graph_storage_mode())?;
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
		self.ensure_graph_write(reference)?;
		self.graphs
			.get_mut(reference.id())
			.and_then(|write| write.value.as_mut())
			.map(|value| GraphMutationView {
				value,
			})
			.ok_or_else(|| FerrosGrynnError::graph_does_not_exist(reference.id().to_string()))
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

	pub fn commit(mut self) -> FerrosGrynnResult<CommitResult> {
		self.ensure_live()?;
		let limits = self.store.inner.transaction_limits();
		let estimated = self.estimated_write_set_bytes();
		if estimated > limits.max_write_set_bytes {
			return Err(FerrosGrynnError::transaction_write_set_limit_exceeded(
				self.id,
				estimated as u64,
				limits.max_write_set_bytes as u64,
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
		let mut changed_graphs = next
			.graphs
			.iter()
			.filter(|(id, after)| {
				current
					.graphs
					.get(*id)
					.is_none_or(|before| before.last_commit_ts != after.last_commit_ts)
			})
			.map(|(id, _stored)| id.clone())
			.collect::<Vec<_>>();
		changed_graphs
			.extend(current.graphs.keys().filter(|id| !next.graphs.contains_key(*id)).cloned());
		next.commit_ts = commit_ts;
		if let Some(persisted) = self.store.inner.persist_root(commit_ts, &next, &changed_graphs)? {
			next.dictionary = Arc::new(persisted.property_dictionary);
		}
		let mut published = self.store.inner.current.write();
		let mut active = self.store.inner.active.lock();
		active.retain(|_, snapshot| snapshot.started.elapsed() <= limits.max_snapshot_age);
		let oldest_snapshot =
			active.values().map(|snapshot| snapshot.start_ts).min().unwrap_or(commit_ts);
		drop(active);
		for stored in next.graphs.values_mut() {
			if stored.changes.len() > 4_096 {
				stored.changes.retain(|(timestamp, _)| *timestamp > oldest_snapshot);
			}
		}
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
		if let Some(error) = &self.storage_error {
			return Err(FerrosGrynnError::invalid_storage_file(format!(
				"failed to register the local storage snapshot: {error}"
			)));
		}
		let limits = self.store.inner.transaction_limits();
		if self.started.elapsed() > limits.max_snapshot_age {
			let transaction = self.storage_tx.lock().take();
			if let Some(transaction) = transaction {
				transaction.cancel();
			}
			return Err(FerrosGrynnError::snapshot_expired(
				self.id,
				limits.max_snapshot_age.as_secs(),
			));
		}
		let transaction = self.storage_tx.lock().as_ref().cloned();
		if let Some(transaction) = transaction {
			transaction.ensure_live()?;
		}
		Ok(())
	}

	fn local_snapshot(&self) -> FerrosGrynnResult<Arc<LocalReadTransaction>> {
		self.ensure_live()?;
		self.storage_tx.lock().as_ref().cloned().ok_or_else(|| {
			FerrosGrynnError::invalid_storage_file("local transaction has no storage snapshot")
		})
	}

	fn read_root(&self) -> Arc<DatabaseRoot> {
		if self.options.isolation == IsolationLevel::ReadCommitted {
			self.store.current_root()
		} else {
			self.root.clone()
		}
	}

	fn visible_graph_kind(&self, id: &RecordId) -> Option<GraphKind> {
		if let Some(write) = self.graphs.get(id) {
			return write.value.as_ref().map(MutableGraphValue::kind);
		}
		self.read_root().graphs.get(id).map(|stored| stored.graph.kind())
	}

	fn visible_table(&self, name: &str) -> Option<Arc<TableSchema>> {
		match self.tables.get(name) {
			Some(Some(schema)) => Some(Arc::new(schema.clone())),
			Some(None) => None,
			None => self.read_root().tables.get(name).cloned(),
		}
	}

	fn ensure_graph_write(&mut self, reference: &GraphRef) -> FerrosGrynnResult<()> {
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
			base: Some(base.clone()),
			base_ts: root.commit_ts,
			value: Some(MutableGraphValue::from_frozen(&base)?),
			force_persist: false,
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
			let before = self.predicate_row(&predicate.root, &schema, atom)?;
			let after = self.predicate_row(current, &schema, atom)?;
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
	) -> FerrosGrynnResult<Option<QueryStorageRow>> {
		if let Some(write) = self.graphs.get(schema.graph.id()) {
			let dirty = write
				.value
				.as_ref()
				.is_some_and(|value| value.dirty().contains(&DirtyTarget::Atom(atom)));
			if dirty {
				return Ok(write
					.value
					.as_ref()
					.and_then(|value| materialize_atom(&value.snapshot(), schema, atom)));
			}
		}
		let Some(stored) = root.graphs.get(schema.graph.id()) else {
			return Ok(None);
		};
		match &stored.graph {
			StoredGraphValue::Materialized(graph) => Ok(materialize_atom(graph, schema, atom)),
			StoredGraphValue::Local(graph) => {
				let local = self.store.inner.backend.local().ok_or_else(|| {
					FerrosGrynnError::invalid_storage_file(
						"local graph is attached to an in-memory backend",
					)
				})?;
				let transaction = self.local_snapshot()?;
				let view = local.graph_view(transaction, graph.clone());
				let row = match atom.kind() {
					Some(AtomKind::Node)
						if matches!(schema.kind, TableKind::Any | TableKind::Normal) =>
					{
						view.node(atom)?
							.map(|node| {
								let record = node.record_id()?.into_owned();
								let properties = node.properties()?.into_owned();
								Ok::<_, FerrosGrynnError>((record, properties))
							})
							.transpose()?
					}
					Some(AtomKind::Edge)
						if matches!(schema.kind, TableKind::Any | TableKind::Relation) =>
					{
						view.edge(atom)?
							.map(|edge| {
								let record = edge.record_id()?.into_owned();
								let properties = edge.properties()?.into_owned();
								Ok::<_, FerrosGrynnError>((record, properties))
							})
							.transpose()?
					}
					_ => None,
				};
				Ok(row.and_then(|(record, properties)| {
					(record.table() == schema.name.as_str())
						.then(|| row_from_properties(atom, &record, &properties, schema))
				}))
			}
		}
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
			all_dirty(&value.snapshot())?
		};
		if dirty.is_empty() && write.base.is_some() && !write.force_persist {
			return Ok(());
		}

		let graph = match (&write.base, current.graphs.get(&id)) {
			(Some(base), Some(latest)) if latest.last_commit_ts > write.base_ts => {
				let latest = latest.graph.materialized().ok_or_else(|| {
					FerrosGrynnError::invalid_storage_file(
						"newer graph revision has no materialized merge image",
					)
				})?;
				merge_materialized(self.id, base, value, latest, &dirty)?
			}
			_ => value.freeze(),
		};
		let graph = Arc::new(graph);
		let mut stored = current.graphs.get(&id).cloned().unwrap_or_else(|| StoredGraph {
			graph: StoredGraphValue::Materialized(graph.clone()),
			record_index: Arc::new(BTreeMap::new()),
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
		stored.graph = StoredGraphValue::Materialized(graph.clone());
		stored.record_index = Arc::new(build_record_index(&graph)?);
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
		constraints: &[ScalarIndexConstraint],
	) -> FerrosGrynnResult<Vec<QueryStorageRow>> {
		self.ensure_live()?;
		let schema = self.query_table(snapshot, table)?;
		self.record_read(&snapshot.root, ReadKey::GraphCatalog(schema.graph.id().clone()));
		if let Some(write) = self.graphs.get(schema.graph.id()) {
			let graph = write.value.as_ref().map(MutableGraphValue::snapshot).ok_or_else(|| {
				FerrosGrynnError::graph_does_not_exist(schema.graph.id().to_string())
			})?;
			return materialize_rows(&graph, &schema);
		}
		let stored =
			snapshot.root.graphs.get(schema.graph.id()).ok_or_else(|| {
				FerrosGrynnError::graph_does_not_exist(schema.graph.id().to_string())
			})?;
		match &stored.graph {
			StoredGraphValue::Materialized(graph) => materialize_rows(graph, &schema),
			StoredGraphValue::Local(graph) => {
				let local = self.store.inner.backend.local().ok_or_else(|| {
					FerrosGrynnError::invalid_storage_file(
						"local graph is attached to an in-memory backend",
					)
				})?;
				let transaction = self.local_snapshot()?;
				let mut rows = Vec::new();
				let collect = |atom: LocalQueryAtom, rows: &mut Vec<QueryStorageRow>| {
					rows.push(row_from_properties(
						atom.atom_id,
						&atom.record_id,
						&atom.properties,
						&schema,
					));
					Ok(())
				};
				let equalities = constraints
					.iter()
					.filter(|constraint| matches!(constraint.comparison, ScalarComparison::Equal))
					.filter_map(|constraint| {
						constraint
							.column
							.checked_sub(1)
							.and_then(|column| schema.fields.get(column))
							.and_then(|field| field.key)
							.map(|key| (key, &constraint.value))
					})
					.collect::<Vec<_>>();
				let mut indexed = local.visit_table_composite_index(
					&transaction,
					graph,
					schema.name.as_str(),
					schema.kind,
					&equalities,
					|atom| collect(atom, &mut rows),
				)?;
				for constraint in constraints {
					if indexed {
						break;
					}
					let field = constraint
						.column
						.checked_sub(1)
						.and_then(|column| schema.fields.get(column))
						.and_then(|field| field.key);
					if let Some(property_key) = field {
						indexed = local.visit_table_index(
							&transaction,
							graph,
							schema.name.as_str(),
							schema.kind,
							property_key,
							constraint.comparison,
							&constraint.value,
							|atom| collect(atom, &mut rows),
						)?;
					}
				}
				if !indexed {
					local.visit_table(
						&transaction,
						graph,
						schema.name.as_str(),
						schema.kind,
						|atom| collect(atom, &mut rows),
					)?;
				}
				rows.sort_unstable_by(|left, right| left.record_id.cmp(&right.record_id));
				Ok(rows)
			}
		}
	}

	/// Resolves one record without scanning the table when the graph comes from
	/// a published snapshot. Transaction-private graph writes use the compact
	/// materialized fallback because they have not built a durable index yet.
	pub(crate) fn query_point(
		&self,
		snapshot: &QuerySnapshot,
		record: &RecordId,
	) -> FerrosGrynnResult<Option<QueryStorageRow>> {
		self.ensure_live()?;
		let schema = self.query_table(snapshot, record.table())?;
		self.record_read(&snapshot.root, ReadKey::GraphCatalog(schema.graph.id().clone()));
		if let Some(write) = self.graphs.get(schema.graph.id()) {
			let Some(graph) = write.value.as_ref().map(MutableGraphValue::snapshot) else {
				return Ok(None);
			};
			let atom =
				if let Some(atom) = graph.atom_id(&AtomRef::new(AtomKind::Node, record.clone()))? {
					Some(atom)
				} else {
					graph.atom_id(&AtomRef::new(AtomKind::Edge, record.clone()))?
				};
			return Ok(atom.and_then(|atom| materialize_atom(&graph, &schema, atom)));
		}
		let stored =
			snapshot.root.graphs.get(schema.graph.id()).ok_or_else(|| {
				FerrosGrynnError::graph_does_not_exist(schema.graph.id().to_string())
			})?;
		match &stored.graph {
			StoredGraphValue::Materialized(graph) => Ok(stored
				.record_index
				.get(&record.to_string())
				.copied()
				.and_then(|atom| materialize_atom(graph, &schema, atom))),
			StoredGraphValue::Local(graph) => {
				let local = self.store.inner.backend.local().ok_or_else(|| {
					FerrosGrynnError::invalid_storage_file(
						"local graph is attached to an in-memory backend",
					)
				})?;
				let transaction = self.local_snapshot()?;
				Ok(local.lookup_record(&transaction, graph, record, schema.kind)?.map(|atom| {
					row_from_properties(atom.atom_id, &atom.record_id, &atom.properties, &schema)
				}))
			}
		}
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
		ensure_store_reference(reference, self.store.inner.graph_storage_mode())?;
		let root = self.read_root();
		self.record_read(&root, ReadKey::GraphCatalog(reference.id().clone()));
		let graph = if let Some(write) = self.graphs.get(reference.id()) {
			let graph =
				write.value.as_ref().map(|value| Arc::new(value.snapshot())).ok_or_else(|| {
					FerrosGrynnError::graph_does_not_exist(reference.id().to_string())
				})?;
			TransactionGraphSource::Materialized(graph)
		} else {
			let stored = root.graphs.get(reference.id()).ok_or_else(|| {
				FerrosGrynnError::graph_does_not_exist(reference.id().to_string())
			})?;
			match &stored.graph {
				StoredGraphValue::Materialized(graph) => {
					TransactionGraphSource::Materialized(graph.clone())
				}
				StoredGraphValue::Local(graph) => {
					let local = self.store.inner.backend.local().ok_or_else(|| {
						FerrosGrynnError::invalid_storage_file(
							"local graph is attached to an in-memory backend",
						)
					})?;
					TransactionGraphSource::Local(
						local.graph_view(self.local_snapshot()?, graph.clone()),
					)
				}
			}
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

enum TransactionGraphSource<'tx> {
	Materialized(Arc<MaterializedGraph>),
	Local(LocalGraphView<'tx>),
}

impl TransactionGraphSource<'_> {
	fn kind(&self) -> GraphKind {
		match self {
			Self::Materialized(graph) => graph.kind(),
			Self::Local(graph) => graph.kind(),
		}
	}
}

/// Snapshot-bound graph view that preserves the selected storage backend.
pub struct TransactionGraphView<'tx> {
	graph: TransactionGraphSource<'tx>,
	reference: GraphRef,
	snapshot: SnapshotId,
	root: Arc<DatabaseRoot>,
	reads: &'tx Mutex<BTreeMap<ReadKey, u64>>,
}

/// Backend-neutral node read returned by a transaction graph view.
#[non_exhaustive]
#[expect(
	clippy::large_enum_variant,
	reason = "boxing a page-guard-backed node would allocate on every point read"
)]
pub enum TransactionNodeRead<'tx> {
	/// Borrowed projection from an in-memory graph.
	Memory(NodeView<'tx>),
	/// Page-guard-backed projection from a local graph.
	Local(LocalNodeRead<'tx>),
}

impl NodeRead for TransactionNodeRead<'_> {
	fn id(&self) -> AtomId {
		match self {
			Self::Memory(node) => node.id(),
			Self::Local(node) => node.id(),
		}
	}

	fn record_id(&self) -> FerrosGrynnResult<Cow<'_, RecordId>> {
		match self {
			Self::Memory(node) => NodeRead::record_id(node),
			Self::Local(node) => node.record_id(),
		}
	}

	fn label_text(&self) -> FerrosGrynnResult<Option<Cow<'_, Text>>> {
		match self {
			Self::Memory(node) => NodeRead::label_text(node),
			Self::Local(node) => node.label_text(),
		}
	}

	fn properties(&self) -> FerrosGrynnResult<Cow<'_, PropertyMap>> {
		match self {
			Self::Memory(node) => NodeRead::properties(node),
			Self::Local(node) => node.properties(),
		}
	}

	fn inner_graph_ref(&self) -> FerrosGrynnResult<Option<Cow<'_, GraphRef>>> {
		match self {
			Self::Memory(node) => NodeRead::inner_graph_ref(node),
			Self::Local(node) => node.inner_graph_ref(),
		}
	}

	fn inner_graph(&self) -> FerrosGrynnResult<Option<Cow<'_, Graph<Frozen>>>> {
		match self {
			Self::Memory(node) => NodeRead::inner_graph(node),
			Self::Local(node) => node.inner_graph(),
		}
	}
}

/// Backend-neutral edge read returned by a transaction graph view.
#[non_exhaustive]
#[expect(
	clippy::large_enum_variant,
	reason = "boxing a page-guard-backed edge would allocate on every point read"
)]
pub enum TransactionEdgeRead<'tx> {
	/// Borrowed projection from an in-memory graph.
	Memory(EdgeView<'tx>),
	/// Page-guard-backed projection from a local graph.
	Local(LocalEdgeRead<'tx>),
}

impl EdgeRead for TransactionEdgeRead<'_> {
	fn id(&self) -> AtomId {
		match self {
			Self::Memory(edge) => edge.id(),
			Self::Local(edge) => edge.id(),
		}
	}

	fn record_id(&self) -> FerrosGrynnResult<Cow<'_, RecordId>> {
		match self {
			Self::Memory(edge) => EdgeRead::record_id(edge),
			Self::Local(edge) => edge.record_id(),
		}
	}

	fn topology(&self) -> FerrosGrynnResult<Cow<'_, crate::values::runtime::graph::EdgeTopology>> {
		match self {
			Self::Memory(edge) => EdgeRead::topology(edge),
			Self::Local(edge) => edge.topology(),
		}
	}

	fn label_text(&self) -> FerrosGrynnResult<Option<Cow<'_, Text>>> {
		match self {
			Self::Memory(edge) => EdgeRead::label_text(edge),
			Self::Local(edge) => edge.label_text(),
		}
	}

	fn properties(&self) -> FerrosGrynnResult<Cow<'_, PropertyMap>> {
		match self {
			Self::Memory(edge) => EdgeRead::properties(edge),
			Self::Local(edge) => edge.properties(),
		}
	}

	fn weight(&self) -> Option<crate::values::runtime::Number> {
		match self {
			Self::Memory(edge) => edge.weight(),
			Self::Local(edge) => edge.weight(),
		}
	}

	fn transition_graph_ref(&self) -> FerrosGrynnResult<Option<Cow<'_, GraphRef>>> {
		match self {
			Self::Memory(edge) => EdgeRead::transition_graph_ref(edge),
			Self::Local(edge) => edge.transition_graph_ref(),
		}
	}

	fn transition_graph(&self) -> FerrosGrynnResult<Option<Cow<'_, Graph<Frozen>>>> {
		match self {
			Self::Memory(edge) => EdgeRead::transition_graph(edge),
			Self::Local(edge) => edge.transition_graph(),
		}
	}
}

impl TransactionGraphView<'_> {
	fn record(&self, key: ReadKey) {
		let version = self.root.version(&key);
		self.reads.lock().entry(key).or_insert(version);
	}
}

impl GraphRead for TransactionGraphView<'_> {
	type Edge<'a>
		= TransactionEdgeRead<'a>
	where
		Self: 'a;
	type EdgeIds<'a>
		= Box<dyn Iterator<Item = FerrosGrynnResult<AtomId>> + 'a>
	where
		Self: 'a;
	type Node<'a>
		= TransactionNodeRead<'a>
	where
		Self: 'a;
	type NodeIds<'a>
		= Box<dyn Iterator<Item = FerrosGrynnResult<AtomId>> + 'a>
	where
		Self: 'a;

	fn identity(&self) -> Uuid {
		match &self.graph {
			TransactionGraphSource::Materialized(graph) => graph.identity(),
			TransactionGraphSource::Local(graph) => graph.identity(),
		}
	}

	fn revision(&self) -> u64 {
		match &self.graph {
			TransactionGraphSource::Materialized(graph) => graph.revision(),
			TransactionGraphSource::Local(graph) => graph.revision(),
		}
	}

	fn kind(&self) -> GraphKind {
		self.graph.kind()
	}

	fn properties(&self) -> FerrosGrynnResult<Cow<'_, PropertyMap>> {
		self.record(ReadKey::GraphProperties(self.reference.id().clone()));
		match &self.graph {
			TransactionGraphSource::Materialized(graph) => Ok(Cow::Borrowed(graph.properties())),
			TransactionGraphSource::Local(graph) => graph.properties(),
		}
	}

	fn node_ids(&self) -> FerrosGrynnResult<Self::NodeIds<'_>> {
		self.record(ReadKey::NodeSet(self.reference.id().clone()));
		match &self.graph {
			TransactionGraphSource::Materialized(graph) => {
				Ok(Box::new(GraphRead::node_ids(graph.as_ref())?))
			}
			TransactionGraphSource::Local(graph) => Ok(Box::new(graph.node_ids()?)),
		}
	}

	fn edge_ids(&self) -> FerrosGrynnResult<Self::EdgeIds<'_>> {
		self.record(ReadKey::EdgeSet(self.reference.id().clone()));
		match &self.graph {
			TransactionGraphSource::Materialized(graph) => {
				Ok(Box::new(GraphRead::edge_ids(graph.as_ref())?))
			}
			TransactionGraphSource::Local(graph) => Ok(Box::new(graph.edge_ids()?)),
		}
	}

	fn node(&self, id: AtomId) -> FerrosGrynnResult<Option<Self::Node<'_>>> {
		self.record(ReadKey::Atom(self.reference.id().clone(), id));
		match &self.graph {
			TransactionGraphSource::Materialized(graph) => {
				Ok(graph.node(id).map(TransactionNodeRead::Memory))
			}
			TransactionGraphSource::Local(graph) => {
				Ok(graph.node(id)?.map(TransactionNodeRead::Local))
			}
		}
	}

	fn edge(&self, id: AtomId) -> FerrosGrynnResult<Option<Self::Edge<'_>>> {
		self.record(ReadKey::Atom(self.reference.id().clone(), id));
		match &self.graph {
			TransactionGraphSource::Materialized(graph) => {
				Ok(graph.edge(id).map(TransactionEdgeRead::Memory))
			}
			TransactionGraphSource::Local(graph) => {
				Ok(graph.edge(id)?.map(TransactionEdgeRead::Local))
			}
		}
	}

	fn snapshot_id(&self) -> SnapshotId {
		self.snapshot
	}

	fn graph_ref(&self) -> Option<&GraphRef> {
		Some(&self.reference)
	}
}

fn all_dirty(graph: &MaterializedGraph) -> FerrosGrynnResult<Vec<DirtyTarget>> {
	let mut dirty = Vec::with_capacity(graph.node_count() + graph.edge_count() + 1);
	dirty.push(DirtyTarget::Graph);
	for atom in GraphRead::node_ids(graph)? {
		dirty.push(DirtyTarget::Atom(atom?));
	}
	for atom in GraphRead::edge_ids(graph)? {
		dirty.push(DirtyTarget::Atom(atom?));
	}
	Ok(dirty)
}

/// Builds the runtime mirror of the durable primary RecordId index.
fn build_record_index(graph: &MaterializedGraph) -> FerrosGrynnResult<BTreeMap<String, AtomId>> {
	let mut index = BTreeMap::new();
	for atom in GraphRead::node_ids(graph)?.chain(GraphRead::edge_ids(graph)?) {
		let atom = atom?;
		let reference = graph.atom_ref(atom)?.ok_or_else(|| {
			FerrosGrynnError::invalid_storage_file("live graph atom has no RecordId")
		})?;
		if index.insert(reference.id().to_string(), atom).is_some() {
			return Err(FerrosGrynnError::graph_constraint_violation(
				"record id",
				format!("duplicate RecordId '{}' in one graph", reference.id()),
			));
		}
	}
	Ok(index)
}

fn ensure_store_reference(
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

fn materialize_rows(
	graph: &MaterializedGraph,
	schema: &TableSchema,
) -> FerrosGrynnResult<Vec<QueryStorageRow>> {
	let mut rows = Vec::new();
	if matches!(schema.kind, TableKind::Any | TableKind::Normal) {
		for id in GraphRead::node_ids(graph)? {
			let id = id?;
			if let Some(node) = graph.node(id)
				&& node.record_id().table() == schema.name.as_str()
			{
				rows.push(row_from_properties(id, node.record_id(), node.properties(), schema));
			}
		}
	}
	if matches!(schema.kind, TableKind::Any | TableKind::Relation) {
		for id in GraphRead::edge_ids(graph)? {
			let id = id?;
			if let Some(edge) = graph.edge(id)
				&& edge.record_id().table() == schema.name.as_str()
			{
				rows.push(row_from_properties(id, edge.record_id(), edge.properties(), schema));
			}
		}
	}
	rows.sort_unstable_by(|left, right| left.record_id.cmp(&right.record_id));
	Ok(rows)
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
	use std::ops::Bound;

	use rust_decimal::Decimal;

	use super::*;
	use crate::values::runtime::{
		Array,
		Bytes,
		DateTime,
		Duration as ValueDuration,
		Geometry,
		Object,
		Position,
		Range,
		Set,
		Ulid,
		Value,
		Vector,
		graph::{EdgeTopology, GraphMut, QualifiedAtomId, query_to_memory},
	};

	/// Unique database directory removed after the Windows integration test.
	#[cfg(windows)]
	struct LocalTestDirectory(std::path::PathBuf);

	#[cfg(windows)]
	impl LocalTestDirectory {
		/// Reserves a unique path without creating it before `create_local`.
		fn new() -> Self {
			Self(
				std::env::temp_dir()
					.join(format!("fgdb-{:016x}", Uuid::now_v7().as_inner().as_u128() as u64)),
			)
		}

		/// Returns the exact database path.
		fn path(&self) -> &Path {
			&self.0
		}
	}

	#[cfg(windows)]
	impl Drop for LocalTestDirectory {
		fn drop(&mut self) {
			drop(std::fs::remove_dir_all(&self.0));
		}
	}

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
			.unwrap()
			.find_map(|id| {
				let id = id.unwrap();
				(graph.node(id).unwrap().record_id().to_string() == "person:carol").then_some(id)
			})
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

	/// The first public local slice survives close/recovery and remains visible
	/// through both the Rust graph API and the read-only query adapter.
	#[cfg(windows)]
	#[test]
	fn local_graph_table_and_edge_survive_reopen() {
		let directory = LocalTestDirectory::new();
		let options = LocalOptions {
			buffer_pool_pages: 16,
			..LocalOptions::default()
		};
		let store = Datastore::create_local(directory.path(), options).unwrap();
		assert_eq!(store.options(), DatastoreOptions::Local(options));
		let name = store.intern_property("name").unwrap();
		let age = store.intern_property("age").unwrap();

		let mut graph = Graph::<Mutable>::new();
		let mut alice_properties = PropertyMap::new();
		alice_properties.insert(name, "Alice");
		alice_properties.insert(age, 30_i64);
		let alice = graph
			.add_node_with_id(
				"person:alice".parse().unwrap(),
				Some("Person".into()),
				alice_properties,
			)
			.unwrap();
		let mut bob_properties = PropertyMap::new();
		bob_properties.insert(name, "Bob");
		bob_properties.insert(age, 20_i64);
		let bob = graph
			.add_node_with_id("person:bob".parse().unwrap(), Some("Person".into()), bob_properties)
			.unwrap();
		graph
			.add_edge_with_id(
				"knows:alice-bob".parse().unwrap(),
				EdgeTopology::directed([alice], [bob]),
				Some("KNOWS".into()),
				PropertyMap::new(),
			)
			.unwrap();

		let mut transaction = store.begin();
		let reference =
			transaction.create_graph(graph_id("people"), graph.freeze().into()).unwrap();
		assert_eq!(reference.storage_mode(), GraphStorageMode::DiskBacked);
		transaction
			.define_table(
				TableSchema::new("person", reference, TableKind::Normal, [
					TableField::new("name", Type::String),
					TableField::new("age", Type::Int),
				])
				.unwrap()
				.with_index(
					TableIndex::new("person_age", TableIndexKind::Standard, ["age"]).unwrap(),
				)
				.unwrap()
				.with_index(
					TableIndex::new("person_name_age", TableIndexKind::Unique, ["name", "age"])
						.unwrap(),
				)
				.unwrap(),
			)
			.unwrap();
		transaction.commit().unwrap();

		let busy = match Datastore::open_local(directory.path(), options) {
			Ok(_unexpected) => panic!("a live local owner must keep db.lock exclusive"),
			Err(error) => error,
		};
		assert_eq!(busy.status().code().to_string(), "FG-51I03");
		drop(store);

		let reopened = Datastore::open_local(directory.path(), options).unwrap();
		let schema = reopened.table("person").unwrap();
		let transaction = reopened.begin();
		let view = schema.graph().read(&transaction).unwrap();
		assert_eq!(view.node_count().unwrap(), 2);
		assert_eq!(view.edge_count().unwrap(), 1);
		assert_eq!(
			view.node(alice).unwrap().unwrap().properties().unwrap().get(name),
			Some(&Value::from("Alice"))
		);
		drop(transaction);

		let results = reopened
			.execute("SELECT VALUE name FROM person WHERE age >= 18", &crate::values::Object::new())
			.unwrap();
		let Value::Array(rows) = results[0].result.as_ref().unwrap().value() else {
			panic!("SELECT must return an array");
		};
		assert_eq!(rows.as_slice(), &[Value::from("Alice"), Value::from("Bob")]);
		let composite = reopened
			.execute(
				"SELECT VALUE id FROM person WHERE name = 'Alice' AND age = 30",
				&crate::values::Object::new(),
			)
			.unwrap();
		let Value::Array(rows) = composite[0].result.as_ref().unwrap().value() else {
			panic!("composite-index SELECT must return an array");
		};
		assert_eq!(rows.as_slice(), &[Value::from("person:alice".parse::<RecordId>().unwrap())]);
		let point = reopened
			.execute("SELECT VALUE name FROM person:alice", &crate::values::Object::new())
			.unwrap();
		let Value::Array(rows) = point[0].result.as_ref().unwrap().value() else {
			panic!("point SELECT must return an array");
		};
		assert_eq!(rows.as_slice(), &[Value::from("Alice")]);

		let mut update = reopened.begin();
		if let GraphWriteView::Graph(graph) = update.edit_graph(schema.graph()).unwrap() {
			graph.node_properties_mut(alice).unwrap().insert(age, 31_i64);
		}
		update.commit().unwrap();
		drop(schema);
		drop(reopened);

		let reopened = Datastore::open_local(directory.path(), options).unwrap();
		let changed = reopened
			.execute("SELECT VALUE age FROM person:alice", &crate::values::Object::new())
			.unwrap();
		let Value::Array(rows) = changed[0].result.as_ref().unwrap().value() else {
			panic!("updated point SELECT must return an array");
		};
		assert_eq!(rows.as_slice(), &[Value::from(31_i64)]);
	}

	/// Nested graphs, transition graphs and cross-level incidence remain
	/// readable after recovery and through explicit hybrid materialization.
	#[cfg(windows)]
	#[test]
	fn local_metagraph_round_trips_nested_content_and_cross_level_incidence() {
		let directory = LocalTestDirectory::new();
		let options = LocalOptions {
			buffer_pool_pages: 16,
			..LocalOptions::default()
		};
		let store = Datastore::create_local(directory.path(), options).unwrap();
		let mut inner = Graph::<Mutable>::new();
		let inner_node = inner
			.add_node_with_id("inner:node".parse().unwrap(), None, PropertyMap::new())
			.unwrap();
		let inner_second = inner
			.add_node_with_id("inner:second".parse().unwrap(), None, PropertyMap::new())
			.unwrap();
		let mut transition = Graph::<Mutable>::new();
		let transition_node = transition
			.add_node_with_id("transition:node".parse().unwrap(), None, PropertyMap::new())
			.unwrap();
		let mut graph = Metagraph::<Mutable>::new();
		let meta = graph.add_metavertex(inner.freeze(), None, PropertyMap::new()).unwrap();
		let local = graph.add_node(None, PropertyMap::new()).unwrap();
		let nested = QualifiedAtomId::new([meta], inner_node).unwrap();
		let nested_second = QualifiedAtomId::new([meta], inner_second).unwrap();
		let cross_topology = EdgeTopology::directed(
			[
				EdgeEndpoint::Nested(nested.clone()),
				EdgeEndpoint::Nested(nested_second),
				EdgeEndpoint::Local(meta),
			],
			[EdgeEndpoint::Local(local)],
		);
		let cross = graph.add_edge(cross_topology.clone(), None, PropertyMap::new()).unwrap();
		let connector = graph
			.add_edge(EdgeTopology::directed([cross], [local]), None, PropertyMap::new())
			.unwrap();
		let metaedge = graph
			.add_metaedge(
				EdgeTopology::directed([meta], [local]),
				transition.freeze(),
				None,
				PropertyMap::new(),
			)
			.unwrap();
		let mut create = store.begin();
		let reference = create.create_graph(graph_id("meta"), graph.freeze().into()).unwrap();
		create.commit().unwrap();
		drop(store);

		let store = Datastore::open_local(directory.path(), options).unwrap();
		let read = store.begin();
		let view = reference.read(&read).unwrap();
		let meta_node = view.node(meta).unwrap().unwrap();
		let inner = meta_node.inner_graph().unwrap().unwrap();
		assert!(inner.node(inner_node).is_some());
		assert!(inner.node(inner_second).is_some());
		assert_eq!(
			view.edge(cross).unwrap().unwrap().topology().unwrap().as_ref(),
			&cross_topology
		);
		assert_eq!(
			view.edge(connector).unwrap().unwrap().topology().unwrap().local_binary(),
			Some((cross, local, EdgeDirection::Directed))
		);
		let meta_edge = view.edge(metaedge).unwrap().unwrap();
		let transition = meta_edge.transition_graph().unwrap().unwrap();
		assert!(transition.node(transition_node).is_some());

		let hybrid = query_to_memory::<Graph<Frozen>, _>(&view).unwrap();
		assert_eq!(hybrid.node_count(), 2);
		assert_eq!(hybrid.edge_count(), 3);
		assert!(hybrid.node_ids().unwrap().any(|id| {
			let id = id.unwrap();
			hybrid.node(id).unwrap().inner_graph().is_some()
		}));
		assert!(hybrid.edge_ids().unwrap().any(|id| {
			let id = id.unwrap();
			hybrid.edge(id).unwrap().transition_graph().is_some()
		}));
	}

	/// One WAL commit publishes two graphs atomically, and a concurrent update
	/// to another atom in the same hot page merges against that durable image.
	#[cfg(windows)]
	#[test]
	fn local_multi_graph_commit_and_same_page_merge_survive_reopen() {
		let directory = LocalTestDirectory::new();
		let options = LocalOptions {
			buffer_pool_pages: 16,
			..LocalOptions::default()
		};
		let store = Datastore::create_local(directory.path(), options).unwrap();
		let value = store.intern_property("value").unwrap();
		let mut first_graph = Graph::<Mutable>::new();
		let left = first_graph.add_node(None, PropertyMap::new()).unwrap();
		let right = first_graph.add_node(None, PropertyMap::new()).unwrap();
		let mut second_graph = Graph::<Mutable>::new();
		let other = second_graph.add_node(None, PropertyMap::new()).unwrap();
		let mut create = store.begin();
		let first_ref =
			create.create_graph(graph_id("first"), first_graph.freeze().into()).unwrap();
		let second_ref =
			create.create_graph(graph_id("second"), second_graph.freeze().into()).unwrap();
		create.commit().unwrap();

		let transaction_options = TransactionOptions {
			isolation: IsolationLevel::Snapshot,
			access: TransactionAccess::ReadWrite,
		};
		let mut multi_graph = store.begin_with(transaction_options);
		let mut same_page = store.begin_with(transaction_options);
		if let GraphWriteView::Graph(graph) = multi_graph.edit_graph(&first_ref).unwrap() {
			graph.node_properties_mut(left).unwrap().insert(value, 1_i64);
		}
		if let GraphWriteView::Graph(graph) = multi_graph.edit_graph(&second_ref).unwrap() {
			graph.node_properties_mut(other).unwrap().insert(value, 2_i64);
		}
		if let GraphWriteView::Graph(graph) = same_page.edit_graph(&first_ref).unwrap() {
			graph.node_properties_mut(right).unwrap().insert(value, 3_i64);
		}
		multi_graph.commit().unwrap();
		same_page.commit().unwrap();
		drop(store);

		let store = Datastore::open_local(directory.path(), options).unwrap();
		let read = store.begin();
		let first = first_ref.read(&read).unwrap();
		assert_eq!(
			first.node(left).unwrap().unwrap().properties().unwrap().get(value),
			Some(&Value::from(1_i64))
		);
		assert_eq!(
			first.node(right).unwrap().unwrap().properties().unwrap().get(value),
			Some(&Value::from(3_i64))
		);
		let second = second_ref.read(&read).unwrap();
		assert_eq!(
			second.node(other).unwrap().unwrap().properties().unwrap().get(value),
			Some(&Value::from(2_i64))
		);
	}

	/// A failed unique-index build publishes neither its graph nor schema,
	/// including after reopening the database.
	#[cfg(windows)]
	#[test]
	fn local_unique_index_violation_rolls_back_publication() {
		let directory = LocalTestDirectory::new();
		let options = LocalOptions {
			buffer_pool_pages: 16,
			..LocalOptions::default()
		};
		let store = Datastore::create_local(directory.path(), options).unwrap();
		let email = store.intern_property("email").unwrap();
		let mut graph = Graph::<Mutable>::new();
		for id in ["first", "second"] {
			let mut properties = PropertyMap::new();
			properties.insert(email, "same@example.test");
			graph
				.add_node_with_id(format!("person:{id}").parse().unwrap(), None, properties)
				.unwrap();
		}
		let mut create = store.begin();
		let reference = create.create_graph(graph_id("duplicates"), graph.freeze().into()).unwrap();
		create
			.define_table(
				TableSchema::new("person", reference.clone(), TableKind::Normal, [
					TableField::new("email", Type::String),
				])
				.unwrap()
				.with_index(
					TableIndex::new("person_email", TableIndexKind::Unique, ["email"]).unwrap(),
				)
				.unwrap(),
			)
			.unwrap();
		assert!(create.commit().is_err());
		assert!(store.table("person").is_none());
		assert!(reference.read(&store.begin()).is_err());
		drop(store);

		let store = Datastore::open_local(directory.path(), options).unwrap();
		assert!(store.table("person").is_none());
		assert!(reference.read(&store.begin()).is_err());
	}

	/// Stable AtomRef point mutations preserve incidence and properties across
	/// close/recovery without exposing transaction-local AtomId handles.
	#[cfg(windows)]
	#[test]
	fn local_point_mutations_survive_reopen() {
		let directory = LocalTestDirectory::new();
		let options = LocalOptions {
			buffer_pool_pages: 16,
			..LocalOptions::default()
		};
		let store = Datastore::create_local(directory.path(), options).unwrap();
		let score = store.intern_property("score").unwrap();
		let transient = store.intern_property("transient").unwrap();
		let mut create = store.begin();
		let graph = create
			.create_graph(graph_id("mutations"), Graph::<Mutable>::new().freeze().into())
			.unwrap();
		create.commit().unwrap();
		drop(store);

		let store = Datastore::open_local(directory.path(), options).unwrap();
		let mut transaction = store.begin();
		let mut mutation = transaction.mutate_graph(&graph).unwrap();
		let alpha = mutation
			.create_node("item:alpha".parse().unwrap(), Some("Item".into()), PropertyMap::new())
			.unwrap();
		let beta =
			mutation.create_node("item:beta".parse().unwrap(), None, PropertyMap::new()).unwrap();
		let gamma =
			mutation.create_node("item:gamma".parse().unwrap(), None, PropertyMap::new()).unwrap();
		let edge = mutation
			.create_edge(
				"link:one".parse().unwrap(),
				&AtomRefTopology::Directed {
					source: vec![alpha.clone()],
					target: vec![beta.clone()],
				},
				None,
				PropertyMap::new(),
			)
			.unwrap();
		mutation.set_property(&alpha, score, Value::from(7_i64)).unwrap();
		assert_eq!(
			mutation.set_label(&alpha, Some("Changed".into())).unwrap(),
			Some("Item".into())
		);
		mutation.set_property(&alpha, transient, Value::from(true)).unwrap();
		assert_eq!(mutation.remove_property(&alpha, transient).unwrap(), Some(Value::from(true)));
		mutation
			.replace_incidence(&edge, &AtomRefTopology::Directed {
				source: vec![alpha.clone()],
				target: vec![gamma.clone()],
			})
			.unwrap();
		mutation.remove_atom(&beta).unwrap();
		drop(mutation);
		transaction.commit().unwrap();
		drop(store);

		let store = Datastore::open_local(directory.path(), options).unwrap();
		let transaction = store.begin();
		let view = graph.read(&transaction).unwrap();
		let alpha_id = view.atom_id(&alpha).unwrap().unwrap();
		let gamma_id = view.atom_id(&gamma).unwrap().unwrap();
		let edge_id = view.atom_id(&edge).unwrap().unwrap();
		assert!(view.atom_id(&beta).unwrap().is_none());
		assert_eq!(
			view.node(alpha_id).unwrap().unwrap().properties().unwrap().get(score),
			Some(&Value::from(7_i64))
		);
		assert_eq!(
			view.node(alpha_id).unwrap().unwrap().label_text().unwrap().as_deref(),
			Some(&Text::from("Changed"))
		);
		assert_eq!(
			view.edge(edge_id).unwrap().unwrap().topology().unwrap().local_binary(),
			Some((alpha_id, gamma_id, EdgeDirection::Directed))
		);
	}

	/// The public local path reads large property chains and adjacency
	/// overflow, while an aborted materialized mutation remains
	/// transaction-private.
	#[cfg(windows)]
	#[test]
	fn local_large_properties_overflow_and_abort_round_trip() {
		let directory = LocalTestDirectory::new();
		let options = LocalOptions {
			buffer_pool_pages: 16,
			..LocalOptions::default()
		};
		let store = Datastore::create_local(directory.path(), options).unwrap();
		let payload_key = store.intern_property("payload").unwrap();
		let payload = Text::from("x".repeat(10_000));
		let mut graph = Graph::<Mutable>::new();
		let mut source = Vec::new();
		let mut target = Vec::new();
		for index in 0..6 {
			let mut properties = PropertyMap::new();
			if index == 0 {
				properties.insert(payload_key, payload.clone());
			}
			source.push(
				graph
					.add_node_with_id(
						format!("item:source-{index}").parse().unwrap(),
						None,
						properties,
					)
					.unwrap(),
			);
			target.push(
				graph
					.add_node_with_id(
						format!("item:target-{index}").parse().unwrap(),
						None,
						PropertyMap::new(),
					)
					.unwrap(),
			);
		}
		let edge = graph
			.add_edge_with_id(
				"link:wide".parse().unwrap(),
				EdgeTopology::directed(source.clone(), target.clone()),
				None,
				PropertyMap::new(),
			)
			.unwrap();
		let mut create = store.begin();
		let reference = create.create_graph(graph_id("wide"), graph.freeze().into()).unwrap();
		create.commit().unwrap();
		drop(store);

		let store = Datastore::open_local(directory.path(), options).unwrap();
		let read = store.begin();
		let view = reference.read(&read).unwrap();
		let node = view.node(source[0]).unwrap().unwrap();
		assert_eq!(
			node.properties().unwrap().get(payload_key),
			Some(&Value::from(payload.clone()))
		);
		let wide_edge = view.edge(edge).unwrap().unwrap();
		let topology = wide_edge.topology().unwrap();
		let EdgeTopology::Directed {
			source: decoded_source,
			target: decoded_target,
		} = topology.as_ref()
		else {
			panic!("wide directed edge changed topology");
		};
		assert_eq!(decoded_source.len(), source.len());
		assert_eq!(decoded_target.len(), target.len());
		drop(view);
		drop(read);

		let source_ref = AtomRef::new(AtomKind::Node, "item:source-0".parse().unwrap());
		let mut aborted = store.begin();
		aborted
			.mutate_graph(&reference)
			.unwrap()
			.set_property(&source_ref, payload_key, Value::from("discarded"))
			.unwrap();
		aborted.abort();
		let read = store.begin();
		let view = reference.read(&read).unwrap();
		assert_eq!(
			view.node(source[0]).unwrap().unwrap().properties().unwrap().get(payload_key),
			Some(&Value::from(payload))
		);
	}

	/// Every runtime Value variant admitted by the format-v1 property codec
	/// survives the complete local commit, recovery and reference-resolution
	/// path.
	#[cfg(windows)]
	#[test]
	fn local_persistable_value_tags_round_trip() {
		let directory = LocalTestDirectory::new();
		let options = LocalOptions {
			buffer_pool_pages: 16,
			..LocalOptions::default()
		};
		let store = Datastore::create_local(directory.path(), options).unwrap();
		let all = store.intern_property("all").unwrap();
		let anchor_record: RecordId = "value:anchor".parse().unwrap();
		let anchor_ref = AtomRef::new(AtomKind::Node, anchor_record.clone());
		let point = Position::new([10.0, 20.0]).unwrap();
		let second = Position::new([-10.0, -20.0]).unwrap();
		let mut values = Object::new();
		values.insert("none", Value::None);
		values.insert("null", Value::Null);
		values.insert("false", false);
		values.insert("true", true);
		values.insert("int", i64::MIN);
		values.insert("float", 1.5_f64);
		values.insert("decimal", "-123.45".parse::<Decimal>().unwrap());
		values.insert("string", "hello");
		values.insert("bytes", Bytes::from([0_u8, 255]));
		values.insert(
			"datetime",
			"1969-12-31T23:59:59.500000000+00:00[UTC]".parse::<DateTime>().unwrap(),
		);
		values.insert("duration", "P1DT2H".parse::<ValueDuration>().unwrap());
		values.insert("uuid", Uuid::new_v4());
		values.insert("ulid", Ulid::new());
		values.insert("record", "person:stored".parse::<RecordId>().unwrap());
		values.insert("array", Array::new(vec![Value::from(1_i64), Value::from("x")]));
		values.insert("set", Set::from(vec![1_i64, 2]));
		values.insert("object", Object::from_iter([("nested", Value::from("value"))]));
		values.insert(
			"range",
			Range::new(Bound::Included(1_i64.into()), Bound::Excluded(3_i64.into())).unwrap(),
		);
		values.insert("point", Geometry::Point(point.clone()));
		values.insert("line", Geometry::LineString(vec![point.clone(), second.clone()]));
		values.insert("polygon", Geometry::Polygon(vec![vec![point.clone(), second.clone()]]));
		values.insert("multipoint", Geometry::MultiPoint(vec![point.clone(), second.clone()]));
		values.insert(
			"multiline",
			Geometry::MultiLineString(vec![vec![point.clone(), second.clone()]]),
		);
		values.insert(
			"multipolygon",
			Geometry::MultiPolygon(vec![vec![vec![point.clone(), second.clone()]]]),
		);
		values.insert("geometry_collection", Geometry::Collection(vec![Geometry::Point(point)]));
		values.insert("vector_f64", Vector::F64(vec![1.0, 2.0]));
		values.insert("vector_f32", Vector::F32(vec![1.0, 2.0]));
		values.insert("vector_i64", Vector::I64(vec![1, 2]));
		values.insert("vector_i32", Vector::I32(vec![1, 2]));
		values.insert("vector_i16", Vector::I16(vec![1, 2]));
		values.insert("atom", anchor_ref);

		let mut properties = PropertyMap::new();
		properties.insert(all, Value::Object(values.clone()));
		let mut graph = Graph::<Mutable>::new();
		let anchor = graph.add_node_with_id(anchor_record, None, properties).unwrap();
		let mut create = store.begin();
		let reference = create.create_graph(graph_id("values"), graph.freeze().into()).unwrap();
		create.commit().unwrap();
		drop(store);

		let store = Datastore::open_local(directory.path(), options).unwrap();
		let read = store.begin();
		let view = reference.read(&read).unwrap();
		assert_eq!(
			view.node(anchor).unwrap().unwrap().properties().unwrap().get(all),
			Some(&Value::Object(values))
		);
	}

	/// A disk tombstone preserves the allocator generation across reopen, so a
	/// reused slot cannot make an old transaction-local handle valid again.
	#[cfg(windows)]
	#[test]
	fn local_reopen_preserves_reused_slot_generation() {
		let directory = LocalTestDirectory::new();
		let options = LocalOptions {
			buffer_pool_pages: 16,
			..LocalOptions::default()
		};
		let store = Datastore::create_local(directory.path(), options).unwrap();
		let mut graph = Graph::<Mutable>::new();
		let stale = graph
			.add_node_with_id("item:stale".parse().unwrap(), None, PropertyMap::new())
			.unwrap();
		graph.add_node_with_id("item:kept".parse().unwrap(), None, PropertyMap::new()).unwrap();
		let mut create = store.begin();
		let reference = create.create_graph(graph_id("slots"), graph.freeze().into()).unwrap();
		create.commit().unwrap();
		drop(store);

		let reopened = Datastore::open_local(directory.path(), options).unwrap();
		let mut remove = reopened.begin();
		if let GraphWriteView::Graph(graph) = remove.edit_graph(&reference).unwrap() {
			graph.remove_atom(stale).unwrap();
		}
		remove.commit().unwrap();
		drop(reopened);

		let reopened = Datastore::open_local(directory.path(), options).unwrap();
		let mut reuse = reopened.begin();
		let replacement = match reuse.edit_graph(&reference).unwrap() {
			GraphWriteView::Graph(graph) => graph
				.add_node_with_id("item:replacement".parse().unwrap(), None, PropertyMap::new())
				.unwrap(),
			_other => panic!("general graph must keep its family"),
		};
		assert_eq!(replacement.slot(), stale.slot());
		assert_eq!(replacement.generation(), stale.generation() + 1);
		reuse.commit().unwrap();
		drop(reopened);

		let reopened = Datastore::open_local(directory.path(), options).unwrap();
		let generation_count = std::fs::read_dir(directory.path().join("graphs"))
			.unwrap()
			.flat_map(|graph| std::fs::read_dir(graph.unwrap().path()).unwrap())
			.count();
		assert_eq!(generation_count, 1);
		let read = reopened.begin();
		let view = reference.read(&read).unwrap();
		assert!(view.node(stale).unwrap().is_none());
		assert!(view.node(replacement).unwrap().is_some());
	}

	/// Passive checkpoint preserves a reader snapshot while Restart invalidates
	/// it and makes the complete durable tail recoverable from home files.
	#[cfg(windows)]
	#[test]
	fn local_restart_checkpoint_cancels_old_snapshot() {
		let directory = LocalTestDirectory::new();
		let options = LocalOptions {
			buffer_pool_pages: 16,
			..LocalOptions::default()
		};
		let store = Datastore::create_local(directory.path(), options).unwrap();
		let value = store.intern_property("value").unwrap();
		let mut properties = PropertyMap::new();
		properties.insert(value, 1_i64);
		let mut graph = Graph::<Mutable>::new();
		let node = graph.add_node_with_id("item:one".parse().unwrap(), None, properties).unwrap();
		let mut create = store.begin();
		let reference = create.create_graph(graph_id("checkpoint"), graph.freeze().into()).unwrap();
		create.commit().unwrap();
		drop(store);

		let store = Datastore::open_local(directory.path(), options).unwrap();
		let old = store.begin();
		let old_view = reference.read(&old).unwrap();
		let mut writer = store.begin();
		if let GraphWriteView::Graph(graph) = writer.edit_graph(&reference).unwrap() {
			graph.node_properties_mut(node).unwrap().insert(value, 2_i64);
		}
		writer.commit().unwrap();

		store.checkpoint(CheckpointMode::Passive).unwrap();
		assert_eq!(
			old_view.node(node).unwrap().unwrap().properties().unwrap().get(value),
			Some(&Value::from(1_i64))
		);
		store.checkpoint(CheckpointMode::Restart).unwrap();
		assert_eq!(old_view.node_count().unwrap_err().status().code().to_string(), "FG-25F02");
		drop(old_view);
		drop(old);
		drop(store);

		let store = Datastore::open_local(directory.path(), options).unwrap();
		let fresh = store.begin();
		let view = reference.read(&fresh).unwrap();
		assert_eq!(
			view.node(node).unwrap().unwrap().properties().unwrap().get(value),
			Some(&Value::from(2_i64))
		);
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
	fn local_checkpoint_policy_validates_percent_and_rounds_up() {
		let validated = validate_local_options(LocalOptions {
			buffer_pool_pages: 10,
			checkpoint: CheckpointPolicy {
				unbackfilled_page_percent: 25,
				interval: Duration::ZERO,
			},
			..LocalOptions::default()
		})
		.unwrap();
		assert_eq!(validated.checkpoint_page_threshold, 3);
		for percent in [0, 101] {
			assert!(
				validate_local_options(LocalOptions {
					checkpoint: CheckpointPolicy {
						unbackfilled_page_percent: percent,
						..CheckpointPolicy::default()
					},
					..LocalOptions::default()
				})
				.is_err()
			);
		}
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

		let old_rows = reader.query_rows(&statement, "person", &[]).unwrap();
		let old_carol =
			old_rows.iter().find(|row| row.record_id.to_string() == "person:carol").unwrap();
		assert_eq!(old_carol.values[2], Value::from(17_i64));

		let fresh = reader.query_snapshot().unwrap();
		let fresh_rows = reader.query_rows(&fresh, "person", &[]).unwrap();
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
		assert_eq!(
			view.node(left).unwrap().unwrap().properties().unwrap().get(key),
			Some(&Value::from(1_i64))
		);
		assert_eq!(
			view.node(right).unwrap().unwrap().properties().unwrap().get(key),
			Some(&Value::from(2_i64))
		);
	}

	#[test]
	fn concurrent_writes_to_the_same_atom_fail_first_committer_wins() {
		let (store, reference, left, _) = seeded_store();
		let options = TransactionOptions {
			isolation: IsolationLevel::Snapshot,
			access: TransactionAccess::ReadWrite,
		};
		let mut first = store.begin_with(options);
		let mut second = store.begin_with(options);
		if let GraphWriteView::Graph(graph) = first.edit_graph(&reference).unwrap() {
			graph.node_properties_mut(left).unwrap().insert(0_u32.into(), 1_i64);
		}
		if let GraphWriteView::Graph(graph) = second.edit_graph(&reference).unwrap() {
			graph.node_properties_mut(left).unwrap().insert(0_u32.into(), 2_i64);
		}
		first.commit().unwrap();
		assert_eq!(second.commit().unwrap_err().status().code().to_string(), "FG-40001");
	}

	#[test]
	fn serializable_reader_aborts_after_concurrent_change() {
		let (store, reference, left, _) = seeded_store();
		let reader = store.begin();
		assert!(reference.read(&reader).unwrap().node(left).unwrap().is_some());

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
		assert!(reference.read(&reader).unwrap().node(left).unwrap().is_some());
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

	#[test]
	fn unavailable_native_async_mode_has_no_sync_fallback() {
		let error = validate_local_options(LocalOptions {
			io_mode: IoMode::NativeAsync,
			..LocalOptions::default()
		})
		.unwrap_err();
		assert_eq!(error.status().code().to_string(), "FG-51I04");
	}
}
