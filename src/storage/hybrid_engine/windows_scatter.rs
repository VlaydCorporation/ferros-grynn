//! Windows scatter transport with one bounded read-ahead extent.

use std::{
	ptr,
	sync::{
		Arc,
		atomic::{AtomicBool, Ordering},
		mpsc::{self, Receiver, SyncSender},
	},
	thread::{self, JoinHandle},
};

use parking_lot::MutexGuard;
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

use super::MAX_SCATTER_PAGES;
use crate::{
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	storage::{
		direct_io::DirectFile,
		page::{AlignedPage, PAGE_SIZE, PageNo},
	},
};

/// Event retained until every overlapped scatter read completes.
struct ScatterEvent(HANDLE);

impl ScatterEvent {
	/// Creates one reusable manual-reset event.
	fn new() -> FerrosGrynnResult<Self> {
		// SAFETY: no security descriptor or name is supplied; the returned
		// event is exclusively owned by this wrapper.
		let handle = unsafe { CreateEventW(None, true, false, PCWSTR::null()) }
			.map_err(|source| windows_failure("create ReadFileScatter event", source))?;
		Ok(Self(handle))
	}

	/// Resets the event after the preceding request reached completion.
	fn reset(&self) -> FerrosGrynnResult<()> {
		// SAFETY: this wrapper owns a valid event and no operation is in
		// flight.
		unsafe { ResetEvent(self.0) }
			.map_err(|source| windows_failure("reset ReadFileScatter event", source))
	}
}

impl Drop for ScatterEvent {
	fn drop(&mut self) {
		// SAFETY: this wrapper owns the valid handle and closes it once.
		drop(unsafe { CloseHandle(self.0) });
	}
}

/// Per-worker state reused for every extent in one scan.
struct ScatterReader {
	system_page_size: usize,
	segments: Vec<FILE_SEGMENT_ELEMENT>,
	event: ScatterEvent,
	overlapped: Box<OVERLAPPED>,
}

impl ScatterReader {
	/// Checks the native page layout and creates reusable request state.
	fn new() -> FerrosGrynnResult<Self> {
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

	/// Issues one native scatter request and waits for exact completion.
	fn read_raw(
		&mut self,
		file: &DirectFile,
		first_page: PageNo,
		pages: &mut [AlignedPage],
	) -> FerrosGrynnResult<()> {
		if pages.is_empty() || pages.len() > MAX_SCATTER_PAGES {
			return Err(FerrosGrynnError::invalid_storage_page(
				first_page.get(),
				"ReadFileScatter extent must contain 1..128 pages",
			));
		}
		let file_offset = file.prepare_read_extent(first_page, pages.len())?;
		let byte_len = pages.len().checked_mul(PAGE_SIZE).ok_or_else(|| {
			FerrosGrynnError::storage_io_failure("ReadFileScatter byte length overflow")
		})?;
		let byte_len = u32::try_from(byte_len).map_err(|source| {
			FerrosGrynnError::storage_io_failure(format!(
				"ReadFileScatter byte length does not fit u32: {source}"
			))
		})?;
		let segments_per_page = PAGE_SIZE / self.system_page_size;
		let segment_count = pages.len().checked_mul(segments_per_page).ok_or_else(|| {
			FerrosGrynnError::storage_io_failure("ReadFileScatter segment count overflow")
		})?;
		self.segments.clear();
		self.segments.reserve(segment_count.saturating_sub(self.segments.capacity()));
		for page in pages {
			let base = page.as_bytes_mut().as_mut_ptr();
			for segment in 0..segments_per_page {
				let offset = segment.checked_mul(self.system_page_size).ok_or_else(|| {
					FerrosGrynnError::storage_io_failure("ReadFileScatter segment offset overflow")
				})?;
				// SAFETY: `offset < PAGE_SIZE`; each pointer names a disjoint
				// system page inside the exclusively borrowed frame.
				let address = unsafe { base.add(offset) };
				if !(address as usize).is_multiple_of(self.system_page_size) {
					return Err(FerrosGrynnError::storage_io_failure(
						"ReadFileScatter destination is not system-page aligned",
					));
				}
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
		// SAFETY: the companion handle is OVERLAPPED|NO_BUFFERING; every
		// segment points into a distinct aligned frame, and all storage remains
		// alive until `GetOverlappedResult` reports terminal completion.
		let submitted = unsafe {
			ReadFileScatter(
				handle,
				self.segments.as_ptr(),
				byte_len,
				None,
				ptr::from_mut(self.overlapped.as_mut()),
			)
		};
		if let Err(source) = submitted
			&& source.code() != HRESULT::from_win32(ERROR_IO_PENDING.0)
		{
			return Err(windows_failure("submit ReadFileScatter", source));
		}
		let mut transferred = 0_u32;
		// SAFETY: waiting for terminal completion ends native access to every
		// borrowed destination before this function returns.
		unsafe {
			GetOverlappedResult(handle, self.overlapped.as_ref(), &raw mut transferred, true)
		}
		.map_err(|source| windows_failure("complete ReadFileScatter", source))?;
		if transferred != byte_len {
			return Err(FerrosGrynnError::storage_io_failure(format!(
				"short ReadFileScatter transfer: {transferred} of {byte_len} bytes"
			)));
		}
		Ok(())
	}
}

/// Owned raw extent passed from the worker to the validating pull cursor.
struct LoadedExtent {
	first_page: PageNo,
	page_count: usize,
	pages: Vec<AlignedPage>,
}

/// Ordered worker result.
struct ScatterMessage {
	ordinal: usize,
	result: FerrosGrynnResult<LoadedExtent>,
}

/// Borrowed view of one fully validated extent.
pub struct ValidatedScatterExtent<'cursor> {
	first_page: PageNo,
	pages: &'cursor [AlignedPage],
}

impl ValidatedScatterExtent<'_> {
	/// Returns the first physical page number in this extent.
	pub const fn first_page(&self) -> PageNo {
		self.first_page
	}

	/// Returns every validated page in physical order.
	pub const fn pages(&self) -> &[AlignedPage] {
		self.pages
	}
}

/// Synchronous ordered cursor with one background extent of read-ahead.
pub struct ScatterPrefetchCursor<'engine> {
	_receiver: Option<Receiver<ScatterMessage>>,
	empty_sender: Option<SyncSender<Vec<AlignedPage>>>,
	cancelled: Arc<AtomicBool>,
	worker: Option<JoinHandle<()>>,
	current: Option<LoadedExtent>,
	next_ordinal: usize,
	_lane: MutexGuard<'engine, ()>,
}

impl<'engine> ScatterPrefetchCursor<'engine> {
	/// Starts one bounded scatter lane with two reusable extent buffers.
	pub(super) fn new(
		file: Arc<DirectFile>,
		first_page: PageNo,
		page_count: usize,
		extent_pages: usize,
		lane: MutexGuard<'engine, ()>,
	) -> FerrosGrynnResult<Self> {
		if page_count == 0 || !(2..=MAX_SCATTER_PAGES).contains(&extent_pages) {
			return Err(FerrosGrynnError::storage_io_failure(
				"scatter prefetch requires pages and two buffers of 2..128 pages",
			));
		}
		file.prepare_read_extent(first_page, page_count)?;
		file.raw_handle()?;

		let (empty_sender, empty_receiver) = mpsc::sync_channel(2);
		for _ in 0..2 {
			let pages =
				(0..extent_pages).map(|_| AlignedPage::zeroed()).collect::<Vec<AlignedPage>>();
			empty_sender.send(pages).map_err(|source| {
				FerrosGrynnError::storage_io_failure("initialize scatter extent buffers")
					.with_technical_source(source)
			})?;
		}
		let (sender, receiver) = mpsc::sync_channel(0);
		let cancelled = Arc::new(AtomicBool::new(false));
		let worker_cancelled = Arc::clone(&cancelled);
		let worker = thread::spawn(move || {
			let mut reader = match ScatterReader::new() {
				Ok(reader) => reader,
				Err(error) => {
					let _ignored = sender.send(ScatterMessage {
						ordinal: 0,
						result: Err(error),
					});
					return;
				}
			};
			let extent_count = page_count.div_ceil(extent_pages);
			for ordinal in 0..extent_count {
				if worker_cancelled.load(Ordering::Acquire) {
					break;
				}
				let mut pages = match empty_receiver.recv() {
					Ok(pages) => pages,
					Err(_) => break,
				};
				let offset = ordinal * extent_pages;
				let count = extent_pages.min(page_count - offset);
				let extent_first = match checked_page_no(first_page, offset) {
					Ok(page_no) => page_no,
					Err(error) => {
						let _ignored = sender.send(ScatterMessage {
							ordinal,
							result: Err(error),
						});
						break;
					}
				};
				let result = reader.read_raw(&file, extent_first, &mut pages[..count]).map(|()| {
					LoadedExtent {
						first_page: extent_first,
						page_count: count,
						pages,
					}
				});
				let terminal = result.is_err();
				if sender
					.send(ScatterMessage {
						ordinal,
						result,
					})
					.is_err() || terminal
				{
					break;
				}
			}
		});
		Ok(Self {
			_receiver: Some(receiver),
			empty_sender: Some(empty_sender),
			cancelled,
			worker: Some(worker),
			current: None,
			next_ordinal: 0,
			_lane: lane,
		})
	}

	/// Pulls the next fully validated extent in physical order.
	pub fn next_extent(&mut self) -> Option<FerrosGrynnResult<ValidatedScatterExtent<'_>>> {
		self.recycle_current();
		let received = self._receiver.as_ref()?.recv();
		match received {
			Ok(message) if message.ordinal == self.next_ordinal => {
				self.next_ordinal += 1;
				match message.result {
					Ok(extent) => {
						for (index, page) in extent.pages[..extent.page_count].iter().enumerate() {
							let result = checked_page_no(extent.first_page, index)
								.and_then(|page_no| page.validate(page_no));
							if let Err(error) = result {
								let _ignored = self.shutdown();
								return Some(Err(error));
							}
						}
						self.current = Some(extent);
						self.current.as_ref().map(|current| {
							Ok(ValidatedScatterExtent {
								first_page: current.first_page,
								pages: &current.pages[..current.page_count],
							})
						})
					}
					Err(error) => {
						let _ignored = self.shutdown();
						Some(Err(error))
					}
				}
			}
			Ok(message) => {
				let error = FerrosGrynnError::storage_io_failure(format!(
					"scatter extent {} arrived while {} was expected",
					message.ordinal, self.next_ordinal
				));
				let _ignored = self.shutdown();
				Some(Err(error))
			}
			Err(_) => match self.shutdown() {
				Ok(()) => None,
				Err(error) => Some(Err(error)),
			},
		}
	}

	/// Joins the worker and releases the single scatter lane.
	pub fn finish(mut self) -> FerrosGrynnResult<()> {
		self.shutdown()
	}

	/// Returns the consumer's preceding buffer for the next read-ahead.
	fn recycle_current(&mut self) {
		if let Some(current) = self.current.take()
			&& let Some(sender) = self.empty_sender.as_ref()
		{
			let _ignored = sender.send(current.pages);
		}
	}

	/// Cancels queued work in deadlock-safe channel order and joins it.
	fn shutdown(&mut self) -> FerrosGrynnResult<()> {
		self.cancelled.store(true, Ordering::Release);
		self.recycle_current();
		drop(self._receiver.take());
		drop(self.empty_sender.take());
		if let Some(worker) = self.worker.take() {
			worker.join().map_err(|payload| {
				drop(payload);
				FerrosGrynnError::storage_io_failure("scatter prefetch worker panicked during join")
			})?;
		}
		Ok(())
	}
}

impl Drop for ScatterPrefetchCursor<'_> {
	fn drop(&mut self) {
		let _ignored = self.shutdown();
	}
}

/// Adds a bounded page offset to one physical page number.
fn checked_page_no(first_page: PageNo, offset: usize) -> FerrosGrynnResult<PageNo> {
	let offset = u64::try_from(offset).map_err(|source| {
		FerrosGrynnError::invalid_storage_page(
			first_page.get(),
			format!("scatter page offset does not fit u64: {source}"),
		)
	})?;
	first_page.get().checked_add(offset).map(PageNo::new).ok_or_else(|| {
		FerrosGrynnError::invalid_storage_page(first_page.get(), "scatter page number overflow")
	})
}

/// Preserves one native failure as the technical storage-error source.
fn windows_failure(operation: &'static str, source: windows::core::Error) -> FerrosGrynnError {
	FerrosGrynnError::storage_io_failure(operation).with_technical_source(source)
}
