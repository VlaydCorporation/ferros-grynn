//! All-or-nothing queue admission outside the database commit mutex.

use std::{
	collections::BTreeMap,
	mem::size_of,
	sync::Arc,
	time::{Duration, Instant},
};

/// Bounded cancellation-safe readiness over the existing consumer authority.
mod readiness;
use parking_lot::{Condvar, Mutex};

use super::{Footprint, PreparedQueue, Queue, SESSION_BYTES, overflow, resource};
use crate::{
	datastore::DatabaseRoot,
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	live::{
		LiveCursor,
		LiveDelivery,
		LiveId,
		LiveNext,
		LiveOptions,
		LiveOverflowPolicy,
		LiveState,
	},
};

/// Private attachment identity; an old handle cannot detach a new consumer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::datastore) struct ConsumerKey {
	/// Stable query incarnation.
	pub(in crate::datastore) id: LiveId,
	/// Checked local attachment nonce, unrelated to durable commit ordering.
	nonce: u64,
}

/// One attached consumer, activated by its registration publication.
struct Consumer {
	/// At most one selectable future borrows this consumer at a time.
	waiter: Option<readiness::Waiter>,
	/// Checked registration nonce prevents an old future removing a new waiter.
	waiter_nonce: u64,
	/// Complete private attachment identity.
	key: ConsumerKey,
	/// Delivery settings captured from the registered definition.
	options: LiveOptions,
	/// Issued, pending and confirmed whole-commit state.
	queue: Queue,
	/// False while a new registration has not passed its barrier.
	active: bool,
	/// Captured closed state distinguishes replay of a closed incarnation
	/// from an explicit close racing an attached replay.
	closed: bool,
	/// Historical head capacity cannot be consumed by a newer live tail.
	replay: Option<ReplayBank>,
	/// New-incarnation replay admitted before publication, moved to its handle.
	registration_replay: Option<super::Replay>,
	/// Accepted activation image captured before concurrent tail publication.
	registration_graph: Option<Arc<crate::live::LiveGraphBatch>>,
	/// Charge remains until the handle consumes its initial graph image.
	registration_graph_bytes: usize,
	/// Additional capacity held by concurrent optimistic preparations.
	reserved: Footprint,
}

/// Conservative head holdback; released at replay EOF, never while waiting.
#[derive(Clone, Copy)]
struct ReplayBank {
	/// Later live publication has stamps strictly above this handshake.
	cutoff: u64,
	/// One complete historical head may use this prepaid capacity.
	capacity: Footprint,
	/// Private encoded registration history retained until replay finishes.
	retained_bytes: usize,
}

impl Consumer {
	/// Remaining holdback excludes an already-issued historical head.
	fn holdback(queue: &Queue, replay: Option<ReplayBank>) -> FerrosGrynnResult<Footprint> {
		let Some(bank) = replay else {
			return Ok(Footprint::default());
		};
		let issued = queue
			.inflight
			.as_ref()
			.filter(|entry| entry.delivery.token().versionstamp().get() <= bank.cutoff)
			.map_or(Footprint::default(), |entry| {
				if entry.cost.entries == 0 {
					Footprint::default()
				} else {
					entry.cost
				}
			});
		Ok(Footprint {
			bytes: bank
				.capacity
				.bytes
				.checked_sub(issued.bytes)
				.and_then(|bytes| bytes.checked_add(bank.retained_bytes))
				.ok_or_else(overflow)?,
			entries: bank.capacity.entries.checked_sub(issued.entries).ok_or_else(overflow)?,
		})
	}

	/// Counts head holdback and actual queue storage only once.
	fn footprint(&self, queue: &Queue) -> FerrosGrynnResult<Footprint> {
		Self::footprint_with(queue, self.replay, self.registration_graph_bytes)
	}

	/// Measures an admitted successor before changing the attached bank.
	fn footprint_with(
		queue: &Queue,
		replay: Option<ReplayBank>,
		graph_bytes: usize,
	) -> FerrosGrynnResult<Footprint> {
		queue.footprint()?.add(Self::holdback(queue, replay)?)?.add(Footprint {
			entries: 0,
			bytes: graph_bytes,
		})
	}
}

/// Mutable consumer authority; all logical mutations follow commit → state.
#[derive(Default)]
struct State {
	/// Process deliveries first issued; never a durable ordering clock.
	issued: u64,
	/// Changes on attachment, issuance, confirmation and publication.
	revision: u64,
	/// Local attachment counter; never persisted or used as a versionstamp.
	nonce: u64,
	/// One consumer per subscription incarnation.
	consumers: BTreeMap<LiveId, Consumer>,
	/// Session-wide capacity reserved by all unpublished candidates.
	reserved: usize,
	/// Last datastore handle closed the transient delivery service.
	closed: bool,
}

/// Store-owned transient delivery service; no delivery worker or snapshots.
#[derive(Default)]
pub(in crate::datastore) struct Consumers {
	/// Empty waiting surface adds only one cheap atomic read to publication.
	waiters: std::sync::atomic::AtomicUsize,
	/// Capacity predicate and consumer state share one mutex.
	state: Mutex<State>,
	/// ACK, detach, publication and shutdown wake capacity waiters.
	changed: Condvar,
}

/// Preallocated successor for one still-attached consumer.
struct Update {
	/// Nonce prevents a detached consumer being confused with its replacement.
	key: ConsumerKey,
	/// Complete admitted queue state, including explicit overflow controls.
	queue: Queue,
	/// Registration activation is published with the durable definition.
	active: bool,
	/// Explicit close discards replay admission in the same publication.
	closed: bool,
	/// A still-needed historical head shares the attachment's prepaid bank.
	replay: Option<ReplayBank>,
	/// Bounded immutable reader prepared before the durable barrier.
	registration_replay: Option<super::Replay>,
	/// Complete activation snapshot, installed only after the source barrier.
	registration_graph: Option<Arc<crate::live::LiveGraphBatch>>,
	/// Already-admitted immutable handoff ownership.
	registration_graph_bytes: usize,
	/// Only positive capacity differences are reserved before publication.
	reserved: Footprint,
}

/// RAII capacity permits; Drop locks only queue state, never the commit mutex.
pub(in crate::datastore) struct Reservations<'a> {
	/// Concrete store-owned reservation authority.
	owner: &'a Consumers,
	/// Captured attachment/issuance revision rechecked under commit lock.
	revision: u64,
	/// Already-checked successor revision, requiring no arithmetic after WAL.
	next_revision: u64,
	/// Immutable successor queues prepared before any durable publication.
	updates: Vec<Update>,
	/// Admitted positive session capacity difference.
	reserved: usize,
}

impl State {
	/// Observes a new issued token under the same revision authority as queues.
	fn record_issue(&mut self, first: bool, result: &LiveNext, revision: u64) {
		if first && matches!(result, LiveNext::Delivery(_)) {
			self.issued = self.issued.saturating_add(1);
			self.revision = revision;
		}
	}

	/// Includes empty consumer metadata, retained ring capacity and issued
	/// data.
	fn retained(&self) -> FerrosGrynnResult<usize> {
		self.consumers.values().try_fold(0_usize, |bytes, consumer| {
			bytes.checked_add(consumer.footprint(&consumer.queue)?.bytes).ok_or_else(overflow)
		})
	}

	/// No local revision may wrap and invalidate optimistic admission checks.
	fn successor(&self) -> FerrosGrynnResult<u64> {
		self.revision.checked_add(1).ok_or_else(overflow)
	}

	/// Finds the exact attachment without exposing internal storage identities.
	fn consumer(&mut self, key: ConsumerKey) -> FerrosGrynnResult<&mut Consumer> {
		self.consumers
			.get_mut(&key.id)
			.filter(|consumer| consumer.key == key)
			.ok_or_else(|| FerrosGrynnError::invalid_argument("LIVE consumer is detached"))
	}
}

impl Consumers {
	/// Reads bounded gauges under the existing state mutex, without payload
	/// copies.
	pub(in crate::datastore) fn observations(&self) -> FerrosGrynnResult<(u64, u64, u64, u64)> {
		let state = self.state.lock();
		let mut events = 0_usize;
		for consumer in state.consumers.values() {
			events =
				events.checked_add(consumer.queue.footprint()?.entries).ok_or_else(overflow)?;
		}
		Ok((state.consumers.len() as u64, events as u64, state.retained()? as u64, state.issued))
	}

	/// Admits an attachment while the caller holds the database commit mutex.
	/// Pending registrations are not visible until their successful barrier.
	pub(in crate::datastore) fn attach(
		&self,
		cursor: LiveCursor,
		options: LiveOptions,
		active: bool,
	) -> FerrosGrynnResult<ConsumerKey> {
		let replay = (!active && !matches!(options.since(), crate::live::LiveSince::Now))
			.then_some(ReplayBank {
				cutoff: u64::MAX,
				capacity: Footprint {
					bytes: options.queue_bytes(),
					entries: options.queue_entries(),
				},
				retained_bytes: 0,
			});
		self.attach_with_replay(cursor, options, active, false, replay)
	}

	/// Attaches the tail under the same commit guard that captured its cutoff.
	pub(in crate::datastore) fn attach_replay(
		&self,
		cursor: LiveCursor,
		options: LiveOptions,
		cutoff: u64,
		closed: bool,
	) -> FerrosGrynnResult<ConsumerKey> {
		let replay = (cutoff > cursor.versionstamp().get()).then_some(ReplayBank {
			cutoff,
			retained_bytes: 0,
			capacity: Footprint {
				bytes: options.queue_bytes(),
				entries: options.queue_entries(),
			},
		});
		self.attach_with_replay(cursor, options, true, closed, replay)
	}

	/// Admits head holdback before constructing a consumer map entry.
	fn attach_with_replay(
		&self,
		cursor: LiveCursor,
		options: LiveOptions,
		active: bool,
		closed: bool,
		replay: Option<ReplayBank>,
	) -> FerrosGrynnResult<ConsumerKey> {
		let mut state = self.state.lock();
		if state.closed || state.consumers.contains_key(&cursor.live_id()) {
			return Err(FerrosGrynnError::invalid_argument(
				"LIVE consumer is closed or already attached",
			));
		}
		let revision = state.successor()?;
		let nonce = state.nonce.checked_add(1).ok_or_else(overflow)?;
		let mut queue = Queue::new(cursor);
		queue.terminal = closed;
		let cost = queue
			.footprint()?
			.bytes
			.checked_add(replay.map_or(0, |bank| bank.capacity.bytes))
			.ok_or_else(overflow)?;
		let bytes = state
			.retained()?
			.checked_add(state.reserved)
			.and_then(|bytes| bytes.checked_add(cost))
			.ok_or_else(overflow)?;
		if bytes > SESSION_BYTES {
			return Err(resource("LIVE session retained bytes", SESSION_BYTES));
		}
		let key = ConsumerKey {
			id: queue.cursor().live_id(),
			nonce,
		};
		state.consumers.insert(key.id, Consumer {
			waiter: None,
			waiter_nonce: 0,
			key,
			options,
			queue,
			active,
			closed,
			replay,
			registration_replay: None,
			registration_graph: None,
			registration_graph_bytes: 0,
			reserved: Footprint::default(),
		});
		state.nonce = nonce;
		state.revision = revision;
		Ok(key)
	}

	/// Transfers an already-admitted reader after activation without allocating
	/// or changing queue capacity; later tail commits keep the fixed cutoff.
	pub(in crate::datastore) fn registration(
		&self,
		key: ConsumerKey,
	) -> FerrosGrynnResult<(
		LiveCursor,
		Option<super::Replay>,
		Option<Arc<crate::live::LiveGraphBatch>>,
	)> {
		let mut state = self.state.lock();
		let consumer = state.consumer(key)?;
		if !consumer.active {
			return Err(FerrosGrynnError::invalid_argument("LIVE registration is not active"));
		}
		Ok((
			consumer.queue.cursor().clone(),
			consumer.registration_replay.take(),
			consumer.registration_graph.take(),
		))
	}

	/// Releases prepaid handoff storage after the SDK takes ownership.
	pub(in crate::datastore) fn release_registration_graph(&self, key: ConsumerKey) {
		let mut state = self.state.lock();
		if let Some(consumer) =
			state.consumers.get_mut(&key.id).filter(|consumer| consumer.key == key)
		{
			consumer.registration_graph_bytes = 0;
			if let Some(revision) = state.revision.checked_add(1) {
				state.revision = revision;
			} else {
				state.closed = true;
			}
			self.changed.notify_all();
		}
	}

	/// Detaches only this handle; descriptors and durable ACK remain intact.
	pub(in crate::datastore) fn detach(&self, key: ConsumerKey) {
		let mut state = self.state.lock();
		if state.consumers.get(&key.id).is_some_and(|consumer| consumer.key == key) {
			state.consumers.remove(&key.id);
			if let Some(revision) = state.revision.checked_add(1) {
				state.revision = revision;
			} else {
				state.closed = true;
			}
		}
		self.changed.notify_all();
	}

	/// Stops the transient service; existing owned payloads remain usable.
	pub(in crate::datastore) fn shutdown(&self) {
		let mut state = self.state.lock();
		state.closed = true;
		// Wakers are moved and invoked only after the caller's commit guard.
		if self.waiters.load(std::sync::atomic::Ordering::Relaxed) == 0 {
			state.consumers.clear();
		}
		self.changed.notify_all();
	}

	/// Nonblocking issuance while the caller holds the commit mutex.
	pub(in crate::datastore) fn next(&self, key: ConsumerKey) -> FerrosGrynnResult<LiveNext> {
		let mut state = self.state.lock();
		if state.closed {
			return Ok(LiveNext::Closed);
		}
		let revision = state.successor()?;
		let consumer = state.consumer(key)?;
		let changes = consumer.active
			&& consumer.replay.is_none()
			&& consumer.queue.inflight.is_none()
			&& !consumer.queue.pending.is_empty();
		let result = if consumer.replay.is_some_and(|bank| bank.capacity.bytes != 0) {
			consumer
				.queue
				.inflight
				.as_ref()
				.map_or(LiveNext::Idle, |entry| LiveNext::Delivery(entry.delivery.clone()))
		} else if consumer.active {
			consumer.queue.next()
		} else {
			LiveNext::Idle
		};
		if changes {
			state.revision = revision;
		}
		state.record_issue(changes, &result, revision);
		Ok(result)
	}

	/// Installs one decoded historical head, preserving the newer pending tail.
	/// The caller holds commit; decoding and allocation have already finished.
	pub(in crate::datastore) fn issue_replay(
		&self,
		key: ConsumerKey,
		delivery: LiveDelivery,
	) -> FerrosGrynnResult<LiveNext> {
		let mut state = self.state.lock();
		if state.closed {
			return Ok(LiveNext::Closed);
		}
		let revision = state.successor()?;
		let consumer = state.consumer(key)?;
		let Some(bank) = consumer.replay else {
			let first = consumer.queue.inflight.is_none();
			let result = consumer.queue.next();
			state.record_issue(first, &result, revision);
			return Ok(result);
		};
		if bank.capacity.bytes == 0 {
			return Ok(LiveNext::Closed);
		}
		if let Some(entry) = &consumer.queue.inflight {
			return Ok(LiveNext::Delivery(entry.delivery.clone()));
		}
		delivery.token().validate(
			consumer.queue.cursor().database_id(),
			key.id,
			consumer.queue.cursor().version(),
		)?;
		if delivery.token().versionstamp() <= consumer.queue.cursor().versionstamp()
			|| delivery.token().versionstamp().get() > bank.cutoff
		{
			return Err(FerrosGrynnError::invalid_argument(
				"LIVE replay head is outside its handshake",
			));
		}
		let cost = Footprint::delivery(&delivery)?;
		if !Footprint::fits_empty(delivery.payload(), &consumer.options)? {
			return Err(resource("LIVE historical whole commit", consumer.options.queue_bytes()));
		}
		consumer.queue.inflight = Some(super::Entry {
			delivery: delivery.clone(),
			cost,
		});
		state.revision = revision;
		let result = LiveNext::Delivery(delivery);
		state.record_issue(true, &result, revision);
		Ok(result)
	}

	/// Releases head holdback and pins only after every historical token is
	/// ACKed.
	pub(in crate::datastore) fn finish_replay(
		&self,
		key: ConsumerKey,
	) -> FerrosGrynnResult<LiveNext> {
		let mut state = self.state.lock();
		if state.closed {
			return Ok(LiveNext::Closed);
		}
		let revision = state.successor()?;
		let consumer = state.consumer(key)?;
		if consumer.queue.inflight.is_some() {
			return Err(FerrosGrynnError::invalid_argument("LIVE replay has an unconfirmed head"));
		}
		consumer.replay = None;
		let result = consumer.queue.next();
		state.revision = revision;
		state.record_issue(true, &result, revision);
		self.changed.notify_all();
		Ok(result)
	}

	/// Checks issuance without releasing capacity or advancing the watermark.
	pub(in crate::datastore) fn validate_ack(
		&self,
		key: ConsumerKey,
		token: &LiveCursor,
	) -> FerrosGrynnResult<bool> {
		let mut state = self.state.lock();
		if state.closed {
			return Err(FerrosGrynnError::invalid_argument("LIVE consumer is closed"));
		}
		state.consumer(key)?.queue.validate_ack(token)
	}

	/// Normalizes repeated historical confirmations to one checked watermark.
	pub(in crate::datastore) fn ack_evidence(
		&self,
		key: ConsumerKey,
		token: &LiveCursor,
	) -> FerrosGrynnResult<LiveCursor> {
		let mut state = self.state.lock();
		if state.closed {
			return Err(FerrosGrynnError::invalid_argument("LIVE consumer is closed"));
		}
		let queue = &state.consumer(key)?.queue;
		Ok(if queue.validate_ack(token)? {
			token.clone()
		} else {
			queue.cursor().clone()
		})
	}

	/// Reserves the entire fanout atomically; no partial permits survive waits.
	/// Cancellation is polled in short slices because an atomic flag has no
	/// wake.
	pub(in crate::datastore) fn reserve<'a>(
		&'a self,
		root: &DatabaseRoot,
		started: Instant,
		deadline: Option<Instant>,
		check: &impl Fn() -> FerrosGrynnResult<()>,
	) -> FerrosGrynnResult<Reservations<'a>> {
		loop {
			check()?;
			let mut state = self.state.lock();
			let next_revision = state.successor()?;
			let count = state.consumers.len();
			let metadata = count.checked_mul(size_of::<Update>()).ok_or_else(overflow)?;
			if metadata > SESSION_BYTES {
				return Err(resource("LIVE queue preparation", SESSION_BYTES));
			}
			let mut updates = Vec::new();
			updates.try_reserve_exact(count).map_err(|error| {
				resource("LIVE queue preparation", SESSION_BYTES).with_technical_source(error)
			})?;
			let mut scratch = metadata;
			let mut reserved = 0_usize;
			let mut wait_until = deadline;
			let mut blocked = false;
			for consumer in state.consumers.values() {
				if state.closed {
					break;
				}
				let Some(definition) = root.live_catalog().queries.get(&consumer.key.id) else {
					continue;
				};
				let mut acknowledged = None;
				if consumer.active
					&& definition.acknowledged > consumer.queue.cursor().versionstamp().get()
				{
					let mut queue = consumer.queue.copy_for_publication(
						SESSION_BYTES.checked_sub(scratch).ok_or_else(overflow)?,
					)?;
					scratch = scratch
						.checked_add(
							queue
								.pending
								.capacity()
								.checked_mul(size_of::<super::Entry>())
								.ok_or_else(overflow)?,
						)
						.ok_or_else(overflow)?;
					let token = LiveCursor::new(
						root.database_uuid.into(),
						definition.id,
						definition.version,
						definition.acknowledged.into(),
					)?;
					queue.confirm(&token)?;
					acknowledged = Some(queue);
				}
				let before_queue = acknowledged.as_ref().unwrap_or(&consumer.queue);
				let closing = definition.state == LiveState::Closed && !consumer.closed;
				let mut registration_replay = None;
				let registration_graph = if !consumer.active && !closing {
					root.live_catalog().graph_checkpoints.get(&consumer.key.id).cloned()
				} else {
					None
				};
				let graph_bytes = if let Some(graph) = &registration_graph {
					Footprint::payload(&crate::live::LivePayload::Graph(Arc::clone(graph)))?.bytes
				} else {
					consumer.registration_graph_bytes
				};
				let replay = if closing {
					// An externally closed idle handle still owns its private
					// registration buffers. Keep only their charge until that
					// handle polls or detaches; the queue no longer needs a
					// head.
					consumer
						.replay
						.filter(|bank| {
							bank.retained_bytes != 0 && consumer.registration_replay.is_none()
						})
						.map(|bank| ReplayBank {
							capacity: Footprint::default(),
							..bank
						})
				} else if !consumer.active && consumer.replay.is_some() {
					let cursor = LiveCursor::new(
						root.database_uuid.into(),
						definition.id,
						definition.version,
						definition.acknowledged.into(),
					)?;
					let (prepared, retained_bytes) =
						super::Replay::registration(root, &cursor, &consumer.options)?;
					prepared.has_work().then(|| {
						registration_replay = Some(prepared);
						ReplayBank {
							cutoff: root.commit_ts,
							capacity: Footprint {
								bytes: consumer.options.queue_bytes(),
								entries: consumer.options.queue_entries(),
							},
							retained_bytes,
						}
					})
				} else {
					consumer.replay
				};
				let queue = if closing {
					let mut queue = Queue::new(LiveCursor::new(
						root.database_uuid.into(),
						definition.id,
						definition.version,
						definition.acknowledged.into(),
					)?);
					queue.terminal = true;
					queue
				} else if !consumer.active {
					Queue::new(LiveCursor::new(
						root.database_uuid.into(),
						definition.id,
						definition.version,
						definition.acknowledged.into(),
					)?)
				} else if let Some((_, payload)) =
					root.live_catalog().prepared.iter().find(|(id, _)| *id == consumer.key.id)
				{
					let delivery = LiveDelivery {
						token: LiveCursor::new(
							root.database_uuid.into(),
							definition.id,
							definition.version,
							root.commit_ts.into(),
						)?,
						payload: Arc::clone(payload),
					};
					let available = SESSION_BYTES.checked_sub(scratch).ok_or_else(overflow)?;
					let prepared = if let Some(bank) = consumer.replay {
						before_queue.prepare_with_head(
							&delivery,
							&consumer.options,
							consumer.reserved,
							available,
							Some(bank.cutoff),
						)?
					} else {
						before_queue.prepare(
							&delivery,
							&consumer.options,
							consumer.reserved,
							available,
						)?
					};
					match prepared {
						PreparedQueue::Ready(queue) => queue,
						PreparedQueue::Blocked => {
							let until = started
								.checked_add(consumer.options.block_timeout())
								.ok_or_else(overflow)?;
							wait_until =
								Some(wait_until.map_or(until, |current| current.min(until)));
							blocked = true;
							continue;
						}
						PreparedQueue::Oversized => {
							return Err(resource(
								"LIVE whole commit exceeds an empty queue",
								consumer.options.queue_bytes(),
							));
						}
					}
				} else if let Some(queue) = acknowledged {
					queue
				} else {
					continue;
				};
				scratch = scratch
					.checked_add(
						queue
							.pending
							.capacity()
							.checked_mul(size_of::<super::Entry>())
							.ok_or_else(overflow)?,
					)
					.ok_or_else(overflow)?;
				if scratch > SESSION_BYTES {
					return Err(resource("LIVE queue preparation", SESSION_BYTES));
				}
				let before = consumer.footprint(&consumer.queue)?;
				let after = Consumer::footprint_with(&queue, replay, graph_bytes)?;
				let additional = Footprint {
					bytes: after.bytes.saturating_sub(before.bytes),
					entries: after.entries.saturating_sub(before.entries),
				};
				reserved = reserved.checked_add(additional.bytes).ok_or_else(overflow)?;
				consumer.reserved.bytes.checked_add(additional.bytes).ok_or_else(overflow)?;
				consumer.reserved.entries.checked_add(additional.entries).ok_or_else(overflow)?;
				updates.push(Update {
					key: consumer.key,
					queue,
					active: true,
					closed: consumer.closed || closing,
					replay,
					registration_replay,
					registration_graph,
					registration_graph_bytes: graph_bytes,
					reserved: additional,
				});
			}
			let claimed = state.reserved.checked_add(reserved).ok_or_else(overflow)?;
			if state.retained()?.checked_add(claimed).ok_or_else(overflow)? > SESSION_BYTES
				&& updates.iter().any(|update| {
					update.registration_replay.is_some() || update.registration_graph.is_some()
				}) {
				return Err(resource("LIVE registration retained history", SESSION_BYTES));
			}
			if !blocked
				&& state.retained()?.checked_add(claimed).ok_or_else(overflow)? > SESSION_BYTES
			{
				// Global pressure follows each consumer's policy too. Prepaid
				// controls keep issued deliveries and cannot grow retained
				// bytes.
				for update in &mut updates {
					if update.reserved.bytes == 0 {
						continue;
					}
					let consumer = state.consumers.get(&update.key.id).ok_or_else(|| {
						FerrosGrynnError::invalid_argument(
							"LIVE consumer detached during admission",
						)
					})?;
					if consumer.options.overflow() == LiveOverflowPolicy::Block {
						let until = started
							.checked_add(consumer.options.block_timeout())
							.ok_or_else(overflow)?;
						wait_until = Some(wait_until.map_or(until, |current| current.min(until)));
						blocked = true;
						continue;
					}
					let token = LiveCursor::new(
						root.database_uuid.into(),
						update.key.id,
						consumer.queue.cursor().version(),
						root.commit_ts.into(),
					)?;
					let queue = std::mem::replace(&mut update.queue, Queue::new(token.clone()));
					update.queue = queue.with_overflow(&token, consumer.options.overflow())?;
					reserved = reserved.checked_sub(update.reserved.bytes).ok_or_else(overflow)?;
					update.reserved = Footprint::default();
				}
			}
			if blocked {
				// Dropping all successor queues frees scratch before releasing
				// the predicate mutex in the atomic Condvar wait.
				drop(updates);
				let remaining = wait_until
					.and_then(|until| until.checked_duration_since(Instant::now()))
					.filter(|remaining| !remaining.is_zero())
					.ok_or_else(|| FerrosGrynnError::query_timed_out(0))?;
				self.changed.wait_for(&mut state, remaining.min(Duration::from_millis(25)));
				continue;
			}
			let claimed = state.reserved.checked_add(reserved).ok_or_else(overflow)?;
			if state.retained()?.checked_add(claimed).ok_or_else(overflow)? > SESSION_BYTES {
				return Err(resource("LIVE session retained bytes", SESSION_BYTES));
			}
			for update in &updates {
				let consumer = state.consumer(update.key)?;
				consumer.reserved.bytes += update.reserved.bytes;
				consumer.reserved.entries += update.reserved.entries;
			}
			state.reserved = claimed;
			return Ok(Reservations {
				owner: self,
				revision: state.revision,
				next_revision,
				updates,
				reserved,
			});
		}
	}
}

impl Reservations<'_> {
	/// Called under the commit mutex immediately before the existing barrier.
	pub(in crate::datastore) fn valid(&self) -> bool {
		let state = self.owner.state.lock();
		!state.closed && state.revision == self.revision
	}

	/// Installs preallocated successors while the same commit mutex is held.
	/// Logical consumer changes cannot race between valid() and this handoff.
	pub(in crate::datastore) fn publish(mut self) {
		let mut state = self.owner.state.lock();
		for update in self.updates.drain(..) {
			if let Some(consumer) = state
				.consumers
				.get_mut(&update.key.id)
				.filter(|consumer| consumer.key == update.key)
			{
				if !consumer.active || update.closed {
					consumer.registration_replay = update.registration_replay;
					consumer.registration_graph = update.registration_graph;
				}
				consumer.registration_graph_bytes = update.registration_graph_bytes;
				consumer.queue = update.queue;
				consumer.active = update.active;
				consumer.closed = update.closed;
				consumer.replay = update.replay;
				consumer.reserved.bytes -= update.reserved.bytes;
				consumer.reserved.entries -= update.reserved.entries;
			}
		}
		state.reserved -= self.reserved;
		self.reserved = 0;
		state.revision = self.next_revision;
		self.owner.changed.notify_all();
	}
}

impl Drop for Reservations<'_> {
	fn drop(&mut self) {
		if self.reserved == 0 && self.updates.is_empty() {
			return;
		}
		let mut state = self.owner.state.lock();
		for update in &self.updates {
			if let Some(consumer) = state
				.consumers
				.get_mut(&update.key.id)
				.filter(|consumer| consumer.key == update.key)
			{
				consumer.reserved.bytes -= update.reserved.bytes;
				consumer.reserved.entries -= update.reserved.entries;
			}
		}
		state.reserved -= self.reserved;
		self.owner.changed.notify_all();
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::{
		datastore::{ClassSchema, Datastore},
		graph::AtomKind,
		live::{LiveBatch, LiveEvent, LiveIdentity, LivePayload, LiveQuery, LiveSource},
		values::{Bytes, DateTime, Value},
	};

	/// First fallback issuance changes revision; replay repeats preserve one
	/// token.
	#[test]
	fn live_queue_replay_fallback_counts_first_issue_and_invalidates_reservation()
	-> FerrosGrynnResult<()> {
		let store = Datastore::in_memory();
		let mut setup = store.begin();
		let class = setup.define_class(ClassSchema::new("item", AtomKind::Node, [])?)?;
		let id = setup
			.register_live(LiveQuery::new(LiveSource::Class(class)), LiveOptions::default())?;
		let stamp = setup.commit()?.versionstamp();
		let mut root = store.inner.current_root().as_ref().clone();
		root.commit_ts += 1;
		let payload = Arc::new(LivePayload::Gap(crate::live::LiveGap {
			id,
			reason: crate::live::LiveGapReason::QueueOverflow,
			from: Some(root.commit_ts.into()),
			through: Some(root.commit_ts.into()),
		}));
		Arc::make_mut(root.live_catalog_mut()).prepared = Arc::from([(id, Arc::clone(&payload))]);
		let consumers = Consumers::default();
		let cursor = LiveCursor::new(
			root.database_uuid.into(),
			id,
			crate::live::LiveVersion::default(),
			stamp,
		)?;
		let key = consumers.attach(cursor, LiveOptions::default(), true)?;
		consumers.reserve(&root, Instant::now(), None, &|| Ok(()))?.publish();
		let mut successor = root.clone();
		successor.commit_ts += 1;
		Arc::make_mut(successor.live_catalog_mut()).prepared = Arc::from([(
			id,
			Arc::new(LivePayload::Gap(crate::live::LiveGap {
				id,
				reason: crate::live::LiveGapReason::QueueOverflow,
				from: Some(successor.commit_ts.into()),
				through: Some(successor.commit_ts.into()),
			})),
		)]);
		let stale = consumers.reserve(&successor, Instant::now(), None, &|| Ok(()))?;
		assert!(stale.valid());
		let delivery = LiveDelivery {
			token: LiveCursor::new(
				root.database_uuid.into(),
				id,
				crate::live::LiveVersion::default(),
				root.commit_ts.into(),
			)?,
			payload,
		};
		let first = consumers.issue_replay(key, delivery)?;
		assert!(matches!(first, LiveNext::Delivery(_)));
		assert!(!stale.valid(), "first fallback issuance invalidates queue successor copies");
		assert_eq!(consumers.observations()?.3, 1);
		assert!(matches!(consumers.next(key)?, LiveNext::Delivery(_)));
		assert_eq!(consumers.observations()?.3, 1, "repeat is not a new issued token");
		Ok(())
	}

	/// External CLOSE cannot release private encoded history still owned by an
	/// idle SDK handle; polling the terminal state releases that ownership.
	#[test]
	fn live_queue_external_close_keeps_registration_reader_charge() -> FerrosGrynnResult<()> {
		use crate::{
			changefeed::{ChangefeedConfig, ChangefeedSource},
			live::{LiveNext, LiveSince},
			values::PropertyMap,
		};
		let store = Datastore::in_memory();
		let mut setup = store.begin();
		let class = setup.define_class(ClassSchema::new("item", AtomKind::Node, [])?)?;
		setup.configure_changefeed(
			ChangefeedSource::Class(class),
			ChangefeedConfig::new("P1D".parse()?)?,
		)?;
		let floor = setup.commit()?.versionstamp();
		let mut insert = store.begin();
		insert.create_atom("item:a".parse()?, PropertyMap::new())?;
		insert.commit()?;
		let mut subscription = store.subscribe_live(
			LiveQuery::new(LiveSource::Class(class)),
			LiveOptions::default().with_since(LiveSince::Versionstamp(floor)),
		)?;
		let id = subscription.id();
		let key = store.inner.live_consumers.state.lock().consumers[&id].key;
		let decoded = {
			let root = store.inner.current_root();
			let mut replay = super::super::Replay::new(
				&root,
				subscription.cursor(),
				&LiveOptions::default(),
				None,
			)?;
			replay.next(&store.inner, subscription.cursor())?.ok_or_else(|| {
				FerrosGrynnError::invalid_argument("racing replay fixture missing")
			})?
		};
		let retained = store.inner.live_consumers.state.lock().consumers[&id]
			.replay
			.ok_or_else(|| FerrosGrynnError::invalid_argument("registration bank missing"))?
			.retained_bytes;
		assert!(retained > 0);
		let mut close = store.begin();
		close.close_live(id)?;
		close.commit()?;
		{
			let state = store.inner.live_consumers.state.lock();
			let bank = state.consumers[&id].replay.ok_or_else(|| {
				FerrosGrynnError::invalid_argument("idle reader charge disappeared")
			})?;
			assert_eq!(bank.retained_bytes, retained);
			assert_eq!(bank.capacity.bytes, 0);
			assert!(state.retained()? >= retained);
		}
		{
			let _commit = store.inner.commit.lock();
			assert!(matches!(
				store.inner.live_consumers.issue_replay(key, decoded)?,
				LiveNext::Closed
			));
		}
		assert!(matches!(subscription.next()?, LiveNext::Closed));
		assert!(!store.inner.live_consumers.state.lock().consumers.contains_key(&id));
		Ok(())
	}

	/// Shutdown racing unlocked historical decoding reports a terminal state.
	#[test]
	fn live_queue_shutdown_during_replay_is_closed() -> FerrosGrynnResult<()> {
		let store = Datastore::in_memory();
		let mut setup = store.begin();
		let class = setup.define_class(ClassSchema::new("item", AtomKind::Node, [])?)?;
		let id = setup
			.register_live(LiveQuery::new(LiveSource::Class(class)), LiveOptions::default())?;
		setup.commit()?;
		let cursor = store.begin().live_info(id)?.cursor().clone();
		let consumers = Consumers::default();
		let key = consumers.attach_replay(
			cursor.clone(),
			LiveOptions::default(),
			cursor.versionstamp().get() + 1,
			false,
		)?;
		let delivery = LiveDelivery {
			token: LiveCursor::new(
				cursor.database_id(),
				id,
				cursor.version(),
				(cursor.versionstamp().get() + 1).into(),
			)?,
			payload: Arc::new(LivePayload::Gap(crate::live::LiveGap {
				id,
				reason: crate::live::LiveGapReason::VolatileHistoryLost,
				from: None,
				through: None,
			})),
		};
		consumers.shutdown();
		assert!(matches!(consumers.issue_replay(key, delivery)?, LiveNext::Closed));
		assert!(matches!(consumers.finish_replay(key)?, LiveNext::Closed));
		Ok(())
	}

	/// Five legal data batches exceed only the aggregate retained ceiling.
	#[test]
	fn live_queue_session_pressure_applies_gap_disconnect_and_block() -> FerrosGrynnResult<()> {
		let store = Datastore::in_memory();
		let mut setup = store.begin();
		let class = setup.define_class(ClassSchema::new("item", AtomKind::Node, [])?)?;
		let ids = (0..5)
			.map(|_| {
				setup
					.register_live(LiveQuery::new(LiveSource::Class(class)), LiveOptions::default())
			})
			.collect::<FerrosGrynnResult<Vec<_>>>()?;
		setup.commit()?;
		let mut root = (*store.inner.current_root()).clone();
		root.commit_ts += 1;
		let stamp = root.commit_ts;
		let mut prepared = Vec::new();
		for id in &ids {
			let batch = LiveBatch {
				id: *id,
				version: crate::live::LiveVersion::default(),
				stamp: stamp.into(),
				committed_at: DateTime::from(
					jiff::Timestamp::from_nanosecond(0)?.to_zoned(jiff::tz::TimeZone::UTC),
				),
				events: vec![LiveEvent {
					output: None,
					ordinal: 0,
					identity: LiveIdentity::Aggregate(Vec::new()),
					before: None,
					after: Some(Value::Bytes(Bytes::from(vec![0; 54 * 1024 * 1024]))),
				}],
				bindings: Vec::new(),
			};
			prepared.push((*id, Arc::new(LivePayload::Batch(batch))));
		}
		Arc::make_mut(root.live_catalog_mut()).prepared = prepared.into();
		let consumers = Consumers::default();
		let mut keys = Vec::new();
		for (ordinal, id) in ids.iter().enumerate() {
			let policy = if ordinal == 0 {
				LiveOverflowPolicy::Disconnect
			} else {
				LiveOverflowPolicy::Gap
			};
			keys.push(consumers.attach(
				LiveCursor::new(
					root.database_uuid.into(),
					*id,
					crate::live::LiveVersion::default(),
					(stamp - 1).into(),
				)?,
				LiveOptions::default().with_overflow(policy),
				true,
			)?);
		}
		let admitted = consumers.reserve(&root, Instant::now(), None, &|| Ok(()))?;
		assert!(admitted.valid());
		admitted.publish();
		assert!(consumers.state.lock().retained()? <= SESSION_BYTES);
		for key in keys {
			let LiveNext::Delivery(delivery) = consumers.next(key)? else {
				return Err(FerrosGrynnError::invalid_argument("global pressure lost control"));
			};
			assert!(matches!(delivery.payload(), LivePayload::Gap(_)));
		}
		let block = Consumers::default();
		for id in ids {
			block.attach(
				LiveCursor::new(
					root.database_uuid.into(),
					id,
					crate::live::LiveVersion::default(),
					(stamp - 1).into(),
				)?,
				LiveOptions::default().with_overflow(LiveOverflowPolicy::Block),
				true,
			)?;
		}
		assert!(
			block
				.reserve(
					&root,
					Instant::now(),
					Instant::now().checked_add(Duration::from_millis(10)),
					&|| Ok(())
				)
				.is_err()
		);
		assert_eq!(block.state.lock().reserved, 0);
		assert!(
			block
				.state
				.lock()
				.consumers
				.values()
				.all(|consumer| consumer.reserved == Footprint::default())
		);
		Ok(())
	}
}
