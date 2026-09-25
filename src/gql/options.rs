use std::{
	num::NonZeroUsize,
	path::PathBuf,
	sync::{
		Arc,
		atomic::{AtomicBool, AtomicUsize, Ordering},
	},
	time::{Duration, Instant},
};

pub(crate) const DEFAULT_BARRIER_MEMORY_BYTES: usize = 256 * 1024 * 1024;

use parking_lot::Mutex;

use super::{ParserSettings, QueryCursor, cursor::CursorRuntime};
use crate::{
	gql_status::{FerrosGrynnError, FerrosGrynnResult, Outcome, Status, StatusReport},
	syn::TimeoutValue,
	values::{Object, Value},
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
	/// Optional cooperative cancellation token.
	pub cancellation: Option<QueryCancellation>,
	/// Shared memory budget for SELECT barriers. Values above the engine
	/// default are rejected; callers may only tighten the limit.
	pub barrier_memory_bytes: NonZeroUsize,
	/// Optional root for transient query-run directories.
	pub temporary_directory: Option<PathBuf>,
}

impl Default for QueryExecutionOptions {
	fn default() -> Self {
		Self {
			parser_settings: ParserSettings::default(),
			statement_timeout: None,
			max_control_depth: 20,
			max_loop_iterations: 1_000_000,
			cancellation: None,
			barrier_memory_bytes: NonZeroUsize::new(DEFAULT_BARRIER_MEMORY_BYTES)
				.unwrap_or(NonZeroUsize::MIN),
			temporary_directory: None,
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
	barrier_memory: Arc<BarrierMemory>,
	pub(crate) temporary_directory: Option<PathBuf>,
	cursors: Option<Arc<Mutex<CursorRuntime>>>,
	continuation: Arc<Mutex<Option<QueryCursor>>>,
}

#[derive(Debug)]
struct BarrierMemory {
	limit: usize,
	used: AtomicUsize,
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
			barrier_memory: Arc::new(BarrierMemory {
				limit: options.barrier_memory_bytes.get(),
				used: AtomicUsize::new(0),
			}),
			temporary_directory: options.temporary_directory.clone(),
			cursors,
			continuation: Arc::new(Mutex::new(None)),
		}
	}

	pub(crate) fn check(&self) -> FerrosGrynnResult<()> {
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
			barrier_memory: self.barrier_memory.clone(),
			temporary_directory: self.temporary_directory.clone(),
			cursors: self.cursors.clone(),
			continuation: self.continuation.clone(),
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

	pub(crate) fn set_continuation(&self, cursor: QueryCursor) {
		*self.continuation.lock() = Some(cursor);
	}

	pub(crate) fn take_continuation(&self) -> Option<QueryCursor> {
		self.continuation.lock().take()
	}

	pub(crate) fn discard_cursor(&self, cursor: QueryCursor) {
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
}
