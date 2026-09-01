//! Windows virtual-address cache prototypes for immutable home pages.

use std::{
	ffi::c_void,
	num::NonZeroUsize,
	ptr::NonNull,
	sync::{
		Arc,
		atomic::{AtomicU64, AtomicUsize, Ordering},
	},
	time::{Duration, Instant},
};

use parking_lot::{Condvar, Mutex};
use rapidhash::{HashMapExt, RapidHashMap};
use windows::Win32::System::Memory::{
	MEM_COMMIT,
	MEM_DECOMMIT,
	MEM_RELEASE,
	MEM_RESERVE,
	PAGE_NOACCESS,
	PAGE_READWRITE,
	VirtualAlloc,
	VirtualFree,
};

use super::{
	SourceEpoch,
	SourceIdentity,
	atomic_arena::{AtomicArenaBufferPool, AtomicPageGuard, ResolvedFrameHandle},
};
use crate::{
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	storage::{
		buffer_pool::{PageCacheKey, PageCacheVersion, validate_loaded_header},
		page::{AlignedPage, PAGE_SIZE, PageHeader},
	},
};

/// Number of complete home-frame clock scans before applying backpressure.
const CLOCK_SCAN_PASSES: usize = 4;
/// Unique identity source preventing resolved handles from crossing caches.
static NEXT_VM_CACHE_ID: AtomicU64 = AtomicU64::new(1);
/// Packed lifecycle field width.
const LIFECYCLE_BITS: u32 = 2;
/// Packed pin-count field width.
const PIN_BITS: u32 = 20;
/// Packed usage-count field width.
const USAGE_BITS: u32 = 3;
/// Packed generation field width.
const GENERATION_BITS: u32 = u64::BITS - LIFECYCLE_BITS - PIN_BITS - USAGE_BITS;
/// Packed pin-count offset.
const PIN_SHIFT: u32 = LIFECYCLE_BITS;
/// Packed usage-count offset.
const USAGE_SHIFT: u32 = PIN_SHIFT + PIN_BITS;
/// Packed generation offset.
const GENERATION_SHIFT: u32 = USAGE_SHIFT + USAGE_BITS;
/// Packed lifecycle mask.
const LIFECYCLE_MASK: u64 = (1_u64 << LIFECYCLE_BITS) - 1;
/// Packed pin-count mask.
const PIN_MASK: u64 = ((1_u64 << PIN_BITS) - 1) << PIN_SHIFT;
/// Packed usage-count mask.
const USAGE_MASK: u64 = ((1_u64 << USAGE_BITS) - 1) << USAGE_SHIFT;
/// Largest representable simultaneous pin count.
const MAX_PINS: u64 = (1_u64 << PIN_BITS) - 1;
/// Largest representable usage value.
const MAX_USAGE: u64 = (1_u64 << USAGE_BITS) - 1;
/// Largest non-wrapping page generation.
const MAX_GENERATION: u64 = (1_u64 << GENERATION_BITS) - 1;

/// Lifecycle of one fixed home-page address.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u64)]
enum VmLifecycle {
	/// Address is reserved but not committed.
	Free = 0,
	/// One loader owns the committed writable page.
	Loading = 1,
	/// Validated immutable bytes are published.
	Resident = 2,
	/// Replacement owns an unpinned page while decommitting it, or retains a
	/// committed page quarantined after rollback decommit failed.
	Evicting = 3,
}

/// Exact registered immutable home-file address space.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct HomeSourceSpec {
	/// Stable file identity.
	source: SourceIdentity,
	/// Exact checkpoint generation and visibility ceiling.
	epoch: SourceEpoch,
	/// Number of addressable database pages in this source.
	page_count: usize,
}

impl HomeSourceSpec {
	/// Creates a source registration from one representative home key.
	///
	/// # Errors
	///
	/// Returns an error for WAL keys or an empty address space.
	pub(super) fn from_key(
		sample_key: PageCacheKey,
		page_count: NonZeroUsize,
	) -> FerrosGrynnResult<Self> {
		if !matches!(sample_key.version(), PageCacheVersion::Home { .. }) {
			return Err(FerrosGrynnError::storage_io_failure(
				"VM home source must use a home page version",
			));
		}
		Ok(Self {
			source: SourceIdentity::from_key(sample_key),
			epoch: SourceEpoch::from_key(sample_key),
			page_count: page_count.get(),
		})
	}
}

/// Source/epoch key whose pages share one reserved virtual range.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct HomeSourceKey {
	/// Stable physical source identity.
	source: SourceIdentity,
	/// Exact checkpoint source epoch.
	epoch: SourceEpoch,
}

impl HomeSourceKey {
	/// Extracts the registered source key from one exact cache key.
	const fn from_key(key: PageCacheKey) -> Self {
		Self {
			source: SourceIdentity::from_key(key),
			epoch: SourceEpoch::from_key(key),
		}
	}
}

/// Reserved address range released as one unit.
struct VmReservation {
	/// Allocation-granularity-aligned base returned by `VirtualAlloc`.
	base: NonNull<u8>,
	/// Complete reserved byte count.
	byte_len: usize,
	/// Test-only failures injected before the native decommit call.
	#[cfg(test)]
	injected_decommit_failures: AtomicUsize,
}

impl VmReservation {
	/// Reserves an inaccessible address range without committing physical
	/// memory.
	fn reserve(page_count: usize) -> FerrosGrynnResult<Self> {
		let byte_len = page_count.checked_mul(PAGE_SIZE).ok_or_else(|| {
			FerrosGrynnError::storage_io_failure("VM home reservation byte length overflow")
		})?;
		// SAFETY: a null preferred address asks Windows to select a suitable
		// region. `MEM_RESERVE` with `PAGE_NOACCESS` commits no physical pages.
		let base = unsafe { VirtualAlloc(None, byte_len, MEM_RESERVE, PAGE_NOACCESS) };
		let base = NonNull::new(base.cast::<u8>()).ok_or_else(|| {
			windows_failure("reserve VM home address range", windows::core::Error::from_thread())
		})?;
		if !(base.as_ptr() as usize).is_multiple_of(align_of::<AlignedPage>()) {
			// SAFETY: the complete range was just reserved and no references exist.
			drop(unsafe { VirtualFree(base.as_ptr().cast::<c_void>(), 0, MEM_RELEASE) });
			return Err(FerrosGrynnError::storage_io_failure(
				"VM reservation base is not database-page aligned",
			));
		}
		Ok(Self {
			base,
			byte_len,
			#[cfg(test)]
			injected_decommit_failures: AtomicUsize::new(0),
		})
	}

	/// Returns one fixed database-page address within the reserved range.
	fn page_address(&self, page_index: usize) -> FerrosGrynnResult<NonNull<AlignedPage>> {
		let offset = page_index.checked_mul(PAGE_SIZE).ok_or_else(|| {
			FerrosGrynnError::storage_io_failure("VM page-address offset overflow")
		})?;
		if offset >= self.byte_len {
			return Err(FerrosGrynnError::storage_io_failure(
				"VM page address is outside its reserved range",
			));
		}
		// SAFETY: the bounds check proves the resulting address remains inside
		// the reservation. No reference is created while it may be decommitted.
		let address = unsafe { self.base.as_ptr().add(offset) }.cast::<AlignedPage>();
		NonNull::new(address).ok_or_else(|| {
			FerrosGrynnError::storage_io_failure("VM page address unexpectedly became null")
		})
	}

	/// Commits exactly one database page at its fixed reserved address.
	fn commit(&self, page_index: usize) -> FerrosGrynnResult<NonNull<AlignedPage>> {
		let address = self.page_address(page_index)?;
		// SAFETY: replacement serializes commit/decommit and calls this only for
		// a free, currently decommitted page-sized subrange.
		let committed = unsafe {
			VirtualAlloc(
				Some(address.as_ptr().cast::<c_void>()),
				PAGE_SIZE,
				MEM_COMMIT,
				PAGE_READWRITE,
			)
		};
		let committed = NonNull::new(committed.cast::<AlignedPage>()).ok_or_else(|| {
			windows_failure("commit VM home page", windows::core::Error::from_thread())
		})?;
		if committed != address {
			// SAFETY: this call decommits only the unexpected page just committed.
			drop(unsafe {
				VirtualFree(committed.as_ptr().cast::<c_void>(), PAGE_SIZE, MEM_DECOMMIT)
			});
			return Err(FerrosGrynnError::storage_io_failure(
				"VirtualAlloc committed a different address than requested",
			));
		}
		Ok(committed)
	}

	/// Decommits one page while retaining the encompassing reservation.
	fn decommit(&self, page_index: usize) -> FerrosGrynnResult<()> {
		let address = self.page_address(page_index)?;
		#[cfg(test)]
		if self
			.injected_decommit_failures
			.fetch_update(Ordering::AcqRel, Ordering::Acquire, |remaining| remaining.checked_sub(1))
			.is_ok()
		{
			return Err(FerrosGrynnError::storage_io_failure(
				"injected VM home-page decommit failure",
			));
		}
		// SAFETY: cache callers hold replacement and either uniquely own a
		// `Loading` page or reached `Evicting` with zero pins and no native I/O.
		// Lifecycle probes likewise hold exclusive ownership of their reservation.
		unsafe { VirtualFree(address.as_ptr().cast::<c_void>(), PAGE_SIZE, MEM_DECOMMIT) }
			.map_err(|source| windows_failure("decommit VM home page", source))
	}

	/// Injects a bounded number of failures before native decommit calls.
	#[cfg(test)]
	fn inject_decommit_failures(&self, failures: usize) {
		self.injected_decommit_failures.store(failures, Ordering::Release);
	}
}

// SAFETY: the pointer is only an address token. All creation of references and
// all commit/decommit transitions are synchronized by `VmCache` lifecycle
// state.
unsafe impl Send for VmReservation {}
// SAFETY: see the `Send` proof; immutable methods do not expose references to
// decommitted memory and replacement serializes mapping mutations.
unsafe impl Sync for VmReservation {}

/// Direct measurements for the fixed-address VM lifecycle. This prototype
/// module is test-only, so the result does not extend a production API.
#[derive(Debug, Clone, Copy)]
pub(super) struct VmLifecycleProbe {
	/// Reserve-only duration with no committed memory.
	pub(super) reserve: Duration,
	/// First commit duration before any page touch.
	pub(super) commit: Duration,
	/// First write across the committed database page.
	pub(super) first_touch: Duration,
	/// Decommit duration while preserving the reservation.
	pub(super) decommit: Duration,
	/// Commit duration at the same fixed address after decommit.
	pub(super) recommit: Duration,
}

/// Measures reserve, commit, first-touch, decommit, and recommit separately.
///
/// # Errors
///
/// Returns the underlying Windows VM allocation failure.
pub(super) fn probe_vm_lifecycle() -> FerrosGrynnResult<VmLifecycleProbe> {
	let started = Instant::now();
	let reservation = VmReservation::reserve(1)?;
	let reserve = started.elapsed();

	let started = Instant::now();
	let address = reservation.commit(0)?;
	let commit = started.elapsed();

	let started = Instant::now();
	// SAFETY: the complete database page is committed, writable, aligned, and
	// exclusively owned by this local reservation.
	unsafe { address.as_ptr().write(AlignedPage::zeroed()) };
	let first_touch = started.elapsed();

	let started = Instant::now();
	reservation.decommit(0)?;
	let decommit = started.elapsed();

	let started = Instant::now();
	let recommitted = reservation.commit(0)?;
	let recommit = started.elapsed();
	if recommitted != address {
		return Err(FerrosGrynnError::storage_io_failure(
			"VM recommit changed the fixed page address",
		));
	}
	reservation.decommit(0)?;

	Ok(VmLifecycleProbe {
		reserve,
		commit,
		first_touch,
		decommit,
		recommit,
	})
}

impl Drop for VmReservation {
	/// Releases the complete range after every guard and cache operation ended.
	fn drop(&mut self) {
		// SAFETY: cache ownership guarantees no page guard or loader can outlive
		// the source; `MEM_RELEASE` requires size zero for the original base.
		drop(unsafe { VirtualFree(self.base.as_ptr().cast::<c_void>(), 0, MEM_RELEASE) });
	}
}

/// Per-page load result retained only while its generation is loading or
/// resident.
struct VmLoadSignal {
	/// Terminal load result, absent until publish or abort.
	result: Mutex<Option<FerrosGrynnResult<()>>>,
	/// Completion wait queue.
	ready: Condvar,
}

impl VmLoadSignal {
	/// Creates one incomplete signal.
	fn new() -> Self {
		Self {
			result: Mutex::new(None),
			ready: Condvar::new(),
		}
	}

	/// Waits without holding replacement state.
	fn wait(&self) -> FerrosGrynnResult<()> {
		let mut result = self.result.lock();
		while result.is_none() {
			self.ready.wait(&mut result);
		}
		match result.as_ref() {
			Some(completed) => completed.clone(),
			None => {
				Err(FerrosGrynnError::storage_io_failure("VM load signal woke without a result"))
			}
		}
	}

	/// Publishes one terminal result.
	fn complete(&self, completed: FerrosGrynnResult<()>) {
		let mut result = self.result.lock();
		if result.is_none() {
			*result = Some(completed);
			self.ready.notify_all();
		}
	}
}

/// Atomic lifecycle metadata for one fixed virtual home-page address.
#[repr(align(64))]
struct VmPageMeta {
	/// Lifecycle, generation, pins, and replacement usage.
	state: AtomicU64,
	/// Current in-flight load signal, installed before `Loading` publication.
	signal: Mutex<Option<(u64, Arc<VmLoadSignal>)>>,
}

impl VmPageMeta {
	/// Creates one never-committed page descriptor.
	fn new() -> Self {
		Self {
			state: AtomicU64::new(pack_state(VmLifecycle::Free, 0, 0, 0)),
			signal: Mutex::new(None),
		}
	}

	/// Clones only the signal for the observed loading generation.
	///
	/// `None` is a normal stale observation: rollback or replacement may have
	/// completed after the caller loaded `Loading` but before it acquired this
	/// mutex. The caller must retry lifecycle resolution instead of surfacing
	/// an invariant error.
	fn loading_signal(&self, generation: u64) -> Option<Arc<VmLoadSignal>> {
		self.signal
			.lock()
			.as_ref()
			.filter(|(current, _)| *current == generation)
			.map(|(_, signal)| Arc::clone(signal))
	}

	/// Clears only the exact load signal being completed or quarantined.
	fn clear_loading_signal(&self, generation: u64, expected: &Arc<VmLoadSignal>) {
		let mut current = self.signal.lock();
		if matches!(
			current.as_ref(),
			Some((current_generation, signal))
				if *current_generation == generation && Arc::ptr_eq(signal, expected)
		) {
			*current = None;
		}
	}
}

/// One exact home-file reservation and its separate metadata.
struct VmHomeSource {
	/// Registered source identity and page count.
	spec: HomeSourceSpec,
	/// Full fixed address range.
	reservation: VmReservation,
	/// One descriptor per fixed page address.
	metadata: Box<[VmPageMeta]>,
}

impl VmHomeSource {
	/// Reserves one source range without committing its pages.
	fn new(spec: HomeSourceSpec) -> FerrosGrynnResult<Self> {
		let reservation = VmReservation::reserve(spec.page_count)?;
		let metadata =
			(0..spec.page_count).map(|_| VmPageMeta::new()).collect::<Vec<_>>().into_boxed_slice();
		Ok(Self {
			spec,
			reservation,
			metadata,
		})
	}
}

/// Exact committed home page occupying one global budget slot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct HomeLocator {
	/// Index into the immutable source table.
	source_index: usize,
	/// Fixed page index within that source reservation.
	page_index: usize,
	/// Incarnation protecting resolved handles from decommit/recommit ABA.
	generation: u64,
}

/// Global home-page replacement state shared by every registered file.
struct HomeReplacement {
	/// Next budget slot inspected by the clock hand.
	hand: usize,
	/// Committed page occupying each global budget slot.
	slots: Box<[Option<HomeLocator>]>,
}

/// Miss admission outcome after replacement synchronization.
enum HomeReserveDecision {
	/// The observed loading generation completed before its signal was cloned.
	Retry,
	/// Another caller published the requested page.
	Resident(HomeLocator),
	/// Another caller owns the requested load.
	Loading(Arc<VmLoadSignal>),
	/// This caller owns the committed writable page.
	Reserved(HomeLoadReservation),
}

/// Unique ownership of one committed loading address.
struct HomeLoadReservation {
	/// Exact cache key being loaded.
	key: PageCacheKey,
	/// Fixed page locator and generation.
	locator: HomeLocator,
	/// Global committed-budget slot.
	slot_index: usize,
	/// Signal shared with coalesced callers.
	signal: Arc<VmLoadSignal>,
}

/// RAII group of committed home addresses owned by one direct transport.
pub(super) struct VmBatchReservation<'cache> {
	/// Cache that owns every source reservation and budget slot.
	cache: &'cache VmCache,
	/// Exact loading pages in caller order.
	reservations: Vec<HomeLoadReservation>,
	/// Whether all pages were published together.
	published: bool,
}

impl<'cache> VmBatchReservation<'cache> {
	/// Returns disjoint committed writable home pages in key order.
	///
	/// # Errors
	///
	/// Returns an internal error for an impossible source/page locator.
	#[allow(
		clippy::needless_pass_by_ref_mut,
		reason = "exclusive receiver prevents repeated calls from creating aliased mutable VM \
		          references"
	)]
	pub(super) fn pages_mut(&mut self) -> FerrosGrynnResult<Vec<&mut AlignedPage>> {
		let mut pages = Vec::with_capacity(self.reservations.len());
		for reservation in &self.reservations {
			let address = self.cache.home_address(reservation.locator)?;
			// SAFETY: each reservation owns a different global budget slot and a
			// different fixed `Loading` page; no reader may pin it before publish.
			pages.push(unsafe { &mut *address.as_ptr() });
		}
		Ok(pages)
	}

	/// Validates every exact page and publishes the extent all at once.
	///
	/// # Errors
	///
	/// Returns a validation or lifecycle preflight error; Drop decommits every
	/// unpublished page.
	pub(super) fn publish(mut self) -> FerrosGrynnResult<Vec<VmPageGuard<'cache>>> {
		for reservation in &self.reservations {
			let address = self.cache.home_address(reservation.locator)?;
			// SAFETY: publication consumes the unique batch owner after every
			// mutable page borrow has ended. The address remains committed and in
			// `Loading`, so no reader or replacement owner can alias these bytes.
			let page = unsafe { &*address.as_ptr() };
			let actual = page.validate(reservation.key.page_no())?;
			validate_loaded_header(reservation.key, actual)?;
		}

		let replacement = self.cache.replacement.lock();
		for reservation in &self.reservations {
			let meta = &self.cache.sources[reservation.locator.source_index].metadata
				[reservation.locator.page_index];
			let expected = pack_state(VmLifecycle::Loading, reservation.locator.generation, 0, 0);
			if meta.state.load(Ordering::Acquire) != expected
				|| replacement.slots.get(reservation.slot_index).copied().flatten()
					!= Some(reservation.locator)
			{
				return Err(FerrosGrynnError::storage_io_failure(
					"VM batch loading state changed before publication",
				));
			}
		}
		for reservation in &self.reservations {
			self.cache.sources[reservation.locator.source_index].metadata
				[reservation.locator.page_index]
				.state
				.store(
					pack_state(VmLifecycle::Resident, reservation.locator.generation, 1, 1),
					Ordering::Release,
				);
		}
		self.published = true;
		drop(replacement);
		for reservation in &self.reservations {
			reservation.signal.complete(Ok(()));
		}
		self.reservations
			.iter()
			.map(|reservation| self.cache.home_guard(reservation.locator).map(VmPageGuard::Home))
			.collect()
	}
}

impl Drop for VmBatchReservation<'_> {
	/// Decommits every page unless all were published together.
	fn drop(&mut self) {
		if self.published {
			return;
		}
		for reservation in &self.reservations {
			let _rollback_error = self.cache.abort_home_load(
				reservation,
				FerrosGrynnError::storage_io_failure("VM batch reservation was cancelled"),
			);
		}
	}
}

/// RAII registration closing the home-capacity lost-wakeup window.
struct VmCapacityWaiter<'counter> {
	/// Waiter counter observed by the last home guard drop.
	counter: &'counter AtomicUsize,
}

impl<'counter> VmCapacityWaiter<'counter> {
	/// Registers before replacement capacity is rechecked.
	fn new(counter: &'counter AtomicUsize) -> Self {
		counter.fetch_add(1, Ordering::SeqCst);
		Self {
			counter,
		}
	}
}

impl Drop for VmCapacityWaiter<'_> {
	/// Unregisters on all success and error exits.
	fn drop(&mut self) {
		self.counter.fetch_sub(1, Ordering::SeqCst);
	}
}

/// Result of one atomic generation-aware pin attempt.
enum VmPinResult {
	/// Exact resident generation was pinned.
	Pinned,
	/// Lifecycle or generation changed.
	Stale,
	/// Simultaneous guard count exceeds the packed representation.
	Exhausted,
}

/// Opaque pre-resolved exact home or WAL address.
#[derive(Debug, Clone, Copy)]
pub(super) struct VmResolvedHandle(VmResolvedHandleKind);

/// Private representation preventing safe callers from forging raw addresses.
#[derive(Debug, Clone, Copy)]
enum VmResolvedHandleKind {
	/// Fixed home address plus its current generation.
	Home {
		/// Identity of the cache that owns this fixed address.
		cache_id: u64,
		/// Complete key used for epoch validation and stale fallback.
		key: PageCacheKey,
		/// Fixed source/page and generation.
		locator: HomeLocator,
		/// Precomputed aligned virtual address.
		address: usize,
	},
	/// Bounded WAL-slab locator.
	Wal {
		/// Complete WAL-exact key.
		key: PageCacheKey,
		/// Atomic-arena locator.
		handle: ResolvedFrameHandle,
	},
}

impl VmResolvedHandle {
	/// Returns the exact immutable key embedded in the handle.
	const fn key(&self) -> PageCacheKey {
		match &self.0 {
			VmResolvedHandleKind::Home {
				key,
				..
			}
			| VmResolvedHandleKind::Wal {
				key,
				..
			} => *key,
		}
	}
}

/// Windows VM-address cache with a global home budget and bounded WAL slab.
pub(super) struct VmCache {
	/// Unique ownership identity for resolved handles.
	cache_id: u64,
	/// Immutable registered home-file address spaces.
	sources: Box<[VmHomeSource]>,
	/// Source/epoch mapping used only by generic home hits.
	source_index: RapidHashMap<HomeSourceKey, usize>,
	/// Shared home-frame clock and committed budget slots.
	replacement: Mutex<HomeReplacement>,
	/// Serializes construction of multi-page VM reservations so concurrent
	/// batches cannot retain mutually blocking partial allocations.
	batch_admission: Mutex<()>,
	/// Capacity wait queue used only on miss.
	capacity_available: Condvar,
	/// Registered waiters used to avoid hit-path notification locks.
	capacity_waiters: AtomicUsize,
	/// Number of currently committed home pages.
	committed_pages: AtomicUsize,
	/// Bounded stable arena for exact WAL versions.
	wal: Option<AtomicArenaBufferPool>,
}

impl VmCache {
	/// Reserves all home address spaces and splits one total frame budget
	/// between home pages and the bounded WAL slab.
	///
	/// # Errors
	///
	/// Returns an allocation error or rejects duplicate exact source epochs.
	pub(super) fn new(
		home_sources: &[HomeSourceSpec],
		total_capacity: NonZeroUsize,
		wal_capacity: usize,
	) -> FerrosGrynnResult<Self> {
		let home_capacity = total_capacity.get().checked_sub(wal_capacity).ok_or_else(|| {
			FerrosGrynnError::storage_io_failure(
				"VM WAL slab must be smaller than the total frame budget",
			)
		})?;
		let home_capacity = NonZeroUsize::new(home_capacity).ok_or_else(|| {
			FerrosGrynnError::storage_io_failure(
				"VM total frame budget must retain at least one home frame",
			)
		})?;
		let mut sources = Vec::with_capacity(home_sources.len());
		let mut source_index = RapidHashMap::with_capacity(home_sources.len());
		for spec in home_sources.iter().copied() {
			let key = HomeSourceKey {
				source: spec.source,
				epoch: spec.epoch,
			};
			if source_index.contains_key(&key) {
				return Err(FerrosGrynnError::storage_io_failure(
					"duplicate exact VM home source registration",
				));
			}
			let index = sources.len();
			sources.push(VmHomeSource::new(spec)?);
			source_index.insert(key, index);
		}
		Ok(Self {
			cache_id: NEXT_VM_CACHE_ID.fetch_add(1, Ordering::Relaxed),
			sources: sources.into_boxed_slice(),
			source_index,
			replacement: Mutex::new(HomeReplacement {
				hand: 0,
				slots: vec![None; home_capacity.get()].into_boxed_slice(),
			}),
			batch_admission: Mutex::new(()),
			capacity_available: Condvar::new(),
			capacity_waiters: AtomicUsize::new(0),
			committed_pages: AtomicUsize::new(0),
			wal: NonZeroUsize::new(wal_capacity).map(AtomicArenaBufferPool::new),
		})
	}

	/// Returns a validated pinned home or WAL page, loading on an exact miss.
	///
	/// # Errors
	///
	/// Returns source registration, loader, validation, allocation, or
	/// lifecycle errors.
	pub(super) fn get_or_load<Loader>(
		&self,
		key: PageCacheKey,
		loader: Loader,
	) -> FerrosGrynnResult<VmPageGuard<'_>>
	where
		Loader: FnOnce(&mut AlignedPage) -> FerrosGrynnResult<PageHeader>,
	{
		match key.version() {
			PageCacheVersion::Home {
				..
			} => self.get_or_load_home(key, loader).map(VmPageGuard::Home),
			PageCacheVersion::Wal(_) => self
				.wal
				.as_ref()
				.ok_or_else(|| {
					FerrosGrynnError::storage_io_failure(
						"VM cache was configured without a WAL frame slab",
					)
				})?
				.get_or_load(key, loader)
				.map(VmPageGuard::Wal),
		}
	}

	/// Pins an exact home page only when it is already resident.
	///
	/// # Errors
	///
	/// Returns source-registration, page-range, or exhausted-pin errors.
	pub(super) fn get_if_home_resident(
		&self,
		key: PageCacheKey,
	) -> FerrosGrynnResult<Option<VmPageGuard<'_>>> {
		let (source_index, page_index) = self.locate_home(key)?;
		loop {
			let state =
				self.sources[source_index].metadata[page_index].state.load(Ordering::Acquire);
			if lifecycle(state) != VmLifecycle::Resident {
				return Ok(None);
			}
			let locator = HomeLocator {
				source_index,
				page_index,
				generation: generation(state),
			};
			match self.try_pin_home(locator) {
				VmPinResult::Pinned => {
					return self.home_guard(locator).map(VmPageGuard::Home).map(Some);
				}
				VmPinResult::Stale => {}
				VmPinResult::Exhausted => return Err(pin_exhausted(key)),
			}
		}
	}

	/// Returns whether an exact home page is resident or currently loading.
	///
	/// # Errors
	///
	/// Returns source-registration or page-range errors.
	pub(super) fn is_home_mapped(&self, key: PageCacheKey) -> FerrosGrynnResult<bool> {
		let (source_index, page_index) = self.locate_home(key)?;
		let state = self.sources[source_index].metadata[page_index].state.load(Ordering::Acquire);
		Ok(matches!(lifecycle(state), VmLifecycle::Loading | VmLifecycle::Resident))
	}

	/// Decommits one exact unpinned resident home page after its pins drain.
	///
	/// This prototype-only operation supports cold-to-hot promotion without
	/// retaining the same page in both VM budgets.
	///
	/// # Errors
	///
	/// Returns source-registration, page-range, load, decommit, or lifecycle
	/// errors.
	pub(super) fn remove_home_resident(&self, key: PageCacheKey) -> FerrosGrynnResult<bool> {
		let (source_index, page_index) = self.locate_home(key)?;
		let meta = &self.sources[source_index].metadata[page_index];
		let mut replacement = self.replacement.lock();
		let mut registration = None;
		loop {
			let state = meta.state.load(Ordering::SeqCst);
			match lifecycle(state) {
				VmLifecycle::Free => return Ok(false),
				VmLifecycle::Loading => {
					let Some(signal) = meta.loading_signal(generation(state)) else {
						continue;
					};
					drop(registration.take());
					drop(replacement);
					signal.wait()?;
					replacement = self.replacement.lock();
				}
				VmLifecycle::Resident => {
					if pins(state) != 0 {
						if registration.is_none() {
							registration = Some(VmCapacityWaiter::new(&self.capacity_waiters));
							continue;
						}
						self.capacity_available.wait(&mut replacement);
						continue;
					}
					let locator = HomeLocator {
						source_index,
						page_index,
						generation: generation(state),
					};
					let evicting = with_lifecycle(state, VmLifecycle::Evicting);
					if meta
						.state
						.compare_exchange(state, evicting, Ordering::AcqRel, Ordering::Acquire)
						.is_err()
					{
						continue;
					}
					let Some(slot_index) =
						replacement.slots.iter().position(|current| *current == Some(locator))
					else {
						meta.state.store(state, Ordering::Release);
						return Err(FerrosGrynnError::storage_io_failure(
							"VM promoted home page lost its committed-budget slot",
						));
					};
					if let Err(error) = self.sources[source_index].reservation.decommit(page_index)
					{
						meta.state.store(state, Ordering::Release);
						return Err(error);
					}
					meta.state.store(
						pack_state(VmLifecycle::Free, locator.generation, 0, 0),
						Ordering::Release,
					);
					*meta.signal.lock() = None;
					replacement.slots[slot_index] = None;
					let previous = self.committed_pages.fetch_sub(1, Ordering::Release);
					debug_assert!(previous > 0);
					drop(replacement);
					self.capacity_available.notify_all();
					return Ok(true);
				}
				VmLifecycle::Evicting => {
					let locator = HomeLocator {
						source_index,
						page_index,
						generation: generation(state),
					};
					self.cleanup_quarantined_home_locked(&mut replacement, locator)?;
					return Ok(false);
				}
			}
		}
	}

	/// Reserves an all-home cold-miss batch for direct scatter or frame IoRing.
	///
	/// # Errors
	///
	/// Rejects empty, duplicate, oversized, resident, loading, or WAL batches.
	pub(super) fn reserve_home_batch(
		&self,
		keys: &[PageCacheKey],
	) -> FerrosGrynnResult<VmBatchReservation<'_>> {
		self.reserve_home_batch_with_hook(keys, |_| {})
	}

	/// Builds one home batch while invoking a test hook after every admission.
	/// Batch construction remains outside every resident-hit path.
	fn reserve_home_batch_with_hook<AfterReserve>(
		&self,
		keys: &[PageCacheKey],
		mut after_reserve: AfterReserve,
	) -> FerrosGrynnResult<VmBatchReservation<'_>>
	where
		AfterReserve: FnMut(usize),
	{
		if keys.is_empty() {
			return Err(FerrosGrynnError::storage_io_failure(
				"VM batch reservation must not be empty",
			));
		}
		if keys.len() > self.replacement.lock().slots.len() {
			return Err(FerrosGrynnError::storage_io_failure(
				"VM batch reservation exceeds the home-frame budget",
			));
		}
		let _batch_admission = self.batch_admission.lock();
		let mut seen = RapidHashMap::with_capacity(keys.len());
		let mut batch = VmBatchReservation {
			cache: self,
			reservations: Vec::with_capacity(keys.len()),
			published: false,
		};
		for key in keys.iter().copied() {
			if seen.insert(key, ()).is_some() {
				return Err(FerrosGrynnError::storage_io_failure(
					"VM batch reservation contains a duplicate exact key",
				));
			}
			let (source_index, page_index) = self.locate_home(key)?;
			'reserve_key: loop {
				match self.reserve_home(key, source_index, page_index)? {
					HomeReserveDecision::Retry => continue,
					HomeReserveDecision::Reserved(reservation) => {
						batch.reservations.push(reservation);
						after_reserve(batch.reservations.len());
						break 'reserve_key;
					}
					HomeReserveDecision::Resident(_) => {
						return Err(FerrosGrynnError::storage_io_failure(
							"VM direct-to-frame batch requires cold misses",
						));
					}
					HomeReserveDecision::Loading(_) => {
						return Err(FerrosGrynnError::storage_io_failure(
							"VM direct-to-frame batch encountered an in-flight key",
						));
					}
				}
			}
		}
		Ok(batch)
	}

	/// Resolves an already resident exact page without pinning it.
	pub(super) fn resolve(&self, key: PageCacheKey) -> Option<VmResolvedHandle> {
		match key.version() {
			PageCacheVersion::Wal(_) => self.wal.as_ref()?.resolve(key).map(|handle| {
				VmResolvedHandle(VmResolvedHandleKind::Wal {
					key,
					handle,
				})
			}),
			PageCacheVersion::Home {
				..
			} => {
				let (source_index, page_index) = self.locate_home(key).ok()?;
				let meta = &self.sources[source_index].metadata[page_index];
				let state = meta.state.load(Ordering::Acquire);
				if lifecycle(state) != VmLifecycle::Resident {
					return None;
				}
				let locator = HomeLocator {
					source_index,
					page_index,
					generation: generation(state),
				};
				let address = self.home_address(locator).ok()?.as_ptr() as usize;
				Some(VmResolvedHandle(VmResolvedHandleKind::Home {
					cache_id: self.cache_id,
					key,
					locator,
					address,
				}))
			}
		}
	}

	/// Pins an exact page through a pre-resolved address without source lookup.
	///
	/// # Errors
	///
	/// Returns an error only for exhausted pin state or an invalid stored
	/// address.
	pub(super) fn get_resolved(
		&self,
		handle: &VmResolvedHandle,
	) -> FerrosGrynnResult<Option<VmPageGuard<'_>>> {
		match &handle.0 {
			VmResolvedHandleKind::Wal {
				handle,
				..
			} => match &self.wal {
				Some(wal) => wal.get_resolved(handle).map(|guard| guard.map(VmPageGuard::Wal)),
				None => Ok(None),
			},
			VmResolvedHandleKind::Home {
				cache_id,
				key,
				locator,
				address,
			} => {
				if *cache_id != self.cache_id {
					return Ok(None);
				}
				match self.try_pin_home(*locator) {
					VmPinResult::Pinned => {
						if HomeSourceKey::from_key(*key)
							!= (HomeSourceKey {
								source: self.sources[locator.source_index].spec.source,
								epoch: self.sources[locator.source_index].spec.epoch,
							}) {
							self.release_home_pin(*locator);
							return Ok(None);
						}
						Ok(Some(VmPageGuard::Home(VmHomeGuard {
							cache: self,
							locator: *locator,
							address: *address,
						})))
					}
					VmPinResult::Stale => Ok(None),
					VmPinResult::Exhausted => Err(pin_exhausted(*key)),
				}
			}
		}
	}

	/// Uses a valid handle or falls back to generic resolution and refreshes
	/// it.
	///
	/// # Errors
	///
	/// Returns the same errors as [`Self::get_or_load`].
	pub(super) fn get_resolved_or_load<Loader>(
		&self,
		handle: &mut VmResolvedHandle,
		key: PageCacheKey,
		loader: Loader,
	) -> FerrosGrynnResult<VmPageGuard<'_>>
	where
		Loader: FnOnce(&mut AlignedPage) -> FerrosGrynnResult<PageHeader>,
	{
		if handle.key() == key {
			if let Some(guard) = self.get_resolved(handle)? {
				return Ok(guard);
			}
		}
		let guard = self.get_or_load(key, loader)?;
		*handle = self.resolve(key).ok_or_else(|| {
			FerrosGrynnError::storage_io_failure(
				"VM page was not resolvable immediately after a pinned load",
			)
		})?;
		Ok(guard)
	}

	/// Returns the total reserved home address-space bytes.
	pub(super) fn reserved_bytes(&self) -> usize {
		self.sources.iter().map(|source| source.reservation.byte_len).sum()
	}

	/// Returns currently committed home bytes.
	pub(super) fn committed_bytes(&self) -> usize {
		self.committed_pages.load(Ordering::Acquire) * PAGE_SIZE
	}

	/// Returns currently resident exact WAL pages.
	pub(super) fn wal_resident_count(&self) -> usize {
		self.wal.as_ref().map_or(0, AtomicArenaBufferPool::resident_count)
	}

	/// Resolves source and page indexes for an exact registered home key.
	fn locate_home(&self, key: PageCacheKey) -> FerrosGrynnResult<(usize, usize)> {
		if !matches!(key.version(), PageCacheVersion::Home { .. }) {
			return Err(FerrosGrynnError::storage_io_failure(
				"WAL key cannot use a fixed home VM address",
			));
		}
		let source_index =
			self.source_index.get(&HomeSourceKey::from_key(key)).copied().ok_or_else(|| {
				FerrosGrynnError::storage_io_failure(
					"exact home source epoch was not registered in the VM cache",
				)
			})?;
		let page_index = usize::try_from(key.page_no().get()).map_err(|source| {
			FerrosGrynnError::storage_io_failure(format!(
				"VM page number does not fit usize: {source}"
			))
		})?;
		if page_index >= self.sources[source_index].metadata.len() {
			return Err(FerrosGrynnError::invalid_storage_page(
				key.page_no().get(),
				"VM page number is outside its registered home file",
			));
		}
		Ok((source_index, page_index))
	}

	/// Generic fixed-address home path.
	fn get_or_load_home<Loader>(
		&self,
		key: PageCacheKey,
		loader: Loader,
	) -> FerrosGrynnResult<VmHomeGuard<'_>>
	where
		Loader: FnOnce(&mut AlignedPage) -> FerrosGrynnResult<PageHeader>,
	{
		self.get_or_load_home_with_loading_hook(key, loader, |_| {})
	}

	/// Generic fixed-address home path with a deterministic loading-race hook.
	fn get_or_load_home_with_loading_hook<Loader, LoadingObserved>(
		&self,
		key: PageCacheKey,
		loader: Loader,
		mut loading_observed: LoadingObserved,
	) -> FerrosGrynnResult<VmHomeGuard<'_>>
	where
		Loader: FnOnce(&mut AlignedPage) -> FerrosGrynnResult<PageHeader>,
		LoadingObserved: FnMut(u64),
	{
		let (source_index, page_index) = self.locate_home(key)?;
		let mut loader = Some(loader);
		loop {
			let meta = &self.sources[source_index].metadata[page_index];
			let state = meta.state.load(Ordering::Acquire);
			match lifecycle(state) {
				VmLifecycle::Resident => {
					let locator = HomeLocator {
						source_index,
						page_index,
						generation: generation(state),
					};
					match self.try_pin_home(locator) {
						VmPinResult::Pinned => return Ok(self.home_guard(locator)?),
						VmPinResult::Stale => continue,
						VmPinResult::Exhausted => return Err(pin_exhausted(key)),
					}
				}
				VmLifecycle::Loading => {
					let observed_generation = generation(state);
					loading_observed(observed_generation);
					let Some(signal) = meta.loading_signal(observed_generation) else {
						continue;
					};
					signal.wait()?;
				}
				VmLifecycle::Free => match self.reserve_home(key, source_index, page_index)? {
					HomeReserveDecision::Retry => continue,
					HomeReserveDecision::Resident(locator) => match self.try_pin_home(locator) {
						VmPinResult::Pinned => return Ok(self.home_guard(locator)?),
						VmPinResult::Stale => continue,
						VmPinResult::Exhausted => return Err(pin_exhausted(key)),
					},
					HomeReserveDecision::Loading(signal) => signal.wait()?,
					HomeReserveDecision::Reserved(reservation) => {
						let Some(loader) = loader.take() else {
							let error = FerrosGrynnError::storage_io_failure(
								"VM loader was consumed before reservation",
							);
							return Err(self.abort_home_load(&reservation, error));
						};
						return self.load_and_publish_home(reservation, loader);
					}
				},
				VmLifecycle::Evicting => {
					self.retry_quarantined_home(HomeLocator {
						source_index,
						page_index,
						generation: generation(state),
					})?;
				}
			}
		}
	}

	/// Serializes home miss admission and global replacement only.
	fn reserve_home(
		&self,
		key: PageCacheKey,
		source_index: usize,
		page_index: usize,
	) -> FerrosGrynnResult<HomeReserveDecision> {
		let meta = &self.sources[source_index].metadata[page_index];
		let mut replacement = self.replacement.lock();
		let mut registration = None;
		loop {
			let state = meta.state.load(Ordering::SeqCst);
			match lifecycle(state) {
				VmLifecycle::Resident => {
					return Ok(HomeReserveDecision::Resident(HomeLocator {
						source_index,
						page_index,
						generation: generation(state),
					}));
				}
				VmLifecycle::Loading => {
					return Ok(match meta.loading_signal(generation(state)) {
						Some(signal) => HomeReserveDecision::Loading(signal),
						None => HomeReserveDecision::Retry,
					});
				}
				VmLifecycle::Evicting => {
					let locator = HomeLocator {
						source_index,
						page_index,
						generation: generation(state),
					};
					self.cleanup_quarantined_home_locked(&mut replacement, locator)?;
					continue;
				}
				VmLifecycle::Free => {}
			}

			let next_generation = next_generation(state, key)?;
			let signal = Arc::new(VmLoadSignal::new());
			if let Some(slot_index) = self.select_home_slot(&mut replacement)? {
				let address = match self.sources[source_index].reservation.commit(page_index) {
					Ok(address) => address,
					Err(error) => {
						self.capacity_available.notify_all();
						return Err(error);
					}
				};
				debug_assert_eq!(
					address.as_ptr() as usize,
					self.sources[source_index].reservation.page_address(page_index)?.as_ptr()
						as usize
				);
				*meta.signal.lock() = Some((next_generation, Arc::clone(&signal)));
				meta.state.store(
					pack_state(VmLifecycle::Loading, next_generation, 0, 0),
					Ordering::Release,
				);
				let locator = HomeLocator {
					source_index,
					page_index,
					generation: next_generation,
				};
				replacement.slots[slot_index] = Some(locator);
				self.committed_pages.fetch_add(1, Ordering::Release);
				return Ok(HomeReserveDecision::Reserved(HomeLoadReservation {
					key,
					locator,
					slot_index,
					signal,
				}));
			}

			if registration.is_none() {
				registration = Some(VmCapacityWaiter::new(&self.capacity_waiters));
				continue;
			}
			self.capacity_available.wait(&mut replacement);
		}
	}

	/// Retries cleanup of one committed page quarantined after rollback failed.
	fn retry_quarantined_home(&self, locator: HomeLocator) -> FerrosGrynnResult<()> {
		let mut replacement = self.replacement.lock();
		let cleaned = self.cleanup_quarantined_home_locked(&mut replacement, locator)?;
		drop(replacement);
		if cleaned.is_some() {
			self.capacity_available.notify_all();
		}
		Ok(())
	}

	/// Decommits one persistent `Evicting` page while replacement is locked.
	///
	/// A transient eviction cannot reach this method: it owns the same
	/// replacement mutex until it either restores `Resident` or stores `Free`.
	/// Therefore an `Evicting` state observed under this mutex is a quarantined
	/// rollback whose native decommit must be retried synchronously.
	fn cleanup_quarantined_home_locked(
		&self,
		replacement: &mut HomeReplacement,
		locator: HomeLocator,
	) -> FerrosGrynnResult<Option<usize>> {
		let meta = &self.sources[locator.source_index].metadata[locator.page_index];
		let state = meta.state.load(Ordering::SeqCst);
		if lifecycle(state) != VmLifecycle::Evicting || generation(state) != locator.generation {
			return Ok(None);
		}
		let slot_index =
			replacement.slots.iter().position(|current| *current == Some(locator)).ok_or_else(
				|| {
					FerrosGrynnError::storage_io_failure(
						"VM quarantined home page lost its committed-budget slot",
					)
				},
			)?;
		self.sources[locator.source_index].reservation.decommit(locator.page_index).map_err(
			|source| {
				FerrosGrynnError::storage_io_failure(format!(
					"VM quarantined home-page cleanup retry failed for source {} page {}: {source}",
					locator.source_index, locator.page_index
				))
			},
		)?;
		meta.state
			.store(pack_state(VmLifecycle::Free, locator.generation, 0, 0), Ordering::Release);
		*meta.signal.lock() = None;
		replacement.slots[slot_index] = None;
		let previous = self.committed_pages.fetch_sub(1, Ordering::Release);
		debug_assert!(previous > 0);
		Ok(Some(slot_index))
	}

	/// Finds a free budget slot or decommits one cold unpinned page.
	fn select_home_slot(
		&self,
		replacement: &mut HomeReplacement,
	) -> FerrosGrynnResult<Option<usize>> {
		let capacity = replacement.slots.len();
		let scan_limit = capacity.saturating_mul(CLOCK_SCAN_PASSES);
		for _ in 0..scan_limit {
			let slot_index = replacement.hand;
			replacement.hand = (replacement.hand + 1) % capacity;
			let Some(locator) = replacement.slots[slot_index] else {
				return Ok(Some(slot_index));
			};
			let meta = &self.sources[locator.source_index].metadata[locator.page_index];
			let state = meta.state.load(Ordering::SeqCst);
			if generation(state) != locator.generation {
				return Err(FerrosGrynnError::storage_io_failure(
					"VM replacement slot retained a stale generation",
				));
			}
			match lifecycle(state) {
				VmLifecycle::Resident if pins(state) == 0 && usage(state) > 0 => {
					let cooled = state - (1_u64 << USAGE_SHIFT);
					let _ = meta.state.compare_exchange(
						state,
						cooled,
						Ordering::AcqRel,
						Ordering::Acquire,
					);
				}
				VmLifecycle::Resident if pins(state) == 0 => {
					let evicting = with_lifecycle(state, VmLifecycle::Evicting);
					if meta
						.state
						.compare_exchange(state, evicting, Ordering::AcqRel, Ordering::Acquire)
						.is_err()
					{
						continue;
					}
					if let Err(error) =
						self.sources[locator.source_index].reservation.decommit(locator.page_index)
					{
						meta.state.store(state, Ordering::Release);
						return Err(error);
					}
					meta.state.store(
						pack_state(VmLifecycle::Free, locator.generation, 0, 0),
						Ordering::Release,
					);
					*meta.signal.lock() = None;
					replacement.slots[slot_index] = None;
					self.committed_pages.fetch_sub(1, Ordering::Release);
					return Ok(Some(slot_index));
				}
				VmLifecycle::Evicting => {
					let cleaned = self.cleanup_quarantined_home_locked(replacement, locator)?;
					if let Some(cleaned_slot) = cleaned {
						return Ok(Some(cleaned_slot));
					}
				}
				VmLifecycle::Free => {
					return Err(FerrosGrynnError::storage_io_failure(
						"VM free page still occupied a replacement slot",
					));
				}
				VmLifecycle::Loading | VmLifecycle::Resident => {}
			}
		}
		Ok(None)
	}

	/// Loads, validates, and publishes one uniquely owned committed page.
	fn load_and_publish_home<Loader>(
		&self,
		reservation: HomeLoadReservation,
		loader: Loader,
	) -> FerrosGrynnResult<VmHomeGuard<'_>>
	where
		Loader: FnOnce(&mut AlignedPage) -> FerrosGrynnResult<PageHeader>,
	{
		let address = self.home_address(reservation.locator)?;
		// SAFETY: this reservation uniquely owns a committed `Loading` page;
		// readers cannot pin it and replacement skips loading slots.
		let page = unsafe { &mut *address.as_ptr() };
		let loaded = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| loader(page)));
		let result = match loaded {
			Ok(result) => result.and_then(|reported| {
				let actual = page.validate(reservation.key.page_no())?;
				validate_loaded_header(reservation.key, actual)?;
				if actual != reported {
					return Err(FerrosGrynnError::invalid_storage_page(
						reservation.key.page_no().get(),
						"VM loader header does not match its current page bytes",
					));
				}
				Ok(())
			}),
			Err(payload) => {
				let _rollback_error = self.abort_home_load(
					&reservation,
					FerrosGrynnError::storage_io_failure("VM home loader panicked"),
				);
				std::panic::resume_unwind(payload);
			}
		};
		if let Err(error) = result {
			return Err(self.abort_home_load(&reservation, error));
		}

		let meta = &self.sources[reservation.locator.source_index].metadata
			[reservation.locator.page_index];
		let loading = pack_state(VmLifecycle::Loading, reservation.locator.generation, 0, 0);
		let resident = pack_state(VmLifecycle::Resident, reservation.locator.generation, 1, 1);
		if meta
			.state
			.compare_exchange(loading, resident, Ordering::Release, Ordering::Acquire)
			.is_err()
		{
			let error =
				FerrosGrynnError::storage_io_failure("VM loading state changed before publication");
			return Err(self.abort_home_load(&reservation, error));
		}
		reservation.signal.complete(Ok(()));
		self.home_guard(reservation.locator)
	}

	/// Decommits one failed unpublished page or quarantines it for retry.
	fn abort_home_load(
		&self,
		reservation: &HomeLoadReservation,
		error: FerrosGrynnError,
	) -> FerrosGrynnError {
		let mut replacement = self.replacement.lock();
		let meta = &self.sources[reservation.locator.source_index].metadata
			[reservation.locator.page_index];
		let mut terminal_error = error.clone();
		let slot_matches = replacement.slots.get(reservation.slot_index).copied().flatten()
			== Some(reservation.locator);
		let state = meta.state.load(Ordering::Acquire);
		if slot_matches
			&& lifecycle(state) == VmLifecycle::Loading
			&& generation(state) == reservation.locator.generation
		{
			match self.sources[reservation.locator.source_index]
				.reservation
				.decommit(reservation.locator.page_index)
			{
				Ok(()) => {
					meta.state.store(
						pack_state(VmLifecycle::Free, reservation.locator.generation, 0, 0),
						Ordering::Release,
					);
					replacement.slots[reservation.slot_index] = None;
					let previous = self.committed_pages.fetch_sub(1, Ordering::Release);
					debug_assert!(previous > 0);
				}
				Err(cleanup_error) => {
					meta.state.store(
						pack_state(VmLifecycle::Evicting, reservation.locator.generation, 0, 0),
						Ordering::Release,
					);
					terminal_error = FerrosGrynnError::storage_io_failure(format!(
						"VM home-page load failed and rollback decommit failed; source {} page {} \
						 remains quarantined: cleanup error: {cleanup_error}; load error: {error}",
						reservation.locator.source_index, reservation.locator.page_index
					));
				}
			}
		}
		meta.clear_loading_signal(reservation.locator.generation, &reservation.signal);
		drop(replacement);
		reservation.signal.complete(Err(terminal_error.clone()));
		self.capacity_available.notify_all();
		terminal_error
	}

	/// Atomically pins only one exact resident home generation.
	fn try_pin_home(&self, locator: HomeLocator) -> VmPinResult {
		let Some(source) = self.sources.get(locator.source_index) else {
			return VmPinResult::Stale;
		};
		let Some(meta) = source.metadata.get(locator.page_index) else {
			return VmPinResult::Stale;
		};
		let mut observed = meta.state.load(Ordering::Acquire);
		loop {
			if lifecycle(observed) != VmLifecycle::Resident
				|| generation(observed) != locator.generation
			{
				return VmPinResult::Stale;
			}
			if pins(observed) == MAX_PINS {
				return VmPinResult::Exhausted;
			}
			let next_usage = usage(observed).saturating_add(1).min(MAX_USAGE);
			let desired = with_pins_and_usage(observed, pins(observed) + 1, next_usage);
			match meta.state.compare_exchange_weak(
				observed,
				desired,
				Ordering::Acquire,
				Ordering::Acquire,
			) {
				Ok(_) => return VmPinResult::Pinned,
				Err(current) => observed = current,
			}
		}
	}

	/// Builds one borrowed guard after a successful pin or publication pin.
	fn home_guard(&self, locator: HomeLocator) -> FerrosGrynnResult<VmHomeGuard<'_>> {
		let address = self.home_address(locator)?.as_ptr() as usize;
		Ok(VmHomeGuard {
			cache: self,
			locator,
			address,
		})
	}

	/// Returns the fixed address for a valid home locator without dereferencing
	/// it.
	fn home_address(&self, locator: HomeLocator) -> FerrosGrynnResult<NonNull<AlignedPage>> {
		self.sources
			.get(locator.source_index)
			.ok_or_else(|| {
				FerrosGrynnError::storage_io_failure("VM source locator is out of range")
			})?
			.reservation
			.page_address(locator.page_index)
	}

	/// Releases one home pin and wakes replacement only when it may be blocked.
	fn release_home_pin(&self, locator: HomeLocator) {
		let meta = &self.sources[locator.source_index].metadata[locator.page_index];
		let previous = meta.state.fetch_sub(1_u64 << PIN_SHIFT, Ordering::SeqCst);
		debug_assert_eq!(lifecycle(previous), VmLifecycle::Resident);
		debug_assert_eq!(generation(previous), locator.generation);
		debug_assert!(pins(previous) > 0);
		if pins(previous) != 1 || self.capacity_waiters.load(Ordering::SeqCst) == 0 {
			return;
		}
		// The waiter registers and rechecks VM state with `SeqCst` while holding
		// `replacement`. Therefore either that recheck observes this last-pin
		// release or this load observes the registration; taking the mutex then
		// closes the condvar enqueue window.
		let _replacement = self.replacement.lock();
		if self.capacity_waiters.load(Ordering::SeqCst) > 0 {
			self.capacity_available.notify_one();
		}
	}
}

/// Borrowed immutable pin over one committed fixed home address.
pub(super) struct VmHomeGuard<'cache> {
	/// Cache that owns the source reservation and metadata.
	cache: &'cache VmCache,
	/// Exact pinned page generation.
	locator: HomeLocator,
	/// Fixed precomputed address valid while this pin exists.
	address: usize,
}

impl VmHomeGuard<'_> {
	/// Returns the immutable committed page protected by this guard's pin.
	fn as_page(&self) -> &AlignedPage {
		let meta = &self.cache.sources[self.locator.source_index].metadata[self.locator.page_index];
		let state = meta.state.load(Ordering::Acquire);
		debug_assert_eq!(lifecycle(state), VmLifecycle::Resident);
		debug_assert_eq!(generation(state), self.locator.generation);
		debug_assert!(pins(state) > 0);
		// SAFETY: the exact resident generation remains pinned for this guard,
		// so replacement cannot decommit or make this address writable.
		unsafe { &*(self.address as *const AlignedPage) }
	}
}

impl Drop for VmHomeGuard<'_> {
	/// Releases the exact home-generation pin.
	fn drop(&mut self) {
		self.cache.release_home_pin(self.locator);
	}
}

/// Borrowed page guard over either fixed home VM or bounded WAL slab storage.
pub(super) enum VmPageGuard<'cache> {
	/// Fixed home address.
	Home(VmHomeGuard<'cache>),
	/// Stable WAL arena frame.
	Wal(AtomicPageGuard<'cache>),
}

impl VmPageGuard<'_> {
	/// Returns the pinned immutable page.
	pub(super) fn as_page(&self) -> &AlignedPage {
		match self {
			Self::Home(guard) => guard.as_page(),
			Self::Wal(guard) => guard.as_page(),
		}
	}

	/// Returns the pinned immutable page bytes.
	pub(super) fn as_bytes(&self) -> &[u8; PAGE_SIZE] {
		self.as_page().as_bytes()
	}
}

/// Packs complete home-page state.
const fn pack_state(lifecycle: VmLifecycle, generation: u64, pins: u64, usage: u64) -> u64 {
	(lifecycle as u64)
		| (pins << PIN_SHIFT)
		| (usage << USAGE_SHIFT)
		| (generation << GENERATION_SHIFT)
}

/// Decodes the closed lifecycle field.
const fn lifecycle(raw: u64) -> VmLifecycle {
	match raw & LIFECYCLE_MASK {
		0 => VmLifecycle::Free,
		1 => VmLifecycle::Loading,
		2 => VmLifecycle::Resident,
		3 => VmLifecycle::Evicting,
		_ => VmLifecycle::Evicting,
	}
}

/// Decodes the generation field.
const fn generation(raw: u64) -> u64 {
	raw >> GENERATION_SHIFT
}

/// Decodes the pin-count field.
const fn pins(raw: u64) -> u64 {
	(raw & PIN_MASK) >> PIN_SHIFT
}

/// Decodes the replacement-usage field.
const fn usage(raw: u64) -> u64 {
	(raw & USAGE_MASK) >> USAGE_SHIFT
}

/// Replaces only the lifecycle field.
const fn with_lifecycle(raw: u64, lifecycle: VmLifecycle) -> u64 {
	(raw & !LIFECYCLE_MASK) | lifecycle as u64
}

/// Replaces pin and usage fields for one CAS target.
const fn with_pins_and_usage(raw: u64, pins: u64, usage: u64) -> u64 {
	(raw & !(PIN_MASK | USAGE_MASK)) | (pins << PIN_SHIFT) | (usage << USAGE_SHIFT)
}

/// Advances a page generation without ABA-producing wraparound.
fn next_generation(raw: u64, key: PageCacheKey) -> FerrosGrynnResult<u64> {
	let current = generation(raw);
	if current == MAX_GENERATION {
		return Err(FerrosGrynnError::invalid_storage_page(
			key.page_no().get(),
			"VM page generation is exhausted",
		));
	}
	Ok(current + 1)
}

/// Builds an error for an impossible number of simultaneous page guards.
fn pin_exhausted(key: PageCacheKey) -> FerrosGrynnError {
	FerrosGrynnError::storage_io_failure(format!(
		"VM pin counter is exhausted for page {}",
		key.page_no().get()
	))
}

/// Preserves a native memory error behind a project storage error.
fn windows_failure(operation: &'static str, source: windows::core::Error) -> FerrosGrynnError {
	FerrosGrynnError::storage_io_failure(operation).with_technical_source(source)
}

#[cfg(test)]
#[allow(
	clippy::panic_in_result_fn,
	reason = "fallible VM tests return Result while assertions report contract failures"
)]
mod tests {
	use std::{
		error::Error,
		num::NonZeroUsize,
		sync::{
			Arc,
			Barrier,
			atomic::{AtomicUsize, Ordering},
			mpsc,
		},
		thread,
		time::Duration,
	};

	use super::{HomeSourceSpec, VmCache, VmReservation, probe_vm_lifecycle};
	use crate::{
		gql_status::{FerrosGrynnError, FerrosGrynnResult},
		storage::{
			buffer_pool::{PageCacheKey, PageCacheVersion},
			page::{AlignedPage, PageFlags, PageHeader, PageLsn, PageNo, PageType},
			types::{CheckpointGeneration, DataFileId, DataFileKind, GraphId, Lsn, PageKey},
		},
	};

	/// Creates one exact home key for deterministic VM tests.
	fn home_key(
		file_instance: u64,
		generation: u64,
		page_no: u64,
	) -> FerrosGrynnResult<PageCacheKey> {
		let file_id = DataFileId::new(file_instance, DataFileKind::NodesHot).ok_or_else(|| {
			FerrosGrynnError::invalid_storage_file("VM test file identity is out of range")
		})?;
		Ok(PageCacheKey::new(
			PageKey::new(GraphId::new(1), file_id, PageNo::new(page_no)),
			PageCacheVersion::Home {
				generation: CheckpointGeneration::new(generation),
				ceiling: Lsn::new(100),
			},
		))
	}

	/// Creates one exact WAL key sharing a physical home-page identity.
	fn wal_key(file_instance: u64, page_no: u64, lsn: u64) -> FerrosGrynnResult<PageCacheKey> {
		let file_id = DataFileId::new(file_instance, DataFileKind::NodesHot).ok_or_else(|| {
			FerrosGrynnError::invalid_storage_file("VM test WAL identity is out of range")
		})?;
		Ok(PageCacheKey::new(
			PageKey::new(GraphId::new(1), file_id, PageNo::new(page_no)),
			PageCacheVersion::Wal(PageLsn::new(lsn)),
		))
	}

	/// Fills one valid page matching its exact physical page number.
	fn load(key: PageCacheKey, page: &mut AlignedPage) -> FerrosGrynnResult<PageHeader> {
		let page_lsn = match key.version() {
			PageCacheVersion::Wal(lsn) => lsn,
			PageCacheVersion::Home {
				..
			} => PageLsn::new(10),
		};
		*page = AlignedPage::new(PageType::NodeHot, PageFlags::empty(), page_lsn, key.page_no());
		page.validate(key.page_no())
	}

	#[test]
	fn reservation_recommits_the_same_fixed_address() -> Result<(), Box<dyn Error>> {
		let reservation = VmReservation::reserve(2)?;
		let first = reservation.commit(1)?;
		// SAFETY: this page is committed, exclusively owned by the test, and
		// correctly aligned for `AlignedPage`.
		unsafe { first.as_ptr().write(AlignedPage::zeroed()) };
		reservation.decommit(1)?;
		let second = reservation.commit(1)?;
		assert_eq!(first, second);
		// SAFETY: same proof as the first initialized write.
		unsafe { second.as_ptr().write(AlignedPage::zeroed()) };
		reservation.decommit(1)?;
		Ok(())
	}

	#[test]
	fn vm_lifecycle_component_probe_completes() -> Result<(), Box<dyn Error>> {
		let sample = probe_vm_lifecycle()?;
		std::hint::black_box((
			sample.reserve,
			sample.commit,
			sample.first_touch,
			sample.decommit,
			sample.recommit,
		));
		Ok(())
	}

	#[test]
	fn home_resolved_handle_stales_after_decommit_and_recommit() -> Result<(), Box<dyn Error>> {
		let key0 = home_key(1, 1, 0)?;
		let key1 = home_key(1, 1, 1)?;
		let source = HomeSourceSpec::from_key(key0, NonZeroUsize::new(2).ok_or("zero")?)?;
		let cache = VmCache::new(&[source], NonZeroUsize::new(1).ok_or("zero")?, 0)?;
		let guard = cache.get_or_load(key0, |page| load(key0, page))?;
		let old_address = guard.as_page() as *const AlignedPage as usize;
		drop(guard);
		let old_handle = cache.resolve(key0).ok_or("missing first handle")?;
		drop(cache.get_or_load(key1, |page| load(key1, page))?);
		assert!(cache.get_resolved(&old_handle)?.is_none());
		let reloaded = cache.get_or_load(key0, |page| load(key0, page))?;
		assert_eq!(old_address, reloaded.as_page() as *const AlignedPage as usize);
		assert_eq!(cache.committed_bytes(), crate::storage::page::PAGE_SIZE);
		Ok(())
	}

	#[test]
	fn vm_direct_to_frame_batch_publishes_all_or_none() -> Result<(), Box<dyn Error>> {
		let keys = [home_key(1, 1, 0)?, home_key(1, 1, 1)?, home_key(1, 1, 2)?];
		let source = HomeSourceSpec::from_key(keys[0], NonZeroUsize::new(8).ok_or("zero")?)?;
		let cache = VmCache::new(&[source], NonZeroUsize::new(4).ok_or("zero")?, 0)?;
		let mut batch = cache.reserve_home_batch(&keys)?;
		let _headers = batch
			.pages_mut()?
			.into_iter()
			.zip(keys)
			.map(|(page, key)| load(key, page))
			.collect::<FerrosGrynnResult<Vec<_>>>()?;
		assert!(keys.iter().all(|key| cache.resolve(*key).is_none()));
		let guards = batch.publish()?;
		for (guard, key) in guards.iter().zip(keys) {
			assert_eq!(guard.as_page().validate(key.page_no())?.page_no(), key.page_no());
		}
		drop(guards);
		assert!(keys.iter().all(|key| cache.resolve(*key).is_some()));
		let duplicate = home_key(1, 1, 5)?;
		assert!(cache.reserve_home_batch(&[duplicate, duplicate]).is_err());
		assert!(cache.resolve(duplicate).is_none());

		let failed_keys = [home_key(1, 1, 6)?, home_key(1, 1, 7)?];
		let mut failed = cache.reserve_home_batch(&failed_keys)?;
		let _failed_headers = failed
			.pages_mut()?
			.into_iter()
			.zip(failed_keys)
			.map(|(page, key)| load(key, page))
			.collect::<FerrosGrynnResult<Vec<_>>>()?;
		let mut failed_pages = failed.pages_mut()?;
		failed_pages[0].as_bytes_mut()[0] ^= 0xff;
		drop(failed_pages);
		assert!(failed.publish().is_err());
		assert!(failed_keys.iter().all(|key| cache.resolve(*key).is_none()));
		Ok(())
	}

	#[test]
	fn concurrent_vm_batches_cannot_retain_mutually_blocking_partial_capacity()
	-> Result<(), Box<dyn Error>> {
		let sample = home_key(1, 1, 0)?;
		let source = HomeSourceSpec::from_key(sample, NonZeroUsize::new(4).ok_or("zero")?)?;
		let cache =
			Arc::new(VmCache::new(&[source], NonZeroUsize::new(2).ok_or("zero capacity")?, 0)?);
		let start = Arc::new(Barrier::new(3));
		let (acquired_tx, acquired_rx) = mpsc::channel();
		let (release_zero_tx, release_zero_rx) = mpsc::channel();
		let (release_one_tx, release_one_rx) = mpsc::channel();
		let (done_tx, done_rx) = mpsc::channel();

		let zero_cache = Arc::clone(&cache);
		let zero_start = Arc::clone(&start);
		let zero_acquired = acquired_tx.clone();
		let zero_done = done_tx.clone();
		let zero_keys = [home_key(1, 1, 0)?, home_key(1, 1, 1)?];
		let zero = thread::spawn(move || {
			zero_start.wait();
			let result = zero_cache
				.reserve_home_batch_with_hook(&zero_keys, |count| {
					if count == 1 {
						assert!(zero_acquired.send(0).is_ok());
						assert!(release_zero_rx.recv().is_ok());
					}
				})
				.map(drop);
			let _ = zero_done.send(result);
		});

		let one_cache = Arc::clone(&cache);
		let one_start = Arc::clone(&start);
		let one_acquired = acquired_tx;
		let one_keys = [home_key(1, 1, 2)?, home_key(1, 1, 3)?];
		let one = thread::spawn(move || {
			one_start.wait();
			let result = one_cache
				.reserve_home_batch_with_hook(&one_keys, |count| {
					if count == 1 {
						assert!(one_acquired.send(1).is_ok());
						assert!(release_one_rx.recv().is_ok());
					}
				})
				.map(drop);
			let _ = done_tx.send(result);
		});

		start.wait();
		let first = acquired_rx.recv_timeout(Duration::from_secs(1))?;
		assert!(matches!(
			acquired_rx.recv_timeout(Duration::from_millis(30)),
			Err(mpsc::RecvTimeoutError::Timeout)
		));
		match first {
			0 => release_zero_tx.send(())?,
			1 => release_one_tx.send(())?,
			_ => return Err("unknown VM batch worker".into()),
		}
		let second = acquired_rx.recv_timeout(Duration::from_secs(1))?;
		assert_ne!(first, second);
		match second {
			0 => release_zero_tx.send(())?,
			1 => release_one_tx.send(())?,
			_ => return Err("unknown VM batch worker".into()),
		}
		done_rx.recv_timeout(Duration::from_secs(1))??;
		done_rx.recv_timeout(Duration::from_secs(1))??;
		zero.join().map_err(|payload| {
			drop(payload);
			"first VM batch worker panicked"
		})?;
		one.join().map_err(|payload| {
			drop(payload);
			"second VM batch worker panicked"
		})?;
		assert_eq!(cache.committed_bytes(), 0);
		Ok(())
	}

	#[test]
	fn panicking_vm_loader_rolls_back_committed_capacity() -> Result<(), Box<dyn Error>> {
		let key = home_key(1, 1, 0)?;
		let source = HomeSourceSpec::from_key(key, NonZeroUsize::MIN)?;
		let cache = VmCache::new(&[source], NonZeroUsize::new(2).ok_or("zero")?, 0)?;
		let panicked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
			let _ = cache.get_or_load(key, |_| -> FerrosGrynnResult<PageHeader> {
				panic!("intentional VM loader panic");
			});
		}));
		assert!(panicked.is_err());
		assert_eq!(cache.committed_bytes(), 0);
		drop(cache.get_or_load(key, |page| load(key, page))?);
		Ok(())
	}

	#[test]
	fn vm_bytes_corrupted_after_header_validation_are_never_published() -> Result<(), Box<dyn Error>>
	{
		let key = home_key(1, 1, 0)?;
		let source = HomeSourceSpec::from_key(key, NonZeroUsize::MIN)?;
		let cache = VmCache::new(&[source], NonZeroUsize::MIN, 0)?;
		let result = cache.get_or_load(key, |page| {
			let header = load(key, page)?;
			page.as_bytes_mut()[0] ^= 0xff;
			Ok(header)
		});
		assert!(result.is_err());
		assert!(cache.resolve(key).is_none());
		assert_eq!(cache.committed_bytes(), 0);
		Ok(())
	}

	#[test]
	fn distinct_failed_home_pages_do_not_retain_source_sized_signals() -> Result<(), Box<dyn Error>>
	{
		const PAGE_COUNT: usize = 32;
		let first = home_key(1, 1, 0)?;
		let source = HomeSourceSpec::from_key(first, NonZeroUsize::new(PAGE_COUNT).ok_or("zero")?)?;
		let cache = VmCache::new(&[source], NonZeroUsize::new(2).ok_or("zero")?, 0)?;

		for raw_page in 0..PAGE_COUNT as u64 {
			let key = home_key(1, 1, raw_page)?;
			let result = cache.get_or_load(key, |page| {
				let header = load(key, page)?;
				page.as_bytes_mut()[0] ^= 0xff;
				Ok(header)
			});
			assert!(result.is_err());
			assert_eq!(cache.committed_bytes(), 0);
		}

		assert!(cache.sources[0].metadata.iter().all(|meta| meta.signal.lock().is_none()));
		assert!(cache.sources[0].metadata.iter().all(|meta| {
			super::lifecycle(meta.state.load(Ordering::Acquire)) == super::VmLifecycle::Free
		}));
		assert!(cache.replacement.lock().slots.iter().all(Option::is_none));
		Ok(())
	}

	#[test]
	fn stale_loading_signal_observation_retries_after_reuse() -> Result<(), Box<dyn Error>> {
		let key = home_key(1, 1, 0)?;
		let source = HomeSourceSpec::from_key(key, NonZeroUsize::MIN)?;
		let cache = Arc::new(VmCache::new(&[source], NonZeroUsize::MIN, 0)?);
		let waiter_loads = Arc::new(AtomicUsize::new(0));
		let (owner_started_tx, owner_started_rx) = mpsc::channel();
		let (owner_release_tx, owner_release_rx) = mpsc::channel();
		let (owner_done_tx, owner_done_rx) = mpsc::channel();

		let owner_cache = Arc::clone(&cache);
		let owner = thread::spawn(move || {
			let result = owner_cache
				.get_or_load(key, |_page| {
					let _ = owner_started_tx.send(());
					owner_release_rx.recv().map_err(|source| {
						FerrosGrynnError::storage_io_failure(source.to_string())
					})?;
					Err(FerrosGrynnError::storage_io_failure("intentional VM load failure"))
				})
				.map(drop);
			let _ = owner_done_tx.send(result);
		});
		owner_started_rx.recv_timeout(Duration::from_secs(1))?;

		let (observed_tx, observed_rx) = mpsc::channel();
		let (waiter_release_tx, waiter_release_rx) = mpsc::channel();
		let (waiter_done_tx, waiter_done_rx) = mpsc::channel();
		let waiter_cache = Arc::clone(&cache);
		let waiter_loads_ref = Arc::clone(&waiter_loads);
		let waiter = thread::spawn(move || {
			let mut paused = false;
			let result = waiter_cache
				.get_or_load_home_with_loading_hook(
					key,
					|page| {
						waiter_loads_ref.fetch_add(1, Ordering::Relaxed);
						load(key, page)
					},
					|generation| {
						if !paused {
							paused = true;
							let _ = observed_tx.send(generation);
							let _ = waiter_release_rx.recv();
						}
					},
				)
				.map(drop);
			let _ = waiter_done_tx.send(result);
		});
		let stale_generation = observed_rx.recv_timeout(Duration::from_secs(1))?;
		owner_release_tx.send(())?;
		assert!(owner_done_rx.recv_timeout(Duration::from_secs(1))?.is_err());

		drop(cache.get_or_load(key, |page| load(key, page))?);
		let current_generation =
			super::generation(cache.sources[0].metadata[0].state.load(Ordering::Acquire));
		assert_ne!(stale_generation, current_generation);
		waiter_release_tx.send(())?;
		waiter_done_rx.recv_timeout(Duration::from_secs(1))??;
		assert_eq!(waiter_loads.load(Ordering::Relaxed), 0);

		owner.join().map_err(|payload| {
			drop(payload);
			"VM signal-race owner panicked"
		})?;
		waiter.join().map_err(|payload| {
			drop(payload);
			"VM signal-race waiter panicked"
		})?;
		Ok(())
	}

	#[test]
	fn failed_decommit_quarantines_without_hanging_and_retries_cleanup()
	-> Result<(), Box<dyn Error>> {
		let key = home_key(1, 1, 0)?;
		let source = HomeSourceSpec::from_key(key, NonZeroUsize::MIN)?;
		let cache = VmCache::new(&[source], NonZeroUsize::MIN, 0)?;
		cache.sources[0].reservation.inject_decommit_failures(1);

		let first = cache.get_or_load(key, |_page| {
			Err(FerrosGrynnError::storage_io_failure("intentional VM load failure"))
		});
		let first_error = first.err().ok_or("injected rollback failure unexpectedly succeeded")?;
		assert!(first_error.to_string().contains("remains quarantined"));
		assert_eq!(cache.committed_bytes(), crate::storage::page::PAGE_SIZE);
		assert_eq!(
			super::lifecycle(cache.sources[0].metadata[0].state.load(Ordering::Acquire)),
			super::VmLifecycle::Evicting
		);
		assert!(cache.sources[0].metadata[0].signal.lock().is_none());

		cache.sources[0].reservation.inject_decommit_failures(1);
		let loader_calls = AtomicUsize::new(0);
		let retry = cache.get_or_load(key, |page| {
			loader_calls.fetch_add(1, Ordering::Relaxed);
			load(key, page)
		});
		let retry_error =
			retry.err().ok_or("quarantined cleanup failure unexpectedly succeeded")?;
		assert!(retry_error.to_string().contains("quarantined home-page cleanup retry failed"));
		assert_eq!(loader_calls.load(Ordering::Relaxed), 0);
		assert_eq!(cache.committed_bytes(), crate::storage::page::PAGE_SIZE);

		drop(cache.get_or_load(key, |page| {
			loader_calls.fetch_add(1, Ordering::Relaxed);
			load(key, page)
		})?);
		assert_eq!(loader_calls.load(Ordering::Relaxed), 1);
		assert_eq!(cache.committed_bytes(), crate::storage::page::PAGE_SIZE);
		assert_eq!(
			super::lifecycle(cache.sources[0].metadata[0].state.load(Ordering::Acquire)),
			super::VmLifecycle::Resident
		);
		Ok(())
	}

	#[test]
	fn concurrent_vm_miss_is_coalesced_to_one_loader() -> Result<(), Box<dyn Error>> {
		let key = home_key(1, 1, 0)?;
		let source = HomeSourceSpec::from_key(key, NonZeroUsize::MIN)?;
		let cache = VmCache::new(&[source], NonZeroUsize::MIN, 0)?;
		let loaders = AtomicUsize::new(0);
		let (started_tx, started_rx) = mpsc::channel();
		let (release_tx, release_rx) = mpsc::channel();
		let (done_tx, done_rx) = mpsc::channel();

		thread::scope(|scope| {
			let first_done = done_tx.clone();
			let cache_ref = &cache;
			let loaders_ref = &loaders;
			scope.spawn(move || {
				let result = cache_ref
					.get_or_load(key, |page| {
						loaders_ref.fetch_add(1, Ordering::Relaxed);
						let _ = started_tx.send(());
						release_rx.recv().map_err(|error| {
							FerrosGrynnError::storage_io_failure(error.to_string())
						})?;
						load(key, page)
					})
					.map(drop);
				let _ = first_done.send(result);
			});
			assert!(started_rx.recv_timeout(Duration::from_secs(1)).is_ok());
			scope.spawn(|| {
				let result = cache
					.get_or_load(key, |page| {
						loaders.fetch_add(1, Ordering::Relaxed);
						load(key, page)
					})
					.map(drop);
				let _ = done_tx.send(result);
			});
			assert!(matches!(
				done_rx.recv_timeout(Duration::from_millis(30)),
				Err(mpsc::RecvTimeoutError::Timeout)
			));
			assert!(release_tx.send(()).is_ok());
		});
		done_rx.recv_timeout(Duration::from_secs(1))??;
		done_rx.recv_timeout(Duration::from_secs(1))??;
		assert_eq!(loaders.load(Ordering::Relaxed), 1);
		Ok(())
	}

	#[test]
	fn pinned_vm_page_applies_backpressure_until_guard_drop() -> Result<(), Box<dyn Error>> {
		let first = home_key(1, 1, 0)?;
		let second = home_key(1, 1, 1)?;
		let source = HomeSourceSpec::from_key(first, NonZeroUsize::new(2).ok_or("zero")?)?;
		let cache = VmCache::new(&[source], NonZeroUsize::MIN, 0)?;
		let pinned = cache.get_or_load(first, |page| load(first, page))?;
		let (entered_tx, entered_rx) = mpsc::channel();
		let (loader_tx, loader_rx) = mpsc::channel();
		let (done_tx, done_rx) = mpsc::channel();

		thread::scope(|scope| {
			scope.spawn(|| {
				let _ = entered_tx.send(());
				let result = cache
					.get_or_load(second, |page| {
						let _ = loader_tx.send(());
						load(second, page)
					})
					.map(drop);
				let _ = done_tx.send(result);
			});
			assert!(entered_rx.recv_timeout(Duration::from_secs(1)).is_ok());
			assert!(matches!(
				loader_rx.recv_timeout(Duration::from_millis(30)),
				Err(mpsc::RecvTimeoutError::Timeout)
			));
			drop(pinned);
			assert!(loader_rx.recv_timeout(Duration::from_secs(1)).is_ok());
		});
		done_rx.recv_timeout(Duration::from_secs(1))??;
		Ok(())
	}

	#[test]
	fn resolved_home_handle_cannot_cross_vm_cache_ownership() -> Result<(), Box<dyn Error>> {
		let key = home_key(1, 1, 0)?;
		let source = HomeSourceSpec::from_key(key, NonZeroUsize::MIN)?;
		let first = VmCache::new(&[source], NonZeroUsize::new(2).ok_or("zero")?, 0)?;
		let second = VmCache::new(&[source], NonZeroUsize::new(2).ok_or("zero")?, 0)?;
		drop(first.get_or_load(key, |page| load(key, page))?);
		drop(second.get_or_load(key, |page| load(key, page))?);
		let handle = first.resolve(key).ok_or("first VM cache did not resolve")?;
		assert!(second.get_resolved(&handle)?.is_none());
		Ok(())
	}

	#[test]
	fn multiple_files_and_epochs_share_one_home_budget() -> Result<(), Box<dyn Error>> {
		let mut keys = (1..=8)
			.map(|file_instance| home_key(file_instance, 1, 0))
			.collect::<FerrosGrynnResult<Vec<_>>>()?;
		let next_epoch = home_key(1, 2, 0)?;
		keys.push(next_epoch);
		let specs = keys
			.iter()
			.copied()
			.map(|key| HomeSourceSpec::from_key(key, NonZeroUsize::MIN))
			.collect::<FerrosGrynnResult<Vec<_>>>()?;
		let cache = VmCache::new(&specs, NonZeroUsize::new(4).ok_or("zero")?, 0)?;
		for key in keys {
			drop(cache.get_or_load(key, |page| load(key, page))?);
			assert!(cache.committed_bytes() <= 4 * crate::storage::page::PAGE_SIZE);
		}
		assert_eq!(cache.reserved_bytes(), 9 * crate::storage::page::PAGE_SIZE);
		Ok(())
	}

	#[test]
	fn wal_versions_use_the_bounded_atomic_slab() -> Result<(), Box<dyn Error>> {
		let home = home_key(1, 1, 0)?;
		let source = HomeSourceSpec::from_key(home, NonZeroUsize::MIN)?;
		let cache = VmCache::new(&[source], NonZeroUsize::new(2).ok_or("zero")?, 1)?;
		for key in [wal_key(1, 0, 1)?, wal_key(1, 0, 2)?] {
			drop(cache.get_or_load(key, |page| load(key, page))?);
		}
		assert_eq!(cache.wal_resident_count(), 1);
		assert_eq!(cache.committed_bytes(), 0);
		Ok(())
	}

	#[test]
	fn failed_validation_never_leaves_committed_resident_bytes() -> Result<(), Box<dyn Error>> {
		let key = home_key(1, 1, 0)?;
		let source = HomeSourceSpec::from_key(key, NonZeroUsize::MIN)?;
		let cache = VmCache::new(&[source], NonZeroUsize::new(2).ok_or("zero")?, 0)?;
		let result = cache.get_or_load(key, |page| {
			*page = AlignedPage::zeroed();
			page.validate(key.page_no())
		});
		assert!(result.is_err());
		assert!(cache.resolve(key).is_none());
		assert_eq!(cache.committed_bytes(), 0);
		Ok(())
	}
}
