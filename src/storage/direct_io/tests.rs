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
	let page =
		AlignedPage::new(PageType::StorageSuperblock, PageFlags::empty(), PageLsn::new(1), page_no);

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
	// `F_GETFL` takes no variadic argument, and does not transfer
	// ownership.
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
