//! Lock-partitioned, fixed-arena resident-page prototype.

use std::{
	num::NonZeroUsize,
	sync::{
		Arc,
		atomic::{AtomicU64, AtomicUsize, Ordering},
	},
};

use parking_lot::{Condvar, Mutex};
use rapidhash::{HashMapExt, RapidHashMap};

use crate::{
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	storage::{
		page::{AlignedPage, PageHeader},
		page_cache::{PageCacheKey, PageCacheVersion, validate_loaded_header},
	},
};

/// Number of independently locked directory partitions.
const DIRECTORY_SHARDS: usize = 64;
/// Maximum number of full clock passes before the caller sleeps for capacity.
const CLOCK_SCAN_PASSES: usize = 4;
/// Unique identity source preventing resolved handles from crossing arenas.
static NEXT_ARENA_ID: AtomicU64 = AtomicU64::new(1);

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

mod locator;

use locator::FrameLocator;
pub use locator::{ResolvedFrameHandle, SourceBoundLocator};

mod support;

pub use support::AtomicBatchReservation;
use support::{
	CapacityWaiter,
	DirectoryEntry,
	DirectoryLookup,
	DirectoryShard,
	FrameBytes,
	FrameMeta,
	LoadReceipt,
	LoadReservation,
	LoadSignal,
	PinResult,
	ReplacementState,
	ReserveDecision,
};

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

mod guard;

pub use guard::{AtomicEpochPage, AtomicPageGuard, AtomicReadEpoch};

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
	let mut mixed = page_key.space_id().raw()
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
