//! Snapshot-exact history reading, independent of feed staging and publication.

use std::sync::{
	Arc,
	atomic::{AtomicU64, Ordering},
};

use super::{LocalStore, changefeed::ChangefeedFiles, layout::invalid};
use crate::{
	datastore::{CapturedCommit, DataSpaceId},
	gql_status::{Condition, FerrosGrynnResult},
	storage::{
		ChangefeedGroupInfo,
		HistoryOwner,
		HistorySegment,
		changefeed::{
			CodecBudget,
			CommitLocator,
			decode_group,
			decode_header,
			decode_locator,
			decode_pages,
			first_payload,
			inspect_group,
		},
		page::{AlignedPage, PAGE_SIZE, PageNo},
		types::{CheckpointGeneration, Lsn, PageKey, StorageSnapshot},
		wal::TxHandle,
	},
};

/// Constant-size history pin; constructing it never scans graph/index bindings.
#[derive(Clone)]
pub struct LocalChangefeedSnapshot(
	/// Shared WAL visibility pin and immutable changefeed file handles.
	Arc<ChangefeedSnapshotInner>,
	/// Per-reader page-attempt and scratch high-water observations.
	Arc<ReadObservation>,
);

/// Per-reader counters; the durable pin remains shared independently.
#[derive(Default)]
struct ReadObservation {
	/// Logical read attempts, including cache/WAL hits and failures.
	pages: AtomicU64,
	/// Conservative codec admission high-water mark.
	scratch: AtomicU64,
}

impl LocalChangefeedSnapshot {
	/// Reuses file/WAL pins with an independent constant-size observation
	/// scope.
	pub(crate) fn observed_reader(&self) -> Self {
		Self(Arc::clone(&self.0), Arc::new(ReadObservation::default()))
	}

	/// Returns logical page attempts and admitted codec scratch bytes.
	pub(crate) fn read_observation(&self) -> (u64, u64) {
		(self.1.pages.load(Ordering::Relaxed), self.1.scratch.load(Ordering::Relaxed))
	}

	/// Accounts both source and private LIVE decode admission in the same pin.
	pub(super) fn record_history_scratch(&self, bytes: usize) {
		self.1.scratch.fetch_max(bytes as u64, Ordering::Relaxed);
	}
}

/// Immutable feed handles and visibility protected by one existing WAL pin.
struct ChangefeedSnapshotInner {
	/// Registered visibility pin, released after the last reader/snapshot
	/// clone.
	_pin: TxHandle,
	/// Exact durable commit visibility.
	snapshot: StorageSnapshot,
	/// Home namespace selected with the logical root.
	home_generation: CheckpointGeneration,
	/// Snapshot-visible home LSN ceiling after checkpoint.
	home_ceiling: Lsn,
	/// Immutable shared feed file handles.
	changefeeds: ChangefeedFiles,
}

impl LocalStore {
	/// Pins only history files and WAL visibility at the exact logical root
	/// clock.
	pub(crate) fn capture_changefeed_snapshot(
		&self,
		stamp: u64,
	) -> FerrosGrynnResult<Option<LocalChangefeedSnapshot>> {
		let state = self.state.lock();
		let pin = self.wal.begin_tx()?;
		let snapshot = pin.snapshot();
		if snapshot.commit_ts().get() != stamp {
			return Ok(None);
		}
		Ok(Some(LocalChangefeedSnapshot(
			Arc::new(ChangefeedSnapshotInner {
				_pin: pin,
				snapshot,
				home_generation: state.database_superblock.generation,
				home_ceiling: snapshot.visible_lsn(),
				changefeeds: Arc::clone(&state.changefeed_files),
			}),
			Arc::new(ReadObservation::default()),
		)))
	}

	/// Reads fixed metadata without allocating dictionary or image payloads.
	/// None identifies unavailable segment bytes with independently trusted
	/// bounds.
	pub(crate) fn inspect_changefeed_group(
		&self,
		snapshot: &LocalChangefeedSnapshot,
		segment: HistorySegment,
		first: u64,
		memory_bytes: usize,
	) -> FerrosGrynnResult<Option<ChangefeedGroupInfo>> {
		check_source_access(snapshot, segment, memory_bytes)?;
		available((|| {
			let mut page = AlignedPage::zeroed();
			let locator = self.history_locator(snapshot, segment, first, &mut page)?;
			let mut info = inspect_group(first_payload(&page)?, locator.bytes)?;
			if segment.header.owner != HistoryOwner::Changefeed(info.source)
				|| info.stamp != locator.stamp
				|| info.time < segment.header.first_time
				|| info.time > segment.header.last_time
			{
				return Err(invalid("changefeed group descriptor disagrees with segment bounds"));
			}
			info.next_page = first + u64::from(locator.pages);
			info.pages = locator.pages;
			Ok(info)
		})())
	}

	/// Materializes one complete group or reports unavailable segment bytes.
	pub(crate) fn read_changefeed_batch(
		&self,
		snapshot: &LocalChangefeedSnapshot,
		segment: HistorySegment,
		first: u64,
		memory_bytes: usize,
	) -> FerrosGrynnResult<Option<(CapturedCommit, u64)>> {
		check_source_access(snapshot, segment, memory_bytes)?;
		available(self.read_changefeed_group(snapshot, segment, first, memory_bytes))
	}

	/// Restores exact owned images through the existing pinned WAL/home route.
	pub(crate) fn read_changefeed_group(
		&self,
		snapshot: &LocalChangefeedSnapshot,
		segment: HistorySegment,
		first: u64,
		memory_bytes: usize,
	) -> FerrosGrynnResult<(CapturedCommit, u64)> {
		check_source_access(snapshot, segment, memory_bytes)?;
		let mut budget = CodecBudget::new(memory_bytes, PAGE_SIZE)?;
		let result = (|| {
			let mut page = AlignedPage::zeroed();
			let locator = self.history_locator(snapshot, segment, first, &mut page)?;
			let mut read = |number, output: &mut AlignedPage| {
				self.read_history_page(snapshot, segment, number, output)
			};
			let bytes = decode_pages(locator, segment.header.pages, &mut read, &mut budget)?;
			let group = decode_group(&bytes, &mut budget)?;
			let time = group.commit.net.committed_at().as_inner().timestamp().as_nanosecond();
			if segment.header.owner != HistoryOwner::Changefeed(group.commit.net.source())
				|| group.commit.net.versionstamp().get() != locator.stamp
				|| time < segment.header.first_time
				|| time > segment.header.last_time
			{
				return Err(invalid("changefeed group descriptor disagrees with segment bounds"));
			}
			Ok((group.commit, first + u64::from(locator.pages)))
		})();
		snapshot.record_history_scratch(budget.admitted_bytes());
		result
	}

	/// Verifies page-zero authority and the first continuation before payload
	/// work.
	pub(super) fn history_locator(
		&self,
		snapshot: &LocalChangefeedSnapshot,
		segment: HistorySegment,
		first: u64,
		page: &mut AlignedPage,
	) -> FerrosGrynnResult<CommitLocator> {
		self.read_history_page(snapshot, segment, PageNo::new(0), page)?;
		if decode_header(page)? != segment.header {
			return Err(invalid("changefeed segment header disagrees with snapshot catalog"));
		}
		if first == 0 || first >= segment.header.pages {
			return Err(invalid("changefeed group ordinal is outside its segment"));
		}
		self.read_history_page(snapshot, segment, PageNo::new(first), page)?;
		let locator = decode_locator(page, PageNo::new(first), segment.header.pages)?;
		if locator.stamp < segment.header.first_stamp || locator.stamp > segment.header.last_stamp {
			return Err(invalid("changefeed group stamp exceeds trusted bounds"));
		}
		Ok(locator)
	}

	/// Resolves a pinned file without consulting later catalog generations.
	pub(super) fn read_history_page(
		&self,
		snapshot: &LocalChangefeedSnapshot,
		segment: HistorySegment,
		number: PageNo,
		output: &mut AlignedPage,
	) -> FerrosGrynnResult<()> {
		let _previous =
			snapshot.1.pages.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |value| {
				Some(value.saturating_add(1))
			});
		snapshot.1.scratch.fetch_max(PAGE_SIZE as u64, Ordering::Relaxed);
		let file = snapshot
			.0
			.changefeeds
			.get(&segment.file)
			.ok_or_else(|| invalid("changefeed segment file is unavailable"))?;
		crate::storage::image_io::read_pinned_page_cached(
			&self.wal,
			file,
			&self.engine,
			snapshot.0.snapshot,
			snapshot.0.home_generation,
			snapshot.0.home_ceiling,
			PageKey::new(DataSpaceId::Database, segment.file, number),
			output,
		)
	}
}

/// Authoritative corruption, expired pins and resource limits are never GAPs.
fn check_source_access(
	snapshot: &LocalChangefeedSnapshot,
	segment: HistorySegment,
	memory_bytes: usize,
) -> FerrosGrynnResult<()> {
	check_history_access(snapshot, segment, memory_bytes)?;
	if !matches!(segment.owner(), HistoryOwner::Changefeed(_)) {
		return Err(invalid("source history reader received a private LIVE segment"));
	}
	Ok(())
}

/// Shared authoritative checks run before optional bytes may degrade to a GAP.
pub(super) fn check_history_access(
	snapshot: &LocalChangefeedSnapshot,
	segment: HistorySegment,
	memory_bytes: usize,
) -> FerrosGrynnResult<()> {
	if !snapshot.0._pin.is_active() {
		return Err(invalid("changefeed snapshot is no longer accessible"));
	}
	segment.header.validate()?;
	CodecBudget::new(memory_bytes, PAGE_SIZE)?;
	Ok(())
}

/// Only optional segment I/O/format failures degrade to trusted-bound GAPs.
pub(super) fn available<T>(result: FerrosGrynnResult<T>) -> FerrosGrynnResult<Option<T>> {
	match result {
		Ok(value) => Ok(Some(value)),
		Err(error) if error.status().condition() == Condition::ProgramLimitExceeded => Err(error),
		Err(_error) => Ok(None),
	}
}
