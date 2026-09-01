//! Bounded synchronous-pull prefetch pipeline prototype.

use std::{
	num::NonZeroUsize,
	sync::{
		Arc,
		atomic::{AtomicBool, Ordering},
		mpsc::{self, Receiver},
	},
	thread::{self, JoinHandle},
};

use ferros_grynn::gql_status::{FerrosGrynnError, FerrosGrynnResult};

/// Ordered result produced by the prefetch worker.
struct PrefetchMessage<Extent> {
	/// Original input position.
	ordinal: usize,
	/// Loaded extent or its first terminal error.
	result: FerrosGrynnResult<Extent>,
}

/// Background loader with a bounded channel and synchronous ordered pull API.
pub(super) struct PrototypePrefetchCursor<Extent> {
	/// Receiver dropped before joining so a blocked worker send is cancelled.
	receiver: Option<Receiver<PrefetchMessage<Extent>>>,
	/// Cooperative stop flag checked before each new load.
	cancelled: Arc<AtomicBool>,
	/// Owned worker joined on finish, error, or early drop.
	worker: Option<JoinHandle<()>>,
	/// Next ordinal accepted by the consumer.
	next_ordinal: usize,
}

impl<Extent> PrototypePrefetchCursor<Extent>
where
	Extent: Send + 'static,
{
	/// Starts a bounded worker. Depth zero is intentionally handled by the
	/// caller's ordinary synchronous loop, so measured depth zero has no
	/// thread.
	pub(super) fn new<Inputs, Input, Loader>(
		inputs: Inputs,
		depth: NonZeroUsize,
		mut loader: Loader,
	) -> Self
	where
		Inputs: IntoIterator<Item = Input>,
		Inputs::IntoIter: Send + 'static,
		Input: Send + 'static,
		Loader: FnMut(Input) -> FerrosGrynnResult<Extent> + Send + 'static,
	{
		// One extent may be in the worker and one in the consumer; the channel
		// stores only the remaining requested look-ahead depth.
		let (sender, receiver) = mpsc::sync_channel(depth.get().saturating_sub(1));
		let cancelled = Arc::new(AtomicBool::new(false));
		let worker_cancelled = Arc::clone(&cancelled);
		let inputs = inputs.into_iter();
		let worker = thread::spawn(move || {
			for (ordinal, input) in inputs.enumerate() {
				if worker_cancelled.load(Ordering::Acquire) {
					break;
				}
				let result = loader(input);
				let terminal = result.is_err();
				if sender
					.send(PrefetchMessage {
						ordinal,
						result,
					})
					.is_err() || terminal
				{
					break;
				}
			}
		});
		Self {
			receiver: Some(receiver),
			cancelled,
			worker: Some(worker),
			next_ordinal: 0,
		}
	}

	/// Pulls the next extent in input order.
	pub(super) fn next(&mut self) -> Option<FerrosGrynnResult<Extent>> {
		let received = self.receiver.as_ref()?.recv();
		match received {
			Ok(message) if message.ordinal == self.next_ordinal => {
				self.next_ordinal += 1;
				if message.result.is_err() {
					let _ = self.shutdown();
				}
				Some(message.result)
			}
			Ok(message) => {
				let error = FerrosGrynnError::storage_io_failure(format!(
					"prefetch extent {} arrived while {} was expected",
					message.ordinal, self.next_ordinal
				));
				let _ = self.shutdown();
				Some(Err(error))
			}
			Err(_) => match self.shutdown() {
				Ok(()) => None,
				Err(error) => Some(Err(error)),
			},
		}
	}

	/// Cancels pending work, drops queued extents, and joins the worker.
	///
	/// # Errors
	///
	/// Returns an internal storage error if the worker panicked.
	pub(super) fn finish(mut self) -> FerrosGrynnResult<()> {
		self.shutdown()
	}

	/// Implements the cancellation order required by native I/O lifetimes.
	fn shutdown(&mut self) -> FerrosGrynnResult<()> {
		self.cancelled.store(true, Ordering::Release);
		drop(self.receiver.take());
		if let Some(worker) = self.worker.take() {
			worker.join().map_err(|payload| {
				drop(payload);
				FerrosGrynnError::storage_io_failure("prefetch worker panicked during join")
			})?;
		}
		Ok(())
	}
}

impl<Extent> Drop for PrototypePrefetchCursor<Extent> {
	/// Early cursor close cancels and joins before owned extent buffers drop.
	fn drop(&mut self) {
		self.cancelled.store(true, Ordering::Release);
		drop(self.receiver.take());
		if let Some(worker) = self.worker.take() {
			drop(worker.join());
		}
	}
}

#[cfg(test)]
#[allow(
	clippy::panic_in_result_fn,
	reason = "fallible test setup returns Result while assertions report contract failures"
)]
mod tests {
	use std::{
		error::Error,
		num::NonZeroUsize,
		sync::{
			Arc,
			atomic::{AtomicBool, AtomicUsize, Ordering},
		},
		thread,
		time::{Duration, Instant},
	};

	use ferros_grynn::gql_status::FerrosGrynnError;

	use super::PrototypePrefetchCursor;

	/// Owned reservation token used to prove queued and failed sends are
	/// dropped.
	struct ReservationToken {
		/// Shared cleanup count.
		dropped: Arc<AtomicUsize>,
		/// Original sequence number.
		ordinal: usize,
	}

	impl Drop for ReservationToken {
		fn drop(&mut self) {
			self.dropped.fetch_add(1, Ordering::Relaxed);
		}
	}

	#[test]
	fn pipeline_preserves_input_order() -> Result<(), Box<dyn Error>> {
		let depth = NonZeroUsize::new(4).ok_or("zero depth")?;
		let mut cursor = PrototypePrefetchCursor::new(0..32, depth, |value| Ok(value * 3));
		for expected in 0..32 {
			let actual = cursor.next().ok_or("prefetch ended early")??;
			assert_eq!(actual, expected * 3);
		}
		assert!(cursor.next().is_none());
		cursor.finish()?;
		Ok(())
	}

	#[test]
	fn first_error_is_terminal_and_ordered() -> Result<(), Box<dyn Error>> {
		let depth = NonZeroUsize::new(2).ok_or("zero depth")?;
		let loaded = Arc::new(AtomicUsize::new(0));
		let worker_loaded = Arc::clone(&loaded);
		let mut cursor = PrototypePrefetchCursor::new(0..10, depth, move |value| {
			worker_loaded.fetch_add(1, Ordering::Relaxed);
			if value == 3 {
				Err(FerrosGrynnError::storage_io_failure("intentional prefetch failure"))
			} else {
				Ok(value)
			}
		});
		for expected in 0..3 {
			assert_eq!(cursor.next().ok_or("prefetch ended early")??, expected);
		}
		assert!(cursor.next().ok_or("prefetch omitted terminal error")?.is_err());
		assert!(cursor.next().is_none());
		assert_eq!(loaded.load(Ordering::Relaxed), 4);
		Ok(())
	}

	#[test]
	fn worker_panic_is_reported_as_one_terminal_error() -> Result<(), Box<dyn Error>> {
		let depth = NonZeroUsize::new(2).ok_or("zero depth")?;
		let mut cursor = PrototypePrefetchCursor::new(0..4, depth, |value| {
			if value == 0 {
				panic!("intentional prefetch worker panic");
			}
			Ok(value)
		});

		assert!(cursor.next().ok_or("worker panic was reported as EOF")?.is_err());
		assert!(cursor.next().is_none());
		cursor.finish()?;
		Ok(())
	}

	#[test]
	fn early_drop_cancels_queued_work_and_joins() -> Result<(), Box<dyn Error>> {
		let depth = NonZeroUsize::new(2).ok_or("zero depth")?;
		let created = Arc::new(AtomicUsize::new(0));
		let dropped = Arc::new(AtomicUsize::new(0));
		let worker_finished = Arc::new(AtomicBool::new(false));
		let worker_created = Arc::clone(&created);
		let worker_dropped = Arc::clone(&dropped);
		let finished = Arc::clone(&worker_finished);
		let cursor = PrototypePrefetchCursor::new(0..100, depth, move |ordinal| {
			worker_created.fetch_add(1, Ordering::Relaxed);
			if ordinal == 99 {
				finished.store(true, Ordering::Release);
			}
			Ok(ReservationToken {
				dropped: Arc::clone(&worker_dropped),
				ordinal,
			})
		});

		let deadline = Instant::now() + Duration::from_secs(1);
		while created.load(Ordering::Relaxed) < 2 && Instant::now() < deadline {
			thread::yield_now();
		}
		drop(cursor);
		assert_eq!(created.load(Ordering::Relaxed), dropped.load(Ordering::Relaxed));
		assert!(!worker_finished.load(Ordering::Acquire));
		Ok(())
	}

	#[test]
	fn channel_depth_bounds_completed_extents() -> Result<(), Box<dyn Error>> {
		let depth = NonZeroUsize::new(4).ok_or("zero depth")?;
		let produced = Arc::new(AtomicUsize::new(0));
		let consumed = Arc::new(AtomicUsize::new(0));
		let maximum_ahead = Arc::new(AtomicUsize::new(0));
		let worker_produced = Arc::clone(&produced);
		let worker_consumed = Arc::clone(&consumed);
		let worker_maximum = Arc::clone(&maximum_ahead);
		let mut cursor = PrototypePrefetchCursor::new(0..64, depth, move |value| {
			let current = worker_produced.fetch_add(1, Ordering::Relaxed) + 1;
			let ahead = current.saturating_sub(worker_consumed.load(Ordering::Relaxed));
			worker_maximum.fetch_max(ahead, Ordering::Relaxed);
			Ok(value)
		});
		while let Some(result) = cursor.next() {
			let _ = result?;
			consumed.fetch_add(1, Ordering::Relaxed);
			thread::sleep(Duration::from_micros(20));
		}
		cursor.finish()?;
		assert!(maximum_ahead.load(Ordering::Relaxed) <= depth.get() + 1);
		Ok(())
	}

	#[test]
	fn reservation_token_exposes_ordinal() -> Result<(), Box<dyn Error>> {
		let dropped = Arc::new(AtomicUsize::new(0));
		let token = ReservationToken {
			dropped: Arc::clone(&dropped),
			ordinal: 7,
		};
		assert_eq!(token.ordinal, 7);
		drop(token);
		assert_eq!(dropped.load(Ordering::Relaxed), 1);
		Ok(())
	}
}
