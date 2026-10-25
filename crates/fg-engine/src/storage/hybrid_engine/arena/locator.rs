//! Source-bound page-number locators and reusable resident handles.

#[cfg(feature = "storage-engine-prototype-api")]
use std::num::NonZeroUsize;
use std::sync::{
	Arc,
	atomic::{AtomicU64, AtomicUsize, Ordering},
};

use parking_lot::{Mutex, RwLock};
#[cfg(feature = "storage-engine-prototype-api")]
use rapidhash::HashMapExt;
use rapidhash::RapidHashMap;

#[cfg(feature = "storage-engine-prototype-api")]
use super::{
	AtomicArenaBufferPool,
	AtomicEpochPage,
	AtomicPageGuard,
	AtomicReadEpoch,
	DENSE_LOCATOR_INDEX_BITS,
	DENSE_LOCATOR_INDEX_MASK,
	DENSE_LOCATOR_LEAF_SLOTS,
	DENSE_LOCATOR_VALID,
	MAX_GENERATION,
	PinResult,
	pin_exhausted,
};
use crate::storage::hybrid_engine::{SourceEpoch, SourceIdentity};
#[cfg(feature = "storage-engine-prototype-api")]
use crate::{
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	storage::{
		page::{AlignedPage, PageHeader},
		page_cache::{PageCacheKey, PageCacheVersion},
	},
};

/// Stable locator protected against frame reuse by a generation number.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct FrameLocator {
	/// Index into the fixed page and metadata arenas.
	pub(super) index: usize,
	/// Generation captured when the frame was reserved.
	pub(super) generation: u64,
}

/// Cached resolution of one exact immutable page version.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg(feature = "storage-engine-prototype-api")]
pub struct ResolvedFrameHandle {
	/// Identity of the arena that produced this locator.
	pub(super) arena_id: u64,
	/// Complete key used for stale fallback.
	pub(super) key: PageCacheKey,
	/// Arena frame and generation observed during resolution.
	pub(super) locator: FrameLocator,
}

#[cfg(feature = "storage-engine-prototype-api")]
impl ResolvedFrameHandle {
	/// Creates a resolved handle from a resident directory entry.
	pub(super) const fn new(arena_id: u64, key: PageCacheKey, locator: FrameLocator) -> Self {
		Self {
			arena_id,
			key,
			locator,
		}
	}

	/// Confirms that the caller still requests the exact resolved source epoch.
	pub(super) fn matches(&self, key: PageCacheKey) -> bool {
		self.key == key
	}

	/// Returns the exact page version captured by this resolution.
	pub(in crate::storage::hybrid_engine) const fn key(&self) -> PageCacheKey {
		self.key
	}

	/// Returns the arena that produced this immutable locator.
	pub(in crate::storage::hybrid_engine) const fn arena_id(&self) -> u64 {
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
	_arena_id: u64,
	/// Stable physical source identity.
	_source: SourceIdentity,
	/// Exact immutable source version.
	_epoch: SourceEpoch,
	/// Number of addressable pages in this source.
	_page_count: usize,
	/// Lazily allocated page-number chunks.
	_chunks: RwLock<RapidHashMap<usize, Arc<LocatorChunk>>>,
	/// Live slots owned by this source epoch.
	entries: AtomicUsize,
	/// Arena-wide live locator slots across all source epochs.
	global_entries: Arc<AtomicUsize>,
	/// Maximum arena-wide live locator slots.
	_entry_budget: usize,
}

/// One removable page-number chunk.
struct LocatorChunk {
	/// Compact frame locators for consecutive source pages.
	_slots: Box<[AtomicU64]>,
	/// Nonempty slots in this chunk.
	_live: AtomicUsize,
	/// Serializes slot-count changes outside the page hot loop.
	_updates: Mutex<()>,
}

#[cfg(feature = "storage-engine-prototype-api")]
impl LocatorChunk {
	/// Allocates one zeroed chunk.
	fn new() -> Self {
		Self {
			_slots: (0..DENSE_LOCATOR_LEAF_SLOTS)
				.map(|_| AtomicU64::new(0))
				.collect::<Vec<_>>()
				.into_boxed_slice(),
			_live: AtomicUsize::new(0),
			_updates: Mutex::new(()),
		}
	}
}

#[cfg(feature = "storage-engine-prototype-api")]
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
			_arena_id: pool._arena_id,
			_source: SourceIdentity::from_key(first_key),
			_epoch: SourceEpoch::from_key(first_key),
			_page_count: page_count,
			_chunks: RwLock::new(RapidHashMap::new()),
			entries: AtomicUsize::new(0),
			global_entries: Arc::clone(&pool.locator_entries),
			_entry_budget: pool.capacity(),
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
				return Some(ResolvedFrameHandle::new(pool._arena_id, key, locator));
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
		self._chunks.read().len()
	}

	/// Returns the source page range accepted by this locator.
	pub const fn page_count(&self) -> usize {
		self._page_count
	}

	/// Returns whether a key belongs to this arena, source epoch, and page
	/// range.
	pub fn accepts(&self, pool: &AtomicArenaBufferPool, key: PageCacheKey) -> bool {
		self.exact_index(pool, key).is_some()
	}

	/// Returns the exact page-number slot only for this arena and source epoch.
	fn exact_index(&self, pool: &AtomicArenaBufferPool, key: PageCacheKey) -> Option<usize> {
		if pool._arena_id != self._arena_id
			|| SourceIdentity::from_key(key) != self._source
			|| SourceEpoch::from_key(key) != self._epoch
		{
			return None;
		}
		let index = usize::try_from(key.page_no().get()).ok()?;
		(index < self._page_count).then_some(index)
	}

	/// Loads and decodes one compact locator slot.
	fn load_locator(&self, index: usize) -> Option<FrameLocator> {
		let chunk_index = index / DENSE_LOCATOR_LEAF_SLOTS;
		let slot_index = index % DENSE_LOCATOR_LEAF_SLOTS;
		let chunks = self._chunks.read();
		let chunk = chunks.get(&chunk_index)?;
		unpack_dense_locator(chunk._slots[slot_index].load(Ordering::Acquire))
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
			let chunks = self._chunks.read();
			if let Some(chunk) = chunks.get(&chunk_index) {
				let _updates = chunk._updates.lock();
				let slot = &chunk._slots[slot_index];
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
			let chunks = self._chunks.read();
			if let Some(chunk) = chunks.get(&chunk_index) {
				let _updates = chunk._updates.lock();
				let previous = chunk._slots[slot_index].swap(packed, Ordering::AcqRel);
				if previous == 0 {
					chunk._live.fetch_add(1, Ordering::Release);
					self.entries.fetch_add(1, Ordering::Release);
				} else {
					self.global_entries.fetch_sub(1, Ordering::AcqRel);
				}
				return true;
			}
		}

		let mut chunks = self._chunks.write();
		let chunk =
			Arc::clone(chunks.entry(chunk_index).or_insert_with(|| Arc::new(LocatorChunk::new())));
		let _updates = chunk._updates.lock();
		let previous = chunk._slots[slot_index].swap(packed, Ordering::AcqRel);
		if previous == 0 {
			chunk._live.fetch_add(1, Ordering::Release);
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
				(entries < self._entry_budget).then_some(entries + 1)
			})
			.is_ok()
		{
			return true;
		}
		self.prune_stale(pool);
		self.global_entries
			.fetch_update(Ordering::AcqRel, Ordering::Acquire, |entries| {
				(entries < self._entry_budget).then_some(entries + 1)
			})
			.is_ok()
	}

	/// Clears one stale slot and removes its chunk after the last entry.
	fn clear_slot(&self, index: usize) {
		let chunk_index = index / DENSE_LOCATOR_LEAF_SLOTS;
		let slot_index = index % DENSE_LOCATOR_LEAF_SLOTS;
		let chunks = self._chunks.read();
		let Some(chunk) = chunks.get(&chunk_index) else {
			return;
		};
		let chunk = Arc::clone(chunk);
		let _updates = chunk._updates.lock();
		if chunk._slots[slot_index].swap(0, Ordering::AcqRel) == 0 {
			return;
		}
		chunk._live.fetch_sub(1, Ordering::AcqRel);
		self.entries.fetch_sub(1, Ordering::AcqRel);
		self.global_entries.fetch_sub(1, Ordering::AcqRel);
		drop(_updates);
		drop(chunks);
		self.remove_empty_chunk(chunk_index, &chunk);
	}

	/// Clears stale generations without touching live resident frames.
	fn prune_stale(&self, pool: &AtomicArenaBufferPool) {
		let chunks = self
			._chunks
			.read()
			.iter()
			.map(|(index, chunk)| (*index, Arc::clone(chunk)))
			.collect::<Vec<_>>();
		for (chunk_index, chunk) in chunks {
			let updates = chunk._updates.lock();
			for slot in &chunk._slots {
				let raw = slot.load(Ordering::Acquire);
				let Some(locator) = unpack_dense_locator(raw) else {
					continue;
				};
				if pool.is_locator_resident(locator)
					|| slot.compare_exchange(raw, 0, Ordering::AcqRel, Ordering::Acquire).is_err()
				{
					continue;
				}
				chunk._live.fetch_sub(1, Ordering::AcqRel);
				self.entries.fetch_sub(1, Ordering::AcqRel);
				self.global_entries.fetch_sub(1, Ordering::AcqRel);
			}
			drop(updates);
			self.remove_empty_chunk(chunk_index, &chunk);
		}
	}

	/// Removes an exact empty chunk without racing a concurrent insertion.
	fn remove_empty_chunk(&self, chunk_index: usize, chunk: &Arc<LocatorChunk>) {
		if chunk._live.load(Ordering::Acquire) != 0 {
			return;
		}
		let mut chunks = self._chunks.write();
		let remove = chunks.get(&chunk_index).is_some_and(|current| Arc::ptr_eq(current, chunk))
			&& chunk._live.load(Ordering::Acquire) == 0;
		if remove {
			chunks.remove(&chunk_index);
		}
	}
}

impl Drop for SourceBoundLocator {
	/// Returns this source's remaining slots to the arena-wide metadata budget.
	fn drop(&mut self) {
		// This destructor owns the outstanding charge even when locator lookup
		// APIs are feature-gated.
		self.global_entries.fetch_sub(self.entries.load(Ordering::Acquire), Ordering::AcqRel);
	}
}

/// Packs one arena locator while preserving every frame-generation bit.
#[cfg(feature = "storage-engine-prototype-api")]
fn pack_dense_locator(locator: FrameLocator) -> u64 {
	debug_assert!(locator.index as u64 <= DENSE_LOCATOR_INDEX_MASK);
	debug_assert!(locator.generation <= MAX_GENERATION);
	DENSE_LOCATOR_VALID | (locator.generation << DENSE_LOCATOR_INDEX_BITS) | locator.index as u64
}

/// Decodes one nonempty compact arena locator.
#[cfg(feature = "storage-engine-prototype-api")]
fn unpack_dense_locator(raw: u64) -> Option<FrameLocator> {
	(raw & DENSE_LOCATOR_VALID != 0).then_some(FrameLocator {
		index: (raw & DENSE_LOCATOR_INDEX_MASK) as usize,
		generation: (raw & !DENSE_LOCATOR_VALID) >> DENSE_LOCATOR_INDEX_BITS,
	})
}
