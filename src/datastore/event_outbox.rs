//! Snapshot-owned accepted invocation payloads and atomic completion markers.

use std::{collections::BTreeMap, sync::Arc};

use super::{EventDefinition, EventExecution, EventId, EventTarget, JobId, SchemaVersion};
use crate::{
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	values::Text,
};

/// Independent durable ceiling for pending invocations in one datastore.
pub const MAX_PENDING_INVOCATIONS: usize = 10_000;
/// Payload bytes remain bounded independently of transaction write admission.
pub const MAX_PENDING_BYTES: usize = 64 * 1024 * 1024;
/// Terminal metadata is retained without keeping completed handler payloads.
const MAX_TERMINAL_INVOCATIONS: usize = 10_000;
/// Terminal diagnostics cannot make authoritative snapshots exceed codec
/// scratch.
const MAX_TERMINAL_BYTES: usize = 8 * 1024 * 1024;
/// Stored diagnostics never retain unbounded handler output.
pub const MAX_DIAGNOSTIC_BYTES: usize = 4096;

/// Causal ancestry survives synchronous calls and durable asynchronous
/// transitions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EventCause {
	/// Root invocation is zero; every accepted descendant increases the depth.
	pub(crate) depth: u16,
	/// The tightest accepted ancestor limit; sync-only chains use control
	/// depth.
	pub(crate) ceiling: Option<u8>,
}

impl EventCause {
	/// Checks the next accepted invocation without weakening any ancestor
	/// bound.
	pub(crate) fn next(parent: Option<Self>, execution: EventExecution) -> FerrosGrynnResult<Self> {
		let depth = match parent {
			None => 0,
			Some(parent) => parent
				.depth
				.checked_add(1)
				.ok_or_else(|| FerrosGrynnError::operation_overflow("EVENT causal depth"))?,
		};
		let inherited = parent.and_then(|parent| parent.ceiling);
		let ceiling = match execution {
			EventExecution::Sync => inherited,
			EventExecution::Async(options) => Some(
				inherited.map_or(options.max_depth(), |ancestor| ancestor.min(options.max_depth())),
			),
		};
		if let Some(ceiling) = ceiling
			&& depth > u16::from(ceiling)
		{
			return Err(FerrosGrynnError::control_limit_exceeded(
				"EVENT causal depth",
				u64::from(ceiling),
			));
		}
		Ok(Self {
			depth,
			ceiling,
		})
	}
}

/// Authoritative lifecycle, independent of the administrative job projection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EventInvocationState {
	/// Accepted source publication is waiting for its due time.
	Queued,
	/// An incomplete attempt is replayable after recovery.
	Running,
	/// Handler effects and this marker share one primary WAL publication.
	Completed,
	/// Explicit cancellation excludes all subsequent attempts.
	Cancelled,
	/// The retry budget is exhausted; the diagnostic remains bounded.
	Failed,
}

impl EventInvocationState {
	/// Terminal markers retain no executable payload or event context.
	pub(crate) const fn is_pending(self) -> bool {
		matches!(self, Self::Queued | Self::Running)
	}
}

/// One immutable invocation revision, using a stable job identity for
/// administration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EventInvocation {
	/// Never changes between retries or recovery attempts.
	pub(crate) id: JobId,
	/// Accepted source identity, retained after ALTER/REMOVE.
	pub(crate) event: EventId,
	/// Accepted definition version, independent of the current schema epoch.
	pub(crate) version: SchemaVersion,
	/// Stable historical source owner.
	pub(crate) target: EventTarget,
	/// Authoritative lifecycle state.
	pub(crate) state: EventInvocationState,
	/// Number of distinct attempts started; recovery resumes an incomplete
	/// attempt.
	pub(crate) attempts: u16,
	/// Additional attempts after the first failure.
	pub(crate) retries: u8,
	/// Historical causal depth, unchanged by retries.
	pub(crate) depth: u8,
	/// Tightest retained ancestor/current MAXDEPTH.
	pub(crate) ceiling: u8,
	/// Earliest UTC epoch nanoseconds for the next attempt.
	pub(crate) due_at_ns: i128,
	/// Source publication stamp; assigned by ordinary replay.
	pub(crate) accepted_at: u64,
	/// Last state publication stamp, including atomic completion.
	pub(crate) updated_at: u64,
	/// Source commit time recorded independently of the retry due time.
	pub(crate) created_at_ns: i128,
	/// Last lifecycle commit time used by the administration projection.
	pub(crate) updated_at_ns: i128,
	/// Bounded terminal/last-attempt explanation.
	pub(crate) diagnostic: Option<Text>,
	/// Versioned source/context bytes; no typed executable IR is retained.
	pub(crate) payload: Option<Arc<[u8]>>,
}

impl EventInvocation {
	/// Builds checked queue metadata after the source codec admits the owned
	/// bytes.
	pub(crate) fn queued(
		event: &EventDefinition,
		cause: EventCause,
		payload: Arc<[u8]>,
		due_at_ns: i128,
	) -> FerrosGrynnResult<Self> {
		let EventExecution::Async(options) = event.execution() else {
			return Err(FerrosGrynnError::invalid_argument(
				"durable invocation requires ASYNC EVENT",
			));
		};
		let id = event.id().ok_or_else(|| {
			FerrosGrynnError::invalid_argument("durable invocation requires EventId")
		})?;
		jiff::Timestamp::from_nanosecond(due_at_ns).map_err(|error| {
			FerrosGrynnError::invalid_argument("EVENT due time is outside UTC range")
				.with_technical_source(error)
		})?;
		let depth = u8::try_from(cause.depth).map_err(|error| {
			FerrosGrynnError::control_limit_exceeded(
				"EVENT causal depth",
				u64::from(options.max_depth()),
			)
			.with_technical_source(error)
		})?;
		let ceiling = cause.ceiling.ok_or_else(|| {
			FerrosGrynnError::invalid_argument("ASYNC EVENT has no causal ceiling")
		})?;
		if depth > ceiling
			|| ceiling > options.max_depth()
			|| payload.is_empty()
			|| payload.len() > MAX_PENDING_BYTES
		{
			return Err(FerrosGrynnError::invalid_argument(
				"durable EVENT depth or payload is invalid",
			));
		}
		Ok(Self {
			id: JobId::new(),
			event: id,
			version: event.version(),
			target: event.target(),
			state: EventInvocationState::Queued,
			attempts: 0,
			retries: options.retries(),
			depth,
			ceiling,
			due_at_ns,
			accepted_at: 0,
			updated_at: 0,
			created_at_ns: 0,
			updated_at_ns: 0,
			diagnostic: None,
			payload: Some(payload),
		})
	}
}

/// One COW domain under the existing immutable historical root binding.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EventOutbox {
	/// Pending payloads and bounded terminal completion/cancellation markers.
	pub(crate) invocations: BTreeMap<JobId, Arc<EventInvocation>>,
	/// Exact sum of queued/running payload lengths, without decoder scratch.
	pub(crate) pending_bytes: usize,
	/// Queued and running identities admitted by the datastore ceiling.
	pub(crate) pending_count: usize,
}

impl EventOutbox {
	/// Freezes publication time after replay without altering the accepted
	/// payload.
	pub(crate) fn stamp_time(
		&mut self,
		id: JobId,
		stamp: u64,
		time: i128,
	) -> FerrosGrynnResult<()> {
		let invocation = self
			.invocations
			.get_mut(&id)
			.ok_or_else(|| FerrosGrynnError::maintenance_job_not_found(id.to_string()))?;
		if invocation.updated_at != stamp {
			return Err(FerrosGrynnError::invalid_argument(
				"EVENT lifecycle publication stamp differs",
			));
		}
		let invocation = Arc::make_mut(invocation);
		if invocation.accepted_at == stamp {
			invocation.created_at_ns = time;
		}
		invocation.updated_at_ns = time;
		Ok(())
	}

	/// Rejects recovered resource counts without pruning authoritative markers.
	pub(crate) fn validate_retained_bounds(&self) -> FerrosGrynnResult<()> {
		let terminal = self.invocations.len().saturating_sub(self.pending_count);
		let bytes = self
			.invocations
			.values()
			.filter(|invocation| !invocation.state.is_pending())
			.try_fold(0usize, |bytes, invocation| {
			bytes
				.checked_add(256 + invocation.diagnostic.as_ref().map_or(0, Text::len))
				.ok_or_else(|| FerrosGrynnError::operation_overflow("EVENT terminal bytes"))
		})?;
		if self.pending_count > MAX_PENDING_INVOCATIONS
			|| self.pending_bytes > MAX_PENDING_BYTES
			|| terminal > MAX_TERMINAL_INVOCATIONS
			|| bytes > MAX_TERMINAL_BYTES
		{
			return Err(FerrosGrynnError::invalid_argument(
				"EVENT retained counts exceed their bounds",
			));
		}
		Ok(())
	}

	/// Checks aggregate capacity before source publication or map insertion.
	pub(crate) fn admit_pending(&self, bytes: usize) -> FerrosGrynnResult<()> {
		self.admit_pending_batch(1, bytes)
	}

	/// Private import invocations share the datastore ceiling before END
	/// publication.
	pub(crate) fn admit_pending_batch(
		&self,
		additional: usize,
		bytes: usize,
	) -> FerrosGrynnResult<()> {
		let count = self
			.pending_count
			.checked_add(additional)
			.ok_or_else(|| FerrosGrynnError::operation_overflow("EVENT pending count"))?;
		let total = self
			.pending_bytes
			.checked_add(bytes)
			.ok_or_else(|| FerrosGrynnError::operation_overflow("EVENT pending payload bytes"))?;
		if count > MAX_PENDING_INVOCATIONS || total > MAX_PENDING_BYTES {
			return Err(FerrosGrynnError::traversal_resource_limit_exceeded(
				"EVENT pending outbox",
				total as u64,
			));
		}
		Ok(())
	}

	/// Replay installs accepted bytes without running any user code.
	pub(crate) fn enqueue(
		&mut self,
		invocation: Arc<EventInvocation>,
		published_at: u64,
	) -> FerrosGrynnResult<()> {
		if self.invocations.contains_key(&invocation.id)
			|| invocation.state != EventInvocationState::Queued
			|| invocation.attempts != 0
		{
			return Err(FerrosGrynnError::invalid_argument(
				"EVENT invocation identity or initial state is invalid",
			));
		}
		let bytes = invocation
			.payload
			.as_ref()
			.ok_or_else(|| FerrosGrynnError::invalid_argument("queued EVENT payload is absent"))?
			.len();
		self.admit_pending(bytes)?;
		let mut invocation = (*invocation).clone();
		invocation.accepted_at = published_at;
		invocation.updated_at = published_at;
		self.pending_count += 1;
		self.pending_bytes += bytes;
		self.invocations.insert(invocation.id, Arc::new(invocation));
		Ok(())
	}

	/// Publishes a checked attempt transition and releases terminal payload
	/// bytes.
	pub(crate) fn transition(
		&mut self,
		id: JobId,
		expected_attempts: u16,
		state: EventInvocationState,
		due_at_ns: i128,
		diagnostic: Option<Text>,
		published_at: u64,
	) -> FerrosGrynnResult<()> {
		if diagnostic.as_ref().is_some_and(|message| {
			message.len() > MAX_DIAGNOSTIC_BYTES || message.as_str().contains('\0')
		}) {
			return Err(FerrosGrynnError::invalid_argument("EVENT diagnostic exceeds its bound"));
		}
		let current = self
			.invocations
			.get(&id)
			.ok_or_else(|| FerrosGrynnError::maintenance_job_not_found(id.to_string()))?;
		if current.attempts != expected_attempts || !current.state.is_pending() {
			return Err(FerrosGrynnError::invalid_argument(
				"EVENT attempt or terminal marker changed",
			));
		}
		let attempts = match (current.state, state) {
			(EventInvocationState::Queued, EventInvocationState::Running) => current
				.attempts
				.checked_add(1)
				.filter(|attempts| *attempts <= u16::from(current.retries) + 1)
				.ok_or_else(|| {
					FerrosGrynnError::invalid_argument("EVENT retry budget is exhausted")
				})?,
			(
				EventInvocationState::Running,
				EventInvocationState::Running
				| EventInvocationState::Completed
				| EventInvocationState::Failed,
			) => current.attempts,
			(EventInvocationState::Running, EventInvocationState::Queued)
				if current.attempts <= u16::from(current.retries) =>
			{
				current.attempts
			}
			(_, EventInvocationState::Cancelled) => current.attempts,
			_ => {
				return Err(FerrosGrynnError::invalid_argument(
					"EVENT lifecycle transition is invalid",
				));
			}
		};
		let mut next = (**current).clone();
		next.state = state;
		next.attempts = attempts;
		next.due_at_ns = due_at_ns;
		next.diagnostic = diagnostic;
		next.updated_at = published_at;
		if !state.is_pending() {
			let bytes = next
				.payload
				.take()
				.ok_or_else(|| {
					FerrosGrynnError::invalid_argument("pending EVENT payload is absent")
				})?
				.len();
			let count = self.pending_count.checked_sub(1).ok_or_else(|| {
				FerrosGrynnError::invalid_argument("EVENT pending count is inconsistent")
			})?;
			let bytes = self.pending_bytes.checked_sub(bytes).ok_or_else(|| {
				FerrosGrynnError::invalid_argument("EVENT pending bytes are inconsistent")
			})?;
			self.pending_count = count;
			self.pending_bytes = bytes;
		}
		self.invocations.insert(id, Arc::new(next));
		loop {
			let terminal = self.invocations.len().saturating_sub(self.pending_count);
			let bytes = self
				.invocations
				.values()
				.filter(|invocation| !invocation.state.is_pending())
				.fold(0usize, |bytes, invocation| {
					bytes.saturating_add(256 + invocation.diagnostic.as_ref().map_or(0, Text::len))
				});
			if terminal <= MAX_TERMINAL_INVOCATIONS && bytes <= MAX_TERMINAL_BYTES {
				break;
			}
			let oldest = self.invocations.iter().find_map(|(candidate, invocation)| {
				(!invocation.state.is_pending() && *candidate != id).then_some(*candidate)
			});
			if let Some(oldest) = oldest {
				self.invocations.remove(&oldest);
			} else {
				return Err(FerrosGrynnError::invalid_argument(
					"EVENT terminal retention is inconsistent",
				));
			}
		}
		Ok(())
	}
}

#[cfg(test)]
/// Regression tests for event outbox behavior.
mod tests {
	use super::*;
	use crate::datastore::EventAsyncOptions;

	/// Verifies event cause preserves zero and ancestor bounds across sync
	/// calls.
	#[test]
	fn event_cause_preserves_zero_and_ancestor_bounds_across_sync_calls() {
		use crate::test_assertions::AssertResult as _;

		let zero =
			EventCause::next(None, EventExecution::Async(EventAsyncOptions::new(0, 0).assert_ok()))
				.assert_ok();
		assert_eq!(zero.depth, 0);
		assert!(EventCause::next(Some(zero), EventExecution::Sync).is_err());
		let mut cause =
			EventCause::next(None, EventExecution::Async(EventAsyncOptions::new(0, 3).assert_ok()))
				.assert_ok();
		for depth in 1..=3 {
			cause = EventCause::next(Some(cause), EventExecution::Sync).assert_ok();
			assert_eq!(cause.depth, depth);
		}
		assert!(
			EventCause::next(
				Some(cause),
				EventExecution::Async(EventAsyncOptions::new(0, 16).assert_ok())
			)
			.is_err()
		);
		let mut cause = EventCause::next(
			None,
			EventExecution::Async(EventAsyncOptions::new(0, 16).assert_ok()),
		)
		.assert_ok();
		for _ in 0..16 {
			cause = EventCause::next(Some(cause), EventExecution::Sync).assert_ok();
		}
		assert!(EventCause::next(Some(cause), EventExecution::Sync).is_err());
	}
}
