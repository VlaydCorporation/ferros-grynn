//! Synchronous page I/O that bypasses the operating-system page cache.

use std::{
	fs::{File, OpenOptions},
	io,
	path::Path,
	sync::atomic::{AtomicU64, Ordering as AtomicOrdering},
};

use super::page::{AlignedPage, PAGE_SIZE, PageHeader, PageNo};
use crate::gql_status::{FerrosGrynnError, FerrosGrynnResult};

/// Linux positional Direct-I/O shims.
#[cfg(target_os = "linux")]
mod direct_unix;
/// Windows positional Direct-I/O shims.
#[cfg(windows)]
mod direct_windows;

#[cfg(target_os = "linux")]
use direct_unix::{configure_direct_io, read_at, write_at};
#[cfg(windows)]
use direct_windows::{configure_direct_io, read_at, write_at};

/// Binary kibibyte used by named Direct-I/O extent sizes.
const KIBIBYTE: usize = 1024;

/// Windows extent selected by the native 16/64/256/1024/2048 KiB profile.
#[cfg(windows)]
pub(super) const SYNC_BATCH_BYTES: usize = 2048 * KIBIBYTE;

/// Provisional Linux extent; the Linux/NVMe native gate must calibrate it.
#[cfg(target_os = "linux")]
pub(super) const SYNC_BATCH_BYTES: usize = 256 * KIBIBYTE;

/// Compile-time placeholder for targets on which Direct I/O is rejected.
#[cfg(not(any(target_os = "linux", windows)))]
pub(super) const SYNC_BATCH_BYTES: usize = 256 * KIBIBYTE;

/// Number of complete pages in the calibrated synchronous extent.
pub(super) const SYNC_BATCH_PAGES: usize = SYNC_BATCH_BYTES / PAGE_SIZE;

/// Windows handle behavior selected by the page transport.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DirectFileOpenMode {
	/// Synchronous positional operations on a non-overlapped handle.
	Synchronous,
	/// Native queued operations on a `FILE_FLAG_OVERLAPPED` handle.
	Overlapped,
}

/// A data file opened with native Direct I/O flags.
///
/// The owned [`File`] closes automatically. Every operation uses an explicit
/// byte offset; no shared file cursor participates in page I/O.
pub struct DirectFile {
	/// Native RAII file handle.
	file: File,
	/// Companion handle used only for queued Windows submissions; the primary
	/// handle keeps scalar positional I/O synchronous.
	#[cfg(windows)]
	overlapped_file: Option<File>,
	/// Cached file length in pages.
	///
	/// Relaxed ordering is sufficient: this value is only a numeric bound and
	/// does not publish page contents between threads.
	page_count: AtomicU64,
}

impl DirectFile {
	/// Creates a new read-write data file without replacing an existing file.
	///
	/// # Errors
	///
	/// Returns a structured I/O error if creation fails, or Direct I/O is not
	/// available on the target platform or filesystem.
	pub fn create_new(path: impl AsRef<Path>) -> FerrosGrynnResult<Self> {
		Self::open(path.as_ref(), OpenMode::CreateNew, DirectFileOpenMode::Synchronous)
	}

	/// Opens an existing data file for positional reads.
	///
	/// # Errors
	///
	/// Returns an error if opening fails, Direct I/O is unavailable, or the
	/// existing file length is not a multiple of [`PAGE_SIZE`].
	pub fn open_read_only(path: impl AsRef<Path>) -> FerrosGrynnResult<Self> {
		Self::open(path.as_ref(), OpenMode::ReadOnly, DirectFileOpenMode::Synchronous)
	}

	/// Opens an existing data file for positional reads and writes.
	///
	/// # Errors
	///
	/// Returns an error if opening fails, Direct I/O is unavailable, or the
	/// existing file length is not a multiple of [`PAGE_SIZE`].
	pub(super) fn open_read_write(path: impl AsRef<Path>) -> FerrosGrynnResult<Self> {
		Self::open(path.as_ref(), OpenMode::ReadWrite, DirectFileOpenMode::Synchronous)
	}

	/// Opens an existing read-write file with the requested native handle
	/// behavior.
	///
	/// # Errors
	///
	/// Returns an error if opening fails, Direct I/O is unavailable, or the
	/// existing file length is not a multiple of [`PAGE_SIZE`].
	pub fn open_read_write_with_mode(
		path: impl AsRef<Path>,
		io_mode: DirectFileOpenMode,
	) -> FerrosGrynnResult<Self> {
		Self::open(path.as_ref(), OpenMode::ReadWrite, io_mode)
	}

	/// Returns the number of complete pages known to this handle.
	pub(super) fn page_count(&self) -> u64 {
		self.page_count.load(AtomicOrdering::Relaxed)
	}

	/// Resizes the file to an exact number of pages.
	///
	/// Newly allocated pages contain unspecified filesystem bytes until the
	/// caller writes sealed pages to them.
	///
	/// # Errors
	///
	/// Returns an error if the requested length overflows the supported file
	/// offset range or the operating system rejects the resize.
	pub fn resize_pages(&self, page_count: u64) -> FerrosGrynnResult<()> {
		let length = checked_file_length(page_count)?;
		self.file.set_len(length).map_err(|source| io_failure("resize data file", source))?;
		self.page_count.store(page_count, AtomicOrdering::Relaxed);
		Ok(())
	}

	/// Reads and validates one complete page at its physical page number.
	///
	/// # Errors
	///
	/// Returns an error for an out-of-range page, short I/O, operating-system
	/// failure, malformed header, unsupported format, or checksum mismatch.
	pub(super) fn read_page(
		&self,
		page_no: PageNo,
		page: &mut AlignedPage,
	) -> FerrosGrynnResult<PageHeader> {
		self.read_page_raw(page_no, page)?;
		page.validate(page_no)
	}

	/// Reads one complete page without decoding its header or checksum.
	///
	/// This transport-only path exists for controlled benchmark baselines and
	/// for callers that validate the page immediately at a higher boundary.
	///
	/// # Errors
	///
	/// Returns an error for an out-of-range page, short I/O, offset overflow,
	/// or an operating-system failure.
	pub fn read_page_raw(&self, page_no: PageNo, page: &mut AlignedPage) -> FerrosGrynnResult<()> {
		let offset = checked_page_extent(page_no, 1, self.page_count())?;
		read_exact_at(&self.file, page.as_bytes_mut(), offset)
	}

	/// Reads and validates one physically contiguous page extent.
	///
	/// The transport performs one successful positional read for the whole
	/// extent. Interrupted calls may be retried before any bytes are reported.
	///
	/// # Errors
	///
	/// Returns an error for a range outside the file, arithmetic overflow,
	/// short I/O, native I/O failure, or any invalid page in the extent.
	pub(super) fn read_pages(
		&self,
		first_page: PageNo,
		pages: &mut [AlignedPage],
	) -> FerrosGrynnResult<Vec<PageHeader>> {
		self.read_pages_raw(first_page, pages)?;
		let mut headers = Vec::with_capacity(pages.len());
		for (index, page) in pages.iter_mut().enumerate() {
			let index = u64::try_from(index).map_err(|source| {
				FerrosGrynnError::invalid_storage_page(
					first_page.get(),
					format!("batch page index does not fit u64: {source}"),
				)
			})?;
			let page_no =
				first_page.get().checked_add(index).map(PageNo::new).ok_or_else(|| {
					FerrosGrynnError::invalid_storage_page(
						first_page.get(),
						"batch page number overflow",
					)
				})?;
			headers.push(page.validate(page_no)?);
		}
		Ok(headers)
	}

	/// Reads one physically contiguous extent without validating page bytes.
	///
	/// An empty extent is a no-op and does not issue a system call.
	///
	/// # Errors
	///
	/// Returns an error for a range outside the file, arithmetic overflow,
	/// short I/O, or an operating-system failure.
	pub fn read_pages_raw(
		&self,
		first_page: PageNo,
		pages: &mut [AlignedPage],
	) -> FerrosGrynnResult<()> {
		if pages.is_empty() {
			return Ok(());
		}
		let offset = self.prepare_read_extent(first_page, pages.len())?;
		read_extent_at(&self.file, AlignedPage::slice_as_bytes_mut(pages), offset)
	}

	/// Writes one already sealed page at its physical page number.
	///
	/// This method does not make the write durable; callers define durability
	/// with an explicit [`Self::sync_data`] barrier.
	///
	/// # Errors
	///
	/// Returns an error for an invalid page, an offset gap, short I/O, or an
	/// operating-system failure.
	pub(super) fn write_page(&self, page_no: PageNo, page: &AlignedPage) -> FerrosGrynnResult<()> {
		let offset = checked_page_offset(page_no)?;
		page.validate(page_no)?;
		let page_count = self.page_count();
		if page_no.get() > page_count {
			return Err(FerrosGrynnError::invalid_storage_page(
				page_no.get(),
				"writing the page would leave an uninitialized file gap",
			));
		}
		write_all_at(&self.file, page.as_bytes(), offset)?;
		if page_no.get() == page_count {
			self.page_count.fetch_max(page_count + 1, AtomicOrdering::Relaxed);
		}
		Ok(())
	}

	/// Writes unchecked aligned bytes for private scratch files and
	/// deterministic torn-page tests.
	///
	/// # Errors
	///
	/// Returns an error for an offset gap, short I/O, or native failure.
	pub(super) fn write_page_raw(
		&self,
		page_no: PageNo,
		page: &AlignedPage,
	) -> FerrosGrynnResult<()> {
		let offset = checked_page_offset(page_no)?;
		let page_count = self.page_count();
		if page_no.get() > page_count {
			return Err(FerrosGrynnError::invalid_storage_page(
				page_no.get(),
				"writing the raw test page would leave a file gap",
			));
		}
		write_all_at(&self.file, page.as_bytes(), offset)?;
		if page_no.get() == page_count {
			self.page_count.fetch_max(page_count + 1, AtomicOrdering::Relaxed);
		}
		Ok(())
	}

	/// Writes one physically contiguous extent with one successful positional
	/// operation.
	///
	/// Every page must already be sealed for its physical position. An empty
	/// extent is a no-op. Durability still requires an explicit
	/// [`Self::sync_data`] barrier.
	///
	/// # Errors
	///
	/// Returns an error for an invalid page, an offset gap, arithmetic
	/// overflow, short I/O, or an operating-system failure.
	pub fn write_pages(&self, first_page: PageNo, pages: &[AlignedPage]) -> FerrosGrynnResult<()> {
		if pages.is_empty() {
			return Ok(());
		}
		let (offset, end_page) = self.prepare_write_extent(first_page, pages)?;
		write_extent_at(&self.file, AlignedPage::slice_as_bytes(pages), offset)?;
		self.complete_write_extent(end_page);
		Ok(())
	}

	/// Validates one raw read extent and returns its byte offset.
	///
	/// # Errors
	///
	/// Returns an error for an empty or out-of-range extent, conversion
	/// failure, or arithmetic overflow.
	pub fn prepare_read_extent(
		&self,
		first_page: PageNo,
		page_count: usize,
	) -> FerrosGrynnResult<u64> {
		if page_count == 0 {
			return Err(FerrosGrynnError::invalid_storage_page(
				first_page.get(),
				"Direct-I/O read extent must not be empty",
			));
		}
		let page_count = u64::try_from(page_count).map_err(|source| {
			FerrosGrynnError::invalid_storage_page(
				first_page.get(),
				format!("batch page count does not fit u64: {source}"),
			)
		})?;
		checked_page_extent(first_page, page_count, self.page_count())
	}

	/// Validates pages for one queued write and returns its byte offset and
	/// exclusive ending page number.
	///
	/// # Errors
	///
	/// Returns an error for an empty or invalid extent, a file gap, conversion
	/// failure, or arithmetic overflow.
	pub(super) fn prepare_write_extent(
		&self,
		first_page: PageNo,
		pages: &[AlignedPage],
	) -> FerrosGrynnResult<(u64, u64)> {
		if pages.is_empty() {
			return Err(FerrosGrynnError::invalid_storage_page(
				first_page.get(),
				"Direct-I/O write extent must not be empty",
			));
		}
		let page_count = u64::try_from(pages.len()).map_err(|source| {
			FerrosGrynnError::invalid_storage_page(
				first_page.get(),
				format!("batch page count does not fit u64: {source}"),
			)
		})?;
		let end_page = first_page.get().checked_add(page_count).ok_or_else(|| {
			FerrosGrynnError::invalid_storage_page(first_page.get(), "page extent overflow")
		})?;
		checked_file_length(end_page)?;
		if first_page.get() > self.page_count() {
			return Err(FerrosGrynnError::invalid_storage_page(
				first_page.get(),
				"writing the extent would leave an uninitialized file gap",
			));
		}
		for (index, page) in pages.iter().enumerate() {
			let index = u64::try_from(index).map_err(|source| {
				FerrosGrynnError::invalid_storage_page(
					first_page.get(),
					format!("batch page index does not fit u64: {source}"),
				)
			})?;
			page.validate(PageNo::new(first_page.get() + index))?;
		}
		Ok((checked_page_offset(first_page)?, end_page))
	}

	/// Publishes a successfully written extent to the cached length bound.
	pub(super) fn complete_write_extent(&self, end_page: u64) {
		self.page_count.fetch_max(end_page, AtomicOrdering::Relaxed);
	}

	/// Borrows the native Windows handle for an overlapped queued read.
	///
	/// # Errors
	///
	/// Returns an error when this file was not opened with its overlapped
	/// companion handle.
	#[cfg(windows)]
	pub fn raw_handle(&self) -> FerrosGrynnResult<windows::Win32::Foundation::HANDLE> {
		use std::os::windows::io::AsRawHandle;

		self.overlapped_file
			.as_ref()
			.map(|file| windows::Win32::Foundation::HANDLE(file.as_raw_handle()))
			.ok_or_else(|| {
				FerrosGrynnError::storage_io_failure(
					"queued Windows read requested a file without an overlapped companion handle",
				)
			})
	}

	/// Makes preceding data-file writes durable at an explicit protocol
	/// barrier.
	///
	/// # Errors
	///
	/// Returns an error if the operating system cannot synchronize file data.
	pub fn sync_data(&self) -> FerrosGrynnResult<()> {
		self.durability_handle()
			.sync_data()
			.map_err(|source| io_failure("synchronize data file", source))
	}

	/// Makes preceding data and metadata changes durable.
	///
	/// # Errors
	///
	/// Returns an error if the operating system cannot synchronize the file.
	pub(super) fn sync_all(&self) -> FerrosGrynnResult<()> {
		self.durability_handle()
			.sync_all()
			.map_err(|source| io_failure("synchronize complete data file", source))
	}

	/// Selects the handle that receives queued writes in native mode.
	fn durability_handle(&self) -> &File {
		#[cfg(windows)]
		if let Some(file) = self.overlapped_file.as_ref() {
			return file;
		}
		&self.file
	}

	/// Opens and validates a Direct I/O file in one of the supported modes.
	fn open(path: &Path, mode: OpenMode, io_mode: DirectFileOpenMode) -> FerrosGrynnResult<Self> {
		#[cfg(windows)]
		let file = open_native_file(path, mode, DirectFileOpenMode::Synchronous)?;
		#[cfg(not(windows))]
		let file = open_native_file(path, mode, io_mode)?;
		#[cfg(windows)]
		let overlapped_file = match io_mode {
			DirectFileOpenMode::Synchronous => None,
			DirectFileOpenMode::Overlapped => {
				Some(open_native_file(path, OpenMode::ReadWrite, io_mode)?)
			}
		};
		let length =
			file.metadata().map_err(|source| io_failure("read data-file metadata", source))?.len();
		validate_file_length(length)?;
		Ok(Self {
			file,
			#[cfg(windows)]
			overlapped_file,
			page_count: AtomicU64::new(length / PAGE_SIZE as u64),
		})
	}
}

/// Opens one native handle with explicit access and cache flags.
fn open_native_file(
	path: &Path,
	mode: OpenMode,
	io_mode: DirectFileOpenMode,
) -> FerrosGrynnResult<File> {
	let mut options = OpenOptions::new();
	mode.configure(&mut options);
	configure_direct_io(&mut options, io_mode)?;
	options.open(path).map_err(map_open_error)
}

/// Supported file-open contracts without ambiguous boolean combinations.
#[derive(Debug, Clone, Copy)]
enum OpenMode {
	/// Create a new read-write file and fail if it already exists.
	CreateNew,
	/// Open an existing file for reads only.
	ReadOnly,
	/// Open an existing file for reads and writes.
	ReadWrite,
}

impl OpenMode {
	/// Applies access and creation semantics without platform flags.
	fn configure(self, options: &mut OpenOptions) {
		match self {
			Self::CreateNew => {
				options.read(true).write(true).create_new(true);
			}
			Self::ReadOnly => {
				options.read(true);
			}
			Self::ReadWrite => {
				options.read(true).write(true);
			}
		}
	}
}

/// Rejects platforms for which format-v1 Direct I/O is not specified.
#[cfg(not(any(target_os = "linux", windows)))]
fn configure_direct_io(
	_options: &mut OpenOptions,
	_io_mode: DirectFileOpenMode,
) -> FerrosGrynnResult<()> {
	Err(FerrosGrynnError::direct_io_unavailable(format!(
		"target {} is not supported; format-v1 data files require Linux or Windows",
		std::env::consts::OS
	)))
}

/// Returns an unsupported error on unspecified targets.
#[cfg(not(any(target_os = "linux", windows)))]
fn read_at(_file: &File, _buffer: &mut [u8], _offset: u64) -> io::Result<usize> {
	Err(io::Error::new(io::ErrorKind::Unsupported, "Direct I/O is unavailable"))
}

/// Returns an unsupported error on unspecified targets.
#[cfg(not(any(target_os = "linux", windows)))]
fn write_at(_file: &File, _buffer: &[u8], _offset: u64) -> io::Result<usize> {
	Err(io::Error::new(io::ErrorKind::Unsupported, "Direct I/O is unavailable"))
}

/// Reads exactly one page using repeated positional operations.
fn read_exact_at(file: &File, mut buffer: &mut [u8], mut offset: u64) -> FerrosGrynnResult<()> {
	while !buffer.is_empty() {
		match read_at(file, buffer, offset) {
			Ok(0) => {
				return Err(io_failure(
					"read data page",
					io::Error::new(io::ErrorKind::UnexpectedEof, "short positional read"),
				));
			}
			Ok(read) => {
				let (_, rest) = buffer.split_at_mut(read);
				buffer = rest;
				offset = offset.checked_add(read as u64).ok_or_else(|| {
					FerrosGrynnError::invalid_storage_file("page read offset overflow")
				})?;
			}
			Err(source) if source.kind() == io::ErrorKind::Interrupted => {}
			Err(source) => return Err(map_direct_io_error("read data page", source)),
		}
	}
	Ok(())
}

/// Reads one complete extent with a single successful positional operation.
fn read_extent_at(file: &File, buffer: &mut [u8], offset: u64) -> FerrosGrynnResult<()> {
	loop {
		match read_at(file, buffer, offset) {
			Ok(read) if read == buffer.len() => return Ok(()),
			Ok(read) => {
				return Err(io_failure(
					"read data-page extent",
					io::Error::new(
						io::ErrorKind::UnexpectedEof,
						format!("short positional extent read: {read} of {} bytes", buffer.len()),
					),
				));
			}
			Err(source) if source.kind() == io::ErrorKind::Interrupted => {}
			Err(source) => return Err(map_direct_io_error("read data-page extent", source)),
		}
	}
}

/// Writes exactly one page using repeated positional operations.
fn write_all_at(file: &File, mut buffer: &[u8], mut offset: u64) -> FerrosGrynnResult<()> {
	while !buffer.is_empty() {
		match write_at(file, buffer, offset) {
			Ok(0) => {
				return Err(io_failure(
					"write data page",
					io::Error::new(io::ErrorKind::WriteZero, "short positional write"),
				));
			}
			Ok(written) => {
				buffer = &buffer[written..];
				offset = offset.checked_add(written as u64).ok_or_else(|| {
					FerrosGrynnError::invalid_storage_file("page write offset overflow")
				})?;
			}
			Err(source) if source.kind() == io::ErrorKind::Interrupted => {}
			Err(source) => return Err(map_direct_io_error("write data page", source)),
		}
	}
	Ok(())
}

/// Writes one complete extent with a single successful positional operation.
fn write_extent_at(file: &File, buffer: &[u8], offset: u64) -> FerrosGrynnResult<()> {
	loop {
		match write_at(file, buffer, offset) {
			Ok(written) if written == buffer.len() => return Ok(()),
			Ok(written) => {
				return Err(io_failure(
					"write data-page extent",
					io::Error::new(
						io::ErrorKind::WriteZero,
						format!(
							"short positional extent write: {written} of {} bytes",
							buffer.len()
						),
					),
				));
			}
			Err(source) if source.kind() == io::ErrorKind::Interrupted => {}
			Err(source) => return Err(map_direct_io_error("write data-page extent", source)),
		}
	}
}

/// Converts a page number to a byte offset without overflow.
fn checked_page_offset(page_no: PageNo) -> FerrosGrynnResult<u64> {
	let offset = page_no.get().checked_mul(PAGE_SIZE as u64).ok_or_else(|| {
		FerrosGrynnError::invalid_storage_page(page_no.get(), "page offset overflow")
	})?;
	let end = offset.checked_add(PAGE_SIZE as u64).ok_or_else(|| {
		FerrosGrynnError::invalid_storage_page(page_no.get(), "page end offset overflow")
	})?;
	if end > i64::MAX as u64 {
		return Err(FerrosGrynnError::invalid_storage_page(
			page_no.get(),
			"page offset exceeds the supported signed file range",
		));
	}
	Ok(offset)
}

/// Validates a contiguous page range and returns its byte offset.
fn checked_page_extent(
	first_page: PageNo,
	page_count: u64,
	file_page_count: u64,
) -> FerrosGrynnResult<u64> {
	let end_page = first_page.get().checked_add(page_count).ok_or_else(|| {
		FerrosGrynnError::invalid_storage_page(first_page.get(), "page extent overflow")
	})?;
	if end_page > file_page_count {
		return Err(FerrosGrynnError::invalid_storage_page(
			first_page.get(),
			"page extent is outside the data file",
		));
	}
	checked_file_length(end_page)?;
	checked_page_offset(first_page)
}

/// Converts a page count to a byte length without overflow.
fn checked_file_length(page_count: u64) -> FerrosGrynnResult<u64> {
	let length = page_count
		.checked_mul(PAGE_SIZE as u64)
		.ok_or_else(|| FerrosGrynnError::invalid_storage_file("data-file length overflow"))?;
	if length > i64::MAX as u64 {
		return Err(FerrosGrynnError::invalid_storage_file(
			"data-file length exceeds the supported signed file range",
		));
	}
	Ok(length)
}

/// Ensures an existing data file consists only of complete pages.
fn validate_file_length(length: u64) -> FerrosGrynnResult<()> {
	if !length.is_multiple_of(PAGE_SIZE as u64) {
		return Err(FerrosGrynnError::invalid_storage_file(format!(
			"length {length} is not a multiple of {PAGE_SIZE}"
		)));
	}
	Ok(())
}

/// Maps a native open failure to the Direct I/O or generic I/O status.
fn map_open_error(source: io::Error) -> FerrosGrynnError {
	map_direct_io_error("open data file with Direct I/O", source)
}

/// Maps native errors that signal an unsupported Direct I/O contract.
fn map_direct_io_error(operation: &'static str, source: io::Error) -> FerrosGrynnError {
	if direct_io_is_unavailable(&source) {
		return FerrosGrynnError::direct_io_unavailable(
			"the platform or filesystem rejected the required Direct I/O contract",
		)
		.with_technical_source(source);
	}
	io_failure(operation, source)
}

/// Returns whether an operating-system error denotes unsupported Direct I/O.
fn direct_io_is_unavailable(source: &io::Error) -> bool {
	if source.kind() == io::ErrorKind::Unsupported {
		return true;
	}
	direct_io_raw_error_is_unavailable(source.raw_os_error())
}

/// Classifies Linux errors for unsupported Direct I/O.
#[cfg(target_os = "linux")]
const fn direct_io_raw_error_is_unavailable(raw_error: Option<i32>) -> bool {
	matches!(raw_error, Some(nix::libc::EINVAL | nix::libc::EOPNOTSUPP))
}

/// Classifies Windows errors for unsupported Direct I/O.
#[cfg(windows)]
const fn direct_io_raw_error_is_unavailable(raw_error: Option<i32>) -> bool {
	use windows::Win32::Foundation::{
		ERROR_INVALID_FUNCTION,
		ERROR_INVALID_PARAMETER,
		ERROR_NOT_SUPPORTED,
	};

	matches!(raw_error, Some(error) if error == ERROR_INVALID_FUNCTION.0 as i32
		|| error == ERROR_INVALID_PARAMETER.0 as i32
		|| error == ERROR_NOT_SUPPORTED.0 as i32)
}

/// Other targets are rejected before a native file operation is attempted.
#[cfg(not(any(target_os = "linux", windows)))]
const fn direct_io_raw_error_is_unavailable(_raw_error: Option<i32>) -> bool {
	false
}

/// Builds the project-wide structured I/O status while retaining the native
/// cause.
fn io_failure(operation: impl Into<String>, source: io::Error) -> FerrosGrynnError {
	FerrosGrynnError::storage_io_failure(operation).with_technical_source(source)
}

#[cfg(test)]
#[allow(
	clippy::panic_in_result_fn,
	reason = "fallible test setup returns Result while assertions report contract failures"
)]
mod tests;
