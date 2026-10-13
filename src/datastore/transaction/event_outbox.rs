//! Bounded accepted payloads and attempt markers use ordinary transaction
//! replay.

use std::sync::Arc;

use super::{Mutation, Transaction};
use crate::{
	datastore::{
		ConflictKey,
		EventCause,
		EventDefinition,
		EventInvocation,
		EventInvocationState,
		JobId,
	},
	gql::ExecutionControl,
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	values::Text,
};

impl Transaction {
	/// Enters one accepted handler without resetting an ancestor MAXDEPTH.
	pub(crate) fn with_event_cause<T>(
		&mut self,
		event: &EventDefinition,
		apply: impl FnOnce(&mut Self, EventCause) -> FerrosGrynnResult<T>,
	) -> FerrosGrynnResult<T> {
		let cause = EventCause::next(self.event_cause, event.execution())?;
		let previous = self.event_cause.replace(cause);
		let result = apply(self, cause);
		self.event_cause = previous;
		result
	}

	/// Enqueue admission precedes payload ownership, COW map copies and
	/// publication.
	pub(crate) fn enqueue_event_payload(
		&mut self,
		event: &EventDefinition,
		cause: EventCause,
		payload: Vec<u8>,
		control: &ExecutionControl,
	) -> FerrosGrynnResult<JobId> {
		control.check()?;
		if self.import_events.is_some() {
			self.admit_import_event_payload(payload.len())?;
			let due = jiff::Timestamp::now()
				.as_nanosecond()
				.checked_add(
					i128::try_from(self.store.inner.event_processing_interval.as_nanos()).map_err(
						|error| {
							FerrosGrynnError::operation_overflow("EVENT interval")
								.with_technical_source(error)
						},
					)?,
				)
				.ok_or_else(|| FerrosGrynnError::operation_overflow("EVENT due time"))?;
			let bytes = payload.len().saturating_add(std::mem::size_of::<EventInvocation>() + 32);
			self.ensure_capacity(bytes)?;
			return self.spool_event_invocation(Arc::new(EventInvocation::queued(
				event,
				cause,
				Arc::from(payload),
				due,
			)?));
		}
		self.working.event_outbox().admit_pending(payload.len())?;
		let map_bytes = crate::values::storage::tree_allocation_bytes(
			self.working.event_outbox().invocations.len() + 1,
			std::mem::size_of::<(JobId, Arc<EventInvocation>)>(),
		)?;
		let bytes = payload
			.len()
			.checked_add(map_bytes)
			.and_then(|bytes| bytes.checked_add(1024))
			.ok_or_else(|| FerrosGrynnError::operation_overflow("EVENT enqueue bytes"))?;
		let bytes = bytes
			.checked_add(self.working.event_revision_copy_bytes()?)
			.ok_or_else(|| FerrosGrynnError::operation_overflow("EVENT enqueue revision bytes"))?;
		self.ensure_capacity(bytes)?;
		let due = jiff::Timestamp::now()
			.as_nanosecond()
			.checked_add(
				i128::try_from(self.store.inner.event_processing_interval.as_nanos()).map_err(
					|error| {
						FerrosGrynnError::operation_overflow("EVENT interval")
							.with_technical_source(error)
					},
				)?,
			)
			.ok_or_else(|| FerrosGrynnError::operation_overflow("EVENT due time"))?;
		let invocation = Arc::new(EventInvocation::queued(event, cause, Arc::from(payload), due)?);
		let id = invocation.id;
		Arc::make_mut(self.working.event_outbox_mut())
			.enqueue(Arc::clone(&invocation), self.snapshot.commit_ts)?;
		self.insert_write(ConflictKey::EventInvocation(id), 0);
		self.write_bytes += bytes;
		self.mark_range_change(ConflictKey::EventOutboxRange);
		self.mutations.push(Mutation::EnqueueEvent(invocation));
		Ok(id)
	}

	/// Starts the existing worker only when an ordinary handler accepts ASYNC
	/// work.
	pub(crate) fn start_event_worker(&self) -> FerrosGrynnResult<()> {
		if self.import_events.is_some() {
			return Ok(());
		}
		self.store.inner.maintenance.start(&self.store.inner)
	}

	/// Historical depth is installed for an attempt; retry and recovery do not
	/// add depth.
	pub(crate) fn run_accepted_event(
		&mut self,
		payload: &crate::storage::event_outbox::EventPayload,
		cause: EventCause,
		control: &ExecutionControl,
	) -> FerrosGrynnResult<()> {
		let previous = self.event_cause.replace(cause);
		let result = self.with_event_control(control, |transaction| {
			crate::gql::execute_event_handler(
				transaction,
				&payload.definition,
				&payload.arguments,
				control,
				Some(&payload.functions),
			)
		});
		self.event_cause = previous;
		result
	}

	/// Lifecycle writes share the effects transaction and cannot race KILL
	/// publication.
	pub(crate) fn transition_event_invocation(
		&mut self,
		id: JobId,
		expected_attempts: u16,
		state: EventInvocationState,
		due_at_ns: i128,
		diagnostic: Option<Text>,
	) -> FerrosGrynnResult<()> {
		self.prepare_control()?;
		self.record_read(ConflictKey::EventInvocation(id));
		let bytes = crate::values::storage::tree_allocation_bytes(
			self.working.event_outbox().invocations.len(),
			std::mem::size_of::<(JobId, Arc<EventInvocation>)>(),
		)?
		.checked_add(diagnostic.as_ref().map_or(0, Text::len))
		.and_then(|bytes| bytes.checked_add(1024))
		.ok_or_else(|| FerrosGrynnError::operation_overflow("EVENT lifecycle bytes"))?;
		let bytes =
			bytes.checked_add(self.working.event_revision_copy_bytes()?).ok_or_else(|| {
				FerrosGrynnError::operation_overflow("EVENT lifecycle revision bytes")
			})?;
		self.ensure_capacity(bytes)?;
		Arc::make_mut(self.working.event_outbox_mut()).transition(
			id,
			expected_attempts,
			state,
			due_at_ns,
			diagnostic.clone(),
			self.snapshot.commit_ts,
		)?;
		self.insert_write(ConflictKey::EventInvocation(id), 0);
		self.write_bytes += bytes;
		self.mark_range_change(ConflictKey::EventOutboxRange);
		self.mutations.push(Mutation::TransitionEvent {
			id,
			expected_attempts,
			state,
			due_at_ns,
			diagnostic,
		});
		Ok(())
	}
}
