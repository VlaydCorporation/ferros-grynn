//! One waiter per consumer, with wake callbacks outside publication mutexes.

use std::{ops::Bound, sync::atomic::Ordering, task::Waker};

use super::{Consumer, ConsumerKey, Consumers, FerrosGrynnError, FerrosGrynnResult, overflow};

/// One fixed consumer waiter slot with an explicit cancellation incarnation.
pub(super) struct Waiter {
	/// Distinguishes successive waiter incarnations on the same consumer.
	pub(super) nonce: u64,
	/// Suspends data readiness while an unresolved conflict awaits a user
	/// decision.
	lifecycle_only: bool,
	/// Retains the caller executor wake handle, dropped outside publication
	/// locks.
	waker: Waker,
}

/// Deferred wake scope must be created before every publication guard.
pub(in crate::datastore) struct Notification<'a>(&'a Consumers);

impl Drop for Notification<'_> {
	fn drop(&mut self) {
		self.0.wake_ready();
	}
}

/// Checks the queue predicate under the waiter registration mutex.
fn ready(consumer: &Consumer, lifecycle_only: bool) -> bool {
	consumer.active
		&& if lifecycle_only {
			consumer.queue.terminal
				&& consumer.queue.inflight.is_none()
				&& consumer.queue.pending.is_empty()
		} else {
			consumer.queue.terminal
				|| consumer.queue.inflight.is_some()
				|| !consumer.queue.pending.is_empty()
		}
}

impl Consumers {
	/// Defers wake callbacks until all younger commit/state guards are dropped.
	pub(in crate::datastore) fn notification(&self) -> Notification<'_> {
		Notification(self)
	}

	/// Registers before returning Pending, under the same queue predicate lock.
	pub(in crate::datastore) fn poll_ready(
		&self,
		key: ConsumerKey,
		registration: &mut Option<u64>,
		lifecycle_only: bool,
		waker: &Waker,
	) -> FerrosGrynnResult<bool> {
		// RawWaker clone/drop callbacks are executor code, outside our locks.
		let incoming = waker.clone();
		let mut replaced = None;
		let mut state = self.state.lock();
		if state.closed {
			return Ok(true);
		}
		let consumer = state.consumer(key)?;
		if ready(consumer, lifecycle_only) {
			return Ok(true);
		}
		if let Some(waiter) = &mut consumer.waiter {
			if *registration == Some(waiter.nonce) {
				if !waiter.waker.will_wake(waker) {
					replaced = Some(std::mem::replace(&mut waiter.waker, incoming));
				}
				waiter.lifecycle_only = lifecycle_only;
				drop(state);
				drop(replaced);
				return Ok(false);
			}
			return Err(FerrosGrynnError::invalid_argument(
				"LIVE consumer already has a readiness waiter",
			));
		}
		let nonce = consumer.waiter_nonce.checked_add(1).ok_or_else(overflow)?;
		consumer.waiter_nonce = nonce;
		consumer.waiter = Some(Waiter {
			nonce,
			lifecycle_only,
			waker: incoming,
		});
		*registration = Some(nonce);
		self.waiters.fetch_add(1, Ordering::Relaxed);
		Ok(false)
	}

	/// Dropping a losing select branch unregisters only its own fixed slot.
	pub(in crate::datastore) fn cancel_ready(&self, key: ConsumerKey, registration: u64) {
		let removed = {
			let mut state = self.state.lock();
			let removed = state
				.consumers
				.get_mut(&key.id)
				.filter(|consumer| {
					consumer.key == key
						&& consumer
							.waiter
							.as_ref()
							.is_some_and(|waiter| waiter.nonce == registration)
				})
				.and_then(|consumer| consumer.waiter.take());
			if removed.is_some() {
				self.waiters.fetch_sub(1, Ordering::Relaxed);
			}
			if state.closed && self.waiters.load(Ordering::Relaxed) == 0 {
				state.consumers.clear();
			}
			removed
		};
		drop(removed);
	}

	/// Fixed stack chunks avoid allocations after the WAL publication barrier.
	pub(in crate::datastore) fn wake_ready(&self) {
		if self.waiters.load(Ordering::Relaxed) == 0 {
			return;
		}
		let mut after = None;
		loop {
			let mut batch: [Option<Waker>; 8] = std::array::from_fn(|_| None);
			let mut count = 0;
			let closed;
			{
				let mut state = self.state.lock();
				closed = state.closed;
				let lower = after.map_or(Bound::Unbounded, Bound::Excluded);
				for (id, consumer) in state.consumers.range_mut((lower, Bound::Unbounded)) {
					after = Some(*id);
					if consumer
						.waiter
						.as_ref()
						.is_some_and(|waiter| closed || ready(consumer, waiter.lifecycle_only))
					{
						if let Some(waiter) = consumer.waiter.take() {
							batch[count] = Some(waiter.waker);
							count += 1;
							self.waiters.fetch_sub(1, Ordering::Relaxed);
						}
					}
					if count == batch.len() {
						break;
					}
				}
			}
			for waker in batch.into_iter().flatten() {
				waker.wake();
			}
			if count < 8 {
				if closed {
					self.state.lock().consumers.clear();
				}
				break;
			}
		}
	}
}

#[cfg(test)]
mod tests {
	use std::{
		sync::{Arc, Weak, atomic::AtomicUsize},
		task::Wake,
	};

	use super::*;
	use crate::live::{LiveCursor, LiveOptions};

	struct DropProbe {
		owner: Weak<Consumers>,
		drops: Arc<AtomicUsize>,
	}
	impl Wake for DropProbe {
		fn wake(self: Arc<Self>) {}
	}
	impl Drop for DropProbe {
		fn drop(&mut self) {
			if let Some(owner) = self.owner.upgrade() {
				assert!(owner.state.try_lock().is_some(), "Waker dropped under consumer mutex");
			}
			self.drops.fetch_add(1, Ordering::Relaxed);
		}
	}
	struct Noop;
	impl Wake for Noop {
		fn wake(self: Arc<Self>) {}
	}

	#[test]
	fn readiness_changed_waker_drop_and_shutdown_cancel_cleanup() -> FerrosGrynnResult<()> {
		let owner = Arc::new(Consumers::default());
		let cursor = LiveCursor::new(
			uuid::Uuid::from_u128(1).into(),
			crate::live::LiveId::from_uuid(uuid::Uuid::from_u128(2).into())?,
			crate::live::LiveVersion::default(),
			1.into(),
		)?;
		let key = owner.attach(cursor, LiveOptions::default(), true)?;
		let drops = Arc::new(AtomicUsize::new(0));
		let first = Waker::from(Arc::new(DropProbe {
			owner: Arc::downgrade(&owner),
			drops: Arc::clone(&drops),
		}));
		let mut registration = None;
		assert!(!owner.poll_ready(key, &mut registration, false, &first)?);
		drop(first);
		let second = Waker::from(Arc::new(Noop));
		assert!(!owner.poll_ready(key, &mut registration, false, &second)?);
		assert_eq!(drops.load(Ordering::Relaxed), 1);
		owner.shutdown();
		let registration = registration
			.ok_or_else(|| FerrosGrynnError::invalid_argument("waiter registration absent"))?;
		owner.cancel_ready(key, registration);
		owner.wake_ready();
		assert_eq!(owner.waiters.load(Ordering::Relaxed), 0);
		assert!(owner.state.lock().consumers.is_empty());
		Ok(())
	}
}
