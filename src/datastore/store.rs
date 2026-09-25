//! Public datastore construction and immutable-snapshot entry points.

use std::{
	path::Path,
	sync::{
		Arc,
		atomic::{AtomicU64, Ordering},
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
#[derive(Clone)]
pub struct Datastore {
	pub(crate) inner: Arc<StoreInner>,
}

impl Datastore {
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
		Ok(Self::from_parts(
			DatastoreOptions::Local(options),
			StoreBackend::Local(Box::new(local)),
			root,
		))
	}

	/// Opens, recovers, and exclusively owns an existing local database.
	pub fn open_local(path: impl AsRef<Path>, options: LocalOptions) -> FerrosGrynnResult<Self> {
		let validated = validate_local_options(options)?;
		let (local, root) = LocalStore::open(
			path.as_ref(),
			validated.pool_pages,
			validated.io_mode,
			validated.checkpoint_page_threshold,
			options.checkpoint.interval,
		)?;
		Ok(Self::from_parts(
			DatastoreOptions::Local(options),
			StoreBackend::Local(Box::new(local)),
			root,
		))
	}

	fn from_parts(options: DatastoreOptions, backend: StoreBackend, root: DatabaseRoot) -> Self {
		let next_schema_id = root.schema.next_schema_id;
		let next_graph_id = root.schema.next_graph_id;
		let query_scratch_root = match &backend {
			StoreBackend::InMemory => std::env::temp_dir()
				.join(format!("ferros-grynn-query-{}", crate::values::Uuid::now_v7())),
			StoreBackend::Local(local) => local.import_scratch_root().to_path_buf(),
		};
		Self {
			inner: Arc::new(StoreInner {
				options,
				backend,
				next_transaction: AtomicU64::new(1),
				next_schema_id: AtomicU64::new(next_schema_id),
				next_graph_id: AtomicU64::new(next_graph_id),
				import_owner: AtomicU64::new(0),
				current: RwLock::new(Arc::new(root)),
				commit: Mutex::new(()),
				query_scratch_root,
			}),
		}
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
		let root = self.inner.current_root();
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
