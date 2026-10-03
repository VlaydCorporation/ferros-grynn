//! Public datastore construction and immutable-snapshot entry points.

use std::{
	path::Path,
	sync::{
		Arc,
		atomic::{AtomicU64, AtomicUsize, Ordering},
	},
};

use parking_lot::{Mutex, RwLock};

use super::{
	CheckpointMode,
	ClassId,
	ClassSchema,
	DatabaseRoot,
	DatastoreOptions,
	GraphSchema,
	InMemoryOptions,
	LocalOptions,
	LocalStorageStatus,
	MaintenanceAnalyzeMode,
	MaintenanceCoordinator,
	MaintenanceJobInfo,
	MaintenanceJobKind,
	MaintenanceOptions,
	MaintenanceTarget,
	SchemaCatalog,
	StoreBackend,
	StoreInner,
	Transaction,
	TransactionOptions,
	validate_local_options,
};
use crate::{
	gql_status::FerrosGrynnResult,
	graph::GraphId,
	storage::LocalStore,
	values::{Object, PropertyDictionary},
};

/// Database owning one global atom registry and any number of graph views.
pub struct Datastore {
	pub(crate) inner: Arc<StoreInner>,
}

impl Clone for Datastore {
	fn clone(&self) -> Self {
		if self
			.inner
			.datastore_handles
			.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |handles| handles.checked_add(1))
			.is_err()
		{
			std::process::abort();
		}
		Self {
			inner: self.inner.clone(),
		}
	}
}

impl Drop for Datastore {
	fn drop(&mut self) {
		if self.inner.datastore_handles.fetch_sub(1, Ordering::AcqRel) == 1 {
			self.inner.maintenance.shutdown();
		}
	}
}

impl Datastore {
	/// Opens one immutable rejected-item set by its durable identity.
	///
	/// # Errors
	///
	/// Returns an unknown-identity, corruption, decode, or I/O error.
	pub fn open_quarantine(
		&self,
		id: super::QuarantineId,
	) -> FerrosGrynnResult<super::QuarantineReader> {
		self.inner.open_quarantine(id.as_uuid()).map(super::QuarantineReader::new)
	}

	/// Removes one rejected-item set from the durable catalog.
	///
	/// # Errors
	///
	/// Returns an unknown-identity, persistence, or I/O error.
	pub fn remove_quarantine(&self, id: super::QuarantineId) -> FerrosGrynnResult<()> {
		self.inner.remove_quarantine(id.as_uuid())
	}

	/// Returns effective local storage budgets and pressure counters, or `None`
	/// for an in-memory datastore.
	pub fn local_storage_status(&self) -> Option<LocalStorageStatus> {
		self.inner.local_storage_status()
	}

	/// Creates a volatile datastore with default transaction limits.
	pub fn in_memory() -> Self {
		Self::in_memory_with(InMemoryOptions::default())
	}

	/// Creates a volatile datastore with explicit transaction limits.
	pub fn in_memory_with(options: InMemoryOptions) -> Self {
		Self::from_parts(
			DatastoreOptions::InMemory(options),
			StoreBackend::InMemory,
			DatabaseRoot::default(),
		)
	}

	/// Creates and exclusively opens a new local database directory.
	pub fn create_local(path: impl AsRef<Path>, options: LocalOptions) -> FerrosGrynnResult<Self> {
		let validated = validate_local_options(options)?;
		let (local, root) = LocalStore::create(
			path.as_ref(),
			validated.pool_pages,
			validated.io_mode,
			validated.checkpoint_page_threshold,
			options.checkpoint.interval,
		)?;
		let store = Self::from_parts(
			DatastoreOptions::Local(options),
			StoreBackend::Local(Box::new(local)),
			root,
		);
		store.inner.maintenance.start(&store.inner)?;
		Ok(store)
	}

	/// Opens, recovers, and exclusively owns an existing local database.
	pub fn open_local(path: impl AsRef<Path>, options: LocalOptions) -> FerrosGrynnResult<Self> {
		let validated = validate_local_options(options)?;
		let (local, root) = crate::storage::with_external_sort_options(
			options.maintenance.max_memory_bytes,
			options.maintenance.max_parallelism,
			|| {
				LocalStore::open(
					path.as_ref(),
					validated.pool_pages,
					validated.io_mode,
					validated.checkpoint_page_threshold,
					options.checkpoint.interval,
				)
			},
		)?;
		let store = Self::from_parts(
			DatastoreOptions::Local(options),
			StoreBackend::Local(Box::new(local)),
			root,
		);
		store.inner.maintenance.start(&store.inner)?;
		Ok(store)
	}

	fn from_parts(options: DatastoreOptions, backend: StoreBackend, root: DatabaseRoot) -> Self {
		let next_schema_id = root.schema.next_schema_id;
		let next_graph_id = root.schema.next_graph_id;
		let query_scratch_root = match &backend {
			StoreBackend::InMemory => std::env::temp_dir()
				.join(format!("ferros-grynn-query-{}", crate::values::Uuid::now_v7())),
			StoreBackend::Local(local) => local.import_scratch_root().to_path_buf(),
		};
		let recovered_jobs = match &backend {
			StoreBackend::InMemory => Vec::new(),
			StoreBackend::Local(local) => local.maintenance_jobs(),
		};
		Self {
			inner: Arc::new(StoreInner {
				options,
				backend,
				next_transaction: AtomicU64::new(1),
				next_schema_id: AtomicU64::new(next_schema_id),
				next_graph_id: AtomicU64::new(next_graph_id),
				import_owner: AtomicU64::new(0),
				datastore_handles: AtomicUsize::new(1),
				current: RwLock::new(Arc::new(root)),
				commit: Mutex::new(()),
				query_scratch_root,
				quarantines: Mutex::new(std::collections::BTreeMap::new()),
				maintenance: MaintenanceCoordinator::new(recovered_jobs),
			}),
		}
	}

	/// Returns one durable maintenance job, if it exists.
	pub fn job(&self, id: super::JobId) -> FerrosGrynnResult<Option<MaintenanceJobInfo>> {
		Ok(self.inner.maintenance.job(id))
	}

	/// Returns all retained active and terminal maintenance jobs.
	pub fn jobs(&self) -> FerrosGrynnResult<Vec<MaintenanceJobInfo>> {
		Ok(self.inner.maintenance.jobs())
	}

	/// Cooperatively cancels one queued or running maintenance job.
	pub fn kill_job(&self, id: super::JobId) -> FerrosGrynnResult<MaintenanceJobInfo> {
		self.inner.maintenance.kill(self.inner.as_ref(), id)
	}

	/// Builds or rebuilds one graph index through the existing publication
	/// path.
	///
	/// # Errors
	/// Returns a missing definition, invalid options, concurrent revision,
	/// resource, cancellation or storage error.
	pub fn rebuild_graph_index(
		&self,
		graph: crate::graph::GraphId,
		index: super::IndexId,
		options: MaintenanceOptions,
	) -> FerrosGrynnResult<()> {
		let target = self.graph_index_target(graph, index)?;
		self.run_foreground_maintenance(MaintenanceJobKind::RebuildIndex, target, options)
	}

	/// Queues a graph-index rebuild on the datastore's durable maintenance
	/// worker.
	///
	/// # Errors
	/// Returns a missing definition, invalid options, queue or storage error.
	pub fn rebuild_graph_index_concurrently(
		&self,
		graph: crate::graph::GraphId,
		index: super::IndexId,
		options: Option<MaintenanceOptions>,
	) -> FerrosGrynnResult<super::JobId> {
		let target = self.graph_index_target(graph, index)?;
		self.enqueue_maintenance(MaintenanceJobKind::RebuildIndex, target, options)
	}

	/// Resolves an exact graph index/version for both SDK maintenance routes.
	fn graph_index_target(
		&self,
		graph: crate::graph::GraphId,
		index: super::IndexId,
	) -> FerrosGrynnResult<MaintenanceTarget> {
		let root = self.inner.current_root();
		let schema = root.schema.graph(graph).ok_or_else(|| {
			crate::gql_status::FerrosGrynnError::graph_does_not_exist(graph.to_string())
		})?;
		let version = if let Some(definition) =
			schema.indexes().find(|definition| definition.id() == Some(index))
		{
			definition.version()
		} else if let Some(endpoint) =
			schema.edge_endpoint_index().filter(|endpoint| endpoint.id() == index)
		{
			endpoint.version()
		} else {
			return Err(crate::gql_status::FerrosGrynnError::invalid_argument(
				"graph index is absent",
			));
		};
		Ok(MaintenanceTarget::GraphIndex {
			graph,
			index,
			version,
		})
	}

	pub(crate) fn enqueue_maintenance(
		&self,
		kind: MaintenanceJobKind,
		target: MaintenanceTarget,
		options: Option<MaintenanceOptions>,
	) -> FerrosGrynnResult<super::JobId> {
		self.enqueue_maintenance_with_analysis(kind, target, options, None)
	}

	pub(crate) fn enqueue_maintenance_coalesced(
		&self,
		kind: MaintenanceJobKind,
		target: MaintenanceTarget,
		options: Option<MaintenanceOptions>,
	) -> FerrosGrynnResult<super::JobId> {
		let options = options.unwrap_or_else(|| match self.options() {
			DatastoreOptions::InMemory(options) => options.maintenance,
			DatastoreOptions::Local(options) => options.maintenance,
		});
		self.inner.maintenance.enqueue_coalesced(&self.inner, kind, target, options, None)
	}

	pub(crate) fn enqueue_maintenance_with_analysis(
		&self,
		kind: MaintenanceJobKind,
		target: MaintenanceTarget,
		options: Option<MaintenanceOptions>,
		analyze_mode: Option<MaintenanceAnalyzeMode>,
	) -> FerrosGrynnResult<super::JobId> {
		let analyze_mode = if kind == MaintenanceJobKind::Analyze {
			Some(self.inner.resolve_analyze_mode(
				target,
				analyze_mode.unwrap_or(MaintenanceAnalyzeMode::Automatic),
			)?)
		} else {
			if analyze_mode.is_some() {
				return Err(crate::gql_status::FerrosGrynnError::invalid_argument(
					"only ANALYZE maintenance accepts a statistics build mode",
				));
			}
			None
		};
		let configured = match self.inner.options {
			DatastoreOptions::InMemory(options) => options.maintenance,
			DatastoreOptions::Local(options) => options.maintenance,
		};
		let selected = options.unwrap_or(MaintenanceOptions {
			max_memory_bytes: configured.max_memory_bytes,
			max_parallelism: std::num::NonZeroUsize::MIN,
		});
		self.inner.maintenance.enqueue(&self.inner, kind, target, selected, analyze_mode)
	}

	pub(crate) fn record_deferred_index_change(
		&self,
		target: MaintenanceTarget,
		revision: u64,
	) -> FerrosGrynnResult<super::JobId> {
		let configured = match self.inner.options {
			DatastoreOptions::InMemory(options) => options.maintenance,
			DatastoreOptions::Local(options) => options.maintenance,
		};
		let selected = MaintenanceOptions {
			max_memory_bytes: configured.max_memory_bytes,
			max_parallelism: std::num::NonZeroUsize::MIN,
		};
		self.inner.maintenance.record_deferred_index_change(&self.inner, target, revision, selected)
	}

	pub(crate) fn run_foreground_maintenance(
		&self,
		kind: MaintenanceJobKind,
		target: MaintenanceTarget,
		options: MaintenanceOptions,
	) -> FerrosGrynnResult<()> {
		self.run_foreground_maintenance_with_analysis(kind, target, options, None)
	}

	pub(crate) fn run_foreground_maintenance_with_analysis(
		&self,
		kind: MaintenanceJobKind,
		target: MaintenanceTarget,
		options: MaintenanceOptions,
		analyze_mode: Option<MaintenanceAnalyzeMode>,
	) -> FerrosGrynnResult<()> {
		let analyze_mode = if kind == MaintenanceJobKind::Analyze {
			Some(self.inner.resolve_analyze_mode(
				target,
				analyze_mode.unwrap_or(MaintenanceAnalyzeMode::Automatic),
			)?)
		} else {
			if analyze_mode.is_some() {
				return Err(crate::gql_status::FerrosGrynnError::invalid_argument(
					"only ANALYZE maintenance accepts a statistics build mode",
				));
			}
			None
		};
		self.inner.run_foreground_maintenance(kind, target, options, analyze_mode)
	}

	#[cfg(test)]
	pub(crate) fn from_root_for_test(options: InMemoryOptions, root: DatabaseRoot) -> Self {
		Self::from_parts(DatastoreOptions::InMemory(options), StoreBackend::InMemory, root)
	}

	/// Opens a default serializable read-write transaction.
	pub fn begin(&self) -> Transaction {
		self.begin_with(TransactionOptions::default())
	}

	/// Creates a persistent GrynnQL session with independent lexical bindings.
	pub fn session(&self) -> super::GqlSession {
		super::GqlSession::new(self.clone())
	}

	/// Opens a transaction on one immutable database-wide snapshot.
	pub fn begin_with(&self, options: TransactionOptions) -> Transaction {
		let mut root = self.inner.current_root();
		let _commit = (matches!(&self.inner.backend, StoreBackend::Local(_))
			&& root.schema.has_snapshot_indexes())
		.then(|| self.inner.commit.lock());
		if _commit.is_some() {
			root = self.inner.current_root();
		}
		Transaction::new(
			self.clone(),
			self.inner.next_transaction.fetch_add(1, Ordering::Relaxed),
			options,
			root,
		)
	}

	/// Parses, analyzes, and executes one GrynQL program in an automatic
	/// transaction or atomic import session.
	///
	/// # Errors
	///
	/// Returns parser, analysis, transaction, persistence, or publication
	/// failures. Statement execution failures remain in their query result.
	pub fn execute(
		&self,
		source: &str,
		variables: &Object,
	) -> FerrosGrynnResult<Vec<crate::gql::QueryResult>> {
		self.execute_with_settings(source, variables, crate::gql::ParserSettings::default())
	}

	/// Executes one GrynQL program with explicit parser settings.
	///
	/// # Errors
	///
	/// Returns parser, analysis, transaction, persistence, or publication
	/// failures.
	pub fn execute_with_settings(
		&self,
		source: &str,
		variables: &Object,
		settings: crate::gql::ParserSettings,
	) -> FerrosGrynnResult<Vec<crate::gql::QueryResult>> {
		self.execute_with_options(source, variables, crate::gql::QueryExecutionOptions {
			parser_settings: settings,
			..crate::gql::QueryExecutionOptions::default()
		})
	}

	/// Executes a bounded program with explicit parser and runtime controls.
	///
	/// # Errors
	///
	/// Returns parse, structural-validation, import, persistence, or
	/// publication failures. A bounded call must close every explicit
	/// transaction.
	pub fn execute_with_options(
		&self,
		source: &str,
		variables: &Object,
		options: crate::gql::QueryExecutionOptions,
	) -> FerrosGrynnResult<Vec<crate::gql::QueryResult>> {
		super::GqlSession::new_transient(self.clone())
			.execute_internal(source, variables, &options, true)
	}

	/// Returns the selected backend and resource limits.
	pub fn options(&self) -> DatastoreOptions {
		self.inner.options
	}

	/// Runs explicit local maintenance; volatile stores have no checkpoint.
	pub fn checkpoint(&self, mode: CheckpointMode) -> FerrosGrynnResult<bool> {
		self.inner.checkpoint(mode)
	}

	/// Returns the currently published schema catalog.
	pub fn schema_catalog(&self) -> Arc<SchemaCatalog> {
		self.inner.current_root().schema.clone()
	}

	/// Returns the currently published property-key dictionary.
	pub fn property_dictionary(&self) -> Arc<PropertyDictionary> {
		self.inner.current_root().dictionary.clone()
	}

	/// Resolves one currently published class.
	pub fn class(&self, name: &str) -> Option<Arc<ClassSchema>> {
		self.schema_catalog().class(name)
	}

	/// Resolves one currently published graph.
	pub fn graph(&self, id: GraphId) -> Option<Arc<GraphSchema>> {
		self.schema_catalog().graph(id)
	}

	/// Resolves a class by its stable identity.
	pub fn class_by_id(&self, id: ClassId) -> Option<Arc<ClassSchema>> {
		self.schema_catalog().class_by_id(id)
	}
}
