//! Whole-commit consumer state, separate from immutable database roots.
//!
//! Publication prepares an owned successor before the WAL barrier. The store
//! serializes issuance, confirmation and successor installation using its
//! commit mutex; capacity waiting never holds that mutex.

use std::{collections::VecDeque, mem::size_of, sync::Arc};

use fg_runtime::__internal::ValueInternal as _;

/// Owned SDK adapter and transactional ACK/close operations.
mod handle;
/// Store-owned consumer service and all-or-nothing publication permits.
mod registry;
/// Lazy saved-result replay with fixed authoritative bounds and file pins.
mod replay;
pub use handle::SubscriptionHandle;
pub(super) use registry::{ConsumerKey, Consumers, Reservations};
pub(super) use replay::Replay;

use crate::{
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	live::{LiveCursor, LiveDelivery, LiveGap, LiveGapReason, LiveNext, LiveOptions, LivePayload},
	values::Value,
};

/// Session-wide retained consumer ceiling; scratch is admitted separately.
pub(super) const SESSION_BYTES: usize = 256 * 1024 * 1024;
/// Fixed queue/cursor/control metadata, charged even when the queue is empty.
const QUEUE_BYTES: usize = 1024 + 2 * 64 * 1024 + size_of::<Entry>();

/// Checked retained payload cost, including identities and owned result images.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(super) struct Footprint {
	/// Data entries; whole-commit controls do not consume the event-entry cap.
	pub(super) entries: usize,
	/// Conservative owned bytes, including delivery and queue storage.
	pub(super) bytes: usize,
}

impl Footprint {
	/// Inspects owned images without reading primary records or allocating.
	pub(super) fn delivery(delivery: &LiveDelivery) -> FerrosGrynnResult<Self> {
		Self::payload(delivery.payload())
	}

	/// Shared capture/admission accounting, before durable envelope allocation.
	pub(in crate::datastore) fn payload(payload: &LivePayload) -> FerrosGrynnResult<Self> {
		let (entries, bytes) = match payload {
			LivePayload::Batch(batch) => {
				let mut bytes = 512_usize
					.checked_add(
						batch
							.bindings
							.capacity()
							.checked_mul(size_of::<crate::live::LiveBindingRevision>())
							.ok_or_else(overflow)?,
					)
					.ok_or_else(overflow)?;
				bytes = bytes
					.checked_add(
						batch
							.events
							.capacity()
							.checked_mul(size_of::<crate::live::LiveEvent>())
							.ok_or_else(overflow)?,
					)
					.ok_or_else(overflow)?;
				for entry in &batch.events {
					if let Some(output) = &entry.output {
						bytes = bytes.checked_add(output.reserved_bytes).ok_or_else(overflow)?;
					}
					bytes = bytes
						.checked_add(192)
						.and_then(|bytes| {
							bytes.checked_add(
								entry.before.as_ref().map_or(0, Value::estimated_heap_bytes),
							)
						})
						.and_then(|bytes| {
							bytes.checked_add(
								entry.after.as_ref().map_or(0, Value::estimated_heap_bytes),
							)
						})
						.and_then(|bytes| bytes.checked_add(entry.identity.estimated_heap_bytes()))
						.ok_or_else(overflow)?;
				}
				(batch.events.len(), bytes)
			}
			LivePayload::Graph(batch) => {
				(batch.delta().changes().len().max(1), batch.estimated_heap_bytes()?)
			}
			LivePayload::Gap(_) => (0, 512),
			// Computed diagnostics have a separate checked 64-KiB capture cap.
			LivePayload::Fault {
				..
			} => (0, 64 * 1024),
		};
		Ok(Self {
			entries,
			bytes,
		})
	}

	/// A whole data batch must fit even before any other delivery is retained.
	pub(in crate::datastore) fn fits_empty(
		payload: &LivePayload,
		options: &LiveOptions,
	) -> FerrosGrynnResult<bool> {
		let cost = Self::payload(payload)?;
		Ok(cost.entries == 0
			|| cost.entries <= options.queue_entries()
				&& cost.bytes.checked_add(size_of::<Entry>()).ok_or_else(overflow)?
					<= options.queue_bytes())
	}

	/// Adds already-admitted costs with explicit overflow detection.
	fn add(self, other: Self) -> FerrosGrynnResult<Self> {
		Ok(Self {
			entries: self.entries.checked_add(other.entries).ok_or_else(overflow)?,
			bytes: self.bytes.checked_add(other.bytes).ok_or_else(overflow)?,
		})
	}
}

/// One immutable delivery and its admitted retained cost.
#[derive(Debug, Clone)]
struct Entry {
	/// Token and payload remain identical until successful confirmation.
	delivery: LiveDelivery,
	/// Retained cost computed before enqueueing.
	cost: Footprint,
}

/// Concrete queue state; never stored in a durable root or executable
/// descriptor.
#[derive(Debug)]
pub(super) struct Queue {
	/// The last accepted whole-delivery watermark.
	confirmed: LiveCursor,
	/// Issued storage is separate from the one pending control slot.
	inflight: Option<Entry>,
	/// Data and at most one unissued control, in commit order.
	pending: VecDeque<Entry>,
	/// Drain admitted deliveries before reporting terminal state.
	terminal: bool,
}

/// Admission is distinct from an empty delivery queue.
pub(super) enum PreparedQueue {
	/// Complete successor, installable without allocation or evaluation.
	Ready(
		/// Complete successor, installable without allocation or evaluation.
		Queue,
	),
	/// A BLOCK subscriber requires capacity before the source commit.
	Blocked,
	/// This batch cannot fit even in an empty queue; publication must pause it.
	Oversized,
}

impl Queue {
	/// Starts after the registration commit, without emitting initial records.
	pub(super) const fn new(confirmed: LiveCursor) -> Self {
		Self {
			confirmed,
			inflight: None,
			pending: VecDeque::new(),
			terminal: false,
		}
	}

	/// Returns the confirmed watermark, independently of an issued delivery.
	pub(super) const fn cursor(&self) -> &LiveCursor {
		&self.confirmed
	}

	/// Repeats an issued payload; moving the next head does not allocate.
	pub(super) fn next(&mut self) -> LiveNext {
		if self.inflight.is_none() {
			self.inflight = self.pending.pop_front();
		}
		match &self.inflight {
			Some(entry) => LiveNext::Delivery(entry.delivery.clone()),
			None if self.terminal => LiveNext::Closed,
			None => LiveNext::Idle,
		}
	}

	/// Validates origin and actual issuance before a durable ACK is prepared.
	pub(super) fn validate_ack(&self, token: &LiveCursor) -> FerrosGrynnResult<bool> {
		token.validate(
			self.confirmed.database_id(),
			self.confirmed.live_id(),
			self.confirmed.version(),
		)?;
		if token.versionstamp() <= self.confirmed.versionstamp() {
			return Ok(false);
		}
		if self.inflight.as_ref().is_none_or(|entry| entry.delivery.token() != token) {
			return Err(FerrosGrynnError::invalid_argument(
				"LIVE ACK was not issued by this consumer",
			));
		}
		Ok(true)
	}

	/// Releases capacity only after the same token was durably confirmed.
	pub(super) fn confirm(&mut self, token: &LiveCursor) -> FerrosGrynnResult<()> {
		if self.validate_ack(token)? {
			self.confirmed = token.clone();
			self.inflight = None;
		}
		Ok(())
	}

	/// Accounts issued and pending payloads; a returned payload is shared by
	/// Arc.
	pub(super) fn footprint(&self) -> FerrosGrynnResult<Footprint> {
		let ring_slots = self
			.pending
			.capacity()
			.saturating_sub(1)
			.max(self.pending.iter().filter(|entry| entry.cost.entries != 0).count());
		let initial = Footprint {
			entries: 0,
			bytes: QUEUE_BYTES
				.checked_add(ring_slots.checked_mul(size_of::<Entry>()).ok_or_else(overflow)?)
				.ok_or_else(overflow)?,
		};
		self.inflight.iter().chain(self.pending.iter()).try_fold(initial, |cost, entry| {
			// Issued and pending controls and one pending slot are prepaid.
			if entry.cost.entries == 0 {
				Ok(cost)
			} else {
				cost.add(entry.cost)
			}
		})
	}

	/// Copies only admitted immutable handles, with a checked scratch ceiling.
	fn copy_for_publication(&self, scratch: usize) -> FerrosGrynnResult<Self> {
		let slots = self.pending.len().checked_add(1).ok_or_else(overflow)?;
		let bytes = slots
			.checked_mul(size_of::<Entry>())
			.and_then(|bytes| bytes.checked_add(size_of::<Self>()))
			.ok_or_else(overflow)?;
		if bytes > scratch {
			return Err(resource("LIVE queue preparation", scratch));
		}
		let mut pending = VecDeque::new();
		pending.try_reserve_exact(slots).map_err(|error| {
			resource("LIVE queue preparation", scratch).with_technical_source(error)
		})?;
		pending.extend(self.pending.iter().cloned());
		Ok(Self {
			confirmed: self.confirmed.clone(),
			inflight: self.inflight.clone(),
			pending,
			terminal: self.terminal,
		})
	}

	/// Stages all evictions/controls before publication; failure leaves self
	/// intact.
	pub(super) fn prepare(
		&self,
		delivery: &LiveDelivery,
		options: &LiveOptions,
		reserved: Footprint,
		scratch: usize,
	) -> FerrosGrynnResult<PreparedQueue> {
		self.prepare_with_head(delivery, options, reserved, scratch, None)
	}

	/// Historical head storage is prepaid separately from the bounded live
	/// tail.
	fn prepare_with_head(
		&self,
		delivery: &LiveDelivery,
		options: &LiveOptions,
		reserved: Footprint,
		scratch: usize,
		historical: Option<u64>,
	) -> FerrosGrynnResult<PreparedQueue> {
		delivery.token().validate(
			self.confirmed.database_id(),
			self.confirmed.live_id(),
			self.confirmed.version(),
		)?;
		if delivery.token().versionstamp() <= self.confirmed.versionstamp() || self.terminal {
			return Ok(PreparedQueue::Ready(self.copy_for_publication(scratch)?));
		}
		let cost = Footprint::delivery(delivery)?;
		let tail = self.pending.back().or(self.inflight.as_ref());
		if tail.is_some_and(|entry| {
			entry.delivery.token().versionstamp() >= delivery.token().versionstamp()
		}) {
			return Err(FerrosGrynnError::invalid_argument(
				"LIVE queue delivery is not in increasing commit order",
			));
		}
		if cost.entries > options.queue_entries()
			|| cost.entries != 0
				&& cost.bytes.checked_add(size_of::<Entry>()).ok_or_else(overflow)?
					> options.queue_bytes()
		{
			return Ok(PreparedQueue::Oversized);
		}
		let mut next = self.copy_for_publication(scratch)?;
		if cost.entries != 0 {
			// Stage the complete data successor before measuring its ring
			// slots. The prepaid control slot cannot be counted as
			// a free data slot.
			next.pending.push_back(Entry {
				delivery: delivery.clone(),
				cost,
			});
		}
		let mut retained = next.footprint()?;
		if let Some(entry) = next.inflight.as_ref().filter(|entry| {
			historical.is_some_and(|cutoff| entry.delivery.token().versionstamp().get() <= cutoff)
				&& entry.cost.entries != 0
		}) {
			retained.bytes = retained.bytes.checked_sub(entry.cost.bytes).ok_or_else(overflow)?;
			retained.entries =
				retained.entries.checked_sub(entry.cost.entries).ok_or_else(overflow)?;
		}
		let retained = retained.add(reserved)?;
		let fits = retained.entries <= options.queue_entries()
			&& retained.bytes <= options.queue_bytes().saturating_add(QUEUE_BYTES);
		if fits || cost.entries == 0 {
			// A fault terminates evaluation and occupies the pending control
			// slot. Preserve an issued control separately; merge
			// only unissued controls.
			if cost.entries == 0
				&& let Some(position) =
					next.pending.iter().position(|entry| entry.cost.entries == 0)
			{
				let previous = &next.pending[position].delivery;
				let merged = merge_control(previous, delivery)?;
				next.pending.truncate(position);
				let cost = Footprint::delivery(&merged)?;
				next.pending.push_back(Entry {
					delivery: merged,
					cost,
				});
				return Ok(PreparedQueue::Ready(next));
			}
			if cost.entries == 0 {
				next.pending.push_back(Entry {
					delivery: delivery.clone(),
					cost,
				});
			}
			return Ok(PreparedQueue::Ready(next));
		}
		match options.overflow() {
			crate::live::LiveOverflowPolicy::Block => Ok(PreparedQueue::Blocked),
			policy => Ok(PreparedQueue::Ready(next.with_overflow(delivery.token(), policy)?)),
		}
	}

	/// Uses prepaid control storage for per-query or session-wide overflow.
	/// Issued data is preserved; the control covers every dropped commit.
	pub(super) fn with_overflow(
		mut self,
		token: &LiveCursor,
		policy: crate::live::LiveOverflowPolicy,
	) -> FerrosGrynnResult<Self> {
		let from = self.pending.front().map_or(Some(token.versionstamp()), |entry| {
			match entry.delivery.payload() {
				LivePayload::Gap(gap)
				| LivePayload::Fault {
					gap,
					..
				} => gap.from,
				LivePayload::Batch(_) | LivePayload::Graph(_) => {
					Some(entry.delivery.token().versionstamp())
				}
			}
		});
		let diagnostic = self.pending.iter().find_map(|entry| match entry.delivery.payload() {
			LivePayload::Fault {
				diagnostic,
				..
			} => Some(diagnostic.clone()),
			_ => None,
		});
		let gap = LiveGap {
			id: token.live_id(),
			reason: if diagnostic.is_some() {
				LiveGapReason::EvaluationFault
			} else {
				LiveGapReason::QueueOverflow
			},
			from,
			through: Some(token.versionstamp()),
		};
		let mut pending = VecDeque::new();
		pending.try_reserve_exact(1).map_err(|error| {
			resource("LIVE reserved control slot", QUEUE_BYTES).with_technical_source(error)
		})?;
		pending.push_back(Entry {
			delivery: LiveDelivery {
				token: token.clone(),
				payload: Arc::new(match diagnostic {
					Some(diagnostic) => LivePayload::Fault {
						diagnostic,
						gap,
					},
					None => LivePayload::Gap(gap),
				}),
			},
			cost: Footprint {
				entries: 0,
				bytes: 512,
			},
		});
		self.pending = pending;
		self.terminal = policy == crate::live::LiveOverflowPolicy::Disconnect;
		Ok(self)
	}
}

/// Coalesces unissued controls and covers every intervening dropped batch.
/// Unknown authoritative bounds remain unknown; issued data is never changed.
fn merge_control(
	previous: &LiveDelivery,
	incoming: &LiveDelivery,
) -> FerrosGrynnResult<LiveDelivery> {
	let gap = |payload: &LivePayload| match payload {
		LivePayload::Gap(gap)
		| LivePayload::Fault {
			gap,
			..
		} => Some(gap.clone()),
		LivePayload::Batch(_) | LivePayload::Graph(_) => None,
	};
	let before = gap(previous.payload())
		.ok_or_else(|| FerrosGrynnError::invalid_argument("LIVE control expected"))?;
	let mut after = gap(incoming.payload())
		.ok_or_else(|| FerrosGrynnError::invalid_argument("LIVE control expected"))?;
	after.from = match (before.from, after.from) {
		(Some(left), Some(right)) => Some(left.min(right)),
		_ => None,
	};
	after.through = match (before.through, after.through) {
		(Some(left), Some(right)) => Some(left.max(right)),
		_ => None,
	};
	let diagnostic = match (incoming.payload(), previous.payload()) {
		(
			LivePayload::Fault {
				diagnostic,
				..
			},
			_,
		)
		| (
			_,
			LivePayload::Fault {
				diagnostic,
				..
			},
		) => Some(diagnostic.clone()),
		_ => None,
	};
	let payload = match diagnostic {
		Some(diagnostic) => {
			after.reason = LiveGapReason::EvaluationFault;
			LivePayload::Fault {
				diagnostic,
				gap: after,
			}
		}
		None => LivePayload::Gap(after),
	};
	Ok(LiveDelivery {
		token: incoming.token.clone(),
		payload: Arc::new(payload),
	})
}

/// Queue overflow remains a typed resource diagnostic.
fn overflow() -> FerrosGrynnError {
	FerrosGrynnError::operation_overflow("LIVE queue accounting")
}

/// Queue resource ceilings are checked before allocating successors.
fn resource(name: &'static str, maximum: usize) -> FerrosGrynnError {
	FerrosGrynnError::traversal_resource_limit_exceeded(name, maximum as u64)
}

#[cfg(test)]
mod tests;
