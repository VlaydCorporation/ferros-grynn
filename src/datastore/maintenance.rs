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
pub struct JobId(Uuid);

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
	fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		Display::fmt(&self.0, formatter)
	}
}

/// Operation performed by one maintenance job.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum MaintenanceJobKind {
	BuildIndex,
	RebuildIndex,
	DeferredIndexCatchUp,
	Analyze,
	CompactIndex,
	CompactGraph,
	CompactDatabase,
}

/// Durable lifecycle state of one maintenance job.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum MaintenanceJobState {
	Queued,
	BuildingInitial,
	CatchingUp,
	Publishing,
	Completed,
	Cancelled,
	Failed,
	Interrupted,
}

/// Statistics construction mode retained by an `ANALYZE` job.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MaintenanceAnalyzeMode {
	Automatic,
	Full,
	Approximate {
		sample_count: u32,
	},
}

/// Logical target retained by the durable job ledger.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) enum MaintenanceTarget {
	Index {
		class: ClassId,
		index: IndexId,
		version: SchemaVersion,
	},
	BaseClass(ClassId),
	EffectiveClass {
		class: ClassId,
		graph: GraphId,
	},
	Graph(GraphId),
	Database,
}

/// Immutable public projection of one durable job record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MaintenanceJobInfo {
	pub(crate) id: JobId,
	pub(crate) kind: MaintenanceJobKind,
	pub(crate) state: MaintenanceJobState,
	pub(crate) target: MaintenanceTarget,
	pub(crate) source_revision: u64,
	pub(crate) processed_items: u64,
	pub(crate) written_bytes: u64,
	pub(crate) sort_runs: u64,
	pub(crate) restart_count: u32,
	pub(crate) max_memory_bytes: NonZeroUsize,
	pub(crate) max_parallelism: NonZeroUsize,
	pub(crate) candidate_fingerprint: Option<u64>,
	pub(crate) analyze_mode: Option<MaintenanceAnalyzeMode>,
	pub(crate) terminal_status: Option<StatusCode>,
	pub(crate) diagnostic: Option<String>,
	pub(crate) created_at: SystemTime,
	pub(crate) updated_at: SystemTime,
}

impl MaintenanceJobInfo {
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

	pub(crate) fn value(&self) -> Value {
		Value::from(Object::from_iter([
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
		]))
	}
}

const fn job_kind_name(kind: MaintenanceJobKind) -> &'static str {
	match kind {
		MaintenanceJobKind::BuildIndex => "build_index",
		MaintenanceJobKind::RebuildIndex => "rebuild_index",
		MaintenanceJobKind::DeferredIndexCatchUp => "deferred_index_catch_up",
		MaintenanceJobKind::Analyze => "analyze",
		MaintenanceJobKind::CompactIndex => "compact_index",
		MaintenanceJobKind::CompactGraph => "compact_graph",
		MaintenanceJobKind::CompactDatabase => "compact_database",
	}
}

const fn job_state_name(state: MaintenanceJobState) -> &'static str {
	match state {
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
	fn new(
		kind: MaintenanceJobKind,
		target: MaintenanceTarget,
		source_revision: u64,
		options: MaintenanceOptions,
		analyze_mode: Option<MaintenanceAnalyzeMode>,
	) -> Self {
		let now = SystemTime::now();
		Self {
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
	pub(crate) fn display_name(self) -> String {
		match self {
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
struct CoordinatorState {
	jobs: BTreeMap<JobId, MaintenanceJobInfo>,
	queue: VecDeque<JobId>,
	pending_index_changes: VecDeque<(MaintenanceTarget, u64)>,
	pending_full_rebuilds: BTreeMap<MaintenanceTarget, u64>,
	cancelled: BTreeSet<JobId>,
	cancellation_tokens: BTreeMap<JobId, Arc<AtomicBool>>,
	stopping: bool,
}

const INDEX_PENDING_QUEUE_CAPACITY: usize = 65_536;

impl CoordinatorState {
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

	fn has_index_change_after(&self, target: MaintenanceTarget, revision: u64) -> bool {
		self.pending_full_rebuilds.get(&target).is_some_and(|latest| *latest > revision)
			|| self
				.pending_index_changes
				.iter()
				.any(|(candidate, changed_at)| *candidate == target && *changed_at > revision)
	}

	fn acknowledge_index_changes(&mut self, target: MaintenanceTarget, revision: u64) {
		self.pending_index_changes
			.retain(|(candidate, changed_at)| *candidate != target || *changed_at > revision);
		if self.pending_full_rebuilds.get(&target).is_some_and(|latest| *latest <= revision) {
			self.pending_full_rebuilds.remove(&target);
		}
	}
}

struct CoordinatorShared {
	state: Mutex<CoordinatorState>,
	wake: Condvar,
}

/// One owned FIFO maintenance worker and its durable state mirror.
pub(crate) struct MaintenanceCoordinator {
	shared: Arc<CoordinatorShared>,
	worker: Mutex<Option<JoinHandle<()>>>,
}

impl MaintenanceCoordinator {
	pub(crate) fn new(recovered: Vec<MaintenanceJobInfo>) -> Self {
		let jobs: BTreeMap<JobId, MaintenanceJobInfo> =
			recovered.into_iter().map(|job| (job.id, job)).collect();
		let cancellation_tokens =
			jobs.keys().copied().map(|id| (id, Arc::new(AtomicBool::new(false)))).collect();
		Self {
			shared: Arc::new(CoordinatorShared {
				state: Mutex::new(CoordinatorState {
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
		if changed {
			if let Err(error) = persist_state(store, &state) {
				*state = previous;
				return Err(error);
			}
		}
		Ok(())
	}

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
		if let Some(existing) =
			state.jobs.values().find(|job| job.target == target && !terminal(job.state))
		{
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

	pub(crate) fn has_pending_index_change_after(
		&self,
		target: MaintenanceTarget,
		revision: u64,
	) -> bool {
		self.shared.state.lock().has_index_change_after(target, revision)
	}

	pub(crate) fn acknowledge_index_changes(&self, target: MaintenanceTarget, revision: u64) {
		self.shared.state.lock().acknowledge_index_changes(target, revision);
	}

	pub(crate) fn job(&self, id: JobId) -> Option<MaintenanceJobInfo> {
		self.shared.state.lock().jobs.get(&id).cloned()
	}

	pub(crate) fn jobs(&self) -> Vec<MaintenanceJobInfo> {
		self.shared.state.lock().jobs.values().cloned().collect()
	}

	pub(crate) fn active_job_for(&self, target: MaintenanceTarget) -> Option<MaintenanceJobInfo> {
		self.shared
			.state
			.lock()
			.jobs
			.values()
			.find(|job| job.target == target && !terminal(job.state))
			.cloned()
	}

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
		Ok(state
			.jobs
			.get(&id)
			.cloned()
			.ok_or_else(|| FerrosGrynnError::maintenance_job_not_found(id.to_string()))?)
	}

	pub(crate) fn is_cancelled(&self, id: JobId) -> bool {
		let state = self.shared.state.lock();
		state.stopping
			|| state.cancelled.contains(&id)
			|| state.cancellation_tokens.get(&id).is_some_and(|token| token.load(Ordering::Acquire))
	}

	pub(crate) fn cancellation_token(&self, id: JobId) -> Arc<AtomicBool> {
		self.shared
			.state
			.lock()
			.cancellation_tokens
			.get(&id)
			.cloned()
			.unwrap_or_else(|| Arc::new(AtomicBool::new(true)))
	}

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
		if let Some(handle) = handle {
			if handle.thread().id() != std::thread::current().id() {
				drop(handle.join());
			}
		}
	}
}

fn worker_loop(shared: &Arc<CoordinatorShared>, store: &Weak<StoreInner>) {
	loop {
		let id = {
			let mut state = shared.state.lock();
			while state.queue.is_empty() && !state.stopping {
				shared.wake.wait(&mut state);
			}
			if state.stopping {
				return;
			}
			match state.queue.pop_front() {
				Some(id) => id,
				None => continue,
			}
		};
		let Some(store) = store.upgrade() else {
			return;
		};
		if begin_job(shared, store.as_ref(), id).is_err() {
			continue;
		}
		let result = store.run_maintenance_job(id);
		finish_job(shared, store.as_ref(), id, result);
	}
}

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

fn persist_state(store: &StoreInner, state: &CoordinatorState) -> FerrosGrynnResult<()> {
	store.persist_maintenance_jobs(&state.jobs.values().cloned().collect::<Vec<_>>())
}

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

const fn terminal(state: MaintenanceJobState) -> bool {
	matches!(
		state,
		MaintenanceJobState::Completed
			| MaintenanceJobState::Cancelled
			| MaintenanceJobState::Failed
	)
}

/// Result returned by one concrete maintenance build attempt.
pub(crate) enum MaintenanceRunOutcome {
	Completed,
	Retry,
	Cancelled,
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::{
		datastore::{Datastore, LocalOptions},
		stats::{HistogramMethod, PropertyHistogramKey, StatisticsScope},
		values::Vector,
	};

	static NEXT_LOCAL_DIRECTORY: std::sync::atomic::AtomicU64 =
		std::sync::atomic::AtomicU64::new(0);

	struct TestDirectory(std::path::PathBuf);

	impl TestDirectory {
		fn new() -> Self {
			Self(std::env::temp_dir().join(format!(
				"fgdb-{:016x}",
				NEXT_LOCAL_DIRECTORY.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
			)))
		}
	}

	impl Drop for TestDirectory {
		fn drop(&mut self) {
			drop(std::fs::remove_dir_all(&self.0));
		}
	}

	#[test]
	fn pending_index_queue_is_bounded_and_coalesces_to_full_rebuild() -> FerrosGrynnResult<()> {
		let target = MaintenanceTarget::Index {
			class: ClassId::new(1)
				.ok_or_else(|| FerrosGrynnError::invalid_argument("invalid test ClassId"))?,
			index: IndexId::new(2)
				.ok_or_else(|| FerrosGrynnError::invalid_argument("invalid test IndexId"))?,
			version: SchemaVersion::INITIAL,
		};
		let mut state = CoordinatorState {
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
		Ok(())
	}

	#[test]
	fn maintenance_retries_when_dml_changes_the_built_snapshot() -> FerrosGrynnResult<()> {
		let store = Datastore::in_memory();
		let setup = store.execute(
			"DEFINE ANALYZER words TOKENIZERS blank FILTERS lowercase; DEFINE CLASS document TYPE \
			 NODE SCHEMAFULL; DEFINE FIELD title ON CLASS document TYPE string; DEFINE FIELD \
			 embedding ON CLASS document TYPE vector<f32>; CREATE document:a SET title = 'old', \
			 embedding = $old; CREATE document:b SET title = 'other', embedding = $other; DEFINE \
			 INDEX title_search ON CLASS document FIELDS title FULLTEXT ANALYZER words HIGHLIGHTS \
			 SCOPE BASE; DEFINE INDEX embedding_disk ON CLASS document FIELDS embedding DISKANN \
			 DIMENSION 2 TYPE F32 DIST EUCLIDEAN DEGREE 4 L_BUILD 8 ALPHA 1.2 SCOPE BASE;",
			&Object::from_iter([
				("old", Value::Vector(Vector::F32(vec![10.0, 0.0]))),
				("other", Value::Vector(Vector::F32(vec![20.0, 0.0]))),
			]),
		)?;
		if setup.iter().any(|result| result.result.is_err()) {
			return Err(FerrosGrynnError::invalid_argument(
				"maintenance revision-race fixture setup failed",
			));
		}
		let class = store
			.class("document")
			.ok_or_else(|| FerrosGrynnError::class_does_not_exist("document"))?;
		let class_id = class.id().ok_or_else(|| {
			FerrosGrynnError::invalid_storage_file("fixture class has no identity")
		})?;
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
				.ok_or_else(|| FerrosGrynnError::invalid_storage_file("fixture index is absent"))?;
			let target = MaintenanceTarget::Index {
				class: class_id,
				index: index.id().ok_or_else(|| {
					FerrosGrynnError::invalid_storage_file("fixture index has no identity")
				})?,
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
			begin_job(store.inner.maintenance.shared.as_ref(), store.inner.as_ref(), id)?;
			let outcome = store.inner.run_maintenance_job_interleaved(id, || {
				let results = store.execute(mutation, &parameters)?;
				if results.iter().any(|result| result.result.is_err()) {
					return Err(FerrosGrynnError::invalid_argument(
						"interleaved DML failed during maintenance revision-race test",
					));
				}
				Ok(())
			})?;
			if !matches!(outcome, MaintenanceRunOutcome::Retry) {
				return Err(FerrosGrynnError::invalid_argument(
					"maintenance published a candidate built from a stale snapshot",
				));
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
					.ok_or_else(|| FerrosGrynnError::maintenance_job_not_found(id.to_string()))?;
				assert_eq!(job.state(), MaintenanceJobState::Queued);
				assert_eq!(job.restart_count(), 1);
			}
			begin_job(store.inner.maintenance.shared.as_ref(), store.inner.as_ref(), id)?;
			let outcome = store.inner.run_maintenance_job(id)?;
			if !matches!(outcome, MaintenanceRunOutcome::Completed) {
				return Err(FerrosGrynnError::invalid_argument(
					"maintenance retry did not publish the current snapshot",
				));
			}
			finish_job(
				store.inner.maintenance.shared.as_ref(),
				store.inner.as_ref(),
				id,
				Ok(MaintenanceRunOutcome::Completed),
			);
			assert_eq!(
				store
					.job(id)?
					.ok_or_else(|| FerrosGrynnError::maintenance_job_not_found(id.to_string()))?
					.state(),
				MaintenanceJobState::Completed,
			);
			let root = store.inner.current_root();
			let built_class = root
				.schema
				.class_by_id(class_id)
				.ok_or_else(|| FerrosGrynnError::invalid_storage_file("fixture class is absent"))?;
			let built = built_class
				.indexes()
				.find(|index| index.name().as_str() == name)
				.ok_or_else(|| FerrosGrynnError::invalid_storage_file("rebuilt index is absent"))?;
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
			let forced = store.execute(forced, &query)?.remove(0).result?.into_parts().0;
			let scan = store.execute(scan, &query)?.remove(0).result?.into_parts().0;
			assert_eq!(forced, scan);
		}
		Ok(())
	}

	#[test]
	fn fifo_worker_completes_database_compaction_and_stops_cleanly() -> FerrosGrynnResult<()> {
		let store = Datastore::in_memory();
		let setup = store
			.execute("DEFINE CLASS item TYPE NODE SCHEMALESS; CREATE item:one;", &Object::new())?;
		assert!(setup.iter().all(|result| result.result.is_ok()));
		let id = store.enqueue_maintenance(
			MaintenanceJobKind::CompactDatabase,
			MaintenanceTarget::Database,
			None,
		)?;
		for _ in 0..100_000 {
			let job = store
				.job(id)?
				.ok_or_else(|| FerrosGrynnError::maintenance_job_not_found(id.to_string()))?;
			if job.state() == MaintenanceJobState::Completed {
				assert!(store.inner.current_root().commit_ts > 0);
				assert!(job.processed_items() >= 1);
				return Ok(());
			}
			if job.state() == MaintenanceJobState::Failed {
				return Err(FerrosGrynnError::invalid_argument(
					"maintenance worker failed its compaction test",
				));
			}
			std::thread::yield_now();
		}
		Err(FerrosGrynnError::invalid_argument(
			"maintenance worker did not finish within the bounded test loop",
		))
	}

	#[test]
	fn gql_admin_surface_returns_and_resolves_durable_job_ids() -> FerrosGrynnResult<()> {
		let store = Datastore::in_memory();
		let graph_target = store.execute(
			"DEFINE GRAPH social; DEFINE CLASS person TYPE NODE SCHEMAFULL; DEFINE FIELD email ON \
			 CLASS person TYPE string; DEFINE INDEX by_email ON CLASS person FIELDS email SCOPE \
			 EFFECTIVE IN GRAPH social; COMPACT INDEX by_email ON GRAPH social;",
			&Object::new(),
		)?;
		assert!(graph_target.iter().all(|result| result.result.is_ok()));
		let results = store.execute(
			"COMPACT DATABASE CONCURRENTLY WITH (SORT_MEM = 4096, PARALLELISM = 1)",
			&Object::new(),
		)?;
		let result = results.first().ok_or_else(|| {
			FerrosGrynnError::invalid_argument("COMPACT returned no statement result")
		})?;
		let outcome = result.result.as_ref().map_err(Clone::clone)?;
		let Value::Uuid(job) = outcome.value() else {
			return Err(FerrosGrynnError::invalid_argument("CONCURRENTLY did not return a UUID"));
		};
		let retained = store
			.job(JobId::from_uuid(*job))?
			.ok_or_else(|| FerrosGrynnError::maintenance_job_not_found(job.to_string()))?;
		assert_eq!(retained.max_memory_bytes().get(), 4096);
		assert_eq!(retained.max_parallelism().get(), 1);
		let mut bindings = Object::new();
		bindings.insert("job", Value::from(*job));
		let info = store.execute("INFO FOR JOB $job", &bindings)?;
		let result = info.first().ok_or_else(|| {
			FerrosGrynnError::invalid_argument("INFO FOR JOB returned no statement result")
		})?;
		let outcome = result.result.as_ref().map_err(Clone::clone)?;
		if !matches!(outcome.value(), Value::Object(_)) {
			return Err(FerrosGrynnError::invalid_argument(
				"INFO FOR JOB did not return an object",
			));
		}
		Ok(())
	}

	#[test]
	fn gql_analyze_supports_scoped_approximate_histograms() -> FerrosGrynnResult<()> {
		let store = Datastore::in_memory();
		let setup = store.execute(
			"DEFINE CLASS person TYPE NODE SCHEMAFULL; DEFINE FIELD age ON CLASS person TYPE int; \
			 CREATE person:one SET age = 42; ANALYZE CLASS person BASE APPROXIMATE SAMPLE 1024;",
			&Object::new(),
		)?;
		assert!(setup.iter().all(|result| result.result.is_ok()));
		let class = store
			.class("person")
			.ok_or_else(|| FerrosGrynnError::class_does_not_exist("person"))?;
		let class_id = class
			.id()
			.ok_or_else(|| FerrosGrynnError::invalid_storage_file("class identity absent"))?;
		let field = class
			.fields()
			.next()
			.ok_or_else(|| FerrosGrynnError::invalid_storage_file("field absent"))?;
		let field_id = field
			.id()
			.ok_or_else(|| FerrosGrynnError::invalid_storage_file("field identity absent"))?;
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
			.ok_or_else(|| FerrosGrynnError::invalid_storage_file("histogram absent"))?;
		assert_eq!(histogram.method, HistogramMethod::Approximate);
		assert_eq!(histogram.sample_count, 1);

		let automatic = store.execute("ANALYZE CLASS person BASE CONCURRENTLY;", &Object::new())?;
		let Value::Uuid(job_id) = automatic
			.first()
			.and_then(|result| result.result.as_ref().ok())
			.map(crate::gql_status::Outcome::value)
			.ok_or_else(|| FerrosGrynnError::invalid_argument("ANALYZE returned no job value"))?
		else {
			return Err(FerrosGrynnError::invalid_argument(
				"concurrent ANALYZE did not return a UUID",
			));
		};
		let automatic = store
			.job(JobId::from_uuid(*job_id))?
			.ok_or_else(|| FerrosGrynnError::maintenance_job_not_found(job_id.to_string()))?;
		assert_eq!(automatic.analyze_mode(), Some("full"));
		assert_eq!(automatic.analyze_sample_count(), None);

		let invalid =
			store.execute("ANALYZE CLASS person BASE APPROXIMATE SAMPLE 1023;", &Object::new())?;
		assert!(invalid.first().is_some_and(|result| result.result.is_err()));
		assert!(store.execute("ANALYZE DATABASE;", &Object::new()).is_err());
		assert!(store.execute("ANALYZE INDEX by_age ON CLASS person;", &Object::new()).is_err());
		Ok(())
	}

	#[test]
	fn recovery_cancels_orphans_and_requires_exact_index_build_revision() -> FerrosGrynnResult<()> {
		let store = Datastore::in_memory();
		let results = store.execute(
			"DEFINE CLASS person TYPE NODE SCHEMAFULL; DEFINE FIELD age ON CLASS person TYPE int; \
			 DEFINE INDEX by_age ON CLASS person FIELDS age SCOPE BASE;",
			&Object::new(),
		)?;
		if results.iter().any(|result| result.result.is_err()) {
			return Err(FerrosGrynnError::invalid_argument("recovery fixture schema setup failed"));
		}
		let class = store
			.class("person")
			.ok_or_else(|| FerrosGrynnError::class_does_not_exist("person"))?;
		let class_id = class.id().ok_or_else(|| {
			FerrosGrynnError::invalid_storage_file("fixture class has no identity")
		})?;
		let index = class
			.indexes()
			.next()
			.ok_or_else(|| FerrosGrynnError::invalid_storage_file("fixture class has no index"))?;
		let index_id = index.id().ok_or_else(|| {
			FerrosGrynnError::invalid_storage_file("fixture index has no identity")
		})?;
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
			.ok_or_else(|| FerrosGrynnError::invalid_argument("invalid fixture graph identity"))?;
		let orphan = MaintenanceJobInfo::new(
			MaintenanceJobKind::CompactGraph,
			MaintenanceTarget::Graph(missing_graph),
			store.inner.current_root().commit_ts,
			MaintenanceOptions::default(),
			None,
		);
		let orphan_id = orphan.id();

		let coordinator = MaintenanceCoordinator::new(vec![published, interrupted, orphan]);
		coordinator.reconcile(&store.inner)?;
		assert_eq!(
			coordinator.job(published_id).map(|job| job.state()),
			Some(MaintenanceJobState::Completed)
		);
		let interrupted = coordinator.job(interrupted_id).ok_or_else(|| {
			FerrosGrynnError::maintenance_job_not_found(interrupted_id.to_string())
		})?;
		assert_eq!(interrupted.state(), MaintenanceJobState::Queued);
		assert_eq!(interrupted.restart_count(), 1);
		assert_eq!(
			coordinator.job(orphan_id).map(|job| job.state()),
			Some(MaintenanceJobState::Cancelled)
		);
		Ok(())
	}

	#[test]
	fn local_reopen_reconciles_published_interrupted_and_orphan_jobs() -> FerrosGrynnResult<()> {
		let directory = TestDirectory::new();
		let options = LocalOptions::default();
		let store = Datastore::create_local(&directory.0, options)?;
		let setup = store.execute(
			"DEFINE CLASS document TYPE NODE SCHEMAFULL; DEFINE FIELD embedding ON CLASS document \
			 TYPE vector<f32>; CREATE document:a SET embedding = $a; CREATE document:b SET \
			 embedding = $b; DEFINE INDEX embedding_hnsw ON CLASS document FIELDS embedding HNSW \
			 DIMENSION 2 TYPE F32 DIST EUCLIDEAN EFC 16 M 4 SCOPE BASE; DEFINE INDEX \
			 embedding_disk ON CLASS document FIELDS embedding DISKANN DIMENSION 2 TYPE F32 DIST \
			 EUCLIDEAN DEGREE 4 L_BUILD 8 ALPHA 1.2 SCOPE BASE;",
			&Object::from_iter([
				("a", Value::Vector(Vector::F32(vec![0.0, 0.0]))),
				("b", Value::Vector(Vector::F32(vec![1.0, 0.0]))),
			]),
		)?;
		if setup.iter().any(|result| result.result.is_err()) {
			return Err(FerrosGrynnError::invalid_argument(
				"local maintenance recovery fixture setup failed",
			));
		}
		let class = store
			.class("document")
			.ok_or_else(|| FerrosGrynnError::class_does_not_exist("document"))?;
		let class_id = class.id().ok_or_else(|| {
			FerrosGrynnError::invalid_storage_file("fixture class has no identity")
		})?;
		let hnsw =
			class.indexes().find(|index| index.name().as_str() == "embedding_hnsw").ok_or_else(
				|| FerrosGrynnError::invalid_storage_file("fixture HNSW index is absent"),
			)?;
		let disk_ann =
			class.indexes().find(|index| index.name().as_str() == "embedding_disk").ok_or_else(
				|| FerrosGrynnError::invalid_storage_file("fixture DiskANN index is absent"),
			)?;
		let hnsw_target = MaintenanceTarget::Index {
			class: class_id,
			index: hnsw.id().ok_or_else(|| {
				FerrosGrynnError::invalid_storage_file("fixture HNSW index has no identity")
			})?,
			version: hnsw.version(),
		};
		let disk_ann_target = MaintenanceTarget::Index {
			class: class_id,
			index: disk_ann.id().ok_or_else(|| {
				FerrosGrynnError::invalid_storage_file("fixture DiskANN index has no identity")
			})?,
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
			.ok_or_else(|| FerrosGrynnError::invalid_argument("invalid fixture graph identity"))?;
		let orphan = MaintenanceJobInfo::new(
			MaintenanceJobKind::CompactGraph,
			MaintenanceTarget::Graph(missing_graph),
			source_revision,
			MaintenanceOptions::default(),
			None,
		);
		let orphan_id = orphan.id();
		store.inner.persist_maintenance_jobs(&[published, interrupted, orphan])?;
		drop(store);

		let reopened = Datastore::open_local(&directory.0, options)?;
		let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
		loop {
			let published = reopened.job(published_id)?.ok_or_else(|| {
				FerrosGrynnError::maintenance_job_not_found(published_id.to_string())
			})?;
			let interrupted = reopened.job(interrupted_id)?.ok_or_else(|| {
				FerrosGrynnError::maintenance_job_not_found(interrupted_id.to_string())
			})?;
			let orphan = reopened.job(orphan_id)?.ok_or_else(|| {
				FerrosGrynnError::maintenance_job_not_found(orphan_id.to_string())
			})?;
			if published.state() == MaintenanceJobState::Completed
				&& interrupted.state() == MaintenanceJobState::Completed
				&& orphan.state() == MaintenanceJobState::Cancelled
			{
				assert_eq!(interrupted.restart_count(), 1);
				break;
			}
			if std::time::Instant::now() >= deadline {
				return Err(FerrosGrynnError::invalid_argument(format!(
					"local maintenance reconciliation timed out: {published:?}, {interrupted:?}, \
					 {orphan:?}",
				)));
			}
			std::thread::sleep(std::time::Duration::from_millis(1));
		}
		Ok(())
	}
}
