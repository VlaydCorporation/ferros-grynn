//! Hybrid physical-read engine assembled from measured resident and I/O paths.
//!
//! Point reads use the R8 resident arena, contiguous scans bypass admission via
//! validated scatter/prefetch, and known sparse windows may use frame-oriented
//! IoRing. The hidden prototype API exposes measurement hooks only and never
//! selects production behavior.

use std::{num::NonZeroUsize, ptr::NonNull, sync::Arc};

use parking_lot::{Mutex, RwLock};
use rapidhash::{HashMapExt, RapidHashMap};

#[cfg(all(windows, feature = "native-async-io"))]
use super::page_io::{FrameRead, WindowsIoRing};
use super::{
	PageIoMode,
	page::{AlignedPage, PageHeader},
	page_cache::{PageCacheKey, PageCacheVersion},
	types::DataFileId,
};
#[cfg(windows)]
use super::{direct_io::DirectFile, page::PageNo};
use crate::{
	datastore::DataSpaceId,
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
};

mod arena;
mod route;
#[cfg(windows)]
mod windows_scatter;

#[cfg(feature = "storage-engine-prototype-api")]
pub use arena::AtomicBatchReservation;
pub use arena::{
	AtomicArenaBufferPool,
	AtomicEpochPage,
	AtomicPageGuard,
	ResolvedFrameHandle,
	SourceBoundLocator,
};
#[cfg(feature = "storage-engine-prototype-api")]
pub use route::{MAX_IORING_QUEUE_DEPTH, PREFETCH_DEPTH};
pub use route::{
	MAX_READ_SCOPE_OPERATIONS,
	MAX_SCATTER_PAGES,
	ReadIntent,
	ReadRoute,
	ReadWindow,
	TransportCapabilities,
	route_read,
	scatter_extent_pages,
	sparse_transient_pages,
	transient_pages,
};
#[cfg(windows)]
pub use windows_scatter::ScatterPrefetchCursor;
#[cfg(all(windows, feature = "storage-engine-prototype-api"))]
pub use windows_scatter::ValidatedScatterExtent;

/// Fixed transient-memory partition and its single transport lane.
struct TransientArena {
	/// Total cache-bypassing pages allowed beside resident frames.
	capacity: usize,
	/// Pages in either of the two reusable scatter buffers.
	scatter_extent_pages: usize,
	/// Pages available to one sparse IoRing batch.
	sparse_pages: usize,
	/// One Windows scatter pipeline may run at a time until measured otherwise.
	#[cfg(windows)]
	scatter_lane: Mutex<()>,
	/// Lazily created frame-oriented ring serialized for one bounded batch.
	#[cfg(all(windows, feature = "native-async-io"))]
	io_ring: Mutex<Option<WindowsIoRing>>,
}

impl TransientArena {
	/// Partitions the fixed ten-percent budget without overcommit.
	const fn new(resident_pages: usize) -> Self {
		let capacity = transient_pages(resident_pages);
		Self {
			capacity,
			scatter_extent_pages: scatter_extent_pages(capacity),
			sparse_pages: sparse_transient_pages(capacity),
			#[cfg(windows)]
			scatter_lane: Mutex::new(()),
			#[cfg(all(windows, feature = "native-async-io"))]
			io_ring: Mutex::new(None),
		}
	}
}

/// Fully completed and validated sparse cold-page window.
#[cfg(all(windows, feature = "native-async-io"))]
pub struct SparseReadBatch {
	/// Requested physical page numbers in caller order.
	page_numbers: Vec<PageNo>,
	/// Corresponding validated pages in caller order.
	pages: Vec<AlignedPage>,
}

#[cfg(all(windows, feature = "native-async-io"))]
impl SparseReadBatch {
	/// Returns the number of completed pages.
	pub const fn len(&self) -> usize {
		self.pages.len()
	}

	/// Returns whether the completed window is empty.
	pub const fn is_empty(&self) -> bool {
		self.pages.is_empty()
	}

	/// Returns the physical page number at the caller's position.
	pub fn page_no(&self, index: usize) -> Option<PageNo> {
		self.page_numbers.get(index).copied()
	}

	/// Returns all validated pages in caller order, including duplicates.
	pub const fn pages(&self) -> &[AlignedPage] {
		self.pages.as_slice()
	}
}

/// Stable identity of a physical page source independent of its immutable
/// version.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct SourceIdentity {
	/// Database-global or graph-local address space owning the file.
	space_id: DataSpaceId,
	/// Never-reused physical file identity.
	file_id: DataFileId,
}

impl SourceIdentity {
	/// Extracts source identity from an exact cache key.
	const fn from_key(key: PageCacheKey) -> Self {
		let page_key = key.page_key();
		Self {
			space_id: page_key.space_id(),
			file_id: page_key.file_id(),
		}
	}
}

/// Exact immutable epoch of a physical page source.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct SourceEpoch(PageCacheVersion);

impl SourceEpoch {
	/// Extracts the source epoch from an exact cache key.
	const fn from_key(key: PageCacheKey) -> Self {
		Self(key.version())
	}
}

/// Exact immutable source selected for one bounded read scope.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct SourceState {
	/// Stable graph and file identity.
	identity: SourceIdentity,
	/// Home checkpoint generation or exact WAL LSN.
	epoch: SourceEpoch,
}

impl SourceState {
	/// Extracts exact source state from a resident cache key.
	const fn from_key(key: PageCacheKey) -> Self {
		Self {
			identity: SourceIdentity::from_key(key),
			epoch: SourceEpoch::from_key(key),
		}
	}

	/// Returns whether this is a persistent home-file epoch.
	const fn is_home(self) -> bool {
		matches!(self.epoch.0, PageCacheVersion::Home { .. })
	}
}

/// Lazily created home-source locators retained only for active source epochs.
struct SourceRegistry {
	/// Exact home source state to bounded locator.
	home: RwLock<RapidHashMap<SourceState, Arc<SourceBoundLocator>>>,
}

impl SourceRegistry {
	/// Creates an empty source registry.
	fn new() -> Self {
		Self {
			home: RwLock::new(RapidHashMap::new()),
		}
	}

	/// Returns or creates one exact home-source locator.
	fn home_locator(
		&self,
		arena: &AtomicArenaBufferPool,
		first_key: PageCacheKey,
		page_count: NonZeroUsize,
	) -> FerrosGrynnResult<Arc<SourceBoundLocator>> {
		let state = SourceState::from_key(first_key);
		if !state.is_home() {
			return Err(FerrosGrynnError::storage_io_failure(
				"WAL-exact pages use a bounded cursor-local resolved table",
			));
		}
		if let Some(locator) = self.home.read().get(&state).cloned() {
			return validate_source_page_count(locator, page_count);
		}
		let candidate = Arc::new(SourceBoundLocator::new(arena, first_key, page_count)?);
		let mut home = self.home.write();
		let locator = Arc::clone(home.entry(state).or_insert(candidate));
		validate_source_page_count(locator, page_count)
	}

	/// Retires older home epochs while active scopes retain their own `Arc`.
	fn retire_home_versions(&self, current: SourceState) -> usize {
		let mut removed = 0;
		self.home.write().retain(|state, _| {
			let retain = state.identity != current.identity || *state == current;
			removed += usize::from(!retain);
			retain
		});
		removed
	}

	/// Returns exact home epochs held by the registry.
	fn len(&self) -> usize {
		self.home.read().len()
	}
}

/// Validates that all users agree on one source's physical page range.
fn validate_source_page_count(
	locator: Arc<SourceBoundLocator>,
	page_count: NonZeroUsize,
) -> FerrosGrynnResult<Arc<SourceBoundLocator>> {
	if locator.page_count() == page_count.get() {
		Ok(locator)
	} else {
		Err(FerrosGrynnError::storage_io_failure(
			"home source was reopened with a different physical page count",
		))
	}
}

/// Resident core and bounded transient-memory policy of the hybrid read engine.
pub struct HybridReadEngine {
	/// Fixed R8 arena shared by every graph, file, and page version.
	arena: AtomicArenaBufferPool,
	/// Exact persistent home epochs and their lazy locator chunks.
	sources: SourceRegistry,
	/// Pages available to cache-bypassing transports.
	transient: TransientArena,
}

impl HybridReadEngine {
	/// Creates one engine with a global resident-frame budget.
	pub fn new(buffer_pool_pages: NonZeroUsize) -> Self {
		Self {
			arena: AtomicArenaBufferPool::new(buffer_pool_pages),
			sources: SourceRegistry::new(),
			transient: TransientArena::new(buffer_pool_pages.get()),
		}
	}

	/// Returns the exact resident-frame budget.
	pub const fn resident_capacity(&self) -> usize {
		self.arena.capacity()
	}

	/// Returns the resident capacity through the page-manager cache contract.
	pub(super) const fn capacity(&self) -> usize {
		self.resident_capacity()
	}

	/// Returns the bounded cache-bypassing frame budget.
	pub const fn transient_capacity(&self) -> usize {
		self.transient.capacity
	}

	/// Returns the size of either reusable scatter buffer.
	pub const fn scatter_extent_capacity(&self) -> usize {
		self.transient.scatter_extent_pages
	}

	/// Returns the bounded sparse transport window.
	pub const fn sparse_window_capacity(&self) -> usize {
		self.transient.sparse_pages
	}

	/// Selects a physical path before transport submission.
	pub const fn route(
		&self,
		exact_version_resident: bool,
		intent: ReadIntent,
		window: ReadWindow,
		io_mode: PageIoMode,
		capabilities: TransportCapabilities,
	) -> ReadRoute {
		route_read(
			exact_version_resident,
			intent,
			window,
			io_mode,
			capabilities,
			self.transient.capacity,
		)
	}

	/// Starts one Windows `ReadFileScatter` scan with prefetch depth one.
	///
	/// # Errors
	///
	/// Returns a range, budget, handle, allocation, worker, or native-I/O setup
	/// error. Once the worker submits I/O, errors are never retried through a
	/// different transport.
	#[cfg(windows)]
	pub fn scatter_cursor(
		&self,
		file: Arc<DirectFile>,
		first_page: PageNo,
		page_count: usize,
	) -> FerrosGrynnResult<ScatterPrefetchCursor<'_>> {
		if self.transient.scatter_extent_pages < 2 {
			return Err(FerrosGrynnError::io_mode_unavailable(
				"ReadFileScatter",
				"transient budget cannot hold two useful scatter buffers",
			));
		}
		ScatterPrefetchCursor::new(
			file,
			first_page,
			page_count,
			self.transient.scatter_extent_pages,
			self.transient.scatter_lane.lock(),
		)
	}

	/// Reads one known sparse cold window with one IoRing SQE per frame.
	///
	/// The complete batch is validated before any page is returned and never
	/// enters resident replacement.
	///
	/// # Errors
	///
	/// Returns an error for an empty or oversized window, unavailable IoRing,
	/// invalid file handle, native completion failure, or any invalid page.
	#[cfg(all(windows, feature = "native-async-io"))]
	pub fn read_sparse_ioring(
		&self,
		reads: &[(Arc<DirectFile>, PageNo)],
	) -> FerrosGrynnResult<SparseReadBatch> {
		if reads.is_empty() || reads.len() > self.transient.sparse_pages {
			return Err(FerrosGrynnError::storage_io_failure(format!(
				"sparse IoRing window must contain 1..{} cold pages",
				self.transient.sparse_pages
			)));
		}
		let mut pages = (0..reads.len()).map(|_| AlignedPage::zeroed()).collect::<Vec<_>>();
		let mut ring = self.transient.io_ring.lock();
		if ring.is_none() {
			*ring = Some(WindowsIoRing::new()?);
		}
		let Some(ring) = ring.as_mut() else {
			return Err(FerrosGrynnError::io_mode_unavailable(
				"NativeAsync",
				"IoRing initialization did not produce a queue",
			));
		};
		let mut frame_reads = reads
			.iter()
			.zip(pages.iter_mut())
			.map(|((file, page_no), frame)| FrameRead::new(file, *page_no, frame))
			.collect::<Vec<_>>();
		ring.read_frames(&mut frame_reads)?;
		drop(frame_reads);
		for ((_, page_no), page) in reads.iter().zip(&pages) {
			page.validate(*page_no)?;
		}
		Ok(SparseReadBatch {
			page_numbers: reads.iter().map(|(_, page_no)| *page_no).collect(),
			pages,
		})
	}

	/// Returns a validated pinned page, loading an exact-version miss once.
	pub fn get_or_load<Loader>(
		&self,
		key: PageCacheKey,
		loader: Loader,
	) -> FerrosGrynnResult<AtomicPageGuard<'_>>
	where
		Loader: FnOnce(&mut AlignedPage) -> FerrosGrynnResult<PageHeader>,
	{
		self.arena.get_or_load(key, loader)
	}

	/// Probes the exact-version resident directory without starting I/O.
	pub fn get_if_resident(
		&self,
		key: PageCacheKey,
	) -> FerrosGrynnResult<Option<AtomicPageGuard<'_>>> {
		self.arena.get_if_resident(key)
	}

	/// Resolves one exact resident version for repeated scoped access.
	pub fn resolve(&self, key: PageCacheKey) -> Option<ResolvedFrameHandle> {
		self.arena.resolve(key)
	}

	/// Returns the lazy locator for one exact home-file epoch.
	pub fn home_locator(
		&self,
		first_key: PageCacheKey,
		page_count: NonZeroUsize,
	) -> FerrosGrynnResult<Arc<SourceBoundLocator>> {
		self.sources.home_locator(&self.arena, first_key, page_count)
	}

	/// Removes registry ownership of older home epochs for the same source.
	///
	/// Existing scopes remain valid because they own the retired locator.
	pub fn retire_home_versions(&self, current_key: PageCacheKey) -> FerrosGrynnResult<usize> {
		let current = SourceState::from_key(current_key);
		if !current.is_home() {
			return Err(FerrosGrynnError::storage_io_failure(
				"only a home-file epoch can retire older home locators",
			));
		}
		Ok(self.sources.retire_home_versions(current))
	}

	/// Returns exact home epochs retained by the source registry.
	pub fn source_count(&self) -> usize {
		self.sources.len()
	}

	/// Returns globally bounded live home-locator entries.
	pub fn locator_entry_count(&self) -> usize {
		self.arena.locator_entry_count()
	}

	/// Opens one bounded cursor read epoch.
	pub fn read_scope(&self) -> FerrosGrynnResult<ReadScope<'_>> {
		ReadScope::new(&self.arena)
	}

	/// Returns the number of currently resident exact page versions.
	pub fn resident_count(&self) -> usize {
		self.arena.resident_count()
	}

	/// Returns miss, coalescing, eviction, and capacity-wait counts.
	pub fn slow_path_counts(&self) -> (u64, u64, u64, u64) {
		self.arena.slow_path_counts()
	}
}

/// Cursor-owned read epoch for at most 256 page operations from one source.
pub struct ReadScope<'engine> {
	/// Arena used to rotate epochs without changing engines.
	arena: &'engine AtomicArenaBufferPool,
	/// Current reclamation guard.
	epoch: Option<arena::AtomicReadEpoch<'engine>>,
	/// Exact source protected by this epoch.
	source: Option<SourceState>,
	/// Logical page operations completed in this epoch.
	operations: usize,
}

/// Reusable compact translation from logical page order to arena frames.
pub struct ResolvedPageTable {
	/// Arena that produced every handle in this table.
	arena_id: u64,
	/// Exact source shared by all table entries.
	source: Option<SourceState>,
	/// Exact locators retained for validation after an eviction.
	handles: Vec<ResolvedFrameHandle>,
	/// Direct page addresses populated by the latest complete validation.
	pages: Vec<NonNull<AlignedPage>>,
	/// Arena invalidation token observed during the last stable validation.
	validated_epoch: Option<u64>,
}

/// Borrowed view over a reusable exact-version page table.
pub struct ResolvedPageWindow<'scope, 'engine> {
	/// Live epoch preventing the validated frames from being reused.
	_epoch: &'scope arena::AtomicReadEpoch<'engine>,
	/// Page order prepared by the physical cursor.
	pages: &'scope [NonNull<AlignedPage>],
}

impl ResolvedPageWindow<'_, '_> {
	/// Returns the number of pages available in caller order.
	pub const fn len(&self) -> usize {
		self.pages.len()
	}

	/// Returns whether this window contains no pages.
	pub const fn is_empty(&self) -> bool {
		self.pages.is_empty()
	}

	/// Returns one prevalidated page in caller order.
	pub fn page(&self, index: usize) -> Option<&AlignedPage> {
		let page = self.pages.get(index)?;
		// SAFETY: `get_resolved_window` either validates every exact handle
		// inside this epoch or proves no arena invalidation occurred since the
		// table's prior stable validation. This window keeps the epoch alive.
		Some(unsafe { page.as_ref() })
	}
}

impl ResolvedPageTable {
	/// Replaces the cursor-owned order while retaining both allocations.
	fn replace(
		&mut self,
		arena: &AtomicArenaBufferPool,
		handles: &[ResolvedFrameHandle],
	) -> FerrosGrynnResult<()> {
		let source = validate_resolved_handles(arena, handles)?;
		self.arena_id = arena.arena_id();
		self.source = source;
		self.handles.clear();
		self.handles.extend_from_slice(handles);
		self.pages.clear();
		self.validated_epoch = None;
		Ok(())
	}

	/// Takes a prepared window while returning the table's previous allocation.
	fn replace_owned(
		&mut self,
		arena: &AtomicArenaBufferPool,
		handles: &mut Vec<ResolvedFrameHandle>,
	) -> FerrosGrynnResult<()> {
		let source = validate_resolved_handles(arena, handles)?;
		self.arena_id = arena.arena_id();
		self.source = source;
		std::mem::swap(&mut self.handles, handles);
		handles.clear();
		self.pages.clear();
		self.validated_epoch = None;
		Ok(())
	}
}

impl<'engine> ReadScope<'engine> {
	/// Enters the first bounded epoch.
	fn new(arena: &'engine AtomicArenaBufferPool) -> FerrosGrynnResult<Self> {
		Ok(Self {
			arena,
			epoch: Some(arena.read_epoch()?),
			source: None,
			operations: 0,
		})
	}

	/// Resolves at most one bounded window through the shared directory.
	///
	/// # Errors
	///
	/// Rejects windows larger than the read-epoch operation limit.
	pub fn resolve_window(
		&self,
		keys: &[PageCacheKey],
	) -> FerrosGrynnResult<Vec<Option<ResolvedFrameHandle>>> {
		validate_window(keys)?;
		Ok(keys.iter().map(|key| self.arena.resolve(*key)).collect())
	}

	/// Resolves a home window through its bounded source-local locator.
	///
	/// # Errors
	///
	/// Rejects an oversized window or a key from another source epoch/range.
	pub fn resolve_home_window(
		&self,
		locator: &SourceBoundLocator,
		keys: &[PageCacheKey],
	) -> FerrosGrynnResult<Vec<Option<ResolvedFrameHandle>>> {
		validate_window(keys)?;
		if keys.iter().any(|key| !locator.accepts(self.arena, *key)) {
			return Err(FerrosGrynnError::storage_io_failure(
				"home read window crosses its registered source epoch or page range",
			));
		}
		Ok(keys.iter().map(|key| locator.resolve(self.arena, *key)).collect())
	}

	/// Borrows one resolved page, rotating before the scope crosses its source
	/// or operation boundary.
	///
	/// # Errors
	///
	/// Returns an internal storage error if a new read epoch cannot be entered.
	pub fn get_resolved<'scope>(
		&'scope mut self,
		handle: &ResolvedFrameHandle,
	) -> FerrosGrynnResult<Option<AtomicEpochPage<'scope>>> {
		let source = SourceState::from_key(handle.key());
		if self.operations == MAX_READ_SCOPE_OPERATIONS
			|| self.source.is_some_and(|old| old != source)
		{
			self.rotate()?;
		}
		self.source = Some(source);
		self.operations += 1;
		Ok(self.current_epoch()?.get_resolved(handle))
	}

	/// Builds one reusable compact page table for a physical cursor.
	///
	/// # Errors
	///
	/// Rejects more than 256 handles or handles from different arenas or exact
	/// source epochs.
	pub fn prepare_resolved_table(
		&self,
		handles: &[ResolvedFrameHandle],
	) -> FerrosGrynnResult<ResolvedPageTable> {
		let mut table = ResolvedPageTable {
			arena_id: self.arena.arena_id(),
			source: None,
			handles: Vec::with_capacity(handles.len()),
			pages: Vec::with_capacity(handles.len()),
			validated_epoch: None,
		};
		table.replace(self.arena, handles)?;
		Ok(table)
	}

	/// Replaces a reusable table's bounded logical order without reallocating.
	///
	/// # Errors
	///
	/// Returns the same validation errors as [`Self::prepare_resolved_table`].
	pub fn update_resolved_table(
		&self,
		table: &mut ResolvedPageTable,
		handles: &[ResolvedFrameHandle],
	) -> FerrosGrynnResult<()> {
		table.replace(self.arena, handles)
	}

	/// Moves one prepared window into a reusable table without copying handles.
	///
	/// The input is empty on success and retains the table's previous
	/// allocation for the caller's next bounded window.
	///
	/// # Errors
	///
	/// Returns the same validation errors as [`Self::prepare_resolved_table`]
	/// without changing either buffer.
	pub fn update_resolved_table_owned(
		&self,
		table: &mut ResolvedPageTable,
		handles: &mut Vec<ResolvedFrameHandle>,
	) -> FerrosGrynnResult<()> {
		table.replace_owned(self.arena, handles)
	}

	/// Borrows a reusable exact-source table for one bounded operation window.
	///
	/// A stable arena token avoids repeated per-frame generation loads when no
	/// resident page was invalidated. Any eviction forces a complete exact-key
	/// revalidation before the compact indices can be used again.
	///
	/// # Errors
	///
	/// Returns an error for invalid operation counts, a foreign table, or
	/// failure to rotate the read epoch.
	pub fn get_resolved_window<'scope>(
		&'scope mut self,
		table: &'scope mut ResolvedPageTable,
		operation_count: usize,
	) -> FerrosGrynnResult<Option<ResolvedPageWindow<'scope, 'engine>>> {
		if table.arena_id != self.arena.arena_id() {
			return Err(FerrosGrynnError::storage_io_failure(
				"resolved page table belongs to another arena",
			));
		}
		let Some(source) = table.source else {
			if operation_count != 0 {
				return Err(FerrosGrynnError::storage_io_failure(
					"resolved read window has operations but no page handles",
				));
			}
			let epoch = self.current_epoch()?;
			return Ok(Some(ResolvedPageWindow {
				_epoch: epoch,
				pages: &table.pages,
			}));
		};
		if operation_count == 0 || operation_count > MAX_READ_SCOPE_OPERATIONS {
			return Err(FerrosGrynnError::storage_io_failure(
				"resolved read window operation count is outside the bounded epoch",
			));
		}
		if self.operations.saturating_add(operation_count) > MAX_READ_SCOPE_OPERATIONS
			|| self.source.is_some_and(|old| old != source)
		{
			self.rotate()?;
		}
		self.source = Some(source);
		self.operations += operation_count;

		let before = self.arena.resolved_table_epoch();
		if table.validated_epoch != Some(before) {
			let epoch = self.current_epoch()?;
			table.pages.clear();
			for handle in &table.handles {
				let Some(page) = epoch.get_resolved(handle) else {
					table.pages.clear();
					table.validated_epoch = None;
					return Ok(None);
				};
				table.pages.push(NonNull::from(page.as_page()));
			}
			let after = self.arena.resolved_table_epoch();
			table.validated_epoch = (before == after).then_some(after);
		}
		let epoch = self.current_epoch()?;
		Ok(Some(ResolvedPageWindow {
			_epoch: epoch,
			pages: &table.pages,
		}))
	}

	/// Returns operations completed since the last rotation.
	pub const fn operations(&self) -> usize {
		self.operations
	}

	/// Replaces the reclamation guard and resets bounded-window state.
	fn rotate(&mut self) -> FerrosGrynnResult<()> {
		drop(self.epoch.take());
		self.source = None;
		self.operations = 0;
		self.epoch = Some(self.arena.read_epoch()?);
		Ok(())
	}

	/// Returns the current read epoch after successful scope construction.
	fn current_epoch(&self) -> FerrosGrynnResult<&arena::AtomicReadEpoch<'engine>> {
		self.epoch.as_ref().ok_or_else(|| {
			FerrosGrynnError::storage_io_failure("hybrid read scope has no active epoch")
		})
	}
}

/// Checks the universal read-window operation bound.
fn validate_window<T>(items: &[T]) -> FerrosGrynnResult<()> {
	if items.len() > MAX_READ_SCOPE_OPERATIONS {
		Err(FerrosGrynnError::storage_io_failure(
			"hybrid read window exceeds the bounded epoch operation limit",
		))
	} else {
		Ok(())
	}
}

/// Validates the arena, source epoch, and universal table bound.
fn validate_resolved_handles(
	arena: &AtomicArenaBufferPool,
	handles: &[ResolvedFrameHandle],
) -> FerrosGrynnResult<Option<SourceState>> {
	validate_window(handles)?;
	let source = handles.first().map(|handle| SourceState::from_key(handle.key()));
	if handles.iter().any(|handle| {
		handle.arena_id() != arena.arena_id()
			|| source.is_some_and(|source| SourceState::from_key(handle.key()) != source)
	}) {
		return Err(FerrosGrynnError::storage_io_failure(
			"resolved page table crosses its arena or exact source epoch",
		));
	}
	Ok(source)
}
