//! Read-only version-aware Clock-Pro page cache.
//!
//! Resident pages are shared through [`Arc`] guards, so a cache hit does not
//! copy the 16 KiB page. The metadata mutex protects only cache state: page I/O
//! runs after it is released. A per-load signal coalesces concurrent misses for
//! the same key, while a condition variable bounds resident and in-flight pages
//! to the configured frame count.

use std::{
	collections::VecDeque,
	num::NonZeroUsize,
	sync::{
		Arc,
		atomic::{AtomicUsize, Ordering as AtomicOrdering},
	},
};

use parking_lot::{Condvar, Mutex};
use rapidhash::{HashMapExt, HashSetExt, RapidHashMap, RapidHashSet};
use rayon::prelude::{
	IndexedParallelIterator,
	IntoParallelRefIterator,
	IntoParallelRefMutIterator,
	ParallelIterator,
};

use super::{
	page::{AlignedPage, PAGE_SIZE, PageHeader, PageLsn, PageNo},
	types::{CheckpointGeneration, Lsn, PageKey},
};
use crate::gql_status::{FerrosGrynnError, FerrosGrynnResult};

/// Batched miss reservation and installation.
mod batch;
/// Clock-Pro replacement state machine.
mod clock_pro;

pub(in crate::storage) use batch::PageLoadExtent;
use batch::{BatchLoadReservation, LoadSignal, load_reserved_extents};
use clock_pro::ClockPro;
#[cfg(test)]
use clock_pro::FrameState;

/// Maximum number of complete clock passes made before applying backpressure.
const CLOCK_SCAN_PASSES: usize = 4;

/// Smallest extent for which parallel checksum work amortizes Rayon dispatch.
const PARALLEL_VALIDATION_MIN_PAGES: usize = 16;

/// Shared immutable page backed by one permanently aligned frame allocation.
type CachedPage = Arc<Box<AlignedPage>>;

/// Immutable source version represented by one cache entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum PageCacheVersion {
	/// Exact full-page image retained in WAL.
	Wal(PageLsn),
	/// Home-file image under one logical checkpoint generation.
	Home {
		/// Generation that invalidates every older home-cache entry.
		generation: CheckpointGeneration,
		/// Highest page LSN permitted by the database checkpoint.
		ceiling: Lsn,
	},
}

/// Complete identity of one immutable page version.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PageCacheKey {
	/// Complete database page identity.
	page_key: PageKey,
	/// WAL-exact or checkpoint-generation home version.
	version: PageCacheVersion,
}

impl PageCacheKey {
	/// Creates a cache key for one exact page image.
	pub const fn new(page_key: PageKey, version: PageCacheVersion) -> Self {
		Self {
			page_key,
			version,
		}
	}

	/// Returns the complete physical page identity.
	pub const fn page_key(self) -> PageKey {
		self.page_key
	}

	/// Returns the physical page number.
	pub const fn page_no(self) -> PageNo {
		self.page_key.page_no()
	}

	/// Returns the immutable cache-source version.
	pub const fn version(self) -> PageCacheVersion {
		self.version
	}
}

/// Snapshot of read-only cache counters.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) struct BufferPoolStats {
	/// Aligned page-buffer allocations performed while creating fixed frames.
	page_buffer_allocations: usize,
	/// Requests served from an already resident frame.
	hits: u64,
	/// Physical loads started by cache misses.
	misses: u64,
	/// Requests joined to an already in-flight load.
	coalesced_misses: u64,
	/// Resident pages removed by the clock hand.
	evictions: u64,
	/// Requests that had to wait for an unpinned frame.
	backpressure_waits: u64,
}

impl BufferPoolStats {
	/// Returns the lifetime number of aligned page-buffer allocations.
	pub(super) const fn page_buffer_allocations(self) -> usize {
		self.page_buffer_allocations
	}

	/// Returns resident cache hits.
	pub(super) const fn hits(self) -> u64 {
		self.hits
	}

	/// Returns physical cache misses.
	pub(super) const fn misses(self) -> u64 {
		self.misses
	}

	/// Returns misses coalesced with an in-flight load.
	pub(super) const fn coalesced_misses(self) -> u64 {
		self.coalesced_misses
	}

	/// Returns completed evictions.
	pub(super) const fn evictions(self) -> u64 {
		self.evictions
	}

	/// Returns capacity waits caused by pinned or loading frames.
	pub(super) const fn backpressure_waits(self) -> u64 {
		self.backpressure_waits
	}
}

/// RAII pin for one immutable resident page.
pub struct PageGuard {
	/// Shared resident page, declared first so its pin drops before
	/// notification.
	page: CachedPage,
	/// Notification sent after the preceding page field releases its pin.
	release: ReleaseNotifier,
}

impl PageGuard {
	/// Returns the pinned immutable aligned page.
	pub fn as_page(&self) -> &AlignedPage {
		self.page.as_ref().as_ref()
	}

	/// Returns the pinned immutable page without copying it.
	pub fn as_bytes(&self) -> &[u8; PAGE_SIZE] {
		self.as_page().as_bytes()
	}
}

/// Wakes capacity waiters after the preceding guard page field is dropped.
struct ReleaseNotifier {
	/// Pool notification endpoint retained for the guard lifetime.
	shared: Arc<Shared>,
}

impl Drop for ReleaseNotifier {
	/// Notifies only after Rust's declaration-order field drop released the
	/// pin.
	fn drop(&mut self) {
		if self.shared.capacity_waiters.load(AtomicOrdering::Relaxed) == 0 {
			return;
		}
		let _state = self.shared.state.lock();
		if self.shared.capacity_waiters.load(AtomicOrdering::Relaxed) > 0 {
			self.shared.capacity_available.notify_one();
		}
	}
}

/// Thread-safe bounded page cache.
#[derive(Clone)]
pub struct BufferPool {
	/// Shared metadata and wait queues retained by page guards.
	shared: Arc<Shared>,
}

impl BufferPool {
	/// Creates a cache with an exact non-zero frame budget.
	pub fn new(capacity: NonZeroUsize) -> Self {
		Self {
			shared: Arc::new(Shared {
				state: Mutex::new(ClockPro::new(capacity)),
				batch_admission: Mutex::new(()),
				capacity_available: Condvar::new(),
				capacity_waiters: AtomicUsize::new(0),
			}),
		}
	}

	/// Returns the fixed number of permanently allocated page frames.
	pub(super) fn capacity(&self) -> usize {
		self.shared.state.lock().frames.len()
	}

	/// Returns a pinned page, loading and validating it on a miss.
	///
	/// Only one caller executes `loader` for a particular key. Other callers
	/// wait for its result without holding the cache metadata mutex. If every
	/// frame is pinned or loading, this method waits until a guard or load
	/// frees capacity rather than exceeding the frame budget.
	///
	/// # Errors
	///
	/// Returns the loader error, an invalid-page error when the loaded header
	/// does not match the key, or an internal storage error if cache metadata
	/// is inconsistent.
	pub fn get_or_load<Loader>(
		&self,
		key: PageCacheKey,
		loader: Loader,
	) -> FerrosGrynnResult<PageGuard>
	where
		Loader: FnOnce(&mut AlignedPage) -> FerrosGrynnResult<PageHeader>,
	{
		match self.prepare_access(key)? {
			AccessDecision::Ready(page) => Ok(self.guard(page)),
			AccessDecision::Wait(signal) => Ok(self.guard(signal.wait()?)),
			AccessDecision::Load {
				frame_index,
				signal,
				mut page,
			} => {
				let load_result = Arc::get_mut(&mut page)
					.ok_or_else(|| {
						FerrosGrynnError::storage_io_failure(
							"buffer-pool loading frame was not uniquely owned",
						)
					})
					.and_then(|aligned_page| loader(aligned_page.as_mut()))
					.and_then(|header| validate_loaded_header(key, header));

				match load_result {
					Ok(()) => {
						let temperature =
							self.shared.state.lock().loading_temperature(frame_index, key);
						let Some(temperature) = temperature else {
							let error = FerrosGrynnError::storage_io_failure(
								"buffer-pool loading frame changed before publication",
							);
							self.abort_load(frame_index, key, page, &signal, error.clone());
							return Err(error);
						};
						let pin = Arc::clone(&page);
						self.shared.state.lock().publish_reserved(
							frame_index,
							key,
							page,
							temperature,
						);
						signal.finish(Ok(Arc::clone(&pin)));
						Ok(self.guard(pin))
					}
					Err(error) => {
						self.abort_load(frame_index, key, page, &signal, error.clone());
						Err(error)
					}
				}
			}
		}
	}

	/// Returns pinned pages in input order, preserving duplicate keys while
	/// loading physically adjacent misses as bounded extents.
	///
	/// Exact duplicate keys share one frame and one physical load. The method
	/// never holds the metadata mutex during `loader`, and a failed extent
	/// releases every reservation that has not been published.
	///
	/// # Errors
	///
	/// Returns an error when the unique batch exceeds pool capacity, a loader
	/// fails, a page does not match its key, an in-flight load fails, or cache
	/// metadata is inconsistent.
	pub(super) fn get_or_load_batch<Loader>(
		&self,
		keys: &[PageCacheKey],
		scratch: &mut [AlignedPage],
		mut loader: Loader,
	) -> FerrosGrynnResult<Vec<PageGuard>>
	where
		Loader: FnMut(PageKey, &mut [AlignedPage]) -> FerrosGrynnResult<()>,
	{
		self.get_or_load_batch_extents(keys, scratch, |extents, pages| {
			for extent in extents {
				let end = extent.buffer_start + extent.page_count;
				loader(extent.first_key, &mut pages[extent.buffer_start..end])?;
			}
			Ok(())
		})
	}

	/// Returns a batch while exposing all independent extents to one loader so
	/// a native transport may submit them concurrently.
	///
	/// # Errors
	///
	/// Returns the same admission, load, validation, and metadata errors as
	/// [`Self::get_or_load_batch`].
	pub(super) fn get_or_load_batch_extents<Loader>(
		&self,
		keys: &[PageCacheKey],
		scratch: &mut [AlignedPage],
		mut loader: Loader,
	) -> FerrosGrynnResult<Vec<PageGuard>>
	where
		Loader: FnMut(&[PageLoadExtent], &mut [AlignedPage]) -> FerrosGrynnResult<()>,
	{
		if keys.is_empty() {
			return Ok(Vec::new());
		}
		if scratch.is_empty() {
			return Err(FerrosGrynnError::storage_io_failure(
				"buffer-pool batch scratch space must not be empty",
			));
		}

		let mut unique_by_key = RapidHashMap::with_capacity(keys.len());
		let mut unique_keys = Vec::with_capacity(keys.len());
		let mut input_order = Vec::with_capacity(keys.len());
		for key in keys.iter().copied() {
			let unique_index = if let Some(index) = unique_by_key.get(&key).copied() {
				index
			} else {
				let index = unique_keys.len();
				unique_keys.push(key);
				unique_by_key.insert(key, index);
				index
			};
			input_order.push(unique_index);
		}
		if unique_keys.len() > self.shared.state.lock().frames.len() {
			return Err(FerrosGrynnError::storage_io_failure(
				"unique page batch exceeds buffer-pool capacity",
			));
		}

		let batch_admission = self.shared.batch_admission.lock();
		let mut resolved = vec![None; unique_keys.len()];
		let mut waiting = Vec::with_capacity(unique_keys.len());
		let mut loading = Vec::with_capacity(unique_keys.len());
		let mut next_unique = 0;
		let mut recorded_backpressure = false;
		let mut registered_waiter = false;
		let preparation = 'prepare: loop {
			let mut state = self.shared.state.lock();
			while next_unique < unique_keys.len() {
				let key = unique_keys[next_unique];
				let decision = if let Some(frame_index) = state.entries.get(&key).copied() {
					match state.access_resident(frame_index) {
						Ok(decision) => decision,
						Err(error) => break 'prepare Err(error),
					}
				} else if let Some(decision) = state.reserve(key) {
					decision
				} else {
					break;
				};
				match decision {
					AccessDecision::Ready(page) => resolved[next_unique] = Some(page),
					AccessDecision::Wait(signal) => waiting.push((next_unique, signal)),
					AccessDecision::Load {
						frame_index,
						signal,
						page,
					} => loading.push(BatchLoadReservation {
						unique_index: next_unique,
						frame_index,
						key,
						signal,
						page,
					}),
				}
				next_unique += 1;
			}
			if next_unique == unique_keys.len() {
				break Ok(());
			}
			if !recorded_backpressure {
				state.stats.backpressure_waits += 1;
				recorded_backpressure = true;
			}
			if !registered_waiter {
				drop(state);
				self.shared.capacity_waiters.fetch_add(1, AtomicOrdering::Relaxed);
				registered_waiter = true;
				continue;
			}
			self.shared.capacity_available.wait(&mut state);
		};
		if registered_waiter {
			self.shared.capacity_waiters.fetch_sub(1, AtomicOrdering::Relaxed);
		}
		if let Err(error) = preparation {
			self.abort_batch_loads(loading, error.clone());
			return Err(error);
		}
		drop(batch_admission);

		loading.sort_unstable_by_key(|reservation| {
			(reservation.key.page_key(), reservation.key.version())
		});
		let load_result = load_reserved_extents(&mut loading, scratch, &mut loader);
		if let Err(error) = load_result {
			self.abort_batch_loads(loading, error.clone());
			return Err(error);
		}

		let temperatures = {
			let state = self.shared.state.lock();
			loading
				.iter()
				.map(|reservation| {
					state.loading_temperature(reservation.frame_index, reservation.key)
				})
				.collect::<Option<Vec<_>>>()
		};
		let Some(temperatures) = temperatures else {
			let error = FerrosGrynnError::storage_io_failure(
				"buffer-pool batch reservation changed before publication",
			);
			self.abort_batch_loads(loading, error.clone());
			return Err(error);
		};
		let mut completed = Vec::with_capacity(loading.len());
		{
			let mut state = self.shared.state.lock();
			for (reservation, temperature) in loading.drain(..).zip(temperatures) {
				let pin = Arc::clone(&reservation.page);
				state.publish_reserved(
					reservation.frame_index,
					reservation.key,
					reservation.page,
					temperature,
				);
				completed.push((reservation.unique_index, reservation.signal, pin));
			}
		}
		for (unique_index, signal, page) in completed {
			resolved[unique_index] = Some(Arc::clone(&page));
			signal.finish(Ok(page));
		}
		for (unique_index, signal) in waiting {
			resolved[unique_index] = Some(signal.wait()?);
		}

		let mut guards = Vec::with_capacity(keys.len());
		for unique_index in input_order {
			let page =
				resolved.get(unique_index).and_then(Option::as_ref).cloned().ok_or_else(|| {
					FerrosGrynnError::storage_io_failure("buffer-pool batch page was not resolved")
				})?;
			guards.push(self.guard(page));
		}
		Ok(guards)
	}

	/// Resolves metadata while registering capacity waiters without a lost-wake
	/// window.
	fn prepare_access(&self, key: PageCacheKey) -> FerrosGrynnResult<AccessDecision> {
		let mut recorded_backpressure = false;
		let mut registered_waiter = false;
		let decision = loop {
			let mut state = self.shared.state.lock();
			if let Some(frame_index) = state.entries.get(&key).copied() {
				break state.access_resident(frame_index);
			}
			if let Some(decision) = state.reserve(key) {
				if self.shared.capacity_waiters.load(AtomicOrdering::Relaxed) > 0 {
					self.shared.capacity_available.notify_all();
				}
				break Ok(decision);
			}
			if !recorded_backpressure {
				state.stats.backpressure_waits += 1;
				recorded_backpressure = true;
			}
			if !registered_waiter {
				drop(state);
				self.shared.capacity_waiters.fetch_add(1, AtomicOrdering::Relaxed);
				registered_waiter = true;
				continue;
			}
			self.shared.capacity_available.wait(&mut state);
		};
		if registered_waiter {
			self.shared.capacity_waiters.fetch_sub(1, AtomicOrdering::Relaxed);
		}
		decision
	}

	/// Wraps a shared frame in an RAII capacity-notifying guard.
	fn guard(&self, page: CachedPage) -> PageGuard {
		PageGuard {
			page,
			release: ReleaseNotifier {
				shared: Arc::clone(&self.shared),
			},
		}
	}

	/// Returns a consistent snapshot of cache counters.
	pub(super) fn stats(&self) -> BufferPoolStats {
		self.shared.state.lock().stats
	}

	/// Returns the number of resident or in-flight frame reservations.
	pub(super) fn frame_count(&self) -> usize {
		self.shared.state.lock().entries.len()
	}

	/// Returns whether an exact page version is resident or loading.
	#[cfg(test)]
	fn contains(&self, key: PageCacheKey) -> bool {
		self.shared.state.lock().entries.contains_key(&key)
	}

	/// Returns the current number of Hot resident pages.
	#[cfg(test)]
	fn hot_count(&self) -> usize {
		self.shared.state.lock().hot_count
	}

	/// Returns the number of non-resident test entries retained by Clock-Pro.
	#[cfg(test)]
	fn ghost_count(&self) -> usize {
		self.shared.state.lock().ghost_set.len()
	}

	/// Frees a failed loading reservation and wakes both miss and capacity
	/// waiters.
	fn abort_load(
		&self,
		frame_index: usize,
		key: PageCacheKey,
		page: CachedPage,
		signal: &LoadSignal,
		error: FerrosGrynnError,
	) {
		self.shared.state.lock().abort(frame_index, key, page);
		signal.finish(Err(error));
		self.shared.capacity_available.notify_one();
	}

	/// Rolls back all unpublished frames from a failed batch.
	fn abort_batch_loads(&self, reservations: Vec<BatchLoadReservation>, error: FerrosGrynnError) {
		let mut state = self.shared.state.lock();
		let mut signals = Vec::with_capacity(reservations.len());
		for reservation in reservations {
			state.abort(reservation.frame_index, reservation.key, reservation.page);
			signals.push(reservation.signal);
		}
		drop(state);
		for signal in signals {
			signal.finish(Err(error.clone()));
		}
		self.shared.capacity_available.notify_all();
	}
}

/// Shared state retained by both the pool and page guards.
struct Shared {
	/// Clock-Pro metadata; never held while a page loader runs.
	state: Mutex<ClockPro>,
	/// Serializes multi-frame admission so capacity waiters cannot retain
	/// mutually blocking partial batch reservations.
	///
	/// ponytail: keep one admission lock until concurrent sync batch admission
	/// is a measured bottleneck; I/O remains parallel after reservation.
	batch_admission: Mutex<()>,
	/// Notification for a loading completion or released page guard.
	capacity_available: Condvar,
	/// Callers registered to recheck frame capacity before sleeping.
	capacity_waiters: AtomicUsize,
}

/// Result of one metadata lookup.
enum AccessDecision {
	/// Resident immutable page, already pinned by its cloned [`Arc`].
	Ready(CachedPage),
	/// Existing load whose result must be awaited.
	Wait(Arc<LoadSignal>),
	/// Newly reserved frame whose page must be loaded by this caller.
	Load {
		/// Fixed frame selected by the clock hand.
		frame_index: usize,
		/// Per-miss result shared with coalesced callers.
		signal: Arc<LoadSignal>,
		/// Unique aligned frame removed from metadata while I/O runs.
		page: CachedPage,
	},
}

/// Ensures a loader returned the immutable page version named by its key.
pub fn validate_loaded_header(key: PageCacheKey, header: PageHeader) -> FerrosGrynnResult<()> {
	if header.page_no() != key.page_no() {
		return Err(FerrosGrynnError::invalid_storage_page(
			key.page_no().get(),
			format!("loader returned page {}", header.page_no().get()),
		));
	}
	match key.version() {
		PageCacheVersion::Wal(expected) if header.page_lsn() != expected => {
			return Err(FerrosGrynnError::invalid_storage_page(
				key.page_no().get(),
				format!(
					"loader returned page LSN {}, expected WAL LSN {}",
					header.page_lsn().get(),
					expected.get()
				),
			));
		}
		PageCacheVersion::Home {
			ceiling,
			..
		} if header.page_lsn().get() > ceiling.get() => {
			return Err(FerrosGrynnError::invalid_storage_page(
				key.page_no().get(),
				format!(
					"home page LSN {} exceeds checkpoint ceiling {}",
					header.page_lsn().get(),
					ceiling.get()
				),
			));
		}
		PageCacheVersion::Wal(_)
		| PageCacheVersion::Home {
			..
		} => {}
	}
	Ok(())
}

#[cfg(test)]
#[allow(
	clippy::panic_in_result_fn,
	reason = "fallible test setup returns Result while assertions report contract failures"
)]
mod tests {
	use std::{
		error::Error,
		hint::black_box,
		io,
		num::NonZeroUsize,
		sync::{
			Arc,
			Barrier,
			atomic::{AtomicUsize, Ordering},
			mpsc,
		},
		thread,
		time::{Duration, Instant},
	};

	use dashmap::DashMap;
	use rapidhash::fast::RandomState as RapidRandomState;

	use super::*;
	use crate::storage::{
		page::{PAGE_HEADER_SIZE, PageFlags, PageType},
		types::{DataFileId, DataFileKind, GraphId},
	};

	/// Data file used by deterministic unit tests.
	const TEST_FILE_ID: DataFileId = DataFileId::for_kind(DataFileKind::SystemCatalog);
	/// Maximum time allowed for a deterministic inter-thread handoff.
	const TEST_TIMEOUT: Duration = Duration::from_secs(2);
	/// Repeated samples used by the ignored resident-hit cost probe.
	const RESIDENT_COST_SAMPLES: usize = 7;

	/// Creates a non-zero test capacity without panicking.
	fn capacity(value: usize) -> Result<NonZeroUsize, io::Error> {
		NonZeroUsize::new(value).ok_or_else(|| io::Error::other("test capacity must be non-zero"))
	}

	/// Creates a typed cache key for the test source.
	const fn key(page_no: u64, version: u64) -> PageCacheKey {
		PageCacheKey::new(
			PageKey::new(GraphId::SYSTEM, TEST_FILE_ID, PageNo::new(page_no)),
			PageCacheVersion::Wal(PageLsn::new(version)),
		)
	}

	/// Creates a WAL cache key for one page in the same physical file.
	const fn file_key(first: PageKey, page_no: PageNo, version: PageLsn) -> PageCacheKey {
		PageCacheKey::new(
			PageKey::new(first.graph_id(), first.file_id(), page_no),
			PageCacheVersion::Wal(version),
		)
	}

	/// Builds and validates one sealed test page in the provided aligned frame.
	fn load_test_page(
		page: &mut AlignedPage,
		key: PageCacheKey,
		marker: u8,
	) -> FerrosGrynnResult<PageHeader> {
		let page_lsn = match key.version() {
			PageCacheVersion::Wal(page_lsn) => page_lsn,
			PageCacheVersion::Home {
				ceiling,
				..
			} => PageLsn::new(ceiling.get()),
		};
		*page = AlignedPage::new(
			PageType::StorageSuperblock,
			PageFlags::empty(),
			page_lsn,
			key.page_no(),
		);
		page.as_bytes_mut()[PAGE_HEADER_SIZE] = marker;
		page.seal()?;
		page.validate(key.page_no())
	}

	/// Waits for a predicate using scheduler yields rather than timing sleeps.
	fn wait_until(mut predicate: impl FnMut() -> bool) -> bool {
		let deadline = Instant::now() + TEST_TIMEOUT;
		while Instant::now() < deadline {
			if predicate() {
				return true;
			}
			thread::yield_now();
		}
		false
	}

	/// Returns the median aggregate cost of one repeated diagnostic operation.
	#[allow(
		clippy::cast_precision_loss,
		reason = "diagnostic nanosecond output needs only human-readable precision"
	)]
	fn median_ns_per_operation(iterations: usize, mut operation: impl FnMut()) -> f64 {
		let mut samples = [0.0; RESIDENT_COST_SAMPLES];
		for _ in 0..iterations / 10 {
			operation();
		}
		for sample in &mut samples {
			let started = Instant::now();
			for _ in 0..iterations {
				operation();
			}
			*sample = started.elapsed().as_secs_f64() * 1_000_000_000.0 / iterations as f64;
		}
		samples.sort_unstable_by(f64::total_cmp);
		samples[samples.len() / 2]
	}

	/// Measures aggregate resident throughput with a synchronized reader set.
	#[allow(
		clippy::cast_precision_loss,
		reason = "diagnostic throughput needs only human-readable precision"
	)]
	fn concurrent_resident_ops(
		pool: &BufferPool,
		keys: &[PageCacheKey],
		thread_count: usize,
		operations_per_thread: usize,
		same_key: bool,
	) -> io::Result<f64> {
		let barrier = Arc::new(Barrier::new(thread_count + 1));
		let elapsed = thread::scope(|scope| -> io::Result<Duration> {
			let mut handles = Vec::with_capacity(thread_count);
			for thread_index in 0..thread_count {
				let barrier = Arc::clone(&barrier);
				handles.push(scope.spawn(move || -> io::Result<()> {
					barrier.wait();
					for operation in 0..operations_per_thread {
						let index = if same_key {
							0
						} else {
							(thread_index + operation * thread_count) % keys.len()
						};
						let guard = pool
							.get_or_load(keys[index], |_page| {
								Err(FerrosGrynnError::storage_io_failure(
									"concurrent resident probe unexpectedly invoked its loader",
								))
							})
							.map_err(|source| io::Error::other(source.to_string()))?;
						black_box(guard.as_bytes()[PAGE_HEADER_SIZE]);
					}
					Ok(())
				}));
			}
			let started = Instant::now();
			barrier.wait();
			for handle in handles {
				handle.join().map_err(|_panic_payload| {
					io::Error::other("resident probe reader panicked")
				})??;
			}
			Ok(started.elapsed())
		})?;
		Ok((thread_count * operations_per_thread) as f64 / elapsed.as_secs_f64())
	}

	#[test]
	fn hit_shares_page_and_key_separates_versions() -> Result<(), Box<dyn Error>> {
		let pool = BufferPool::new(capacity(2)?);
		let first_key = key(7, 11);
		let second_key = key(7, 12);
		let loads = AtomicUsize::new(0);

		let first = pool.get_or_load(first_key, |page| {
			loads.fetch_add(1, Ordering::Relaxed);
			load_test_page(page, first_key, 1)
		})?;
		let hit = pool.get_or_load(first_key, |_page| {
			Err(FerrosGrynnError::storage_io_failure("resident loader was called"))
		})?;
		assert!(std::ptr::eq(first.as_bytes(), hit.as_bytes()));
		assert_eq!(hit.as_bytes()[PAGE_HEADER_SIZE], 1);

		let second = pool.get_or_load(second_key, |page| {
			loads.fetch_add(1, Ordering::Relaxed);
			load_test_page(page, second_key, 2)
		})?;
		assert!(!std::ptr::eq(first.as_bytes(), second.as_bytes()));
		assert_eq!(loads.load(Ordering::Relaxed), 2);
		assert_eq!(pool.stats().hits(), 1);
		assert_eq!(pool.stats().misses(), 2);
		Ok(())
	}

	#[test]
	fn keys_from_different_files_share_capacity_without_aliasing() -> Result<(), Box<dyn Error>> {
		let first_file = DataFileId::new(1, DataFileKind::Index)
			.ok_or_else(|| io::Error::other("invalid first cache test file id"))?;
		let second_file = DataFileId::new(2, DataFileKind::Index)
			.ok_or_else(|| io::Error::other("invalid second cache test file id"))?;
		let page_no = PageNo::new(7);
		let version = PageCacheVersion::Wal(PageLsn::new(11));
		let first_key =
			PageCacheKey::new(PageKey::new(GraphId::SYSTEM, first_file, page_no), version);
		let second_key =
			PageCacheKey::new(PageKey::new(GraphId::SYSTEM, second_file, page_no), version);
		let pool = BufferPool::new(capacity(2)?);

		drop(pool.get_or_load(first_key, |page| load_test_page(page, first_key, 1))?);
		drop(pool.get_or_load(second_key, |page| load_test_page(page, second_key, 2))?);
		let first = pool.get_or_load(first_key, |_page| {
			Err(FerrosGrynnError::storage_io_failure("first resident loader was called"))
		})?;
		let second = pool.get_or_load(second_key, |_page| {
			Err(FerrosGrynnError::storage_io_failure("second resident loader was called"))
		})?;

		assert_eq!(first.as_bytes()[PAGE_HEADER_SIZE], 1);
		assert_eq!(second.as_bytes()[PAGE_HEADER_SIZE], 2);
		assert!(!std::ptr::eq(first.as_bytes(), second.as_bytes()));
		assert_eq!(pool.frame_count(), 2);
		assert_eq!(pool.stats().hits(), 2);
		assert_eq!(pool.stats().misses(), 2);
		Ok(())
	}

	#[test]
	fn concurrent_miss_coalesces_without_holding_metadata_lock() -> Result<(), Box<dyn Error>> {
		let pool = BufferPool::new(capacity(2)?);
		let hot_key = key(0, 1);
		let miss_key = key(1, 1);
		drop(pool.get_or_load(hot_key, |page| load_test_page(page, hot_key, 9))?);

		let pool = Arc::new(pool);
		let (load_started_tx, load_started_rx) = mpsc::channel();
		let (release_tx, release_rx) = mpsc::channel();
		let first_pool = Arc::clone(&pool);
		let first = thread::spawn(move || {
			first_pool.get_or_load(miss_key, |page| {
				load_started_tx.send(()).map_err(|source| {
					FerrosGrynnError::storage_io_failure("signal loader start")
						.with_technical_source(source)
				})?;
				release_rx.recv().map_err(|source| {
					FerrosGrynnError::storage_io_failure("release loader")
						.with_technical_source(source)
				})?;
				load_test_page(page, miss_key, 3)
			})
		});
		load_started_rx.recv_timeout(TEST_TIMEOUT)?;

		let second_pool = Arc::clone(&pool);
		let second_loads = Arc::new(AtomicUsize::new(0));
		let second_counter = Arc::clone(&second_loads);
		let second = thread::spawn(move || {
			second_pool.get_or_load(miss_key, |page| {
				second_counter.fetch_add(1, Ordering::Relaxed);
				load_test_page(page, miss_key, 4)
			})
		});
		assert!(wait_until(|| pool.stats().coalesced_misses() == 1));

		let (hit_tx, hit_rx) = mpsc::channel();
		let hit_pool = Arc::clone(&pool);
		let hit = thread::spawn(move || {
			let result = hit_pool.get_or_load(hot_key, |_page| {
				Err(FerrosGrynnError::storage_io_failure("hot loader was called"))
			});
			drop(hit_tx.send(result.map(|guard| guard.as_bytes()[PAGE_HEADER_SIZE])));
		});
		let hot_marker = hit_rx.recv_timeout(TEST_TIMEOUT);
		release_tx.send(())?;
		assert_eq!(hot_marker??, 9);

		let first_guard = first
			.join()
			.map_err(|_panic_payload| io::Error::other("first loader thread panicked"))??;
		let second_guard = second
			.join()
			.map_err(|_panic_payload| io::Error::other("coalesced loader thread panicked"))??;
		hit.join().map_err(|_panic_payload| io::Error::other("resident hit thread panicked"))?;
		assert!(std::ptr::eq(first_guard.as_bytes(), second_guard.as_bytes()));
		assert_eq!(second_loads.load(Ordering::Relaxed), 0);
		Ok(())
	}

	#[test]
	fn pinned_frame_applies_backpressure_until_guard_drops() -> Result<(), Box<dyn Error>> {
		let pool = Arc::new(BufferPool::new(NonZeroUsize::MIN));
		let pinned_key = key(0, 1);
		let next_key = key(1, 1);
		let guard = pool.get_or_load(pinned_key, |page| load_test_page(page, pinned_key, 1))?;
		let (loader_tx, loader_rx) = mpsc::channel();
		let waiting_pool = Arc::clone(&pool);
		let waiting = thread::spawn(move || {
			waiting_pool.get_or_load(next_key, |page| {
				loader_tx.send(()).map_err(|source| {
					FerrosGrynnError::storage_io_failure("signal capacity release")
						.with_technical_source(source)
				})?;
				load_test_page(page, next_key, 2)
			})
		});

		assert!(wait_until(|| pool.stats().backpressure_waits() == 1));
		assert_eq!(pool.frame_count(), 1);
		assert!(loader_rx.try_recv().is_err());
		drop(guard);
		loader_rx.recv_timeout(TEST_TIMEOUT)?;
		let next = waiting
			.join()
			.map_err(|_panic_payload| io::Error::other("capacity waiter thread panicked"))??;
		assert_eq!(next.as_bytes()[PAGE_HEADER_SIZE], 2);
		assert_eq!(pool.frame_count(), 1);
		Ok(())
	}

	#[test]
	fn guard_drop_without_capacity_waiters_does_not_lock_metadata() -> Result<(), Box<dyn Error>> {
		let pool = Arc::new(BufferPool::new(NonZeroUsize::MIN));
		let pinned_key = key(0, 1);
		let guard = pool.get_or_load(pinned_key, |page| load_test_page(page, pinned_key, 1))?;
		let state = pool.shared.state.lock();
		let (dropped_tx, dropped_rx) = mpsc::channel();
		let dropper = thread::spawn(move || {
			drop(guard);
			let _send_result = dropped_tx.send(());
		});

		dropped_rx.recv_timeout(TEST_TIMEOUT)?;
		assert!(matches!(
			&state.frames[0],
			FrameState::Resident { page, .. } if Arc::strong_count(page) == 1
		));
		drop(state);
		dropper.join().map_err(|_panic_payload| io::Error::other("guard dropper panicked"))?;
		Ok(())
	}

	/// Decomposes the uncontended resident-hit path without per-operation
	/// tracing.
	#[test]
	#[ignore = "release-mode diagnostic for resident buffer-pool hit costs"]
	fn resident_hit_cost_probe() -> Result<(), Box<dyn Error>> {
		const CAPACITY: usize = 4096;
		const ITERATIONS: usize = 1_000_000;
		const CONCURRENT_OPERATIONS: usize = 100_000;
		let pool = BufferPool::new(capacity(CAPACITY)?);
		let mut keys = Vec::with_capacity(CAPACITY);
		for index in 0..CAPACITY {
			let raw_index = u64::try_from(index).map_err(|source| {
				io::Error::other(format!("resident key index overflow: {source}"))
			})?;
			let resident_key = key(raw_index, raw_index + 1);
			drop(pool.get_or_load(resident_key, |page| {
				load_test_page(page, resident_key, raw_index.to_le_bytes()[0])
			})?);
			keys.push(resident_key);
		}
		let (frame_indices, pages) = {
			let state = pool.shared.state.lock();
			let mut frame_indices = Vec::with_capacity(CAPACITY);
			let mut pages = Vec::with_capacity(CAPACITY);
			for resident_key in &keys {
				let frame_index = state
					.entries
					.get(resident_key)
					.copied()
					.ok_or_else(|| io::Error::other("resident probe key is not indexed"))?;
				let FrameState::Resident {
					page,
					..
				} = &state.frames[frame_index]
				else {
					return Err(io::Error::other("resident probe frame is not resident").into());
				};
				frame_indices.push(frame_index);
				pages.push(Arc::clone(page));
			}
			(frame_indices, pages)
		};
		let dash_entries = DashMap::with_capacity_and_hasher(CAPACITY, RapidRandomState::default());
		for (resident_key, frame_index) in keys.iter().copied().zip(frame_indices.iter().copied()) {
			dash_entries.insert(resident_key, frame_index);
		}

		let mut cursor = 0_usize;
		let empty = median_ns_per_operation(ITERATIONS, || {
			let index = cursor & (CAPACITY - 1);
			cursor = cursor.wrapping_add(1);
			black_box(keys[index]);
		});
		let mut cursor = 0_usize;
		let lock = median_ns_per_operation(ITERATIONS, || {
			let index = cursor & (CAPACITY - 1);
			cursor = cursor.wrapping_add(1);
			black_box(keys[index]);
			let state = pool.shared.state.lock();
			black_box(state.frames.len());
		});
		let hash = {
			let state = pool.shared.state.lock();
			let mut cursor = 0_usize;
			median_ns_per_operation(ITERATIONS, || {
				let index = cursor & (CAPACITY - 1);
				cursor = cursor.wrapping_add(1);
				black_box(state.entries.get(black_box(&keys[index])).copied());
			})
		};
		let mut cursor = 0_usize;
		let lock_hash = median_ns_per_operation(ITERATIONS, || {
			let index = cursor & (CAPACITY - 1);
			cursor = cursor.wrapping_add(1);
			let state = pool.shared.state.lock();
			black_box(state.entries.get(black_box(&keys[index])).copied());
		});
		let resident_update = {
			let mut state = pool.shared.state.lock();
			let mut cursor = 0_usize;
			median_ns_per_operation(ITERATIONS, || {
				let index = cursor & (CAPACITY - 1);
				cursor = cursor.wrapping_add(1);
				let decision = state.access_resident(frame_indices[index]);
				black_box(decision.is_ok());
			})
		};
		let mut cursor = 0_usize;
		let prepare_access = median_ns_per_operation(ITERATIONS, || {
			let index = cursor & (CAPACITY - 1);
			cursor = cursor.wrapping_add(1);
			let decision = pool.prepare_access(black_box(keys[index]));
			black_box(decision.is_ok());
		});
		let mut cursor = 0_usize;
		let page_arc = median_ns_per_operation(ITERATIONS, || {
			let index = cursor & (CAPACITY - 1);
			cursor = cursor.wrapping_add(1);
			black_box(Arc::clone(&pages[index]));
		});
		let mut cursor = 0_usize;
		let shared_arc = median_ns_per_operation(ITERATIONS, || {
			let index = cursor & (CAPACITY - 1);
			cursor = cursor.wrapping_add(1);
			black_box(keys[index]);
			black_box(Arc::clone(&pool.shared));
		});
		let mut cursor = 0_usize;
		let waiter_load = median_ns_per_operation(ITERATIONS, || {
			let index = cursor & (CAPACITY - 1);
			cursor = cursor.wrapping_add(1);
			black_box(keys[index]);
			black_box(pool.shared.capacity_waiters.load(AtomicOrdering::Relaxed));
		});
		let mut cursor = 0_usize;
		let guard = median_ns_per_operation(ITERATIONS, || {
			let index = cursor & (CAPACITY - 1);
			cursor = cursor.wrapping_add(1);
			let guard = pool.guard(Arc::clone(&pages[index]));
			black_box(guard.as_bytes()[PAGE_HEADER_SIZE]);
		});
		let mut cursor = 0_usize;
		let full_hit = median_ns_per_operation(ITERATIONS, || {
			let index = cursor & (CAPACITY - 1);
			cursor = cursor.wrapping_add(1);
			let result = pool.get_or_load(black_box(keys[index]), |_page| {
				Err(FerrosGrynnError::storage_io_failure(
					"resident probe unexpectedly invoked its loader",
				))
			});
			if let Ok(guard) = result {
				black_box(guard.as_bytes()[PAGE_HEADER_SIZE]);
			}
		});
		let mut cursor = 0_usize;
		let dash_lookup = median_ns_per_operation(ITERATIONS, || {
			let index = cursor & (CAPACITY - 1);
			cursor = cursor.wrapping_add(1);
			black_box(dash_entries.get(black_box(&keys[index])).map(|entry| *entry));
		});
		let mut cursor = 0_usize;
		let dash_lock_update = median_ns_per_operation(ITERATIONS, || {
			let index = cursor & (CAPACITY - 1);
			cursor = cursor.wrapping_add(1);
			let frame_index = dash_entries.get(black_box(&keys[index])).map(|entry| *entry);
			if let Some(frame_index) = frame_index {
				let decision = pool.shared.state.lock().access_resident(frame_index);
				black_box(decision.is_ok());
			}
		});

		let final_guard = pool.get_or_load(keys[0], |_page| {
			Err(FerrosGrynnError::storage_io_failure(
				"resident probe unexpectedly invoked its loader",
			))
		})?;
		assert_eq!(final_guard.as_bytes()[PAGE_HEADER_SIZE], 0);
		println!(
			"resident_hit_cost_probe entries={CAPACITY} iterations={ITERATIONS} key_bytes={} \
			 guard_bytes={} empty_ns={empty:.2} lock_ns={lock:.2} hash_ns={hash:.2} \
			 lock_hash_ns={lock_hash:.2} resident_update_ns={resident_update:.2} \
			 prepare_access_ns={prepare_access:.2} page_arc_ns={page_arc:.2} \
			 shared_arc_ns={shared_arc:.2} waiter_load_ns={waiter_load:.2} \
			 guard_roundtrip_ns={guard:.2} full_hit_ns={full_hit:.2} \
			 dash_lookup_ns={dash_lookup:.2} dash_lock_update_ns={dash_lock_update:.2}",
			size_of::<PageCacheKey>(),
			size_of::<PageGuard>(),
		);
		drop(final_guard);
		let max_threads = thread::available_parallelism()?.get().min(8);
		for thread_count in [1, 2, 4, 8] {
			if thread_count > max_threads {
				continue;
			}
			let same_key =
				concurrent_resident_ops(&pool, &keys, thread_count, CONCURRENT_OPERATIONS, true)?;
			let distributed =
				concurrent_resident_ops(&pool, &keys, thread_count, CONCURRENT_OPERATIONS, false)?;
			println!(
				"resident_hit_concurrency threads={thread_count} same_key_ops_s={same_key:.0} \
				 distributed_keys_ops_s={distributed:.0}"
			);
		}
		assert_eq!(pool.stats().backpressure_waits(), 0);
		Ok(())
	}

	#[test]
	fn released_frames_wake_waiters_and_preserve_coalescing() -> Result<(), Box<dyn Error>> {
		let pool = Arc::new(BufferPool::new(capacity(2)?));
		let first_key = key(0, 1);
		let second_key = key(1, 1);
		let coalesced_key = key(2, 1);
		let distinct_key = key(3, 1);
		let first_guard = pool.get_or_load(first_key, |page| load_test_page(page, first_key, 1))?;
		let second_guard =
			pool.get_or_load(second_key, |page| load_test_page(page, second_key, 2))?;
		let (coalesced_started_tx, coalesced_started_rx) = mpsc::channel();
		let (release_coalesced_tx, release_coalesced_rx) = mpsc::channel();
		let release_coalesced_rx = Arc::new(Mutex::new(release_coalesced_rx));

		let first_pool = Arc::clone(&pool);
		let first_started = coalesced_started_tx.clone();
		let first_release = Arc::clone(&release_coalesced_rx);
		let first_waiter = thread::spawn(move || {
			first_pool.get_or_load(coalesced_key, |page| {
				first_started.send(()).map_err(|source| {
					FerrosGrynnError::storage_io_failure("signal coalesced loader")
						.with_technical_source(source)
				})?;
				first_release.lock().recv().map_err(|source| {
					FerrosGrynnError::storage_io_failure("release coalesced loader")
						.with_technical_source(source)
				})?;
				load_test_page(page, coalesced_key, 3)
			})
		});
		assert!(wait_until(|| pool.stats().backpressure_waits() == 1));

		let second_pool = Arc::clone(&pool);
		let second_started = coalesced_started_tx;
		let second_release = Arc::clone(&release_coalesced_rx);
		let second_waiter = thread::spawn(move || {
			second_pool.get_or_load(coalesced_key, |page| {
				second_started.send(()).map_err(|source| {
					FerrosGrynnError::storage_io_failure("signal coalesced loader")
						.with_technical_source(source)
				})?;
				second_release.lock().recv().map_err(|source| {
					FerrosGrynnError::storage_io_failure("release coalesced loader")
						.with_technical_source(source)
				})?;
				load_test_page(page, coalesced_key, 3)
			})
		});
		assert!(wait_until(|| pool.stats().backpressure_waits() == 2));

		drop(first_guard);
		coalesced_started_rx.recv_timeout(TEST_TIMEOUT)?;
		assert!(wait_until(|| pool.stats().coalesced_misses() == 1));

		let (distinct_started_tx, distinct_started_rx) = mpsc::channel();
		let distinct_pool = Arc::clone(&pool);
		let distinct_waiter = thread::spawn(move || {
			distinct_pool.get_or_load(distinct_key, |page| {
				distinct_started_tx.send(()).map_err(|source| {
					FerrosGrynnError::storage_io_failure("signal distinct loader")
						.with_technical_source(source)
				})?;
				load_test_page(page, distinct_key, 4)
			})
		});
		assert!(wait_until(|| pool.stats().backpressure_waits() == 3));

		drop(second_guard);
		distinct_started_rx.recv_timeout(TEST_TIMEOUT)?;
		release_coalesced_tx.send(())?;
		let first_page = first_waiter
			.join()
			.map_err(|_panic_payload| io::Error::other("first coalesced waiter panicked"))??;
		let second_page = second_waiter
			.join()
			.map_err(|_panic_payload| io::Error::other("second coalesced waiter panicked"))??;
		let distinct_page = distinct_waiter
			.join()
			.map_err(|_panic_payload| io::Error::other("distinct waiter panicked"))??;
		assert!(std::ptr::eq(first_page.as_bytes(), second_page.as_bytes()));
		assert_eq!(distinct_page.as_bytes()[PAGE_HEADER_SIZE], 4);
		Ok(())
	}

	#[test]
	fn frame_payload_address_is_reused_after_eviction() -> Result<(), Box<dyn Error>> {
		let pool = BufferPool::new(NonZeroUsize::MIN);
		assert_eq!(pool.stats().page_buffer_allocations(), 1);
		let first_key = key(0, 1);
		let first = pool.get_or_load(first_key, |page| load_test_page(page, first_key, 1))?;
		let first_address = first.as_bytes().as_ptr();
		drop(first);

		let second_key = key(1, 1);
		let second = pool.get_or_load(second_key, |page| load_test_page(page, second_key, 2))?;
		assert_eq!(first_address, second.as_bytes().as_ptr());
		assert_eq!((second.as_bytes().as_ptr() as usize) % PAGE_SIZE, 0);
		assert_eq!(pool.stats().page_buffer_allocations(), 1);
		Ok(())
	}

	#[test]
	fn batch_preserves_order_duplicates_and_contiguous_extents() -> Result<(), Box<dyn Error>> {
		let pool = BufferPool::new(capacity(8)?);
		let second_key = PageCacheKey::new(
			PageKey::new(GraphId::new(2), TEST_FILE_ID, PageNo::new(0)),
			PageCacheVersion::Wal(PageLsn::new(1)),
		);
		let requests = [key(2, 1), key(0, 1), key(2, 1), second_key, key(1, 1), key(5, 1)];
		let mut extents = Vec::new();
		let mut scratch = (0..4).map(|_| AlignedPage::zeroed()).collect::<Vec<_>>();
		let pages = pool.get_or_load_batch(&requests, &mut scratch, |first, output| {
			extents.push((first, output.len()));
			for (index, page) in output.iter_mut().enumerate() {
				let raw_index = u64::try_from(index).map_err(|source| {
					FerrosGrynnError::storage_io_failure(format!(
						"test batch index conversion failed: {source}"
					))
				})?;
				let page_no = PageNo::new(first.page_no().get() + raw_index);
				let page_key = file_key(first, page_no, PageLsn::new(1));
				load_test_page(page, page_key, page_no.get() as u8)?;
			}
			Ok(())
		})?;

		assert_eq!(extents, [
			(PageKey::new(GraphId::SYSTEM, TEST_FILE_ID, PageNo::new(0)), 3),
			(PageKey::new(GraphId::SYSTEM, TEST_FILE_ID, PageNo::new(5)), 1),
			(PageKey::new(GraphId::new(2), TEST_FILE_ID, PageNo::new(0)), 1),
		]);
		assert_eq!(
			pages.iter().map(|guard| guard.as_bytes()[PAGE_HEADER_SIZE]).collect::<Vec<_>>(),
			[2, 0, 2, 0, 1, 5]
		);
		assert!(std::ptr::eq(pages[0].as_bytes(), pages[2].as_bytes()));
		Ok(())
	}

	#[test]
	fn large_contiguous_batch_installs_every_validated_page() -> Result<(), Box<dyn Error>> {
		let page_count = PARALLEL_VALIDATION_MIN_PAGES;
		let pool = BufferPool::new(capacity(page_count)?);
		let requests = (0..page_count)
			.map(|page_no| {
				let page_no = page_no as u64;
				key(page_no, 1)
			})
			.collect::<Vec<_>>();
		let mut scratch = (0..page_count).map(|_| AlignedPage::zeroed()).collect::<Vec<_>>();

		let pages = pool.get_or_load_batch(&requests, &mut scratch, |first, output| {
			for (index, page) in output.iter_mut().enumerate() {
				let page_no = PageNo::new(first.page_no().get() + index as u64);
				let page_key = file_key(first, page_no, PageLsn::new(1));
				load_test_page(page, page_key, page_no.get() as u8)?;
			}
			Ok(())
		})?;

		assert_eq!(pages.len(), page_count);
		assert!(
			pages
				.iter()
				.enumerate()
				.all(|(index, page)| { page.as_bytes()[PAGE_HEADER_SIZE] == index as u8 })
		);
		assert_eq!(pool.stats().page_buffer_allocations(), page_count);
		Ok(())
	}

	#[test]
	fn failed_batch_releases_every_unpublished_reservation() -> Result<(), Box<dyn Error>> {
		let pool = BufferPool::new(capacity(4)?);
		let requests = [key(0, 1), key(1, 2), key(2, 3)];
		let mut scratch = (0..4).map(|_| AlignedPage::zeroed()).collect::<Vec<_>>();
		let error = pool
			.get_or_load_batch(&requests, &mut scratch, |_first, _pages| {
				Err(FerrosGrynnError::storage_io_failure("intentional batch failure"))
			})
			.err()
			.ok_or_else(|| io::Error::other("failed batch unexpectedly succeeded"))?;
		assert_eq!(error.status().code().to_string(), "FG-51I02");
		assert_eq!(pool.frame_count(), 0);
		assert!(requests.iter().all(|request| !pool.contains(*request)));

		let recovered =
			pool.get_or_load(requests[0], |page| load_test_page(page, requests[0], 9))?;
		assert_eq!(recovered.as_bytes()[PAGE_HEADER_SIZE], 9);
		Ok(())
	}

	#[test]
	fn batch_handles_resident_loading_missing_and_duplicate_pages() -> Result<(), Box<dyn Error>> {
		let pool = Arc::new(BufferPool::new(capacity(4)?));
		let resident_key = key(0, 1);
		let loading_key = key(1, 2);
		let missing_key = key(2, 3);
		drop(pool.get_or_load(resident_key, |page| load_test_page(page, resident_key, 10))?);

		let (scalar_started_tx, scalar_started_rx) = mpsc::channel();
		let (release_scalar_tx, release_scalar_rx) = mpsc::channel();
		let scalar_pool = Arc::clone(&pool);
		let scalar = thread::spawn(move || {
			scalar_pool.get_or_load(loading_key, |page| {
				scalar_started_tx.send(()).map_err(|source| {
					FerrosGrynnError::storage_io_failure("signal scalar batch test load")
						.with_technical_source(source)
				})?;
				release_scalar_rx.recv().map_err(|source| {
					FerrosGrynnError::storage_io_failure("release scalar batch test load")
						.with_technical_source(source)
				})?;
				load_test_page(page, loading_key, 11)
			})
		});
		scalar_started_rx.recv_timeout(TEST_TIMEOUT)?;

		let (batch_loaded_tx, batch_loaded_rx) = mpsc::channel();
		let batch_pool = Arc::clone(&pool);
		let batch = thread::spawn(move || {
			let mut scratch = [AlignedPage::zeroed()];
			batch_pool.get_or_load_batch(
				&[resident_key, loading_key, missing_key, missing_key],
				&mut scratch,
				|first, pages| {
					if first != missing_key.page_key() || pages.len() != 1 {
						return Err(FerrosGrynnError::storage_io_failure(
							"batch loader received an unexpected extent",
						));
					}
					load_test_page(&mut pages[0], missing_key, 12)?;
					batch_loaded_tx.send(()).map_err(|source| {
						FerrosGrynnError::storage_io_failure("signal batch extent completion")
							.with_technical_source(source)
					})?;
					Ok(())
				},
			)
		});
		batch_loaded_rx.recv_timeout(TEST_TIMEOUT)?;
		release_scalar_tx.send(())?;

		let scalar_page = scalar
			.join()
			.map_err(|_panic_payload| io::Error::other("scalar batch-test loader panicked"))??;
		let pages = batch
			.join()
			.map_err(|_panic_payload| io::Error::other("mixed batch loader panicked"))??;
		assert_eq!(
			pages.iter().map(|guard| guard.as_bytes()[PAGE_HEADER_SIZE]).collect::<Vec<_>>(),
			[10, 11, 12, 12]
		);
		assert!(std::ptr::eq(scalar_page.as_bytes(), pages[1].as_bytes()));
		assert!(std::ptr::eq(pages[2].as_bytes(), pages[3].as_bytes()));
		Ok(())
	}

	#[test]
	fn concurrent_batches_cannot_hold_partial_admission_against_each_other()
	-> Result<(), Box<dyn Error>> {
		let pool = Arc::new(BufferPool::new(capacity(2)?));
		let pinned_key = key(0, 1);
		let pinned = pool.get_or_load(pinned_key, |page| load_test_page(page, pinned_key, 0))?;

		let (first_tx, first_rx) = mpsc::channel();
		let first_pool = Arc::clone(&pool);
		let first = thread::spawn(move || {
			let requests = [key(1, 2), key(2, 3)];
			let mut scratch = (0..2).map(|_| AlignedPage::zeroed()).collect::<Vec<_>>();
			let result = first_pool
				.get_or_load_batch(&requests, &mut scratch, |first, output| {
					for (index, page) in output.iter_mut().enumerate() {
						let page_no = PageNo::new(first.page_no().get() + index as u64);
						load_test_page(
							page,
							file_key(first, page_no, PageLsn::new(page_no.get() + 1)),
							page_no.get() as u8,
						)?;
					}
					Ok(())
				})
				.map(|pages| {
					pages.iter().map(|page| page.as_bytes()[PAGE_HEADER_SIZE]).collect::<Vec<_>>()
				});
			drop(first_tx.send(result));
		});
		assert!(wait_until(|| pool.stats().backpressure_waits() == 1));
		assert!(pool.shared.batch_admission.try_lock().is_none());

		let (second_tx, second_rx) = mpsc::channel();
		let second_pool = Arc::clone(&pool);
		let second = thread::spawn(move || {
			let requests = [key(3, 4), key(4, 5)];
			let mut scratch = (0..2).map(|_| AlignedPage::zeroed()).collect::<Vec<_>>();
			let result = second_pool
				.get_or_load_batch(&requests, &mut scratch, |first, output| {
					for (index, page) in output.iter_mut().enumerate() {
						let page_no = PageNo::new(first.page_no().get() + index as u64);
						load_test_page(
							page,
							file_key(first, page_no, PageLsn::new(page_no.get() + 1)),
							page_no.get() as u8,
						)?;
					}
					Ok(())
				})
				.map(|pages| {
					pages.iter().map(|page| page.as_bytes()[PAGE_HEADER_SIZE]).collect::<Vec<_>>()
				});
			drop(second_tx.send(result));
		});

		drop(pinned);
		assert_eq!(first_rx.recv_timeout(TEST_TIMEOUT)??, [1, 2]);
		assert_eq!(second_rx.recv_timeout(TEST_TIMEOUT)??, [3, 4]);
		first
			.join()
			.map_err(|_panic_payload| io::Error::other("first concurrent batch thread panicked"))?;
		second.join().map_err(|_panic_payload| {
			io::Error::other("second concurrent batch thread panicked")
		})?;
		Ok(())
	}

	#[test]
	fn failed_load_releases_reservation_and_error_to_waiters() -> Result<(), Box<dyn Error>> {
		let pool = BufferPool::new(NonZeroUsize::MIN);
		let failed_key = key(0, 1);
		let error = pool
			.get_or_load(failed_key, |_page| {
				Err(FerrosGrynnError::storage_io_failure("intentional test load failure"))
			})
			.err()
			.ok_or_else(|| io::Error::other("failed load unexpectedly succeeded"))?;
		assert_eq!(error.status().code().to_string(), "FG-51I02");
		assert_eq!(pool.frame_count(), 0);

		let recovered = pool.get_or_load(failed_key, |page| load_test_page(page, failed_key, 5))?;
		assert_eq!(recovered.as_bytes()[PAGE_HEADER_SIZE], 5);
		Ok(())
	}

	#[test]
	fn clock_pro_retains_periodically_referenced_page_during_scan() -> Result<(), Box<dyn Error>> {
		let pool = BufferPool::new(capacity(8)?);
		let hot_key = key(0, 1);
		let hot_loads = AtomicUsize::new(0);
		drop(pool.get_or_load(hot_key, |page| {
			hot_loads.fetch_add(1, Ordering::Relaxed);
			load_test_page(page, hot_key, 7)
		})?);

		for page_no in 1_u64..32 {
			let scan_key = key(page_no, 1);
			drop(pool.get_or_load(scan_key, |page| load_test_page(page, scan_key, 0))?);
			if page_no.is_multiple_of(4) {
				drop(pool.get_or_load(hot_key, |page| {
					hot_loads.fetch_add(1, Ordering::Relaxed);
					load_test_page(page, hot_key, 7)
				})?);
			}
		}

		assert!(pool.contains(hot_key));
		assert!(pool.hot_count() > 0);
		assert!(pool.ghost_count() <= 8);
		assert_eq!(hot_loads.load(Ordering::Relaxed), 1);
		assert!(pool.stats().evictions() > 0);
		Ok(())
	}
}
