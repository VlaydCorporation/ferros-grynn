//! Storage-only read intent and admission-policy prototypes.

use std::{
	cell::Cell,
	num::NonZeroUsize,
	sync::atomic::{AtomicU64, Ordering},
};

#[cfg(windows)]
use ferros_grynn::storage_engine_prototype_api::page::PAGE_SIZE;
use ferros_grynn::{
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	storage_engine_prototype_api::{
		buffer_pool::{PageCacheKey, validate_loaded_header},
		page::{AlignedPage, PageHeader},
	},
};
use parking_lot::Mutex;
use rapidhash::{HashMapExt, RapidHashMap};

use super::atomic_arena::AtomicArenaBufferPool;
#[cfg(windows)]
use super::windows_vm::{HomeSourceSpec, VmCache};

/// Unique epoch source for independently created storage cursors.
static NEXT_CURSOR_EPOCH: AtomicU64 = AtomicU64::new(1);

std::thread_local! {
	/// Whether this thread is currently executing a prototype cache read.
	static READ_WITH_ACTIVE: Cell<bool> = const { Cell::new(false) };
}

/// Clears the current thread's active-read marker when a read leaves its scope.
struct ReadWithScope;

impl ReadWithScope {
	/// Marks one top-level prototype cache read active on the current thread.
	///
	/// # Errors
	///
	/// Returns a project storage error when the current thread is already
	/// inside any [`PrototypeReadCache::read_with`] call.
	fn enter() -> FerrosGrynnResult<Self> {
		READ_WITH_ACTIVE.with(|active| {
			if active.get() {
				return Err(FerrosGrynnError::storage_io_failure(
					"nested PrototypeReadCache::read_with on one thread",
				));
			}
			active.set(true);
			Ok(Self)
		})
	}
}

impl Drop for ReadWithScope {
	fn drop(&mut self) {
		READ_WITH_ACTIVE.with(|active| active.set(false));
	}
}

/// Physical read intent inferred by a future storage/query cursor boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ReadIntent {
	/// One unrelated page lookup.
	Point,
	/// Repeated access to index roots/internal pages or graph frontiers.
	IndexTraversal,
	/// Ordered page range owned by one scan.
	SequentialScan {
		/// Logical scan identity used for diagnostics.
		scan_id: u64,
		/// Expected number of pages in the range.
		expected_pages: u64,
		/// Whether the cursor predicts no page reuse.
		one_shot: bool,
	},
	/// Bulk construction that should not displace query hot pages.
	BulkBuild,
}

/// Test-only page admission policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ReadPolicy {
	/// Admit every validated miss to the main resident arena.
	CacheAll,
	/// Use a reusable scratch page for non-resident scan misses.
	RocksLikeBypass,
	/// Admit scan misses to a bounded cold arena before promotion.
	InnodbLikeCold,
}

/// Synchronous pull cursor carrying physical read intent and epoch.
pub(super) struct PrototypeScanCursor {
	/// Intent inferred from the storage operation.
	intent: ReadIntent,
	/// Unique creation epoch used by cold-page promotion.
	epoch: u64,
	/// Optional forced policy used to compare mechanics with identical input.
	forced_policy: Option<ReadPolicy>,
	/// Reusable miss buffer owned by this synchronous pull cursor.
	scratch: Mutex<AlignedPage>,
}

impl PrototypeScanCursor {
	/// Creates a cursor using the policy inferred from its intent.
	pub(super) fn new(intent: ReadIntent) -> Self {
		Self::with_policy(intent, None)
	}

	/// Creates a cursor with an optional benchmark policy override.
	pub(super) fn with_policy(intent: ReadIntent, forced_policy: Option<ReadPolicy>) -> Self {
		Self {
			intent,
			epoch: NEXT_CURSOR_EPOCH.fetch_add(1, Ordering::Relaxed),
			forced_policy,
			scratch: Mutex::new(AlignedPage::zeroed()),
		}
	}

	/// Returns the selected policy.
	pub(super) const fn policy(&self) -> ReadPolicy {
		match self.forced_policy {
			Some(policy) => policy,
			None => match self.intent {
				ReadIntent::Point | ReadIntent::IndexTraversal => ReadPolicy::CacheAll,
				ReadIntent::SequentialScan {
					one_shot: true,
					..
				}
				| ReadIntent::BulkBuild => ReadPolicy::RocksLikeBypass,
				ReadIntent::SequentialScan {
					one_shot: false,
					..
				} => ReadPolicy::InnodbLikeCold,
			},
		}
	}

	/// Returns whether a cold hit must be promoted to the main arena.
	fn promotes(&self, admitted_epoch: u64) -> bool {
		matches!(self.intent, ReadIntent::Point | ReadIntent::IndexTraversal)
			|| self.epoch != admitted_epoch
	}
}

/// Admission metadata for one page currently associated with the cold arena.
#[derive(Debug, Clone, Copy)]
struct ColdEntry {
	/// Cursor epoch that first admitted the page.
	admitted_epoch: u64,
}

/// Fixed-budget policy wrapper over one or two atomic arenas.
pub(super) struct PrototypeReadCache {
	/// Main arena for reusable point/root/traversal pages.
	hot: AtomicArenaBufferPool,
	/// Optional `3/8` cold arena used only by `InnodbLikeCold`.
	cold: Option<AtomicArenaBufferPool>,
	/// Serializes only cross-tier miss admission and promotion. Resident hot
	/// reads remain lock-free at this wrapper level.
	admission: Mutex<()>,
	/// Cursor epoch associated with tracked cold pages.
	cold_entries: Mutex<RapidHashMap<PageCacheKey, ColdEntry>>,
	/// Maximum number of frames in the cold arena.
	cold_capacity: usize,
}

impl PrototypeReadCache {
	/// Creates a total frame budget. Budgets below three pages cannot form a
	/// useful cold partition and therefore use bypass for cold intent.
	pub(super) fn new(pool_budget: NonZeroUsize) -> Self {
		Self::for_policy(pool_budget, ReadPolicy::InnodbLikeCold)
	}

	/// Creates the exact frame split needed by one policy so every standalone
	/// comparison uses the same total memory budget.
	pub(super) fn for_policy(pool_budget: NonZeroUsize, policy: ReadPolicy) -> Self {
		let pool_budget = pool_budget.get();
		let cold_capacity = if policy == ReadPolicy::InnodbLikeCold && pool_budget >= 3 {
			(pool_budget * 3 / 8).max(1)
		} else {
			0
		};
		let hot_capacity = pool_budget - cold_capacity;
		let hot = AtomicArenaBufferPool::new(
			NonZeroUsize::new(hot_capacity).unwrap_or(NonZeroUsize::MIN),
		);
		let cold = NonZeroUsize::new(cold_capacity).map(AtomicArenaBufferPool::new);
		Self {
			hot,
			cold,
			admission: Mutex::new(()),
			cold_entries: Mutex::new(RapidHashMap::new()),
			cold_capacity,
		}
	}

	/// Reads one page synchronously and invokes a monomorphized consumer while
	/// the selected resident guard or bypass scratch borrow remains valid.
	/// Same-thread nesting is rejected, while unrelated threads use independent
	/// markers and are not coordinated by this reentrancy guard.
	///
	/// # Errors
	///
	/// Returns a project storage error before acquiring scratch, admission, or
	/// frame state when the current thread is already executing any
	/// `PrototypeReadCache::read_with` call. Otherwise returns loader,
	/// validation, cache lifecycle, or consumer errors.
	///
	/// # Panics
	///
	/// Propagates loader and consumer panics. The per-thread reentrancy marker
	/// is cleared while unwinding, so a later top-level read on the thread
	/// remains valid.
	pub(super) fn read_with<Loader, Consumer, Output>(
		&self,
		cursor: &PrototypeScanCursor,
		key: PageCacheKey,
		loader: Loader,
		consumer: Consumer,
	) -> FerrosGrynnResult<Output>
	where
		Loader: FnOnce(&mut AlignedPage) -> FerrosGrynnResult<PageHeader>,
		Consumer: FnOnce(&AlignedPage) -> FerrosGrynnResult<Output>,
	{
		let _read_scope = ReadWithScope::enter()?;
		match cursor.policy() {
			ReadPolicy::CacheAll => self.read_cache_all(key, loader, consumer),
			ReadPolicy::RocksLikeBypass => self.read_bypass(cursor, key, loader, consumer),
			ReadPolicy::InnodbLikeCold => self.read_cold(cursor, key, loader, consumer),
		}
	}

	/// Returns the number of main-arena resident frames.
	pub(super) fn hot_resident_count(&self) -> usize {
		self.hot.resident_count()
	}

	/// Returns the number of cold-arena resident frames.
	pub(super) fn cold_resident_count(&self) -> usize {
		self.cold.as_ref().map_or(0, AtomicArenaBufferPool::resident_count)
	}

	/// Returns the configured bounded cold-frame count.
	pub(super) const fn cold_capacity(&self) -> usize {
		self.cold_capacity
	}

	/// Handles a reusable read, promoting an existing cold page before I/O.
	fn read_cache_all<Loader, Consumer, Output>(
		&self,
		key: PageCacheKey,
		loader: Loader,
		consumer: Consumer,
	) -> FerrosGrynnResult<Output>
	where
		Loader: FnOnce(&mut AlignedPage) -> FerrosGrynnResult<PageHeader>,
		Consumer: FnOnce(&AlignedPage) -> FerrosGrynnResult<Output>,
	{
		if let Some(guard) = self.hot.get_if_resident(key)? {
			return consumer(guard.as_page());
		}
		let admission = self.admission.lock();
		if let Some(guard) = self.hot.get_if_resident(key)? {
			drop(admission);
			return consumer(guard.as_page());
		}
		if let Some(cold) = &self.cold {
			if let Some(cold_guard) = cold.get_if_resident(key)? {
				let promoted = self.hot.get_or_copy(key, cold_guard.as_page())?;
				drop(cold_guard);
				drop(admission);
				cold.remove_resident(key)?;
				self.cold_entries.lock().remove(&key);
				return consumer(promoted.as_page());
			}
		}
		let guard = self.hot.get_or_load(key, loader)?;
		self.cold_entries.lock().remove(&key);
		drop(admission);
		consumer(guard.as_page())
	}

	/// Reads a resident hot page or performs a validated non-admitting miss.
	fn read_bypass<Loader, Consumer, Output>(
		&self,
		cursor: &PrototypeScanCursor,
		key: PageCacheKey,
		loader: Loader,
		consumer: Consumer,
	) -> FerrosGrynnResult<Output>
	where
		Loader: FnOnce(&mut AlignedPage) -> FerrosGrynnResult<PageHeader>,
		Consumer: FnOnce(&AlignedPage) -> FerrosGrynnResult<Output>,
	{
		if let Some(guard) = self.hot.get_if_resident(key)? {
			return consumer(guard.as_page());
		}
		if let Some(cold) = &self.cold {
			if let Some(guard) = cold.get_if_resident(key)? {
				return consumer(guard.as_page());
			}
		}
		let mut scratch = cursor.scratch.lock();
		let reported = loader(&mut scratch)?;
		let actual = scratch.validate(key.page_no())?;
		validate_loaded_header(key, actual)?;
		if actual != reported {
			return Err(FerrosGrynnError::invalid_storage_page(
				key.page_no().get(),
				"bypass loader header does not match its current page bytes",
			));
		}
		consumer(&scratch)
	}

	/// Reads through the bounded cold arena and promotes only on a qualifying
	/// cursor epoch or point/traversal intent.
	fn read_cold<Loader, Consumer, Output>(
		&self,
		cursor: &PrototypeScanCursor,
		key: PageCacheKey,
		loader: Loader,
		consumer: Consumer,
	) -> FerrosGrynnResult<Output>
	where
		Loader: FnOnce(&mut AlignedPage) -> FerrosGrynnResult<PageHeader>,
		Consumer: FnOnce(&AlignedPage) -> FerrosGrynnResult<Output>,
	{
		if let Some(guard) = self.hot.get_if_resident(key)? {
			return consumer(guard.as_page());
		}
		let Some(cold) = &self.cold else {
			return self.read_bypass(cursor, key, loader, consumer);
		};

		if let Some(cold_guard) = cold.get_if_resident(key)? {
			let admitted_epoch =
				self.cold_entries.lock().get(&key).map(|entry| entry.admitted_epoch);
			if admitted_epoch.is_some_and(|epoch| !cursor.promotes(epoch)) {
				return consumer(cold_guard.as_page());
			}
			// A qualifying promotion must re-enter through the cross-tier
			// admission lock. Drop this speculative pin before waiting so
			// removal cannot form a lock/pin cycle with another consumer.
			drop(cold_guard);
		}

		let admission = self.admission.lock();
		if let Some(guard) = self.hot.get_if_resident(key)? {
			drop(admission);
			return consumer(guard.as_page());
		}
		if let Some(cold_guard) = cold.get_if_resident(key)? {
			let admitted_epoch =
				self.cold_entries.lock().get(&key).map(|entry| entry.admitted_epoch);
			if admitted_epoch.is_none_or(|epoch| cursor.promotes(epoch)) {
				let promoted = self.hot.get_or_copy(key, cold_guard.as_page())?;
				drop(cold_guard);
				drop(admission);
				cold.remove_resident(key)?;
				self.cold_entries.lock().remove(&key);
				return consumer(promoted.as_page());
			}
			drop(admission);
			return consumer(cold_guard.as_page());
		}

		let cold_guard = cold.get_or_load_before_publish(key, loader, || {
			let mut entries = self.cold_entries.lock();
			entries.retain(|tracked_key, _| cold.is_mapped(*tracked_key));
			entries.insert(key, ColdEntry {
				admitted_epoch: cursor.epoch,
			});
			if entries.len() > self.cold_capacity {
				return Err(FerrosGrynnError::storage_io_failure(
					"cold admission metadata exceeded its frame budget",
				));
			}
			Ok(())
		})?;
		drop(admission);
		consumer(cold_guard.as_page())
	}
}

/// Fixed-budget policy wrapper over one or two Windows VM caches.
#[cfg(windows)]
pub(super) struct VmPrototypeReadCache {
	/// Main VM cache for reusable point/root/traversal pages.
	hot: VmCache,
	/// Optional `3/8` VM cache used only by `InnodbLikeCold`.
	cold: Option<VmCache>,
	/// Serializes only cross-tier miss admission and promotion.
	admission: Mutex<()>,
	/// Cursor epoch associated with tracked cold pages.
	cold_entries: Mutex<RapidHashMap<PageCacheKey, ColdEntry>>,
	/// Maximum number of committed pages in the cold VM cache.
	cold_capacity: usize,
}

#[cfg(windows)]
impl VmPrototypeReadCache {
	/// Creates a total frame budget with an optional `3/8` cold partition.
	///
	/// # Errors
	///
	/// Returns VM source-registration or address-reservation errors.
	pub(super) fn new(
		home_sources: &[HomeSourceSpec],
		pool_budget: NonZeroUsize,
	) -> FerrosGrynnResult<Self> {
		Self::for_policy(home_sources, pool_budget, ReadPolicy::InnodbLikeCold)
	}

	/// Creates the exact VM budget split required by one admission policy.
	///
	/// # Errors
	///
	/// Returns VM source-registration or address-reservation errors.
	pub(super) fn for_policy(
		home_sources: &[HomeSourceSpec],
		pool_budget: NonZeroUsize,
		policy: ReadPolicy,
	) -> FerrosGrynnResult<Self> {
		let pool_budget = pool_budget.get();
		let cold_capacity = if policy == ReadPolicy::InnodbLikeCold && pool_budget >= 3 {
			(pool_budget * 3 / 8).max(1)
		} else {
			0
		};
		let hot_capacity =
			NonZeroUsize::new(pool_budget - cold_capacity).unwrap_or(NonZeroUsize::MIN);
		let hot = VmCache::new(home_sources, hot_capacity, 0)?;
		let cold = match NonZeroUsize::new(cold_capacity) {
			Some(capacity) => Some(VmCache::new(home_sources, capacity, 0)?),
			None => None,
		};
		Ok(Self {
			hot,
			cold,
			admission: Mutex::new(()),
			cold_entries: Mutex::new(RapidHashMap::new()),
			cold_capacity,
		})
	}

	/// Reads one exact home page under the cursor-selected policy.
	///
	/// # Errors
	///
	/// Returns reentrancy, source-registration, loader, validation, VM
	/// lifecycle, or consumer errors.
	///
	/// # Panics
	///
	/// Propagates loader and consumer panics. The thread-local reentrancy
	/// marker is cleared while unwinding.
	pub(super) fn read_with<Loader, Consumer, Output>(
		&self,
		cursor: &PrototypeScanCursor,
		key: PageCacheKey,
		loader: Loader,
		consumer: Consumer,
	) -> FerrosGrynnResult<Output>
	where
		Loader: FnOnce(&mut AlignedPage) -> FerrosGrynnResult<PageHeader>,
		Consumer: FnOnce(&AlignedPage) -> FerrosGrynnResult<Output>,
	{
		let _read_scope = ReadWithScope::enter()?;
		match cursor.policy() {
			ReadPolicy::CacheAll => self.read_cache_all(key, loader, consumer),
			ReadPolicy::RocksLikeBypass => self.read_bypass(cursor, key, loader, consumer),
			ReadPolicy::InnodbLikeCold => self.read_cold(cursor, key, loader, consumer),
		}
	}

	/// Returns the number of committed pages in the main VM cache.
	pub(super) fn hot_resident_count(&self) -> usize {
		self.hot.committed_bytes() / PAGE_SIZE
	}

	/// Returns the number of committed pages in the cold VM cache.
	pub(super) fn cold_resident_count(&self) -> usize {
		self.cold.as_ref().map_or(0, |cold| cold.committed_bytes() / PAGE_SIZE)
	}

	/// Returns the configured bounded cold-frame count.
	pub(super) const fn cold_capacity(&self) -> usize {
		self.cold_capacity
	}

	/// Returns total committed bytes across both VM cache tiers.
	pub(super) fn committed_bytes(&self) -> usize {
		self.hot.committed_bytes() + self.cold.as_ref().map_or(0, VmCache::committed_bytes)
	}

	/// Handles a reusable read, promoting an existing cold page before I/O.
	fn read_cache_all<Loader, Consumer, Output>(
		&self,
		key: PageCacheKey,
		loader: Loader,
		consumer: Consumer,
	) -> FerrosGrynnResult<Output>
	where
		Loader: FnOnce(&mut AlignedPage) -> FerrosGrynnResult<PageHeader>,
		Consumer: FnOnce(&AlignedPage) -> FerrosGrynnResult<Output>,
	{
		if let Some(guard) = self.hot.get_if_home_resident(key)? {
			return consumer(guard.as_page());
		}
		let admission = self.admission.lock();
		if let Some(guard) = self.hot.get_if_home_resident(key)? {
			drop(admission);
			return consumer(guard.as_page());
		}
		if let Some(cold) = &self.cold {
			if let Some(cold_guard) = cold.get_if_home_resident(key)? {
				let promoted = self.hot.get_or_load(key, |target| {
					target.as_bytes_mut().copy_from_slice(cold_guard.as_page().as_bytes());
					target.validate(key.page_no())
				})?;
				drop(cold_guard);
				cold.remove_home_resident(key)?;
				self.cold_entries.lock().remove(&key);
				drop(admission);
				return consumer(promoted.as_page());
			}
		}
		let guard = self.hot.get_or_load(key, loader)?;
		self.cold_entries.lock().remove(&key);
		drop(admission);
		consumer(guard.as_page())
	}

	/// Reads a resident page or performs a validated non-admitting miss.
	fn read_bypass<Loader, Consumer, Output>(
		&self,
		cursor: &PrototypeScanCursor,
		key: PageCacheKey,
		loader: Loader,
		consumer: Consumer,
	) -> FerrosGrynnResult<Output>
	where
		Loader: FnOnce(&mut AlignedPage) -> FerrosGrynnResult<PageHeader>,
		Consumer: FnOnce(&AlignedPage) -> FerrosGrynnResult<Output>,
	{
		if let Some(guard) = self.hot.get_if_home_resident(key)? {
			return consumer(guard.as_page());
		}
		if let Some(cold) = &self.cold {
			if let Some(guard) = cold.get_if_home_resident(key)? {
				return consumer(guard.as_page());
			}
		}
		let mut scratch = cursor.scratch.lock();
		let reported = loader(&mut scratch)?;
		let actual = scratch.validate(key.page_no())?;
		validate_loaded_header(key, actual)?;
		if actual != reported {
			return Err(FerrosGrynnError::invalid_storage_page(
				key.page_no().get(),
				"VM bypass loader header does not match its current page bytes",
			));
		}
		consumer(&scratch)
	}

	/// Reads through the bounded cold VM cache and promotes qualifying hits.
	fn read_cold<Loader, Consumer, Output>(
		&self,
		cursor: &PrototypeScanCursor,
		key: PageCacheKey,
		loader: Loader,
		consumer: Consumer,
	) -> FerrosGrynnResult<Output>
	where
		Loader: FnOnce(&mut AlignedPage) -> FerrosGrynnResult<PageHeader>,
		Consumer: FnOnce(&AlignedPage) -> FerrosGrynnResult<Output>,
	{
		if let Some(guard) = self.hot.get_if_home_resident(key)? {
			return consumer(guard.as_page());
		}
		let Some(cold) = &self.cold else {
			return self.read_bypass(cursor, key, loader, consumer);
		};
		let admission = self.admission.lock();
		if let Some(guard) = self.hot.get_if_home_resident(key)? {
			drop(admission);
			return consumer(guard.as_page());
		}
		if let Some(cold_guard) = cold.get_if_home_resident(key)? {
			let admitted_epoch =
				self.cold_entries.lock().get(&key).map(|entry| entry.admitted_epoch);
			if admitted_epoch.is_some_and(|epoch| !cursor.promotes(epoch)) {
				drop(admission);
				return consumer(cold_guard.as_page());
			}
			let promoted = self.hot.get_or_load(key, |target| {
				target.as_bytes_mut().copy_from_slice(cold_guard.as_page().as_bytes());
				target.validate(key.page_no())
			})?;
			drop(cold_guard);
			cold.remove_home_resident(key)?;
			self.cold_entries.lock().remove(&key);
			drop(admission);
			return consumer(promoted.as_page());
		}

		let cold_guard = cold.get_or_load(key, loader)?;
		let metadata_result = (|| {
			let mut entries = self.cold_entries.lock();
			#[allow(
				clippy::needless_collect,
				reason = "a key snapshot permits fallible VM lookup before mutating the same map"
			)]
			let tracked_keys = entries.keys().copied().collect::<Vec<_>>();
			for tracked_key in tracked_keys {
				if !cold.is_home_mapped(tracked_key)? {
					entries.remove(&tracked_key);
				}
			}
			entries.insert(key, ColdEntry {
				admitted_epoch: cursor.epoch,
			});
			if entries.len() > self.cold_capacity {
				return Err(FerrosGrynnError::storage_io_failure(
					"VM cold admission metadata exceeded its frame budget",
				));
			}
			Ok(())
		})();
		if let Err(error) = metadata_result {
			drop(cold_guard);
			cold.remove_home_resident(key)?;
			return Err(error);
		}
		drop(admission);
		consumer(cold_guard.as_page())
	}
}

#[cfg(test)]
#[allow(
	clippy::panic,
	clippy::panic_in_result_fn,
	reason = "fallible tests report contract failures and one test intentionally unwinds"
)]
mod tests {
	use std::{
		error::Error,
		num::NonZeroUsize,
		sync::{
			atomic::{AtomicUsize, Ordering},
			mpsc,
		},
		thread,
		time::Duration,
	};

	use ferros_grynn::{
		gql_status::{FerrosGrynnError, FerrosGrynnResult},
		storage_engine_prototype_api::{
			buffer_pool::{PageCacheKey, PageCacheVersion},
			page::{AlignedPage, PageFlags, PageHeader, PageLsn, PageNo, PageType},
			types::{CheckpointGeneration, DataFileId, DataFileKind, GraphId, Lsn, PageKey},
		},
	};

	#[cfg(windows)]
	use super::VmPrototypeReadCache;
	use super::{PrototypeReadCache, PrototypeScanCursor, ReadIntent, ReadPolicy};
	#[cfg(debug_assertions)]
	use crate::atomic_arena::publication_validation_count;
	#[cfg(windows)]
	use crate::windows_vm::HomeSourceSpec;

	/// Creates a home key for one deterministic test page.
	fn key(page_no: u64) -> FerrosGrynnResult<PageCacheKey> {
		key_with_version(page_no, 1, 100)
	}

	/// Creates an exact home key for one source epoch.
	fn key_with_version(
		page_no: u64,
		generation: u64,
		ceiling: u64,
	) -> FerrosGrynnResult<PageCacheKey> {
		let file_id = DataFileId::new(1, DataFileKind::NodesHot).ok_or_else(|| {
			FerrosGrynnError::invalid_storage_file("test data-file identity is out of range")
		})?;
		let graph_id = GraphId::new(1)
			.ok_or_else(|| FerrosGrynnError::invalid_storage_file("test graph identity is zero"))?;
		Ok(PageCacheKey::new(
			PageKey::for_graph(graph_id, file_id, PageNo::new(page_no)),
			PageCacheVersion::Home {
				generation: CheckpointGeneration::new(generation),
				ceiling: Lsn::new(ceiling),
			},
		))
	}

	/// Fills a page matching one test key.
	fn load(key: PageCacheKey, target: &mut AlignedPage) -> FerrosGrynnResult<PageHeader> {
		*target = AlignedPage::new(
			PageType::NodeHot,
			PageFlags::empty(),
			PageLsn::new(10),
			key.page_no(),
		);
		target.validate(key.page_no())
	}

	#[test]
	fn intent_selects_the_expected_default_policy() {
		assert_eq!(PrototypeScanCursor::new(ReadIntent::Point).policy(), ReadPolicy::CacheAll);
		assert_eq!(
			PrototypeScanCursor::new(ReadIntent::IndexTraversal).policy(),
			ReadPolicy::CacheAll
		);
		assert_eq!(
			PrototypeScanCursor::new(ReadIntent::SequentialScan {
				scan_id: 1,
				expected_pages: 10,
				one_shot: true,
			})
			.policy(),
			ReadPolicy::RocksLikeBypass
		);
		assert_eq!(
			PrototypeScanCursor::new(ReadIntent::SequentialScan {
				scan_id: 1,
				expected_pages: 10,
				one_shot: false,
			})
			.policy(),
			ReadPolicy::InnodbLikeCold
		);
		assert_eq!(
			PrototypeScanCursor::new(ReadIntent::BulkBuild).policy(),
			ReadPolicy::RocksLikeBypass
		);
	}

	#[test]
	fn nested_same_cursor_read_fails_before_bypass_scratch_relock() -> Result<(), Box<dyn Error>> {
		let cache = PrototypeReadCache::for_policy(NonZeroUsize::MIN, ReadPolicy::RocksLikeBypass);
		let cursor = PrototypeScanCursor::new(ReadIntent::BulkBuild);
		let outer_key = key(20)?;
		let nested_key = key(21)?;
		let nested_loader_calls = AtomicUsize::new(0);
		let nested_consumer_calls = AtomicUsize::new(0);

		let nested_status = cache.read_with(
			&cursor,
			outer_key,
			|page| load(outer_key, page),
			|_| {
				let nested = cache.read_with(
					&cursor,
					nested_key,
					|page| {
						nested_loader_calls.fetch_add(1, Ordering::Relaxed);
						load(nested_key, page)
					},
					|_| {
						nested_consumer_calls.fetch_add(1, Ordering::Relaxed);
						Ok(())
					},
				);
				let error = nested.err().ok_or_else(|| {
					FerrosGrynnError::storage_io_failure(
						"nested bypass read unexpectedly succeeded",
					)
				})?;
				Ok(error.status().code().to_string())
			},
		)?;

		assert_eq!(nested_status, "FG-51I02");
		assert_eq!(nested_loader_calls.load(Ordering::Relaxed), 0);
		assert_eq!(nested_consumer_calls.load(Ordering::Relaxed), 0);
		assert_eq!(cache.hot_resident_count(), 0);
		Ok(())
	}

	#[test]
	fn nested_capacity_one_miss_fails_before_waiting_for_the_pinned_frame()
	-> Result<(), Box<dyn Error>> {
		let cache = PrototypeReadCache::for_policy(NonZeroUsize::MIN, ReadPolicy::CacheAll);
		let cursor = PrototypeScanCursor::new(ReadIntent::Point);
		let outer_key = key(22)?;
		let nested_key = key(23)?;
		let nested_loader_calls = AtomicUsize::new(0);
		let nested_consumer_calls = AtomicUsize::new(0);

		let nested_status = cache.read_with(
			&cursor,
			outer_key,
			|page| load(outer_key, page),
			|_| {
				let nested = cache.read_with(
					&cursor,
					nested_key,
					|page| {
						nested_loader_calls.fetch_add(1, Ordering::Relaxed);
						load(nested_key, page)
					},
					|_| {
						nested_consumer_calls.fetch_add(1, Ordering::Relaxed);
						Ok(())
					},
				);
				let error = nested.err().ok_or_else(|| {
					FerrosGrynnError::storage_io_failure(
						"nested capacity-one miss unexpectedly succeeded",
					)
				})?;
				Ok(error.status().code().to_string())
			},
		)?;

		assert_eq!(nested_status, "FG-51I02");
		assert_eq!(nested_loader_calls.load(Ordering::Relaxed), 0);
		assert_eq!(nested_consumer_calls.load(Ordering::Relaxed), 0);
		assert_eq!(cache.hot_resident_count(), 1);
		cache.read_with(
			&cursor,
			outer_key,
			|_| Err(FerrosGrynnError::storage_io_failure("resident page invoked loader")),
			|page| Ok(page.validate(outer_key.page_no())?.page_no()),
		)?;
		Ok(())
	}

	#[test]
	fn read_with_reentrancy_marker_is_cleared_during_unwind() -> Result<(), Box<dyn Error>> {
		let cache = PrototypeReadCache::for_policy(NonZeroUsize::MIN, ReadPolicy::RocksLikeBypass);
		let cursor = PrototypeScanCursor::new(ReadIntent::BulkBuild);
		let first_key = key(24)?;
		let second_key = key(25)?;

		let panicked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
			cache
				.read_with(
					&cursor,
					first_key,
					|page| load(first_key, page),
					|_| -> FerrosGrynnResult<()> {
						panic!("intentional consumer panic verifies RAII cleanup")
					},
				)
				.ok()
		}));
		assert!(panicked.is_err());

		cache.read_with(
			&cursor,
			second_key,
			|page| load(second_key, page),
			|page| Ok(page.validate(second_key.page_no())?.page_no()),
		)?;
		Ok(())
	}

	#[test]
	fn bypass_preserves_resident_count_and_still_hits_hot_pages() -> Result<(), Box<dyn Error>> {
		let cache = PrototypeReadCache::for_policy(
			NonZeroUsize::new(8).ok_or("zero capacity")?,
			ReadPolicy::RocksLikeBypass,
		);
		let hot_cursor = PrototypeScanCursor::new(ReadIntent::Point);
		let hot_key = key(1)?;
		cache.read_with(&hot_cursor, hot_key, |page| load(hot_key, page), |_| Ok(()))?;
		let before = cache.hot_resident_count();

		let bypass = PrototypeScanCursor::new(ReadIntent::SequentialScan {
			scan_id: 9,
			expected_pages: 16,
			one_shot: true,
		});
		for page_no in 2..12 {
			let page_key = key(page_no)?;
			cache.read_with(&bypass, page_key, |page| load(page_key, page), |_| Ok(()))?;
		}
		assert_eq!(cache.hot_resident_count(), before);
		cache.read_with(
			&bypass,
			hot_key,
			|_| Err(FerrosGrynnError::storage_io_failure("hot bypass hit invoked loader")),
			|page| Ok(page.validate(hot_key.page_no())?.page_no()),
		)?;
		assert_eq!(cache.hot_resident_count(), before);
		Ok(())
	}

	#[test]
	fn cold_region_is_bounded_and_other_cursor_promotes() -> Result<(), Box<dyn Error>> {
		let cache = PrototypeReadCache::new(NonZeroUsize::new(8).ok_or("zero capacity")?);
		let first_scan = PrototypeScanCursor::new(ReadIntent::SequentialScan {
			scan_id: 5,
			expected_pages: 16,
			one_shot: false,
		});
		for page_no in 1..12 {
			let page_key = key(page_no)?;
			cache.read_with(&first_scan, page_key, |page| load(page_key, page), |_| Ok(()))?;
			assert!(cache.cold_resident_count() <= cache.cold_capacity());
		}
		assert_eq!(cache.hot_resident_count(), 0);
		let promoted_key = key(11)?;
		cache.read_with(
			&first_scan,
			promoted_key,
			|_| Err(FerrosGrynnError::storage_io_failure("same scan cold hit invoked loader")),
			|_| Ok(()),
		)?;
		assert_eq!(cache.hot_resident_count(), 0);

		let cold_before_promotion = cache.cold_resident_count();
		let second_scan = PrototypeScanCursor::new(ReadIntent::SequentialScan {
			scan_id: 5,
			expected_pages: 16,
			one_shot: false,
		});
		cache.read_with(
			&second_scan,
			promoted_key,
			|_| Err(FerrosGrynnError::storage_io_failure("cold promotion invoked loader")),
			|page| Ok(page.validate(promoted_key.page_no())?.page_no()),
		)?;
		assert_eq!(cache.hot_resident_count(), 1);
		assert_eq!(cache.cold_resident_count(), cold_before_promotion - 1);
		Ok(())
	}

	#[cfg(debug_assertions)]
	#[test]
	fn cold_to_hot_promotion_validates_destination_once() -> Result<(), Box<dyn Error>> {
		let cache = PrototypeReadCache::new(NonZeroUsize::new(8).ok_or("zero capacity")?);
		let page_key = key(17)?;
		let cold_cursor = PrototypeScanCursor::new(ReadIntent::SequentialScan {
			scan_id: 17,
			expected_pages: 1,
			one_shot: false,
		});
		cache.read_with(&cold_cursor, page_key, |page| load(page_key, page), |_| Ok(()))?;

		let cold = cache.cold.as_ref().ok_or("cold arena was not configured")?;
		let cold_guard =
			cold.get_if_resident(page_key)?.ok_or("cold page was not resident before promotion")?;
		let validations_before = publication_validation_count();
		let promoted = cache.hot.get_or_copy(page_key, cold_guard.as_page())?;
		let validations_after = publication_validation_count();

		assert_eq!(validations_after - validations_before, 1);
		assert_eq!(promoted.as_page().as_bytes(), cold_guard.as_page().as_bytes());
		drop((promoted, cold_guard));
		cold.remove_resident(page_key)?;
		cache.cold_entries.lock().remove(&page_key);
		assert_eq!(cache.hot_resident_count(), 1);
		assert_eq!(cache.cold_resident_count(), 0);
		Ok(())
	}

	#[test]
	fn bypass_uses_a_page_already_resident_in_the_cold_region() -> Result<(), Box<dyn Error>> {
		let cache = PrototypeReadCache::new(NonZeroUsize::new(8).ok_or("zero capacity")?);
		let page_key = key(7)?;
		let cold = PrototypeScanCursor::new(ReadIntent::SequentialScan {
			scan_id: 1,
			expected_pages: 8,
			one_shot: false,
		});
		cache.read_with(&cold, page_key, |page| load(page_key, page), |_| Ok(()))?;
		let bypass = PrototypeScanCursor::with_policy(
			ReadIntent::SequentialScan {
				scan_id: 2,
				expected_pages: 8,
				one_shot: true,
			},
			Some(ReadPolicy::RocksLikeBypass),
		);
		cache.read_with(
			&bypass,
			page_key,
			|_| Err(FerrosGrynnError::storage_io_failure("cold bypass hit invoked loader")),
			|page| Ok(page.validate(page_key.page_no())?.page_no()),
		)?;
		assert_eq!(cache.hot_resident_count(), 0);
		assert_eq!(cache.cold_resident_count(), 1);
		Ok(())
	}

	#[test]
	fn cold_loading_and_point_read_coalesce_without_cross_tier_duplicate()
	-> Result<(), Box<dyn Error>> {
		let cache = PrototypeReadCache::new(NonZeroUsize::new(8).ok_or("zero capacity")?);
		let page_key = key(12)?;
		let cold = PrototypeScanCursor::new(ReadIntent::SequentialScan {
			scan_id: 1,
			expected_pages: 8,
			one_shot: false,
		});
		let point = PrototypeScanCursor::new(ReadIntent::Point);
		let loaders = AtomicUsize::new(0);
		let (started_tx, started_rx) = mpsc::channel();
		let (release_tx, release_rx) = mpsc::channel();
		let (done_tx, done_rx) = mpsc::channel();

		thread::scope(|scope| {
			let cold_done = done_tx.clone();
			let cache_ref = &cache;
			let cold_ref = &cold;
			let loaders_ref = &loaders;
			scope.spawn(move || {
				let result = cache_ref.read_with(
					cold_ref,
					page_key,
					|page| {
						loaders_ref.fetch_add(1, Ordering::Relaxed);
						let _ = started_tx.send(());
						release_rx.recv().map_err(|error| {
							FerrosGrynnError::storage_io_failure(error.to_string())
						})?;
						load(page_key, page)
					},
					|_| Ok(()),
				);
				let _ = cold_done.send(result);
			});
			assert!(started_rx.recv_timeout(Duration::from_secs(1)).is_ok());
			let cache_ref = &cache;
			let point_ref = &point;
			let loaders_ref = &loaders;
			scope.spawn(move || {
				let result = cache_ref.read_with(
					point_ref,
					page_key,
					|page| {
						loaders_ref.fetch_add(1, Ordering::Relaxed);
						load(page_key, page)
					},
					|_| Ok(()),
				);
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
		assert_eq!(cache.hot_resident_count(), 1);
		assert_eq!(cache.cold_resident_count(), 0);
		Ok(())
	}

	#[test]
	fn bypass_revalidates_bytes_after_loader_returns_header() -> Result<(), Box<dyn Error>> {
		let cache = PrototypeReadCache::for_policy(
			NonZeroUsize::new(4).ok_or("zero capacity")?,
			ReadPolicy::RocksLikeBypass,
		);
		let cursor = PrototypeScanCursor::new(ReadIntent::BulkBuild);
		let page_key = key(15)?;
		let result = cache.read_with(
			&cursor,
			page_key,
			|page| {
				let header = load(page_key, page)?;
				page.as_bytes_mut()[0] ^= 0xff;
				Ok(header)
			},
			|_| Ok(()),
		);
		assert!(result.is_err());
		assert_eq!(cache.hot_resident_count(), 0);
		Ok(())
	}

	#[cfg(windows)]
	#[test]
	fn vm_cache_all_admits_and_reuses_one_exact_home_page() -> Result<(), Box<dyn Error>> {
		let page_key = key(1)?;
		let source =
			HomeSourceSpec::from_key(page_key, NonZeroUsize::new(8).ok_or("zero page count")?)?;
		let cache = VmPrototypeReadCache::for_policy(
			&[source],
			NonZeroUsize::new(4).ok_or("zero capacity")?,
			ReadPolicy::CacheAll,
		)?;
		let cursor = PrototypeScanCursor::new(ReadIntent::Point);
		let loader_calls = AtomicUsize::new(0);

		for _ in 0..2 {
			cache.read_with(
				&cursor,
				page_key,
				|page| {
					loader_calls.fetch_add(1, Ordering::Relaxed);
					load(page_key, page)
				},
				|page| Ok(page.validate(page_key.page_no())?.page_no()),
			)?;
		}

		assert_eq!(loader_calls.load(Ordering::Relaxed), 1);
		assert_eq!(cache.hot_resident_count(), 1);
		assert_eq!(cache.cold_resident_count(), 0);
		assert_eq!(
			cache.committed_bytes(),
			ferros_grynn::storage_engine_prototype_api::page::PAGE_SIZE
		);
		Ok(())
	}

	#[cfg(windows)]
	#[test]
	fn vm_bypass_hits_hot_pages_without_polluting_the_vm_budget() -> Result<(), Box<dyn Error>> {
		let sample = key(0)?;
		let source =
			HomeSourceSpec::from_key(sample, NonZeroUsize::new(32).ok_or("zero page count")?)?;
		let cache = VmPrototypeReadCache::for_policy(
			&[source],
			NonZeroUsize::new(8).ok_or("zero capacity")?,
			ReadPolicy::RocksLikeBypass,
		)?;
		let hot_cursor = PrototypeScanCursor::new(ReadIntent::Point);
		cache.read_with(&hot_cursor, sample, |page| load(sample, page), |_| Ok(()))?;
		let resident_before = cache.hot_resident_count();
		let committed_before = cache.committed_bytes();
		let bypass = PrototypeScanCursor::new(ReadIntent::SequentialScan {
			scan_id: 50,
			expected_pages: 16,
			one_shot: true,
		});

		for page_no in 1..16 {
			let page_key = key(page_no)?;
			cache.read_with(&bypass, page_key, |page| load(page_key, page), |_| Ok(()))?;
		}
		cache.read_with(
			&bypass,
			sample,
			|_| Err(FerrosGrynnError::storage_io_failure("VM hot hit invoked bypass loader")),
			|page| Ok(page.validate(sample.page_no())?.page_no()),
		)?;

		assert_eq!(cache.hot_resident_count(), resident_before);
		assert_eq!(cache.committed_bytes(), committed_before);
		Ok(())
	}

	#[cfg(windows)]
	#[test]
	fn vm_cold_region_is_bounded_and_promotes_by_cursor_epoch_or_point()
	-> Result<(), Box<dyn Error>> {
		let sample = key(0)?;
		let source =
			HomeSourceSpec::from_key(sample, NonZeroUsize::new(32).ok_or("zero page count")?)?;
		let cache =
			VmPrototypeReadCache::new(&[source], NonZeroUsize::new(8).ok_or("zero capacity")?)?;
		let first_scan = PrototypeScanCursor::new(ReadIntent::SequentialScan {
			scan_id: 60,
			expected_pages: 16,
			one_shot: false,
		});
		for page_no in 1..12 {
			let page_key = key(page_no)?;
			cache.read_with(&first_scan, page_key, |page| load(page_key, page), |_| Ok(()))?;
			assert!(cache.cold_resident_count() <= cache.cold_capacity());
			assert!(cache.hot_resident_count() + cache.cold_resident_count() <= 8);
		}
		let epoch_key = key(11)?;
		cache.read_with(
			&first_scan,
			epoch_key,
			|_| Err(FerrosGrynnError::storage_io_failure("same VM scan invoked loader")),
			|_| Ok(()),
		)?;
		assert_eq!(cache.hot_resident_count(), 0);

		let cold_before_epoch = cache.cold_resident_count();
		let second_scan = PrototypeScanCursor::new(ReadIntent::SequentialScan {
			scan_id: 60,
			expected_pages: 16,
			one_shot: false,
		});
		cache.read_with(
			&second_scan,
			epoch_key,
			|_| Err(FerrosGrynnError::storage_io_failure("VM epoch promotion invoked loader")),
			|_| Ok(()),
		)?;
		assert_eq!(cache.hot_resident_count(), 1);
		assert_eq!(cache.cold_resident_count(), cold_before_epoch - 1);

		let point_key = key(10)?;
		let point = PrototypeScanCursor::new(ReadIntent::Point);
		cache.read_with(
			&point,
			point_key,
			|_| Err(FerrosGrynnError::storage_io_failure("VM point promotion invoked loader")),
			|_| Ok(()),
		)?;
		assert_eq!(cache.hot_resident_count(), 2);
		assert!(cache.hot_resident_count() + cache.cold_resident_count() <= 8);
		Ok(())
	}

	#[cfg(windows)]
	#[test]
	fn vm_policy_distinguishes_exact_home_source_epochs() -> Result<(), Box<dyn Error>> {
		let first = key_with_version(0, 1, 100)?;
		let second = key_with_version(0, 2, 200)?;
		let unregistered = key_with_version(0, 3, 300)?;
		let page_count = NonZeroUsize::MIN;
		let sources = [
			HomeSourceSpec::from_key(first, page_count)?,
			HomeSourceSpec::from_key(second, page_count)?,
		];
		let cache = VmPrototypeReadCache::for_policy(
			&sources,
			NonZeroUsize::new(4).ok_or("zero capacity")?,
			ReadPolicy::CacheAll,
		)?;
		let cursor = PrototypeScanCursor::new(ReadIntent::Point);
		let loader_calls = AtomicUsize::new(0);
		for page_key in [first, second] {
			cache.read_with(
				&cursor,
				page_key,
				|page| {
					loader_calls.fetch_add(1, Ordering::Relaxed);
					load(page_key, page)
				},
				|_| Ok(()),
			)?;
		}
		assert_eq!(loader_calls.load(Ordering::Relaxed), 2);
		assert_eq!(cache.hot_resident_count(), 2);

		let unregistered_loader_calls = AtomicUsize::new(0);
		let result = cache.read_with(
			&cursor,
			unregistered,
			|page| {
				unregistered_loader_calls.fetch_add(1, Ordering::Relaxed);
				load(unregistered, page)
			},
			|_| Ok(()),
		);
		assert!(result.is_err());
		assert_eq!(unregistered_loader_calls.load(Ordering::Relaxed), 0);
		assert_eq!(cache.hot_resident_count(), 2);
		Ok(())
	}
}
