//! Computed LIVE replay through the same pinned WAL/home pages as source
//! history.

use super::{
	LocalChangefeedSnapshot,
	LocalStore,
	changefeed_reader::{available, check_history_access},
	layout::invalid,
};
use crate::{
	gql_status::FerrosGrynnResult,
	live::LivePayload,
	storage::{
		HistoryOwner,
		HistorySegment,
		history_codec::{
			CodecBudget,
			pages::{decode_pages, first_payload},
		},
		live_history::{self, LiveGroupInfo},
		page::{AlignedPage, PAGE_SIZE},
	},
};

impl LocalStore {
	/// Reads fixed private metadata before allocating historical values.
	pub(crate) fn inspect_live_group(
		&self,
		snapshot: &LocalChangefeedSnapshot,
		segment: HistorySegment,
		first: u64,
		memory_bytes: usize,
	) -> FerrosGrynnResult<Option<(LiveGroupInfo, u64)>> {
		check_live_access(snapshot, segment, memory_bytes)?;
		available((|| {
			let mut page = AlignedPage::zeroed();
			let locator = self.history_locator(snapshot, segment, first, &mut page)?;
			let info = live_history::inspect(first_payload(&page)?, locator.bytes)?;
			check_group(info, segment, locator.stamp)?;
			Ok((info, first + u64::from(locator.pages)))
		})())
	}

	/// Restores saved result images, without querying current records or FETCH.
	pub(crate) fn read_live_payload(
		&self,
		snapshot: &LocalChangefeedSnapshot,
		segment: HistorySegment,
		first: u64,
		memory_bytes: usize,
	) -> FerrosGrynnResult<Option<(LivePayload, LiveGroupInfo, u64)>> {
		check_live_access(snapshot, segment, memory_bytes)?;
		let mut budget = CodecBudget::new(memory_bytes, PAGE_SIZE)?;
		let result = (|| {
			let mut page = AlignedPage::zeroed();
			let locator = self.history_locator(snapshot, segment, first, &mut page)?;
			let info = live_history::inspect(first_payload(&page)?, locator.bytes)?;
			check_group(info, segment, locator.stamp)?;
			info.admit_delivery(locator.bytes)?;
			let mut read = |number, output: &mut AlignedPage| {
				self.read_history_page(snapshot, segment, number, output)
			};
			let bytes = decode_pages(locator, segment.header.pages, &mut read, &mut budget)?;
			let payload = live_history::decode_delivery_bounded(&bytes, &mut budget)?;
			Ok((payload, info, first + u64::from(locator.pages)))
		})();
		snapshot.record_history_scratch(budget.admitted_bytes());
		available(result)
	}
}

/// Wrong authoritative owners must never be converted into optional corruption.
fn check_live_access(
	snapshot: &LocalChangefeedSnapshot,
	segment: HistorySegment,
	memory_bytes: usize,
) -> FerrosGrynnResult<()> {
	check_history_access(snapshot, segment, memory_bytes)?;
	if !matches!(segment.owner(), HistoryOwner::Live(_)) {
		return Err(invalid("private LIVE reader received source history"));
	}
	Ok(())
}

/// The logical envelope, continuation and trusted snapshot bounds must agree.
fn check_group(info: LiveGroupInfo, segment: HistorySegment, stamp: u64) -> FerrosGrynnResult<()> {
	if segment.owner() != HistoryOwner::Live(info.id)
		|| info.stamp != stamp
		|| info.time < segment.header.first_time
		|| info.time > segment.header.last_time
	{
		return Err(invalid("LIVE envelope disagrees with its authoritative segment"));
	}
	Ok(())
}
