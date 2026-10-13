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
	/// Background transactions retain storage without extending user ownership.
	owns_handle: bool,
}

#[cfg(test)]
mod ownership_tests {
	use super::*;

	#[test]
	fn event_background_transaction_does_not_extend_user_lifetime() {
		let store = Datastore::in_memory();
		let background = Datastore::background(Arc::clone(&store.inner));
		let transaction = background.begin();
		assert_eq!(background.inner.datastore_handles.load(Ordering::Acquire), 1);
		drop(store);
		assert_eq!(background.inner.datastore_handles.load(Ordering::Acquire), 0);
		assert!(background.inner.maintenance.is_stopping());
		drop(transaction);
		drop(background);
	}
}

impl Clone for Datastore {
	fn clone(&self) -> Self {
		if self.owns_handle
			&& self
				.inner
				.datastore_handles
				.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |handles| {
					handles.checked_add(1)
				})
				.is_err()
		{
			std::process::abort();
		}
		Self {
			inner: self.inner.clone(),
			owns_handle: self.owns_handle,
		}
	}
}

impl Drop for Datastore {
	fn drop(&mut self) {
		if self.owns_handle && self.inner.datastore_handles.fetch_sub(1, Ordering::AcqRel) == 1 {
			{
				let _commit = self.inner.commit.lock();
				self.inner.live_consumers.shutdown();
			}
			self.inner.live_consumers.wake_ready();
			self.inner.maintenance.shutdown();
		}
	}
}

impl Datastore {
	/// The owned worker checks shutdown and carries its existing cancellation
	/// token.
	pub(in crate::datastore) fn background(inner: Arc<StoreInner>) -> Self {
		Self {
			inner,
			owns_handle: false,
		}
	}

	/// Temporarily acquires a logical datastore owner for an attached control
	/// operation; a weak consumer cannot resurrect a closed datastore.
	pub(in crate::datastore) fn from_inner_alive(
		inner: Arc<StoreInner>,
	) -> FerrosGrynnResult<Self> {
		inner
			.datastore_handles
			.fetch_update(Ordering::AcqRel, Ordering::Acquire, |handles| {
				if handles == 0 {
					None
				} else {
					handles.checked_add(1)
				}
			})
			.map_err(|_handles| {
				crate::gql_status::FerrosGrynnError::invalid_argument("LIVE datastore is closed")
			})?;
		Ok(Self {
			inner,
			owns_handle: true,
		})
	}

	/// Registers and attaches one owned LIVE consumer atomically with
	/// activation.
	///
	/// # Errors
	///
	/// Returns registration, capture, capacity, conflict or publication errors.
	/// Historical starting positions replay proven source coverage, explicitly
	/// report unavailable inputs, then join the exact current tail.
	pub fn subscribe_live(
		&self,
		query: crate::live::LiveQuery,
		options: crate::live::LiveOptions,
	) -> FerrosGrynnResult<crate::live::LiveSubscription> {
		let mut tx = self.begin();
		let id = tx.register_live(query, options)?;
		self.attach_live_registration(tx, id)
	}

	/// Subscribes to an exact concrete graph family through common RETURN
	/// GRAPH.
	///
	/// # Errors
	/// Returns a recipe/family mismatch before registration, or existing LIVE
	/// registration, graph validation, resource and publication errors.
	pub fn subscribe_live_graph<
		G: crate::graph::LiveFamily<State = crate::graph::Frozen, Frozen = G> + Clone,
	>(
		&self,
		query: crate::live::LiveQuery,
		options: crate::live::LiveOptions,
	) -> FerrosGrynnResult<crate::live::LiveGraphSubscription<crate::graph::Frozen, G>> {
		let recipe = query.graph_result().ok_or_else(|| {
			crate::gql_status::FerrosGrynnError::invalid_argument(
				"LIVE graph subscription requires a graph-result recipe",
			)
		})?;
		if recipe.family() != G::family_kind() {
			return Err(crate::gql_status::FerrosGrynnError::invalid_argument(
				"LIVE graph recipe does not match the requested Rust family",
			));
		}
		let subscription = self.subscribe_live(query, options.clone())?;
		crate::live::LiveGraphSubscription::registered(subscription, options)
	}

	/// Restores the covered graph at confirmed ACK and then joins saved deltas.
	///
	/// # Errors
	/// Returns incarnation/family, coverage, graph validation or resource
	/// errors.
	pub fn resume_live_graph<
		G: crate::graph::LiveFamily<State = crate::graph::Frozen, Frozen = G> + Clone,
	>(
		&self,
		cursor: &crate::live::LiveCursor,
	) -> FerrosGrynnResult<crate::live::LiveGraphSubscription<crate::graph::Frozen, G>> {
		let options = {
			let _commit = self.inner.commit.lock();
			let root = self.inner.current_root();
			let definition =
				root.live_catalog().queries.get(&cursor.live_id()).ok_or_else(|| {
					crate::gql_status::FerrosGrynnError::invalid_argument(
						"LIVE graph resume incarnation is absent",
					)
				})?;
			let recipe = definition.query.graph_result().ok_or_else(|| {
				crate::gql_status::FerrosGrynnError::invalid_argument(
					"LIVE graph resume requires a graph-result recipe",
				)
			})?;
			if recipe.family() != G::family_kind() {
				return Err(crate::gql_status::FerrosGrynnError::invalid_argument(
					"LIVE graph resume family differs from the Rust family",
				));
			}
			definition.options.clone()
		};
		crate::live::LiveGraphSubscription::registered(self.resume_live(cursor)?, options)
	}

	/// Replaces this result incarnation with a fresh current-data snapshot,
	/// atomically closing its former capture during ordinary registration.
	///
	/// # Errors
	/// Returns the existing origin, recipe, family, capacity or publication
	/// errors. SDK materialization is checked after successful registration.
	pub fn resnapshot_live_graph<
		G: crate::graph::LiveFamily<State = crate::graph::Frozen, Frozen = G> + Clone,
	>(
		&self,
		cursor: &crate::live::LiveCursor,
	) -> FerrosGrynnResult<crate::live::LiveGraphSubscription<crate::graph::Frozen, G>> {
		let mut tx = self.begin();
		let info = tx.live_info(cursor.live_id())?;
		cursor.validate(info.cursor().database_id(), info.id(), info.cursor().version())?;
		let recipe = info.query().graph_result().ok_or_else(|| {
			crate::gql_status::FerrosGrynnError::invalid_argument(
				"resnapshot requires a graph recipe",
			)
		})?;
		if recipe.family() != G::family_kind() {
			return Err(crate::gql_status::FerrosGrynnError::invalid_argument(
				"resnapshot family mismatch",
			));
		}
		let options = info.options().clone().with_since(crate::live::LiveSince::Now);
		let id = tx.reregister_live(info.id(), info.query().clone(), options.clone())?;
		crate::live::LiveGraphSubscription::registered(
			self.attach_live_registration(tx, id)?,
			options,
		)
	}

	/// Resumes from the authoritative persisted ACK, then joins the live tail.
	/// A stale saved cursor uses the newer persisted ACK; an unconfirmed cursor
	/// above that watermark is rejected. Saved results never FETCH current
	/// data.
	///
	/// # Errors
	///
	/// Returns origin/query/version, unconfirmed-position, history/catalog,
	/// snapshot-pin or bounded capacity errors before attaching the consumer.
	///
	/// ```
	/// use ferros_grynn::{
	/// 	changefeed::{ChangefeedConfig, ChangefeedSource},
	/// 	datastore::{ClassSchema, Datastore},
	/// 	graph::AtomKind,
	/// 	live::{LiveNext, LiveOptions, LiveQuery, LiveSource},
	/// };
	/// # fn example() -> ferros_grynn::gql_status::FerrosGrynnResult<()> {
	/// let store = Datastore::in_memory();
	/// let mut tx = store.begin();
	/// let class = tx.define_class(ClassSchema::new("person", AtomKind::Node, [])?)?;
	/// let config = ChangefeedConfig::new("P1D".parse()?)?;
	/// tx.configure_changefeed(ChangefeedSource::Class(class), config)?;
	/// tx.commit()?;
	/// let query = LiveQuery::new(LiveSource::Class(class));
	/// let subscription = store.subscribe_live(query, LiveOptions::default())?;
	/// let saved = subscription.cursor().clone();
	/// drop(subscription);
	/// let mut resumed = store.resume_live(&saved)?;
	/// assert!(matches!(resumed.next()?, LiveNext::Idle));
	/// # Ok(()) }
	/// # example()?;
	/// # Ok::<(), ferros_grynn::gql_status::FerrosGrynnError>(())
	/// ```
	#[expect(
		clippy::tabs_in_doc_comments,
		reason = "Nightly rustfmt applies the project's required hard tabs to this Rustdoc example"
	)]
	pub fn resume_live(
		&self,
		cursor: &crate::live::LiveCursor,
	) -> FerrosGrynnResult<crate::live::LiveSubscription> {
		let _commit = self.inner.commit.lock();
		let root = self.inner.current_root();
		let definition = root.live_catalog().queries.get(&cursor.live_id()).ok_or_else(|| {
			crate::gql_status::FerrosGrynnError::invalid_argument(
				"LIVE resume incarnation is absent",
			)
		})?;
		cursor.validate(root.database_uuid.into(), definition.id, definition.version)?;
		if cursor.versionstamp().get() > definition.acknowledged {
			return Err(crate::gql_status::FerrosGrynnError::invalid_argument(
				"LIVE resume position was not confirmed",
			));
		}
		let confirmed = crate::live::LiveCursor::new(
			root.database_uuid.into(),
			definition.id,
			definition.version,
			definition.acknowledged.into(),
		)?;
		let physical = if root
			.live_segments()
			.get(&definition.id)
			.is_some_and(|segments| !segments.is_empty())
			|| root.live_catalog().graph_segments.contains_key(&definition.id)
		{
			self.inner.capture_changefeed_snapshot(root.commit_ts)?
		} else {
			None
		};
		let replay =
			super::live_queue::Replay::new(&root, &confirmed, &definition.options, physical)?;
		let has_work = replay.has_work();
		let key = self.inner.live_consumers.attach_replay(
			confirmed.clone(),
			definition.options.clone(),
			if has_work {
				replay.cutoff()
			} else {
				confirmed.versionstamp().get()
			},
			definition.state == crate::live::LiveState::Closed,
		)?;
		let checkpoint = root.live_catalog().graph_checkpoints.get(&definition.id).cloned();
		let handle = super::SubscriptionHandle::new(&self.inner, key, confirmed)
			.with_graph_checkpoint(checkpoint);
		let handle = if has_work {
			handle.with_replay(replay)
		} else {
			handle
		};
		Ok(crate::live::LiveSubscription {
			inner: handle,
		})
	}

	/// Closes an incarnation and registers its replacement in one commit.
	///
	/// # Errors
	///
	/// Returns source/capture validation, capacity, conflict or publication
	/// errors.
	pub fn reregister_live(
		&self,
		old: crate::live::LiveId,
		query: crate::live::LiveQuery,
		options: crate::live::LiveOptions,
	) -> FerrosGrynnResult<crate::live::LiveSubscription> {
		let mut tx = self.begin();
		let id = tx.reregister_live(old, query, options)?;
		self.attach_live_registration(tx, id)
	}

	/// Keeps pending-attachment cleanup outside the consuming commit call.
	fn attach_live_registration(
		&self,
		mut tx: Transaction,
		id: crate::live::LiveId,
	) -> FerrosGrynnResult<crate::live::LiveSubscription> {
		let mut subscription = self.prepare_live_attachment(&mut tx, id)?;
		tx.commit()?;
		subscription.inner.finish_registration()?;
		Ok(subscription)
	}

	/// Admits one pending consumer before publication; its RAII owner remains
	/// outside the consuming commit call, including every error return.
	pub(crate) fn prepare_live_attachment(
		&self,
		tx: &mut Transaction,
		id: crate::live::LiveId,
	) -> FerrosGrynnResult<crate::live::LiveSubscription> {
		let info = tx.live_info(id)?;
		let key = {
			let _commit = self.inner.commit.lock();
			self.inner.live_consumers.attach(
				info.cursor().clone(),
				info.options().clone(),
				false,
			)?
		};
		let handle = super::SubscriptionHandle::new(&self.inner, key, info.cursor().clone());
		Ok(crate::live::LiveSubscription {
			inner: handle,
		})
	}

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
			owns_handle: true,
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
				bridge_adapters: RwLock::new(std::collections::BTreeMap::new()),
				live_consumers: super::live_queue::Consumers::default(),
				query_scratch_root,
				quarantines: Mutex::new(std::collections::BTreeMap::new()),
				maintenance: MaintenanceCoordinator::new(recovered_jobs),
				event_processing_interval: super::event_worker::configured_interval(),
				changefeed_observation: super::changefeed_observation::Counters::default(),
				live_observation: super::live_observation::Counters::default(),
			}),
		}
	}

	/// Queues bounded retention passes on the existing maintenance worker.
	///
	/// ```
	/// use ferros_grynn::datastore::Datastore;
	/// # fn main() -> ferros_grynn::gql_status::FerrosGrynnResult<()> {
	/// let store = Datastore::in_memory();
	/// let job = store.purge_changefeeds(None)?;
	/// assert!(store.job(job)?.is_some());
	/// # Ok(())
	/// # }
	/// ```
	///
	/// # Errors
	///
	/// Returns worker, durable job publication or resource errors.
	pub fn purge_changefeeds(
		&self,
		options: Option<MaintenanceOptions>,
	) -> FerrosGrynnResult<super::JobId> {
		self.enqueue_maintenance_coalesced(
			MaintenanceJobKind::PurgeChangefeed,
			MaintenanceTarget::Database,
			options,
		)
	}

	/// Returns one durable maintenance job, if it exists.
	pub fn job(&self, id: super::JobId) -> FerrosGrynnResult<Option<MaintenanceJobInfo>> {
		if let Some(invocation) = self.inner.current_root().event_outbox().invocations.get(&id) {
			return self.inner.event_job_info(invocation).map(Some);
		}
		Ok(self.inner.maintenance.job(id))
	}

	/// Returns all retained active and terminal maintenance jobs.
	pub fn jobs(&self) -> FerrosGrynnResult<Vec<MaintenanceJobInfo>> {
		let mut jobs = self.inner.maintenance.jobs();
		for invocation in self.inner.current_root().event_outbox().invocations.values() {
			jobs.push(self.inner.event_job_info(invocation)?);
		}
		jobs.sort_unstable_by_key(MaintenanceJobInfo::id);
		Ok(jobs)
	}

	/// Cooperatively cancels one queued or running maintenance job.
	pub fn kill_job(&self, id: super::JobId) -> FerrosGrynnResult<MaintenanceJobInfo> {
		if self.inner.current_root().event_outbox().invocations.contains_key(&id) {
			return self.kill_event_job(id);
		}
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
			&& (root.schema.has_snapshot_indexes() || !root.changefeed_segments().is_empty()))
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
