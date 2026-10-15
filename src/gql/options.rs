use std::{
	collections::BTreeMap,
	num::NonZeroUsize,
	path::PathBuf,
	sync::{
		Arc,
		atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
	},
	time::{Duration, Instant},
};

/// Configured default barrier memory bytes value.
pub const DEFAULT_BARRIER_MEMORY_BYTES: usize = 256 * 1024 * 1024;
/// Shared next statement nonce value.
static NEXT_STATEMENT_NONCE: AtomicU64 = AtomicU64::new(1);

/// Returns the next statement nonce.
fn next_statement_nonce() -> u64 {
	// The nonce distinguishes statement identity; it does not publish execution
	// state.
	NEXT_STATEMENT_NONCE.fetch_add(1, Ordering::Relaxed)
}

use parking_lot::Mutex;

use super::{
	ExecutionMode,
	ParserSettings,
	QueryCursor,
	closure::{LanguageClosureEntry, LanguageClosureState},
	cursor::CursorRuntime,
	ir::{ClosureEffect, LogicalClosure, NamedFunctionKey},
};
use crate::{
	datastore::{FunctionId, Transaction},
	gql_status::{FerrosGrynnError, FerrosGrynnResult, Outcome, Status, StatusReport},
	syn::TimeoutValue,
	values::{Closure, Object, Value},
};

/// Cooperative cancellation handle shared with one or more query executions.
#[derive(Debug, Clone, Default)]
pub struct QueryCancellation(
	/// Shared cancellation flag observed by query resource checks.
	Arc<AtomicBool>,
);

/// Owned interruption boundary shared with a private bounded compiler scope.
#[derive(Debug, Clone)]
pub(in crate::gql) struct ExecutionChecks {
	/// Absolute deadline is never restarted for registration.
	deadline: Option<Instant>,
	/// Original timeout diagnostic, independent of elapsed registration work.
	timeout_ms: u64,
	/// Shared cooperative cancellation token.
	cancellation: Option<QueryCancellation>,
	/// Optional embedding check.
	cooperative: Option<fn() -> FerrosGrynnResult<()>>,
}

impl ExecutionChecks {
	/// Uses the same typed interruption semantics as query execution.
	pub(in crate::gql) fn check(&self) -> FerrosGrynnResult<()> {
		check_interrupt(
			self.cancellation.as_ref(),
			self.deadline,
			self.timeout_ms,
			self.cooperative,
		)
	}
}

/// One check authority for runtime and compiler without per-row clones.
fn check_interrupt(
	cancellation: Option<&QueryCancellation>,
	deadline: Option<Instant>,
	timeout_ms: u64,
	cooperative: Option<fn() -> FerrosGrynnResult<()>>,
) -> FerrosGrynnResult<()> {
	if cancellation.is_some_and(QueryCancellation::is_cancelled) {
		return Err(FerrosGrynnError::query_cancelled());
	}
	if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
		return Err(FerrosGrynnError::query_timed_out(timeout_ms));
	}
	if let Some(check) = cooperative {
		check()?;
	}
	Ok(())
}

impl QueryCancellation {
	/// Adapts the datastore worker's existing cooperative token without a
	/// second flag.
	pub(crate) fn from_token(token: Arc<AtomicBool>) -> Self {
		Self(token)
	}

	/// Requests cancellation. In-flight synchronous system I/O observes it
	/// after the system call returns.
	pub fn cancel(&self) {
		self.0.store(true, Ordering::Release);
	}

	/// Returns whether cancellation has been requested.
	pub fn is_cancelled(&self) -> bool {
		self.0.load(Ordering::Acquire)
	}

	/// Shares cancellation with the async bridge dispatcher.
	pub(crate) fn atomic(&self) -> Arc<AtomicBool> {
		self.0.clone()
	}
}

/// Parser and bounded-runtime settings for one `execute` call.
#[derive(Clone)]
#[non_exhaustive]
pub struct QueryExecutionOptions {
	/// Opaque runtime authority carried between a bridge and its queries.
	/// Applications normally retain the default context; adapters obtain the
	/// shared context through `BridgeContext::options`.
	pub context: QueryExecutionContext,
	/// Parser nesting and object-depth limits.
	pub parser_settings: ParserSettings,
	/// Default per-statement timeout, overridden only by an earlier clause
	/// deadline.
	pub statement_timeout: Option<Duration>,
	/// Maximum nesting depth of executable control-flow blocks.
	pub max_control_depth: u32,
	/// Combined number of loop backedges allowed in one top-level statement.
	pub max_loop_iterations: u64,
	/// Maximum topology branches, hyper assignments, and containment links
	/// considered by MATCH in one statement. Also bounds exact neighbourhood
	/// projection/traversal work across all kHopEstimate seeds in that
	/// statement.
	pub max_path_expansions: u64,
	/// Tensor/analytics scratch, aggregate work and complete-path limits.
	pub graph_algorithm_limits: crate::graph::tensor::GraphAlgorithmLimits,
	/// Maximum unique node atoms retained by one graph-result assembly.
	pub max_graph_nodes: u64,
	/// Maximum unique edge atoms retained by one graph-result assembly.
	pub max_graph_edges: u64,
	/// Optional cooperative cancellation token.
	pub cancellation: Option<QueryCancellation>,
	/// Shared memory budget for SELECT barriers. Values above the engine
	/// default are rejected; callers may only tighten the limit.
	pub barrier_memory_bytes: NonZeroUsize,
	/// Optional root for transient query-run directories.
	pub temporary_directory: Option<PathBuf>,
	/// User component of strict deterministic replay seeds.
	pub deterministic_seed: u64,
}

/// Runtime-owned bridge authority carried by execution options.
///
/// Cloning options preserves the shared admission account, dispatcher and
/// source-side effect ceiling. Adapters executing options obtained from
/// `BridgeContext::options` must retain this context when changing ordinary
/// settings. The default context represents a query without a bridge.
#[derive(Debug, Clone, Default)]
pub struct QueryExecutionContext {
	/// Admission account shared by source and receiving queries.
	pub(crate) bridge_memory: Option<Arc<Mutex<BarrierAccount>>>,
	/// Query-local dispatcher, never persisted in a durable recipe.
	pub(crate) bridge_scope: Option<Arc<crate::bridge::BridgeQueryScope>>,
	/// Source-side effect ceiling inherited by nested function calls.
	pub(crate) bridge_read_only: bool,
}

impl std::fmt::Debug for QueryExecutionOptions {
	/// Formats `QueryExecutionOptions` using the requested formatter.
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		// Keep the existing diagnostic representation after encapsulating
		// state.
		f.debug_struct("QueryExecutionOptions")
			.field("bridge_memory", &self.context.bridge_memory)
			.field("bridge_scope", &self.context.bridge_scope)
			.field("bridge_read_only", &self.context.bridge_read_only)
			.field("parser_settings", &self.parser_settings)
			.field("statement_timeout", &self.statement_timeout)
			.field("max_control_depth", &self.max_control_depth)
			.field("max_loop_iterations", &self.max_loop_iterations)
			.field("max_path_expansions", &self.max_path_expansions)
			.field("graph_algorithm_limits", &self.graph_algorithm_limits)
			.field("max_graph_nodes", &self.max_graph_nodes)
			.field("max_graph_edges", &self.max_graph_edges)
			.field("cancellation", &self.cancellation)
			.field("barrier_memory_bytes", &self.barrier_memory_bytes)
			.field("temporary_directory", &self.temporary_directory)
			.field("deterministic_seed", &self.deterministic_seed)
			.finish()
	}
}

impl Default for QueryExecutionOptions {
	/// Constructs the default initial state of `QueryExecutionOptions`.
	fn default() -> Self {
		Self {
			context: QueryExecutionContext::default(),
			parser_settings: ParserSettings::default(),
			statement_timeout: None,
			max_control_depth: 20,
			max_loop_iterations: 1_000_000,
			max_path_expansions: 1_000_000,
			graph_algorithm_limits: crate::graph::tensor::GraphAlgorithmLimits::default(),
			max_graph_nodes: 1_000_000,
			max_graph_edges: 5_000_000,
			cancellation: None,
			barrier_memory_bytes: NonZeroUsize::new(DEFAULT_BARRIER_MEMORY_BYTES)
				.unwrap_or(NonZeroUsize::MIN),
			temporary_directory: None,
			deterministic_seed: 0,
		}
	}
}

impl QueryExecutionOptions {
	/// Validates query execution options.
	pub(crate) fn validate(&self) -> FerrosGrynnResult<()> {
		let limits = self.graph_algorithm_limits;
		if limits.memory_bytes == 0
			|| limits.memory_bytes > DEFAULT_BARRIER_MEMORY_BYTES
			|| limits.max_work == 0
			|| limits.max_paths == 0
		{
			return Err(FerrosGrynnError::invalid_argument(
				"graph algorithm limits must be positive and scratch may not exceed 256 MiB",
			));
		}
		if self.barrier_memory_bytes.get() > DEFAULT_BARRIER_MEMORY_BYTES {
			return Err(FerrosGrynnError::invalid_argument(format!(
				"barrier_memory_bytes may not exceed {DEFAULT_BARRIER_MEMORY_BYTES}."
			)));
		}
		if self.max_path_expansions == 0 {
			return Err(FerrosGrynnError::invalid_argument(
				"max_path_expansions must be greater than zero.",
			));
		}
		if self.max_graph_nodes == 0 || self.max_graph_edges == 0 {
			return Err(FerrosGrynnError::invalid_argument(
				"max_graph_nodes and max_graph_edges must be greater than zero.",
			));
		}
		Ok(())
	}
}

/// Admitted session authority shared with nested statement runtimes.
#[derive(Debug)]
struct LiveAttachmentScope {
	/// Exact retained consumer identities, never serialized in a recipe.
	entries: Vec<crate::datastore::GqlLiveAttachment>,
	/// Retained authority stays charged until all child runtimes release it.
	_account: BarrierAccount,
}

/// Statement-local deadline and cancellation state.
#[derive(Debug, Clone)]
pub struct ExecutionControl {
	/// Query-scoped async port used by the synchronous executor.
	pub(crate) bridge_scope: Option<Arc<crate::bridge::BridgeQueryScope>>,
	/// Only an implicit session transaction may infer write access from typed
	/// IR.
	infer_write_access: bool,
	/// A callback cannot exceed the effect declared by its enclosing function.
	effect_ceiling: ClosureEffect,
	/// Session-only ACK authority; recipes and interruption contexts never
	/// retain it.
	live_attachments: Option<Arc<LiveAttachmentScope>>,
	/// Clause deadlines include analysis and snapshot setup time.
	started: Instant,
	/// Absolute deadline used for cooperative timeout checks.
	deadline: Option<Instant>,
	/// Timeout ms associated with this execution control.
	timeout_ms: u64,
	/// Shared token observed by cooperative cancellation checks.
	cancellation: Option<QueryCancellation>,
	/// Optional cooperative check supplied by an embedding maintenance worker.
	cooperative_check: Option<fn() -> FerrosGrynnResult<()>>,
	/// Notifications associated with this execution control.
	notifications: Arc<Mutex<Vec<Status>>>,
	/// Max control depth associated with this execution control.
	pub(crate) max_control_depth: u32,
	/// Max loop iterations associated with this execution control.
	pub(crate) max_loop_iterations: u64,
	/// Max path expansions associated with this execution control.
	max_path_expansions: u64,
	/// Shared caps for exact tensor and analytical operations.
	graph_algorithm_limits: crate::graph::tensor::GraphAlgorithmLimits,
	/// Max graph nodes associated with this execution control.
	max_graph_nodes: u64,
	/// Max graph edges associated with this execution control.
	max_graph_edges: u64,
	/// Path expansions associated with this execution control.
	path_expansions: Arc<AtomicU64>,
	/// Match metrics associated with this execution control.
	match_metrics: Arc<MatchMetrics>,
	/// Graph assembly metrics associated with this execution control.
	graph_assembly_metrics: Arc<GraphAssemblyMetrics>,
	/// Barrier memory associated with this execution control.
	barrier_memory: Arc<BarrierMemory>,
	/// Temporary directory associated with this execution control.
	pub(crate) temporary_directory: Option<PathBuf>,
	/// Cursors associated with this execution control.
	cursors: Option<Arc<Mutex<CursorRuntime>>>,
	/// Continuation associated with this execution control.
	continuation: Arc<Mutex<Option<QueryCursor>>>,
	/// Pending index seek associated with this execution control.
	pending_index_seek: Arc<Mutex<Option<QueryCursor>>>,
	/// Closure registry associated with this execution control.
	closure_registry: Arc<LanguageClosureState>,
	/// One shared execution gas/depth authority for nested calls and handlers.
	runtime_budget: Arc<RuntimeBudget>,
	/// Execution mode associated with this execution control.
	execution_mode: ExecutionMode,
	/// User seed associated with this execution control.
	user_seed: u64,
	/// Parameter fingerprint associated with this execution control.
	parameter_fingerprint: u64,
	/// Statement nonce associated with this execution control.
	statement_nonce: u64,
	/// Publication deferred associated with this execution control.
	publication_deferred: bool,
	/// Instrumentation associated with this execution control.
	instrumentation: bool,
	/// Optional counters scoped to one ANALYZE execution and its child
	/// controls.
	changefeed: Option<Arc<[AtomicU64; 9]>>,
}

/// Existing depth allocation also owns gas shared across invocation frames.
#[derive(Debug, Default)]
pub(in crate::gql) struct RuntimeBudget {
	/// Named-function depth is protected only while entering/leaving a frame.
	function_depths: Mutex<BTreeMap<FunctionId, u8>>,
	/// Independent quota accounting publishes no data; relaxed ordering
	/// suffices.
	loop_iterations: AtomicU64,
}

impl RuntimeBudget {
	/// Admits one loop step without holding a lock around user code.
	pub(in crate::gql) fn record_iteration(&self, maximum: u64) -> FerrosGrynnResult<()> {
		self.loop_iterations
			.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |value| {
				value.checked_add(1).filter(|next| *next <= maximum)
			})
			.map_err(|_value| {
				FerrosGrynnError::control_limit_exceeded("loop iterations", maximum)
			})?;
		Ok(())
	}
}

#[derive(Debug, Default)]
/// Match metrics state.
struct MatchMetrics {
	/// Actual graph-kernel work, shared by nested statement controls.
	algorithm_work: AtomicU64,
	/// Complete all-shortest paths shared by nested controls and seed batches.
	algorithm_paths: AtomicUsize,
	/// Captures using a snapshot-exact tensor generation.
	tensor_cache_hits: AtomicU64,
	/// Captures using exact incidence rather than a compatible generation.
	tensor_fallbacks: AtomicU64,
	/// Full logical source/target plane cells represented by captures.
	tensor_cells: AtomicU64,
	/// Actual distinct source/target memberships represented by captures.
	tensor_nonzeros: AtomicU64,
	/// Completed analytical iterations across tensor consumers.
	tensor_iterations: AtomicU64,
	/// Encoded generation footprints used by cache captures, not bytes read.
	tensor_artifact_bytes: AtomicU64,
	/// Successful reads of directed landmark bounds.
	landmark_lookups: AtomicU64,
	/// Searches using exact traversal without a compatible bound.
	landmark_fallbacks: AtomicU64,
	/// Generation bytes admitted once per bound source-target search.
	landmark_artifact_bytes: AtomicU64,
	/// Seeds associated with this match metrics.
	seeds: AtomicU64,
	/// Frontier peak associated with this match metrics.
	frontier_peak: AtomicU64,
	/// Trace nodes associated with this match metrics.
	trace_nodes: AtomicU64,
	/// Graph opens associated with this match metrics.
	graph_opens: AtomicU64,
	/// Containment links associated with this match metrics.
	containment_links: AtomicU64,
	/// Hyper assignments associated with this match metrics.
	hyper_assignments: AtomicU64,
	/// Cancellation checks associated with this match metrics.
	cancellation_checks: AtomicU64,
	/// Physical windows associated with this match metrics.
	physical_windows: AtomicU64,
	/// Physical resident pages associated with this match metrics.
	physical_resident_pages: AtomicU64,
	/// Physical wal pages associated with this match metrics.
	physical_wal_pages: AtomicU64,
	/// Physical sync pages associated with this match metrics.
	physical_sync_pages: AtomicU64,
	/// Physical ioring pages associated with this match metrics.
	physical_ioring_pages: AtomicU64,
}

#[derive(Debug, Default)]
/// Graph assembly metrics state.
struct GraphAssemblyMetrics {
	/// Source rows associated with this graph assembly metrics.
	source_rows: AtomicU64,
	/// Selected nodes associated with this graph assembly metrics.
	selected_nodes: AtomicU64,
	/// Selected edges associated with this graph assembly metrics.
	selected_edges: AtomicU64,
	/// Closure nodes associated with this graph assembly metrics.
	closure_nodes: AtomicU64,
	/// Closure edges associated with this graph assembly metrics.
	closure_edges: AtomicU64,
	/// Deduplicated values associated with this graph assembly metrics.
	deduplicated_values: AtomicU64,
	/// Final nodes associated with this graph assembly metrics.
	final_nodes: AtomicU64,
	/// Final edges associated with this graph assembly metrics.
	final_edges: AtomicU64,
	/// Child graphs associated with this graph assembly metrics.
	child_graphs: AtomicU64,
	/// Scope slots associated with this graph assembly metrics.
	scope_slots: AtomicU64,
	/// Validation ns associated with this graph assembly metrics.
	validation_ns: AtomicU64,
}

/// Snapshot of MATCH-specific counters collected by `EXPLAIN ANALYZE`.
#[derive(Debug, Clone, Copy, Default)]
pub struct MatchMetricsSnapshot {
	/// Shared graph-kernel work admission, including retained failed work.
	pub(crate) algorithm_work: u64,
	/// Complete logical paths charged against the statement cap.
	pub(crate) algorithm_paths: usize,
	/// Snapshot-exact tensor cache captures.
	pub(crate) tensor_cache_hits: u64,
	/// Exact on-demand captures.
	pub(crate) tensor_fallbacks: u64,
	/// Logical plane cells represented by captures.
	pub(crate) tensor_cells: u64,
	/// Distinct source/target memberships represented by captures.
	pub(crate) tensor_nonzeros: u64,
	/// Completed analytical iterations.
	pub(crate) tensor_iterations: u64,
	/// Complete encoded footprints, independent of actual I/O counters.
	pub(crate) tensor_artifact_bytes: u64,
	/// Directed lower-bound reads and exact-search fallbacks.
	pub(crate) landmark_lookups: u64,
	/// Searches using exact traversal without a compatible bound.
	pub(crate) landmark_fallbacks: u64,
	/// Generation bytes admitted once per bound source-target search.
	pub(crate) landmark_artifact_bytes: u64,
	/// Seeds associated with this match metrics snapshot.
	pub(crate) seeds: u64,
	/// Frontier peak associated with this match metrics snapshot.
	pub(crate) frontier_peak: u64,
	/// Trace nodes associated with this match metrics snapshot.
	pub(crate) trace_nodes: u64,
	/// Graph opens associated with this match metrics snapshot.
	pub(crate) graph_opens: u64,
	/// Containment links associated with this match metrics snapshot.
	pub(crate) containment_links: u64,
	/// Hyper assignments associated with this match metrics snapshot.
	pub(crate) hyper_assignments: u64,
	/// Cancellation checks associated with this match metrics snapshot.
	pub(crate) cancellation_checks: u64,
	/// Physical windows associated with this match metrics snapshot.
	pub(crate) physical_windows: u64,
	/// Physical resident pages associated with this match metrics snapshot.
	pub(crate) physical_resident_pages: u64,
	/// Physical wal pages associated with this match metrics snapshot.
	pub(crate) physical_wal_pages: u64,
	/// Physical sync pages associated with this match metrics snapshot.
	pub(crate) physical_sync_pages: u64,
	/// Physical ioring pages associated with this match metrics snapshot.
	pub(crate) physical_ioring_pages: u64,
}

impl MatchMetricsSnapshot {
	/// Returns a delta between two observations of the same statement control.
	pub(crate) const fn saturating_sub(self, earlier: Self) -> Self {
		Self {
			algorithm_work: self.algorithm_work.saturating_sub(earlier.algorithm_work),
			algorithm_paths: self.algorithm_paths.saturating_sub(earlier.algorithm_paths),
			tensor_cache_hits: self.tensor_cache_hits.saturating_sub(earlier.tensor_cache_hits),
			tensor_fallbacks: self.tensor_fallbacks.saturating_sub(earlier.tensor_fallbacks),
			tensor_cells: self.tensor_cells.saturating_sub(earlier.tensor_cells),
			tensor_nonzeros: self.tensor_nonzeros.saturating_sub(earlier.tensor_nonzeros),
			tensor_iterations: self.tensor_iterations.saturating_sub(earlier.tensor_iterations),
			tensor_artifact_bytes: self
				.tensor_artifact_bytes
				.saturating_sub(earlier.tensor_artifact_bytes),
			landmark_lookups: self.landmark_lookups.saturating_sub(earlier.landmark_lookups),
			landmark_fallbacks: self.landmark_fallbacks.saturating_sub(earlier.landmark_fallbacks),
			landmark_artifact_bytes: self
				.landmark_artifact_bytes
				.saturating_sub(earlier.landmark_artifact_bytes),
			seeds: self.seeds.saturating_sub(earlier.seeds),
			frontier_peak: self.frontier_peak,
			trace_nodes: self.trace_nodes.saturating_sub(earlier.trace_nodes),
			graph_opens: self.graph_opens.saturating_sub(earlier.graph_opens),
			containment_links: self.containment_links.saturating_sub(earlier.containment_links),
			hyper_assignments: self.hyper_assignments.saturating_sub(earlier.hyper_assignments),
			cancellation_checks: self
				.cancellation_checks
				.saturating_sub(earlier.cancellation_checks),
			physical_windows: self.physical_windows.saturating_sub(earlier.physical_windows),
			physical_resident_pages: self
				.physical_resident_pages
				.saturating_sub(earlier.physical_resident_pages),
			physical_wal_pages: self.physical_wal_pages.saturating_sub(earlier.physical_wal_pages),
			physical_sync_pages: self
				.physical_sync_pages
				.saturating_sub(earlier.physical_sync_pages),
			physical_ioring_pages: self
				.physical_ioring_pages
				.saturating_sub(earlier.physical_ioring_pages),
		}
	}
}

/// Snapshot of owned graph assembly counters collected by `EXPLAIN ANALYZE`.
#[derive(Debug, Clone, Copy, Default)]
pub struct GraphAssemblyMetricsSnapshot {
	/// Source rows associated with this graph assembly metrics snapshot.
	pub(crate) source_rows: u64,
	/// Selected nodes associated with this graph assembly metrics snapshot.
	pub(crate) selected_nodes: u64,
	/// Selected edges associated with this graph assembly metrics snapshot.
	pub(crate) selected_edges: u64,
	/// Closure nodes associated with this graph assembly metrics snapshot.
	pub(crate) closure_nodes: u64,
	/// Closure edges associated with this graph assembly metrics snapshot.
	pub(crate) closure_edges: u64,
	/// Deduplicated values associated with this graph assembly metrics
	/// snapshot.
	pub(crate) deduplicated_values: u64,
	/// Final nodes associated with this graph assembly metrics snapshot.
	pub(crate) final_nodes: u64,
	/// Final edges associated with this graph assembly metrics snapshot.
	pub(crate) final_edges: u64,
	/// Child graphs associated with this graph assembly metrics snapshot.
	pub(crate) child_graphs: u64,
	/// Scope slots associated with this graph assembly metrics snapshot.
	pub(crate) scope_slots: u64,
	/// Validation ns associated with this graph assembly metrics snapshot.
	pub(crate) validation_ns: u64,
}

impl GraphAssemblyMetricsSnapshot {
	/// Returns a delta between two observations of the same statement control.
	pub(crate) const fn saturating_sub(self, earlier: Self) -> Self {
		Self {
			source_rows: self.source_rows.saturating_sub(earlier.source_rows),
			selected_nodes: self.selected_nodes.saturating_sub(earlier.selected_nodes),
			selected_edges: self.selected_edges.saturating_sub(earlier.selected_edges),
			closure_nodes: self.closure_nodes.saturating_sub(earlier.closure_nodes),
			closure_edges: self.closure_edges.saturating_sub(earlier.closure_edges),
			deduplicated_values: self
				.deduplicated_values
				.saturating_sub(earlier.deduplicated_values),
			final_nodes: self.final_nodes.saturating_sub(earlier.final_nodes),
			final_edges: self.final_edges.saturating_sub(earlier.final_edges),
			child_graphs: self.child_graphs.saturating_sub(earlier.child_graphs),
			scope_slots: self.scope_slots.saturating_sub(earlier.scope_slots),
			validation_ns: self.validation_ns.saturating_sub(earlier.validation_ns),
		}
	}
}

#[derive(Debug)]
/// Barrier memory state.
struct BarrierMemory {
	/// Limit associated with this barrier memory.
	limit: usize,
	/// Used associated with this barrier memory.
	used: AtomicUsize,
	/// Peak associated with this barrier memory.
	peak: AtomicUsize,
	/// Spill bytes associated with this barrier memory.
	spill_bytes: AtomicU64,
	/// Spill runs associated with this barrier memory.
	spill_runs: AtomicU64,
	/// Row pulls associated with this barrier memory.
	row_pulls: AtomicU64,
	/// Checked work for ordinary reads of reactive lexical values.
	reactive_work: AtomicU64,
	/// MBR candidates returned by all spatial sources.
	spatial_candidates: AtomicU64,
	/// Rows sent through exact spatial post-filters.
	spatial_refinements: AtomicU64,
	/// Automatic spatial sources falling back to exact scans.
	spatial_fallbacks: AtomicU64,
	/// Tree node page reads, including resident hits.
	spatial_tree_pages: AtomicU64,
	/// Snapshot replacement records consulted.
	spatial_delta_records: AtomicU64,
	/// Sketch hits, exact fallbacks, charged work and consulted artifact bytes.
	neighbourhood: [AtomicU64; 4],
}

impl BarrierMemory {
	/// Admits bytes in the existing statement pool before an owner allocates.
	fn reserve(&self, bytes: usize) -> bool {
		if bytes == 0 {
			return true;
		}
		let mut used = self.used.load(Ordering::Acquire);
		loop {
			let Some(next) = used.checked_add(bytes).filter(|next| *next <= self.limit) else {
				return false;
			};
			match self.used.compare_exchange_weak(used, next, Ordering::AcqRel, Ordering::Acquire) {
				Ok(_) => {
					self.peak.fetch_max(next, Ordering::AcqRel);
					return true;
				}
				Err(current) => used = current,
			}
		}
	}
}

/// Bytes retained by one in-memory SELECT barrier.
#[derive(Debug)]
pub struct BarrierAccount {
	/// Memory associated with this barrier account.
	memory: Arc<BarrierMemory>,
	/// Bytes associated with this barrier account.
	bytes: usize,
}

impl BarrierAccount {
	/// Creates a new `BarrierAccount` value.
	pub(crate) fn new(limit: usize) -> Self {
		Self {
			memory: Arc::new(BarrierMemory {
				limit,
				used: AtomicUsize::new(0),
				peak: AtomicUsize::new(0),
				spill_bytes: AtomicU64::new(0),
				spill_runs: AtomicU64::new(0),
				row_pulls: AtomicU64::new(0),
				reactive_work: AtomicU64::new(0),
				spatial_candidates: AtomicU64::new(0),
				spatial_refinements: AtomicU64::new(0),
				spatial_fallbacks: AtomicU64::new(0),
				spatial_tree_pages: AtomicU64::new(0),
				spatial_delta_records: AtomicU64::new(0),
				neighbourhood: std::array::from_fn(|_| AtomicU64::new(0)),
			}),
			bytes: 0,
		}
	}

	/// Reserves barrier account.
	pub(crate) fn reserve(&mut self, bytes: usize) -> bool {
		if !self.memory.reserve(bytes) {
			return false;
		}
		self.bytes += bytes;
		true
	}

	/// Releases all.
	pub(crate) fn release_all(&mut self) {
		if self.bytes != 0 {
			self.memory.used.fetch_sub(self.bytes, Ordering::AcqRel);
			self.bytes = 0;
		}
	}

	/// Releases barrier account.
	pub(crate) fn release(&mut self, bytes: usize) {
		let bytes = bytes.min(self.bytes);
		if bytes != 0 {
			self.memory.used.fetch_sub(bytes, Ordering::AcqRel);
			self.bytes -= bytes;
		}
	}

	/// Records spill.
	pub(crate) fn record_spill(&self, bytes: u64) {
		self.memory.spill_bytes.fetch_add(bytes, Ordering::Relaxed);
		self.memory.spill_runs.fetch_add(1, Ordering::Relaxed);
	}
}

impl Drop for BarrierAccount {
	/// Runs cleanup for this `BarrierAccount` before its fields are released.
	fn drop(&mut self) {
		self.release_all();
	}
}

impl ExecutionControl {
	/// Creates a new `ExecutionControl` value.
	pub(crate) fn new(options: &QueryExecutionOptions, timeout: Option<Duration>) -> Self {
		Self::with_cursors(options, timeout, None)
	}

	/// Configures cursors.
	pub(crate) fn with_cursors(
		options: &QueryExecutionOptions,
		timeout: Option<Duration>,
		cursors: Option<Arc<Mutex<CursorRuntime>>>,
	) -> Self {
		Self::with_runtime(options, timeout, cursors, Arc::new(LanguageClosureState::default()))
	}

	/// Configures runtime.
	pub(crate) fn with_runtime(
		options: &QueryExecutionOptions,
		timeout: Option<Duration>,
		cursors: Option<Arc<Mutex<CursorRuntime>>>,
		closure_registry: Arc<LanguageClosureState>,
	) -> Self {
		Self::with_runtime_origin(options, timeout, cursors, closure_registry, None)
	}

	/// Reuses an unused session origin only when no statement timeout can be
	/// resolved.
	pub(crate) fn with_session_runtime(
		options: &QueryExecutionOptions,
		statement_timed: bool,
		started: Instant,
		cursors: Option<Arc<Mutex<CursorRuntime>>>,
		closure_registry: Arc<LanguageClosureState>,
	) -> Self {
		let origin = (options.statement_timeout.is_none() && !statement_timed).then_some(started);
		Self::with_runtime_origin(options, None, cursors, closure_registry, origin)
	}

	/// Shared initializer; generic callers retain their original origin
	/// capture.
	fn with_runtime_origin(
		options: &QueryExecutionOptions,
		timeout: Option<Duration>,
		cursors: Option<Arc<Mutex<CursorRuntime>>>,
		closure_registry: Arc<LanguageClosureState>,
		origin: Option<Instant>,
	) -> Self {
		let timeout = match (options.statement_timeout, timeout) {
			(Some(left), Some(right)) => Some(left.min(right)),
			(Some(value), None) | (None, Some(value)) => Some(value),
			(None, None) => None,
		};
		let started = origin.unwrap_or_else(Instant::now);
		Self {
			started,
			bridge_scope: options.context.bridge_scope.clone(),
			deadline: timeout.and_then(|value| started.checked_add(value)),
			infer_write_access: false,
			effect_ceiling: ClosureEffect::SchemaMutation,
			live_attachments: None,
			timeout_ms: timeout
				.map(|value| u64::try_from(value.as_millis()).unwrap_or(u64::MAX))
				.unwrap_or(0),
			cancellation: options.cancellation.clone(),
			cooperative_check: None,
			notifications: Arc::new(Mutex::new(Vec::new())),
			max_control_depth: options.max_control_depth,
			max_loop_iterations: options.max_loop_iterations,
			max_path_expansions: options.max_path_expansions,
			graph_algorithm_limits: options.graph_algorithm_limits,
			max_graph_nodes: options.max_graph_nodes,
			max_graph_edges: options.max_graph_edges,
			path_expansions: Arc::new(AtomicU64::new(0)),
			match_metrics: Arc::new(MatchMetrics::default()),
			graph_assembly_metrics: Arc::new(GraphAssemblyMetrics::default()),
			barrier_memory: options.context.bridge_memory.as_ref().map_or_else(
				|| {
					Arc::new(BarrierMemory {
						limit: options.barrier_memory_bytes.get(),
						used: AtomicUsize::new(0),
						peak: AtomicUsize::new(0),
						spill_bytes: AtomicU64::new(0),
						spill_runs: AtomicU64::new(0),
						row_pulls: AtomicU64::new(0),
						reactive_work: AtomicU64::new(0),
						spatial_candidates: AtomicU64::new(0),
						spatial_refinements: AtomicU64::new(0),
						spatial_fallbacks: AtomicU64::new(0),
						spatial_tree_pages: AtomicU64::new(0),
						spatial_delta_records: AtomicU64::new(0),
						neighbourhood: std::array::from_fn(|_| AtomicU64::new(0)),
					})
				},
				|account| account.lock().memory.clone(),
			),
			temporary_directory: options.temporary_directory.clone(),
			cursors,
			continuation: Arc::new(Mutex::new(None)),
			pending_index_seek: Arc::new(Mutex::new(None)),
			closure_registry,
			runtime_budget: Arc::new(RuntimeBudget::default()),
			execution_mode: ExecutionMode::BestEffort,
			user_seed: options.deterministic_seed,
			parameter_fingerprint: 0,
			statement_nonce: next_statement_nonce(),
			publication_deferred: false,
			instrumentation: false,
			changefeed: None,
		}
	}

	/// Configures execution mode.
	pub(crate) fn with_execution_mode(
		mut self,
		execution_mode: ExecutionMode,
		parameters: &Object,
	) -> Self {
		self.execution_mode = execution_mode;
		if self.deterministic() {
			self.parameter_fingerprint = crate::gql::optimizer::canonical_hash(
				crate::gql::optimizer::PLAN_ENCODING_VERSION,
				b"query-parameters",
				parameters,
			);
		}
		self
	}

	/// Allows typed effects to select access for an implicit session
	/// transaction.
	pub(crate) const fn with_inferred_write_access(mut self) -> Self {
		self.infer_write_access = true;
		self
	}

	/// Explicit transaction access is never inferred or promoted.
	pub(in crate::gql) const fn infer_write_access(&self) -> bool {
		self.infer_write_access
	}

	/// Preserves declared effects through dynamically resolved nested
	/// callbacks.
	pub(in crate::gql) fn closure_scope(&self, effect: ClosureEffect) -> FerrosGrynnResult<Self> {
		if effect > self.effect_ceiling {
			return Err(FerrosGrynnError::closure_effect_not_allowed(
				format!("{effect:?}"),
				format!("{:?} caller", self.effect_ceiling),
			));
		}
		let mut child = self.clone();
		child.effect_ceiling = effect;
		Ok(child)
	}

	/// Reports whether the current execution has an explicit effect ceiling.
	pub(in crate::gql) fn effect_is_limited(&self) -> bool {
		self.effect_ceiling < ClosureEffect::SchemaMutation
	}

	/// Attaches pre-admitted immutable authority for the current session call.
	pub(crate) fn with_live_attachments(
		mut self,
		entries: Vec<crate::datastore::GqlLiveAttachment>,
		account: BarrierAccount,
	) -> Self {
		self.live_attachments = Some(Arc::new(LiveAttachmentScope {
			entries,
			_account: account,
		}));
		self
	}

	/// Requires retained attachment ownership in addition to a valid cursor.
	pub(in crate::gql) fn live_attachment(
		&self,
		id: crate::live::LiveId,
	) -> FerrosGrynnResult<crate::datastore::GqlLiveAttachment> {
		self.live_attachments
			.as_ref()
			.and_then(|scope| scope.entries.iter().find(|entry| entry.id() == id))
			.copied()
			.ok_or_else(|| {
				FerrosGrynnError::invalid_argument(
					"GQL ACK requires a retained session LIVE consumer",
				)
			})
	}

	/// Creates closure.
	pub(in crate::gql) fn create_closure(
		&self,
		plan: &LogicalClosure,
		slots: &[Value],
	) -> FerrosGrynnResult<Closure> {
		self.closure_registry.register(plan, slots, self.statement_nonce)
	}

	/// Returns the function cache result.
	pub(in crate::gql) fn function_cache(&self) -> &LanguageClosureState {
		&self.closure_registry
	}

	/// Returns the statement nonce result.
	pub(crate) const fn statement_nonce(&self) -> u64 {
		self.statement_nonce
	}

	/// Resolves closure.
	pub(in crate::gql) fn resolve_closure(
		&self,
		closure: &Closure,
	) -> FerrosGrynnResult<LanguageClosureEntry> {
		self.closure_registry.resolve(closure)
	}

	/// Resolves named function.
	pub(in crate::gql) fn resolve_named_function(
		&self,
		function: NamedFunctionKey,
	) -> FerrosGrynnResult<Arc<LogicalClosure>> {
		self.closure_registry.resolve_named(function)
	}

	/// Returns the enter named function result.
	pub(in crate::gql) fn enter_named_function(
		&self,
		function: NamedFunctionKey,
	) -> FerrosGrynnResult<NamedFunctionFrame> {
		// Catalog validation rejects recursive cycles without MAXDEPTH, so a
		// non-recursive call does not need the shared depth map.
		let Some(maximum) = function.max_depth.map(u32::from) else {
			return Ok(NamedFunctionFrame {
				depths: None,
				function: function.id,
			});
		};
		let maximum = maximum.min(self.max_control_depth);
		let mut depths = self.runtime_budget.function_depths.lock();
		let depth = depths.entry(function.id).or_default();
		if u32::from(*depth) >= maximum {
			return Err(FerrosGrynnError::control_limit_exceeded(
				"function recursion".to_owned(),
				u64::from(maximum),
			));
		}
		*depth = depth.saturating_add(1);
		Ok(NamedFunctionFrame {
			depths: Some(self.runtime_budget.clone()),
			function: function.id,
		})
	}

	/// Returns whether the deterministic condition holds.
	pub(crate) const fn deterministic(&self) -> bool {
		matches!(self.execution_mode, ExecutionMode::Deterministic)
	}

	/// Pins the same gas authority for another interpreter invocation frame.
	pub(in crate::gql) fn runtime_budget(&self) -> Arc<RuntimeBudget> {
		Arc::clone(&self.runtime_budget)
	}

	/// Starts an independent top-level statement; nested calls never reset gas.
	pub(in crate::gql) fn reset_loop_statement(&self) {
		self.runtime_budget.loop_iterations.store(0, Ordering::Relaxed);
	}

	/// Swaps compilation cache while retaining deadlines and shared resources.
	pub(in crate::gql) fn with_closure_registry(
		mut self,
		registry: Arc<LanguageClosureState>,
	) -> Self {
		self.closure_registry = registry;
		self
	}

	/// Returns whether the fast condition holds.
	pub(crate) const fn fast(&self) -> bool {
		matches!(self.execution_mode, ExecutionMode::Fast)
	}

	/// Configures publication deferred.
	pub(crate) const fn with_publication_deferred(mut self, deferred: bool) -> Self {
		self.publication_deferred = deferred;
		self
	}

	/// Opens one independent actual-observation scope for EXPLAIN ANALYZE.
	pub(crate) fn with_instrumentation(mut self) -> Self {
		self.instrumentation = true;
		self.changefeed = Some(Arc::new(std::array::from_fn(|_| AtomicU64::new(0))));
		self
	}

	/// Returns whether the publication deferred condition holds.
	pub(crate) const fn publication_deferred(&self) -> bool {
		self.publication_deferred
	}

	/// Returns the statement seed result.
	pub(crate) fn statement_seed(&self, query_fingerprint: u64, snapshot: u64) -> u64 {
		let replay = (
			query_fingerprint,
			self.parameter_fingerprint,
			snapshot,
			self.user_seed,
			if self.deterministic() {
				0
			} else {
				self.statement_nonce
			},
		);
		crate::gql::optimizer::canonical_hash(
			crate::gql::optimizer::PLAN_ENCODING_VERSION,
			b"statement-seed",
			&replay,
		)
	}

	/// Keeps publication waits within the statement's original deadline.
	pub(crate) const fn deadline(&self) -> Option<Instant> {
		self.deadline
	}

	/// Preserves caller interruption while keeping independent LIVE budgets.
	pub(crate) fn inherit_checks(mut self, parent: &Self) -> Self {
		self.started = parent.started;
		self.deadline = parent.deadline;
		self.timeout_ms = parent.timeout_ms;
		self.cancellation = parent.cancellation.clone();
		self.cooperative_check = parent.cooperative_check;
		self
	}

	/// Copies interruption only; no cache, cursor or snapshot is retained.
	pub(in crate::gql) fn interruption(&self) -> ExecutionChecks {
		ExecutionChecks {
			deadline: self.deadline,
			timeout_ms: self.timeout_ms,
			cancellation: self.cancellation.clone(),
			cooperative: self.cooperative_check,
		}
	}

	/// Checks execution control.
	pub(crate) fn check(&self) -> FerrosGrynnResult<()> {
		if let Some(scope) = &self.bridge_scope {
			scope.context.check()?;
		}
		if self.instrumentation {
			self.match_metrics.cancellation_checks.fetch_add(1, Ordering::Relaxed);
		}
		check_interrupt(
			self.cancellation.as_ref(),
			self.deadline,
			self.timeout_ms,
			self.cooperative_check,
		)
	}

	/// Attaches the caller's cancellation boundary to every child statement.
	pub(in crate::gql) fn with_cooperative_check(
		mut self,
		check: fn() -> FerrosGrynnResult<()>,
	) -> Self {
		self.cooperative_check = Some(check);
		self
	}

	/// Returns the child result.
	pub(crate) fn child(&self, timeout: Option<Duration>) -> Self {
		let child_deadline = timeout.and_then(|value| Instant::now().checked_add(value));
		self.child_at(timeout, child_deadline)
	}

	/// Applies a top-level clause without restarting its clock or resources.
	pub(in crate::gql) fn restrict_timeout(&mut self, timeout: Duration) -> FerrosGrynnResult<()> {
		let deadline = self.started.checked_add(timeout).ok_or_else(|| {
			FerrosGrynnError::invalid_argument("TIMEOUT duration is too large for this platform.")
		})?;
		if self.deadline.is_none_or(|parent| deadline < parent) {
			self.deadline = Some(deadline);
			self.timeout_ms = u64::try_from(timeout.as_millis()).unwrap_or(u64::MAX);
		}
		Ok(())
	}

	/// Rejects duration overflow rather than silently dropping a clause
	/// deadline.
	pub(in crate::gql) fn checked_child(
		&self,
		timeout: Option<Duration>,
	) -> FerrosGrynnResult<Self> {
		let child_deadline = timeout
			.map(|value| {
				Instant::now().checked_add(value).ok_or_else(|| {
					FerrosGrynnError::invalid_argument(
						"TIMEOUT duration is too large for this platform.",
					)
				})
			})
			.transpose()?;
		Ok(self.child_at(timeout, child_deadline))
	}

	/// Builds a child once its relative deadline has been resolved.
	fn child_at(&self, timeout: Option<Duration>, child_deadline: Option<Instant>) -> Self {
		let (deadline, timeout_ms) = match (self.deadline, child_deadline) {
			(Some(parent), Some(child)) if child < parent => (
				Some(child),
				timeout
					.map(|value| u64::try_from(value.as_millis()).unwrap_or(u64::MAX))
					.unwrap_or(0),
			),
			(Some(parent), _) => (Some(parent), self.timeout_ms),
			(None, Some(child)) => (
				Some(child),
				timeout
					.map(|value| u64::try_from(value.as_millis()).unwrap_or(u64::MAX))
					.unwrap_or(0),
			),
			(None, None) => (None, 0),
		};
		Self {
			started: self.started,
			bridge_scope: self.bridge_scope.clone(),
			deadline,
			infer_write_access: self.infer_write_access,
			effect_ceiling: self.effect_ceiling,
			live_attachments: self.live_attachments.clone(),
			timeout_ms,
			cancellation: self.cancellation.clone(),
			cooperative_check: self.cooperative_check,
			notifications: self.notifications.clone(),
			max_control_depth: self.max_control_depth,
			max_loop_iterations: self.max_loop_iterations,
			max_path_expansions: self.max_path_expansions,
			graph_algorithm_limits: self.graph_algorithm_limits,
			max_graph_nodes: self.max_graph_nodes,
			max_graph_edges: self.max_graph_edges,
			path_expansions: self.path_expansions.clone(),
			match_metrics: self.match_metrics.clone(),
			graph_assembly_metrics: self.graph_assembly_metrics.clone(),
			barrier_memory: self.barrier_memory.clone(),
			temporary_directory: self.temporary_directory.clone(),
			cursors: self.cursors.clone(),
			continuation: self.continuation.clone(),
			pending_index_seek: self.pending_index_seek.clone(),
			closure_registry: self.closure_registry.clone(),
			runtime_budget: self.runtime_budget.clone(),
			execution_mode: self.execution_mode,
			user_seed: self.user_seed,
			parameter_fingerprint: self.parameter_fingerprint,
			statement_nonce: self.statement_nonce,
			publication_deferred: self.publication_deferred,
			instrumentation: self.instrumentation,
			changefeed: self.changefeed.clone(),
		}
	}

	/// Returns the cursors result.
	pub(crate) fn cursors(&self) -> Option<&Arc<Mutex<CursorRuntime>>> {
		self.cursors.as_ref()
	}

	/// Returns the barrier account result.
	pub(crate) fn barrier_account(&self) -> BarrierAccount {
		BarrierAccount {
			memory: self.barrier_memory.clone(),
			bytes: 0,
		}
	}

	/// Admits the full reactive payload before cloning it for ordinary
	/// evaluation.
	pub(crate) fn copy_reactive_value(&self, value: &Value) -> FerrosGrynnResult<Value> {
		self.admit_reactive_value(value)?;
		Ok(value.clone())
	}

	/// Shared admission also covers clones performed by scalar operators.
	pub(crate) fn admit_reactive_value(&self, value: &Value) -> FerrosGrynnResult<()> {
		// Reactive copies remain conservatively charged until the statement
		// pool is released, including all child controls and callbacks.
		let admit = |bytes| self.admit_reactive_bytes(bytes);
		let work = |amount| self.admit_reactive_work(amount);
		crate::gql::value_admission::admit_value(value, self.barrier_memory.limit, &admit, &work)
	}

	/// Shares checked work with operators consuming admitted reactive values.
	pub(crate) fn admit_reactive_work(&self, amount: u64) -> FerrosGrynnResult<()> {
		self.check()?;
		self.barrier_memory
			.reactive_work
			.fetch_update(Ordering::AcqRel, Ordering::Acquire, |current| {
				current.checked_add(amount).filter(|next| *next <= self.max_loop_iterations)
			})
			.map_err(|_current| {
				FerrosGrynnError::traversal_resource_limit_exceeded(
					"LIVE LET value work",
					self.max_loop_iterations,
				)
			})?;
		Ok(())
	}

	/// Retains collection shells in the same statement-wide scratch pool.
	pub(crate) fn admit_reactive_bytes(&self, bytes: usize) -> FerrosGrynnResult<()> {
		self.check()?;
		if self.barrier_memory.reserve(bytes) {
			Ok(())
		} else {
			Err(FerrosGrynnError::traversal_resource_limit_exceeded(
				"LIVE LET value scratch",
				self.barrier_memory.limit as u64,
			))
		}
	}

	/// Materialized operands can outlive their first admitted reactive copy.
	pub(crate) fn has_reactive_copies(&self) -> bool {
		self.barrier_memory.reactive_work.load(Ordering::Acquire) != 0
	}

	/// Returns the statement-wide barrier limit used by physical planning.
	pub(crate) fn barrier_memory_limit(&self) -> usize {
		self.barrier_memory.limit
	}

	/// Returns currently unreserved statement scratch memory.
	pub(crate) fn barrier_memory_available(&self) -> usize {
		self.barrier_memory.limit.saturating_sub(self.barrier_memory.used.load(Ordering::Acquire))
	}

	/// Returns the remaining shared graph-kernel caps before an operation.
	pub(crate) fn graph_algorithm_limits(&self) -> crate::graph::tensor::GraphAlgorithmLimits {
		crate::graph::tensor::GraphAlgorithmLimits {
			memory_bytes: self
				.graph_algorithm_limits
				.memory_bytes
				.min(self.barrier_memory_available()),
			max_work: self
				.graph_algorithm_limits
				.max_work
				.saturating_sub(self.match_metrics.algorithm_work.load(Ordering::Acquire)),
			max_paths: self
				.graph_algorithm_limits
				.max_paths
				.saturating_sub(self.match_metrics.algorithm_paths.load(Ordering::Acquire)),
		}
	}

	/// Reserves exact operation work even when telemetry is disabled.
	pub(crate) fn record_graph_work(&self, work: u64) -> FerrosGrynnResult<()> {
		self.match_metrics
			.algorithm_work
			.fetch_update(Ordering::AcqRel, Ordering::Acquire, |current| {
				current
					.checked_add(work)
					.filter(|next| *next <= self.graph_algorithm_limits.max_work)
			})
			.map_err(|_current| {
				FerrosGrynnError::traversal_resource_limit_exceeded(
					"graph algorithm work",
					self.graph_algorithm_limits.max_work,
				)
			})?;
		Ok(())
	}

	/// Returns unused admission after a complete, successful graph operation.
	pub(crate) fn release_graph_work(&self, work: u64) {
		self.match_metrics.algorithm_work.fetch_sub(work, Ordering::AcqRel);
	}

	/// Charges complete paths before retaining them; never silently truncates.
	pub(crate) fn record_graph_paths(&self, paths: usize) -> FerrosGrynnResult<()> {
		self.match_metrics
			.algorithm_paths
			.fetch_update(Ordering::AcqRel, Ordering::Acquire, |current| {
				current
					.checked_add(paths)
					.filter(|next| *next <= self.graph_algorithm_limits.max_paths)
			})
			.map_err(|_current| {
				FerrosGrynnError::traversal_resource_limit_exceeded(
					"all-shortest enumeration",
					self.graph_algorithm_limits.max_paths as u64,
				)
			})?;
		Ok(())
	}

	/// Returns the remaining statement-wide exact neighbourhood work budget.
	pub(crate) fn remaining_neighbourhood_work(&self) -> u64 {
		self.max_path_expansions
			.saturating_sub(self.barrier_memory.neighbourhood[2].load(Ordering::Acquire))
	}

	/// Charges exact work even when telemetry is disabled.
	///
	/// # Errors
	/// Rejects work exceeding the statement-wide neighbourhood budget.
	pub(crate) fn record_neighbourhood_estimate(
		&self,
		approximate: bool,
		work: u64,
		bytes: u64,
	) -> FerrosGrynnResult<()> {
		self.barrier_memory.neighbourhood[2]
			.fetch_update(Ordering::AcqRel, Ordering::Acquire, |current| {
				current.checked_add(work).filter(|next| *next <= self.max_path_expansions)
			})
			.map_err(|_current| {
				FerrosGrynnError::traversal_resource_limit_exceeded(
					"neighbourhood exact work",
					self.max_path_expansions,
				)
			})?;
		if self.instrumentation {
			self.barrier_memory.neighbourhood[usize::from(!approximate)]
				.fetch_add(1, Ordering::Relaxed);
			self.barrier_memory.neighbourhood[3].fetch_add(bytes, Ordering::Relaxed);
		}
		Ok(())
	}

	/// Returns sketch hits, exact fallbacks, work and artifact bytes.
	pub(crate) fn neighbourhood_metrics(&self) -> [u64; 4] {
		std::array::from_fn(|index| {
			self.barrier_memory.neighbourhood[index].load(Ordering::Acquire)
		})
	}

	/// Records one completed history reader using bounded-cardinality scalar
	/// counters.
	pub(crate) fn record_changefeed(
		&self,
		statistics: crate::changefeed::ChangefeedReadStatistics,
	) {
		let Some(counters) = &self.changefeed else {
			return;
		};
		let values = [
			statistics.batches,
			statistics.entries,
			statistics.gaps,
			statistics.groups_inspected,
			statistics.page_reads,
			statistics.bytes_read,
			statistics.segment_visits,
			statistics.work,
		];
		for (counter, value) in counters.iter().zip(values) {
			let _previous = counter.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
				Some(current.saturating_add(value))
			});
		}
		counters[8].fetch_max(statistics.peak_scratch_bytes, Ordering::Relaxed);
	}

	/// Returns cumulative logical reader counters and the admitted scratch
	/// peak.
	pub(crate) fn changefeed_metrics(&self) -> [u64; 9] {
		self.changefeed.as_ref().map_or([0; 9], |counters| {
			std::array::from_fn(|index| counters[index].load(Ordering::Acquire))
		})
	}

	/// Returns the statement-wide unique-node ceiling for owned graph results.
	pub(crate) const fn max_graph_nodes(&self) -> u64 {
		self.max_graph_nodes
	}

	/// Returns the statement-wide unique-edge ceiling for owned graph results.
	pub(crate) const fn max_graph_edges(&self) -> u64 {
		self.max_graph_edges
	}

	/// Returns the barrier memory peak result.
	pub(crate) fn barrier_memory_peak(&self) -> usize {
		self.barrier_memory.peak.load(Ordering::Acquire)
	}

	/// Returns the spill metrics result.
	pub(crate) fn spill_metrics(&self) -> (u64, u64) {
		(
			self.barrier_memory.spill_bytes.load(Ordering::Acquire),
			self.barrier_memory.spill_runs.load(Ordering::Acquire),
		)
	}

	/// Returns the row pulls result.
	pub(crate) fn row_pulls(&self) -> u64 {
		self.barrier_memory.row_pulls.load(Ordering::Acquire)
	}

	/// Records pull.
	pub(crate) fn record_pull(&self) {
		if self.instrumentation {
			self.barrier_memory.row_pulls.fetch_add(1, Ordering::Relaxed);
		}
	}

	/// Charges one considered graph branch against the statement-wide MATCH
	/// budget shared by nested execution controls.
	pub(crate) fn record_path_expansion(&self) -> FerrosGrynnResult<()> {
		let mut current = self.path_expansions.load(Ordering::Acquire);
		loop {
			if current >= self.max_path_expansions {
				return Err(FerrosGrynnError::path_expansion_limit_exceeded(
					self.max_path_expansions,
				));
			}
			match self.path_expansions.compare_exchange_weak(
				current,
				current + 1,
				Ordering::AcqRel,
				Ordering::Acquire,
			) {
				Ok(_) => return Ok(()),
				Err(actual) => current = actual,
			}
		}
	}

	/// Returns the number of topology branches charged by this statement.
	pub(crate) fn path_expansions(&self) -> u64 {
		self.path_expansions.load(Ordering::Acquire)
	}

	/// Records admitted MBR candidates when EXPLAIN ANALYZE is active.
	pub(crate) fn record_spatial_candidates(&self, count: usize) {
		if self.instrumentation {
			self.barrier_memory.spatial_candidates.fetch_add(count as u64, Ordering::Relaxed);
		}
	}

	/// Records one exact spatial refinement.
	pub(crate) fn record_spatial_refinement(&self) {
		if self.instrumentation {
			self.barrier_memory.spatial_refinements.fetch_add(1, Ordering::Relaxed);
		}
	}

	/// Records one unavailable automatic geometry binding.
	pub(crate) fn record_spatial_fallback(&self) {
		if self.instrumentation {
			self.barrier_memory.spatial_fallbacks.fetch_add(1, Ordering::Relaxed);
		}
	}

	/// Records tree-page and replacement work for one spatial lookup.
	pub(crate) fn record_spatial_work(&self, pages: u64, delta_records: u64) {
		if self.instrumentation {
			self.barrier_memory.spatial_tree_pages.fetch_add(pages, Ordering::Relaxed);
			self.barrier_memory.spatial_delta_records.fetch_add(delta_records, Ordering::Relaxed);
		}
	}

	/// Returns candidates, refinements, fallbacks, tree pages and delta
	/// records, in that order.
	pub(crate) fn spatial_metrics(&self) -> [u64; 5] {
		[
			self.barrier_memory.spatial_candidates.load(Ordering::Acquire),
			self.barrier_memory.spatial_refinements.load(Ordering::Acquire),
			self.barrier_memory.spatial_fallbacks.load(Ordering::Acquire),
			self.barrier_memory.spatial_tree_pages.load(Ordering::Acquire),
			self.barrier_memory.spatial_delta_records.load(Ordering::Acquire),
		]
	}

	/// Records one accepted MATCH seed when instrumentation is active.
	pub(crate) fn record_match_seed(&self) {
		if self.instrumentation {
			self.match_metrics.seeds.fetch_add(1, Ordering::Relaxed);
		}
	}

	/// Records the largest active traversal frontier observed by this
	/// statement.
	pub(crate) fn record_match_frontier(&self, frontier: usize) {
		if self.instrumentation {
			self.match_metrics
				.frontier_peak
				.fetch_max(u64::try_from(frontier).unwrap_or(u64::MAX), Ordering::Relaxed);
		}
	}

	/// Records one retained named-path trace node.
	pub(crate) fn record_match_trace_node(&self) {
		if self.instrumentation {
			self.match_metrics.trace_nodes.fetch_add(1, Ordering::Relaxed);
		}
	}

	/// Records one immutable graph view opened by MATCH execution.
	pub(crate) fn record_match_graph_open(&self) {
		if self.instrumentation {
			self.match_metrics.graph_opens.fetch_add(1, Ordering::Relaxed);
		}
	}

	/// Records one containment link considered by WITHIN.
	pub(crate) fn record_match_containment_link(&self) {
		if self.instrumentation {
			self.match_metrics.containment_links.fetch_add(1, Ordering::Relaxed);
		}
	}

	/// Records one hyperedge endpoint assignment considered by MATCH.
	pub(crate) fn record_match_hyper_assignment(&self) {
		if self.instrumentation {
			self.match_metrics.hyper_assignments.fetch_add(1, Ordering::Relaxed);
		}
	}

	/// Records one fully decoded local physical-frontier window.
	pub(crate) fn record_physical_traversal(
		&self,
		resident_pages: u64,
		wal_pages: u64,
		sync_pages: u64,
		ioring_pages: u64,
	) {
		if !self.instrumentation {
			return;
		}
		self.match_metrics.physical_windows.fetch_add(1, Ordering::Relaxed);
		self.match_metrics.physical_resident_pages.fetch_add(resident_pages, Ordering::Relaxed);
		self.match_metrics.physical_wal_pages.fetch_add(wal_pages, Ordering::Relaxed);
		self.match_metrics.physical_sync_pages.fetch_add(sync_pages, Ordering::Relaxed);
		self.match_metrics.physical_ioring_pages.fetch_add(ioring_pages, Ordering::Relaxed);
	}

	/// Records a successful logical lower-bound lookup.
	pub(crate) fn record_landmark_lookup(&self, bytes: u64) {
		if self.instrumentation {
			self.match_metrics.landmark_lookups.fetch_add(1, Ordering::Relaxed);
			self.match_metrics.landmark_artifact_bytes.fetch_add(bytes, Ordering::Relaxed);
		}
	}

	/// Records one source-target search using the exact fallback.
	pub(crate) fn record_landmark_fallback(&self) {
		if self.instrumentation {
			self.match_metrics.landmark_fallbacks.fetch_add(1, Ordering::Relaxed);
		}
	}

	/// Records one actual cache/on-demand capture at the adapter boundary.
	pub(crate) fn record_tensor_capture(
		&self,
		tensor: &crate::graph::tensor::EndpointTensor,
		binding: Option<&crate::datastore::QueryTensorCache>,
	) {
		if !self.instrumentation {
			return;
		}
		if let Some(binding) = binding {
			self.match_metrics.tensor_cache_hits.fetch_add(1, Ordering::Relaxed);
			self.match_metrics.tensor_artifact_bytes.fetch_add(binding.bytes, Ordering::Relaxed);
		} else {
			self.match_metrics.tensor_fallbacks.fetch_add(1, Ordering::Relaxed);
		}
		let (rows, connectors, sides) = tensor.shape();
		self.match_metrics.tensor_cells.fetch_add(
			(rows as u64).saturating_mul(connectors as u64).saturating_mul(sides as u64),
			Ordering::Relaxed,
		);
		self.match_metrics.tensor_nonzeros.fetch_add(
			(tensor.source.stored_cells() as u64)
				.saturating_add(tensor.target.stored_cells() as u64),
			Ordering::Relaxed,
		);
	}

	/// Records only completed converged analytical iterations.
	pub(crate) fn record_tensor_iterations(&self, iterations: u64) {
		if self.instrumentation {
			self.match_metrics.tensor_iterations.fetch_add(iterations, Ordering::Relaxed);
		}
	}

	/// Returns MATCH counters collected so far.
	pub(crate) fn match_metrics(&self) -> MatchMetricsSnapshot {
		MatchMetricsSnapshot {
			algorithm_work: self.match_metrics.algorithm_work.load(Ordering::Acquire),
			algorithm_paths: self.match_metrics.algorithm_paths.load(Ordering::Acquire),
			tensor_cache_hits: self.match_metrics.tensor_cache_hits.load(Ordering::Acquire),
			tensor_fallbacks: self.match_metrics.tensor_fallbacks.load(Ordering::Acquire),
			tensor_cells: self.match_metrics.tensor_cells.load(Ordering::Acquire),
			tensor_nonzeros: self.match_metrics.tensor_nonzeros.load(Ordering::Acquire),
			tensor_iterations: self.match_metrics.tensor_iterations.load(Ordering::Acquire),
			tensor_artifact_bytes: self.match_metrics.tensor_artifact_bytes.load(Ordering::Acquire),
			landmark_lookups: self.match_metrics.landmark_lookups.load(Ordering::Acquire),
			landmark_fallbacks: self.match_metrics.landmark_fallbacks.load(Ordering::Acquire),
			landmark_artifact_bytes: self
				.match_metrics
				.landmark_artifact_bytes
				.load(Ordering::Acquire),
			seeds: self.match_metrics.seeds.load(Ordering::Acquire),
			frontier_peak: self.match_metrics.frontier_peak.load(Ordering::Acquire),
			trace_nodes: self.match_metrics.trace_nodes.load(Ordering::Acquire),
			graph_opens: self.match_metrics.graph_opens.load(Ordering::Acquire),
			containment_links: self.match_metrics.containment_links.load(Ordering::Acquire),
			hyper_assignments: self.match_metrics.hyper_assignments.load(Ordering::Acquire),
			cancellation_checks: self.match_metrics.cancellation_checks.load(Ordering::Acquire),
			physical_windows: self.match_metrics.physical_windows.load(Ordering::Acquire),
			physical_resident_pages: self
				.match_metrics
				.physical_resident_pages
				.load(Ordering::Acquire),
			physical_wal_pages: self.match_metrics.physical_wal_pages.load(Ordering::Acquire),
			physical_sync_pages: self.match_metrics.physical_sync_pages.load(Ordering::Acquire),
			physical_ioring_pages: self.match_metrics.physical_ioring_pages.load(Ordering::Acquire),
		}
	}

	/// Records graph source row.
	pub(crate) fn record_graph_source_row(&self) {
		if self.instrumentation {
			self.graph_assembly_metrics.source_rows.fetch_add(1, Ordering::Relaxed);
		}
	}

	/// Records graph selected atom.
	pub(crate) fn record_graph_selected_atom(&self, kind: crate::graph::AtomKind) {
		if !self.instrumentation {
			return;
		}
		match kind {
			crate::graph::AtomKind::Node => {
				self.graph_assembly_metrics.selected_nodes.fetch_add(1, Ordering::Relaxed);
			}
			crate::graph::AtomKind::Edge => {
				self.graph_assembly_metrics.selected_edges.fetch_add(1, Ordering::Relaxed);
			}
		}
	}

	/// Records graph selected counts.
	pub(crate) fn record_graph_selected_counts(&self, nodes: usize, edges: usize) {
		if !self.instrumentation {
			return;
		}
		self.graph_assembly_metrics
			.selected_nodes
			.fetch_add(u64::try_from(nodes).unwrap_or(u64::MAX), Ordering::Relaxed);
		self.graph_assembly_metrics
			.selected_edges
			.fetch_add(u64::try_from(edges).unwrap_or(u64::MAX), Ordering::Relaxed);
	}

	/// Records graph closure atom.
	pub(crate) fn record_graph_closure_atom(&self, kind: crate::graph::AtomKind) {
		if !self.instrumentation {
			return;
		}
		match kind {
			crate::graph::AtomKind::Node => {
				self.graph_assembly_metrics.closure_nodes.fetch_add(1, Ordering::Relaxed);
			}
			crate::graph::AtomKind::Edge => {
				self.graph_assembly_metrics.closure_edges.fetch_add(1, Ordering::Relaxed);
			}
		}
	}

	/// Records graph deduplicated value.
	pub(crate) fn record_graph_deduplicated_value(&self) {
		if self.instrumentation {
			self.graph_assembly_metrics.deduplicated_values.fetch_add(1, Ordering::Relaxed);
		}
	}

	/// Records graph scope slots.
	pub(crate) fn record_graph_scope_slots(&self, slots: usize) {
		if self.instrumentation {
			self.graph_assembly_metrics
				.scope_slots
				.fetch_add(u64::try_from(slots).unwrap_or(u64::MAX), Ordering::Relaxed);
		}
	}

	/// Records graph assembly result.
	pub(crate) fn record_graph_assembly_result(
		&self,
		nodes: usize,
		edges: usize,
		child_graphs: usize,
	) {
		if !self.instrumentation {
			return;
		}
		self.graph_assembly_metrics
			.final_nodes
			.fetch_add(u64::try_from(nodes).unwrap_or(u64::MAX), Ordering::Relaxed);
		self.graph_assembly_metrics
			.final_edges
			.fetch_add(u64::try_from(edges).unwrap_or(u64::MAX), Ordering::Relaxed);
		self.graph_assembly_metrics
			.child_graphs
			.fetch_add(u64::try_from(child_graphs).unwrap_or(u64::MAX), Ordering::Relaxed);
	}

	/// Records graph validation.
	pub(crate) fn record_graph_validation(&self, validation: Duration) {
		if !self.instrumentation {
			return;
		}
		self.graph_assembly_metrics
			.validation_ns
			.fetch_add(u64::try_from(validation.as_nanos()).unwrap_or(u64::MAX), Ordering::Relaxed);
	}

	/// Returns the graph assembly metrics result.
	pub(crate) fn graph_assembly_metrics(&self) -> GraphAssemblyMetricsSnapshot {
		GraphAssemblyMetricsSnapshot {
			source_rows: self.graph_assembly_metrics.source_rows.load(Ordering::Acquire),
			selected_nodes: self.graph_assembly_metrics.selected_nodes.load(Ordering::Acquire),
			selected_edges: self.graph_assembly_metrics.selected_edges.load(Ordering::Acquire),
			closure_nodes: self.graph_assembly_metrics.closure_nodes.load(Ordering::Acquire),
			closure_edges: self.graph_assembly_metrics.closure_edges.load(Ordering::Acquire),
			deduplicated_values: self
				.graph_assembly_metrics
				.deduplicated_values
				.load(Ordering::Acquire),
			final_nodes: self.graph_assembly_metrics.final_nodes.load(Ordering::Acquire),
			final_edges: self.graph_assembly_metrics.final_edges.load(Ordering::Acquire),
			child_graphs: self.graph_assembly_metrics.child_graphs.load(Ordering::Acquire),
			scope_slots: self.graph_assembly_metrics.scope_slots.load(Ordering::Acquire),
			validation_ns: self.graph_assembly_metrics.validation_ns.load(Ordering::Acquire),
		}
	}

	/// Sets continuation.
	pub(crate) fn set_continuation(&self, cursor: QueryCursor) {
		*self.continuation.lock() = Some(cursor);
	}

	/// Borrows the issued position without consuming publication state.
	pub(in crate::gql) fn continuation(&self) -> Option<QueryCursor> {
		*self.continuation.lock()
	}

	/// Samples caller-owned admission while preparing one nonconcurrent cursor
	/// frame.
	pub(in crate::gql) fn admitted_memory(&self) -> usize {
		self.barrier_memory.used.load(Ordering::Relaxed)
	}

	/// Sets pending index seek.
	pub(crate) fn set_pending_index_seek(&self, cursor: QueryCursor) {
		*self.pending_index_seek.lock() = Some(cursor);
	}

	/// Attaches pending index seek.
	pub(crate) fn attach_pending_index_seek(
		&self,
		transaction: Transaction,
	) -> FerrosGrynnResult<Option<Transaction>> {
		if self.cursors.is_none() {
			return Ok(Some(transaction));
		}
		let Some(cursor) = self.pending_index_seek.lock().take() else {
			return Ok(Some(transaction));
		};
		let runtime = self.cursors.as_ref().ok_or_else(|| {
			FerrosGrynnError::invalid_query_cursor(
				"pending index continuation has no session runtime",
			)
		})?;
		runtime.lock().attach_index_seek(cursor, transaction)?;
		Ok(None)
	}

	/// Takes continuation.
	pub(crate) fn take_continuation(&self) -> Option<QueryCursor> {
		self.cursors.as_ref()?;
		self.continuation.lock().take()
	}

	/// Performs the discard cursor operation.
	pub(crate) fn discard_cursor(&self, cursor: QueryCursor) {
		let mut pending = self.pending_index_seek.lock();
		if pending.as_ref() == Some(&cursor) {
			pending.take();
		}
		drop(pending);
		if let Some(runtime) = &self.cursors {
			drop(runtime.lock().close(cursor));
		}
	}

	/// Performs the discard continuation operation.
	pub(crate) fn discard_continuation(&self) {
		if let Some(cursor) = self.take_continuation() {
			self.discard_cursor(cursor);
		}
	}

	/// Performs the notify operation.
	pub(crate) fn notify(&self, status: Status) {
		self.notifications.lock().push(status);
	}

	/// Returns the outcome result.
	pub(crate) fn outcome(&self, value: Value) -> FerrosGrynnResult<Outcome<Value>> {
		let mut statuses = StatusReport::successful();
		for status in self.notifications.lock().drain(..) {
			statuses.push(status).map_err(|error| {
				FerrosGrynnError::invalid_argument(format!(
					"Invalid execution notification: {error}"
				))
			})?;
		}
		Ok(Outcome::new(value, statuses))
	}
}

/// Named function frame state.
pub(in crate::gql) struct NamedFunctionFrame {
	/// The shared recursion authority released when the invocation frame exits.
	depths: Option<Arc<RuntimeBudget>>,
	/// Function associated with this named function frame.
	function: FunctionId,
}

impl Drop for NamedFunctionFrame {
	/// Runs cleanup for this `NamedFunctionFrame` before its fields are
	/// released.
	fn drop(&mut self) {
		let Some(depths) = &self.depths else {
			return;
		};
		let mut depths = depths.function_depths.lock();
		if let Some(depth) = depths.get_mut(&self.function) {
			*depth = depth.saturating_sub(1);
			if *depth == 0 {
				depths.remove(&self.function);
			}
		}
	}
}

/// Resolves duration.
pub fn resolve_duration(
	value: &TimeoutValue,
	bindings: &Object,
	subject: &str,
) -> FerrosGrynnResult<Duration> {
	match value {
		TimeoutValue::Literal {
			value,
			..
		} => parse_fixed_duration(value, subject),
		TimeoutValue::Parameter(parameter) => {
			let Some(value) = bindings.get(&parameter.value) else {
				return Err(FerrosGrynnError::invalid_argument(format!(
					"{subject} parameter `${}` is not defined.",
					parameter.value
				)));
			};
			parameter_duration(value, subject)
		}
	}
}

/// Converts a borrowed scalar without copying or traversing invalid payloads.
pub(in crate::gql) fn parameter_duration(
	value: &Value,
	subject: &str,
) -> FerrosGrynnResult<Duration> {
	let Value::Duration(value) = value else {
		return Err(FerrosGrynnError::invalid_argument(format!(
			"{subject} parameter has type {}, expected duration.",
			value.kind().name()
		)));
	};
	let nanoseconds = value.fixed_nanoseconds().ok_or_else(|| {
		FerrosGrynnError::invalid_argument(format!(
			"{subject} cannot use a calendar duration containing years or months."
		))
	})?;
	if nanoseconds < 0 {
		return Err(FerrosGrynnError::invalid_argument(format!("{subject} cannot be negative.")));
	}
	let nanoseconds = u64::try_from(nanoseconds).map_err(|_conversion_error| {
		FerrosGrynnError::invalid_argument(format!("{subject} is too large for this platform."))
	})?;
	Ok(Duration::from_nanos(nanoseconds))
}

/// Parses fixed duration.
fn parse_fixed_duration(value: &str, subject: &str) -> FerrosGrynnResult<Duration> {
	let split = value
		.char_indices()
		.find_map(|(index, character)| character.is_alphabetic().then_some(index))
		.ok_or_else(|| {
			FerrosGrynnError::invalid_argument(format!("{subject} requires a duration unit."))
		})?;
	let (number, unit) = value.split_at(split);
	let number = number.replace('_', "").parse::<f64>().map_err(|_parse_error| {
		FerrosGrynnError::invalid_argument(format!("Invalid {subject} duration `{value}`."))
	})?;
	let scale = match unit.to_ascii_lowercase().as_str() {
		"ns" => 1.0e-9,
		"us" => 1.0e-6,
		"ms" => 1.0e-3,
		"s" => 1.0,
		"m" => 60.0,
		"h" => 3_600.0,
		"d" => 86_400.0,
		_ => {
			return Err(FerrosGrynnError::invalid_argument(format!(
				"Unsupported {subject} duration unit `{unit}`."
			)));
		}
	};
	let seconds = number * scale;
	Duration::try_from_secs_f64(seconds).map_err(|_range_error| {
		FerrosGrynnError::invalid_argument(format!("Invalid {subject} duration `{value}`."))
	})
}

#[cfg(test)]
/// Regression tests for options behavior.
mod tests {
	use std::num::NonZeroUsize;

	use super::{ExecutionControl, QueryExecutionOptions, parse_fixed_duration};
	#[cfg(test)]
	use crate::test_assertions::AssertResult as _;
	use crate::{
		gql::ExecutionMode,
		values::{Object, Value},
	};

	/// Compiler checks and child runtimes share the caller's cancellation
	/// token.
	#[test]
	fn cancellation_is_observed_by_runtime_compiler_and_children() {
		let cancellation = super::QueryCancellation::default();
		let options = QueryExecutionOptions {
			cancellation: Some(cancellation.clone()),
			..QueryExecutionOptions::default()
		};
		let control = ExecutionControl::new(&options, None);
		let child = control.child(None);
		let checks = control.interruption();
		assert!(control.check().is_ok());
		cancellation.cancel();
		assert!(cancellation.is_cancelled());
		assert!(options.cancellation.as_ref().is_some_and(super::QueryCancellation::is_cancelled));
		assert!(control.check().is_err());
		assert!(child.check().is_err());
		assert!(checks.check().is_err());
	}

	/// Verifies duration at rounded u64 boundary is rejected without panicking.
	#[test]
	fn duration_at_rounded_u64_boundary_is_rejected_without_panicking() {
		assert!(parse_fixed_duration("18446744073709551615s", "TIMEOUT").is_err());
	}

	/// Untimed reuse does not anchor nested or subsequently resolved deadlines
	/// early.
	#[test]
	fn session_untimed_origin_preserves_timed_and_nested_deadlines() {
		use std::{
			sync::Arc,
			time::{Duration, Instant},
		};

		use crate::{gql_status::FerrosGrynnError, test_assertions::AssertResult as _};

		let now = Instant::now();
		let old = now
			.checked_sub(Duration::from_secs(1))
			.ok_or_else(|| {
				FerrosGrynnError::invalid_argument("clock fixture cannot subtract one second")
			})
			.assert_ok();
		let construct = |options: &QueryExecutionOptions, statement_timed| {
			ExecutionControl::with_session_runtime(
				options,
				statement_timed,
				old,
				None,
				Arc::new(super::LanguageClosureState::default()),
			)
		};
		let untimed = construct(&QueryExecutionOptions::default(), false);
		assert_eq!(untimed.started, old);
		assert!(untimed.deadline().is_none());
		let invoked = Instant::now();
		let child = untimed.checked_child(Some(Duration::from_secs(1))).assert_ok();
		assert!(child.deadline().is_some_and(|deadline| {
			invoked.checked_add(Duration::from_secs(1)).is_some_and(|minimum| deadline >= minimum)
		}));
		let mut timed = construct(&QueryExecutionOptions::default(), true);
		assert!(timed.started >= now);
		let origin = timed.started;
		timed.restrict_timeout(Duration::from_secs(1)).assert_ok();
		assert_eq!(timed.deadline(), origin.checked_add(Duration::from_secs(1)));
		let configured = construct(
			&QueryExecutionOptions {
				statement_timeout: Some(Duration::from_secs(1)),
				..QueryExecutionOptions::default()
			},
			false,
		);
		assert!(configured.started >= now);
		assert_eq!(configured.deadline(), configured.started.checked_add(Duration::from_secs(1)));
	}

	/// Resolution after analysis cannot renew a top-level timeout.
	#[test]
	fn duration_top_level_clock_anchor_and_default_minimum() {
		use crate::test_assertions::AssertResult as _;

		let mut control = ExecutionControl::new(&QueryExecutionOptions::default(), None);
		let origin = control.started;
		control.restrict_timeout(std::time::Duration::from_secs(1)).assert_ok();
		assert_eq!(control.deadline(), origin.checked_add(std::time::Duration::from_secs(1)));
		let mut copy = control.clone();
		copy.restrict_timeout(std::time::Duration::from_secs(2)).assert_ok();
		assert_eq!(copy.deadline(), control.deadline());
		assert_eq!(copy.started, origin);
		assert_eq!(control.child(None).started, origin);
		let mut earlier = ExecutionControl::new(
			&QueryExecutionOptions {
				statement_timeout: Some(std::time::Duration::from_millis(50)),
				..QueryExecutionOptions::default()
			},
			None,
		);
		let default = earlier.deadline();
		earlier.restrict_timeout(std::time::Duration::from_secs(1)).assert_ok();
		assert_eq!(earlier.deadline(), default);
		assert!(earlier.restrict_timeout(std::time::Duration::MAX).is_err());
	}

	/// Verifies child controls share one barrier budget.
	#[test]
	fn child_controls_share_one_barrier_budget() {
		let options = QueryExecutionOptions {
			barrier_memory_bytes: NonZeroUsize::new(64).unwrap_or(NonZeroUsize::MIN),
			..QueryExecutionOptions::default()
		};
		let control = ExecutionControl::new(&options, None);
		let child = control.child(None);
		let mut outer = control.barrier_account();
		let mut nested = child.barrier_account();

		assert!(outer.reserve(48));
		assert!(!nested.reserve(17));
		assert!(nested.reserve(16));
		drop(outer);

		let mut replacement = control.barrier_account();
		assert!(replacement.reserve(48));
	}

	/// Verifies deterministic statement seed covers parameters and user seed.
	#[test]
	fn deterministic_statement_seed_covers_parameters_and_user_seed() {
		let mut parameters = Object::new();
		parameters.insert("value", Value::from(7_i64));
		let options = QueryExecutionOptions {
			deterministic_seed: 11,
			..QueryExecutionOptions::default()
		};
		let first = ExecutionControl::new(&options, None)
			.with_execution_mode(ExecutionMode::Deterministic, &parameters)
			.statement_seed(13, 17);
		let second = ExecutionControl::new(&options, None)
			.with_execution_mode(ExecutionMode::Deterministic, &parameters)
			.statement_seed(13, 17);
		assert_eq!(first, second);

		parameters.insert("value", Value::from(8_i64));
		let changed = ExecutionControl::new(&options, None)
			.with_execution_mode(ExecutionMode::Deterministic, &parameters)
			.statement_seed(13, 17);
		assert_ne!(first, changed);
	}

	/// Verifies path expansion budget is positive and shared by children.
	#[test]
	fn path_expansion_budget_is_positive_and_shared_by_children() {
		let invalid = QueryExecutionOptions {
			max_path_expansions: 0,
			..QueryExecutionOptions::default()
		};
		assert!(invalid.validate().is_err());

		let options = QueryExecutionOptions {
			max_path_expansions: 1,
			..QueryExecutionOptions::default()
		};
		let control = ExecutionControl::new(&options, None);
		let child = control.child(None);
		assert!(control.record_path_expansion().is_ok());
		assert!(child.record_path_expansion().is_err());
	}

	/// Verifies graph work admission is shared and only complete work is
	/// refunded.
	#[test]
	fn graph_work_admission_is_shared_and_only_complete_work_is_refunded() {
		let mut options = QueryExecutionOptions::default();
		options.graph_algorithm_limits.max_work = 100;
		let control = ExecutionControl::new(&options, None);
		let child = control.child(None);
		control.record_graph_work(100).assert_ok();
		assert!(child.record_graph_work(1).is_err());
		control.release_graph_work(70);
		assert_eq!(child.graph_algorithm_limits().max_work, 70);
		child.record_graph_work(70).assert_ok();
		assert!(control.record_graph_work(1).is_err());
	}

	/// Verifies exhausted path quota does not reject non enumerating graph
	/// work.
	#[test]
	fn exhausted_path_quota_does_not_reject_non_enumerating_graph_work() {
		let mut options = QueryExecutionOptions::default();
		options.graph_algorithm_limits.max_paths = 1;
		let control = ExecutionControl::new(&options, None);
		control.record_graph_paths(1).assert_ok();
		let remaining = control.graph_algorithm_limits();
		assert_eq!(remaining.max_paths, 0);
		assert!(crate::graph::tensor::control::Control::new(remaining, &mut || Ok(())).is_ok());
		assert!(control.record_graph_paths(1).is_err());
	}
}
