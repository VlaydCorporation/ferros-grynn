//! Synchronous page I/O that bypasses the operating-system page cache.

use std::{
	fs::{File, OpenOptions},
	io,
	path::Path,
	sync::atomic::{AtomicU64, Ordering as AtomicOrdering},
};

use super::page::{AlignedPage, PAGE_SIZE, PageHeader, PageNo};
use crate::gql_status::{FerrosGrynnError, FerrosGrynnResult};

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

/// A data file opened with the native synchronous Direct I/O flag.
///
/// The owned [`File`] closes automatically. Every operation uses an explicit
/// byte offset; no shared file cursor participates in page I/O.
pub(super) struct DirectFile {
	/// Native RAII file handle.
	file: File,
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
	pub(super) fn create_new(path: impl AsRef<Path>) -> FerrosGrynnResult<Self> {
		Self::open(path.as_ref(), OpenMode::CreateNew)
	}

	/// Opens an existing data file for positional reads.
	///
	/// # Errors
	///
	/// Returns an error if opening fails, Direct I/O is unavailable, or the
	/// existing file length is not a multiple of [`PAGE_SIZE`].
	pub(super) fn open_read_only(path: impl AsRef<Path>) -> FerrosGrynnResult<Self> {
		Self::open(path.as_ref(), OpenMode::ReadOnly)
	}

	/// Opens an existing data file for positional reads and writes.
	///
	/// # Errors
	///
	/// Returns an error if opening fails, Direct I/O is unavailable, or the
	/// existing file length is not a multiple of [`PAGE_SIZE`].
	pub(super) fn open_read_write(path: impl AsRef<Path>) -> FerrosGrynnResult<Self> {
		Self::open(path.as_ref(), OpenMode::ReadWrite)
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
	pub(super) fn resize_pages(&self, page_count: u64) -> FerrosGrynnResult<()> {
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
		page.validate_mut(page_no)
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
	pub(super) fn read_page_raw(
		&self,
		page_no: PageNo,
		page: &mut AlignedPage,
	) -> FerrosGrynnResult<()> {
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
			headers.push(page.validate_mut(page_no)?);
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
	pub(super) fn read_pages_raw(
		&self,
		first_page: PageNo,
		pages: &mut [AlignedPage],
	) -> FerrosGrynnResult<()> {
		if pages.is_empty() {
			return Ok(());
		}
		let page_count = u64::try_from(pages.len()).map_err(|source| {
			FerrosGrynnError::invalid_storage_page(
				first_page.get(),
				format!("batch page count does not fit u64: {source}"),
			)
		})?;
		let offset = checked_page_extent(first_page, page_count, self.page_count())?;
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
	pub(super) fn write_pages(
		&self,
		first_page: PageNo,
		pages: &[AlignedPage],
	) -> FerrosGrynnResult<()> {
		if pages.is_empty() {
			return Ok(());
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
			let page_no = PageNo::new(first_page.get() + index);
			page.validate(page_no)?;
		}
		let offset = checked_page_offset(first_page)?;
		write_extent_at(&self.file, AlignedPage::slice_as_bytes(pages), offset)?;
		self.page_count.fetch_max(end_page, AtomicOrdering::Relaxed);
		Ok(())
	}

	/// Makes preceding data-file writes durable at an explicit protocol
	/// barrier.
	///
	/// # Errors
	///
	/// Returns an error if the operating system cannot synchronize file data.
	pub(super) fn sync_data(&self) -> FerrosGrynnResult<()> {
		self.file.sync_data().map_err(|source| io_failure("synchronize data file", source))
	}

	/// Opens and validates a Direct I/O file in one of the supported modes.
	fn open(path: &Path, mode: OpenMode) -> FerrosGrynnResult<Self> {
		let mut options = OpenOptions::new();
		mode.configure(&mut options);
		configure_direct_io(&mut options)?;
		let file = options.open(path).map_err(map_open_error)?;
		let length =
			file.metadata().map_err(|source| io_failure("read data-file metadata", source))?.len();
		validate_file_length(length)?;
		Ok(Self {
			file,
			page_count: AtomicU64::new(length / PAGE_SIZE as u64),
		})
	}
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

/// Adds the Linux `O_DIRECT` flag without per-write synchronization.
#[cfg(target_os = "linux")]
#[allow(
	clippy::unnecessary_wraps,
	reason = "all platform shims share the fallible signature used by unsupported targets"
)]
fn configure_direct_io(options: &mut OpenOptions) -> FerrosGrynnResult<()> {
	use std::os::unix::fs::OpenOptionsExt;

	options.custom_flags(nix::libc::O_DIRECT | nix::libc::O_CLOEXEC);
	Ok(())
}

/// Adds `FILE_FLAG_NO_BUFFERING` without `FILE_FLAG_WRITE_THROUGH`.
#[cfg(windows)]
#[allow(
	clippy::unnecessary_wraps,
	reason = "all platform shims share the fallible signature used by unsupported targets"
)]
fn configure_direct_io(options: &mut OpenOptions) -> FerrosGrynnResult<()> {
	use std::os::windows::fs::OpenOptionsExt;

	use windows::Win32::Storage::FileSystem::FILE_FLAG_NO_BUFFERING;

	options.custom_flags(FILE_FLAG_NO_BUFFERING.0);
	Ok(())
}

/// Rejects platforms for which format-v1 Direct I/O is not specified.
#[cfg(not(any(target_os = "linux", windows)))]
fn configure_direct_io(_options: &mut OpenOptions) -> FerrosGrynnResult<()> {
	Err(FerrosGrynnError::direct_io_unavailable(format!(
		"target {} is not supported; format-v1 data files require Linux or Windows",
		std::env::consts::OS
	)))
}

/// Performs one positional read on Linux.
#[cfg(target_os = "linux")]
fn read_at(file: &File, buffer: &mut [u8], offset: u64) -> io::Result<usize> {
	use std::os::unix::fs::FileExt;

	file.read_at(buffer, offset)
}

/// Performs one positional read on Windows.
#[cfg(windows)]
fn read_at(file: &File, buffer: &mut [u8], offset: u64) -> io::Result<usize> {
	use std::os::windows::fs::FileExt;

	file.seek_read(buffer, offset)
}

/// Returns an unsupported error on unspecified targets.
#[cfg(not(any(target_os = "linux", windows)))]
fn read_at(_file: &File, _buffer: &mut [u8], _offset: u64) -> io::Result<usize> {
	Err(io::Error::new(io::ErrorKind::Unsupported, "Direct I/O is unavailable"))
}

/// Performs one positional write on Linux.
#[cfg(target_os = "linux")]
fn write_at(file: &File, buffer: &[u8], offset: u64) -> io::Result<usize> {
	use std::os::unix::fs::FileExt;

	file.write_at(buffer, offset)
}

/// Performs one positional write on Windows.
#[cfg(windows)]
fn write_at(file: &File, buffer: &[u8], offset: u64) -> io::Result<usize> {
	use std::os::windows::fs::FileExt;

	file.seek_write(buffer, offset)
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
mod tests {
	use std::{
		error::Error,
		path::PathBuf,
		sync::atomic::{AtomicU64, Ordering},
	};
	#[cfg(any(target_os = "linux", windows))]
	use std::{sync::Arc, thread};

	use super::*;
	#[cfg(any(target_os = "linux", windows))]
	use crate::storage::page::{PAGE_HEADER_SIZE, PageFlags, PageLsn, PageType};

	/// Monotonic suffix for collision-resistant test paths within one process.
	static NEXT_TEST_FILE: AtomicU64 = AtomicU64::new(0);

	/// Temporary test path removed even when a test assertion unwinds.
	struct TestPath(PathBuf);

	impl TestPath {
		/// Allocates a unique path in the current workspace directory.
		fn new() -> io::Result<Self> {
			let suffix = NEXT_TEST_FILE.fetch_add(1, Ordering::Relaxed);
			let name = format!(".ferros-grynn-direct-{}-{suffix}.fgb", std::process::id());
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
	fn direct_file_round_trips_concurrent_positional_pages() -> Result<(), Box<dyn Error>> {
		let path = TestPath::new()?;
		let file = Arc::new(DirectFile::create_new(path.as_path())?);
		file.resize_pages(4)?;

		let write_handles = (0_u64..4)
			.map(|raw_page_no| {
				let file = Arc::clone(&file);
				thread::spawn(move || -> FerrosGrynnResult<()> {
					let page_no = PageNo::new(raw_page_no);
					let mut page = AlignedPage::new(
						PageType::StorageSuperblock,
						PageFlags::empty(),
						PageLsn::new(raw_page_no),
						page_no,
					);
					page.as_bytes_mut()[PAGE_HEADER_SIZE] = raw_page_no as u8;
					page.seal()?;
					file.write_page(page_no, &page)
				})
			})
			.collect::<Vec<_>>();
		for handle in write_handles {
			handle
				.join()
				.map_err(|_panic_payload| io::Error::other("direct writer thread panicked"))??;
		}
		file.sync_data()?;
		drop(file);
		let file = Arc::new(DirectFile::open_read_write(path.as_path())?);

		let read_handles = (0_u64..4)
			.map(|raw_page_no| {
				let file = Arc::clone(&file);
				thread::spawn(move || -> FerrosGrynnResult<u8> {
					let page_no = PageNo::new(raw_page_no);
					let mut page = AlignedPage::zeroed();
					file.read_page(page_no, &mut page)?;
					Ok(page.as_bytes()[PAGE_HEADER_SIZE])
				})
			})
			.collect::<Vec<_>>();
		for (page_no, handle) in read_handles.into_iter().enumerate() {
			let marker = handle
				.join()
				.map_err(|_panic_payload| io::Error::other("direct reader thread panicked"))??;
			assert_eq!(marker, page_no as u8);
		}
		Ok(())
	}

	#[cfg(any(target_os = "linux", windows))]
	#[test]
	fn direct_file_tracks_page_count_after_append() -> Result<(), Box<dyn Error>> {
		let path = TestPath::new()?;
		let file = DirectFile::create_new(path.as_path())?;
		let page_no = PageNo::new(0);
		let page = AlignedPage::new(
			PageType::StorageSuperblock,
			PageFlags::empty(),
			PageLsn::new(1),
			page_no,
		);

		file.write_page(page_no, &page)?;
		assert_eq!(file.page_count(), 1);
		let mut loaded = AlignedPage::zeroed();
		file.read_page(page_no, &mut loaded)?;
		Ok(())
	}

	#[cfg(any(target_os = "linux", windows))]
	#[test]
	fn direct_file_reads_and_validates_one_contiguous_extent() -> Result<(), Box<dyn Error>> {
		let path = TestPath::new()?;
		let file = DirectFile::create_new(path.as_path())?;
		file.resize_pages(4)?;
		for raw_page_no in 0_u64..4 {
			let page_no = PageNo::new(raw_page_no);
			let mut page = AlignedPage::new(
				PageType::StorageSuperblock,
				PageFlags::empty(),
				PageLsn::new(raw_page_no + 1),
				page_no,
			);
			page.as_bytes_mut()[PAGE_HEADER_SIZE] = raw_page_no as u8;
			page.seal()?;
			file.write_page(page_no, &page)?;
		}
		file.sync_data()?;

		let mut pages = (0..2).map(|_| AlignedPage::zeroed()).collect::<Vec<_>>();
		let headers = file.read_pages(PageNo::new(1), &mut pages)?;
		assert_eq!(headers.iter().map(|header| header.page_no()).collect::<Vec<_>>(), [
			PageNo::new(1),
			PageNo::new(2),
		]);
		assert_eq!(pages[0].as_bytes()[PAGE_HEADER_SIZE], 1);
		assert_eq!(pages[1].as_bytes()[PAGE_HEADER_SIZE], 2);
		Ok(())
	}

	#[cfg(any(target_os = "linux", windows))]
	#[test]
	fn direct_file_writes_one_contiguous_extent_without_a_gap() -> Result<(), Box<dyn Error>> {
		let path = TestPath::new()?;
		let file = DirectFile::create_new(path.as_path())?;
		let mut pages = Vec::new();
		for raw_page_no in 0_u64..3 {
			let mut page = AlignedPage::new(
				PageType::StorageSuperblock,
				PageFlags::empty(),
				PageLsn::new(raw_page_no + 1),
				PageNo::new(raw_page_no),
			);
			page.as_bytes_mut()[PAGE_HEADER_SIZE] = raw_page_no as u8;
			page.seal()?;
			pages.push(page);
		}

		file.write_pages(PageNo::new(0), &pages)?;
		assert_eq!(file.page_count(), 3);
		assert!(file.write_pages(PageNo::new(4), &pages[..1]).is_err());
		let mut loaded = (0..3).map(|_| AlignedPage::zeroed()).collect::<Vec<_>>();
		file.read_pages(PageNo::new(0), &mut loaded)?;
		assert_eq!(loaded[2].as_bytes()[PAGE_HEADER_SIZE], 2);
		Ok(())
	}

	#[cfg(any(target_os = "linux", windows))]
	#[test]
	fn direct_file_batch_rejects_range_and_page_corruption() -> Result<(), Box<dyn Error>> {
		let path = TestPath::new()?;
		let file = DirectFile::create_new(path.as_path())?;
		file.resize_pages(2)?;
		for raw_page_no in 0_u64..2 {
			let page_no = PageNo::new(raw_page_no);
			let page = AlignedPage::new(
				PageType::StorageSuperblock,
				PageFlags::empty(),
				PageLsn::new(raw_page_no + 1),
				page_no,
			);
			file.write_page(page_no, &page)?;
		}
		file.sync_data()?;
		let mut outside = (0..2).map(|_| AlignedPage::zeroed()).collect::<Vec<_>>();
		assert!(file.read_pages_raw(PageNo::new(1), &mut outside).is_err());
		drop(file);

		let buffered = OpenOptions::new().write(true).open(path.as_path())?;
		let corruption_offset = PAGE_SIZE as u64 + PAGE_HEADER_SIZE as u64;
		let written = write_at(&buffered, &[0xff], corruption_offset)?;
		assert_eq!(written, 1);
		buffered.sync_data()?;
		drop(buffered);

		let file = DirectFile::open_read_only(path.as_path())?;
		let mut pages = (0..2).map(|_| AlignedPage::zeroed()).collect::<Vec<_>>();
		let error = file
			.read_pages(PageNo::new(0), &mut pages)
			.err()
			.ok_or_else(|| io::Error::other("corrupt page extent was accepted"))?;
		assert_eq!(error.status().code().to_string(), "FG-22S08");
		Ok(())
	}

	#[cfg(target_os = "linux")]
	#[test]
	fn direct_file_sets_o_direct_on_native_descriptor() -> Result<(), Box<dyn Error>> {
		use std::os::fd::AsRawFd;

		let path = TestPath::new()?;
		let file = DirectFile::create_new(path.as_path())?;
		// SAFETY: `file` owns a live descriptor for the duration of the call,
		// `F_GETFL` takes no variadic argument, and does not transfer ownership.
		let flags = unsafe { nix::libc::fcntl(file.file.as_raw_fd(), nix::libc::F_GETFL) };
		if flags == -1 {
			return Err(io::Error::last_os_error().into());
		}
		assert_ne!(flags & nix::libc::O_DIRECT, 0);
		Ok(())
	}

	#[cfg(windows)]
	#[test]
	fn direct_file_rejects_unaligned_io() -> Result<(), Box<dyn Error>> {
		use windows::Win32::Foundation::ERROR_INVALID_PARAMETER;

		let path = TestPath::new()?;
		let file = DirectFile::create_new(path.as_path())?;
		file.resize_pages(1)?;
		let mut byte = [0_u8; 1];
		let error = read_at(&file.file, &mut byte, 0)
			.err()
			.ok_or_else(|| io::Error::other("Direct I/O accepted a one-byte read"))?;
		assert_eq!(error.raw_os_error(), Some(ERROR_INVALID_PARAMETER.0 as i32));
		Ok(())
	}

	#[cfg(any(target_os = "linux", windows))]
	#[test]
	fn direct_file_rejects_non_page_multiple_length() -> Result<(), Box<dyn Error>> {
		let path = TestPath::new()?;
		std::fs::write(path.as_path(), [0_u8])?;

		let error = DirectFile::open_read_only(path.as_path())
			.err()
			.ok_or_else(|| io::Error::other("malformed file length was accepted"))?;
		assert_eq!(error.status().code().to_string(), "FG-22S09");
		Ok(())
	}

	#[cfg(any(target_os = "linux", windows))]
	#[test]
	fn exact_positional_read_rejects_short_input() -> Result<(), Box<dyn Error>> {
		let path = TestPath::new()?;
		std::fs::write(path.as_path(), [0_u8])?;
		let file = File::open(path.as_path())?;
		let mut page = AlignedPage::zeroed();

		let error = read_exact_at(&file, page.as_bytes_mut(), 0)
			.err()
			.ok_or_else(|| io::Error::other("short positional read was accepted"))?;
		assert_eq!(error.status().code().to_string(), "FG-51I02");
		Ok(())
	}

	#[cfg(any(target_os = "linux", windows))]
	#[test]
	fn exact_extent_read_rejects_short_input() -> Result<(), Box<dyn Error>> {
		let path = TestPath::new()?;
		std::fs::write(path.as_path(), [0_u8])?;
		let file = File::open(path.as_path())?;
		let mut pages = (0..2).map(|_| AlignedPage::zeroed()).collect::<Vec<_>>();

		let error = read_extent_at(&file, AlignedPage::slice_as_bytes_mut(&mut pages), 0)
			.err()
			.ok_or_else(|| io::Error::other("short positional extent read was accepted"))?;
		assert_eq!(error.status().code().to_string(), "FG-51I02");
		Ok(())
	}

	#[test]
	fn page_offset_rejects_overflow() {
		let error = checked_page_offset(PageNo::NULL);
		assert_eq!(
			error.err().map(|error| error.status().code().to_string()),
			Some("FG-22S08".to_owned())
		);
		assert!(checked_page_extent(PageNo::NULL, 1, u64::MAX).is_err());
	}

	#[cfg(any(target_os = "linux", windows))]
	#[test]
	fn create_new_never_overwrites_existing_file() -> Result<(), Box<dyn Error>> {
		let path = TestPath::new()?;
		let file = DirectFile::create_new(path.as_path())?;
		drop(file);

		assert!(DirectFile::create_new(path.as_path()).is_err());
		Ok(())
	}

	#[cfg(not(any(target_os = "linux", windows)))]
	#[test]
	fn unsupported_target_returns_typed_direct_io_status() -> Result<(), Box<dyn Error>> {
		let path = TestPath::new()?;
		let error = DirectFile::create_new(path.as_path())
			.err()
			.ok_or_else(|| io::Error::other("unsupported target opened a Direct I/O file"))?;
		assert_eq!(error.status().code().to_string(), "FG-51I01");
		Ok(())
	}
}
