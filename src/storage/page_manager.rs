//! Read-only synchronous page-manager slice used before frame-WAL exists.
//!
//! The full snapshot-aware `PageManager` depends on the stage-3 WAL version
//! index. This stage-2 type connects one immutable Direct-I/O source to the
//! version-aware [`BufferPool`] without inventing transaction semantics.

use std::{num::NonZeroUsize, path::Path};

use parking_lot::Mutex;

use super::{
	buffer_pool::{BufferPool, CacheSourceId, PageCacheKey, PageGuard},
	direct::{DirectFile, SYNC_BATCH_PAGES},
	page::{AlignedPage, PageLsn, PageNo},
};
use crate::gql_status::{FerrosGrynnError, FerrosGrynnResult};

/// Synchronous read path for one immutable physical page source.
pub(super) struct SyncPageManager {
	/// Stable identity included in every cache key.
	source_id: CacheSourceId,
	/// Data file opened with native Direct I/O.
	file: DirectFile,
	/// Fixed-budget immutable page cache.
	pool: BufferPool,
	/// Reusable contiguous storage for one calibrated physical extent.
	batch_scratch: Mutex<Vec<AlignedPage>>,
}

impl SyncPageManager {
	/// Opens one immutable Direct-I/O source and allocates its fixed buffers.
	///
	/// # Errors
	///
	/// Returns an error if Direct I/O is unavailable, the file cannot be
	/// opened, or its length violates the page format.
	pub(super) fn open_read_only(
		path: impl AsRef<Path>,
		source_id: CacheSourceId,
		pool_pages: NonZeroUsize,
	) -> FerrosGrynnResult<Self> {
		Ok(Self {
			source_id,
			file: DirectFile::open_read_only(path)?,
			pool: BufferPool::new(pool_pages),
			batch_scratch: Mutex::new(
				(0..SYNC_BATCH_PAGES).map(|_| AlignedPage::zeroed()).collect(),
			),
		})
	}

	/// Reads one exact immutable page version.
	///
	/// # Errors
	///
	/// Returns an I/O, page-validation, version-mismatch, or cache error.
	pub(super) fn read_page(
		&self,
		page_no: PageNo,
		version: PageLsn,
	) -> FerrosGrynnResult<PageGuard> {
		let key = PageCacheKey::new(self.source_id, page_no, version);
		self.pool.get_or_load(key, |page| self.file.read_page(page_no, page))
	}

	/// Reads exact immutable page versions in input order.
	///
	/// Duplicate requests share one pin. Physically adjacent misses are loaded
	/// in calibrated extents; the cache metadata mutex is not held during I/O.
	///
	/// # Errors
	///
	/// Returns an error if the unique request count exceeds pool capacity, a
	/// Direct-I/O extent fails, or any page does not match its number/version.
	pub(super) fn read_pages(
		&self,
		pages: &[(PageNo, PageLsn)],
	) -> FerrosGrynnResult<Vec<PageGuard>> {
		if pages.is_empty() {
			return Ok(Vec::new());
		}
		let keys = pages
			.iter()
			.map(|(page_no, version)| PageCacheKey::new(self.source_id, *page_no, *version))
			.collect::<Vec<_>>();
		let mut scratch = self.batch_scratch.lock();
		self.pool.get_or_load_batch(&keys, &mut scratch, |source_id, first_page, output| {
			if source_id != self.source_id {
				return Err(FerrosGrynnError::storage_io_failure(
					"page manager requested an unknown physical source",
				));
			}
			self.file.read_pages_raw(first_page, output)
		})
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
		io,
		path::{Path, PathBuf},
		sync::atomic::{AtomicU64, Ordering},
	};

	use super::*;
	use crate::storage::page::{PAGE_HEADER_SIZE, PageFlags, PageType};

	/// Monotonic suffix for collision-resistant manager test paths.
	static NEXT_TEST_FILE: AtomicU64 = AtomicU64::new(0);

	/// Temporary test path removed on success and unwind.
	struct TestPath(PathBuf);

	impl TestPath {
		/// Allocates one unique path in the workspace.
		fn new() -> io::Result<Self> {
			let suffix = NEXT_TEST_FILE.fetch_add(1, Ordering::Relaxed);
			let name = format!(".ferros-grynn-page-manager-{}-{suffix}.fgb", std::process::id());
			Ok(Self(std::env::current_dir()?.join(name)))
		}

		/// Returns the temporary path.
		fn as_path(&self) -> &Path {
			&self.0
		}
	}

	impl Drop for TestPath {
		fn drop(&mut self) {
			drop(std::fs::remove_file(&self.0));
		}
	}

	#[cfg(any(target_os = "linux", windows))]
	#[test]
	fn batch_read_preserves_order_duplicates_and_versions() -> Result<(), Box<dyn Error>> {
		let path = TestPath::new()?;
		let file = DirectFile::create_new(path.as_path())?;
		let mut source_pages = Vec::new();
		for raw_page_no in 0_u64..4 {
			let page_no = PageNo::new(raw_page_no);
			let version = PageLsn::new(raw_page_no + 10);
			let mut page =
				AlignedPage::new(PageType::StorageSuperblock, PageFlags::empty(), version, page_no);
			page.as_bytes_mut()[PAGE_HEADER_SIZE] = raw_page_no as u8;
			page.seal()?;
			source_pages.push(page);
		}
		file.write_pages(PageNo::new(0), &source_pages)?;
		file.sync_data()?;
		drop(file);

		let manager = SyncPageManager::open_read_only(
			path.as_path(),
			CacheSourceId::new(7),
			NonZeroUsize::new(4).ok_or_else(|| io::Error::other("invalid test capacity"))?,
		)?;
		let requests = [
			(PageNo::new(2), PageLsn::new(12)),
			(PageNo::new(0), PageLsn::new(10)),
			(PageNo::new(2), PageLsn::new(12)),
			(PageNo::new(1), PageLsn::new(11)),
		];
		let pages = manager.read_pages(&requests)?;
		assert_eq!(
			pages.iter().map(|page| page.as_bytes()[PAGE_HEADER_SIZE]).collect::<Vec<_>>(),
			[2, 0, 2, 1]
		);
		assert!(std::ptr::eq(pages[0].as_bytes(), pages[2].as_bytes()));
		drop(pages);
		let scalar = manager.read_page(PageNo::new(3), PageLsn::new(13))?;
		assert_eq!(scalar.as_bytes()[PAGE_HEADER_SIZE], 3);
		Ok(())
	}
}
