#[allow(clippy::wildcard_imports, reason = "buffer-pool helpers use their facade contracts")]
use super::*;

/// One unpublished frame owned by a batched miss operation.
#[cfg(test)]
pub(super) struct BatchLoadReservation {
	/// Position in the deduplicated request vector.
	pub(super) unique_index: usize,
	/// Fixed frame selected under the metadata mutex.
	pub(super) frame_index: usize,
	/// Exact page version being loaded.
	pub(super) key: PageCacheKey,
	/// Completion signal retained by coalesced scalar or batch callers.
	pub(super) signal: Arc<LoadSignal>,
	/// Unique reusable aligned frame buffer.
	pub(super) page: CachedPage,
}

/// One source-local extent and its assigned range in a shared batch buffer.
#[cfg(test)]
pub(in crate::storage) struct PageLoadExtent {
	/// First physical page identity in this extent.
	pub(super) first_key: PageKey,
	/// First page in the shared scratch buffer.
	pub(super) buffer_start: usize,
	/// Number of physically adjacent pages.
	pub(super) page_count: usize,
}

/// Loads sorted reservations as adjacent, source-local extents.
#[cfg(test)]
pub(super) fn load_reserved_extents<Loader>(
	reservations: &mut [BatchLoadReservation],
	scratch: &mut [AlignedPage],
	loader: &mut Loader,
) -> FerrosGrynnResult<()>
where
	Loader: FnMut(&[PageLoadExtent], &mut [AlignedPage]) -> FerrosGrynnResult<()>,
{
	let mut next_reservation = 0;
	while next_reservation < reservations.len() {
		let batch_start = next_reservation;
		let mut buffer_pages = 0;
		let mut extents = Vec::new();
		while next_reservation < reservations.len() && buffer_pages < scratch.len() {
			let extent_start = next_reservation;
			let first_key = reservations[extent_start].key.page_key();
			let version = reservations[extent_start].key.version();
			let remaining_buffer = scratch.len() - buffer_pages;
			let mut extent_end = extent_start + 1;
			while extent_end < reservations.len() && extent_end - extent_start < remaining_buffer {
				let previous = reservations[extent_end - 1].key;
				let current = reservations[extent_end].key;
				let adjacent =
					previous.page_no().get().checked_add(1) == Some(current.page_no().get());
				let same_file = current.page_key().space_id() == first_key.space_id()
					&& current.page_key().file_id() == first_key.file_id();
				if !same_file || current.version() != version || !adjacent {
					break;
				}
				extent_end += 1;
			}
			let extent_len = extent_end - extent_start;
			extents.push(PageLoadExtent {
				first_key,
				buffer_start: buffer_pages,
				page_count: extent_len,
			});
			buffer_pages += extent_len;
			next_reservation = extent_end;
		}

		let batch_pages = &mut scratch[..buffer_pages];
		loader(&extents, batch_pages)?;
		let mut loaded_reservation = batch_start;
		for extent in extents {
			let extent_end = loaded_reservation + extent.page_count;
			let buffer_end = extent.buffer_start + extent.page_count;
			let extent_pages = &batch_pages[extent.buffer_start..buffer_end];
			let extent_reservations = &mut reservations[loaded_reservation..extent_end];
			if extent.page_count >= PARALLEL_VALIDATION_MIN_PAGES {
				extent_reservations.par_iter_mut().zip(extent_pages.par_iter()).try_for_each(
					|(reservation, loaded_page)| install_loaded_page(reservation, loaded_page),
				)?;
			} else {
				for (reservation, loaded_page) in
					extent_reservations.iter_mut().zip(extent_pages.iter())
				{
					install_loaded_page(reservation, loaded_page)?;
				}
			}
			loaded_reservation = extent_end;
		}
	}
	Ok(())
}

/// Validates one loaded image and copies it into its permanent frame.
#[cfg(test)]
fn install_loaded_page(
	reservation: &mut BatchLoadReservation,
	loaded_page: &AlignedPage,
) -> FerrosGrynnResult<()> {
	let header = loaded_page.validate(reservation.key.page_no())?;
	validate_loaded_header(reservation.key, header)?;
	let frame = Arc::get_mut(&mut reservation.page).ok_or_else(|| {
		FerrosGrynnError::storage_io_failure("buffer-pool batch frame was not uniquely owned")
	})?;
	frame.as_mut().as_bytes_mut().copy_from_slice(loaded_page.as_bytes());
	Ok(())
}

/// Completion cell for all callers coalesced onto one physical page load.
pub(super) struct LoadSignal {
	/// `None` while loading, then one clonable pinned page or error result.
	result: Mutex<Option<FerrosGrynnResult<CachedPage>>>,
	/// Notification sent exactly when the result is published.
	completed: Condvar,
}

impl LoadSignal {
	/// Creates an incomplete load signal.
	pub(super) fn new() -> Self {
		Self {
			result: Mutex::new(None),
			completed: Condvar::new(),
		}
	}

	/// Waits until the physical load publishes a result.
	pub(super) fn wait(&self) -> FerrosGrynnResult<CachedPage> {
		let mut result = self.result.lock();
		loop {
			if let Some(result) = result.as_ref() {
				return result.clone();
			}
			self.completed.wait(&mut result);
		}
	}

	/// Publishes the load result and wakes every coalesced caller.
	pub(super) fn finish(&self, outcome: FerrosGrynnResult<CachedPage>) {
		*self.result.lock() = Some(outcome);
		self.completed.notify_all();
	}
}
