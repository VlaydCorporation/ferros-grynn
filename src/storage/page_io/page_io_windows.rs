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
					"Windows IoRing completion queue became empty before all requests completed",
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
			} else if completion.Information != request.byte_len as usize && first_error.is_none() {
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
fn unavailable_windows(operation: &'static str, source: windows::core::Error) -> FerrosGrynnError {
	FerrosGrynnError::io_mode_unavailable("NativeAsync", format!("{operation} failed: {source}"))
		.with_technical_source(source)
}

/// Preserves a post-initialization Windows error behind the storage status.
fn native_failure(operation: &'static str, source: windows::core::Error) -> FerrosGrynnError {
	FerrosGrynnError::storage_io_failure(operation).with_technical_source(source)
}
