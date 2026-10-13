//! Bounded immutable accepted invocations stay private across import segments.

use std::sync::Arc;

use super::Transaction;
use crate::{
	datastore::{EventInvocation, JobId},
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
};

#[cfg(test)]
mod tests;

/// Truncation savepoints preserve capacity reservations after item rejection.
#[derive(Clone, Copy)]
pub(super) struct EventSpoolCheckpoint {
	/// Retained immutable invocations.
	len: usize,
	/// Retained payload and invocation metadata bytes.
	bytes: usize,
	/// Exact pending byte sum restored without rescanning accepted payloads.
	payload_bytes: usize,
}

/// This optional owner exists only for EVENTS=ON imports.
#[derive(Default)]
pub(super) struct ImportEventSpool {
	/// Accepted versions and contexts; no executable IR is retained.
	invocations: Vec<Arc<EventInvocation>>,
	/// Exact payload lengths for the global pending quota.
	payload_bytes: usize,
	/// Payload and invocation metadata retained independently of segment write
	/// sets.
	bytes: usize,
}

impl ImportEventSpool {
	/// Captures rollback positions without cloning payloads or the vector.
	pub(super) fn checkpoint(&self) -> EventSpoolCheckpoint {
		EventSpoolCheckpoint {
			len: self.invocations.len(),
			bytes: self.bytes,
			payload_bytes: self.payload_bytes,
		}
	}

	/// Discards rejected item effects while preserving the actual allocated
	/// vector capacity.
	pub(super) fn restore(&mut self, checkpoint: EventSpoolCheckpoint) {
		self.invocations.truncate(checkpoint.len);
		self.bytes = checkpoint.bytes;
		self.payload_bytes = checkpoint.payload_bytes;
	}

	/// Shared transaction admission includes immutable payloads and retained
	/// vector capacity.
	pub(super) const fn retained_bytes(&self) -> usize {
		self.bytes.saturating_add(
			self.invocations.capacity().saturating_mul(std::mem::size_of::<Arc<EventInvocation>>()),
		)
	}
}

impl Transaction {
	/// Checks aggregate source and transaction limits before copying encoded
	/// bytes into Arc.
	pub(super) fn admit_import_event_payload(&self, payload: usize) -> FerrosGrynnResult<()> {
		let spool = self
			.import_events
			.as_ref()
			.ok_or_else(|| FerrosGrynnError::invalid_argument("IMPORT event spool is absent"))?;
		let total = spool
			.payload_bytes
			.checked_add(payload)
			.ok_or_else(|| FerrosGrynnError::operation_overflow("IMPORT pending payload bytes"))?;
		self.store
			.inner
			.current_root()
			.event_outbox()
			.admit_pending_batch(spool.invocations.len() + 1, total)?;
		let vector = if spool.invocations.len() == spool.invocations.capacity() {
			spool.invocations.capacity().max(4).saturating_mul(2)
		} else {
			0
		};
		let bytes = payload
			.checked_add(std::mem::size_of::<EventInvocation>() + 32)
			.and_then(|bytes| {
				bytes
					.checked_add(vector.saturating_mul(std::mem::size_of::<Arc<EventInvocation>>()))
			})
			.ok_or_else(|| FerrosGrynnError::operation_overflow("IMPORT event admission bytes"))?;
		self.ensure_capacity(bytes)
	}

	/// Source and spool admission precede ownership transfer and vector growth.
	pub(super) fn spool_event_invocation(
		&mut self,
		invocation: Arc<EventInvocation>,
	) -> FerrosGrynnResult<JobId> {
		let spool = self
			.import_events
			.as_ref()
			.ok_or_else(|| FerrosGrynnError::invalid_argument("IMPORT event spool is absent"))?;
		let payload = invocation
			.payload
			.as_ref()
			.ok_or_else(|| FerrosGrynnError::invalid_argument("accepted EVENT payload is absent"))?
			.len();
		let total = spool
			.payload_bytes
			.checked_add(payload)
			.ok_or_else(|| FerrosGrynnError::operation_overflow("IMPORT pending payload bytes"))?;
		self.store
			.inner
			.current_root()
			.event_outbox()
			.admit_pending_batch(spool.invocations.len() + 1, total)?;
		let capacity = if spool.invocations.len() == spool.invocations.capacity() {
			spool.invocations.capacity().max(4).saturating_mul(2)
		} else {
			0
		};
		let bytes = payload
			.checked_add(std::mem::size_of::<EventInvocation>() + 32)
			.ok_or_else(|| FerrosGrynnError::operation_overflow("IMPORT event metadata bytes"))?;
		let allocation = capacity
			.checked_mul(std::mem::size_of::<Arc<EventInvocation>>())
			.and_then(|capacity| capacity.checked_add(bytes))
			.ok_or_else(|| FerrosGrynnError::operation_overflow("IMPORT event vector bytes"))?;
		self.ensure_capacity(allocation)?;
		let spool = self
			.import_events
			.as_mut()
			.ok_or_else(|| FerrosGrynnError::invalid_argument("IMPORT event spool disappeared"))?;
		if capacity != 0 {
			spool.invocations.try_reserve_exact(capacity - spool.invocations.len()).map_err(
				|error| {
					FerrosGrynnError::traversal_resource_limit_exceeded(
						"IMPORT event spool",
						allocation as u64,
					)
					.with_technical_source(error)
				},
			)?;
		}
		let id = invocation.id;
		spool.bytes = spool
			.bytes
			.checked_add(bytes)
			.ok_or_else(|| FerrosGrynnError::operation_overflow("IMPORT event retained bytes"))?;
		spool.payload_bytes = total;
		spool.invocations.push(invocation);
		Ok(id)
	}

	/// Rebuilds metadata from current controls on every optimistic END
	/// preparation.
	pub(super) fn publish_import_events(&mut self, maximum: usize) -> FerrosGrynnResult<()> {
		let Some(spool) = &self.import_events else {
			return Ok(());
		};
		if spool.invocations.is_empty() {
			return Ok(());
		}
		let map = crate::values::storage::tree_allocation_bytes(
			self.working.event_outbox().invocations.len().saturating_add(spool.invocations.len()),
			std::mem::size_of::<(JobId, Arc<EventInvocation>)>(),
		)?;
		let revisions = crate::values::storage::tree_allocation_bytes(
			self.working
				.event_outbox()
				.invocations
				.len()
				.saturating_add(spool.invocations.len() + 1),
			std::mem::size_of::<(crate::datastore::ConflictKey, u64)>(),
		)?;
		let bytes = spool
			.retained_bytes()
			.checked_add(map)
			.and_then(|bytes| bytes.checked_add(revisions))
			.ok_or_else(|| {
				FerrosGrynnError::operation_overflow("IMPORT event publication bytes")
			})?;
		if bytes > maximum {
			return Err(FerrosGrynnError::storage_decode_limit_exceeded(
				"IMPORT event publication",
				bytes as u64,
				maximum as u64,
			));
		}
		let stamp = self.working.commit_ts;
		let time = self.working.committed_at_ns;
		self.working
			.event_outbox()
			.admit_pending_batch(spool.invocations.len(), spool.payload_bytes)?;
		for invocation in &spool.invocations {
			Arc::make_mut(self.working.event_outbox_mut())
				.enqueue(Arc::clone(invocation), stamp)?;
			Arc::make_mut(self.working.event_outbox_mut()).stamp_time(
				invocation.id,
				stamp,
				time,
			)?;
			self.working
				.mark_version(crate::datastore::ConflictKey::EventInvocation(invocation.id), stamp);
		}
		if !spool.invocations.is_empty() {
			self.working.mark_version(crate::datastore::ConflictKey::EventOutboxRange, stamp);
		}
		Ok(())
	}

	/// END starts or wakes the worker only for successfully published private
	/// invocations.
	pub(in crate::datastore) fn has_import_event_jobs(&self) -> bool {
		self.import_events.as_ref().is_some_and(|spool| !spool.invocations.is_empty())
	}
}
