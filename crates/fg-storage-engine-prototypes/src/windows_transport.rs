//! Windows-only Direct-I/O transport prototypes.

use std::ptr;

use ferros_grynn::{
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	storage_engine_prototype_api::{
		direct::DirectFile,
		page::{AlignedPage, PAGE_SIZE, PageHeader, PageNo},
	},
};
use windows::{
	Win32::{
		Foundation::{CloseHandle, ERROR_IO_PENDING, HANDLE},
		Storage::FileSystem::{FILE_SEGMENT_ELEMENT, ReadFileScatter},
		System::{
			IO::{GetOverlappedResult, OVERLAPPED, OVERLAPPED_0, OVERLAPPED_0_0},
			SystemInformation::{GetSystemInfo, SYSTEM_INFO},
			Threading::{CreateEventW, ResetEvent},
		},
	},
	core::{HRESULT, PCWSTR},
};

/// Event retained until each overlapped scatter read fully completes.
struct ScatterEvent(HANDLE);

impl ScatterEvent {
	/// Creates one reusable manual-reset event.
	fn new() -> FerrosGrynnResult<Self> {
		// SAFETY: no security descriptor or name is supplied; the returned
		// event is exclusively owned by the RAII wrapper.
		let handle = unsafe { CreateEventW(None, true, false, PCWSTR::null()) }
			.map_err(|source| windows_failure("create ReadFileScatter event", source))?;
		Ok(Self(handle))
	}

	/// Resets the event after the preceding operation reached completion.
	fn reset(&self) -> FerrosGrynnResult<()> {
		// SAFETY: this is the wrapper's valid event and no operation is in
		// flight.
		unsafe { ResetEvent(self.0) }
			.map_err(|source| windows_failure("reset ReadFileScatter event", source))
	}
}

impl Drop for ScatterEvent {
	/// Closes the event only after the final synchronous completion.
	fn drop(&mut self) {
		// SAFETY: this wrapper owns the valid event handle and closes it once.
		drop(unsafe { CloseHandle(self.0) });
	}
}

/// Reusable scatter state excluded from per-extent allocation measurements.
pub(super) struct ScatterReader {
	/// Native system page size cached once.
	system_page_size: usize,
	/// Reused PVOID64 segment array.
	segments: Vec<FILE_SEGMENT_ELEMENT>,
	/// Reused completion event.
	event: ScatterEvent,
	/// Reused request record retained through completion.
	overlapped: Box<OVERLAPPED>,
}

impl ScatterReader {
	/// Creates a reusable synchronous scatter context.
	///
	/// # Errors
	///
	/// Returns an error for an incompatible system page size or event failure.
	pub(super) fn new() -> FerrosGrynnResult<Self> {
		let mut system_info = SYSTEM_INFO::default();
		// SAFETY: `system_info` is a valid writable output record.
		unsafe { GetSystemInfo(&raw mut system_info) };
		let system_page_size = usize::try_from(system_info.dwPageSize).map_err(|source| {
			FerrosGrynnError::storage_io_failure(format!(
				"Windows system page size does not fit usize: {source}"
			))
		})?;
		if system_page_size == 0 || !PAGE_SIZE.is_multiple_of(system_page_size) {
			return Err(FerrosGrynnError::io_mode_unavailable(
				"ReadFileScatter",
				format!(
					"database page size {PAGE_SIZE} is not a multiple of system page size \
					 {system_page_size}"
				),
			));
		}
		Ok(Self {
			system_page_size,
			segments: Vec::new(),
			event: ScatterEvent::new()?,
			overlapped: Box::new(OVERLAPPED::default()),
		})
	}

	/// Reads one contiguous file extent into disjoint aligned frames without
	/// checksum validation.
	///
	/// # Errors
	///
	/// Returns range, alignment, native I/O, or exact-byte-count errors.
	pub(super) fn read_raw(
		&mut self,
		file: &DirectFile,
		first_page: PageNo,
		pages: &mut [&mut AlignedPage],
	) -> FerrosGrynnResult<()> {
		if pages.is_empty() {
			return Err(FerrosGrynnError::invalid_storage_page(
				first_page.get(),
				"ReadFileScatter extent must not be empty",
			));
		}
		let file_offset = file.prepare_read_extent(first_page, pages.len())?;
		let byte_len = pages.len().checked_mul(PAGE_SIZE).ok_or_else(|| {
			FerrosGrynnError::storage_io_failure("ReadFileScatter byte length overflow")
		})?;
		let byte_len_u32 = u32::try_from(byte_len).map_err(|source| {
			FerrosGrynnError::storage_io_failure(format!(
				"ReadFileScatter byte length does not fit u32: {source}"
			))
		})?;
		let segments_per_page = PAGE_SIZE / self.system_page_size;
		let segment_count = pages.len().checked_mul(segments_per_page).ok_or_else(|| {
			FerrosGrynnError::storage_io_failure("ReadFileScatter segment count overflow")
		})?;
		self.segments.clear();
		if self.segments.capacity() < segment_count {
			self.segments.reserve(segment_count - self.segments.capacity());
		}
		for page in pages.iter_mut() {
			let base = page.as_bytes_mut().as_mut_ptr();
			for segment in 0..segments_per_page {
				let offset = segment.checked_mul(self.system_page_size).ok_or_else(|| {
					FerrosGrynnError::storage_io_failure("ReadFileScatter segment offset overflow")
				})?;
				// SAFETY: `offset < PAGE_SIZE`; each element covers one
				// disjoint system page in an exclusively borrowed aligned
				// frame.
				let address = unsafe { base.add(offset) };
				if !(address as usize).is_multiple_of(self.system_page_size) {
					return Err(FerrosGrynnError::storage_io_failure(
						"ReadFileScatter destination is not system-page aligned",
					));
				}
				// `FILE_SEGMENT_ELEMENT` is a native PVOID64 union. Writing its
				// u64 arm zero-extends 32-bit pointers as required by
				// ReadFileScatter.
				self.segments.push(FILE_SEGMENT_ELEMENT {
					Alignment: (address as usize) as u64,
				});
			}
		}

		self.event.reset()?;
		*self.overlapped = OVERLAPPED::default();
		self.overlapped.Anonymous = OVERLAPPED_0 {
			Anonymous: OVERLAPPED_0_0 {
				Offset: file_offset as u32,
				OffsetHigh: (file_offset >> 32) as u32,
			},
		};
		self.overlapped.hEvent = self.event.0;
		let handle = file.raw_handle()?;
		// SAFETY: the handle is OVERLAPPED|NO_BUFFERING; every PVOID64 segment
		// names a distinct aligned system page, and request storage plus frames
		// remain alive through terminal completion below.
		let submitted = unsafe {
			ReadFileScatter(
				handle,
				self.segments.as_ptr(),
				byte_len_u32,
				None,
				ptr::from_mut(self.overlapped.as_mut()),
			)
		};
		if let Err(source) = submitted {
			if source.code() != HRESULT::from_win32(ERROR_IO_PENDING.0) {
				return Err(windows_failure("submit ReadFileScatter", source));
			}
		}
		let mut transferred = 0_u32;
		// SAFETY: waiting to completion ends every native access to the
		// buffers.
		unsafe {
			GetOverlappedResult(handle, self.overlapped.as_ref(), &raw mut transferred, true)
		}
		.map_err(|source| windows_failure("complete ReadFileScatter", source))?;
		if transferred != byte_len_u32 {
			return Err(FerrosGrynnError::storage_io_failure(format!(
				"short ReadFileScatter transfer: {transferred} of {byte_len_u32} bytes"
			)));
		}
		Ok(())
	}

	/// Reads and validates the complete extent before caller publication.
	///
	/// # Errors
	///
	/// Returns raw transport, header, or checksum errors.
	pub(super) fn read_and_validate(
		&mut self,
		file: &DirectFile,
		first_page: PageNo,
		pages: &mut [&mut AlignedPage],
	) -> FerrosGrynnResult<Vec<PageHeader>> {
		self.read_raw(file, first_page, pages)?;
		let mut headers = Vec::with_capacity(pages.len());
		Self::validate_into(first_page, pages, &mut headers)?;
		Ok(headers)
	}

	/// Validates previously scattered pages into caller-reused header storage.
	///
	/// The output is cleared before validation and again after any failure, so
	/// a caller never observes a partially validated extent as complete.
	///
	/// # Errors
	///
	/// Returns page-number conversion, header, or checksum errors.
	pub(super) fn validate_into(
		first_page: PageNo,
		pages: &[&mut AlignedPage],
		headers: &mut Vec<PageHeader>,
	) -> FerrosGrynnResult<()> {
		headers.clear();
		let result = (|| {
			if headers.capacity() < pages.len() {
				headers.reserve(pages.len() - headers.capacity());
			}
			for (index, page) in pages.iter().enumerate() {
				let page_delta = u64::try_from(index).map_err(|source| {
					FerrosGrynnError::storage_io_failure(format!(
						"ReadFileScatter page index does not fit u64: {source}"
					))
				})?;
				let page_no =
					first_page.get().checked_add(page_delta).map(PageNo::new).ok_or_else(|| {
						FerrosGrynnError::invalid_storage_page(
							first_page.get(),
							"ReadFileScatter page number overflow",
						)
					})?;
				headers.push(page.validate(page_no)?);
			}
			Ok(())
		})();
		if result.is_err() {
			headers.clear();
		}
		result
	}
}

/// One-off functional wrapper; benchmarks retain a [`ScatterReader`].
pub(super) fn read_scatter_extent(
	file: &DirectFile,
	first_page: PageNo,
	pages: &mut [&mut AlignedPage],
) -> FerrosGrynnResult<Vec<PageHeader>> {
	ScatterReader::new()?.read_and_validate(file, first_page, pages)
}

/// Preserves the native source behind a project storage error.
fn windows_failure(operation: &'static str, source: windows::core::Error) -> FerrosGrynnError {
	ferros_grynn::storage_engine_prototype_api::storage_io_failure_with_source(operation, source)
}

#[cfg(test)]
#[allow(
	clippy::panic_in_result_fn,
	reason = "fallible platform test setup returns Result while assertions report contract \
	          failures"
)]
mod tests {
	use std::{error::Error, num::NonZeroUsize, path::PathBuf, sync::Arc};

	use ferros_grynn::storage_engine_prototype_api::{
		buffer_pool::{PageCacheKey, PageCacheVersion},
		direct::{DirectFile, DirectFileOpenMode},
		page::{AlignedPage, PAGE_HEADER_SIZE, PAGE_SIZE, PageFlags, PageLsn, PageNo, PageType},
		page_io::{FrameRead, WindowsIoRing},
		types::{CheckpointGeneration, DataFileId, DataFileKind, GraphId, Lsn, PageKey},
	};

	use super::{
		super::{
			atomic_arena::AtomicArenaBufferPool,
			windows_vm::{HomeSourceSpec, VmCache},
		},
		ScatterReader,
		read_scatter_extent,
	};

	/// Unique path removed after each native transport test.
	struct TestPath(PathBuf);

	impl TestPath {
		/// Creates a collision-resistant workspace-local path.
		fn new() -> Result<Self, Box<dyn Error>> {
			Ok(Self(
				std::env::current_dir()?
					.join(format!(".ferros-grynn-scatter-{}.fgd", uuid::Uuid::new_v4())),
			))
		}
	}

	impl Drop for TestPath {
		fn drop(&mut self) {
			drop(std::fs::remove_file(&self.0));
		}
	}

	/// Creates two exact home keys for direct-to-frame rollback tests.
	fn home_keys() -> Result<[PageCacheKey; 2], Box<dyn Error>> {
		let file_id =
			DataFileId::new(1, DataFileKind::NodesHot).ok_or("invalid test data-file identity")?;
		Ok([0, 1].map(|page_no| {
			PageCacheKey::new(
				PageKey::new(GraphId::new(1), file_id, PageNo::new(page_no)),
				PageCacheVersion::Home {
					generation: CheckpointGeneration::new(1),
					ceiling: Lsn::new(100),
				},
			)
		}))
	}

	/// Opens a two-page overlapped handle before truncating the shared file.
	///
	/// The returned handle retains its two-page range snapshot, so the native
	/// transport, rather than the preflight range check, observes the EOF.
	fn open_then_truncate(path: &TestPath) -> Result<Arc<DirectFile>, Box<dyn Error>> {
		let writer = DirectFile::create_new(&path.0)?;
		writer.resize_pages(2)?;
		let file = Arc::new(DirectFile::open_read_write_with_mode(
			&path.0,
			DirectFileOpenMode::Overlapped,
		)?);
		writer.resize_pages(1)?;
		writer.sync_data()?;
		Ok(file)
	}

	#[test]
	fn scatter_reads_and_validates_disjoint_frames() -> Result<(), Box<dyn Error>> {
		let path = TestPath::new()?;
		let writer = DirectFile::create_new(&path.0)?;
		writer.resize_pages(8)?;
		let mut source = Vec::with_capacity(8);
		for page_no in 0..8_u64 {
			let mut page = AlignedPage::new(
				PageType::NodeHot,
				PageFlags::empty(),
				PageLsn::new(page_no + 1),
				PageNo::new(page_no),
			);
			page.as_bytes_mut()[PAGE_HEADER_SIZE] = page_no.to_le_bytes()[0];
			page.seal()?;
			source.push(page);
		}
		writer.write_pages(PageNo::new(0), &source)?;
		writer.sync_data()?;
		drop(writer);

		let file = DirectFile::open_read_write_with_mode(&path.0, DirectFileOpenMode::Overlapped)?;
		let mut destination = (0..8).map(|_| Box::new(AlignedPage::zeroed())).collect::<Vec<_>>();
		let mut disjoint = destination.iter_mut().map(Box::as_mut).collect::<Vec<_>>();
		let headers = read_scatter_extent(&file, PageNo::new(0), &mut disjoint)?;
		assert_eq!(headers.len(), 8);
		for (index, page) in destination.iter().enumerate() {
			assert_eq!(page.as_bytes()[PAGE_HEADER_SIZE], index.to_le_bytes()[0]);
		}
		Ok(())
	}

	#[test]
	fn scatter_raw_read_and_validation_are_independently_reusable() -> Result<(), Box<dyn Error>> {
		let path = TestPath::new()?;
		let writer = DirectFile::create_new(&path.0)?;
		writer.resize_pages(2)?;
		let mut source = Vec::with_capacity(2);
		for page_no in 0..2_u64 {
			let mut page = AlignedPage::new(
				PageType::NodeHot,
				PageFlags::empty(),
				PageLsn::new(page_no + 1),
				PageNo::new(page_no),
			);
			page.as_bytes_mut()[PAGE_HEADER_SIZE] = page_no.to_le_bytes()[0];
			page.seal()?;
			source.push(page);
		}
		writer.write_pages(PageNo::new(0), &source)?;
		writer.sync_data()?;
		drop(writer);

		let file = DirectFile::open_read_write_with_mode(&path.0, DirectFileOpenMode::Overlapped)?;
		let mut destination = (0..2).map(|_| Box::new(AlignedPage::zeroed())).collect::<Vec<_>>();
		let mut disjoint = destination.iter_mut().map(Box::as_mut).collect::<Vec<_>>();
		let mut reader = ScatterReader::new()?;
		reader.read_raw(&file, PageNo::new(0), &mut disjoint)?;
		let mut headers = Vec::with_capacity(2);
		ScatterReader::validate_into(PageNo::new(0), &disjoint, &mut headers)?;
		assert_eq!(headers.len(), 2);
		disjoint[1].as_bytes_mut()[PAGE_HEADER_SIZE] ^= 0xff;
		assert!(ScatterReader::validate_into(PageNo::new(0), &disjoint, &mut headers).is_err());
		assert!(headers.is_empty());
		Ok(())
	}

	#[test]
	fn scatter_eof_rolls_back_atomic_batch_and_releases_capacity() -> Result<(), Box<dyn Error>> {
		let path = TestPath::new()?;
		let file = open_then_truncate(&path)?;
		let keys = home_keys()?;
		let pool = AtomicArenaBufferPool::new(NonZeroUsize::new(2).ok_or("zero capacity")?);
		let mut batch = pool.reserve_batch(&keys)?;
		let mut pages = batch.pages_mut();
		let mut reader = ScatterReader::new()?;
		let error = reader
			.read_raw(&file, PageNo::new(0), &mut pages)
			.err()
			.ok_or("ReadFileScatter unexpectedly read beyond EOF")?;
		assert!(error.to_string().contains("ReadFileScatter"));
		drop(pages);
		drop(batch);

		assert_eq!(pool.resident_count(), 0);
		assert!(keys.iter().all(|key| pool.resolve(*key).is_none()));
		let retry = pool.reserve_batch(&keys)?;
		drop(retry);
		assert_eq!(pool.resident_count(), 0);
		Ok(())
	}

	#[test]
	fn frame_ioring_eof_rolls_back_vm_batch_and_decommits_budget() -> Result<(), Box<dyn Error>> {
		let path = TestPath::new()?;
		let file = open_then_truncate(&path)?;
		let keys = home_keys()?;
		let source = HomeSourceSpec::from_key(
			keys[0],
			NonZeroUsize::new(keys.len()).ok_or("zero page count")?,
		)?;
		let cache =
			VmCache::new(&[source], NonZeroUsize::new(keys.len()).ok_or("zero capacity")?, 0)?;
		let committed_before = cache.committed_bytes();
		let mut batch = cache.reserve_home_batch(&keys)?;
		assert_eq!(cache.committed_bytes(), committed_before + keys.len() * PAGE_SIZE);
		let pages = batch.pages_mut()?;
		let mut reads = pages
			.into_iter()
			.zip(keys)
			.map(|(frame, key)| FrameRead::new(&file, key.page_no(), frame))
			.collect::<Vec<_>>();
		let mut ring = WindowsIoRing::new()?;
		let error = ring
			.read_frames(&mut reads)
			.err()
			.ok_or("Windows IoRing unexpectedly read beyond EOF")?;
		assert!(error.to_string().contains("Windows IoRing"));
		drop(reads);
		drop(batch);

		assert_eq!(cache.committed_bytes(), committed_before);
		assert!(keys.iter().all(|key| cache.resolve(*key).is_none()));
		let retry = cache.reserve_home_batch(&keys)?;
		assert_eq!(cache.committed_bytes(), committed_before + keys.len() * PAGE_SIZE);
		drop(retry);
		assert_eq!(cache.committed_bytes(), committed_before);
		Ok(())
	}
}
