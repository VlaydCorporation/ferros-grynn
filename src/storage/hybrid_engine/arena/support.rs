//! Frame metadata, directory entries, and batch-reservation support.

use std::{
	cell::UnsafeCell,
	sync::{
		Arc,
		atomic::{AtomicU64, AtomicUsize, Ordering},
	},
};

use parking_lot::{Condvar, Mutex, RwLock};
use rapidhash::{HashMapExt, RapidHashMap};

use super::{
	AtomicArenaBufferPool,
	AtomicPageGuard,
	Lifecycle,
	directory_shard,
	locator::FrameLocator,
	pack_state,
};
use crate::{
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	storage::{
		page::{AlignedPage, PageHeader},
		page_cache::{PageCacheKey, validate_loaded_header},
	},
};

/// One immutable directory value.
pub(super) enum DirectoryEntry {
	/// In-flight load shared by concurrent misses.
	Loading {
		/// Reserved frame generation.
		locator: FrameLocator,
		/// Result notification for coalesced callers.
		signal: Arc<LoadSignal>,
	},
	/// Fully validated and atomically published frame.
	Resident(FrameLocator),
}

impl DirectoryEntry {
	/// Copies the data needed after releasing the directory shard lock.
	pub(super) fn snapshot(&self) -> DirectoryLookup {
		match self {
			Self::Loading {
				signal,
				..
			} => DirectoryLookup::Loading(Arc::clone(signal)),
			Self::Resident(locator) => DirectoryLookup::Resident(*locator),
		}
	}

	/// Returns whether this is the exact in-flight load owned by a reservation.
	pub(super) fn is_loading(&self, locator: FrameLocator, signal: &Arc<LoadSignal>) -> bool {
		match self {
			Self::Loading {
				locator: current,
				signal: current_signal,
			} => *current == locator && Arc::ptr_eq(current_signal, signal),
			Self::Resident(_) => false,
		}
	}

	/// Returns whether this is the exact resident frame selected for removal.
	pub(super) fn is_resident(&self, locator: FrameLocator) -> bool {
		matches!(self, Self::Resident(current) if *current == locator)
	}
}

/// Owned snapshot of one directory lookup after its shard lock is released.
pub(super) enum DirectoryLookup {
	/// No exact key is mapped.
	Absent,
	/// A load is in progress and may be awaited without a directory lock.
	Loading(Arc<LoadSignal>),
	/// A resident locator may be atomically pinned.
	Resident(FrameLocator),
}

/// One independently locked, cache-line-separated directory partition.
#[repr(align(64))]
pub(super) struct DirectoryShard {
	/// Exact key-to-frame mapping.
	pub(super) entries: RwLock<RapidHashMap<PageCacheKey, DirectoryEntry>>,
}

impl DirectoryShard {
	/// Creates an empty directory partition.
	pub(super) fn new() -> Self {
		Self {
			entries: RwLock::new(RapidHashMap::new()),
		}
	}
}

/// Cache-line-separated atomic lifecycle metadata for one page frame.
#[repr(align(64))]
pub(super) struct FrameMeta {
	/// Packed lifecycle, generation, pin count, and usage count.
	pub(super) state: AtomicU64,
}

impl FrameMeta {
	/// Creates a never-used free frame.
	pub(super) const fn new() -> Self {
		Self {
			state: AtomicU64::new(pack_state(Lifecycle::Free, 0, 0, 0)),
		}
	}
}

/// Stable aligned bytes whose mutation is controlled by [`FrameMeta`].
pub(super) struct FrameBytes {
	/// Page allocation written only in `Loading` and read only while pinned.
	pub(super) page: UnsafeCell<AlignedPage>,
}

impl FrameBytes {
	/// Creates a zero-filled Direct-I/O-aligned frame.
	pub(super) const fn new() -> Self {
		Self {
			page: UnsafeCell::new(AlignedPage::zeroed()),
		}
	}
}

// SAFETY: `page` is mutated only by the unique owner of a `Loading` frame.
// A Release publication changes that frame to `Resident`; readers obtain an
// immutable reference only after an Acquire pin CAS or a counted read epoch
// validates the same generation. Replacement requires pin count zero and
// defers reuse of `Evicting` bytes until every read epoch exits.
unsafe impl Sync for FrameBytes {}

/// Clock hand and frame-to-key ownership protected only on miss/eviction.
pub(super) struct ReplacementState {
	/// Next frame inspected by the clock sweep.
	pub(super) hand: usize,
	/// Exact key currently owning each non-free frame.
	pub(super) keys: Box<[Option<PageCacheKey>]>,
}

/// Result shared with callers coalesced on one miss.
pub(super) struct LoadSignal {
	/// Completed load result, absent until publication or abort.
	result: Mutex<Option<FerrosGrynnResult<()>>>,
	/// Wait queue for completion.
	ready: Condvar,
}

impl LoadSignal {
	/// Creates an incomplete load notification.
	pub(super) fn new() -> Self {
		Self {
			result: Mutex::new(None),
			ready: Condvar::new(),
		}
	}

	/// Waits without holding directory or replacement locks.
	pub(super) fn wait(&self) -> FerrosGrynnResult<()> {
		let mut result = self.result.lock();
		while result.is_none() {
			self.ready.wait(&mut result);
		}
		match result.as_ref() {
			Some(completed) => completed.clone(),
			None => Err(FerrosGrynnError::storage_io_failure(
				"atomic-arena load signal woke without a result",
			)),
		}
	}

	/// Publishes exactly one terminal result and wakes every coalesced caller.
	pub(super) fn complete(&self, completed: FerrosGrynnResult<()>) {
		let mut result = self.result.lock();
		if result.is_none() {
			*result = Some(completed);
			self.ready.notify_all();
		}
	}
}

/// Reservation returned to the only caller allowed to fill one frame.
pub(super) struct LoadReservation {
	/// Exact key installed as a loading directory entry.
	pub(super) key: PageCacheKey,
	/// Reserved frame generation.
	pub(super) locator: FrameLocator,
	/// Completion notification stored in the directory entry.
	pub(super) signal: Arc<LoadSignal>,
}

/// RAII group of disjoint loading frames for direct-to-frame transports.
pub struct AtomicBatchReservation<'pool> {
	/// Arena that owns every loading frame.
	pub(super) pool: &'pool AtomicArenaBufferPool,
	/// Exact loading reservations in caller order.
	pub(super) reservations: Vec<LoadReservation>,
	/// Whether every frame was published together.
	pub(super) published: bool,
}

impl<'pool> AtomicBatchReservation<'pool> {
	/// Returns disjoint writable aligned frames in key order.
	///
	/// The returned borrows end before [`Self::publish`] can consume the batch.
	#[allow(
		clippy::needless_pass_by_ref_mut,
		reason = "exclusive receiver prevents repeated calls from creating aliased mutable frame \
		          references"
	)]
	pub fn pages_mut(&mut self) -> Vec<&mut AlignedPage> {
		let mut pages = Vec::with_capacity(self.reservations.len());
		for reservation in &self.reservations {
			let frame = &self.pool.frames[reservation.locator.index];
			// SAFETY: `reserve_batch` rejects duplicate keys and every
			// successful miss reservation owns a distinct `Loading` frame.
			// This batch is the sole load owner, so these mutable references
			// are pairwise disjoint.
			pages.push(unsafe { &mut *frame.page.get() });
		}
		pages
	}

	/// Validates every frame, then publishes the entire extent without a
	/// fallible step between individual frame publications.
	///
	/// # Errors
	///
	/// Returns an exact validation or lifecycle preflight error. Drop rolls all
	/// unpublished reservations back.
	pub fn publish(mut self) -> FerrosGrynnResult<Vec<AtomicPageGuard<'pool>>> {
		for reservation in &self.reservations {
			let frame = &self.pool.frames[reservation.locator.index];
			// SAFETY: `publish` consumes the reservation after every mutable
			// page borrow ended. The exact frame is still `Loading`, so this
			// owner is the only code that may inspect its initialized bytes.
			let page = unsafe { &*frame.page.get() };
			let actual = page.validate(reservation.key.page_no())?;
			validate_loaded_header(reservation.key, actual)?;
		}

		let replacement = self.pool.replacement.lock();
		for reservation in &self.reservations {
			let expected = pack_state(Lifecycle::Loading, reservation.locator.generation, 0, 0);
			if self.pool.metadata[reservation.locator.index].state.load(Ordering::Acquire)
				!= expected
			{
				return Err(FerrosGrynnError::storage_io_failure(
					"atomic batch loading state changed before publication",
				));
			}
			let shard = &self.pool.directory[directory_shard(reservation.key)];
			let entries = shard.entries.read();
			let publishable = entries
				.get(&reservation.key)
				.is_some_and(|entry| entry.is_loading(reservation.locator, &reservation.signal));
			if !publishable {
				return Err(FerrosGrynnError::storage_io_failure(
					"atomic batch loading mapping changed before publication",
				));
			}
		}

		for reservation in &self.reservations {
			self.pool.metadata[reservation.locator.index].state.store(
				pack_state(Lifecycle::Resident, reservation.locator.generation, 1, 1),
				Ordering::Release,
			);
			let shard = &self.pool.directory[directory_shard(reservation.key)];
			shard
				.entries
				.write()
				.insert(reservation.key, DirectoryEntry::Resident(reservation.locator));
		}
		self.published = true;
		drop(replacement);
		for reservation in &self.reservations {
			reservation.signal.complete(Ok(()));
		}
		Ok(self
			.reservations
			.iter()
			.map(|reservation| self.pool.guard(reservation.locator))
			.collect())
	}
}

impl Drop for AtomicBatchReservation<'_> {
	/// Aborts every frame unless all were published together.
	fn drop(&mut self) {
		if self.published {
			return;
		}
		for reservation in &self.reservations {
			self.pool.abort_load(
				reservation,
				FerrosGrynnError::storage_io_failure("atomic batch reservation was cancelled"),
			);
		}
	}
}

/// RAII registration that closes the capacity lost-wakeup window.
pub(super) struct CapacityWaiter<'counter> {
	/// Waiter counter consulted by the last guard drop.
	counter: &'counter AtomicUsize,
}

impl<'counter> CapacityWaiter<'counter> {
	/// Registers before capacity is rechecked under the replacement lock.
	pub(super) fn new(counter: &'counter AtomicUsize) -> Self {
		counter.fetch_add(1, Ordering::SeqCst);
		Self {
			counter,
		}
	}
}

impl Drop for CapacityWaiter<'_> {
	/// Unregisters on every success and error return.
	fn drop(&mut self) {
		self.counter.fetch_sub(1, Ordering::SeqCst);
	}
}

/// Result of serialized miss admission.
pub(super) enum ReserveDecision {
	/// Another caller published a resident mapping before replacement locked.
	Resident(FrameLocator),
	/// Another caller started a load before replacement locked.
	Loading(Arc<LoadSignal>),
	/// This caller owns the new loading frame.
	Reserved(LoadReservation),
}

/// Evidence returned by the unique frame writer before authoritative arena
/// validation.
pub(super) enum LoadReceipt {
	/// A normal I/O loader already parsed the header it observed. The arena
	/// revalidates current bytes and requires the two observations to match.
	Reported(PageHeader),
	/// Bytes were copied from an already validated resident frame. The copy
	/// path deliberately does not parse the destination before arena
	/// validation.
	Copied,
}

/// Result of attempting to pin one packed resident state.
pub(super) enum PinResult {
	/// Pin succeeded for this exact frame generation.
	Pinned,
	/// Frame lifecycle or generation no longer matches the locator.
	Stale,
	/// The packed pin counter cannot represent another simultaneous guard.
	Exhausted,
}
