use std::{
	num::NonZeroUsize,
	path::PathBuf,
	sync::{
		Arc,
		atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
	},
	time::{Duration, Instant},
};

pub(crate) const DEFAULT_BARRIER_MEMORY_BYTES: usize = 256 * 1024 * 1024;
static NEXT_STATEMENT_NONCE: AtomicU64 = AtomicU64::new(1);

fn next_statement_nonce() -> u64 {
	NEXT_STATEMENT_NONCE.fetch_add(1, Ordering::Relaxed)
}

use parking_lot::Mutex;

use super::{
	ExecutionMode,
	ParserSettings,
	QueryCursor,
	closure::{LanguageClosureEntry, LanguageClosureState},
	cursor::CursorRuntime,
	ir::LogicalClosure,
};
use crate::{
	datastore::Transaction,
	gql_status::{FerrosGrynnError, FerrosGrynnResult, Outcome, Status, StatusReport},
	syn::TimeoutValue,
	values::{Closure, Object, Value},
};

/// Cooperative cancellation handle shared with one or more query executions.
#[derive(Debug, Clone, Default)]
pub struct QueryCancellation(Arc<AtomicBool>);

impl QueryCancellation {
	/// Requests cancellation. In-flight synchronous system I/O observes it
	/// after the system call returns.
	pub fn cancel(&self) {
		self.0.store(true, Ordering::Release);
	}

	/// Returns whether cancellation has been requested.
	pub fn is_cancelled(&self) -> bool {
		self.0.load(Ordering::Acquire)
	}
}

/// Parser and bounded-runtime settings for one `execute` call.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct QueryExecutionOptions {
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
	/// considered by MATCH in one statement.
	pub max_path_expansions: u64,
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

impl Default for QueryExecutionOptions {
	fn default() -> Self {
		Self {
			parser_settings: ParserSettings::default(),
			statement_timeout: None,
			max_control_depth: 20,
			max_loop_iterations: 1_000_000,
			max_path_expansions: 1_000_000,
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
	pub(crate) fn validate(&self) -> FerrosGrynnResult<()> {
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

/// Statement-local deadline and cancellation state.
#[derive(Debug, Clone)]
pub(crate) struct ExecutionControl {
	deadline: Option<Instant>,
	timeout_ms: u64,
	cancellation: Option<QueryCancellation>,
	notifications: Arc<Mutex<Vec<Status>>>,
	pub(crate) max_control_depth: u32,
	pub(crate) max_loop_iterations: u64,
	max_path_expansions: u64,
	max_graph_nodes: u64,
	max_graph_edges: u64,
	path_expansions: Arc<AtomicU64>,
	match_metrics: Arc<MatchMetrics>,
	graph_assembly_metrics: Arc<GraphAssemblyMetrics>,
	barrier_memory: Arc<BarrierMemory>,
	pub(crate) temporary_directory: Option<PathBuf>,
	cursors: Option<Arc<Mutex<CursorRuntime>>>,
	continuation: Arc<Mutex<Option<QueryCursor>>>,
	pending_index_seek: Arc<Mutex<Option<QueryCursor>>>,
	closure_registry: Arc<LanguageClosureState>,
	execution_mode: ExecutionMode,
	user_seed: u64,
	parameter_fingerprint: u64,
	statement_nonce: u64,
	publication_deferred: bool,
	instrumentation: bool,
}

#[derive(Debug, Default)]
struct MatchMetrics {
	seeds: AtomicU64,
	frontier_peak: AtomicU64,
	trace_nodes: AtomicU64,
	graph_opens: AtomicU64,
	containment_links: AtomicU64,
	hyper_assignments: AtomicU64,
	cancellation_checks: AtomicU64,
	physical_windows: AtomicU64,
	physical_resident_pages: AtomicU64,
	physical_wal_pages: AtomicU64,
	physical_sync_pages: AtomicU64,
	physical_ioring_pages: AtomicU64,
}

#[derive(Debug, Default)]
struct GraphAssemblyMetrics {
	source_rows: AtomicU64,
	selected_nodes: AtomicU64,
	selected_edges: AtomicU64,
	closure_nodes: AtomicU64,
	closure_edges: AtomicU64,
	deduplicated_values: AtomicU64,
	final_nodes: AtomicU64,
	final_edges: AtomicU64,
	child_graphs: AtomicU64,
	scope_slots: AtomicU64,
	validation_ns: AtomicU64,
}

/// Snapshot of MATCH-specific counters collected by `EXPLAIN ANALYZE`.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct MatchMetricsSnapshot {
	pub(crate) seeds: u64,
	pub(crate) frontier_peak: u64,
	pub(crate) trace_nodes: u64,
	pub(crate) graph_opens: u64,
	pub(crate) containment_links: u64,
	pub(crate) hyper_assignments: u64,
	pub(crate) cancellation_checks: u64,
	pub(crate) physical_windows: u64,
	pub(crate) physical_resident_pages: u64,
	pub(crate) physical_wal_pages: u64,
	pub(crate) physical_sync_pages: u64,
	pub(crate) physical_ioring_pages: u64,
}

impl MatchMetricsSnapshot {
	/// Returns a delta between two observations of the same statement control.
	pub(crate) const fn saturating_sub(self, earlier: Self) -> Self {
		Self {
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
pub(crate) struct GraphAssemblyMetricsSnapshot {
	pub(crate) source_rows: u64,
	pub(crate) selected_nodes: u64,
	pub(crate) selected_edges: u64,
	pub(crate) closure_nodes: u64,
	pub(crate) closure_edges: u64,
	pub(crate) deduplicated_values: u64,
	pub(crate) final_nodes: u64,
	pub(crate) final_edges: u64,
	pub(crate) child_graphs: u64,
	pub(crate) scope_slots: u64,
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
struct BarrierMemory {
	limit: usize,
	used: AtomicUsize,
	peak: AtomicUsize,
	spill_bytes: AtomicU64,
	spill_runs: AtomicU64,
	row_pulls: AtomicU64,
}

/// Bytes retained by one in-memory SELECT barrier.
#[derive(Debug)]
pub(crate) struct BarrierAccount {
	memory: Arc<BarrierMemory>,
	bytes: usize,
}

impl BarrierAccount {
	pub(crate) fn new(limit: usize) -> Self {
		Self {
			memory: Arc::new(BarrierMemory {
				limit,
				used: AtomicUsize::new(0),
				peak: AtomicUsize::new(0),
				spill_bytes: AtomicU64::new(0),
				spill_runs: AtomicU64::new(0),
				row_pulls: AtomicU64::new(0),
			}),
			bytes: 0,
		}
	}

	pub(crate) fn reserve(&mut self, bytes: usize) -> bool {
		if bytes == 0 {
			return true;
		}
		let mut used = self.memory.used.load(Ordering::Acquire);
		loop {
			let Some(next) = used.checked_add(bytes).filter(|next| *next <= self.memory.limit)
			else {
				return false;
			};
			match self.memory.used.compare_exchange_weak(
				used,
				next,
				Ordering::AcqRel,
				Ordering::Acquire,
			) {
				Ok(_) => {
					self.memory.peak.fetch_max(next, Ordering::AcqRel);
					self.bytes += bytes;
					return true;
				}
				Err(current) => used = current,
			}
		}
	}

	pub(crate) fn release_all(&mut self) {
		if self.bytes != 0 {
			self.memory.used.fetch_sub(self.bytes, Ordering::AcqRel);
			self.bytes = 0;
		}
	}

	pub(crate) fn release(&mut self, bytes: usize) {
		let bytes = bytes.min(self.bytes);
		if bytes != 0 {
			self.memory.used.fetch_sub(bytes, Ordering::AcqRel);
			self.bytes -= bytes;
		}
	}

	pub(crate) fn record_spill(&self, bytes: u64) {
		self.memory.spill_bytes.fetch_add(bytes, Ordering::Relaxed);
		self.memory.spill_runs.fetch_add(1, Ordering::Relaxed);
	}
}

impl Drop for BarrierAccount {
	fn drop(&mut self) {
		self.release_all();
	}
}

impl ExecutionControl {
	pub(crate) fn new(options: &QueryExecutionOptions, timeout: Option<Duration>) -> Self {
		Self::with_cursors(options, timeout, None)
	}

	pub(crate) fn with_cursors(
		options: &QueryExecutionOptions,
		timeout: Option<Duration>,
		cursors: Option<Arc<Mutex<CursorRuntime>>>,
	) -> Self {
		Self::with_runtime(options, timeout, cursors, Arc::new(LanguageClosureState::default()))
	}

	pub(crate) fn with_runtime(
		options: &QueryExecutionOptions,
		timeout: Option<Duration>,
		cursors: Option<Arc<Mutex<CursorRuntime>>>,
		closure_registry: Arc<LanguageClosureState>,
	) -> Self {
		let timeout = match (options.statement_timeout, timeout) {
			(Some(left), Some(right)) => Some(left.min(right)),
			(Some(value), None) | (None, Some(value)) => Some(value),
			(None, None) => None,
		};
		Self {
			deadline: timeout.and_then(|value| Instant::now().checked_add(value)),
			timeout_ms: timeout
				.map(|value| u64::try_from(value.as_millis()).unwrap_or(u64::MAX))
				.unwrap_or(0),
			cancellation: options.cancellation.clone(),
			notifications: Arc::new(Mutex::new(Vec::new())),
			max_control_depth: options.max_control_depth,
			max_loop_iterations: options.max_loop_iterations,
			max_path_expansions: options.max_path_expansions,
			max_graph_nodes: options.max_graph_nodes,
			max_graph_edges: options.max_graph_edges,
			path_expansions: Arc::new(AtomicU64::new(0)),
			match_metrics: Arc::new(MatchMetrics::default()),
			graph_assembly_metrics: Arc::new(GraphAssemblyMetrics::default()),
			barrier_memory: Arc::new(BarrierMemory {
				limit: options.barrier_memory_bytes.get(),
				used: AtomicUsize::new(0),
				peak: AtomicUsize::new(0),
				spill_bytes: AtomicU64::new(0),
				spill_runs: AtomicU64::new(0),
				row_pulls: AtomicU64::new(0),
			}),
			temporary_directory: options.temporary_directory.clone(),
			cursors,
			continuation: Arc::new(Mutex::new(None)),
			pending_index_seek: Arc::new(Mutex::new(None)),
			closure_registry,
			execution_mode: ExecutionMode::BestEffort,
			user_seed: options.deterministic_seed,
			parameter_fingerprint: 0,
			statement_nonce: next_statement_nonce(),
			publication_deferred: false,
			instrumentation: false,
		}
	}

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

	pub(in crate::gql) fn create_closure(
		&self,
		plan: &LogicalClosure,
		slots: &[Value],
	) -> FerrosGrynnResult<Closure> {
		self.closure_registry.register(plan, slots, self.statement_nonce)
	}

	pub(crate) const fn statement_nonce(&self) -> u64 {
		self.statement_nonce
	}

	pub(in crate::gql) fn resolve_closure(
		&self,
		closure: &Closure,
	) -> FerrosGrynnResult<LanguageClosureEntry> {
		self.closure_registry.resolve(closure)
	}

	pub(crate) const fn deterministic(&self) -> bool {
		matches!(self.execution_mode, ExecutionMode::Deterministic)
	}

	pub(crate) const fn fast(&self) -> bool {
		matches!(self.execution_mode, ExecutionMode::Fast)
	}

	pub(crate) const fn with_publication_deferred(mut self, deferred: bool) -> Self {
		self.publication_deferred = deferred;
		self
	}

	pub(crate) const fn with_instrumentation(mut self) -> Self {
		self.instrumentation = true;
		self
	}

	pub(crate) const fn publication_deferred(&self) -> bool {
		self.publication_deferred
	}

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

	pub(crate) fn check(&self) -> FerrosGrynnResult<()> {
		if self.instrumentation {
			self.match_metrics.cancellation_checks.fetch_add(1, Ordering::Relaxed);
		}
		if self.cancellation.as_ref().is_some_and(QueryCancellation::is_cancelled) {
			return Err(FerrosGrynnError::query_cancelled());
		}
		if self.deadline.is_some_and(|deadline| Instant::now() >= deadline) {
			return Err(FerrosGrynnError::query_timed_out(self.timeout_ms));
		}
		Ok(())
	}

	pub(crate) fn child(&self, timeout: Option<Duration>) -> Self {
		let child_deadline = timeout.and_then(|value| Instant::now().checked_add(value));
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
			deadline,
			timeout_ms,
			cancellation: self.cancellation.clone(),
			notifications: self.notifications.clone(),
			max_control_depth: self.max_control_depth,
			max_loop_iterations: self.max_loop_iterations,
			max_path_expansions: self.max_path_expansions,
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
			execution_mode: self.execution_mode,
			user_seed: self.user_seed,
			parameter_fingerprint: self.parameter_fingerprint,
			statement_nonce: self.statement_nonce,
			publication_deferred: self.publication_deferred,
			instrumentation: self.instrumentation,
		}
	}

	pub(crate) fn cursors(&self) -> Option<&Arc<Mutex<CursorRuntime>>> {
		self.cursors.as_ref()
	}

	pub(crate) fn barrier_account(&self) -> BarrierAccount {
		BarrierAccount {
			memory: self.barrier_memory.clone(),
			bytes: 0,
		}
	}

	/// Returns the statement-wide barrier limit used by physical planning.
	pub(crate) fn barrier_memory_limit(&self) -> usize {
		self.barrier_memory.limit
	}

	/// Returns the statement-wide unique-node ceiling for owned graph results.
	pub(crate) const fn max_graph_nodes(&self) -> u64 {
		self.max_graph_nodes
	}

	/// Returns the statement-wide unique-edge ceiling for owned graph results.
	pub(crate) const fn max_graph_edges(&self) -> u64 {
		self.max_graph_edges
	}

	pub(crate) fn barrier_memory_peak(&self) -> usize {
		self.barrier_memory.peak.load(Ordering::Acquire)
	}

	pub(crate) fn spill_metrics(&self) -> (u64, u64) {
		(
			self.barrier_memory.spill_bytes.load(Ordering::Acquire),
			self.barrier_memory.spill_runs.load(Ordering::Acquire),
		)
	}

	pub(crate) fn row_pulls(&self) -> u64 {
		self.barrier_memory.row_pulls.load(Ordering::Acquire)
	}

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

	/// Returns MATCH counters collected so far.
	pub(crate) fn match_metrics(&self) -> MatchMetricsSnapshot {
		MatchMetricsSnapshot {
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

	pub(crate) fn record_graph_source_row(&self) {
		if self.instrumentation {
			self.graph_assembly_metrics.source_rows.fetch_add(1, Ordering::Relaxed);
		}
	}

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

	pub(crate) fn record_graph_deduplicated_value(&self) {
		if self.instrumentation {
			self.graph_assembly_metrics.deduplicated_values.fetch_add(1, Ordering::Relaxed);
		}
	}

	pub(crate) fn record_graph_scope_slots(&self, slots: usize) {
		if self.instrumentation {
			self.graph_assembly_metrics
				.scope_slots
				.fetch_add(u64::try_from(slots).unwrap_or(u64::MAX), Ordering::Relaxed);
		}
	}

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

	pub(crate) fn record_graph_validation(&self, validation: Duration) {
		if !self.instrumentation {
			return;
		}
		self.graph_assembly_metrics
			.validation_ns
			.fetch_add(u64::try_from(validation.as_nanos()).unwrap_or(u64::MAX), Ordering::Relaxed);
	}

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

	pub(crate) fn set_continuation(&self, cursor: QueryCursor) {
		*self.continuation.lock() = Some(cursor);
	}

	pub(crate) fn set_pending_index_seek(&self, cursor: QueryCursor) {
		*self.pending_index_seek.lock() = Some(cursor);
	}

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

	pub(crate) fn take_continuation(&self) -> Option<QueryCursor> {
		self.cursors.as_ref()?;
		self.continuation.lock().take()
	}

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

	pub(crate) fn discard_continuation(&self) {
		if let Some(cursor) = self.take_continuation() {
			self.discard_cursor(cursor);
		}
	}

	pub(crate) fn notify(&self, status: Status) {
		self.notifications.lock().push(status);
	}

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

pub(crate) fn resolve_timeout(
	value: Option<&TimeoutValue>,
	bindings: &Object,
) -> FerrosGrynnResult<Option<Duration>> {
	let Some(value) = value else {
		return Ok(None);
	};
	match value {
		TimeoutValue::Literal {
			value,
			..
		} => parse_fixed_duration(value).map(Some),
		TimeoutValue::Parameter(parameter) => {
			let Some(value) = bindings.get(&parameter.value) else {
				return Err(FerrosGrynnError::invalid_argument(format!(
					"TIMEOUT parameter `${}` is not defined.",
					parameter.value
				)));
			};
			let Value::Duration(value) = value else {
				return Err(FerrosGrynnError::invalid_argument(format!(
					"TIMEOUT parameter has type {}, expected duration.",
					value.value_type()
				)));
			};
			let nanoseconds = value.fixed_nanoseconds().ok_or_else(|| {
				FerrosGrynnError::invalid_argument(
					"TIMEOUT cannot use a calendar duration containing years or months.",
				)
			})?;
			if nanoseconds < 0 {
				return Err(FerrosGrynnError::invalid_argument("TIMEOUT cannot be negative."));
			}
			let nanoseconds = u64::try_from(nanoseconds).map_err(|_| {
				FerrosGrynnError::invalid_argument("TIMEOUT is too large for this platform.")
			})?;
			Ok(Some(Duration::from_nanos(nanoseconds)))
		}
	}
}

fn parse_fixed_duration(value: &str) -> FerrosGrynnResult<Duration> {
	let split = value
		.char_indices()
		.find_map(|(index, character)| character.is_alphabetic().then_some(index))
		.ok_or_else(|| FerrosGrynnError::invalid_argument("TIMEOUT requires a duration unit."))?;
	let (number, unit) = value.split_at(split);
	let number = number.replace('_', "").parse::<f64>().map_err(|_parse_error| {
		FerrosGrynnError::invalid_argument(format!("Invalid TIMEOUT duration `{value}`."))
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
				"Unsupported TIMEOUT duration unit `{unit}`."
			)));
		}
	};
	let seconds = number * scale;
	Duration::try_from_secs_f64(seconds).map_err(|_range_error| {
		FerrosGrynnError::invalid_argument(format!("Invalid TIMEOUT duration `{value}`."))
	})
}

#[cfg(test)]
mod tests {
	use std::num::NonZeroUsize;

	use super::{ExecutionControl, QueryExecutionOptions, parse_fixed_duration};
	use crate::{
		gql::ExecutionMode,
		values::{Object, Value},
	};

	#[test]
	fn duration_at_rounded_u64_boundary_is_rejected_without_panicking() {
		assert!(parse_fixed_duration("18446744073709551615s").is_err());
	}

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
}
