//! User-selected synchronous or native queued page transport.

use std::{ops::Range, sync::Arc};

use super::{
	direct::{DirectFile, DirectFileOpenMode},
	page::{AlignedPage, PageHeader, PageNo},
};
use crate::gql_status::{FerrosGrynnError, FerrosGrynnResult};

/// Maximum Windows IoRing submission depth requested by the first native
/// implementation.
#[cfg(all(windows, feature = "native-async-io"))]
const WINDOWS_IORING_QUEUE_DEPTH: u32 = 128;

/// Physical page transport selected when opening a local database.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PageIoMode {
	/// Synchronous positional Direct I/O.
	Sync,
	/// Windows IoRing Direct I/O.
	NativeAsync,
}

/// One file extent and its location inside a shared aligned page buffer.
pub(super) struct FilePageExtent {
	/// File retained until every native completion has been drained.
	file: Arc<DirectFile>,
	/// First physical page in the file extent.
	first_page: PageNo,
	/// First page in the caller-owned aligned buffer.
	buffer_start: usize,
	/// Number of consecutive pages in the extent.
	page_count: usize,
}

impl FilePageExtent {
	/// Describes one non-empty contiguous file extent.
	pub(super) fn new(
		file: Arc<DirectFile>,
		first_page: PageNo,
		buffer_start: usize,
		page_count: usize,
	) -> Self {
		Self {
			file,
			first_page,
			buffer_start,
			page_count,
		}
	}

	/// Returns the checked buffer range for this extent.
	fn buffer_range(&self, buffer_pages: usize) -> FerrosGrynnResult<Range<usize>> {
		let end = self.buffer_start.checked_add(self.page_count).ok_or_else(|| {
			FerrosGrynnError::storage_io_failure("page-I/O buffer extent overflow")
		})?;
		if self.page_count == 0 || end > buffer_pages {
			return Err(FerrosGrynnError::storage_io_failure(
				"page-I/O extent is empty or outside its aligned buffer",
			));
		}
		Ok(self.buffer_start..end)
	}
}

/// Concrete page transport; the surrounding manager deliberately remains a
/// synchronous pull API.
pub(super) enum PageIo {
	/// Synchronous positional operations.
	Sync,
	/// One serialized Windows IoRing submission queue.
	#[cfg(all(windows, feature = "native-async-io"))]
	// ponytail: keep one ring and mutex until profiles show cross-batch queue
	// contention; then shard rings by storage worker.
	NativeAsync(parking_lot::Mutex<WindowsIoRing>),
}

impl PageIo {
	/// Creates the selected transport and performs its runtime capability
	/// check.
	///
	/// # Errors
	///
	/// Returns `IoModeUnavailable` when the build, target, or current Windows
	/// kernel cannot provide the explicitly requested native mode.
	pub(super) fn new(mode: PageIoMode) -> FerrosGrynnResult<Self> {
		match mode {
			PageIoMode::Sync => Ok(Self::Sync),
			PageIoMode::NativeAsync => {
				#[cfg(all(windows, feature = "native-async-io"))]
				{
					WindowsIoRing::new().map(parking_lot::Mutex::new).map(Self::NativeAsync)
				}
				#[cfg(not(all(windows, feature = "native-async-io")))]
				{
					Err(FerrosGrynnError::io_mode_unavailable(
						"NativeAsync",
						"Windows IoRing support requires a Windows build with feature \
						 native-async-io",
					))
				}
			}
		}
	}

	/// Returns the handle flags required for registered data files.
	pub(super) const fn file_open_mode(&self) -> DirectFileOpenMode {
		match self {
			Self::Sync => DirectFileOpenMode::Synchronous,
			#[cfg(all(windows, feature = "native-async-io"))]
			Self::NativeAsync(_) => DirectFileOpenMode::Overlapped,
		}
	}

	/// Reads and validates one page through the selected transport.
	///
	/// # Errors
	///
	/// Returns a native transport, range, header, or checksum error.
	pub(super) fn read_page(
		&self,
		file: &Arc<DirectFile>,
		page_no: PageNo,
		page: &mut AlignedPage,
	) -> FerrosGrynnResult<PageHeader> {
		let extent = FilePageExtent::new(Arc::clone(file), page_no, 0, 1);
		self.read_extents(std::slice::from_ref(&extent), std::slice::from_mut(page))?;
		page.validate(page_no)
	}

	/// Reads independent file extents, preserving their caller-assigned buffer
	/// positions.
	///
	/// # Errors
	///
	/// Returns an error for invalid or overlapping ranges, file bounds, short
	/// I/O, or a native queue failure.
	pub(super) fn read_extents(
		&self,
		extents: &[FilePageExtent],
		pages: &mut [AlignedPage],
	) -> FerrosGrynnResult<()> {
		validate_extent_layout(extents, pages.len())?;
		match self {
			Self::Sync => {
				for extent in extents {
					let range = extent.buffer_range(pages.len())?;
					extent.file.read_pages_raw(extent.first_page, &mut pages[range])?;
				}
				Ok(())
			}
			#[cfg(all(windows, feature = "native-async-io"))]
			Self::NativeAsync(_) if extents.len() == 1 => {
				let extent = &extents[0];
				let range = extent.buffer_range(pages.len())?;
				extent.file.read_pages_raw(extent.first_page, &mut pages[range])
			}
			#[cfg(all(windows, feature = "native-async-io"))]
			Self::NativeAsync(ring) => ring.lock().read_extents(extents, pages),
		}
	}

	/// Writes independent validated file extents through the selected
	/// transport. Durability remains the caller's explicit `sync_data` barrier.
	///
	/// # Errors
	///
	/// Returns an error for invalid or overlapping ranges, file gaps, malformed
	/// pages, short I/O, or a native queue failure.
	pub(super) fn write_extents(
		&self,
		extents: &[FilePageExtent],
		pages: &[AlignedPage],
	) -> FerrosGrynnResult<()> {
		validate_extent_layout(extents, pages.len())?;
		match self {
			Self::Sync => {
				for extent in extents {
					let range = extent.buffer_range(pages.len())?;
					extent.file.write_pages(extent.first_page, &pages[range])?;
				}
				Ok(())
			}
			#[cfg(all(windows, feature = "native-async-io"))]
			Self::NativeAsync(ring) => ring.lock().write_extents(extents, pages),
		}
	}

	/// Writes one page during recovery through the selected transport.
	///
	/// # Errors
	///
	/// Returns the same errors as [`Self::write_extents`].
	pub(super) fn write_page(
		&self,
		file: &Arc<DirectFile>,
		page_no: PageNo,
		page: &AlignedPage,
	) -> FerrosGrynnResult<()> {
		let extent = FilePageExtent::new(Arc::clone(file), page_no, 0, 1);
		self.write_extents(std::slice::from_ref(&extent), std::slice::from_ref(page))
	}
}

/// Ensures concurrent native reads never receive overlapping mutable buffers.
fn validate_extent_layout(
	extents: &[FilePageExtent],
	buffer_pages: usize,
) -> FerrosGrynnResult<()> {
	let mut previous_end = 0;
	for extent in extents {
		let range = extent.buffer_range(buffer_pages)?;
		if range.start < previous_end {
			return Err(FerrosGrynnError::storage_io_failure(
				"page-I/O buffer extents overlap or are out of order",
			));
		}
		previous_end = range.end;
	}
	Ok(())
}

#[cfg(all(windows, feature = "native-async-io"))]
mod windows_ioring {
	use std::{ffi::c_void, mem, sync::Arc};

	use windows::Win32::{
		Foundation::{HANDLE, S_OK},
		Storage::FileSystem::{
			BuildIoRingReadFile,
			BuildIoRingWriteFile,
			CloseIoRing,
			CreateIoRing,
			FILE_WRITE_FLAGS_NONE,
			HIORING,
			IORING_BUFFER_REF,
			IORING_BUFFER_REF_0,
			IORING_CQE,
			IORING_CREATE_ADVISORY_FLAGS_NONE,
			IORING_CREATE_FLAGS,
			IORING_CREATE_REQUIRED_FLAGS_NONE,
			IORING_HANDLE_REF,
			IORING_HANDLE_REF_0,
			IORING_OP_READ,
			IORING_OP_WRITE,
			IORING_REF_RAW,
			IORING_VERSION,
			IOSQE_FLAGS_NONE,
			IsIoRingOpSupported,
			PopIoRingCompletion,
			QueryIoRingCapabilities,
			SubmitIoRing,
		},
	};

	use super::{
		AlignedPage,
		FerrosGrynnError,
		FerrosGrynnResult,
		FilePageExtent,
		WINDOWS_IORING_QUEUE_DEPTH,
	};
	use crate::storage::page::PAGE_SIZE;

	/// Infinite wait accepted by `SubmitIoRing`.
	const INFINITE_WAIT_MILLISECONDS: u32 = u32::MAX;
	/// Completion queues are required to hold at least two entries per SQE.
	const COMPLETIONS_PER_SUBMISSION: u32 = 2;
	/// User-data zero is reserved so a zeroed CQE cannot look valid.
	const FIRST_USER_DATA: usize = 1;
	/// One native request whose file and buffer owners live outside the ring
	/// until all completions are drained.
	struct NativeRequest {
		/// Raw file handle borrowed from a retained `DirectFile`.
		file: HANDLE,
		/// Page-aligned buffer address.
		buffer: *mut c_void,
		/// Exact transfer length.
		byte_len: u32,
		/// Page-aligned file offset.
		file_offset: u64,
		/// Read or write operation.
		operation: NativeOperation,
	}

	/// Mutually exclusive IoRing operation encoded by one SQE.
	#[derive(Clone, Copy)]
	enum NativeOperation {
		/// Read file bytes into the buffer.
		Read,
		/// Write buffer bytes into the file.
		Write,
	}

	/// Owned Windows queue used behind one manager mutex.
	pub(in crate::storage) struct WindowsIoRing {
		/// Owned kernel ring, invalid only after a failed reset.
		ring: HIORING,
		/// Runtime-selected ring version.
		version: IORING_VERSION,
		/// Maximum requests submitted together.
		queue_depth: u32,
		/// Requested completion queue size.
		completion_depth: u32,
	}

	// SAFETY: Win32 IoRing handles may move between threads. The
	// containing PageIo stores this value behind one Mutex, so SQ/CQ mutation is
	// never concurrent, and all borrowed buffers are drained before unlocking.
	unsafe impl Send for WindowsIoRing {}

	impl WindowsIoRing {
		/// Queries IoRing, checks required operations, and creates one bounded
		/// queue.
		pub(in crate::storage) fn new() -> FerrosGrynnResult<Self> {
			// SAFETY: the generated binding returns an owned capabilities value and
			// has no caller-provided pointers.
			let capabilities = unsafe { QueryIoRingCapabilities() }
				.map_err(|source| unavailable_windows("query IoRing capabilities", source))?;

			let completion_limited = capabilities
				.MaxCompletionQueueSize
				.checked_div(COMPLETIONS_PER_SUBMISSION)
				.unwrap_or(0);
			let queue_depth = WINDOWS_IORING_QUEUE_DEPTH
				.min(capabilities.MaxSubmissionQueueSize)
				.min(completion_limited);
			if queue_depth == 0 {
				return Err(FerrosGrynnError::io_mode_unavailable(
					"NativeAsync",
					"Windows reports no usable IoRing queue capacity",
				));
			}
			let completion_depth = queue_depth * COMPLETIONS_PER_SUBMISSION;
			let ring = create_ring(capabilities.MaxVersion, queue_depth, completion_depth)
				.map_err(|source| unavailable_windows("create IoRing", source))?;
			let result = Self {
				ring,
				version: capabilities.MaxVersion,
				queue_depth,
				completion_depth,
			};
			for (operation, name) in [(IORING_OP_READ, "read"), (IORING_OP_WRITE, "write")] {
				// SAFETY: `result.ring` is owned and valid for the duration of the
				// capability query.
				let supported = unsafe { IsIoRingOpSupported(result.ring, operation) };
				if !supported.as_bool() {
					return Err(FerrosGrynnError::io_mode_unavailable(
						"NativeAsync",
						format!("Windows IoRing does not support {name} operations"),
					));
				}
			}
			Ok(result)
		}

		/// Builds and executes queued reads into disjoint aligned page ranges.
		pub(super) fn read_extents(
			&mut self,
			extents: &[FilePageExtent],
			pages: &mut [AlignedPage],
		) -> FerrosGrynnResult<()> {
			let mut requests = Vec::with_capacity(extents.len());
			for extent in extents {
				let range = extent.buffer_range(pages.len())?;
				let file_offset =
					extent.file.prepare_read_extent(extent.first_page, extent.page_count)?;
				let byte_len = checked_byte_len(extent.page_count)?;
				// SAFETY: validate_extent_layout proved every range is non-empty,
				// in bounds, ordered, and disjoint. AlignedPage elements are exactly
				// PAGE_SIZE bytes, and `pages` remains exclusively borrowed until all
				// submitted completions are drained.
				let buffer = unsafe { pages.as_mut_ptr().add(range.start).cast::<c_void>() };
				requests.push(NativeRequest {
					file: extent.file.raw_handle()?,
					buffer,
					byte_len,
					file_offset,
					operation: NativeOperation::Read,
				});
			}
			self.execute(&requests)
		}

		/// Builds and executes queued writes from aligned page ranges.
		pub(super) fn write_extents(
			&mut self,
			extents: &[FilePageExtent],
			pages: &[AlignedPage],
		) -> FerrosGrynnResult<()> {
			let mut requests = Vec::with_capacity(extents.len());
			let mut ending_pages = Vec::with_capacity(extents.len());
			for extent in extents {
				let range = extent.buffer_range(pages.len())?;
				let (file_offset, end_page) =
					extent.file.prepare_write_extent(extent.first_page, &pages[range.clone()])?;
				if end_page > extent.file.page_count() {
					return Err(FerrosGrynnError::invalid_storage_page(
						extent.first_page.get(),
						"native queued writes require the file to be resized first",
					));
				}
				let byte_len = checked_byte_len(extent.page_count)?;
				let buffer = pages[range].as_ptr().cast_mut().cast::<c_void>();
				requests.push(NativeRequest {
					file: extent.file.raw_handle()?,
					buffer,
					byte_len,
					file_offset,
					operation: NativeOperation::Write,
				});
				ending_pages.push((Arc::clone(&extent.file), end_page));
			}
			self.execute(&requests)?;
			for (file, end_page) in ending_pages {
				file.complete_write_extent(end_page);
			}
			Ok(())
		}

		/// Submits requests in runtime-bounded chunks and drains every CQE
		/// before returning borrowed buffers to Rust.
		fn execute(&mut self, requests: &[NativeRequest]) -> FerrosGrynnResult<()> {
			self.ensure_ring()?;
			let queue_depth = usize::try_from(self.queue_depth).map_err(|source| {
				FerrosGrynnError::storage_io_failure(format!(
					"Windows IoRing queue depth does not fit usize: {source}"
				))
			})?;
			for requests in requests.chunks(queue_depth) {
				if let Err(error) = self.execute_chunk(requests) {
					self.reset_ring();
					return Err(error);
				}
			}
			Ok(())
		}

		/// Executes one chunk that fits the submission queue.
		fn execute_chunk(&mut self, requests: &[NativeRequest]) -> FerrosGrynnResult<()> {
			for (index, request) in requests.iter().enumerate() {
				let user_data = index.checked_add(FIRST_USER_DATA).ok_or_else(|| {
					FerrosGrynnError::storage_io_failure("Windows IoRing user-data overflow")
				})?;
				let file = raw_handle_ref(request.file);
				let buffer = raw_buffer_ref(request.buffer);
				let result = match request.operation {
					NativeOperation::Read => {
						// SAFETY: the file handle is retained by FilePageExtent, the aligned
						// buffer remains borrowed until every CQE is drained, byte lengths and
						// offsets were checked, and this ring is exclusively mutex-owned.
						unsafe {
							BuildIoRingReadFile(
								self.ring,
								file,
								buffer,
								request.byte_len,
								request.file_offset,
								user_data,
								IOSQE_FLAGS_NONE,
							)
						}
					}
					NativeOperation::Write => {
						// SAFETY: the file handle is retained by FilePageExtent, the aligned
						// buffer remains borrowed until every CQE is drained, byte lengths and
						// offsets were checked, and this ring is exclusively mutex-owned.
						unsafe {
							BuildIoRingWriteFile(
								self.ring,
								file,
								buffer,
								request.byte_len,
								request.file_offset,
								FILE_WRITE_FLAGS_NONE,
								user_data,
								IOSQE_FLAGS_NONE,
							)
						}
					}
				};
				result.map_err(|source| native_failure("build Windows IoRing request", source))?;
			}

			let expected_completions = u32::try_from(requests.len()).map_err(|source| {
				FerrosGrynnError::storage_io_failure(format!(
					"Windows IoRing request count does not fit u32: {source}"
				))
			})?;
			let mut submitted = 0_u32;
			// SAFETY: exactly `requests.len()` SQEs were built into this exclusively
			// owned ring, the wait is bounded by that count, and `submitted` is a
			// valid output pointer for the call.
			unsafe {
				SubmitIoRing(
					self.ring,
					expected_completions,
					INFINITE_WAIT_MILLISECONDS,
					Some(&raw mut submitted),
				)
			}
			.map_err(|source| native_failure("submit Windows IoRing requests", source))?;
			if submitted != expected_completions {
				return Err(FerrosGrynnError::storage_io_failure(format!(
					"Windows IoRing submitted {submitted} of {expected_completions} requests"
				)));
			}

			let mut seen = vec![false; requests.len()];
			let mut first_error = None;
			for _ in requests {
				let mut completion = IORING_CQE::default();
				// SAFETY: SubmitIoRing waited for every expected completion and
				// `completion` is a valid writable CQE.
				let popped = unsafe { PopIoRingCompletion(self.ring, &raw mut completion) };
				if popped != S_OK {
					return Err(FerrosGrynnError::storage_io_failure(
						"Windows IoRing completion queue became empty before all requests \
						 completed",
					));
				}
				let Some(index) = completion.UserData.checked_sub(FIRST_USER_DATA) else {
					return Err(FerrosGrynnError::storage_io_failure(
						"Windows IoRing returned reserved user data",
					));
				};
				let Some(request) = requests.get(index) else {
					return Err(FerrosGrynnError::storage_io_failure(
						"Windows IoRing returned unknown user data",
					));
				};
				if mem::replace(&mut seen[index], true) {
					return Err(FerrosGrynnError::storage_io_failure(
						"Windows IoRing returned a duplicate completion",
					));
				}
				if completion.ResultCode.is_err() && first_error.is_none() {
					first_error = Some(native_failure(
						"complete Windows IoRing request",
						windows::core::Error::from_hresult(completion.ResultCode),
					));
				} else if completion.Information != request.byte_len as usize
					&& first_error.is_none()
				{
					first_error = Some(FerrosGrynnError::storage_io_failure(format!(
						"short Windows IoRing transfer: {} of {} bytes",
						completion.Information, request.byte_len
					)));
				}
			}
			first_error.map_or(Ok(()), Err)
		}

		/// Recreates an invalidated ring before a later operation.
		fn ensure_ring(&mut self) -> FerrosGrynnResult<()> {
			if !self.ring.is_invalid() {
				return Ok(());
			}
			self.ring = create_ring(self.version, self.queue_depth, self.completion_depth)
				.map_err(|source| native_failure("recreate Windows IoRing", source))?;
			Ok(())
		}

		/// Discards a possibly contaminated SQ/CQ after a protocol failure.
		fn reset_ring(&mut self) {
			self.close_ring();
		}

		/// Closes the current ring once.
		fn close_ring(&mut self) {
			if self.ring.is_invalid() {
				return;
			}
			// SAFETY: this object owns the ring, all submitted operations were
			// drained or are being discarded while their buffers remain live, and
			// the handle is invalidated immediately after this call.
			let _ignored = unsafe { CloseIoRing(self.ring) };
			self.ring = HIORING::default();
		}
	}

	impl Drop for WindowsIoRing {
		fn drop(&mut self) {
			self.close_ring();
		}
	}

	/// Creates one raw ring with no advisory behavior.
	fn create_ring(
		version: IORING_VERSION,
		queue_depth: u32,
		completion_depth: u32,
	) -> windows::core::Result<HIORING> {
		let flags = IORING_CREATE_FLAGS {
			Required: IORING_CREATE_REQUIRED_FLAGS_NONE,
			Advisory: IORING_CREATE_ADVISORY_FLAGS_NONE,
		};
		// SAFETY: the requested capacities were bounded by
		// QueryIoRingCapabilities and the generated binding returns the owned
		// ring handle directly.
		unsafe { CreateIoRing(version, flags, queue_depth, completion_depth) }
	}

	/// Builds a raw file-handle reference without transferring ownership.
	const fn raw_handle_ref(handle: HANDLE) -> IORING_HANDLE_REF {
		IORING_HANDLE_REF {
			Kind: IORING_REF_RAW,
			Handle: IORING_HANDLE_REF_0 {
				Handle: handle,
			},
		}
	}

	/// Builds a raw buffer reference retained by the surrounding request.
	const fn raw_buffer_ref(address: *mut c_void) -> IORING_BUFFER_REF {
		IORING_BUFFER_REF {
			Kind: IORING_REF_RAW,
			Buffer: IORING_BUFFER_REF_0 {
				Address: address,
			},
		}
	}

	/// Converts a complete-page count to the IoRing `u32` byte length.
	fn checked_byte_len(page_count: usize) -> FerrosGrynnResult<u32> {
		let byte_len = page_count.checked_mul(PAGE_SIZE).ok_or_else(|| {
			FerrosGrynnError::storage_io_failure("Windows IoRing byte length overflow")
		})?;
		u32::try_from(byte_len).map_err(|source| {
			FerrosGrynnError::storage_io_failure(format!(
				"Windows IoRing byte length does not fit u32: {source}"
			))
		})
	}

	/// Maps runtime Windows initialization failure to the selected-mode status.
	fn unavailable_windows(
		operation: &'static str,
		source: windows::core::Error,
	) -> FerrosGrynnError {
		FerrosGrynnError::io_mode_unavailable(
			"NativeAsync",
			format!("{operation} failed: {source}"),
		)
		.with_technical_source(source)
	}

	/// Preserves a post-initialization Windows error behind the storage status.
	fn native_failure(operation: &'static str, source: windows::core::Error) -> FerrosGrynnError {
		FerrosGrynnError::storage_io_failure(operation).with_technical_source(source)
	}
}

#[cfg(all(windows, feature = "native-async-io"))]
use windows_ioring::WindowsIoRing;

#[cfg(test)]
mod tests {
	use std::path::{Path, PathBuf};

	use super::*;

	/// Unique workspace-local file removed after one native test.
	struct TestPath(PathBuf);

	impl TestPath {
		/// Creates a collision-resistant path without opening it.
		fn new() -> FerrosGrynnResult<Self> {
			let directory = std::env::current_dir().map_err(|source| {
				FerrosGrynnError::storage_io_failure("resolve page-I/O test directory")
					.with_technical_source(source)
			})?;
			Ok(Self(directory.join(format!(".ferros-grynn-page-io-{}.fgd", uuid::Uuid::new_v4()))))
		}

		/// Returns the exact test path.
		fn as_path(&self) -> &Path {
			&self.0
		}
	}

	impl Drop for TestPath {
		fn drop(&mut self) {
			let _ignored = std::fs::remove_file(&self.0);
		}
	}

	/// Overlap rejection protects the safe mutable-buffer boundary before any
	/// native call.
	#[test]
	fn overlapping_buffer_extents_are_rejected() -> FerrosGrynnResult<()> {
		let path = TestPath::new()?;
		let file = Arc::new(DirectFile::create_new(path.as_path())?);
		let extents = [
			FilePageExtent::new(Arc::clone(&file), PageNo::new(0), 0, 2),
			FilePageExtent::new(file, PageNo::new(2), 1, 1),
		];
		let result = validate_extent_layout(&extents, 2);
		assert!(result.is_err());
		Ok(())
	}

	/// One queue submission must preserve sparse extent placement for both
	/// reads and writes.
	#[cfg(all(windows, feature = "native-async-io"))]
	#[test]
	fn windows_ioring_reads_and_writes_sparse_extents() -> FerrosGrynnResult<()> {
		use crate::storage::page::PAGE_HEADER_SIZE;

		let path = TestPath::new()?;
		let page_count = 8_u64;
		{
			let writer = DirectFile::create_new(path.as_path())?;
			writer.resize_pages(page_count)?;
			let mut pages = Vec::with_capacity(page_count as usize);
			for raw_page_no in 0..page_count {
				pages.push(test_page(raw_page_no, raw_page_no as u8)?);
			}
			writer.write_pages(PageNo::new(0), &pages)?;
			writer.sync_data()?;
		}

		let file = Arc::new(DirectFile::open_read_write_with_mode(
			path.as_path(),
			DirectFileOpenMode::Overlapped,
		)?);
		let page_io = PageIo::new(PageIoMode::NativeAsync)?;
		let mut loaded = (0..4).map(|_| AlignedPage::zeroed()).collect::<Vec<_>>();
		let read_extents = [
			FilePageExtent::new(Arc::clone(&file), PageNo::new(0), 0, 1),
			FilePageExtent::new(Arc::clone(&file), PageNo::new(3), 1, 2),
			FilePageExtent::new(Arc::clone(&file), PageNo::new(7), 3, 1),
		];
		page_io.read_extents(&read_extents, &mut loaded)?;
		for (page, raw_page_no) in loaded.iter_mut().zip([0_u64, 3, 4, 7]) {
			page.validate(PageNo::new(raw_page_no))?;
			assert_eq!(page.as_bytes()[PAGE_HEADER_SIZE], raw_page_no as u8);
		}

		let replacements = [test_page(1, 101)?, test_page(5, 105)?, test_page(6, 106)?];
		let write_extents = [
			FilePageExtent::new(Arc::clone(&file), PageNo::new(1), 0, 1),
			FilePageExtent::new(Arc::clone(&file), PageNo::new(5), 1, 2),
		];
		page_io.write_extents(&write_extents, &replacements)?;
		file.sync_data()?;
		let mut verified = (0..3).map(|_| AlignedPage::zeroed()).collect::<Vec<_>>();
		page_io.read_extents(&write_extents, &mut verified)?;
		for ((page, raw_page_no), marker) in
			verified.iter_mut().zip([1_u64, 5, 6]).zip([101_u8, 105, 106])
		{
			page.validate(PageNo::new(raw_page_no))?;
			assert_eq!(page.as_bytes()[PAGE_HEADER_SIZE], marker);
		}
		Ok(())
	}

	/// Builds one sealed test page with an observable payload byte.
	#[cfg(all(windows, feature = "native-async-io"))]
	fn test_page(raw_page_no: u64, marker: u8) -> FerrosGrynnResult<AlignedPage> {
		use crate::storage::page::{PAGE_HEADER_SIZE, PageFlags, PageLsn, PageType};

		let page_no = PageNo::new(raw_page_no);
		let mut page = AlignedPage::new(
			PageType::StorageSuperblock,
			PageFlags::empty(),
			PageLsn::new(raw_page_no + 1),
			page_no,
		);
		page.as_bytes_mut()[PAGE_HEADER_SIZE] = marker;
		page.seal()?;
		Ok(page)
	}

	/// Native queued reads must win at useful queue depths without regressing
	/// the scalar Direct-I/O baseline.
	#[cfg(all(windows, feature = "native-async-io"))]
	#[test]
	#[ignore = "native NVMe calibration; run explicitly on the target Windows host"]
	fn windows_ioring_qd_gate() -> FerrosGrynnResult<()> {
		const FILE_PAGES: u64 = 8_192;
		const SAMPLE_PAGES: usize = 8_192;
		const WARMUP_PAGES: usize = 1_024;
		const QUEUE_DEPTHS: [usize; 4] = [1, 8, 32, 128];

		let path = TestPath::new()?;
		{
			let writer = DirectFile::create_new(path.as_path())?;
			writer.resize_pages(FILE_PAGES)?;
			let batch_pages = crate::storage::direct::SYNC_BATCH_PAGES;
			for first in (0..FILE_PAGES).step_by(batch_pages) {
				let count = usize::try_from((FILE_PAGES - first).min(batch_pages as u64)).map_err(
					|source| {
						FerrosGrynnError::storage_io_failure(format!(
							"benchmark page count does not fit usize: {source}"
						))
					},
				)?;
				let mut pages = Vec::with_capacity(count);
				for offset in 0..count {
					let raw_page_no = first + offset as u64;
					pages.push(test_page(raw_page_no, raw_page_no as u8)?);
				}
				writer.write_pages(PageNo::new(first), &pages)?;
			}
			writer.sync_data()?;
		}

		let file = Arc::new(DirectFile::open_read_write_with_mode(
			path.as_path(),
			DirectFileOpenMode::Overlapped,
		)?);
		let sync = PageIo::new(PageIoMode::Sync)?;
		let native = PageIo::new(PageIoMode::NativeAsync)?;
		profile_transport(&sync, &file, 1, FILE_PAGES, WARMUP_PAGES, 0x0d1a_0001)?;
		profile_transport(&native, &file, 1, FILE_PAGES, WARMUP_PAGES, 0x0d1a_0002)?;
		let mut high_qd_passed = false;
		let mut qd1_passed = false;
		for queue_depth in QUEUE_DEPTHS {
			let (sync_sample, native_sample) = paired_median_profile(
				&sync,
				&file,
				&native,
				&file,
				queue_depth,
				FILE_PAGES,
				SAMPLE_PAGES,
			)?;
			let throughput_ratio = native_sample.mib_per_second / sync_sample.mib_per_second;
			let p99_ratio = native_sample.p99_micros / sync_sample.p99_micros;
			println!(
				"IoRing QD{queue_depth}: sync={:.1} MiB/s p99={:.1} us, native={:.1} MiB/s \
				 p99={:.1} us, throughput={throughput_ratio:.3}x p99={p99_ratio:.3}x",
				sync_sample.mib_per_second,
				sync_sample.p99_micros,
				native_sample.mib_per_second,
				native_sample.p99_micros,
			);
			if queue_depth == 1 {
				qd1_passed = throughput_ratio >= 0.95 && p99_ratio <= 1.05;
			} else {
				high_qd_passed |= throughput_ratio >= 1.15 || p99_ratio <= 0.90;
			}
		}
		assert!(high_qd_passed, "IoRing did not pass the throughput or p99 gate at QD 8/32/128");
		assert!(qd1_passed, "IoRing QD1 regressed beyond the 5% gate");
		Ok(())
	}

	/// Alternates backend order and returns independent five-run medians for a
	/// fixed queue depth.
	#[cfg(all(windows, feature = "native-async-io"))]
	fn paired_median_profile(
		sync: &PageIo,
		sync_file: &Arc<DirectFile>,
		native: &PageIo,
		native_file: &Arc<DirectFile>,
		queue_depth: usize,
		file_pages: u64,
		sample_pages: usize,
	) -> FerrosGrynnResult<(IoProfile, IoProfile)> {
		let mut sync_samples = Vec::with_capacity(5);
		let mut native_samples = Vec::with_capacity(5);
		for repetition in 0_u64..5 {
			let seed = 0x0d1a_5eed_u64 ^ repetition;
			let profile = |page_io, file| {
				profile_transport(page_io, file, queue_depth, file_pages, sample_pages, seed)
			};
			if repetition.is_multiple_of(2) {
				sync_samples.push(profile(sync, sync_file)?);
				native_samples.push(profile(native, native_file)?);
			} else {
				native_samples.push(profile(native, native_file)?);
				sync_samples.push(profile(sync, sync_file)?);
			}
		}
		Ok((median_sample(&sync_samples), median_sample(&native_samples)))
	}

	/// Computes medians independently so one latency outlier cannot select the
	/// throughput sample.
	#[cfg(all(windows, feature = "native-async-io"))]
	fn median_sample(samples: &[IoProfile]) -> IoProfile {
		let mut throughputs =
			samples.iter().map(|sample| sample.mib_per_second).collect::<Vec<_>>();
		let mut p99 = samples.iter().map(|sample| sample.p99_micros).collect::<Vec<_>>();
		throughputs.sort_unstable_by(f64::total_cmp);
		p99.sort_unstable_by(f64::total_cmp);
		IoProfile {
			mib_per_second: throughputs[throughputs.len() / 2],
			p99_micros: p99[p99.len() / 2],
		}
	}

	/// Measures transport-only sparse reads; page validation is intentionally
	/// outside this stage-6 queue-depth comparison.
	#[cfg(all(windows, feature = "native-async-io"))]
	fn profile_transport(
		page_io: &PageIo,
		file: &Arc<DirectFile>,
		queue_depth: usize,
		file_pages: u64,
		sample_pages: usize,
		mut random_state: u64,
	) -> FerrosGrynnResult<IoProfile> {
		use std::{hint::black_box, time::Instant};

		let mut output = (0..queue_depth).map(|_| AlignedPage::zeroed()).collect::<Vec<_>>();
		let mut extents = Vec::with_capacity(queue_depth);
		let mut batch_latencies = Vec::with_capacity(sample_pages.div_ceil(queue_depth));
		let started = Instant::now();
		let mut completed_pages = 0;
		while completed_pages < sample_pages {
			let batch_pages = queue_depth.min(sample_pages - completed_pages);
			extents.clear();
			for buffer_start in 0..batch_pages {
				random_state = random_state
					.wrapping_mul(6_364_136_223_846_793_005)
					.wrapping_add(1_442_695_040_888_963_407);
				let page_no = PageNo::new(random_state % file_pages);
				extents.push(FilePageExtent::new(Arc::clone(file), page_no, buffer_start, 1));
			}
			let batch_started = Instant::now();
			page_io.read_extents(&extents, &mut output[..batch_pages])?;
			batch_latencies.push(batch_started.elapsed().as_secs_f64() * 1_000_000.0);
			black_box(output[batch_pages - 1].as_bytes()[0]);
			completed_pages += batch_pages;
		}
		let elapsed = started.elapsed().as_secs_f64();
		batch_latencies.sort_unstable_by(f64::total_cmp);
		let p99_index = batch_latencies.len().saturating_mul(99).div_ceil(100).saturating_sub(1);
		let bytes = sample_pages as f64 * crate::storage::page::PAGE_SIZE as f64;
		Ok(IoProfile {
			mib_per_second: bytes / (1024.0 * 1024.0) / elapsed,
			p99_micros: batch_latencies[p99_index],
		})
	}

	/// Transport metrics used by the ignored native calibration gate.
	#[cfg(all(windows, feature = "native-async-io"))]
	#[derive(Clone, Copy)]
	struct IoProfile {
		/// Aggregate physical transfer rate.
		mib_per_second: f64,
		/// Wall latency of one submission batch.
		p99_micros: f64,
	}
}
