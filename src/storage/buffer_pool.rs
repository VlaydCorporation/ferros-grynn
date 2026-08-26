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

/// Maximum number of complete clock passes made before applying backpressure.
const CLOCK_SCAN_PASSES: usize = 4;

/// Smallest extent for which parallel checksum work amortizes Rayon dispatch.
const PARALLEL_VALIDATION_MIN_PAGES: usize = 16;

/// Shared immutable page backed by one permanently aligned frame allocation.
type CachedPage = Arc<Box<AlignedPage>>;

/// Immutable source version represented by one cache entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(super) enum PageCacheVersion {
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
pub(super) struct PageCacheKey {
	/// Complete database page identity.
	page_key: PageKey,
	/// WAL-exact or checkpoint-generation home version.
	version: PageCacheVersion,
}

impl PageCacheKey {
	/// Creates a cache key for one exact page image.
	pub(super) const fn new(page_key: PageKey, version: PageCacheVersion) -> Self {
		Self {
			page_key,
			version,
		}
	}

	/// Returns the complete physical page identity.
	pub(super) const fn page_key(self) -> PageKey {
		self.page_key
	}

	/// Returns the physical page number.
	pub(super) const fn page_no(self) -> PageNo {
		self.page_key.page_no()
	}

	/// Returns the immutable cache-source version.
	pub(super) const fn version(self) -> PageCacheVersion {
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
pub(super) struct PageGuard {
	/// Shared resident page, declared first so its pin drops before
	/// notification.
	page: CachedPage,
	/// Notification sent after the preceding page field releases its pin.
	release: ReleaseNotifier,
}

impl PageGuard {
	/// Returns the pinned immutable aligned page.
	pub(super) fn as_page(&self) -> &AlignedPage {
		self.page.as_ref().as_ref()
	}

	/// Returns the pinned immutable page without copying it.
	pub(super) fn as_bytes(&self) -> &[u8; PAGE_SIZE] {
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
pub(super) struct BufferPool {
	/// Shared metadata and wait queues retained by page guards.
	shared: Arc<Shared>,
}

impl BufferPool {
	/// Creates a cache with an exact non-zero frame budget.
	pub(super) fn new(capacity: NonZeroUsize) -> Self {
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
	pub(super) fn get_or_load<Loader>(
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

/// One unpublished frame owned by a batched miss operation.
struct BatchLoadReservation {
	/// Position in the deduplicated request vector.
	unique_index: usize,
	/// Fixed frame selected under the metadata mutex.
	frame_index: usize,
	/// Exact page version being loaded.
	key: PageCacheKey,
	/// Completion signal retained by coalesced scalar or batch callers.
	signal: Arc<LoadSignal>,
	/// Unique reusable aligned frame buffer.
	page: CachedPage,
}

/// Loads sorted reservations as adjacent, source-local extents.
fn load_reserved_extents<Loader>(
	reservations: &mut [BatchLoadReservation],
	scratch: &mut [AlignedPage],
	loader: &mut Loader,
) -> FerrosGrynnResult<()>
where
	Loader: FnMut(PageKey, &mut [AlignedPage]) -> FerrosGrynnResult<()>,
{
	let mut extent_start = 0;
	while extent_start < reservations.len() {
		let first_key = reservations[extent_start].key.page_key();
		let version = reservations[extent_start].key.version();
		let mut extent_end = extent_start + 1;
		while extent_end < reservations.len() && extent_end - extent_start < scratch.len() {
			let previous = reservations[extent_end - 1].key;
			let current = reservations[extent_end].key;
			let adjacent = previous.page_no().get().checked_add(1) == Some(current.page_no().get());
			let same_file = current.page_key().graph_id() == first_key.graph_id()
				&& current.page_key().file_id() == first_key.file_id();
			if !same_file || current.version() != version || !adjacent {
				break;
			}
			extent_end += 1;
		}

		let extent_len = extent_end - extent_start;
		let extent_pages = &mut scratch[..extent_len];
		loader(first_key, extent_pages)?;
		let extent_reservations = &mut reservations[extent_start..extent_end];
		if extent_len >= PARALLEL_VALIDATION_MIN_PAGES {
			extent_reservations.par_iter_mut().zip(extent_pages.par_iter()).try_for_each(
				|(reservation, loaded_page)| install_loaded_page(reservation, loaded_page),
			)?;
		} else {
			for (reservation, loaded_page) in
				extent_reservations.iter_mut().zip(extent_pages.iter())
			{
				install_loaded_page(reservation, loaded_page)?;
			}
		}
		extent_start = extent_end;
	}
	Ok(())
}

/// Validates one loaded image and copies it into its permanent frame.
fn install_loaded_page(
	reservation: &mut BatchLoadReservation,
	loaded_page: &AlignedPage,
) -> FerrosGrynnResult<()> {
	let header = loaded_page.validate(reservation.key.page_no())?;
	validate_loaded_header(reservation.key, header)?;
	let frame = Arc::get_mut(&mut reservation.page).ok_or_else(|| {
		FerrosGrynnError::storage_io_failure("buffer-pool batch frame was not uniquely owned")
	})?;
	frame.as_mut().as_bytes_mut().copy_from_slice(loaded_page.as_bytes());
	Ok(())
}

/// Completion cell for all callers coalesced onto one physical page load.
struct LoadSignal {
	/// `None` while loading, then one clonable pinned page or error result.
	result: Mutex<Option<FerrosGrynnResult<CachedPage>>>,
	/// Notification sent exactly when the result is published.
	completed: Condvar,
}

impl LoadSignal {
	/// Creates an incomplete load signal.
	fn new() -> Self {
		Self {
			result: Mutex::new(None),
			completed: Condvar::new(),
		}
	}

	/// Waits until the physical load publishes a result.
	fn wait(&self) -> FerrosGrynnResult<CachedPage> {
		let mut result = self.result.lock();
		loop {
			if let Some(result) = result.as_ref() {
				return result.clone();
			}
			self.completed.wait(&mut result);
		}
	}

	/// Publishes the load result and wakes every coalesced caller.
	fn finish(&self, outcome: FerrosGrynnResult<CachedPage>) {
		*self.result.lock() = Some(outcome);
		self.completed.notify_all();
	}
}

/// Temperature of one resident Clock-Pro frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Temperature {
	/// New or not recently re-referenced page.
	Cold,
	/// Page retained after a re-reference or ghost hit.
	Hot,
}

/// Mutually exclusive lifecycle state of one fixed cache frame.
enum FrameState {
	/// Frame contains no page image and retains its reusable aligned buffer.
	Free {
		/// Unique buffer allocated once when the pool is created.
		page: CachedPage,
	},
	/// Frame budget is reserved while its page is loaded without the metadata
	/// lock.
	Loading {
		/// Key being loaded.
		key: PageCacheKey,
		/// Temperature assigned when the load becomes resident.
		temperature: Temperature,
		/// Completion signal retained by coalesced callers.
		signal: Arc<LoadSignal>,
	},
	/// Immutable page image owned by the cache.
	Resident {
		/// Exact cached page version.
		key: PageCacheKey,
		/// Shared page; strong references above one are RAII pins.
		page: CachedPage,
		/// Current Clock-Pro temperature.
		temperature: Temperature,
		/// Reference bit set by resident hits.
		referenced: bool,
	},
}

/// Clock-Pro metadata protected by the pool mutex.
struct ClockPro {
	/// Fixed number of frames defining the hard memory budget.
	frames: Vec<FrameState>,
	/// Exact page key to fixed frame index.
	entries: RapidHashMap<PageCacheKey, usize>,
	/// FIFO order of recently evicted Cold keys.
	ghost_queue: VecDeque<PageCacheKey>,
	/// Constant-time membership for the bounded ghost history.
	ghost_set: RapidHashSet<PageCacheKey>,
	/// Current clock-hand index.
	hand: usize,
	/// Number of resident Hot frames.
	hot_count: usize,
	/// Adaptive upper target for Hot frames.
	hot_target: usize,
	/// Cache counters updated under the same metadata mutex.
	stats: BufferPoolStats,
}

impl ClockPro {
	/// Allocates every fixed frame and aligned payload exactly once.
	fn new(capacity: NonZeroUsize) -> Self {
		let capacity = capacity.get();
		let stats = BufferPoolStats {
			page_buffer_allocations: capacity,
			..BufferPoolStats::default()
		};
		Self {
			frames: (0..capacity)
				.map(|_| FrameState::Free {
					page: Arc::new(Box::new(AlignedPage::zeroed())),
				})
				.collect(),
			entries: RapidHashMap::with_capacity(capacity),
			ghost_queue: VecDeque::with_capacity(capacity),
			ghost_set: RapidHashSet::with_capacity(capacity),
			hand: 0,
			hot_count: 0,
			hot_target: capacity / 2,
			stats,
		}
	}

	/// Resolves a mapped frame to either a resident hit or a coalesced wait.
	fn access_resident(&mut self, frame_index: usize) -> FerrosGrynnResult<AccessDecision> {
		match &mut self.frames[frame_index] {
			FrameState::Loading {
				signal,
				..
			} => {
				self.stats.coalesced_misses += 1;
				Ok(AccessDecision::Wait(Arc::clone(signal)))
			}
			FrameState::Resident {
				page,
				referenced,
				..
			} => {
				*referenced = true;
				self.stats.hits += 1;
				Ok(AccessDecision::Ready(Arc::clone(page)))
			}
			FrameState::Free {
				..
			} => Err(FerrosGrynnError::storage_io_failure(
				"buffer-pool entry pointed to a free frame",
			)),
		}
	}

	/// Reserves a free or evictable frame for one new physical load.
	fn reserve(&mut self, key: PageCacheKey) -> Option<AccessDecision> {
		let frame_index = self.find_available_frame()?;
		let temperature = if self.remove_ghost(key) {
			self.hot_target = (self.hot_target + 1).min(self.frames.len());
			Temperature::Hot
		} else {
			Temperature::Cold
		};
		let signal = Arc::new(LoadSignal::new());
		let previous = std::mem::replace(&mut self.frames[frame_index], FrameState::Loading {
			key,
			temperature,
			signal: Arc::clone(&signal),
		});
		let page = match previous {
			FrameState::Free {
				page,
			} => page,
			FrameState::Resident {
				key: evicted_key,
				page,
				temperature: evicted_temperature,
				..
			} => {
				self.entries.remove(&evicted_key);
				if evicted_temperature == Temperature::Hot {
					self.hot_count -= 1;
				}
				self.add_ghost(evicted_key);
				self.stats.evictions += 1;
				page
			}
			loading @ FrameState::Loading {
				..
			} => {
				self.frames[frame_index] = loading;
				return None;
			}
		};
		self.entries.insert(key, frame_index);
		self.stats.misses += 1;
		Some(AccessDecision::Load {
			frame_index,
			signal,
			page,
		})
	}

	/// Returns the reserved temperature only while the exact load is active.
	fn loading_temperature(&self, frame_index: usize, key: PageCacheKey) -> Option<Temperature> {
		match self.frames.get(frame_index) {
			Some(FrameState::Loading {
				key: loading_key,
				temperature,
				..
			}) if *loading_key == key => Some(*temperature),
			Some(_) | None => None,
		}
	}

	/// Publishes a page after [`Self::loading_temperature`] verified the same
	/// reservation while the caller controlled metadata transitions.
	fn publish_reserved(
		&mut self,
		frame_index: usize,
		key: PageCacheKey,
		page: CachedPage,
		temperature: Temperature,
	) {
		if temperature == Temperature::Hot {
			self.hot_count += 1;
		}
		self.frames[frame_index] = FrameState::Resident {
			key,
			page,
			temperature,
			referenced: false,
		};
	}

	/// Releases a loading frame after an error.
	fn abort(&mut self, frame_index: usize, key: PageCacheKey, page: CachedPage) {
		if matches!(self.frames.get(frame_index), Some(FrameState::Loading { key: loading, .. }) if *loading == key)
		{
			self.frames[frame_index] = FrameState::Free {
				page,
			};
			self.entries.remove(&key);
		}
	}

	/// Finds one free or unpinned resident frame using Clock-Pro transitions.
	fn find_available_frame(&mut self) -> Option<usize> {
		if self.entries.len() < self.frames.len() {
			for _ in 0..self.frames.len() {
				let frame_index = self.advance_hand();
				if matches!(self.frames[frame_index], FrameState::Free { .. }) {
					return Some(frame_index);
				}
			}
		}

		for _ in 0..CLOCK_SCAN_PASSES {
			for _ in 0..self.frames.len() {
				let frame_index = self.advance_hand();
				let mut balance_hot = false;
				match &mut self.frames[frame_index] {
					FrameState::Free {
						..
					} => return Some(frame_index),
					FrameState::Loading {
						..
					} => {}
					FrameState::Resident {
						page,
						temperature,
						referenced,
						..
					} => match *temperature {
						Temperature::Hot => {
							if *referenced {
								*referenced = false;
							} else {
								*temperature = Temperature::Cold;
								self.hot_count -= 1;
							}
						}
						Temperature::Cold => {
							if *referenced {
								*referenced = false;
								*temperature = Temperature::Hot;
								self.hot_count += 1;
								balance_hot = true;
							} else if Arc::strong_count(page) == 1 {
								return Some(frame_index);
							}
						}
					},
				}
				if balance_hot {
					self.balance_hot_target();
				}
			}
		}
		None
	}

	/// Demotes unreferenced Hot pages until the adaptive target is met.
	fn balance_hot_target(&mut self) {
		if self.hot_count <= self.hot_target {
			return;
		}
		for frame in &mut self.frames {
			if self.hot_count <= self.hot_target {
				break;
			}
			if let FrameState::Resident {
				temperature,
				referenced,
				..
			} = frame && *temperature == Temperature::Hot
				&& !*referenced
			{
				*temperature = Temperature::Cold;
				self.hot_count -= 1;
			}
		}
	}

	/// Advances the clock hand and returns its previous frame index.
	fn advance_hand(&mut self) -> usize {
		let frame_index = self.hand;
		self.hand = if self.hand + 1 == self.frames.len() {
			0
		} else {
			self.hand + 1
		};
		frame_index
	}

	/// Inserts one evicted key into bounded FIFO ghost history.
	fn add_ghost(&mut self, key: PageCacheKey) {
		if self.ghost_queue.len() == self.frames.len()
			&& let Some(oldest) = self.ghost_queue.pop_front()
		{
			self.ghost_set.remove(&oldest);
		}
		self.ghost_queue.push_back(key);
		self.ghost_set.insert(key);
	}

	/// Removes and reports one key from the ghost history.
	fn remove_ghost(&mut self, key: PageCacheKey) -> bool {
		if !self.ghost_set.remove(&key) {
			return false;
		}
		self.ghost_queue.retain(|candidate| *candidate != key);
		true
	}
}

/// Ensures a loader returned the immutable page version named by its key.
fn validate_loaded_header(key: PageCacheKey, header: PageHeader) -> FerrosGrynnResult<()> {
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
		io,
		num::NonZeroUsize,
		sync::{
			Arc,
			atomic::{AtomicUsize, Ordering},
			mpsc,
		},
		thread,
		time::{Duration, Instant},
	};

	use super::*;
	use crate::storage::{
		page::{PAGE_HEADER_SIZE, PageFlags, PageType},
		types::{DataFileId, DataFileKind, GraphId},
	};

	/// Data file used by deterministic unit tests.
	const TEST_FILE_ID: DataFileId = DataFileId::for_kind(DataFileKind::SystemCatalog);
	/// Maximum time allowed for a deterministic inter-thread handoff.
	const TEST_TIMEOUT: Duration = Duration::from_secs(2);

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
