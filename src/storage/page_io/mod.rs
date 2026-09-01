//! User-selected synchronous or native queued page transport.

use std::{ops::Range, sync::Arc};

use super::{
	direct::{DirectFile, DirectFileOpenMode},
	page::{AlignedPage, PageHeader, PageNo},
};
use crate::gql_status::{FerrosGrynnError, FerrosGrynnResult};

/// Unix native-mode availability boundary.
#[cfg(not(windows))]
mod page_io_unix;
/// Windows IoRing transport.
#[cfg(all(windows, feature = "native-async-io"))]
mod page_io_windows;

#[cfg(all(windows, feature = "native-async-io"))]
pub(in crate::storage) use page_io_windows::WindowsIoRing;
#[cfg(all(test, windows, feature = "native-async-io"))]
pub(in crate::storage) use page_io_windows::{FrameRead, IoRingSubmissionStats};

/// Maximum Windows IoRing submission-queue capacity requested by the first
/// native implementation.
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
				#[cfg(not(windows))]
				{
					Err(page_io_unix::native_async_unavailable())
				}
				#[cfg(all(windows, not(feature = "native-async-io")))]
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

	/// Test-only frame reads report software SQE batching independently of any
	/// device queue-depth interpretation.
	#[cfg(all(windows, feature = "native-async-io"))]
	#[test]
	fn windows_ioring_reports_bounded_frame_submissions() -> FerrosGrynnResult<()> {
		use std::num::NonZeroUsize;

		use crate::storage::page::PAGE_HEADER_SIZE;

		const PAGE_COUNT: usize = 9;
		const SQE_BATCH_LIMITS: [usize; 4] = [1, 8, 32, 128];

		let path = TestPath::new()?;
		{
			let writer = DirectFile::create_new(path.as_path())?;
			writer.resize_pages(PAGE_COUNT as u64)?;
			let mut pages = Vec::with_capacity(PAGE_COUNT);
			for raw_page_no in 0..PAGE_COUNT as u64 {
				pages.push(test_page(raw_page_no, raw_page_no as u8)?);
			}
			writer.write_pages(PageNo::new(0), &pages)?;
			writer.sync_data()?;
		}

		let file = Arc::new(DirectFile::open_read_write_with_mode(
			path.as_path(),
			DirectFileOpenMode::Overlapped,
		)?);
		let mut ring = WindowsIoRing::new()?;
		let submission_capacity =
			usize::try_from(ring.submission_capacity()).map_err(|source| {
				FerrosGrynnError::storage_io_failure(format!(
					"IoRing test submission capacity does not fit usize: {source}"
				))
			})?;
		assert!(submission_capacity > 0);

		for requested_limit in SQE_BATCH_LIMITS {
			let mut frames = (0..PAGE_COUNT).map(|_| AlignedPage::zeroed()).collect::<Vec<_>>();
			let mut reads = frames
				.iter_mut()
				.enumerate()
				.map(|(index, frame)| FrameRead::new(&file, PageNo::new(index as u64), frame))
				.collect::<Vec<_>>();
			let requested_limit = NonZeroUsize::new(requested_limit).ok_or_else(|| {
				FerrosGrynnError::storage_io_failure("IoRing test SQE batch limit is zero")
			})?;
			let stats = ring.read_frames_measured(&mut reads, requested_limit)?;
			let effective_limit = requested_limit.get().min(submission_capacity);
			assert_eq!(stats.submission_capacity(), ring.submission_capacity());
			assert_eq!(stats.requested_sqe_batch_limit(), requested_limit.get());
			assert_eq!(stats.effective_sqe_batch_limit(), effective_limit);
			assert_eq!(stats.submit_calls(), PAGE_COUNT.div_ceil(effective_limit));
			assert_eq!(stats.submitted_sqes(), PAGE_COUNT);
			assert_eq!(stats.completed_cqes(), PAGE_COUNT);
			drop(reads);
			for (index, frame) in frames.iter().enumerate() {
				frame.validate(PageNo::new(index as u64))?;
				assert_eq!(frame.as_bytes()[PAGE_HEADER_SIZE], index as u8);
			}
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
