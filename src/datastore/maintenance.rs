//! Durable maintenance-job identities, states, public projections, and the
//! single-worker coordinator.

use std::{
	collections::{BTreeMap, BTreeSet, VecDeque},
	fmt::Display,
	num::NonZeroUsize,
	sync::{
		Arc,
		Weak,
		atomic::{AtomicBool, Ordering},
	},
	thread::JoinHandle,
	time::SystemTime,
};

use parking_lot::{Condvar, Mutex};

use super::{
	ClassId,
	ClassIndexState,
	DatastoreOptions,
	IndexId,
	IndexMaintenanceMode,
	MaintenanceOptions,
	SchemaVersion,
	StoreInner,
};
use crate::{
	gql_status::{FerrosGrynnError, FerrosGrynnResult, StatusCode},
	graph::GraphId,
	values::{Object, Uuid, Value},
};

/// Stable identity of one durable maintenance job.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(transparent)]
pub struct JobId(
	/// UUID identifying one maintenance job independently of its queue
	/// position.
	Uuid,
);

impl JobId {
	/// Creates a time-ordered maintenance-job identity.
	pub(crate) fn new() -> Self {
		Self(Uuid::now_v7())
	}

	/// Creates an identity decoded from its durable UUID representation.
	pub(crate) const fn from_uuid(value: Uuid) -> Self {
		Self(value)
	}

	/// Returns the public UUID representation used by GrynnQL.
	pub const fn as_uuid(self) -> Uuid {
		self.0
	}
}

impl Display for JobId {
	/// Formats `JobId` using the requested formatter.
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		Display::fmt(&self.0, f)
	}
}

/// Operation performed by one maintenance job.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum MaintenanceJobKind {
	/// Runs one accepted durable EVENT revision through the query kernel.
	EventInvocation,
	/// Build the first physical generation of an index.
	BuildIndex,
	/// Replace an existing physical index generation.
	RebuildIndex,
	/// Apply deferred changes before publishing the index generation.
	DeferredIndexCatchUp,
	/// Refresh planner statistics for the selected target.
	Analyze,
	/// Compact one index physical representation.
	CompactIndex,
	/// Compact the physical files of one graph.
	CompactGraph,
	/// Compact the database physical storage.
	CompactDatabase,
	/// Publishes expired history prefixes and retires closed sources.
	PurgeChangefeed,
}

/// Durable lifecycle state of one maintenance job.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum MaintenanceJobState {
	/// An EVENT attempt has started; its effects remain transaction-private.
	Running,
	/// Job is waiting to begin execution.
	Queued,
	/// Job is constructing the initial candidate generation.
	BuildingInitial,
	/// Job is applying changes accumulated during the initial build.
	CatchingUp,
	/// Job is publishing its prepared generation through the commit barrier.
	Publishing,
	/// Job finished successfully.
	Completed,
	/// Job stopped after explicit cancellation.
	Cancelled,
	/// Job stopped because an operation failed.
	Failed,
	/// Job was interrupted before completing its publication.
	Interrupted,
}

/// Statistics construction mode retained by an `ANALYZE` job.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MaintenanceAnalyzeMode {
	/// Automatic case of maintenance analyze mode.
	Automatic,
	/// Full case of maintenance analyze mode.
	Full,
	/// Approximate case of maintenance analyze mode.
	Approximate {
		/// Sample count associated with this maintenance analyze mode.
		sample_count: u32,
	},
}

/// Logical target retained by the durable job ledger.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum MaintenanceTarget {
	/// Graph-owned user definition, distinct from graph-wide maintenance.
	GraphIndex {
		/// Graph associated with this maintenance target.
		graph: GraphId,
		/// Index associated with this maintenance target.
		index: IndexId,
		/// Version associated with this maintenance target.
		version: SchemaVersion,
	},
	/// Index case of maintenance target.
	Index {
		/// Class associated with this maintenance target.
		class: ClassId,
		/// Index associated with this maintenance target.
		index: IndexId,
		/// Version associated with this maintenance target.
		version: SchemaVersion,
	},
	/// Base class case of maintenance target.
	BaseClass(
		/// Base class case of maintenance target.
		ClassId,
	),
	/// Effective class case of maintenance target.
	EffectiveClass {
		/// Class associated with this maintenance target.
		class: ClassId,
		/// Graph associated with this maintenance target.
		graph: GraphId,
	},
	/// Graph case of maintenance target.
	Graph(
		/// Graph case of maintenance target.
		GraphId,
	),
	/// Database case of maintenance target.
	Database,
}

/// Accepted EVENT revision and retry ancestry, without retaining its payload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EventJobInfo {
	/// Stable schema identity captured by the source operation.
	pub(crate) id: super::EventId,
	/// Definition revision retained after ALTER or REMOVE.
	pub(crate) version: SchemaVersion,
	/// Distinct attempts started, excluding an interrupted attempt's recovery.
	pub(crate) attempts: u16,
	/// Historical depth, unchanged by retry.
	pub(crate) depth: u8,
	/// Additional permitted attempts.
	pub(crate) retries: u8,
	/// Tightest retained ancestor bound.
	pub(crate) max_depth: u8,
}

impl EventJobInfo {
	/// Returns the accepted definition identity.
	pub const fn id(&self) -> super::EventId {
		self.id
	}

	/// Returns the accepted definition version.
	pub const fn version(&self) -> SchemaVersion {
		self.version
	}

	/// Returns how many distinct attempts started.
	pub const fn attempts(&self) -> u16 {
		self.attempts
	}

	/// Returns the invocation's historical causal depth.
	pub const fn depth(&self) -> u8 {
		self.depth
	}

	/// Returns the number of additional permitted attempts.
	pub const fn retries(&self) -> u8 {
		self.retries
	}

	/// Returns the inherited causal depth ceiling.
	pub const fn max_depth(&self) -> u8 {
		self.max_depth
	}
}

/// Immutable public projection of one durable job record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MaintenanceJobInfo {
	/// Invocation-only metadata is projected from the authoritative MVCC
	/// outbox.
	pub(crate) event: Option<EventJobInfo>,
	/// Identifier used to locate this value within its owning subsystem.
	pub(crate) id: JobId,
	/// Kind associated with this maintenance job info.
	pub(crate) kind: MaintenanceJobKind,
	/// State associated with this maintenance job info.
	pub(crate) state: MaintenanceJobState,
	/// Target associated with this maintenance job info.
	pub(crate) target: MaintenanceTarget,
	/// Source revision associated with this maintenance job info.
	pub(crate) source_revision: u64,
	/// Processed items associated with this maintenance job info.
	pub(crate) processed_items: u64,
	/// Written bytes associated with this maintenance job info.
	pub(crate) written_bytes: u64,
	/// Sort runs associated with this maintenance job info.
	pub(crate) sort_runs: u64,
	/// Restart count associated with this maintenance job info.
	pub(crate) restart_count: u32,
	/// Max memory bytes associated with this maintenance job info.
	pub(crate) max_memory_bytes: NonZeroUsize,
	/// Max parallelism associated with this maintenance job info.
	pub(crate) max_parallelism: NonZeroUsize,
	/// Candidate fingerprint associated with this maintenance job info.
	pub(crate) candidate_fingerprint: Option<u64>,
	/// Analyze mode associated with this maintenance job info.
	pub(crate) analyze_mode: Option<MaintenanceAnalyzeMode>,
	/// Terminal status associated with this maintenance job info.
	pub(crate) terminal_status: Option<StatusCode>,
	/// Diagnostic associated with this maintenance job info.
	pub(crate) diagnostic: Option<String>,
	/// Created at associated with this maintenance job info.
	pub(crate) created_at: SystemTime,
	/// Updated at associated with this maintenance job info.
	pub(crate) updated_at: SystemTime,
}

impl MaintenanceJobInfo {
	/// Returns accepted EVENT metadata for invocation jobs.
	pub const fn event(&self) -> Option<&EventJobInfo> {
		self.event.as_ref()
	}

	/// Returns the stable job identity.
	pub const fn id(&self) -> JobId {
		self.id
	}

	/// Returns the operation performed by the job.
	pub const fn kind(&self) -> MaintenanceJobKind {
		self.kind
	}

	/// Returns the current durable lifecycle state.
	pub const fn state(&self) -> MaintenanceJobState {
		self.state
	}

	/// Returns the source catalog or graph revision captured by the job.
	pub const fn source_revision(&self) -> u64 {
		self.source_revision
	}

	/// Returns logical source items processed so far.
	pub const fn processed_items(&self) -> u64 {
		self.processed_items
	}

	/// Returns bytes written by the current build attempt.
	pub const fn written_bytes(&self) -> u64 {
		self.written_bytes
	}

	/// Returns external-sort runs produced by the current attempt.
	pub const fn sort_runs(&self) -> u64 {
		self.sort_runs
	}

	/// Returns how many interrupted attempts preceded the current one.
	pub const fn restart_count(&self) -> u32 {
		self.restart_count
	}

	/// Returns the configured aggregate builder-memory limit.
	pub const fn max_memory_bytes(&self) -> NonZeroUsize {
		self.max_memory_bytes
	}

	/// Returns the configured upper bound for scoped build threads.
	pub const fn max_parallelism(&self) -> NonZeroUsize {
		self.max_parallelism
	}

	/// Returns the statistics-build policy for an `ANALYZE` job.
	pub const fn analyze_mode(&self) -> Option<&'static str> {
		match self.analyze_mode {
			Some(MaintenanceAnalyzeMode::Automatic) => Some("automatic"),
			Some(MaintenanceAnalyzeMode::Full) => Some("full"),
			Some(MaintenanceAnalyzeMode::Approximate {
				..
			}) => Some("approximate"),
			None => None,
		}
	}

	/// Returns the configured sample ceiling for an approximate `ANALYZE` job.
	pub const fn analyze_sample_count(&self) -> Option<u32> {
		match self.analyze_mode {
			Some(MaintenanceAnalyzeMode::Approximate {
				sample_count,
			}) => Some(sample_count),
			_ => None,
		}
	}

	/// Returns the logical target without exposing physical storage identity.
	pub fn target(&self) -> String {
		self.target.display_name()
	}

	/// Returns the terminal status code, if the job failed.
	pub const fn terminal_status(&self) -> Option<StatusCode> {
		self.terminal_status
	}

	/// Returns a bounded, path-free diagnostic retained for administration.
	pub fn diagnostic(&self) -> Option<&str> {
		self.diagnostic.as_deref()
	}

	/// Returns the creation time recorded by the coordinator.
	pub const fn created_at(&self) -> SystemTime {
		self.created_at
	}

	/// Returns the last durable state-change time.
	pub const fn updated_at(&self) -> SystemTime {
		self.updated_at
	}

	/// Returns the value result.
	pub(crate) fn value(&self) -> Value {
		let mut value = Object::from_iter([
			("id", Value::from(self.id.as_uuid())),
			("kind", Value::from(job_kind_name(self.kind))),
			("state", Value::from(job_state_name(self.state))),
			("target", Value::from(self.target.display_name())),
			("source_revision", Value::from(self.source_revision)),
			("processed_items", Value::from(self.processed_items)),
			("written_bytes", Value::from(self.written_bytes)),
			("sort_runs", Value::from(self.sort_runs)),
			("analyze_mode", self.analyze_mode().map_or(Value::None, Value::from)),
			("analyze_sample_count", self.analyze_sample_count().map_or(Value::None, Value::from)),
			("restart_count", Value::from(self.restart_count)),
			(
				"terminal_status",
				self.terminal_status.map_or(Value::None, |status| Value::from(status.to_string())),
			),
			("diagnostic", self.diagnostic.clone().map_or(Value::None, Value::from)),
		]);
		if let Some(event) = &self.event {
			for (name, field) in [
				("event_id", Value::from(event.id.get())),
				("event_version", Value::from(event.version.get())),
				("attempts", Value::from(event.attempts)),
				("depth", Value::from(event.depth)),
				("retry", Value::from(event.retries)),
				("maxdepth", Value::from(event.max_depth)),
			] {
				value.insert(name, field);
			}
		}
		Value::from(value)
	}
}

/// Returns the job kind name result.
const fn job_kind_name(kind: MaintenanceJobKind) -> &'static str {
	match kind {
		MaintenanceJobKind::EventInvocation => "event_invocation",
		MaintenanceJobKind::BuildIndex => "build_index",
		MaintenanceJobKind::RebuildIndex => "rebuild_index",
		MaintenanceJobKind::DeferredIndexCatchUp => "deferred_index_catch_up",
		MaintenanceJobKind::Analyze => "analyze",
		MaintenanceJobKind::CompactIndex => "compact_index",
		MaintenanceJobKind::CompactGraph => "compact_graph",
		MaintenanceJobKind::CompactDatabase => "compact_database",
		MaintenanceJobKind::PurgeChangefeed => "purge_changefeed",
	}
}

/// Returns the job state name result.
const fn job_state_name(state: MaintenanceJobState) -> &'static str {
	match state {
		MaintenanceJobState::Running => "running",
		MaintenanceJobState::Queued => "queued",
		MaintenanceJobState::BuildingInitial => "building_initial",
		MaintenanceJobState::CatchingUp => "catching_up",
		MaintenanceJobState::Publishing => "publishing",
		MaintenanceJobState::Completed => "completed",
		MaintenanceJobState::Cancelled => "cancelled",
		MaintenanceJobState::Failed => "failed",
		MaintenanceJobState::Interrupted => "interrupted",
	}
}

#[allow(
	clippy::multiple_inherent_impl,
	reason = "public job projection and coordinator-only construction are intentionally separated"
)]
impl MaintenanceJobInfo {
	/// Configures the existing fair maintenance worker with the datastore EVENT
	/// interval.
	pub(in crate::datastore) fn new(
		kind: MaintenanceJobKind,
		target: MaintenanceTarget,
		source_revision: u64,
		options: MaintenanceOptions,
		analyze_mode: Option<MaintenanceAnalyzeMode>,
	) -> Self {
		let now = SystemTime::now();
		Self {
			event: None,
			id: JobId::new(),
			kind,
			state: MaintenanceJobState::Queued,
			target,
			source_revision,
			processed_items: 0,
			written_bytes: 0,
			sort_runs: 0,
			restart_count: 0,
			max_memory_bytes: options.max_memory_bytes,
			max_parallelism: options.max_parallelism,
			candidate_fingerprint: None,
			analyze_mode,
			terminal_status: None,
			diagnostic: None,
			created_at: now,
			updated_at: now,
		}
	}
}

impl MaintenanceTarget {
	/// Returns the display name result.
	pub(crate) fn display_name(self) -> String {
		match self {
			Self::GraphIndex {
				graph,
				index,
				..
			} => format!("graph:{graph}/index:{index}"),
			Self::Index {
				class,
				index,
				..
			} => format!("class:{class}/index:{index}"),
			Self::BaseClass(class) => format!("class:{class}/base"),
			Self::EffectiveClass {
				class,
				graph,
			} => format!("class:{class}/graph:{graph}/effective"),
			Self::Graph(graph) => format!("graph:{graph}"),
			Self::Database => "database".to_owned(),
		}
	}
}

#[derive(Clone)]
/// Maintenance job catalog, runnable queue and cancellation state protected by
/// the coordinator.
struct CoordinatorState {
	/// Protects root-owned invocation wakeups against a predicate/wait race.
	event_wake: u64,
	/// Jobs associated with this coordinator state.
	jobs: BTreeMap<JobId, MaintenanceJobInfo>,
	/// Queue associated with this coordinator state.
	queue: VecDeque<JobId>,
	/// Pending index changes associated with this coordinator state.
	pending_index_changes: VecDeque<(MaintenanceTarget, u64)>,
	/// Pending full rebuilds associated with this coordinator state.
	pending_full_rebuilds: BTreeMap<MaintenanceTarget, u64>,
	/// Cancelled associated with this coordinator state.
	cancelled: BTreeSet<JobId>,
	/// Cancellation tokens associated with this coordinator state.
	cancellation_tokens: BTreeMap<JobId, Arc<AtomicBool>>,
	/// Stopping associated with this coordinator state.
	stopping: bool,
}

/// Configured index pending queue capacity value.
const INDEX_PENDING_QUEUE_CAPACITY: usize = 65_536;

impl CoordinatorState {
	/// Records index change.
	fn record_index_change(&mut self, target: MaintenanceTarget, revision: u64) {
		if let Some(latest) = self.pending_full_rebuilds.get_mut(&target) {
			*latest = (*latest).max(revision);
			return;
		}
		if self.pending_index_changes.len() == INDEX_PENDING_QUEUE_CAPACITY
			&& let Some((overflow_target, overflow_revision)) =
				self.pending_index_changes.pop_front()
		{
			self.pending_full_rebuilds
				.entry(overflow_target)
				.and_modify(|latest| *latest = (*latest).max(overflow_revision))
				.or_insert(overflow_revision);
		}
		self.pending_index_changes.push_back((target, revision));
	}

	/// Returns whether this value has index change after.
	fn has_index_change_after(&self, target: MaintenanceTarget, revision: u64) -> bool {
		self.pending_full_rebuilds.get(&target).is_some_and(|latest| *latest > revision)
			|| self
				.pending_index_changes
				.iter()
				.any(|(candidate, changed_at)| *candidate == target && *changed_at > revision)
	}

	/// Performs the acknowledge index changes operation.
	fn acknowledge_index_changes(&mut self, target: MaintenanceTarget, revision: u64) {
		self.pending_index_changes
			.retain(|(candidate, changed_at)| *candidate != target || *changed_at > revision);
		if self.pending_full_rebuilds.get(&target).is_some_and(|latest| *latest <= revision) {
			self.pending_full_rebuilds.remove(&target);
		}
	}
}

/// Coordinator shared state.
struct CoordinatorShared {
	/// State associated with this coordinator shared.
	state: Mutex<CoordinatorState>,
	/// Wake associated with this coordinator shared.
	wake: Condvar,
}

/// One owned FIFO maintenance worker and its durable state mirror.
pub struct MaintenanceCoordinator {
	/// Shared associated with this maintenance coordinator.
	shared: Arc<CoordinatorShared>,
	/// Worker associated with this maintenance coordinator.
	worker: Mutex<Option<JoinHandle<()>>>,
}

impl MaintenanceCoordinator {
	/// Wakes the existing scheduler after an invocation publication.
	pub(crate) fn wake_events(&self) {
		let mut state = self.shared.state.lock();
		state.event_wake = state.event_wake.wrapping_add(1);
		self.shared.wake.notify_one();
	}

	/// The single worker owns at most one invocation token at a time.
	pub(crate) fn begin_event_attempt(&self, id: JobId) -> Arc<AtomicBool> {
		let mut state = self.shared.state.lock();
		let token = Arc::new(AtomicBool::new(state.stopping));
		state.cancellation_tokens.insert(id, Arc::clone(&token));
		token
	}

	/// Removes the transient attempt token; root state remains the authority.
	pub(crate) fn end_event_attempt(&self, id: JobId) {
		self.shared.state.lock().cancellation_tokens.remove(&id);
	}

	/// KILL publishes cancellation before signalling a running attempt.
	pub(crate) fn cancel_event_attempt(&self, id: JobId) {
		if let Some(token) = self.shared.state.lock().cancellation_tokens.get(&id) {
			token.store(true, Ordering::Release);
		}
		self.shared.wake.notify_one();
	}

	/// Creates a new `MaintenanceCoordinator` value.
	pub(crate) fn new(recovered: Vec<MaintenanceJobInfo>) -> Self {
		let jobs: BTreeMap<JobId, MaintenanceJobInfo> =
			recovered.into_iter().map(|job| (job.id, job)).collect();
		let cancellation_tokens =
			jobs.keys().copied().map(|id| (id, Arc::new(AtomicBool::new(false)))).collect();
		Self {
			shared: Arc::new(CoordinatorShared {
				state: Mutex::new(CoordinatorState {
					event_wake: 0,
					jobs,
					queue: VecDeque::new(),
					pending_index_changes: VecDeque::new(),
					pending_full_rebuilds: BTreeMap::new(),
					cancelled: BTreeSet::new(),
					cancellation_tokens,
					stopping: false,
				}),
				wake: Condvar::new(),
			}),
			worker: Mutex::new(None),
		}
	}

	/// Reconciles recovered records and starts the single owned worker.
	pub(crate) fn start(&self, store: &Arc<StoreInner>) -> FerrosGrynnResult<()> {
		let mut worker = self.worker.lock();
		if worker.is_some() {
			return Ok(());
		}
		self.reconcile(store)?;
		self.recover_stale_indexes(store)?;
		let shared = self.shared.clone();
		let store = Arc::downgrade(store);
		let handle = std::thread::Builder::new()
			.name("ferros-grynn-maintenance".to_owned())
			.spawn(move || worker_loop(&shared, &store))
			.map_err(|source| {
				FerrosGrynnError::storage_io_failure("start maintenance worker")
					.with_technical_source(source)
			})?;
		*worker = Some(handle);
		Ok(())
	}

	/// Recovers stale indexes.
	pub(crate) fn recover_stale_indexes(&self, store: &StoreInner) -> FerrosGrynnResult<()> {
		let root = store.current_root();
		let options = match store.options {
			DatastoreOptions::InMemory(options) => options.maintenance,
			DatastoreOptions::Local(options) => options.maintenance,
		};
		let mut state = self.shared.state.lock();
		let previous = state.clone();
		let mut changed = false;
		for class in root.schema.classes() {
			let Some(class_id) = class.id() else {
				continue;
			};
			for index in class.indexes().filter(|index| index.state() == ClassIndexState::Stale) {
				let Some(index_id) = index.id() else {
					continue;
				};
				let target = MaintenanceTarget::Index {
					class: class_id,
					index: index_id,
					version: index.version(),
				};
				let superseded = state
					.jobs
					.values()
					.filter(|job| {
						!terminal(job.state)
							&& matches!(
								job.target,
								MaintenanceTarget::Index {
									class,
									index,
									..
								} if class == class_id && index == index_id
							)
							&& job.target != target
					})
					.map(|job| job.id)
					.collect::<Vec<_>>();
				for id in superseded {
					if let Some(token) = state.cancellation_tokens.get(&id) {
						token.store(true, Ordering::Release);
					}
					state.queue.retain(|queued| *queued != id);
					if let Some(job) = state.jobs.get_mut(&id) {
						job.state = MaintenanceJobState::Cancelled;
						job.candidate_fingerprint = None;
						job.diagnostic =
							Some("superseded by a newer index definition version".to_owned());
						job.updated_at = SystemTime::now();
						changed = true;
					}
				}
				if state.jobs.values().any(|job| job.target == target && !terminal(job.state)) {
					continue;
				}
				let kind = if index.maintenance_mode() == IndexMaintenanceMode::Deferred {
					MaintenanceJobKind::DeferredIndexCatchUp
				} else {
					MaintenanceJobKind::BuildIndex
				};
				let job = MaintenanceJobInfo::new(kind, target, root.commit_ts, options, None);
				state.cancellation_tokens.insert(job.id, Arc::new(AtomicBool::new(false)));
				state.queue.push_back(job.id);
				state.jobs.insert(job.id, job);
				changed = true;
			}
		}
		if changed && let Err(error) = persist_state(store, &state) {
			*state = previous;
			return Err(error);
		}
		if changed {
			self.shared.wake.notify_one();
		}
		Ok(())
	}

	/// Returns the reconcile result.
	fn reconcile(&self, store: &StoreInner) -> FerrosGrynnResult<()> {
		let mut state = self.shared.state.lock();
		let previous = state.clone();
		let mut changed = false;
		let ids = state.jobs.keys().copied().collect::<Vec<_>>();
		for id in ids {
			let Some(job) = state.jobs.get_mut(&id) else {
				continue;
			};
			if !terminal(job.state) && !store.maintenance_target_exists(job.target) {
				job.state = MaintenanceJobState::Cancelled;
				job.candidate_fingerprint = None;
				job.diagnostic = Some("maintenance target no longer exists".to_owned());
				job.updated_at = SystemTime::now();
				changed = true;
				continue;
			}
			match job.state {
				MaintenanceJobState::Running => {
					return Err(FerrosGrynnError::invalid_argument(
						"EVENT state cannot be recovered from the maintenance ledger",
					));
				}
				MaintenanceJobState::Queued => state.queue.push_back(id),
				MaintenanceJobState::Publishing if store.maintenance_publication_matches(job) => {
					job.state = MaintenanceJobState::Completed;
					job.updated_at = SystemTime::now();
					changed = true;
				}
				MaintenanceJobState::BuildingInitial
				| MaintenanceJobState::CatchingUp
				| MaintenanceJobState::Publishing
				| MaintenanceJobState::Interrupted => {
					job.state = MaintenanceJobState::Queued;
					job.restart_count = job.restart_count.saturating_add(1);
					job.candidate_fingerprint = None;
					job.diagnostic =
						Some("requeued after interrupted maintenance attempt".to_owned());
					job.updated_at = SystemTime::now();
					state.queue.push_back(id);
					changed = true;
				}
				MaintenanceJobState::Completed
				| MaintenanceJobState::Cancelled
				| MaintenanceJobState::Failed => {}
			}
		}
		prune_terminal_history(&mut state);
		if changed && let Err(error) = persist_state(store, &state) {
			*state = previous;
			return Err(error);
		}
		Ok(())
	}

	/// Returns the enqueue result.
	pub(crate) fn enqueue(
		&self,
		store: &Arc<StoreInner>,
		kind: MaintenanceJobKind,
		target: MaintenanceTarget,
		options: MaintenanceOptions,
		analyze_mode: Option<MaintenanceAnalyzeMode>,
	) -> FerrosGrynnResult<JobId> {
		self.enqueue_with_existing(store, kind, target, options, analyze_mode, false)
	}

	/// Returns the enqueue coalesced result.
	pub(crate) fn enqueue_coalesced(
		&self,
		store: &Arc<StoreInner>,
		kind: MaintenanceJobKind,
		target: MaintenanceTarget,
		options: MaintenanceOptions,
		analyze_mode: Option<MaintenanceAnalyzeMode>,
	) -> FerrosGrynnResult<JobId> {
		self.enqueue_with_existing(store, kind, target, options, analyze_mode, true)
	}

	/// Returns the enqueue with existing result.
	fn enqueue_with_existing(
		&self,
		store: &Arc<StoreInner>,
		kind: MaintenanceJobKind,
		target: MaintenanceTarget,
		options: MaintenanceOptions,
		analyze_mode: Option<MaintenanceAnalyzeMode>,
		coalesce: bool,
	) -> FerrosGrynnResult<JobId> {
		self.start(store)?;
		let mut state = self.shared.state.lock();
		if let Some(existing) = state.jobs.values().find(|job| {
			job.target == target
				&& !terminal(job.state)
				&& (job.kind == kind
					|| job.kind != MaintenanceJobKind::PurgeChangefeed
						&& kind != MaintenanceJobKind::PurgeChangefeed)
		}) {
			if coalesce {
				return Ok(existing.id);
			}
			return Err(FerrosGrynnError::maintenance_target_busy(target.display_name()));
		}
		let previous = state.clone();
		let job = MaintenanceJobInfo::new(
			kind,
			target,
			store.current_root().commit_ts,
			options,
			analyze_mode,
		);
		let id = job.id;
		state.cancellation_tokens.insert(id, Arc::new(AtomicBool::new(false)));
		state.jobs.insert(id, job);
		state.queue.push_back(id);
		if let Err(error) = persist_state(store, &state) {
			*state = previous;
			return Err(error);
		}
		self.shared.wake.notify_one();
		Ok(id)
	}

	/// Records one committed invalidation for a deferred index and coalesces it
	/// into the target's active catch-up job.
	pub(crate) fn record_deferred_index_change(
		&self,
		store: &Arc<StoreInner>,
		target: MaintenanceTarget,
		revision: u64,
		options: MaintenanceOptions,
	) -> FerrosGrynnResult<JobId> {
		self.start(store)?;
		let mut state = self.shared.state.lock();
		let previous = state.clone();
		state.record_index_change(target, revision);
		if let Some(id) = state
			.jobs
			.values()
			.find(|job| job.target == target && !terminal(job.state))
			.map(|job| job.id)
		{
			return Ok(id);
		}
		let job = MaintenanceJobInfo::new(
			MaintenanceJobKind::DeferredIndexCatchUp,
			target,
			revision,
			options,
			None,
		);
		let id = job.id;
		state.cancellation_tokens.insert(id, Arc::new(AtomicBool::new(false)));
		state.jobs.insert(id, job);
		state.queue.push_back(id);
		if let Err(error) = persist_state(store, &state) {
			*state = previous;
			return Err(error);
		}
		self.shared.wake.notify_one();
		Ok(id)
	}

	/// Returns whether this value has pending index change after.
	pub(crate) fn has_pending_index_change_after(
		&self,
		target: MaintenanceTarget,
		revision: u64,
	) -> bool {
		self.shared.state.lock().has_index_change_after(target, revision)
	}

	/// Performs the acknowledge index changes operation.
	pub(crate) fn acknowledge_index_changes(&self, target: MaintenanceTarget, revision: u64) {
		self.shared.state.lock().acknowledge_index_changes(target, revision);
	}

	/// Returns the job result.
	pub(crate) fn job(&self, id: JobId) -> Option<MaintenanceJobInfo> {
		self.shared.state.lock().jobs.get(&id).cloned()
	}

	/// Returns the jobs result.
	pub(crate) fn jobs(&self) -> Vec<MaintenanceJobInfo> {
		self.shared.state.lock().jobs.values().cloned().collect()
	}

	/// Returns the active job for result.
	pub(crate) fn active_job_for(&self, target: MaintenanceTarget) -> Option<MaintenanceJobInfo> {
		self.shared
			.state
			.lock()
			.jobs
			.values()
			.find(|job| job.target == target && !terminal(job.state))
			.cloned()
	}

	/// Returns the kill result.
	pub(crate) fn kill(
		&self,
		store: &StoreInner,
		id: JobId,
	) -> FerrosGrynnResult<MaintenanceJobInfo> {
		let mut state = self.shared.state.lock();
		let previous = state.clone();
		let published = state.jobs.get(&id).is_some_and(|job| {
			job.state == MaintenanceJobState::Publishing
				&& store.maintenance_publication_matches(job)
		});
		let job = state
			.jobs
			.get_mut(&id)
			.ok_or_else(|| FerrosGrynnError::maintenance_job_not_found(id.to_string()))?;
		if !terminal(job.state) {
			job.state = if published {
				MaintenanceJobState::Completed
			} else {
				MaintenanceJobState::Cancelled
			};
			job.updated_at = SystemTime::now();
			if !published {
				state.cancelled.insert(id);
				if let Some(token) = state.cancellation_tokens.get(&id) {
					token.store(true, Ordering::Release);
				}
				state.queue.retain(|queued| *queued != id);
			}
			if let Err(error) = persist_state(store, &state) {
				*state = previous;
				return Err(error);
			}
		}
		state
			.jobs
			.get(&id)
			.cloned()
			.ok_or_else(|| FerrosGrynnError::maintenance_job_not_found(id.to_string()))
	}

	/// Returns whether this value is cancelled.
	pub(crate) fn is_cancelled(&self, id: JobId) -> bool {
		let state = self.shared.state.lock();
		state.stopping
			|| state.cancelled.contains(&id)
			|| state.cancellation_tokens.get(&id).is_some_and(|token| token.load(Ordering::Acquire))
	}

	/// Returns the cancellation token result.
	pub(crate) fn cancellation_token(&self, id: JobId) -> Arc<AtomicBool> {
		self.shared
			.state
			.lock()
			.cancellation_tokens
			.get(&id)
			.cloned()
			.unwrap_or_else(|| Arc::new(AtomicBool::new(true)))
	}

	/// Marks publishing.
	pub(crate) fn mark_publishing(
		&self,
		store: &StoreInner,
		id: JobId,
		fingerprint: u64,
	) -> FerrosGrynnResult<bool> {
		let mut state = self.shared.state.lock();
		if state.stopping || state.cancelled.contains(&id) {
			return Ok(false);
		}
		let previous = state.clone();
		let job = state
			.jobs
			.get_mut(&id)
			.ok_or_else(|| FerrosGrynnError::maintenance_job_not_found(id.to_string()))?;
		job.state = MaintenanceJobState::Publishing;
		job.candidate_fingerprint = Some(fingerprint);
		job.updated_at = SystemTime::now();
		if let Err(error) = persist_state(store, &state) {
			*state = previous;
			return Err(error);
		}
		Ok(true)
	}

	/// Stages final build counters for the next durable phase transition.
	///
	/// A crash before `Publishing` already requires rebuilding the unreachable
	/// candidate, so a separate metadata-COW write here would add latency
	/// without providing a recoverable checkpoint. `mark_publishing`
	/// persists these counters together with the candidate fingerprint.
	pub(crate) fn stage_progress(
		&self,
		id: JobId,
		processed_items: u64,
		written_bytes: u64,
		sort_runs: u64,
	) -> FerrosGrynnResult<()> {
		let mut state = self.shared.state.lock();
		let job = state
			.jobs
			.get_mut(&id)
			.ok_or_else(|| FerrosGrynnError::maintenance_job_not_found(id.to_string()))?;
		job.processed_items = processed_items;
		job.written_bytes = written_bytes;
		job.sort_runs = sort_runs;
		job.updated_at = SystemTime::now();
		Ok(())
	}

	/// Performs the shutdown operation.
	pub(crate) fn shutdown(&self) {
		{
			let mut state = self.shared.state.lock();
			state.stopping = true;
			for token in state.cancellation_tokens.values() {
				token.store(true, Ordering::Release);
			}
			self.shared.wake.notify_all();
		}
		let handle = self.worker.lock().take();
		if let Some(handle) = handle
			&& handle.thread().id() != std::thread::current().id()
		{
			drop(handle.join());
		}
	}

	/// Lets idle metadata scans observe the worker owner's shutdown request.
	pub(super) fn is_stopping(&self) -> bool {
		self.shared.state.lock().stopping
	}
}

/// Performs the worker loop operation.
fn worker_loop(shared: &Arc<CoordinatorShared>, store: &Weak<StoreInner>) {
	let mut prefer_events = true;
	let mut last_discovery: Option<std::time::Instant> = None;
	loop {
		let event_wake = shared.state.lock().event_wake;
		let Some(owner) = store.upgrade() else {
			return;
		};
		if shared.state.lock().stopping {
			return;
		}
		if last_discovery.is_none_or(|last| last.elapsed() >= std::time::Duration::from_secs(1)) {
			owner.sweep_retired_changefeeds();
			if owner.has_changefeed_retention_work().is_ok_and(|due| due) {
				let options = owner.transaction_limits().maintenance;
				drop(owner.maintenance.enqueue_coalesced(
					&owner,
					MaintenanceJobKind::PurgeChangefeed,
					MaintenanceTarget::Database,
					options,
					None,
				));
			}
			last_discovery = Some(std::time::Instant::now());
		}
		if (prefer_events || shared.state.lock().queue.is_empty())
			&& owner.run_due_event(jiff::Timestamp::now().as_nanosecond()).is_ok_and(|ran| ran)
		{
			prefer_events = false;
			continue;
		}
		let idle_wait = owner.event_wait_duration(jiff::Timestamp::now().as_nanosecond());
		drop(owner);
		let id = {
			let mut state = shared.state.lock();
			if state.queue.is_empty() && !state.stopping && state.event_wake == event_wake {
				shared.wake.wait_for(&mut state, idle_wait);
			}
			if state.stopping {
				return;
			}
			state.queue.pop_front()
		};
		let Some(store) = store.upgrade() else {
			return;
		};
		let Some(id) = id else {
			if store.run_due_event(jiff::Timestamp::now().as_nanosecond()).is_ok_and(|ran| ran) {
				prefer_events = false;
				continue;
			}
			continue;
		};
		if begin_job(shared, store.as_ref(), id).is_err() {
			continue;
		}
		let result = store.run_maintenance_job(id);
		finish_job(shared, store.as_ref(), id, result);
		prefer_events = true;
	}
}

/// Begins job.
fn begin_job(shared: &CoordinatorShared, store: &StoreInner, id: JobId) -> FerrosGrynnResult<()> {
	let mut state = shared.state.lock();
	let previous = state.clone();
	let source_revision = store.current_root().commit_ts;
	let job = state
		.jobs
		.get_mut(&id)
		.ok_or_else(|| FerrosGrynnError::maintenance_job_not_found(id.to_string()))?;
	if job.state == MaintenanceJobState::Cancelled {
		return Ok(());
	}
	job.state = if job.kind == MaintenanceJobKind::DeferredIndexCatchUp {
		MaintenanceJobState::CatchingUp
	} else {
		MaintenanceJobState::BuildingInitial
	};
	job.source_revision = source_revision;
	job.candidate_fingerprint = None;
	job.terminal_status = None;
	job.diagnostic = None;
	job.updated_at = SystemTime::now();
	if let Err(error) = persist_state(store, &state) {
		*state = previous;
		return Err(error);
	}
	Ok(())
}

/// Finishes job.
fn finish_job(
	shared: &CoordinatorShared,
	store: &StoreInner,
	id: JobId,
	result: FerrosGrynnResult<MaintenanceRunOutcome>,
) {
	let mut state = shared.state.lock();
	let previous = state.clone();
	let cancelled = state.cancelled.remove(&id);
	let mut requeue = false;
	let Some(job) = state.jobs.get_mut(&id) else {
		return;
	};
	if cancelled || job.state == MaintenanceJobState::Cancelled {
		job.state = if store.maintenance_publication_matches(job) {
			MaintenanceJobState::Completed
		} else {
			MaintenanceJobState::Cancelled
		};
	} else {
		match result {
			Ok(MaintenanceRunOutcome::Completed) => {
				job.state = MaintenanceJobState::Completed;
			}
			Ok(MaintenanceRunOutcome::Retry) => {
				job.state = MaintenanceJobState::Queued;
				job.restart_count = job.restart_count.saturating_add(1);
				job.candidate_fingerprint = None;
				job.diagnostic =
					Some("source revision changed; maintenance was requeued".to_owned());
				requeue = true;
			}
			Ok(MaintenanceRunOutcome::Cancelled) => {
				job.state = MaintenanceJobState::Cancelled;
			}
			Err(error) => {
				job.state = MaintenanceJobState::Failed;
				job.terminal_status = Some(error.status().code());
				job.diagnostic = Some(error.status().description().chars().take(4096).collect());
			}
		}
	}
	job.updated_at = SystemTime::now();
	if requeue {
		state.queue.push_back(id);
	}
	prune_terminal_history(&mut state);
	if persist_state(store, &state).is_err() {
		*state = previous;
	} else if requeue {
		shared.wake.notify_one();
	}
}

/// Returns the persist state result.
fn persist_state(store: &StoreInner, state: &CoordinatorState) -> FerrosGrynnResult<()> {
	store.persist_maintenance_jobs(&state.jobs.values().cloned().collect::<Vec<_>>())
}

/// Performs the prune terminal history operation.
fn prune_terminal_history(state: &mut CoordinatorState) {
	let mut terminal_jobs = state
		.jobs
		.values()
		.filter(|job| terminal(job.state))
		.map(|job| (job.updated_at, job.id))
		.collect::<Vec<_>>();
	if terminal_jobs.len() <= 1024 {
		return;
	}
	terminal_jobs.sort_unstable();
	let remove = terminal_jobs.len() - 1024;
	for (_updated, id) in terminal_jobs.into_iter().take(remove) {
		state.jobs.remove(&id);
		state.cancellation_tokens.remove(&id);
	}
}

/// Returns whether the terminal condition holds.
const fn terminal(state: MaintenanceJobState) -> bool {
	matches!(
		state,
		MaintenanceJobState::Completed
			| MaintenanceJobState::Cancelled
			| MaintenanceJobState::Failed
	)
}

/// Result returned by one concrete maintenance build attempt.
pub enum MaintenanceRunOutcome {
	/// Completed case of maintenance run outcome.
	Completed,
	/// Retry case of maintenance run outcome.
	Retry,
	/// Cancelled case of maintenance run outcome.
	Cancelled,
}

#[cfg(test)]
/// Regression tests for maintenance behavior.
mod tests {
	use super::*;
	use crate::{
		datastore::{Datastore, LocalOptions},
		stats::{HistogramMethod, PropertyHistogramKey, StatisticsScope},
		values::Vector,
	};

	/// Process-local suffix counter that separates temporary maintenance-test
	/// directories.
	static NEXT_LOCAL_DIRECTORY: std::sync::atomic::AtomicU64 =
		std::sync::atomic::AtomicU64::new(0);

	/// Temporary directory owner used to keep test files alive for their
	/// scenario.
	struct TestDirectory(
		/// Path retained for the lifetime of the temporary directory owner.
		std::path::PathBuf,
	);

	impl TestDirectory {
		/// Constructs `TestDirectory` from the supplied initial state.
		fn new() -> Self {
			Self(std::env::temp_dir().join(format!(
				"fgdb-{:016x}",
				NEXT_LOCAL_DIRECTORY.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
			)))
		}
	}

	impl Drop for TestDirectory {
		/// Runs cleanup for this `TestDirectory` before its fields are
		/// released.
		fn drop(&mut self) {
			drop(std::fs::remove_dir_all(&self.0));
		}
	}

	/// Verifies pending index queue is bounded and coalesces to full rebuild.
	#[test]
	fn pending_index_queue_is_bounded_and_coalesces_to_full_rebuild() {
		use crate::test_assertions::AssertResult as _;

		let target = MaintenanceTarget::Index {
			class: ClassId::new(1)
				.ok_or_else(|| FerrosGrynnError::invalid_argument("invalid test ClassId"))
				.assert_ok(),
			index: IndexId::new(2)
				.ok_or_else(|| FerrosGrynnError::invalid_argument("invalid test IndexId"))
				.assert_ok(),
			version: SchemaVersion::INITIAL,
		};
		let mut state = CoordinatorState {
			event_wake: 0,
			jobs: BTreeMap::new(),
			queue: VecDeque::new(),
			pending_index_changes: VecDeque::new(),
			pending_full_rebuilds: BTreeMap::new(),
			cancelled: BTreeSet::new(),
			cancellation_tokens: BTreeMap::new(),
			stopping: false,
		};
		for revision in 1..=INDEX_PENDING_QUEUE_CAPACITY as u64 + 1 {
			state.record_index_change(target, revision);
		}
		assert_eq!(state.pending_index_changes.len(), INDEX_PENDING_QUEUE_CAPACITY);
		assert!(state.pending_full_rebuilds.contains_key(&target));
		assert!(state.has_index_change_after(target, INDEX_PENDING_QUEUE_CAPACITY as u64));
		state.acknowledge_index_changes(target, INDEX_PENDING_QUEUE_CAPACITY as u64 + 1);
		assert!(!state.has_index_change_after(target, 0));
	}

	/// Verifies maintenance retries when dml changes the built snapshot.
	#[test]
	fn maintenance_retries_when_dml_changes_the_built_snapshot() {
		use crate::test_assertions::AssertResult as _;

		let store = Datastore::in_memory();
		let setup = store
			.execute(
				"DEFINE ANALYZER words TOKENIZERS blank FILTERS lowercase; DEFINE CLASS document \
				 TYPE NODE SCHEMAFULL; DEFINE FIELD title ON CLASS document TYPE string; DEFINE \
				 FIELD embedding ON CLASS document TYPE vector<f32>; CREATE document:a SET title \
				 = 'old', embedding = $old; CREATE document:b SET title = 'other', embedding = \
				 $other; DEFINE INDEX title_search ON CLASS document FIELDS title FULLTEXT \
				 ANALYZER words HIGHLIGHTS SCOPE BASE; DEFINE INDEX embedding_disk ON CLASS \
				 document FIELDS embedding DISKANN DIMENSION 2 TYPE F32 DIST EUCLIDEAN DEGREE 4 \
				 L_BUILD 8 ALPHA 1.2 SCOPE BASE;",
				&Object::from_iter([
					("old", Value::Vector(Vector::F32(vec![10.0, 0.0]))),
					("other", Value::Vector(Vector::F32(vec![20.0, 0.0]))),
				]),
			)
			.assert_ok();
		if setup.iter().any(|result| result.result.is_err()) {
			return {
				let test_result: FerrosGrynnResult<()> = Err(FerrosGrynnError::invalid_argument(
					"maintenance revision-race fixture setup failed",
				));
				test_result.assert_ok();
			};
		}
		let class = store
			.class("document")
			.ok_or_else(|| FerrosGrynnError::class_does_not_exist("document"))
			.assert_ok();
		let class_id = class
			.id()
			.ok_or_else(|| FerrosGrynnError::invalid_storage_file("fixture class has no identity"))
			.assert_ok();
		let targets = [
			("title_search", "UPDATE document:a SET title = 'revision winner';", Object::new()),
			(
				"embedding_disk",
				"UPDATE document:b SET embedding = $updated;",
				Object::from_iter([("updated", Value::Vector(Vector::F32(vec![0.0, 0.0])))]),
			),
		];
		for (name, mutation, parameters) in targets {
			let index = class
				.indexes()
				.find(|index| index.name().as_str() == name)
				.ok_or_else(|| FerrosGrynnError::invalid_storage_file("fixture index is absent"))
				.assert_ok();
			let target = MaintenanceTarget::Index {
				class: class_id,
				index: index
					.id()
					.ok_or_else(|| {
						FerrosGrynnError::invalid_storage_file("fixture index has no identity")
					})
					.assert_ok(),
				version: index.version(),
			};
			let job = MaintenanceJobInfo::new(
				MaintenanceJobKind::RebuildIndex,
				target,
				store.inner.current_root().commit_ts,
				MaintenanceOptions::default(),
				None,
			);
			let id = job.id;
			{
				let mut state = store.inner.maintenance.shared.state.lock();
				state.cancellation_tokens.insert(id, Arc::new(AtomicBool::new(false)));
				state.jobs.insert(id, job);
			}
			begin_job(store.inner.maintenance.shared.as_ref(), store.inner.as_ref(), id)
				.assert_ok();
			let outcome = store
				.inner
				.run_maintenance_job_interleaved(id, || {
					let results = store.execute(mutation, &parameters)?;
					if results.iter().any(|result| result.result.is_err()) {
						return Err(FerrosGrynnError::invalid_argument(
							"interleaved DML failed during maintenance revision-race test",
						));
					}
					Ok(())
				})
				.assert_ok();
			if !matches!(outcome, MaintenanceRunOutcome::Retry) {
				return {
					let test_result: FerrosGrynnResult<()> =
						Err(FerrosGrynnError::invalid_argument(
							"maintenance published a candidate built from a stale snapshot",
						));
					test_result.assert_ok();
				};
			}
			finish_job(
				store.inner.maintenance.shared.as_ref(),
				store.inner.as_ref(),
				id,
				Ok(MaintenanceRunOutcome::Retry),
			);
			{
				let mut state = store.inner.maintenance.shared.state.lock();
				state.queue.retain(|queued| *queued != id);
				let job = state
					.jobs
					.get(&id)
					.ok_or_else(|| FerrosGrynnError::maintenance_job_not_found(id.to_string()))
					.assert_ok();
				assert_eq!(job.state(), MaintenanceJobState::Queued);
				assert_eq!(job.restart_count(), 1);
			}
			begin_job(store.inner.maintenance.shared.as_ref(), store.inner.as_ref(), id)
				.assert_ok();
			let outcome = store.inner.run_maintenance_job(id).assert_ok();
			if !matches!(outcome, MaintenanceRunOutcome::Completed) {
				return {
					let test_result: FerrosGrynnResult<()> =
						Err(FerrosGrynnError::invalid_argument(
							"maintenance retry did not publish the current snapshot",
						));
					test_result.assert_ok();
				};
			}
			finish_job(
				store.inner.maintenance.shared.as_ref(),
				store.inner.as_ref(),
				id,
				Ok(MaintenanceRunOutcome::Completed),
			);
			assert_eq!(
				store
					.job(id)
					.assert_ok()
					.ok_or_else(|| FerrosGrynnError::maintenance_job_not_found(id.to_string()))
					.assert_ok()
					.state(),
				MaintenanceJobState::Completed,
			);
			let root = store.inner.current_root();
			let built_class = root
				.schema
				.class_by_id(class_id)
				.ok_or_else(|| FerrosGrynnError::invalid_storage_file("fixture class is absent"))
				.assert_ok();
			let built = built_class
				.indexes()
				.find(|index| index.name().as_str() == name)
				.ok_or_else(|| FerrosGrynnError::invalid_storage_file("rebuilt index is absent"))
				.assert_ok();
			assert_eq!(built.build_revision(), root.commit_ts);
		}

		let query = Object::from_iter([("query", Value::Vector(Vector::F32(vec![0.0, 0.0])))]);
		for (forced, scan) in [
			(
				"SELECT VALUE id FROM document WHERE title @@ 'winner' ORDER BY id;",
				"SELECT VALUE id FROM document WITH NOINDEX WHERE title @@ 'winner' ORDER BY id;",
			),
			(
				"SELECT VALUE id FROM document WHERE embedding <| 1, EUCLIDEAN |> $query;",
				"SELECT VALUE id FROM document WITH NOINDEX WHERE embedding <| 1, EUCLIDEAN |> \
				 $query;",
			),
		] {
			let forced = store
				.execute(forced, &query)
				.assert_ok()
				.remove(0)
				.result
				.assert_ok()
				.into_parts()
				.0;
			let scan =
				store.execute(scan, &query).assert_ok().remove(0).result.assert_ok().into_parts().0;
			assert_eq!(forced, scan);
		}
	}

	/// Verifies fifo worker completes database compaction and stops cleanly.
	#[test]
	fn fifo_worker_completes_database_compaction_and_stops_cleanly() {
		use crate::test_assertions::AssertResult as _;

		let store = Datastore::in_memory();
		let setup = store
			.execute("DEFINE CLASS item TYPE NODE SCHEMALESS; CREATE item:one;", &Object::new())
			.assert_ok();
		assert!(setup.iter().all(|result| result.result.is_ok()));
		let id = store
			.enqueue_maintenance(
				MaintenanceJobKind::CompactDatabase,
				MaintenanceTarget::Database,
				None,
			)
			.assert_ok();
		for _ in 0..100_000 {
			let job = store
				.job(id)
				.assert_ok()
				.ok_or_else(|| FerrosGrynnError::maintenance_job_not_found(id.to_string()))
				.assert_ok();
			if job.state() == MaintenanceJobState::Completed {
				assert!(store.inner.current_root().commit_ts > 0);
				assert!(job.processed_items() >= 1);
				return;
			}
			if job.state() == MaintenanceJobState::Failed {
				return {
					let test_result: FerrosGrynnResult<()> =
						Err(FerrosGrynnError::invalid_argument(
							"maintenance worker failed its compaction test",
						));
					test_result.assert_ok();
				};
			}
			std::thread::yield_now();
		}
		let test_result: FerrosGrynnResult<()> = Err(FerrosGrynnError::invalid_argument(
			"maintenance worker did not finish within the bounded test loop",
		));
		test_result.assert_ok();
	}

	/// Verifies gql admin surface returns and resolves durable job ids.
	#[test]
	fn gql_admin_surface_returns_and_resolves_durable_job_ids() {
		use crate::test_assertions::AssertResult as _;

		let store = Datastore::in_memory();
		let graph_target = store
			.execute(
				"DEFINE GRAPH social; DEFINE CLASS person TYPE NODE SCHEMAFULL; DEFINE FIELD \
				 email ON CLASS person TYPE string; DEFINE INDEX by_email ON CLASS person FIELDS \
				 email SCOPE EFFECTIVE IN GRAPH social; COMPACT INDEX by_email ON GRAPH social;",
				&Object::new(),
			)
			.assert_ok();
		assert!(graph_target.iter().all(|result| result.result.is_ok()));
		let results = store
			.execute(
				"COMPACT DATABASE CONCURRENTLY WITH (SORT_MEM = 4096, PARALLELISM = 1)",
				&Object::new(),
			)
			.assert_ok();
		let result = results
			.first()
			.ok_or_else(|| {
				FerrosGrynnError::invalid_argument("COMPACT returned no statement result")
			})
			.assert_ok();
		let outcome = result.result.as_ref().map_err(Clone::clone).assert_ok();
		let Value::Uuid(job) = outcome.value() else {
			return {
				let test_result: FerrosGrynnResult<()> =
					Err(FerrosGrynnError::invalid_argument("CONCURRENTLY did not return a UUID"));
				test_result.assert_ok();
			};
		};
		let retained = store
			.job(JobId::from_uuid(*job))
			.assert_ok()
			.ok_or_else(|| FerrosGrynnError::maintenance_job_not_found(job.to_string()))
			.assert_ok();
		assert_eq!(retained.max_memory_bytes().get(), 4096);
		assert_eq!(retained.max_parallelism().get(), 1);
		let mut bindings = Object::new();
		bindings.insert("job", Value::from(*job));
		let info = store.execute("INFO FOR JOB $job", &bindings).assert_ok();
		let result = info
			.first()
			.ok_or_else(|| {
				FerrosGrynnError::invalid_argument("INFO FOR JOB returned no statement result")
			})
			.assert_ok();
		let outcome = result.result.as_ref().map_err(Clone::clone).assert_ok();
		if !matches!(outcome.value(), Value::Object(_)) {
			{
				let test_result: FerrosGrynnResult<()> = Err(FerrosGrynnError::invalid_argument(
					"INFO FOR JOB did not return an object",
				));
				test_result.assert_ok();
			}
		}
	}

	/// Verifies gql analyze supports scoped approximate histograms.
	#[test]
	fn gql_analyze_supports_scoped_approximate_histograms() {
		use crate::test_assertions::AssertResult as _;

		let store = Datastore::in_memory();
		let setup = store
			.execute(
				"DEFINE CLASS person TYPE NODE SCHEMAFULL; DEFINE FIELD age ON CLASS person TYPE \
				 int; CREATE person:one SET age = 42; ANALYZE CLASS person BASE APPROXIMATE \
				 SAMPLE 1024;",
				&Object::new(),
			)
			.assert_ok();
		assert!(setup.iter().all(|result| result.result.is_ok()));
		let class = store
			.class("person")
			.ok_or_else(|| FerrosGrynnError::class_does_not_exist("person"))
			.assert_ok();
		let class_id = class
			.id()
			.ok_or_else(|| FerrosGrynnError::invalid_storage_file("class identity absent"))
			.assert_ok();
		let field = class
			.fields()
			.next()
			.ok_or_else(|| FerrosGrynnError::invalid_storage_file("field absent"))
			.assert_ok();
		let field_id = field
			.id()
			.ok_or_else(|| FerrosGrynnError::invalid_storage_file("field identity absent"))
			.assert_ok();
		let root = store.inner.current_root();
		let histogram = root
			.statistics
			.base()
			.property_histograms
			.get(&PropertyHistogramKey {
				class_id,
				field_id,
				scope: StatisticsScope::Base,
			})
			.ok_or_else(|| FerrosGrynnError::invalid_storage_file("histogram absent"))
			.assert_ok();
		assert_eq!(histogram.method, HistogramMethod::Approximate);
		assert_eq!(histogram.sample_count, 1);

		let automatic =
			store.execute("ANALYZE CLASS person BASE CONCURRENTLY;", &Object::new()).assert_ok();
		let Value::Uuid(job_id) = automatic
			.first()
			.and_then(|result| result.result.as_ref().ok())
			.map(crate::gql_status::Outcome::value)
			.ok_or_else(|| FerrosGrynnError::invalid_argument("ANALYZE returned no job value"))
			.assert_ok()
		else {
			return {
				let test_result: FerrosGrynnResult<()> = Err(FerrosGrynnError::invalid_argument(
					"concurrent ANALYZE did not return a UUID",
				));
				test_result.assert_ok();
			};
		};
		let automatic = store
			.job(JobId::from_uuid(*job_id))
			.assert_ok()
			.ok_or_else(|| FerrosGrynnError::maintenance_job_not_found(job_id.to_string()))
			.assert_ok();
		assert_eq!(automatic.analyze_mode(), Some("full"));
		assert_eq!(automatic.analyze_sample_count(), None);

		let invalid = store
			.execute("ANALYZE CLASS person BASE APPROXIMATE SAMPLE 1023;", &Object::new())
			.assert_ok();
		assert!(invalid.first().is_some_and(|result| result.result.is_err()));
		assert!(store.execute("ANALYZE DATABASE;", &Object::new()).is_err());
		assert!(store.execute("ANALYZE INDEX by_age ON CLASS person;", &Object::new()).is_err());
	}

	/// Verifies changefeed purge rechecks revision cancellation and job kind.
	#[test]
	fn changefeed_purge_rechecks_revision_cancellation_and_job_kind() {
		use crate::{
			changefeed::{ChangefeedConfig, ChangefeedSource},
			datastore::{ClassSchema, changefeed_retention::TEST_TIME},
			graph::AtomKind,
			test_assertions::AssertResult as _,
			values::{PropertyMap, RecordId},
		};
		/// Test guard restoring the previously injected changefeed timestamp.
		struct Clock(
			/// Previous injected timestamp restored when the test guard is
			/// dropped.
			Option<i128>,
		);
		impl Drop for Clock {
			/// Runs cleanup for this `Clock` before its fields are released.
			fn drop(&mut self) {
				TEST_TIME.set(self.0);
			}
		}
		let store = Datastore::in_memory();
		let mut seed = store.begin();
		let class =
			seed.define_class(ClassSchema::new("n", AtomKind::Node, []).assert_ok()).assert_ok();
		let key = seed.intern_property("value").assert_ok();
		let record: RecordId = "n:a".parse().assert_ok();
		seed.create_atom(record.clone(), PropertyMap::new()).assert_ok();
		let ttl = 86_400_000_000_000;
		let feed = seed
			.configure_changefeed(
				ChangefeedSource::Class(class),
				ChangefeedConfig::from_nanoseconds(ttl).assert_ok(),
			)
			.assert_ok();
		seed.commit().assert_ok();
		let _clock =
			Clock(TEST_TIME.replace(Some(store.inner.current_root().committed_at_ns + ttl)));
		let register = |kind| -> FerrosGrynnResult<JobId> {
			let job = MaintenanceJobInfo::new(
				kind,
				MaintenanceTarget::Database,
				store.inner.current_root().commit_ts,
				MaintenanceOptions::default(),
				None,
			);
			let id = job.id;
			{
				let mut state = store.inner.maintenance.shared.state.lock();
				state.cancellation_tokens.insert(id, Arc::new(AtomicBool::new(false)));
				state.jobs.insert(id, job);
			}
			begin_job(store.inner.maintenance.shared.as_ref(), store.inner.as_ref(), id)?;
			Ok(id)
		};
		let race = register(MaintenanceJobKind::PurgeChangefeed).assert_ok();
		let outcome = store
			.inner
			.run_maintenance_job_interleaved(race, || {
				let mut tx = store.begin();
				tx.set_base_property(&record, key, 2.into())?;
				tx.commit()?;
				Ok(())
			})
			.assert_ok();
		assert!(matches!(outcome, MaintenanceRunOutcome::Retry));
		assert_eq!(store.begin().changefeed_info(feed).assert_ok().purged_through().get(), 0);
		finish_job(
			store.inner.maintenance.shared.as_ref(),
			store.inner.as_ref(),
			race,
			Ok(MaintenanceRunOutcome::Cancelled),
		);
		let cancelled = register(MaintenanceJobKind::PurgeChangefeed).assert_ok();
		let stamp = store.inner.current_root().commit_ts;
		let outcome = store
			.inner
			.run_maintenance_job_interleaved(cancelled, || {
				store.kill_job(cancelled)?;
				Ok(())
			})
			.assert_ok();
		assert!(matches!(outcome, MaintenanceRunOutcome::Cancelled));
		assert_eq!(store.inner.current_root().commit_ts, stamp);
		assert_eq!(store.begin().changefeed_info(feed).assert_ok().purged_through().get(), 0);
		finish_job(
			store.inner.maintenance.shared.as_ref(),
			store.inner.as_ref(),
			cancelled,
			Ok(MaintenanceRunOutcome::Cancelled),
		);
		let compact = register(MaintenanceJobKind::CompactDatabase).assert_ok();
		let purge = register(MaintenanceJobKind::PurgeChangefeed).assert_ok();
		assert_eq!(store.purge_changefeeds(None).assert_ok(), purge);
		assert_eq!(
			store
				.inner
				.maintenance
				.enqueue_coalesced(
					&store.inner,
					MaintenanceJobKind::CompactDatabase,
					MaintenanceTarget::Database,
					MaintenanceOptions::default(),
					None
				)
				.assert_ok(),
			compact
		);
	}

	/// Verifies tensor maintenance rejects rooted revision races and
	/// cancellation.
	#[test]
	fn tensor_maintenance_rejects_rooted_revision_races_and_cancellation() {
		use crate::{
			datastore::{ClassSchema, GraphIndex, TensorCacheFormat},
			graph::{AtomKind, GraphKind, tensor::TensorScope},
			test_assertions::AssertResult as _,
			values::{PropertyMap, RecordId},
		};
		let store = Datastore::in_memory();
		let mut seed = store.begin();
		seed.define_class(ClassSchema::new("n", AtomKind::Node, []).assert_ok()).assert_ok();
		let child = seed.define_graph("child", GraphKind::Graph).assert_ok();
		let parent = seed.define_graph("parent", GraphKind::Graph).assert_ok();
		let owner: RecordId = "n:owner".parse().assert_ok();
		seed.create_atom(owner.clone(), PropertyMap::new()).assert_ok();
		seed.attach_node(parent.id(), &owner, None, Some(child.id())).assert_ok();
		let index = seed
			.define_graph_index(
				"parent",
				GraphIndex::new_tensor(
					"tensor",
					TensorScope::Rooted,
					TensorCacheFormat::Auto,
					1 << 20,
				)
				.assert_ok(),
			)
			.assert_ok();
		seed.commit().assert_ok();
		let target = MaintenanceTarget::GraphIndex {
			graph: parent.id(),
			index,
			version: SchemaVersion::INITIAL,
		};
		let register = || -> FerrosGrynnResult<JobId> {
			let job = MaintenanceJobInfo::new(
				MaintenanceJobKind::RebuildIndex,
				target,
				store.inner.current_root().commit_ts,
				MaintenanceOptions::default(),
				None,
			);
			let id = job.id;
			{
				let mut state = store.inner.maintenance.shared.state.lock();
				state.cancellation_tokens.insert(id, Arc::new(AtomicBool::new(false)));
				state.jobs.insert(id, job);
			}
			begin_job(store.inner.maintenance.shared.as_ref(), store.inner.as_ref(), id)?;
			Ok(id)
		};
		let race = register().assert_ok();
		let outcome = store
			.inner
			.run_maintenance_job_interleaved(race, || {
				let mut write = store.begin();
				let record: RecordId = "n:child".parse()?;
				write.create_atom(record.clone(), PropertyMap::new())?;
				write.attach_node(child.id(), &record, None, None)?;
				write.commit()?;
				Ok(())
			})
			.assert_ok();
		assert!(matches!(outcome, MaintenanceRunOutcome::Retry));
		assert!(!store.inner.current_root().tensor_artifacts.contains_key(&index));
		finish_job(
			store.inner.maintenance.shared.as_ref(),
			store.inner.as_ref(),
			race,
			Ok(MaintenanceRunOutcome::Cancelled),
		);
		let cancelled = register().assert_ok();
		let revision = store.inner.current_root().commit_ts;
		let outcome = store
			.inner
			.run_maintenance_job_interleaved(cancelled, || {
				store.kill_job(cancelled)?;
				Ok(())
			})
			.assert_ok();
		assert!(matches!(outcome, MaintenanceRunOutcome::Cancelled));
		assert_eq!(store.inner.current_root().commit_ts, revision);
		assert!(!store.inner.current_root().tensor_artifacts.contains_key(&index));
		finish_job(
			store.inner.maintenance.shared.as_ref(),
			store.inner.as_ref(),
			cancelled,
			Ok(MaintenanceRunOutcome::Cancelled),
		);
		store.rebuild_graph_index(parent.id(), index, MaintenanceOptions::default()).assert_ok();
		assert!(store.inner.current_root().tensor_artifacts.contains_key(&index));
	}

	/// Verifies recovery cancels orphans and requires exact index build
	/// revision.
	#[test]
	fn recovery_cancels_orphans_and_requires_exact_index_build_revision() {
		use crate::test_assertions::AssertResult as _;

		let store = Datastore::in_memory();
		let results = store
			.execute(
				"DEFINE CLASS person TYPE NODE SCHEMAFULL; DEFINE FIELD age ON CLASS person TYPE \
				 int; DEFINE INDEX by_age ON CLASS person FIELDS age SCOPE BASE;",
				&Object::new(),
			)
			.assert_ok();
		if results.iter().any(|result| result.result.is_err()) {
			return {
				let test_result: FerrosGrynnResult<()> =
					Err(FerrosGrynnError::invalid_argument("recovery fixture schema setup failed"));
				test_result.assert_ok();
			};
		}
		let class = store
			.class("person")
			.ok_or_else(|| FerrosGrynnError::class_does_not_exist("person"))
			.assert_ok();
		let class_id = class
			.id()
			.ok_or_else(|| FerrosGrynnError::invalid_storage_file("fixture class has no identity"))
			.assert_ok();
		let index = class
			.indexes()
			.next()
			.ok_or_else(|| FerrosGrynnError::invalid_storage_file("fixture class has no index"))
			.assert_ok();
		let index_id = index
			.id()
			.ok_or_else(|| FerrosGrynnError::invalid_storage_file("fixture index has no identity"))
			.assert_ok();
		let target = MaintenanceTarget::Index {
			class: class_id,
			index: index_id,
			version: index.version(),
		};

		let mut published = MaintenanceJobInfo::new(
			MaintenanceJobKind::RebuildIndex,
			target,
			store.inner.current_root().commit_ts,
			MaintenanceOptions::default(),
			None,
		);
		published.state = MaintenanceJobState::Publishing;
		published.candidate_fingerprint = Some(index.build_revision());
		let published_id = published.id();

		let mut interrupted = MaintenanceJobInfo::new(
			MaintenanceJobKind::RebuildIndex,
			target,
			store.inner.current_root().commit_ts,
			MaintenanceOptions::default(),
			None,
		);
		interrupted.state = MaintenanceJobState::Publishing;
		interrupted.candidate_fingerprint = Some(index.build_revision().saturating_add(1));
		let interrupted_id = interrupted.id();

		let missing_graph = GraphId::new(u64::MAX)
			.ok_or_else(|| FerrosGrynnError::invalid_argument("invalid fixture graph identity"))
			.assert_ok();
		let orphan = MaintenanceJobInfo::new(
			MaintenanceJobKind::CompactGraph,
			MaintenanceTarget::Graph(missing_graph),
			store.inner.current_root().commit_ts,
			MaintenanceOptions::default(),
			None,
		);
		let orphan_id = orphan.id();

		let coordinator = MaintenanceCoordinator::new(vec![published, interrupted, orphan]);
		coordinator.reconcile(&store.inner).assert_ok();
		assert_eq!(
			coordinator.job(published_id).map(|job| job.state()),
			Some(MaintenanceJobState::Completed)
		);
		let interrupted = coordinator
			.job(interrupted_id)
			.ok_or_else(|| FerrosGrynnError::maintenance_job_not_found(interrupted_id.to_string()))
			.assert_ok();
		assert_eq!(interrupted.state(), MaintenanceJobState::Queued);
		assert_eq!(interrupted.restart_count(), 1);
		assert_eq!(
			coordinator.job(orphan_id).map(|job| job.state()),
			Some(MaintenanceJobState::Cancelled)
		);
	}

	/// Verifies local reopen reconciles published interrupted and orphan jobs.
	#[test]
	fn local_reopen_reconciles_published_interrupted_and_orphan_jobs() {
		use crate::test_assertions::AssertResult as _;

		let directory = TestDirectory::new();
		let options = LocalOptions::default();
		let store = Datastore::create_local(&directory.0, options).assert_ok();
		let setup = store
			.execute(
				"DEFINE CLASS document TYPE NODE SCHEMAFULL; DEFINE FIELD embedding ON CLASS \
				 document TYPE vector<f32>; CREATE document:a SET embedding = $a; CREATE \
				 document:b SET embedding = $b; DEFINE INDEX embedding_hnsw ON CLASS document \
				 FIELDS embedding HNSW DIMENSION 2 TYPE F32 DIST EUCLIDEAN EFC 16 M 4 SCOPE BASE; \
				 DEFINE INDEX embedding_disk ON CLASS document FIELDS embedding DISKANN DIMENSION \
				 2 TYPE F32 DIST EUCLIDEAN DEGREE 4 L_BUILD 8 ALPHA 1.2 SCOPE BASE;",
				&Object::from_iter([
					("a", Value::Vector(Vector::F32(vec![0.0, 0.0]))),
					("b", Value::Vector(Vector::F32(vec![1.0, 0.0]))),
				]),
			)
			.assert_ok();
		if setup.iter().any(|result| result.result.is_err()) {
			return {
				let test_result: FerrosGrynnResult<()> =
					Err(FerrosGrynnError::invalid_argument(format!(
						"local maintenance recovery fixture setup failed: {:?}",
						setup
							.iter()
							.filter_map(|result| result.result.as_ref().err())
							.collect::<Vec<_>>()
					)));
				test_result.assert_ok();
			};
		}
		let class = store
			.class("document")
			.ok_or_else(|| FerrosGrynnError::class_does_not_exist("document"))
			.assert_ok();
		let class_id = class
			.id()
			.ok_or_else(|| FerrosGrynnError::invalid_storage_file("fixture class has no identity"))
			.assert_ok();
		let hnsw = class
			.indexes()
			.find(|index| index.name().as_str() == "embedding_hnsw")
			.ok_or_else(|| FerrosGrynnError::invalid_storage_file("fixture HNSW index is absent"))
			.assert_ok();
		let disk_ann = class
			.indexes()
			.find(|index| index.name().as_str() == "embedding_disk")
			.ok_or_else(|| {
				FerrosGrynnError::invalid_storage_file("fixture DiskANN index is absent")
			})
			.assert_ok();
		let hnsw_target = MaintenanceTarget::Index {
			class: class_id,
			index: hnsw
				.id()
				.ok_or_else(|| {
					FerrosGrynnError::invalid_storage_file("fixture HNSW index has no identity")
				})
				.assert_ok(),
			version: hnsw.version(),
		};
		let disk_ann_target = MaintenanceTarget::Index {
			class: class_id,
			index: disk_ann
				.id()
				.ok_or_else(|| {
					FerrosGrynnError::invalid_storage_file("fixture DiskANN index has no identity")
				})
				.assert_ok(),
			version: disk_ann.version(),
		};
		let source_revision = store.inner.current_root().commit_ts;
		let mut published = MaintenanceJobInfo::new(
			MaintenanceJobKind::RebuildIndex,
			hnsw_target,
			source_revision,
			MaintenanceOptions::default(),
			None,
		);
		published.state = MaintenanceJobState::Publishing;
		published.candidate_fingerprint = Some(hnsw.build_revision());
		let published_id = published.id();
		let mut interrupted = MaintenanceJobInfo::new(
			MaintenanceJobKind::RebuildIndex,
			disk_ann_target,
			source_revision,
			MaintenanceOptions::default(),
			None,
		);
		interrupted.state = MaintenanceJobState::Publishing;
		interrupted.candidate_fingerprint = Some(disk_ann.build_revision().saturating_add(1));
		let interrupted_id = interrupted.id();
		let missing_graph = GraphId::new(u64::MAX)
			.ok_or_else(|| FerrosGrynnError::invalid_argument("invalid fixture graph identity"))
			.assert_ok();
		let orphan = MaintenanceJobInfo::new(
			MaintenanceJobKind::CompactGraph,
			MaintenanceTarget::Graph(missing_graph),
			source_revision,
			MaintenanceOptions::default(),
			None,
		);
		let orphan_id = orphan.id();
		store.inner.persist_maintenance_jobs(&[published, interrupted, orphan]).assert_ok();
		drop(store);

		let reopened = Datastore::open_local(&directory.0, options).assert_ok();
		let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
		loop {
			let published = reopened
				.job(published_id)
				.assert_ok()
				.ok_or_else(|| {
					FerrosGrynnError::maintenance_job_not_found(published_id.to_string())
				})
				.assert_ok();
			let interrupted = reopened
				.job(interrupted_id)
				.assert_ok()
				.ok_or_else(|| {
					FerrosGrynnError::maintenance_job_not_found(interrupted_id.to_string())
				})
				.assert_ok();
			let orphan = reopened
				.job(orphan_id)
				.assert_ok()
				.ok_or_else(|| FerrosGrynnError::maintenance_job_not_found(orphan_id.to_string()))
				.assert_ok();
			if published.state() == MaintenanceJobState::Completed
				&& interrupted.state() == MaintenanceJobState::Completed
				&& orphan.state() == MaintenanceJobState::Cancelled
			{
				assert_eq!(interrupted.restart_count(), 1);
				break;
			}
			if std::time::Instant::now() >= deadline {
				return {
					let test_result: FerrosGrynnResult<()> =
						Err(FerrosGrynnError::invalid_argument(format!(
							"local maintenance reconciliation timed out: {published:?}, \
							 {interrupted:?}, {orphan:?}",
						)));
					test_result.assert_ok();
				};
			}
			std::thread::sleep(std::time::Duration::from_millis(1));
		}
	}
}
