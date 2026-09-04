use std::{
	borrow::Cow,
	collections::{BTreeMap, BTreeSet},
	path::Path,
	sync::{
		Arc,
		atomic::{AtomicU64, Ordering},
	},
	time::Instant,
};

use parking_lot::{Mutex, RwLock};

use crate::{
	collections::cow_vec::CowVec,
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	storage::{
		LocalEdgeRead,
		LocalGraph,
		LocalGraphView,
		LocalNodeRead,
		LocalReadTransaction,
		LocalStore,
		LocalTableCursor,
	},
	values::runtime::{
		PropertyDictionary,
		PropertyKeyId,
		PropertyMap,
		RecordId,
		Text,
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
};

/// RAM/local backend selection and durable-root adaptation.
mod backend;
/// Shared datastore constants.
mod consts;
/// Backend and transaction configuration.
mod options;
/// MVCC root, catalog state, and version histories.
mod root;
/// Public table and graph schema definitions.
mod schema;
/// Transaction lifecycle, mutation, conflict, query, and graph views.
mod transaction;

use backend::{StoreBackend, StoreInner, validate_local_options};
pub(crate) use consts::INTERNAL_NESTED_GRAPH_TABLE;
use consts::{
	CATALOG_DELETE_WRITE_BYTES,
	DIRTY_TARGET_WRITE_BYTES,
	TABLE_FIELD_WRITE_BYTES,
	TABLE_SCHEMA_WRITE_BYTES,
};
pub use options::{
	CheckpointMode,
	CheckpointPolicy,
	DatastoreOptions,
	InMemoryOptions,
	IoMode,
	IsolationLevel,
	LocalOptions,
	TransactionAccess,
	TransactionOptions,
};
use root::{ActiveSnapshot, AtomVersions, DatabaseRoot, StoredGraph, StoredGraphValue};
use schema::validate_graph_name;
pub use schema::{TableField, TableIndex, TableIndexKind, TableKind, TableSchema};
pub use transaction::{
	AtomRefTopology,
	CommitResult,
	GraphMutationView,
	GraphWriteView,
	Transaction,
	TransactionEdgeRead,
	TransactionGraphView,
	TransactionNodeRead,
};
pub(crate) use transaction::{
	PredicateEvaluator,
	QuerySnapshot,
	QueryStorageCursor,
	QueryStorageRow,
	ReadKey,
	ScalarComparison,
	ScalarIndexConstraint,
};

#[derive(Clone)]
pub struct Datastore {
	inner: Arc<StoreInner>,
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
			validated.io_mode,
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
			validated.io_mode,
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
			next.dictionary = persisted.property_dictionary;
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

#[cfg(test)]
mod tests;
