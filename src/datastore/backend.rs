//! Backend ownership and resource-policy conversion for one MVCC store.

use std::{
	collections::BTreeMap,
	fs,
	num::NonZeroUsize,
	path::PathBuf,
	sync::{
		Arc,
		atomic::{AtomicU64, AtomicUsize, Ordering},
	},
};

use parking_lot::{Mutex, RwLock};

use super::{
	CheckpointMode,
	DatabaseRoot,
	DatastoreOptions,
	FunctionId,
	InMemoryOptions,
	IoMode,
	LocalOptions,
	LocalStorageStatus,
	MaintenanceCoordinator,
	MaintenanceJobInfo,
	MaintenanceJobKind,
	MaintenanceRunOutcome,
	MaintenanceTarget,
	QueryStatisticsDemand,
	SchemaAnalyzer,
	SchemaId,
	SchemaVersion,
};
use crate::{
	gql::PureFunctionEvaluator,
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	graph::{AtomKind, GraphId, GraphStorageMode},
	stats::StatisticsStore,
	storage::{
		LocalChangefeedSnapshot,
		LocalQuerySnapshot,
		LocalStore,
		PageIoMode,
		PhysicalIndexRead,
		PhysicalTraversalFieldsRead,
		PhysicalTraversalInnerRead,
		PhysicalTraversalRead,
		QuarantineArchiveReader,
		QuarantineArtifact,
	},
	values::{RecordId, Uuid, Vector, storage::DecodeContext},
};

/// Store inner state.
pub struct StoreInner {
	/// Options associated with this store inner.
	pub(crate) options: DatastoreOptions,
	/// Backend associated with this store inner.
	pub(crate) backend: StoreBackend,
	/// Next transaction associated with this store inner.
	pub(crate) next_transaction: AtomicU64,
	/// Next schema id associated with this store inner.
	pub(crate) next_schema_id: AtomicU64,
	/// Next graph id associated with this store inner.
	pub(crate) next_graph_id: AtomicU64,
	/// Import owner associated with this store inner.
	pub(crate) import_owner: AtomicU64,
	/// Datastore handles associated with this store inner.
	pub(crate) datastore_handles: AtomicUsize,
	/// Current associated with this store inner.
	pub(crate) current: RwLock<Arc<DatabaseRoot>>,
	/// Commit associated with this store inner.
	pub(crate) commit: Mutex<()>,
	/// Runtime-only clients are independent of durable mapping definitions.
	pub(crate) bridge_adapters:
		RwLock<BTreeMap<crate::bridge::BridgeDatabaseId, Arc<dyn crate::bridge::Bridge>>>,
	/// Transient whole-commit delivery authority, independent of MVCC roots.
	pub(in crate::datastore) live_consumers: super::live_queue::Consumers,
	/// Query scratch root associated with this store inner.
	pub(crate) query_scratch_root: PathBuf,
	/// Quarantines associated with this store inner.
	pub(crate) quarantines: Mutex<BTreeMap<Uuid, PathBuf>>,
	/// Maintenance associated with this store inner.
	pub(crate) maintenance: MaintenanceCoordinator,
	/// Positive millisecond setting captured once for accepted invocation
	/// scheduling.
	pub(in crate::datastore) event_processing_interval: std::time::Duration,
	/// Diagnostic capture/publication counts; no per-source metric labels.
	pub(in crate::datastore) changefeed_observation: super::changefeed_observation::Counters,
	/// Non-authoritative LIVE process counters; no per-query labels or
	/// payloads.
	pub(in crate::datastore) live_observation: super::live_observation::Counters,
}

/// Store backend alternatives.
pub enum StoreBackend {
	/// In memory case of store backend.
	InMemory,
	/// Local case of store backend.
	Local(
		/// Local case of store backend.
		Box<LocalStore>,
	),
}

impl StoreInner {
	/// Returns the allocate schema id result.
	pub(crate) fn allocate_schema_id(&self) -> FerrosGrynnResult<SchemaId> {
		let raw = self
			.next_schema_id
			.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
				(current != 0 && current < u64::from(u32::MAX)).then_some(current + 1)
			})
			.map_err(|_current| {
				FerrosGrynnError::invalid_argument("schema identity clock is exhausted")
			})?;
		let raw = u32::try_from(raw).map_err(|_error| {
			FerrosGrynnError::invalid_argument("schema identity clock is exhausted")
		})?;
		SchemaId::new(raw)
			.ok_or_else(|| FerrosGrynnError::invalid_argument("schema identity is reserved"))
	}

	/// Returns the allocate graph id result.
	pub(crate) fn allocate_graph_id(&self) -> FerrosGrynnResult<GraphId> {
		let raw = self
			.next_graph_id
			.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| current.checked_add(1))
			.map_err(|_current| {
				FerrosGrynnError::invalid_argument("graph identity clock is exhausted")
			})?;
		GraphId::new(raw)
			.ok_or_else(|| FerrosGrynnError::invalid_argument("graph identity zero is reserved"))
	}

	/// Returns the current root result.
	pub(crate) fn current_root(&self) -> Arc<DatabaseRoot> {
		self.current.read().clone()
	}

	/// Returns the transaction limits result.
	pub(crate) const fn transaction_limits(&self) -> InMemoryOptions {
		match self.options {
			DatastoreOptions::InMemory(options) => options,
			DatastoreOptions::Local(options) => InMemoryOptions {
				max_snapshot_age: options.max_snapshot_age,
				max_write_set_bytes: options.max_write_set_bytes,
				maintenance: options.maintenance,
			},
		}
	}

	/// Returns the graph storage mode result.
	pub(crate) const fn graph_storage_mode(&self) -> GraphStorageMode {
		match &self.backend {
			StoreBackend::InMemory => GraphStorageMode::InMemory,
			StoreBackend::Local(_) => GraphStorageMode::DiskBacked,
		}
	}

	/// Returns the persist root result.
	pub(crate) fn persist_root(
		&self,
		root: &mut DatabaseRoot,
		previous: Option<&DatabaseRoot>,
		changed_records: Option<&std::collections::BTreeSet<RecordId>>,
		graph_catalog_only: bool,
		dictionary_changed: bool,
	) -> FerrosGrynnResult<()> {
		// Transaction preparation has already frozen the time used by source
		// and LIVE envelopes. Only maintenance callers assign it here.
		if previous.is_none() {
			root.advance_commit_time(jiff::Timestamp::now().as_nanosecond());
		}
		let observation = super::changefeed_observation::Publication::capture(root);
		let current = previous.is_none().then(|| self.current_root());
		let live_observation =
			super::live_observation::Publication::capture(root, previous.or(current.as_deref()));
		let result = match &self.backend {
			StoreBackend::InMemory => {
				root.resident_geometry = Arc::new(crate::storage::build_resident_geometry_indexes(
					previous,
					root,
					changed_records,
				)?);
				Ok(())
			}
			StoreBackend::Local(local) => {
				if !root.bridges.databases.is_empty() {
					let _bridge = Arc::make_mut(&mut root.bridges);
				}
				// Detach outer COW and allocate empty RAM tables before the WAL
				// barrier. The post-barrier path only installs owned handles.
				let empty_history = Arc::default();
				let empty_live_history = Arc::default();
				let _history = root.change_history_mut();
				let evaluator = PureFunctionEvaluator::new(root);
				let mut analyze =
					|analyzer: &SchemaAnalyzer, input: &str| evaluator.analyze(analyzer, input);
				let mut distance = |function: FunctionId,
				                    version: SchemaVersion,
				                    left: &Vector,
				                    right: &Vector| {
					evaluator.distance(function, version, left, right)
				};
				let maintenance = match self.options {
					DatastoreOptions::Local(options) => options.maintenance,
					DatastoreOptions::InMemory(options) => options.maintenance,
				};
				crate::storage::with_external_sort_options(
					maintenance.max_memory_bytes,
					maintenance.max_parallelism,
					|| {
						local.persist_root(
							root,
							previous,
							changed_records,
							graph_catalog_only,
							dictionary_changed,
							&mut analyze,
							&mut distance,
						)
					},
				)?;
				*root.changefeed_segments_mut() = local.changefeed_segments();
				if !root.bridges.databases.is_empty() {
					Arc::make_mut(&mut root.bridges).physical = local.bridge_binding();
				}
				*root.change_history_mut() = empty_history;
				*root.live_segments_mut() = local.live_segments();
				std::sync::Arc::make_mut(root.live_catalog_mut()).graph_segments =
					local.live_graph_segments();
				root.set_event_outbox_segments(local.event_outbox_segments());
				*root.live_history_mut() = empty_live_history;
				Ok(())
			}
		};
		if result.is_ok() {
			self.changefeed_observation.published(observation);
			self.live_observation.published(live_observation);
		}
		result
	}

	/// Returns the persist import root result.
	pub(crate) fn persist_import_root(
		&self,
		root: &mut DatabaseRoot,
		quarantine: Option<&mut QuarantineArtifact>,
	) -> FerrosGrynnResult<BTreeMap<super::IndexId, std::time::Duration>> {
		if !root.bridges.databases.is_empty() {
			let _bridge = Arc::make_mut(&mut root.bridges);
		}
		// END preparation freezes the final stamp/time before encoding any
		// source or computed result envelope, including repeated UTC times.
		let observation = super::changefeed_observation::Publication::capture(root);
		let current = self.current_root();
		let live_observation = super::live_observation::Publication::capture(root, Some(&current));
		let result = match &self.backend {
			StoreBackend::InMemory => {
				root.resident_geometry =
					Arc::new(crate::storage::build_resident_geometry_indexes(None, root, None)?);
				if let Some(artifact) = quarantine {
					let id = artifact.id();
					let path = artifact.install(&self.query_scratch_root)?;
					artifact.publish();
					self.quarantines.lock().insert(id, path);
				}
				Ok(BTreeMap::new())
			}
			StoreBackend::Local(local) => {
				let empty_history = Arc::default();
				let empty_live_history = Arc::default();
				let _history = root.change_history_mut();
				let evaluator = PureFunctionEvaluator::new(root);
				let mut analyze =
					|analyzer: &SchemaAnalyzer, input: &str| evaluator.analyze(analyzer, input);
				let mut distance = |function: FunctionId,
				                    version: SchemaVersion,
				                    left: &Vector,
				                    right: &Vector| {
					evaluator.distance(function, version, left, right)
				};
				let maintenance = match self.options {
					DatastoreOptions::Local(options) => options.maintenance,
					DatastoreOptions::InMemory(options) => options.maintenance,
				};
				let timings = crate::storage::with_external_sort_options(
					maintenance.max_memory_bytes,
					maintenance.max_parallelism,
					|| local.persist_import_root(root, quarantine, &mut analyze, &mut distance),
				)?;
				if !root.bridges.databases.is_empty() {
					Arc::make_mut(&mut root.bridges).physical = local.bridge_binding();
				}
				*root.changefeed_segments_mut() = local.changefeed_segments();
				*root.change_history_mut() = empty_history;
				*root.live_segments_mut() = local.live_segments();
				std::sync::Arc::make_mut(root.live_catalog_mut()).graph_segments =
					local.live_graph_segments();
				root.set_event_outbox_segments(local.event_outbox_segments());
				*root.live_history_mut() = empty_live_history;
				Ok(timings)
			}
		};
		if result.is_ok() {
			self.changefeed_observation.published(observation);
			self.live_observation.published(live_observation);
		}
		result
	}

	/// Opens quarantine.
	pub(crate) fn open_quarantine(&self, id: Uuid) -> FerrosGrynnResult<QuarantineArchiveReader> {
		match &self.backend {
			StoreBackend::InMemory => {
				let path = self.quarantines.lock().get(&id).cloned().ok_or_else(|| {
					FerrosGrynnError::invalid_argument("quarantine identity does not exist")
				})?;
				QuarantineArchiveReader::open(&path)
			}
			StoreBackend::Local(local) => local.open_quarantine(id),
		}
	}

	/// Removes quarantine.
	pub(crate) fn remove_quarantine(&self, id: Uuid) -> FerrosGrynnResult<()> {
		match &self.backend {
			StoreBackend::InMemory => {
				let path = self.quarantines.lock().remove(&id).ok_or_else(|| {
					FerrosGrynnError::invalid_argument("quarantine identity does not exist")
				})?;
				match fs::remove_file(path) {
					Ok(()) => Ok(()),
					Err(source) if source.kind() == std::io::ErrorKind::NotFound => Ok(()),
					Err(source) => {
						Err(FerrosGrynnError::storage_io_failure("remove import quarantine")
							.with_technical_source(source))
					}
				}
			}
			StoreBackend::Local(local) => {
				let _commit = self.commit.lock();
				let snapshot = self.current_root();
				let mut next = (*snapshot).clone();
				next.commit_ts = snapshot.commit_ts.checked_add(1).ok_or_else(|| {
					FerrosGrynnError::invalid_argument(
						"quarantine removal commit clock is exhausted",
					)
				})?;
				next.advance_commit_time(jiff::Timestamp::now().as_nanosecond());
				let evaluator = PureFunctionEvaluator::new(&next);
				let mut analyze =
					|analyzer: &SchemaAnalyzer, input: &str| evaluator.analyze(analyzer, input);
				let mut distance = |function: FunctionId,
				                    version: SchemaVersion,
				                    left: &Vector,
				                    right: &Vector| {
					evaluator.distance(function, version, left, right)
				};
				local.remove_quarantine(&next, id, &mut analyze, &mut distance)?;
				*self.current.write() = Arc::new(next);
				Ok(())
			}
		}
	}

	/// Returns the import scratch root result.
	pub(crate) fn import_scratch_root(&self) -> std::path::PathBuf {
		match &self.backend {
			StoreBackend::InMemory => std::env::temp_dir().join("ferros-grynn-import"),
			StoreBackend::Local(local) => local.import_scratch_root().to_path_buf(),
		}
	}

	/// Returns the persist maintenance jobs result.
	pub(crate) fn persist_maintenance_jobs(
		&self,
		jobs: &[MaintenanceJobInfo],
	) -> FerrosGrynnResult<()> {
		match &self.backend {
			StoreBackend::InMemory => Ok(()),
			StoreBackend::Local(local) => local.persist_maintenance_jobs(jobs),
		}
	}

	/// Returns whether the maintenance publication matches condition holds.
	pub(crate) fn maintenance_publication_matches(&self, job: &MaintenanceJobInfo) -> bool {
		let Some(fingerprint) = job.candidate_fingerprint else {
			return false;
		};
		let root = self.current_root();
		match job.target {
			MaintenanceTarget::GraphIndex {
				graph,
				index,
				version,
			} => root.schema.graph(graph).is_some_and(|definition| {
				definition.edge_endpoint_index().is_some_and(|candidate| {
					candidate.id() == index
						&& candidate.version() == version
						&& root.commit_ts == fingerprint
				}) || definition.indexes().any(|candidate| {
					candidate.id() == Some(index)
						&& candidate.version() == version
						&& candidate.state() == super::ClassIndexState::Ready
						&& candidate.build_revision() == fingerprint
				})
			}),
			MaintenanceTarget::Index {
				class,
				index,
				version,
			} => root.schema.class_by_id(class).is_some_and(|definition| {
				definition.indexes().any(|candidate| {
					candidate.id() == Some(index)
						&& candidate.version() == version
						&& candidate.state() == super::ClassIndexState::Ready
						&& candidate.build_revision() == fingerprint
				})
			}),
			MaintenanceTarget::Graph(graph) => {
				root.commit_ts == fingerprint && root.graph(graph).is_some()
			}
			MaintenanceTarget::BaseClass(class) => {
				root.commit_ts == fingerprint && root.schema.class_by_id(class).is_some()
			}
			MaintenanceTarget::EffectiveClass {
				class,
				graph,
			} => {
				root.commit_ts == fingerprint
					&& root.schema.class_by_id(class).is_some()
					&& root.graph(graph).is_some()
			}
			MaintenanceTarget::Database => root.commit_ts == fingerprint,
		}
	}

	/// Returns whether the maintenance target exists condition holds.
	pub(crate) fn maintenance_target_exists(&self, target: MaintenanceTarget) -> bool {
		let root = self.current_root();
		match target {
			MaintenanceTarget::GraphIndex {
				graph,
				index,
				version,
			} => root.schema.graph(graph).is_some_and(|definition| {
				definition.edge_endpoint_index().is_some_and(|candidate| {
					candidate.id() == index && candidate.version() == version
				}) || definition.indexes().any(|candidate| {
					candidate.id() == Some(index) && candidate.version() == version
				})
			}),
			MaintenanceTarget::Database => true,
			MaintenanceTarget::Graph(graph) => root.graph(graph).is_some(),
			MaintenanceTarget::BaseClass(class) => root.schema.class_by_id(class).is_some(),
			MaintenanceTarget::EffectiveClass {
				class,
				graph,
			} => root.schema.class_by_id(class).is_some() && root.graph(graph).is_some(),
			MaintenanceTarget::Index {
				class,
				index,
				version,
			} => root.schema.class_by_id(class).is_some_and(|definition| {
				definition.indexes().any(|candidate| {
					candidate.id() == Some(index) && candidate.version() == version
				})
			}),
		}
	}

	/// Resolves analyze mode.
	pub(crate) fn resolve_analyze_mode(
		&self,
		target: MaintenanceTarget,
		mode: super::MaintenanceAnalyzeMode,
	) -> FerrosGrynnResult<super::MaintenanceAnalyzeMode> {
		if mode != super::MaintenanceAnalyzeMode::Automatic {
			return Ok(mode);
		}
		let root = self.current_root();
		let visible_hot_pages = match target {
			MaintenanceTarget::Database => root.graphs.values().fold(
				crate::storage::atom_hot_page_count(root.atoms.slot_capacity()),
				|pages, graph| {
					pages.saturating_add(crate::storage::graph_hot_page_count(
						graph.slot_capacity(AtomKind::Node),
						graph.slot_capacity(AtomKind::Edge),
					))
				},
			),
			MaintenanceTarget::BaseClass(class) => {
				if root.schema.class_by_id(class).is_none() {
					return Err(FerrosGrynnError::class_does_not_exist(class.to_string()));
				}
				crate::storage::atom_hot_page_count(root.atoms.slot_capacity())
			}
			MaintenanceTarget::Graph(graph)
			| MaintenanceTarget::EffectiveClass {
				graph,
				..
			} => {
				let graph = root
					.graph(graph)
					.ok_or_else(|| FerrosGrynnError::graph_does_not_exist(graph.to_string()))?;
				crate::storage::graph_hot_page_count(
					graph.slot_capacity(AtomKind::Node),
					graph.slot_capacity(AtomKind::Edge),
				)
			}
			MaintenanceTarget::Index {
				..
			}
			| MaintenanceTarget::GraphIndex {
				..
			} => {
				return Err(FerrosGrynnError::invalid_argument(
					"ANALYZE does not accept an index maintenance target",
				));
			}
		};
		Ok(if visible_hot_pages <= 32 {
			super::MaintenanceAnalyzeMode::Full
		} else {
			super::MaintenanceAnalyzeMode::Approximate {
				sample_count: 30_000,
			}
		})
	}

	/// Returns the run maintenance job result.
	pub(crate) fn run_maintenance_job(
		&self,
		id: super::JobId,
	) -> FerrosGrynnResult<MaintenanceRunOutcome> {
		self.run_maintenance_job_inner(id, || Ok(()))
	}

	/// Returns run maintenance job interleaved for `StoreInner`.
	#[cfg(test)]
	pub(crate) fn run_maintenance_job_interleaved(
		&self,
		id: super::JobId,
		interleave: impl FnOnce() -> FerrosGrynnResult<()>,
	) -> FerrosGrynnResult<MaintenanceRunOutcome> {
		self.run_maintenance_job_inner(id, interleave)
	}

	/// Returns the run maintenance job inner result.
	fn run_maintenance_job_inner(
		&self,
		id: super::JobId,
		interleave: impl FnOnce() -> FerrosGrynnResult<()>,
	) -> FerrosGrynnResult<MaintenanceRunOutcome> {
		if self.maintenance.is_cancelled(id) {
			return Ok(MaintenanceRunOutcome::Cancelled);
		}
		let job = self
			.maintenance
			.job(id)
			.ok_or_else(|| FerrosGrynnError::maintenance_job_not_found(id.to_string()))?;
		let cancellation = self.maintenance.cancellation_token(id);
		let result = crate::storage::with_external_sort_control(
			job.max_memory_bytes,
			job.max_parallelism,
			cancellation.clone(),
			|| {
				let snapshot = self.current_root();
				let publication = snapshot.commit_ts.checked_add(1).ok_or_else(|| {
					FerrosGrynnError::invalid_argument("maintenance commit clock is exhausted")
				})?;
				let candidate = self.build_maintenance_candidate(
					&snapshot,
					job.kind,
					job.target,
					publication,
					job.analyze_mode,
				);
				let mut next = match candidate {
					Err(_error)
						if job.kind == MaintenanceJobKind::PurgeChangefeed
							&& self.current_root().commit_ts != snapshot.commit_ts =>
					{
						return Ok(MaintenanceRunOutcome::Retry);
					}
					other => other?,
				};
				if job.kind == MaintenanceJobKind::PurgeChangefeed
					&& next.changefeeds() == snapshot.changefeeds()
					&& next.live_catalog().histories == snapshot.live_catalog().histories
				{
					return Ok(MaintenanceRunOutcome::Completed);
				}
				let metrics = crate::storage::external_sort_metrics();
				self.maintenance.stage_progress(
					id,
					maintenance_source_items(&next, job.target).max(metrics.processed_items),
					metrics.written_bytes,
					metrics.runs,
				)?;
				if self.maintenance.is_cancelled(id) {
					return Ok(MaintenanceRunOutcome::Cancelled);
				}
				interleave()?;
				if self.maintenance.has_pending_index_change_after(job.target, snapshot.commit_ts) {
					return Ok(MaintenanceRunOutcome::Retry);
				}
				if !self.maintenance.mark_publishing(self, id, publication)? {
					return Ok(MaintenanceRunOutcome::Cancelled);
				}
				let _commit = self.commit.lock();
				if self.current_root().commit_ts != snapshot.commit_ts {
					return Ok(MaintenanceRunOutcome::Retry);
				}
				if self.maintenance.is_cancelled(id) {
					return Ok(MaintenanceRunOutcome::Cancelled);
				}
				self.ensure_write_allowed(0)?;
				next.commit_ts = publication;
				if job.kind == MaintenanceJobKind::PurgeChangefeed {
					next.advance_commit_time(jiff::Timestamp::now().as_nanosecond());
				}
				self.persist_root(
					&mut next,
					None,
					None,
					job.kind == MaintenanceJobKind::PurgeChangefeed,
					false,
				)?;
				*self.current.write() = Arc::new(next);
				self.maintenance.acknowledge_index_changes(job.target, snapshot.commit_ts);
				Ok(MaintenanceRunOutcome::Completed)
			},
		);
		if cancellation.load(std::sync::atomic::Ordering::Acquire) {
			Ok(MaintenanceRunOutcome::Cancelled)
		} else {
			result
		}
	}

	/// Runs the same concrete builder in the caller thread without creating a
	/// durable background job.
	pub(crate) fn run_foreground_maintenance(
		&self,
		kind: MaintenanceJobKind,
		target: MaintenanceTarget,
		options: super::MaintenanceOptions,
		analyze_mode: Option<super::MaintenanceAnalyzeMode>,
	) -> FerrosGrynnResult<()> {
		crate::storage::with_external_sort_options(
			options.max_memory_bytes,
			options.max_parallelism,
			|| {
				let snapshot = self.current_root();
				let publication = snapshot.commit_ts.checked_add(1).ok_or_else(|| {
					FerrosGrynnError::invalid_argument("maintenance commit clock is exhausted")
				})?;
				let mut next = self.build_maintenance_candidate(
					&snapshot,
					kind,
					target,
					publication,
					analyze_mode,
				)?;
				if kind == MaintenanceJobKind::PurgeChangefeed
					&& next.changefeeds() == snapshot.changefeeds()
					&& next.live_catalog().histories == snapshot.live_catalog().histories
				{
					return Ok(());
				}
				let _commit = self.commit.lock();
				if self.current_root().commit_ts != snapshot.commit_ts {
					return Err(FerrosGrynnError::maintenance_revision_race(target.display_name()));
				}
				self.ensure_write_allowed(0)?;
				next.commit_ts = publication;
				if kind == MaintenanceJobKind::PurgeChangefeed {
					next.advance_commit_time(jiff::Timestamp::now().as_nanosecond());
				}
				self.persist_root(
					&mut next,
					None,
					None,
					kind == MaintenanceJobKind::PurgeChangefeed,
					false,
				)?;
				*self.current.write() = Arc::new(next);
				Ok(())
			},
		)
	}

	/// Builds maintenance candidate.
	fn build_maintenance_candidate(
		&self,
		snapshot: &DatabaseRoot,
		kind: MaintenanceJobKind,
		target: MaintenanceTarget,
		build_revision: u64,
		analyze_mode: Option<super::MaintenanceAnalyzeMode>,
	) -> FerrosGrynnResult<DatabaseRoot> {
		if kind == MaintenanceJobKind::PurgeChangefeed {
			if target != MaintenanceTarget::Database {
				return Err(FerrosGrynnError::invalid_argument(
					"changefeed purge requires a database target",
				));
			}
			return self.purge_changefeed_candidate(snapshot, build_revision);
		}
		let mut next = snapshot.clone();
		match kind {
			MaintenanceJobKind::EventInvocation => {
				return Err(FerrosGrynnError::invalid_argument(
					"EVENT effects require an accepted invocation transaction",
				));
			}
			MaintenanceJobKind::BuildIndex
			| MaintenanceJobKind::RebuildIndex
			| MaintenanceJobKind::DeferredIndexCatchUp
			| MaintenanceJobKind::CompactIndex => {
				prepare_index_maintenance(
					&mut next,
					target,
					build_revision,
					&self.import_scratch_root().join("maintenance"),
				)?;
				next.rebuild_indexes()?;
			}
			MaintenanceJobKind::Analyze => {
				self.hydrate_statistics(&mut next)?;
				next.rebuild_statistics_with_mode_at(
					&self.import_scratch_root().join("maintenance"),
					analyze_mode.ok_or_else(|| {
						FerrosGrynnError::invalid_argument(
							"ANALYZE maintenance is missing its resolved build mode",
						)
					})?,
					target,
				)?;
			}
			MaintenanceJobKind::CompactGraph | MaintenanceJobKind::CompactDatabase => {}
			MaintenanceJobKind::PurgeChangefeed => {}
		}
		next.validate_full_import_state()?;
		Ok(next)
	}

	/// Returns the query scratch root result.
	pub(crate) fn query_scratch_root(&self) -> std::path::PathBuf {
		self.query_scratch_root.clone()
	}

	/// Retries physical unlinks after old readers release their file pins.
	pub(super) fn sweep_retired_changefeeds(&self) {
		if let StoreBackend::Local(local) = &self.backend {
			local.sweep_retired_changefeeds();
		}
	}

	/// Captures physical query snapshot.
	pub(crate) fn capture_physical_query_snapshot(
		&self,
		commit_ts: u64,
	) -> FerrosGrynnResult<Option<LocalQuerySnapshot>> {
		match &self.backend {
			StoreBackend::InMemory => Ok(None),
			StoreBackend::Local(local) => local.capture_query_snapshot(commit_ts),
		}
	}

	/// Pins only feed handles and exact visibility without scanning graph
	/// bindings.
	pub(crate) fn capture_changefeed_snapshot(
		&self,
		stamp: u64,
	) -> FerrosGrynnResult<Option<LocalChangefeedSnapshot>> {
		match &self.backend {
			StoreBackend::InMemory => Ok(None),
			StoreBackend::Local(local) => local.capture_changefeed_snapshot(stamp),
		}
	}

	/// Performs allocation-free coverage/seek metadata reads in a pinned
	/// snapshot.
	pub(crate) fn inspect_live_group(
		&self,
		snapshot: &LocalChangefeedSnapshot,
		segment: crate::storage::HistorySegment,
		first: u64,
		memory_bytes: usize,
	) -> FerrosGrynnResult<Option<(crate::storage::live_history::LiveGroupInfo, u64)>> {
		let before = snapshot.read_observation().0;
		let result = match &self.backend {
			StoreBackend::Local(local) => {
				local.inspect_live_group(snapshot, segment, first, memory_bytes)
			}
			StoreBackend::InMemory => Err(FerrosGrynnError::invalid_argument(
				"physical LIVE read requires a local backend",
			)),
		};
		self.live_observation.page_reads(snapshot.read_observation().0.saturating_sub(before));
		result
	}

	/// Restores captured result images without touching current primary data.
	pub(crate) fn read_live_payload(
		&self,
		snapshot: &LocalChangefeedSnapshot,
		segment: crate::storage::HistorySegment,
		first: u64,
		memory_bytes: usize,
	) -> FerrosGrynnResult<
		Option<(crate::live::LivePayload, crate::storage::live_history::LiveGroupInfo, u64)>,
	> {
		let before = snapshot.read_observation().0;
		let result = match &self.backend {
			StoreBackend::Local(local) => {
				local.read_live_payload(snapshot, segment, first, memory_bytes)
			}
			StoreBackend::InMemory => Err(FerrosGrynnError::invalid_argument(
				"physical LIVE read requires a local backend",
			)),
		};
		self.live_observation.page_reads(snapshot.read_observation().0.saturating_sub(before));
		result
	}

	/// Performs allocation-free source coverage/seek metadata reads.
	pub(crate) fn inspect_changefeed_group(
		&self,
		snapshot: &LocalChangefeedSnapshot,
		segment: crate::storage::HistorySegment,
		first: u64,
		memory_bytes: usize,
	) -> FerrosGrynnResult<Option<crate::storage::ChangefeedGroupInfo>> {
		match &self.backend {
			StoreBackend::Local(local) => {
				local.inspect_changefeed_group(snapshot, segment, first, memory_bytes)
			}
			StoreBackend::InMemory => Err(FerrosGrynnError::invalid_argument(
				"physical changefeed read requires a local backend",
			)),
		}
	}

	/// Restores one owned whole commit or unavailable segment under catalog
	/// bounds.
	pub(crate) fn read_changefeed_batch(
		&self,
		snapshot: &LocalChangefeedSnapshot,
		segment: crate::storage::HistorySegment,
		first: u64,
		memory_bytes: usize,
	) -> FerrosGrynnResult<Option<(super::CapturedCommit, u64)>> {
		match &self.backend {
			StoreBackend::Local(local) => {
				local.read_changefeed_batch(snapshot, segment, first, memory_bytes)
			}
			StoreBackend::InMemory => Err(FerrosGrynnError::invalid_argument(
				"physical changefeed read requires a local backend",
			)),
		}
	}

	/// Returns whether this value has physical index.
	pub(crate) fn has_physical_index(
		&self,
		snapshot: &LocalQuerySnapshot,
		index: super::IndexId,
	) -> bool {
		match &self.backend {
			StoreBackend::InMemory => false,
			StoreBackend::Local(_local) => LocalStore::has_physical_index(snapshot, index),
		}
	}

	/// Returns the physical index result.
	pub(crate) fn physical_index(
		&self,
		snapshot: &LocalQuerySnapshot,
		index: super::IndexId,
	) -> FerrosGrynnResult<Option<Arc<PhysicalIndexRead>>> {
		match &self.backend {
			StoreBackend::InMemory => Ok(None),
			StoreBackend::Local(local) => local.physical_index(snapshot, index),
		}
	}

	/// Returns the search diskann index result.
	pub(crate) fn search_diskann_index(
		&self,
		snapshot: &LocalQuerySnapshot,
		index: super::IndexId,
		query: &crate::values::Vector,
		k: usize,
		breadth: usize,
	) -> FerrosGrynnResult<Option<Vec<crate::storage::VectorSearchHit>>> {
		match &self.backend {
			StoreBackend::InMemory => Ok(None),
			StoreBackend::Local(local) => {
				local.search_diskann_index(snapshot, index, query, k, breadth)
			}
		}
	}

	/// Returns the search geometry index result.
	pub(crate) fn search_geometry_index(
		&self,
		snapshot: &LocalQuerySnapshot,
		index: super::IndexId,
		mbr: [f64; 4],
		admit: &mut dyn FnMut(usize) -> FerrosGrynnResult<()>,
	) -> FerrosGrynnResult<Option<crate::storage::GeometryCandidates>> {
		match &self.backend {
			StoreBackend::InMemory => Ok(None),
			StoreBackend::Local(local) => local.search_geometry_index(snapshot, index, mbr, admit),
		}
	}

	/// Returns the search hnsw index result.
	pub(crate) fn search_hnsw_index(
		&self,
		snapshot: &LocalQuerySnapshot,
		index: super::IndexId,
		query: &crate::values::Vector,
		k: usize,
		breadth: usize,
		distance_function: &mut dyn FnMut(
			super::FunctionId,
			super::SchemaVersion,
			&crate::values::Vector,
			&crate::values::Vector,
		) -> FerrosGrynnResult<f64>,
	) -> FerrosGrynnResult<Option<Vec<crate::storage::VectorSearchHit>>> {
		match &self.backend {
			StoreBackend::InMemory => Ok(None),
			StoreBackend::Local(local) => {
				local.search_hnsw_index(snapshot, index, query, k, breadth, distance_function)
			}
		}
	}

	/// Loads planner statistics.
	pub(crate) fn load_planner_statistics(
		&self,
		snapshot: &LocalQuerySnapshot,
		database: &DatabaseRoot,
		demand: &QueryStatisticsDemand,
		statistics: &mut StatisticsStore,
	) {
		if let StoreBackend::Local(local) = &self.backend {
			local.load_planner_statistics(snapshot, database, demand, statistics);
		}
	}

	/// Returns the hydrate statistics result.
	pub(crate) fn hydrate_statistics(&self, database: &mut DatabaseRoot) -> FerrosGrynnResult<()> {
		if !database.statistics.auxiliary_deferred() {
			return Ok(());
		}
		match &self.backend {
			StoreBackend::InMemory => Err(FerrosGrynnError::invalid_storage_file(
				"in-memory root unexpectedly contains deferred statistics",
			)),
			StoreBackend::Local(local) => local.hydrate_statistics(database),
		}
	}

	#[allow(
		clippy::too_many_arguments,
		reason = "the local frontier contract keeps graph, source, and direction axes explicit"
	)]
	/// Returns the physical traversal expand result.
	pub(crate) fn physical_traversal_expand(
		&self,
		snapshot: &LocalQuerySnapshot,
		graph: GraphId,
		source: crate::graph::AtomId,
		forward: bool,
		backward: bool,
		undirected: bool,
	) -> FerrosGrynnResult<Option<PhysicalTraversalRead>> {
		match &self.backend {
			StoreBackend::InMemory => Ok(None),
			StoreBackend::Local(local) => local
				.physical_traversal_expand(snapshot, graph, source, forward, backward, undirected),
		}
	}

	/// Returns the physical traversal inner graph result.
	pub(crate) fn physical_traversal_inner_graph(
		&self,
		snapshot: &LocalQuerySnapshot,
		graph: GraphId,
		owner: crate::graph::AtomId,
	) -> FerrosGrynnResult<Option<PhysicalTraversalInnerRead>> {
		match &self.backend {
			StoreBackend::InMemory => Ok(None),
			StoreBackend::Local(local) => {
				local.physical_traversal_inner_graph(snapshot, graph, owner)
			}
		}
	}

	/// Returns the physical traversal fields result.
	pub(crate) fn physical_traversal_fields(
		&self,
		snapshot: &LocalQuerySnapshot,
		graph: GraphId,
		owner: crate::graph::AtomId,
		context: &DecodeContext<'_>,
	) -> FerrosGrynnResult<Option<PhysicalTraversalFieldsRead>> {
		match &self.backend {
			StoreBackend::InMemory => Ok(None),
			StoreBackend::Local(local) => {
				local.physical_traversal_fields(snapshot, graph, owner, context)
			}
		}
	}

	/// Returns the checkpoint result.
	pub(crate) fn checkpoint(&self, mode: CheckpointMode) -> FerrosGrynnResult<bool> {
		match &self.backend {
			StoreBackend::InMemory => Ok(false),
			StoreBackend::Local(local) => local.checkpoint(mode),
		}
	}

	/// Returns the local storage status result.
	pub(crate) fn local_storage_status(&self) -> Option<LocalStorageStatus> {
		match (&self.backend, self.options) {
			(StoreBackend::Local(local), DatastoreOptions::Local(options)) => {
				Some(local.runtime_status(options))
			}
			_ => None,
		}
	}

	#[cfg(feature = "storage-engine-prototype-api")]
	/// Clears unpinned resident pages for an isolated storage measurement.
	pub(crate) fn clear_resident_for_prototype(&self) -> FerrosGrynnResult<usize> {
		match &self.backend {
			StoreBackend::InMemory => Ok(0),
			StoreBackend::Local(local) => local.clear_resident_for_prototype(),
		}
	}
}

impl Drop for StoreInner {
	/// Runs cleanup for this `StoreInner` before its fields are released.
	fn drop(&mut self) {
		self.maintenance.shutdown();
		if matches!(self.backend, StoreBackend::InMemory) {
			drop(fs::remove_dir_all(&self.query_scratch_root));
		}
	}
}

/// Prepares index maintenance.
fn prepare_index_maintenance(
	database: &mut DatabaseRoot,
	target: MaintenanceTarget,
	build_revision: u64,
	sort_root: &std::path::Path,
) -> FerrosGrynnResult<()> {
	if let MaintenanceTarget::GraphIndex {
		graph,
		index,
		version,
	} = target
	{
		let definition = database
			.schema
			.graph(graph)
			.ok_or_else(|| FerrosGrynnError::graph_does_not_exist(graph.to_string()))?;
		if definition
			.edge_endpoint_index()
			.is_some_and(|candidate| candidate.id() == index && candidate.version() == version)
		{
			return Ok(());
		}
		let mut indexes = definition.indexes().cloned().collect::<Vec<_>>();
		let candidate =
			indexes.iter_mut().find(|candidate| candidate.id() == Some(index)).ok_or_else(
				|| FerrosGrynnError::invalid_argument("graph maintenance index is absent"),
			)?;
		if candidate.version() != version {
			return Err(FerrosGrynnError::invalid_argument(
				"graph maintenance definition changed before build",
			));
		}
		if matches!(candidate.definition(), super::GraphIndexDefinition::Reachability { .. })
			&& !candidate.is_on_demand()
		{
			let artifact = super::ReachabilityArtifact::build(
				database,
				candidate,
				build_revision,
				crate::storage::external_sort_run_budget(),
				&mut crate::storage::check_external_sort_cancelled,
			)?;
			let artifacts = Arc::make_mut(&mut database.reachability_artifacts);
			artifacts.remove(&index);
			if let Some(artifact) = artifact {
				artifacts.insert(index, Arc::new(artifact));
			}
		} else if matches!(
			candidate.definition(),
			super::GraphIndexDefinition::Neighbourhood { .. }
		) && !candidate.is_on_demand()
		{
			let artifact = super::NeighbourhoodArtifact::build(
				database,
				candidate,
				build_revision,
				crate::storage::external_sort_run_budget(),
				&mut crate::storage::check_external_sort_cancelled,
			)?;
			let artifacts = Arc::make_mut(&mut database.neighbourhood_artifacts);
			artifacts.remove(&index);
			if let Some(artifact) = artifact {
				artifacts.insert(index, Arc::new(artifact));
			}
		} else if matches!(candidate.definition(), super::GraphIndexDefinition::Path {
			algorithm: super::PathIndexAlgorithm::LandmarkSssp(_),
			..
		}) {
			if let super::GraphIndexDefinition::Path {
				cost: super::GraphIndexCost::Function {
					function,
					version,
				},
				..
			} = candidate.definition()
			{
				candidate.dependencies = PureFunctionEvaluator::for_builder(
					database,
					crate::storage::external_sort_run_budget(),
					crate::storage::check_external_sort_cancelled,
				)?
				.dependencies(*function, *version)?;
				candidate.validate()?;
			}
			let artifact = super::LandmarkPathArtifact::build(
				database,
				candidate,
				build_revision,
				crate::storage::external_sort_run_budget(),
				&mut crate::storage::check_external_sort_cancelled,
			)?;
			Arc::make_mut(&mut database.path_artifacts).insert(index, Arc::new(artifact));
		} else if matches!(candidate.definition(), super::GraphIndexDefinition::Tensor { .. }) {
			let artifact = super::TensorArtifact::build(
				database,
				candidate,
				build_revision,
				crate::graph::tensor::GraphAlgorithmLimits {
					memory_bytes: crate::storage::external_sort_run_budget(),
					..Default::default()
				},
				Some(sort_root),
				&mut crate::storage::check_external_sort_cancelled,
			)?;
			Arc::make_mut(&mut database.tensor_artifacts).insert(index, Arc::new(artifact));
		} else if !candidate.is_on_demand() {
			return Err(FerrosGrynnError::query_feature_not_supported(
				"materialized graph index build",
			));
		}
		candidate.state = super::ClassIndexState::Ready;
		candidate.build_revision = build_revision;
		let replacement = definition.replace_indexes(indexes)?;
		database.replace_graph_schema(replacement, build_revision)?;
		return Ok(());
	}
	let MaintenanceTarget::Index {
		class,
		index,
		version,
	} = target
	else {
		return Err(FerrosGrynnError::invalid_argument(
			"index maintenance job has a non-index target",
		));
	};
	let catalog = Arc::make_mut(&mut database.schema);
	let definition = catalog
		.classes
		.get_mut(&class)
		.ok_or_else(|| FerrosGrynnError::class_does_not_exist(class.to_string()))?;
	let definition = Arc::make_mut(definition);
	let candidate =
		definition.indexes.iter_mut().find(|candidate| candidate.id() == Some(index)).ok_or_else(
			|| FerrosGrynnError::invalid_argument("maintenance index target no longer exists"),
		)?;
	if candidate.version() != version {
		return Err(FerrosGrynnError::invalid_argument(
			"maintenance index definition changed before build",
		));
	}
	candidate.state = super::ClassIndexState::Ready;
	candidate.build_revision = build_revision;
	Ok(())
}

/// Returns the maintenance source items result.
fn maintenance_source_items(database: &DatabaseRoot, target: MaintenanceTarget) -> u64 {
	match target {
		MaintenanceTarget::Index {
			class,
			..
		} => database
			.atoms
			.atoms()
			.filter(|atom| atom.class_id() == class)
			.count()
			.try_into()
			.unwrap_or(u64::MAX),
		MaintenanceTarget::Graph(graph)
		| MaintenanceTarget::GraphIndex {
			graph,
			..
		} => database
			.graphs
			.get(&graph)
			.map_or(0, |graph| graph.occurrences().count().try_into().unwrap_or(u64::MAX)),
		MaintenanceTarget::BaseClass(class) => database
			.atoms
			.atoms()
			.filter(|atom| atom.class_id() == class)
			.count()
			.try_into()
			.unwrap_or(u64::MAX),
		MaintenanceTarget::EffectiveClass {
			class,
			graph,
		} => database.graphs.get(&graph).map_or(0, |graph| {
			graph
				.occurrences()
				.filter(|occurrence| {
					database
						.atom(occurrence.global_id())
						.is_some_and(|atom| atom.class_id() == class)
				})
				.count()
				.try_into()
				.unwrap_or(u64::MAX)
		}),
		MaintenanceTarget::Database => {
			let atoms = database.atoms.atoms().count().try_into().unwrap_or(u64::MAX);
			let occurrences = database.graphs.values().fold(0_u64, |total, graph| {
				total.saturating_add(graph.occurrences().count().try_into().unwrap_or(u64::MAX))
			});
			atoms.saturating_add(occurrences)
		}
	}
}

/// Validated local options state.
pub struct ValidatedLocalOptions {
	/// Pool pages associated with this validated local options.
	pub(crate) pool_pages: NonZeroUsize,
	/// Checkpoint page threshold associated with this validated local options.
	pub(crate) checkpoint_page_threshold: usize,
	/// Io mode associated with this validated local options.
	pub(crate) io_mode: PageIoMode,
}

/// Validates local options.
pub fn validate_local_options(options: LocalOptions) -> FerrosGrynnResult<ValidatedLocalOptions> {
	let io_mode = match options.io_mode {
		IoMode::Sync => PageIoMode::Sync,
		IoMode::NativeAsync => {
			#[cfg(all(windows, feature = "native-async-io"))]
			{
				PageIoMode::NativeAsync
			}
			#[cfg(not(all(windows, feature = "native-async-io")))]
			{
				return Err(FerrosGrynnError::io_mode_unavailable(
					"NativeAsync",
					"Windows IoRing requires feature native-async-io",
				));
			}
		}
	};
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
		io_mode,
	})
}

#[cfg(test)]
/// Regression tests for backend behavior.
mod query_scratch_tests {
	use super::*;
	#[cfg(test)]
	use crate::test_assertions::AssertResult as _;

	/// Verifies in memory stores own distinct query scratch roots.
	#[test]
	fn in_memory_stores_own_distinct_query_scratch_roots() {
		let first = crate::datastore::Datastore::in_memory();
		let second = crate::datastore::Datastore::in_memory();
		let first_root = first.inner.query_scratch_root();
		let second_root = second.inner.query_scratch_root();
		assert_ne!(first_root, second_root);
		fs::create_dir_all(&first_root).assert_ok();
		fs::create_dir_all(&second_root).assert_ok();

		drop(first);
		assert!(!first_root.exists());
		assert!(second_root.exists());
		drop(second);
		assert!(!second_root.exists());
	}

	/// Verifies in memory has no local storage status.
	#[test]
	fn in_memory_has_no_local_storage_status() {
		assert!(crate::datastore::Datastore::in_memory().local_storage_status().is_none());
	}
}
