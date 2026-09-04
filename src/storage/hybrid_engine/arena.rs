//! Lock-partitioned, fixed-arena resident-page prototype.

#[cfg(debug_assertions)]
use std::cell::Cell;
use std::{
	cell::UnsafeCell,
	num::NonZeroUsize,
	sync::{
		Arc,
		atomic::{AtomicU64, AtomicUsize, Ordering},
	},
};

use parking_lot::{Condvar, Mutex, RwLock};
use rapidhash::{HashMapExt, RapidHashMap};

use super::{SourceEpoch, SourceIdentity};
use crate::{
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	storage::{
		page::{AlignedPage, PAGE_SIZE, PageHeader},
		page_cache::{PageCacheKey, PageCacheVersion, validate_loaded_header},
	},
};

/// Number of independently locked directory partitions.
const DIRECTORY_SHARDS: usize = 64;
/// Maximum number of full clock passes before the caller sleeps for capacity.
const CLOCK_SCAN_PASSES: usize = 4;
/// Unique identity source preventing resolved handles from crossing arenas.
static NEXT_ARENA_ID: AtomicU64 = AtomicU64::new(1);

#[cfg(debug_assertions)]
std::thread_local! {
	/// Number of authoritative single-frame validations on the current test
	/// thread. Release benchmark builds compile this diagnostic state out.
	static PUBLICATION_VALIDATION_COUNT: Cell<usize> = const { Cell::new(0) };
}

/// Returns the current debug thread's authoritative publication validations.
#[cfg(debug_assertions)]
pub(super) fn publication_validation_count() -> usize {
	PUBLICATION_VALIDATION_COUNT.with(Cell::get)
}

/// Packed-state lifecycle field width.
const LIFECYCLE_BITS: u32 = 2;
/// Packed-state pin field width.
const PIN_BITS: u32 = 20;
/// Packed-state usage field width.
const USAGE_BITS: u32 = 3;
/// Packed-state generation field width.
const GENERATION_BITS: u32 = u64::BITS - LIFECYCLE_BITS - PIN_BITS - USAGE_BITS;
/// Bit offset of the pin count.
const PIN_SHIFT: u32 = LIFECYCLE_BITS;
/// Bit offset of the usage count.
const USAGE_SHIFT: u32 = PIN_SHIFT + PIN_BITS;
/// Bit offset of the generation.
const GENERATION_SHIFT: u32 = USAGE_SHIFT + USAGE_BITS;
/// Lifecycle mask in a packed state word.
const LIFECYCLE_MASK: u64 = (1_u64 << LIFECYCLE_BITS) - 1;
/// Pin-count mask in a packed state word.
const PIN_MASK: u64 = ((1_u64 << PIN_BITS) - 1) << PIN_SHIFT;
/// Usage-count mask in a packed state word.
const USAGE_MASK: u64 = ((1_u64 << USAGE_BITS) - 1) << USAGE_SHIFT;
/// Largest representable pin count.
const MAX_PINS: u64 = (1_u64 << PIN_BITS) - 1;
/// Largest representable usage count.
const MAX_USAGE: u64 = (1_u64 << USAGE_BITS) - 1;
/// Largest non-wrapping frame generation.
const MAX_GENERATION: u64 = (1_u64 << GENERATION_BITS) - 1;
/// Frame-index bits retained in one compact source-bound locator.
const DENSE_LOCATOR_INDEX_BITS: u32 = 24;
/// Largest frame index retained in one compact source-bound locator.
const DENSE_LOCATOR_INDEX_MASK: u64 = (1_u64 << DENSE_LOCATOR_INDEX_BITS) - 1;
/// Presence bit distinguishing an empty dense slot from generation zero.
const DENSE_LOCATOR_VALID: u64 = 1_u64 << 63;
/// Slots allocated together by the sparse two-level locator.
const DENSE_LOCATOR_LEAF_SLOTS: usize = 1_024;

/// Lifecycle of one arena frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u64)]
enum Lifecycle {
	/// Frame is not mapped and may be reserved.
	Free = 0,
	/// One owner is filling the frame and no reader may access its bytes.
	Loading = 1,
	/// Immutable bytes are published and may be pinned by readers.
	Resident = 2,
	/// Replacement owns the unpinned frame while removing its mapping.
	Evicting = 3,
}

/// Stable locator protected against frame reuse by a generation number.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FrameLocator {
	/// Index into the fixed page and metadata arenas.
	index: usize,
	/// Generation captured when the frame was reserved.
	generation: u64,
}

/// Cached resolution of one exact immutable page version.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResolvedFrameHandle {
	/// Identity of the arena that produced this locator.
	arena_id: u64,
	/// Complete key used for stale fallback.
	key: PageCacheKey,
	/// Arena frame and generation observed during resolution.
	locator: FrameLocator,
}

impl ResolvedFrameHandle {
	/// Creates a resolved handle from a resident directory entry.
	const fn new(arena_id: u64, key: PageCacheKey, locator: FrameLocator) -> Self {
		Self {
			arena_id,
			key,
			locator,
		}
	}

	/// Confirms that the caller still requests the exact resolved source epoch.
	fn matches(&self, key: PageCacheKey) -> bool {
		self.key == key
	}

	/// Returns the exact page version captured by this resolution.
	pub(super) const fn key(&self) -> PageCacheKey {
		self.key
	}

	/// Returns the arena that produced this immutable locator.
	pub(super) const fn arena_id(&self) -> u64 {
		self.arena_id
	}
}

/// Source-bound translation from page number to arena frame generation.
///
/// The source and epoch are stored once rather than repeated in every slot.
/// A stale slot is harmless: the arena generation check rejects it before any
/// page bytes are exposed.
pub struct SourceBoundLocator {
	/// Arena that produced every stored frame locator.
	arena_id: u64,
	/// Stable physical source identity.
	source: SourceIdentity,
	/// Exact immutable source version.
	epoch: SourceEpoch,
	/// Number of addressable pages in this source.
	page_count: usize,
	/// Lazily allocated page-number chunks.
	chunks: RwLock<RapidHashMap<usize, Arc<LocatorChunk>>>,
	/// Live slots owned by this source epoch.
	entries: AtomicUsize,
	/// Arena-wide live locator slots across all source epochs.
	global_entries: Arc<AtomicUsize>,
	/// Maximum arena-wide live locator slots.
	entry_budget: usize,
}

/// One removable page-number chunk.
struct LocatorChunk {
	/// Compact frame locators for consecutive source pages.
	slots: Box<[AtomicU64]>,
	/// Nonempty slots in this chunk.
	live: AtomicUsize,
	/// Serializes slot-count changes outside the page hot loop.
	updates: Mutex<()>,
}

impl LocatorChunk {
	/// Allocates one zeroed chunk.
	fn new() -> Self {
		Self {
			slots: (0..DENSE_LOCATOR_LEAF_SLOTS)
				.map(|_| AtomicU64::new(0))
				.collect::<Vec<_>>()
				.into_boxed_slice(),
			live: AtomicUsize::new(0),
			updates: Mutex::new(()),
		}
	}
}

impl SourceBoundLocator {
	/// Creates an empty locator for one exact source epoch.
	///
	/// # Errors
	///
	/// Rejects an arena whose frame indices cannot fit the compact locator.
	pub fn new(
		pool: &AtomicArenaBufferPool,
		first_key: PageCacheKey,
		page_count: NonZeroUsize,
	) -> FerrosGrynnResult<Self> {
		if pool.capacity().saturating_sub(1) as u64 > DENSE_LOCATOR_INDEX_MASK {
			return Err(FerrosGrynnError::storage_io_failure(
				"atomic-arena capacity exceeds the compact dense-locator index",
			));
		}
		if !matches!(first_key.version(), PageCacheVersion::Home { .. }) {
			return Err(FerrosGrynnError::storage_io_failure(
				"persistent source-bound locators are only valid for home-file epochs",
			));
		}
		let page_count = page_count.get();
		Ok(Self {
			arena_id: pool.arena_id,
			source: SourceIdentity::from_key(first_key),
			epoch: SourceEpoch::from_key(first_key),
			page_count,
			chunks: RwLock::new(RapidHashMap::new()),
			entries: AtomicUsize::new(0),
			global_entries: Arc::clone(&pool.locator_entries),
			entry_budget: pool.capacity(),
		})
	}

	/// Refreshes one slot from the arena's authoritative directory.
	///
	/// Returns `false` when the key is outside this source epoch or is not
	/// resident. A nonresident exact slot is cleared.
	pub fn refresh(&self, pool: &AtomicArenaBufferPool, key: PageCacheKey) -> bool {
		let Some(index) = self.exact_index(pool, key) else {
			return false;
		};
		let Some(handle) = pool.resolve(key) else {
			self.clear_slot(index);
			return false;
		};
		self.store_locator(pool, index, handle.locator)
	}

	/// Resolves a current resident handle through this source-local table.
	///
	/// A stale compact slot is removed before the authoritative directory is
	/// consulted and refreshed.
	pub fn resolve(
		&self,
		pool: &AtomicArenaBufferPool,
		key: PageCacheKey,
	) -> Option<ResolvedFrameHandle> {
		let index = self.exact_index(pool, key)?;
		if let Some(locator) = self.load_locator(index) {
			if pool.is_locator_resident(locator) {
				return Some(ResolvedFrameHandle::new(pool.arena_id, key, locator));
			}
			self.clear_slot(index);
		}
		let handle = pool.resolve(key)?;
		self.store_locator(pool, index, handle.locator);
		Some(handle)
	}

	/// Pins a resident page through one compact source-local lookup.
	///
	/// # Errors
	///
	/// Returns an internal storage error only when the packed pin counter is
	/// exhausted.
	pub fn get_resolved<'pool>(
		&self,
		pool: &'pool AtomicArenaBufferPool,
		key: PageCacheKey,
	) -> FerrosGrynnResult<Option<AtomicPageGuard<'pool>>> {
		let Some(index) = self.exact_index(pool, key) else {
			return Ok(None);
		};
		let Some(locator) = self.load_locator(index) else {
			return Ok(None);
		};
		match pool.try_pin(locator) {
			PinResult::Pinned => Ok(Some(pool.guard(locator))),
			PinResult::Stale => {
				self.clear_slot(index);
				Ok(None)
			}
			PinResult::Exhausted => Err(pin_exhausted(key)),
		}
	}

	/// Borrows a resident page through one compact source-local lookup while
	/// the caller's read epoch prevents frame-byte reuse.
	pub fn get_epoch_resolved<'epoch>(
		&self,
		epoch: &'epoch AtomicReadEpoch<'_>,
		key: PageCacheKey,
	) -> Option<AtomicEpochPage<'epoch>> {
		let index = self.exact_index(epoch.pool, key)?;
		let locator = self.load_locator(index)?;
		let page = epoch.get_locator(locator);
		if page.is_none() {
			self.clear_slot(index);
		}
		page
	}

	/// Uses a compact slot when valid and refreshes it through the normal miss
	/// path after eviction or frame reuse.
	///
	/// # Errors
	///
	/// Rejects a different source epoch and otherwise returns the same errors
	/// as [`AtomicArenaBufferPool::get_or_load`].
	pub fn get_or_load<'pool, Loader>(
		&self,
		pool: &'pool AtomicArenaBufferPool,
		key: PageCacheKey,
		loader: Loader,
	) -> FerrosGrynnResult<AtomicPageGuard<'pool>>
	where
		Loader: FnOnce(&mut AlignedPage) -> FerrosGrynnResult<PageHeader>,
	{
		let Some(index) = self.exact_index(pool, key) else {
			return Err(FerrosGrynnError::invalid_storage_page(
				key.page_no().get(),
				"source-bound locator received a different source epoch or page range",
			));
		};
		if let Some(locator) = self.load_locator(index) {
			match pool.try_pin(locator) {
				PinResult::Pinned => return Ok(pool.guard(locator)),
				PinResult::Stale => self.clear_slot(index),
				PinResult::Exhausted => return Err(pin_exhausted(key)),
			}
		}
		let guard = pool.get_or_load(key, loader)?;
		self.store_locator(pool, index, guard.locator);
		Ok(guard)
	}

	/// Returns the number of live cached locators for this source epoch.
	pub fn entry_count(&self) -> usize {
		self.entries.load(Ordering::Acquire)
	}

	/// Returns the number of allocated nonempty chunks.
	pub fn chunk_count(&self) -> usize {
		self.chunks.read().len()
	}

	/// Returns the source page range accepted by this locator.
	pub const fn page_count(&self) -> usize {
		self.page_count
	}

	/// Returns whether a key belongs to this arena, source epoch, and page
	/// range.
	pub fn accepts(&self, pool: &AtomicArenaBufferPool, key: PageCacheKey) -> bool {
		self.exact_index(pool, key).is_some()
	}

	/// Returns the exact page-number slot only for this arena and source epoch.
	fn exact_index(&self, pool: &AtomicArenaBufferPool, key: PageCacheKey) -> Option<usize> {
		if pool.arena_id != self.arena_id
			|| SourceIdentity::from_key(key) != self.source
			|| SourceEpoch::from_key(key) != self.epoch
		{
			return None;
		}
		let index = usize::try_from(key.page_no().get()).ok()?;
		(index < self.page_count).then_some(index)
	}

	/// Loads and decodes one compact locator slot.
	fn load_locator(&self, index: usize) -> Option<FrameLocator> {
		let chunk_index = index / DENSE_LOCATOR_LEAF_SLOTS;
		let slot_index = index % DENSE_LOCATOR_LEAF_SLOTS;
		let chunks = self.chunks.read();
		let chunk = chunks.get(&chunk_index)?;
		unpack_dense_locator(chunk.slots[slot_index].load(Ordering::Acquire))
	}

	/// Stores one resident locator while enforcing the shared locator budget.
	fn store_locator(
		&self,
		pool: &AtomicArenaBufferPool,
		index: usize,
		locator: FrameLocator,
	) -> bool {
		let chunk_index = index / DENSE_LOCATOR_LEAF_SLOTS;
		let slot_index = index % DENSE_LOCATOR_LEAF_SLOTS;
		let packed = pack_dense_locator(locator);

		// Updating an existing slot needs no additional budget. The chunk read
		// lock prevents removal while its update lock serializes the slot
		// count.
		{
			let chunks = self.chunks.read();
			if let Some(chunk) = chunks.get(&chunk_index) {
				let _updates = chunk.updates.lock();
				let slot = &chunk.slots[slot_index];
				if slot.load(Ordering::Acquire) != 0 {
					slot.store(packed, Ordering::Release);
					return true;
				}
			}
		}

		if !self.reserve_entry(pool) {
			return false;
		}
		{
			let chunks = self.chunks.read();
			if let Some(chunk) = chunks.get(&chunk_index) {
				let _updates = chunk.updates.lock();
				let previous = chunk.slots[slot_index].swap(packed, Ordering::AcqRel);
				if previous == 0 {
					chunk.live.fetch_add(1, Ordering::Release);
					self.entries.fetch_add(1, Ordering::Release);
				} else {
					self.global_entries.fetch_sub(1, Ordering::AcqRel);
				}
				return true;
			}
		}

		let mut chunks = self.chunks.write();
		let chunk =
			Arc::clone(chunks.entry(chunk_index).or_insert_with(|| Arc::new(LocatorChunk::new())));
		let _updates = chunk.updates.lock();
		let previous = chunk.slots[slot_index].swap(packed, Ordering::AcqRel);
		if previous == 0 {
			chunk.live.fetch_add(1, Ordering::Release);
			self.entries.fetch_add(1, Ordering::Release);
		} else {
			self.global_entries.fetch_sub(1, Ordering::AcqRel);
		}
		true
	}

	/// Reserves one arena-wide locator entry, pruning stale local slots first.
	fn reserve_entry(&self, pool: &AtomicArenaBufferPool) -> bool {
		if self
			.global_entries
			.fetch_update(Ordering::AcqRel, Ordering::Acquire, |entries| {
				(entries < self.entry_budget).then_some(entries + 1)
			})
			.is_ok()
		{
			return true;
		}
		self.prune_stale(pool);
		self.global_entries
			.fetch_update(Ordering::AcqRel, Ordering::Acquire, |entries| {
				(entries < self.entry_budget).then_some(entries + 1)
			})
			.is_ok()
	}

	/// Clears one stale slot and removes its chunk after the last entry.
	fn clear_slot(&self, index: usize) {
		let chunk_index = index / DENSE_LOCATOR_LEAF_SLOTS;
		let slot_index = index % DENSE_LOCATOR_LEAF_SLOTS;
		let chunks = self.chunks.read();
		let Some(chunk) = chunks.get(&chunk_index) else {
			return;
		};
		let chunk = Arc::clone(chunk);
		let _updates = chunk.updates.lock();
		if chunk.slots[slot_index].swap(0, Ordering::AcqRel) == 0 {
			return;
		}
		chunk.live.fetch_sub(1, Ordering::AcqRel);
		self.entries.fetch_sub(1, Ordering::AcqRel);
		self.global_entries.fetch_sub(1, Ordering::AcqRel);
		drop(_updates);
		drop(chunks);
		self.remove_empty_chunk(chunk_index, &chunk);
	}

	/// Clears stale generations without touching live resident frames.
	fn prune_stale(&self, pool: &AtomicArenaBufferPool) {
		let chunks = self
			.chunks
			.read()
			.iter()
			.map(|(index, chunk)| (*index, Arc::clone(chunk)))
			.collect::<Vec<_>>();
		for (chunk_index, chunk) in chunks {
			let updates = chunk.updates.lock();
			for slot in &chunk.slots {
				let raw = slot.load(Ordering::Acquire);
				let Some(locator) = unpack_dense_locator(raw) else {
					continue;
				};
				if pool.is_locator_resident(locator)
					|| slot.compare_exchange(raw, 0, Ordering::AcqRel, Ordering::Acquire).is_err()
				{
					continue;
				}
				chunk.live.fetch_sub(1, Ordering::AcqRel);
				self.entries.fetch_sub(1, Ordering::AcqRel);
				self.global_entries.fetch_sub(1, Ordering::AcqRel);
			}
			drop(updates);
			self.remove_empty_chunk(chunk_index, &chunk);
		}
	}

	/// Removes an exact empty chunk without racing a concurrent insertion.
	fn remove_empty_chunk(&self, chunk_index: usize, chunk: &Arc<LocatorChunk>) {
		if chunk.live.load(Ordering::Acquire) != 0 {
			return;
		}
		let mut chunks = self.chunks.write();
		let remove = chunks.get(&chunk_index).is_some_and(|current| Arc::ptr_eq(current, chunk))
			&& chunk.live.load(Ordering::Acquire) == 0;
		if remove {
			chunks.remove(&chunk_index);
		}
	}
}

impl Drop for SourceBoundLocator {
	/// Returns this source's remaining slots to the arena-wide metadata budget.
	fn drop(&mut self) {
		self.global_entries.fetch_sub(self.entries.load(Ordering::Acquire), Ordering::AcqRel);
	}
}

/// Packs one arena locator while preserving every frame-generation bit.
fn pack_dense_locator(locator: FrameLocator) -> u64 {
	debug_assert!(locator.index as u64 <= DENSE_LOCATOR_INDEX_MASK);
	debug_assert!(locator.generation <= MAX_GENERATION);
	DENSE_LOCATOR_VALID | (locator.generation << DENSE_LOCATOR_INDEX_BITS) | locator.index as u64
}

/// Decodes one nonempty compact arena locator.
fn unpack_dense_locator(raw: u64) -> Option<FrameLocator> {
	(raw & DENSE_LOCATOR_VALID != 0).then_some(FrameLocator {
		index: (raw & DENSE_LOCATOR_INDEX_MASK) as usize,
		generation: (raw & !DENSE_LOCATOR_VALID) >> DENSE_LOCATOR_INDEX_BITS,
	})
}

/// One immutable directory value.
enum DirectoryEntry {
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
	fn snapshot(&self) -> DirectoryLookup {
		match self {
			Self::Loading {
				signal,
				..
			} => DirectoryLookup::Loading(Arc::clone(signal)),
			Self::Resident(locator) => DirectoryLookup::Resident(*locator),
		}
	}

	/// Returns whether this is the exact in-flight load owned by a reservation.
	fn is_loading(&self, locator: FrameLocator, signal: &Arc<LoadSignal>) -> bool {
		match self {
			Self::Loading {
				locator: current,
				signal: current_signal,
			} => *current == locator && Arc::ptr_eq(current_signal, signal),
			Self::Resident(_) => false,
		}
	}

	/// Returns whether this is the exact resident frame selected for removal.
	fn is_resident(&self, locator: FrameLocator) -> bool {
		matches!(self, Self::Resident(current) if *current == locator)
	}
}

/// Owned snapshot of one directory lookup after its shard lock is released.
enum DirectoryLookup {
	/// No exact key is mapped.
	Absent,
	/// A load is in progress and may be awaited without a directory lock.
	Loading(Arc<LoadSignal>),
	/// A resident locator may be atomically pinned.
	Resident(FrameLocator),
}

/// One independently locked, cache-line-separated directory partition.
#[repr(align(64))]
struct DirectoryShard {
	/// Exact key-to-frame mapping.
	entries: RwLock<RapidHashMap<PageCacheKey, DirectoryEntry>>,
}

impl DirectoryShard {
	/// Creates an empty directory partition.
	fn new() -> Self {
		Self {
			entries: RwLock::new(RapidHashMap::new()),
		}
	}
}

/// Cache-line-separated atomic lifecycle metadata for one page frame.
#[repr(align(64))]
struct FrameMeta {
	/// Packed lifecycle, generation, pin count, and usage count.
	state: AtomicU64,
}

impl FrameMeta {
	/// Creates a never-used free frame.
	const fn new() -> Self {
		Self {
			state: AtomicU64::new(pack_state(Lifecycle::Free, 0, 0, 0)),
		}
	}
}

/// Stable aligned bytes whose mutation is controlled by [`FrameMeta`].
struct FrameBytes {
	/// Page allocation written only in `Loading` and read only while pinned.
	page: UnsafeCell<AlignedPage>,
}

impl FrameBytes {
	/// Creates a zero-filled Direct-I/O-aligned frame.
	const fn new() -> Self {
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
struct ReplacementState {
	/// Next frame inspected by the clock sweep.
	hand: usize,
	/// Exact key currently owning each non-free frame.
	keys: Box<[Option<PageCacheKey>]>,
}

/// Result shared with callers coalesced on one miss.
struct LoadSignal {
	/// Completed load result, absent until publication or abort.
	result: Mutex<Option<FerrosGrynnResult<()>>>,
	/// Wait queue for completion.
	ready: Condvar,
}

impl LoadSignal {
	/// Creates an incomplete load notification.
	fn new() -> Self {
		Self {
			result: Mutex::new(None),
			ready: Condvar::new(),
		}
	}

	/// Waits without holding directory or replacement locks.
	fn wait(&self) -> FerrosGrynnResult<()> {
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
	fn complete(&self, completed: FerrosGrynnResult<()>) {
		let mut result = self.result.lock();
		if result.is_none() {
			*result = Some(completed);
			self.ready.notify_all();
		}
	}
}

/// Reservation returned to the only caller allowed to fill one frame.
struct LoadReservation {
	/// Exact key installed as a loading directory entry.
	key: PageCacheKey,
	/// Reserved frame generation.
	locator: FrameLocator,
	/// Completion notification stored in the directory entry.
	signal: Arc<LoadSignal>,
}

/// RAII group of disjoint loading frames for direct-to-frame transports.
pub struct AtomicBatchReservation<'pool> {
	/// Arena that owns every loading frame.
	pool: &'pool AtomicArenaBufferPool,
	/// Exact loading reservations in caller order.
	reservations: Vec<LoadReservation>,
	/// Whether every frame was published together.
	published: bool,
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
struct CapacityWaiter<'counter> {
	/// Waiter counter consulted by the last guard drop.
	counter: &'counter AtomicUsize,
}

impl<'counter> CapacityWaiter<'counter> {
	/// Registers before capacity is rechecked under the replacement lock.
	fn new(counter: &'counter AtomicUsize) -> Self {
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
enum ReserveDecision {
	/// Another caller published a resident mapping before replacement locked.
	Resident(FrameLocator),
	/// Another caller started a load before replacement locked.
	Loading(Arc<LoadSignal>),
	/// This caller owns the new loading frame.
	Reserved(LoadReservation),
}

/// Evidence returned by the unique frame writer before authoritative arena
/// validation.
enum LoadReceipt {
	/// A normal I/O loader already parsed the header it observed. The arena
	/// revalidates current bytes and requires the two observations to match.
	Reported(PageHeader),
	/// Bytes were copied from an already validated resident frame. The copy
	/// path deliberately does not parse the destination before arena
	/// validation.
	Copied,
}

/// Result of attempting to pin one packed resident state.
enum PinResult {
	/// Pin succeeded for this exact frame generation.
	Pinned,
	/// Frame lifecycle or generation no longer matches the locator.
	Stale,
	/// The packed pin counter cannot represent another simultaneous guard.
	Exhausted,
}

/// PostgreSQL-inspired fixed frame arena.
pub struct AtomicArenaBufferPool {
	/// Unique identity used only to reject cross-arena resolved handles.
	arena_id: u64,
	/// Contiguous stable page allocations.
	frames: Box<[FrameBytes]>,
	/// Separate cache-line-aligned atomic metadata.
	metadata: Box<[FrameMeta]>,
	/// Partitioned exact-key mapping index.
	directory: Box<[DirectoryShard]>,
	/// Miss-only clock hand and frame ownership.
	replacement: Mutex<ReplacementState>,
	/// Serializes construction of multi-frame reservations so two batches
	/// cannot each retain a partial allocation while waiting for the other.
	batch_admission: Mutex<()>,
	/// Wait queue used only when every candidate is pinned or loading.
	capacity_available: Condvar,
	/// Callers currently registered to wait for capacity.
	capacity_waiters: AtomicUsize,
	/// Cursor-scoped readers that may retain borrowed resident page addresses.
	active_read_epochs: AtomicUsize,
	/// Monotonic invalidation token for reusable resolved page tables.
	resolved_table_epoch: AtomicU64,
	/// Live source-locator entries, globally bounded by resident capacity.
	locator_entries: Arc<AtomicUsize>,
	/// Slow-path miss count.
	misses: AtomicU64,
	/// Slow-path coalesced miss count.
	coalesced_misses: AtomicU64,
	/// Slow-path eviction count.
	evictions: AtomicU64,
	/// Slow-path capacity-wait count.
	backpressure_waits: AtomicU64,
}

impl AtomicArenaBufferPool {
	/// Creates a fixed R8 arena without counters on the resident fast path.
	pub fn new(capacity: NonZeroUsize) -> Self {
		let capacity = capacity.get();
		let frames =
			(0..capacity).map(|_| FrameBytes::new()).collect::<Vec<_>>().into_boxed_slice();
		let metadata =
			(0..capacity).map(|_| FrameMeta::new()).collect::<Vec<_>>().into_boxed_slice();
		let directory = (0..DIRECTORY_SHARDS)
			.map(|_| DirectoryShard::new())
			.collect::<Vec<_>>()
			.into_boxed_slice();

		Self {
			arena_id: NEXT_ARENA_ID.fetch_add(1, Ordering::Relaxed),
			frames,
			metadata,
			directory,
			replacement: Mutex::new(ReplacementState {
				hand: 0,
				keys: vec![None; capacity].into_boxed_slice(),
			}),
			batch_admission: Mutex::new(()),
			capacity_available: Condvar::new(),
			capacity_waiters: AtomicUsize::new(0),
			active_read_epochs: AtomicUsize::new(0),
			resolved_table_epoch: AtomicU64::new(0),
			locator_entries: Arc::new(AtomicUsize::new(0)),
			misses: AtomicU64::new(0),
			coalesced_misses: AtomicU64::new(0),
			evictions: AtomicU64::new(0),
			backpressure_waits: AtomicU64::new(0),
		}
	}

	/// Returns the exact number of permanently allocated frames.
	pub const fn capacity(&self) -> usize {
		self.frames.len()
	}

	/// Returns the identity embedded in every resolved handle from this arena.
	pub(super) const fn arena_id(&self) -> u64 {
		self.arena_id
	}

	/// Enters one cursor-scoped read epoch.
	///
	/// # Errors
	///
	/// Returns an internal storage error if the reader counter is exhausted.
	pub fn read_epoch(&self) -> FerrosGrynnResult<AtomicReadEpoch<'_>> {
		self.active_read_epochs
			.fetch_update(Ordering::SeqCst, Ordering::SeqCst, |readers| readers.checked_add(1))
			.map_err(|_| {
				FerrosGrynnError::storage_io_failure("atomic-arena read-epoch counter is exhausted")
			})?;
		Ok(AtomicReadEpoch {
			pool: self,
		})
	}

	/// Returns the token changed before any resident frame becomes stale.
	pub(super) fn resolved_table_epoch(&self) -> u64 {
		self.resolved_table_epoch.load(Ordering::SeqCst)
	}

	/// Invalidates every reusable page table before removing a resident frame.
	fn invalidate_resolved_tables(&self) -> FerrosGrynnResult<()> {
		self.resolved_table_epoch
			.fetch_update(Ordering::SeqCst, Ordering::SeqCst, |epoch| epoch.checked_add(1))
			.map(|_| ())
			.map_err(|_| {
				FerrosGrynnError::storage_io_failure(
					"atomic-arena resolved-table epoch is exhausted",
				)
			})
	}

	/// Returns a validated pinned page, loading it on an exact-key miss.
	///
	/// # Errors
	///
	/// Returns the loader or page-validation error, or an internal storage
	/// error if the prototype lifecycle invariant is violated.
	pub fn get_or_load<Loader>(
		&self,
		key: PageCacheKey,
		loader: Loader,
	) -> FerrosGrynnResult<AtomicPageGuard<'_>>
	where
		Loader: FnOnce(&mut AlignedPage) -> FerrosGrynnResult<PageHeader>,
	{
		self.get_or_load_before_publish(key, loader, || Ok(()))
	}

	/// Loads a miss and invokes one owner-only hook after validation but before
	/// resident publication. Hits do not invoke the hook.
	///
	/// # Errors
	///
	/// Returns the loader, hook, page-validation, or lifecycle error.
	pub fn get_or_load_before_publish<Loader, BeforePublish>(
		&self,
		key: PageCacheKey,
		loader: Loader,
		before_publish: BeforePublish,
	) -> FerrosGrynnResult<AtomicPageGuard<'_>>
	where
		Loader: FnOnce(&mut AlignedPage) -> FerrosGrynnResult<PageHeader>,
		BeforePublish: FnOnce() -> FerrosGrynnResult<()>,
	{
		self.get_or_load_before_publish_inner(
			key,
			|page| loader(page).map(LoadReceipt::Reported),
			before_publish,
		)
	}

	/// Copies an already resident page into a hot-arena miss. The destination
	/// is parsed and checksummed only by authoritative arena publication.
	///
	/// # Errors
	///
	/// Returns a page-validation or lifecycle error.
	pub fn get_or_copy(
		&self,
		key: PageCacheKey,
		source: &AlignedPage,
	) -> FerrosGrynnResult<AtomicPageGuard<'_>> {
		self.get_or_load_before_publish_inner(
			key,
			|target| {
				target.as_bytes_mut().copy_from_slice(source.as_bytes());
				Ok(LoadReceipt::Copied)
			},
			|| Ok(()),
		)
	}

	/// Shared miss/coalescing path for reported I/O loads and validated copies.
	fn get_or_load_before_publish_inner<Loader, BeforePublish>(
		&self,
		key: PageCacheKey,
		loader: Loader,
		before_publish: BeforePublish,
	) -> FerrosGrynnResult<AtomicPageGuard<'_>>
	where
		Loader: FnOnce(&mut AlignedPage) -> FerrosGrynnResult<LoadReceipt>,
		BeforePublish: FnOnce() -> FerrosGrynnResult<()>,
	{
		let mut loader = Some(loader);
		let mut before_publish = Some(before_publish);
		loop {
			match self.lookup(key) {
				DirectoryLookup::Resident(locator) => match self.try_pin(locator) {
					PinResult::Pinned => return Ok(self.guard(locator)),
					PinResult::Stale => continue,
					PinResult::Exhausted => return Err(pin_exhausted(key)),
				},
				DirectoryLookup::Loading(signal) => {
					self.coalesced_misses.fetch_add(1, Ordering::Relaxed);
					signal.wait()?;
				}
				DirectoryLookup::Absent => match self.reserve(key)? {
					ReserveDecision::Resident(locator) => match self.try_pin(locator) {
						PinResult::Pinned => return Ok(self.guard(locator)),
						PinResult::Stale => continue,
						PinResult::Exhausted => return Err(pin_exhausted(key)),
					},
					ReserveDecision::Loading(signal) => {
						self.coalesced_misses.fetch_add(1, Ordering::Relaxed);
						signal.wait()?;
					}
					ReserveDecision::Reserved(reservation) => {
						let Some(loader) = loader.take() else {
							self.abort_load(
								&reservation,
								FerrosGrynnError::storage_io_failure(
									"atomic-arena loader was consumed before reservation",
								),
							);
							return Err(FerrosGrynnError::storage_io_failure(
								"atomic-arena loader was consumed before reservation",
							));
						};
						let Some(before_publish) = before_publish.take() else {
							let error = FerrosGrynnError::storage_io_failure(
								"atomic-arena publish hook was consumed before reservation",
							);
							self.abort_load(&reservation, error.clone());
							return Err(error);
						};
						return self.load_and_publish(reservation, loader, before_publish);
					}
				},
			}
		}
	}

	/// Resolves a resident key without pinning it.
	pub fn resolve(&self, key: PageCacheKey) -> Option<ResolvedFrameHandle> {
		match self.lookup(key) {
			DirectoryLookup::Resident(locator) => {
				Some(ResolvedFrameHandle::new(self.arena_id, key, locator))
			}
			DirectoryLookup::Absent | DirectoryLookup::Loading(_) => None,
		}
	}

	/// Pins an exact key only when it is already resident.
	///
	/// # Errors
	///
	/// Returns an internal storage error only when the packed pin counter is
	/// exhausted.
	pub fn get_if_resident(
		&self,
		key: PageCacheKey,
	) -> FerrosGrynnResult<Option<AtomicPageGuard<'_>>> {
		loop {
			match self.lookup(key) {
				DirectoryLookup::Resident(locator) => match self.try_pin(locator) {
					PinResult::Pinned => return Ok(Some(self.guard(locator))),
					PinResult::Stale => continue,
					PinResult::Exhausted => return Err(pin_exhausted(key)),
				},
				DirectoryLookup::Absent | DirectoryLookup::Loading(_) => return Ok(None),
			}
		}
	}

	/// Pins a page through a cached locator without a directory lookup.
	///
	/// # Errors
	///
	/// Returns an internal storage error only when the packed pin counter is
	/// exhausted.
	pub fn get_resolved(
		&self,
		handle: &ResolvedFrameHandle,
	) -> FerrosGrynnResult<Option<AtomicPageGuard<'_>>> {
		if handle.arena_id != self.arena_id {
			return Ok(None);
		}
		match self.try_pin(handle.locator) {
			PinResult::Pinned => Ok(Some(self.guard(handle.locator))),
			PinResult::Stale => Ok(None),
			PinResult::Exhausted => Err(pin_exhausted(handle.key)),
		}
	}

	/// Uses a resolved handle when valid and refreshes it through normal lookup
	/// after eviction, generation reuse, or source-epoch change.
	///
	/// # Errors
	///
	/// Returns the same errors as [`Self::get_or_load`].
	pub fn get_resolved_or_load<Loader>(
		&self,
		handle: &mut ResolvedFrameHandle,
		key: PageCacheKey,
		loader: Loader,
	) -> FerrosGrynnResult<AtomicPageGuard<'_>>
	where
		Loader: FnOnce(&mut AlignedPage) -> FerrosGrynnResult<PageHeader>,
	{
		if handle.matches(key) {
			if let Some(guard) = self.get_resolved(handle)? {
				return Ok(guard);
			}
		}
		let guard = self.get_or_load(key, loader)?;
		*handle = ResolvedFrameHandle::new(self.arena_id, key, guard.locator);
		Ok(guard)
	}

	/// Returns slow-path counters as `(misses, coalesced, evictions, waits)`.
	pub fn slow_path_counts(&self) -> (u64, u64, u64, u64) {
		(
			self.misses.load(Ordering::Relaxed),
			self.coalesced_misses.load(Ordering::Relaxed),
			self.evictions.load(Ordering::Relaxed),
			self.backpressure_waits.load(Ordering::Relaxed),
		)
	}

	/// Counts frames currently published as resident.
	pub fn resident_count(&self) -> usize {
		self.metadata
			.iter()
			.filter(|meta| lifecycle(meta.state.load(Ordering::Acquire)) == Lifecycle::Resident)
			.count()
	}

	/// Counts frames retired until the current read epochs drain.
	pub fn retired_count(&self) -> usize {
		self.metadata
			.iter()
			.filter(|meta| lifecycle(meta.state.load(Ordering::SeqCst)) == Lifecycle::Evicting)
			.count()
	}

	/// Returns live source-locator entries across every source epoch.
	pub fn locator_entry_count(&self) -> usize {
		self.locator_entries.load(Ordering::Acquire)
	}

	/// Reserves a cold-miss batch for scatter or frame-oriented direct I/O.
	///
	/// # Errors
	///
	/// Rejects empty/duplicate batches and keys already resident or loading.
	pub fn reserve_batch(
		&self,
		keys: &[PageCacheKey],
	) -> FerrosGrynnResult<AtomicBatchReservation<'_>> {
		self.reserve_batch_with_hook(keys, |_| {})
	}

	/// Builds one batch while invoking a test hook after each acquired frame.
	/// Batch admission remains miss-only and never enters the resident hit
	/// path.
	fn reserve_batch_with_hook<AfterReserve>(
		&self,
		keys: &[PageCacheKey],
		mut after_reserve: AfterReserve,
	) -> FerrosGrynnResult<AtomicBatchReservation<'_>>
	where
		AfterReserve: FnMut(usize),
	{
		if keys.is_empty() {
			return Err(FerrosGrynnError::storage_io_failure(
				"atomic batch reservation must not be empty",
			));
		}
		if keys.len() > self.capacity() {
			return Err(FerrosGrynnError::storage_io_failure(
				"atomic batch reservation exceeds the frame budget",
			));
		}
		let _batch_admission = self.batch_admission.lock();
		let mut seen = RapidHashMap::with_capacity(keys.len());
		let mut batch = AtomicBatchReservation {
			pool: self,
			reservations: Vec::with_capacity(keys.len()),
			published: false,
		};
		for key in keys.iter().copied() {
			if seen.insert(key, ()).is_some() {
				return Err(FerrosGrynnError::storage_io_failure(
					"atomic batch reservation contains a duplicate exact key",
				));
			}
			match self.reserve(key)? {
				ReserveDecision::Reserved(reservation) => {
					batch.reservations.push(reservation);
					after_reserve(batch.reservations.len());
				}
				ReserveDecision::Resident(_) => {
					return Err(FerrosGrynnError::storage_io_failure(
						"atomic direct-to-frame batch requires cold misses",
					));
				}
				ReserveDecision::Loading(_) => {
					return Err(FerrosGrynnError::storage_io_failure(
						"atomic direct-to-frame batch encountered an in-flight key",
					));
				}
			}
		}
		Ok(batch)
	}

	/// Returns whether an exact key is resident or currently loading.
	pub fn is_mapped(&self, key: PageCacheKey) -> bool {
		!matches!(self.lookup(key), DirectoryLookup::Absent)
	}

	/// Removes one exact resident mapping after all of its pins drain.
	///
	/// This prototype-only operation supports cold-to-hot promotion without
	/// retaining the same page in both fixed arenas.
	///
	/// # Errors
	///
	/// Returns a load or lifecycle error if the mapping changes inconsistently.
	pub fn remove_resident(&self, key: PageCacheKey) -> FerrosGrynnResult<bool> {
		let mut replacement = self.replacement.lock();
		let mut registration = None;
		loop {
			match self.lookup(key) {
				DirectoryLookup::Absent => return Ok(false),
				DirectoryLookup::Loading(signal) => {
					drop(registration.take());
					drop(replacement);
					signal.wait()?;
					replacement = self.replacement.lock();
				}
				DirectoryLookup::Resident(locator) => {
					let meta = &self.metadata[locator.index];
					let state = meta.state.load(Ordering::SeqCst);
					if lifecycle(state) != Lifecycle::Resident
						|| generation(state) != locator.generation
					{
						continue;
					}
					if pins(state) != 0 {
						if registration.is_none() {
							registration = Some(CapacityWaiter::new(&self.capacity_waiters));
							continue;
						}
						self.capacity_available.wait(&mut replacement);
						continue;
					}
					let evicting = with_lifecycle(state, Lifecycle::Evicting);
					if meta
						.state
						.compare_exchange(state, evicting, Ordering::SeqCst, Ordering::SeqCst)
						.is_err()
					{
						continue;
					}
					if let Err(error) = self.invalidate_resolved_tables() {
						meta.state.store(state, Ordering::SeqCst);
						return Err(error);
					}
					if !self.remove_resident_mapping(key, locator) {
						meta.state.store(state, Ordering::Release);
						return Err(FerrosGrynnError::storage_io_failure(
							"atomic-arena promoted mapping changed during removal",
						));
					}
					if replacement.keys[locator.index] != Some(key) {
						meta.state.store(state, Ordering::Release);
						return Err(FerrosGrynnError::storage_io_failure(
							"atomic-arena promoted frame retained a different key",
						));
					}
					replacement.keys[locator.index] = None;
					let reclaimed = self.active_read_epochs.load(Ordering::SeqCst) == 0;
					if reclaimed {
						meta.state.store(
							pack_state(Lifecycle::Free, locator.generation, 0, 0),
							Ordering::SeqCst,
						);
					}
					drop(replacement);
					if reclaimed {
						self.capacity_available.notify_all();
					}
					return Ok(true);
				}
			}
		}
	}

	/// Looks up a directory entry and releases its shard lock immediately.
	fn lookup(&self, key: PageCacheKey) -> DirectoryLookup {
		let shard = &self.directory[directory_shard(key)];
		let entries = shard.entries.read();
		entries.get(&key).map_or(DirectoryLookup::Absent, DirectoryEntry::snapshot)
	}

	/// Serializes only miss admission and replacement.
	fn reserve(&self, key: PageCacheKey) -> FerrosGrynnResult<ReserveDecision> {
		let mut replacement = self.replacement.lock();
		let mut registration = None;
		let mut recorded_backpressure = false;
		loop {
			match self.lookup(key) {
				DirectoryLookup::Resident(locator) => {
					return Ok(ReserveDecision::Resident(locator));
				}
				DirectoryLookup::Loading(signal) => {
					return Ok(ReserveDecision::Loading(signal));
				}
				DirectoryLookup::Absent => {}
			}

			if let Some(locator) = self.select_frame(&mut replacement, key)? {
				let signal = Arc::new(LoadSignal::new());
				let shard = &self.directory[directory_shard(key)];
				let mut entries = shard.entries.write();
				if entries.contains_key(&key) {
					replacement.keys[locator.index] = None;
					self.metadata[locator.index].state.store(
						pack_state(Lifecycle::Free, locator.generation, 0, 0),
						Ordering::Release,
					);
					self.capacity_available.notify_one();
					return Err(FerrosGrynnError::storage_io_failure(
						"atomic-arena duplicate mapping appeared during reservation",
					));
				}
				entries.insert(key, DirectoryEntry::Loading {
					locator,
					signal: Arc::clone(&signal),
				});
				drop(entries);
				self.misses.fetch_add(1, Ordering::Relaxed);
				return Ok(ReserveDecision::Reserved(LoadReservation {
					key,
					locator,
					signal,
				}));
			}

			if !recorded_backpressure {
				self.backpressure_waits.fetch_add(1, Ordering::Relaxed);
				recorded_backpressure = true;
			}
			if registration.is_none() {
				registration = Some(CapacityWaiter::new(&self.capacity_waiters));
				continue;
			}
			self.capacity_available.wait(&mut replacement);
		}
	}

	/// Selects a free or cold unpinned frame while the replacement lock is
	/// held.
	fn select_frame(
		&self,
		replacement: &mut ReplacementState,
		new_key: PageCacheKey,
	) -> FerrosGrynnResult<Option<FrameLocator>> {
		let scan_limit = self.capacity().saturating_mul(CLOCK_SCAN_PASSES);
		for _ in 0..scan_limit {
			let index = replacement.hand;
			replacement.hand = (replacement.hand + 1) % self.capacity();
			let meta = &self.metadata[index];
			let raw = meta.state.load(Ordering::SeqCst);
			match lifecycle(raw) {
				Lifecycle::Free => {
					if replacement.keys[index].is_some() {
						return Err(FerrosGrynnError::storage_io_failure(
							"atomic-arena free frame retained a key",
						));
					}
					let next_generation = next_generation(raw, new_key)?;
					let loading = pack_state(Lifecycle::Loading, next_generation, 0, 0);
					if meta
						.state
						.compare_exchange(raw, loading, Ordering::AcqRel, Ordering::Acquire)
						.is_ok()
					{
						replacement.keys[index] = Some(new_key);
						return Ok(Some(FrameLocator {
							index,
							generation: next_generation,
						}));
					}
				}
				Lifecycle::Resident if pins(raw) == 0 && usage(raw) > 0 => {
					let cooled = raw - (1_u64 << USAGE_SHIFT);
					let _ = meta.state.compare_exchange(
						raw,
						cooled,
						Ordering::AcqRel,
						Ordering::Acquire,
					);
				}
				Lifecycle::Resident if pins(raw) == 0 => {
					let evicting = with_lifecycle(raw, Lifecycle::Evicting);
					if meta
						.state
						.compare_exchange(raw, evicting, Ordering::SeqCst, Ordering::SeqCst)
						.is_err()
					{
						continue;
					}
					if let Err(error) = self.invalidate_resolved_tables() {
						meta.state.store(raw, Ordering::SeqCst);
						return Err(error);
					}
					let old_key = match replacement.keys[index] {
						Some(old_key) => old_key,
						None => {
							meta.state.store(raw, Ordering::Release);
							return Err(FerrosGrynnError::storage_io_failure(
								"atomic-arena resident frame had no key",
							));
						}
					};
					if !self.remove_resident_mapping(old_key, FrameLocator {
						index,
						generation: generation(raw),
					}) {
						meta.state.store(raw, Ordering::Release);
						return Err(FerrosGrynnError::storage_io_failure(
							"atomic-arena resident mapping changed during eviction",
						));
					}
					replacement.keys[index] = None;
					self.evictions.fetch_add(1, Ordering::Relaxed);
					if self.active_read_epochs.load(Ordering::SeqCst) > 0 {
						return Ok(None);
					}
				}
				Lifecycle::Evicting if self.active_read_epochs.load(Ordering::SeqCst) == 0 => {
					if replacement.keys[index].is_some() {
						return Err(FerrosGrynnError::storage_io_failure(
							"atomic-arena retired frame retained a key",
						));
					}
					let next_generation = next_generation(raw, new_key)?;
					let loading = pack_state(Lifecycle::Loading, next_generation, 0, 0);
					if meta
						.state
						.compare_exchange(raw, loading, Ordering::SeqCst, Ordering::SeqCst)
						.is_ok()
					{
						replacement.keys[index] = Some(new_key);
						return Ok(Some(FrameLocator {
							index,
							generation: next_generation,
						}));
					}
				}
				Lifecycle::Loading | Lifecycle::Resident | Lifecycle::Evicting => {}
			}
		}
		Ok(None)
	}

	/// Removes only the exact locator selected by replacement.
	fn remove_resident_mapping(&self, key: PageCacheKey, locator: FrameLocator) -> bool {
		let shard = &self.directory[directory_shard(key)];
		let mut entries = shard.entries.write();
		let matches = entries.get(&key).is_some_and(|entry| entry.is_resident(locator));
		if matches {
			entries.remove(&key);
		}
		matches
	}

	/// Fills, validates, and publishes one uniquely owned loading frame.
	fn load_and_publish<Loader, BeforePublish>(
		&self,
		reservation: LoadReservation,
		loader: Loader,
		before_publish: BeforePublish,
	) -> FerrosGrynnResult<AtomicPageGuard<'_>>
	where
		Loader: FnOnce(&mut AlignedPage) -> FerrosGrynnResult<LoadReceipt>,
		BeforePublish: FnOnce() -> FerrosGrynnResult<()>,
	{
		let frame = &self.frames[reservation.locator.index];
		// SAFETY: reservation is created under the replacement lock after the
		// frame enters `Loading`. Its mapping stores exactly one load owner;
		// resident readers cannot pin this lifecycle and replacement cannot
		// reuse it until publication or abort.
		let page = unsafe { &mut *frame.page.get() };
		let loaded = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| loader(page)));
		let load_result = match loaded {
			Ok(result) => result.and_then(|receipt| {
				#[cfg(debug_assertions)]
				PUBLICATION_VALIDATION_COUNT.with(|count| count.set(count.get() + 1));
				let actual = page.validate(reservation.key.page_no())?;
				validate_loaded_header(reservation.key, actual)?;
				if matches!(receipt, LoadReceipt::Reported(reported) if actual != reported) {
					return Err(FerrosGrynnError::invalid_storage_page(
						reservation.key.page_no().get(),
						"atomic loader header does not match its current frame bytes",
					));
				}
				Ok(())
			}),
			Err(payload) => {
				self.abort_load(
					&reservation,
					FerrosGrynnError::storage_io_failure("atomic-arena loader panicked"),
				);
				std::panic::resume_unwind(payload);
			}
		};
		if let Err(error) = load_result {
			self.abort_load(&reservation, error.clone());
			return Err(error);
		}
		let hooked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(before_publish));
		match hooked {
			Ok(Ok(())) => {}
			Ok(Err(error)) => {
				self.abort_load(&reservation, error.clone());
				return Err(error);
			}
			Err(payload) => {
				self.abort_load(
					&reservation,
					FerrosGrynnError::storage_io_failure(
						"atomic-arena before-publish hook panicked",
					),
				);
				std::panic::resume_unwind(payload);
			}
		}

		let meta = &self.metadata[reservation.locator.index];
		let loading = pack_state(Lifecycle::Loading, reservation.locator.generation, 0, 0);
		let resident = pack_state(Lifecycle::Resident, reservation.locator.generation, 1, 1);
		if meta
			.state
			.compare_exchange(loading, resident, Ordering::Release, Ordering::Acquire)
			.is_err()
		{
			let error = FerrosGrynnError::storage_io_failure(
				"atomic-arena loading state changed before publication",
			);
			self.abort_load(&reservation, error.clone());
			return Err(error);
		}

		let shard = &self.directory[directory_shard(reservation.key)];
		let mut entries = shard.entries.write();
		let publishable = entries
			.get(&reservation.key)
			.is_some_and(|entry| entry.is_loading(reservation.locator, &reservation.signal));
		if !publishable {
			meta.state.store(loading, Ordering::Release);
			drop(entries);
			let error = FerrosGrynnError::storage_io_failure(
				"atomic-arena loading mapping changed before publication",
			);
			self.abort_load(&reservation, error.clone());
			return Err(error);
		}
		entries.insert(reservation.key, DirectoryEntry::Resident(reservation.locator));
		drop(entries);
		reservation.signal.complete(Ok(()));
		Ok(self.guard(reservation.locator))
	}

	/// Removes a failed loading mapping and returns its frame to the free list.
	fn abort_load(&self, reservation: &LoadReservation, error: FerrosGrynnError) {
		let mut replacement = self.replacement.lock();
		let shard = &self.directory[directory_shard(reservation.key)];
		let mut entries = shard.entries.write();
		let removable = entries
			.get(&reservation.key)
			.is_some_and(|entry| entry.is_loading(reservation.locator, &reservation.signal));
		if removable {
			entries.remove(&reservation.key);
		}
		drop(entries);
		if replacement.keys[reservation.locator.index] == Some(reservation.key) {
			replacement.keys[reservation.locator.index] = None;
			self.metadata[reservation.locator.index].state.store(
				pack_state(Lifecycle::Free, reservation.locator.generation, 0, 0),
				Ordering::Release,
			);
		}
		drop(replacement);
		reservation.signal.complete(Err(error));
		self.capacity_available.notify_all();
	}

	/// Atomically pins only a resident frame in the requested generation.
	fn try_pin(&self, locator: FrameLocator) -> PinResult {
		let Some(meta) = self.metadata.get(locator.index) else {
			return PinResult::Stale;
		};
		let mut observed = meta.state.load(Ordering::Acquire);
		loop {
			if lifecycle(observed) != Lifecycle::Resident
				|| generation(observed) != locator.generation
			{
				return PinResult::Stale;
			}
			if pins(observed) == MAX_PINS {
				return PinResult::Exhausted;
			}
			let next_usage = usage(observed).saturating_add(1).min(MAX_USAGE);
			let desired = with_pins_and_usage(observed, pins(observed) + 1, next_usage);
			match meta.state.compare_exchange_weak(
				observed,
				desired,
				Ordering::Acquire,
				Ordering::Acquire,
			) {
				Ok(_) => {
					// This CAS installs a pin in the exact resident generation.
					// Replacement can change lifecycle/generation only after
					// pins reach zero, so a second state load cannot
					// invalidate success.
					return PinResult::Pinned;
				}
				Err(current) => observed = current,
			}
		}
	}

	/// Checks whether a compact locator still names the resident generation.
	fn is_locator_resident(&self, locator: FrameLocator) -> bool {
		self.metadata.get(locator.index).is_some_and(|meta| {
			let state = meta.state.load(Ordering::Acquire);
			lifecycle(state) == Lifecycle::Resident && generation(state) == locator.generation
		})
	}

	/// Creates a borrowed guard after a successful pin or publication pin.
	const fn guard(&self, locator: FrameLocator) -> AtomicPageGuard<'_> {
		AtomicPageGuard {
			pool: self,
			locator,
		}
	}

	/// Releases one pin and wakes replacement only when it may be waiting.
	fn release_pin(&self, locator: FrameLocator) {
		let meta = &self.metadata[locator.index];
		let previous = meta.state.fetch_sub(1_u64 << PIN_SHIFT, Ordering::SeqCst);
		debug_assert_eq!(lifecycle(previous), Lifecycle::Resident);
		debug_assert_eq!(generation(previous), locator.generation);
		debug_assert!(pins(previous) > 0);
		if pins(previous) != 1 || self.capacity_waiters.load(Ordering::SeqCst) == 0 {
			return;
		}
		// The waiter registers and rechecks frame state with `SeqCst` while
		// holding `replacement`. Therefore either that recheck observes this
		// last-pin release or this load observes the registration; taking the
		// mutex then closes the condvar enqueue window.
		let _replacement = self.replacement.lock();
		if self.capacity_waiters.load(Ordering::SeqCst) > 0 {
			self.capacity_available.notify_one();
		}
	}

	/// Leaves one cursor-scoped read epoch and releases retired frames for
	/// miss-side reclamation after the last reader.
	fn release_read_epoch(&self) {
		let previous = self.active_read_epochs.fetch_sub(1, Ordering::SeqCst);
		debug_assert!(previous > 0);
		if previous != 1 || self.capacity_waiters.load(Ordering::SeqCst) == 0 {
			return;
		}
		let _replacement = self.replacement.lock();
		if self.capacity_waiters.load(Ordering::SeqCst) > 0 {
			self.capacity_available.notify_all();
		}
	}
}

/// Cursor-scoped reclamation guard for borrowed arena page addresses.
///
/// ponytail: one global reader count delays every retired frame; replace it
/// with generation-bucketed epochs only if measured miss-side backpressure
/// makes that extra machinery necessary.
pub struct AtomicReadEpoch<'pool> {
	/// Arena whose retired bytes remain immutable until this guard drops.
	pool: &'pool AtomicArenaBufferPool,
}

impl AtomicReadEpoch<'_> {
	/// Borrows a page through one cached locator without per-page pin/drop.
	pub fn get_resolved<'epoch>(
		&'epoch self,
		handle: &ResolvedFrameHandle,
	) -> Option<AtomicEpochPage<'epoch>> {
		(handle.arena_id == self.pool.arena_id).then(|| self.get_locator(handle.locator)).flatten()
	}

	/// Validates one exact frame generation before exposing immutable bytes.
	fn get_locator<'epoch>(&'epoch self, locator: FrameLocator) -> Option<AtomicEpochPage<'epoch>> {
		let meta = self.pool.metadata.get(locator.index)?;
		let state = meta.state.load(Ordering::SeqCst);
		if state & LIFECYCLE_MASK != Lifecycle::Resident as u64
			|| generation(state) != locator.generation
		{
			return None;
		}
		// SAFETY: entering this epoch precedes the SeqCst resident-generation
		// check. Replacement first publishes `Evicting` with SeqCst and can
		// reuse those `UnsafeCell` bytes only after the global reader count
		// reaches zero. The returned lifetime is tied to this borrowed epoch,
		// so safe code cannot drop the epoch while this immutable reference
		// remains usable.
		let page = unsafe { &*self.pool.frames[locator.index].page.get() };
		Some(AtomicEpochPage {
			page,
		})
	}
}

impl Drop for AtomicReadEpoch<'_> {
	/// Leaves the reclamation epoch exactly once.
	fn drop(&mut self) {
		self.pool.release_read_epoch();
	}
}

/// Page reference whose lifetime cannot exceed its reclamation epoch.
pub struct AtomicEpochPage<'epoch> {
	/// Immutable validated bytes in the stable frame arena.
	page: &'epoch AlignedPage,
}

impl AtomicEpochPage<'_> {
	/// Returns the validated aligned page.
	pub const fn as_page(&self) -> &AlignedPage {
		self.page
	}

	/// Returns the validated page bytes without copying them.
	pub fn as_bytes(&self) -> &[u8; PAGE_SIZE] {
		self.page.as_bytes()
	}
}

/// Borrowed pin guard over immutable arena bytes.
pub struct AtomicPageGuard<'pool> {
	/// Pool that owns both the stable frame and its pin counter.
	pool: &'pool AtomicArenaBufferPool,
	/// Exact pinned frame generation.
	locator: FrameLocator,
}

impl AtomicPageGuard<'_> {
	/// Returns the pinned immutable aligned page.
	pub fn as_page(&self) -> &AlignedPage {
		let state = self.pool.metadata[self.locator.index].state.load(Ordering::Acquire);
		debug_assert_eq!(lifecycle(state), Lifecycle::Resident);
		debug_assert_eq!(generation(state), self.locator.generation);
		debug_assert!(pins(state) > 0);
		// SAFETY: this guard owns one pin for the exact resident generation.
		// Replacement cannot enter `Evicting` or `Loading` until every pin
		// drops, so no mutable reference can coexist with this immutable
		// reference.
		unsafe { &*self.pool.frames[self.locator.index].page.get() }
	}

	/// Returns the pinned page bytes without copying them.
	pub fn as_bytes(&self) -> &[u8; PAGE_SIZE] {
		self.as_page().as_bytes()
	}
}

impl Drop for AtomicPageGuard<'_> {
	/// Releases the exact generation pin.
	fn drop(&mut self) {
		self.pool.release_pin(self.locator);
	}
}

/// Packs the complete atomic frame state.
const fn pack_state(lifecycle: Lifecycle, generation: u64, pins: u64, usage: u64) -> u64 {
	(lifecycle as u64)
		| (pins << PIN_SHIFT)
		| (usage << USAGE_SHIFT)
		| (generation << GENERATION_SHIFT)
}

/// Decodes the closed lifecycle field.
const fn lifecycle(raw: u64) -> Lifecycle {
	match raw & LIFECYCLE_MASK {
		0 => Lifecycle::Free,
		1 => Lifecycle::Loading,
		2 => Lifecycle::Resident,
		3 => Lifecycle::Evicting,
		_ => Lifecycle::Evicting,
	}
}

/// Decodes the frame generation.
const fn generation(raw: u64) -> u64 {
	raw >> GENERATION_SHIFT
}

/// Decodes the pin count.
const fn pins(raw: u64) -> u64 {
	(raw & PIN_MASK) >> PIN_SHIFT
}

/// Decodes the usage count.
const fn usage(raw: u64) -> u64 {
	(raw & USAGE_MASK) >> USAGE_SHIFT
}

/// Replaces only the lifecycle field.
const fn with_lifecycle(raw: u64, lifecycle: Lifecycle) -> u64 {
	(raw & !LIFECYCLE_MASK) | lifecycle as u64
}

/// Replaces pin and usage fields in one CAS target.
const fn with_pins_and_usage(raw: u64, pins: u64, usage: u64) -> u64 {
	(raw & !(PIN_MASK | USAGE_MASK)) | (pins << PIN_SHIFT) | (usage << USAGE_SHIFT)
}

/// Advances a frame generation without permitting ABA-producing wraparound.
fn next_generation(raw: u64, key: PageCacheKey) -> FerrosGrynnResult<u64> {
	let current = generation(raw);
	if current == MAX_GENERATION {
		return Err(FerrosGrynnError::invalid_storage_page(
			key.page_no().get(),
			"atomic-arena frame generation is exhausted",
		));
	}
	Ok(current + 1)
}

/// Maps an exact key to a stable power-of-two directory shard.
fn directory_shard(key: PageCacheKey) -> usize {
	let page_key = key.page_key();
	let version = match key.version() {
		PageCacheVersion::Wal(lsn) => lsn.get().rotate_left(7) ^ 0xa076_1d64_78bd_642f,
		PageCacheVersion::Home {
			generation,
			ceiling,
		} => generation.get().rotate_left(11) ^ ceiling.get().rotate_left(29),
	};
	let mut mixed = page_key.graph_id().get()
		^ page_key.file_id().get().rotate_left(17)
		^ page_key.page_no().get().rotate_left(37)
		^ version;
	mixed ^= mixed >> 32;
	mixed = mixed.wrapping_mul(0xe703_7ed1_a0b4_28db);
	mixed ^= mixed >> 29;
	(mixed as usize) & (DIRECTORY_SHARDS - 1)
}

/// Builds a stable error for an impossible number of simultaneous guards.
fn pin_exhausted(key: PageCacheKey) -> FerrosGrynnError {
	FerrosGrynnError::storage_io_failure(format!(
		"atomic-arena pin counter is exhausted for page {}",
		key.page_no().get()
	))
}
