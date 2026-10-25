//! Whole-group page packing shared by source, LIVE and invocation journals.

use super::layout::{FileImage, invalid};
use crate::{
	datastore::{DataSpaceId, DatabaseRoot},
	gql_status::FerrosGrynnResult,
	storage::{
		HistoryOwner,
		HistorySegment,
		catalog::{CatalogPath, FileCatalog, FileCatalogEntry},
		file_registry::DataFileRole,
		history_codec::{
			CodecBudget,
			pages::{SegmentHeader, encode_header, encode_pages, payload_pages},
		},
		page::{AlignedPage, PAGE_SIZE, PageNo},
		types::{DataFileId, DataFileKind},
	},
};

/// Stages one whole result or source group through the existing WAL transport.
pub(super) fn append_group(
	root: &DatabaseRoot,
	owner: HistoryOwner,
	payload: &[u8],
	clock: (u64, i128),
	segments: &mut Vec<HistorySegment>,
	files: &mut FileCatalog,
	images: &mut Vec<FileImage>,
	budget: &mut CodecBudget,
) -> FerrosGrynnResult<()> {
	let (stamp, time) = clock;
	let (file_kind, directory, extension) = match owner {
		HistoryOwner::EventOutbox(_) => (DataFileKind::EventOutbox, "events", "feo"),
		HistoryOwner::Changefeed(_) | HistoryOwner::Live(_) => {
			(DataFileKind::Changefeed, "changefeed", "fcf")
		}
	};
	if stamp == 0 || stamp > root.commit_ts || time > root.committed_at_ns {
		return Err(invalid("feed group exceeds the publication clock"));
	}
	let group_pages = u64::from(payload_pages(payload.len())?);
	let append = segments.last().copied().filter(|segment| {
		segment
			.header
			.pages
			.checked_add(group_pages)
			.and_then(|pages| pages.checked_mul(PAGE_SIZE as u64))
			.is_some_and(|bytes| bytes <= super::changefeed::segment_target())
	});
	// Page packing checks the complete payload before allocating
	// continuations.
	let first = append.map_or(1, |segment| segment.header.pages);
	let (locator, pages) = encode_pages(payload, stamp, first, budget)?;
	let mut segment = match append {
		Some(segment) => segment,
		None => {
			let file = DataFileId::new(files.next_file_instance_id, file_kind)
				.ok_or_else(|| invalid("feed file identity clock is exhausted"))?;
			files.next_file_instance_id = files
				.next_file_instance_id
				.checked_add(1)
				.ok_or_else(|| invalid("feed file identity clock is exhausted"))?;
			HistorySegment {
				file,
				header: SegmentHeader {
					database: root.database_uuid,
					owner,
					segment: uuid::Uuid::now_v7(),
					pages: 1,
					commits: 0,
					first_stamp: stamp,
					last_stamp: stamp,
					first_time: time,
					last_time: time,
				},
			}
		}
	};
	if segment.header.commits != 0
		&& (segment.header.last_stamp >= stamp || segment.header.last_time > time)
	{
		return Err(invalid("feed append clocks moved backwards"));
	}
	segment.header.pages = first
		.checked_add(u64::from(locator.pages))
		.ok_or_else(|| invalid("feed page ceiling overflows"))?;
	segment.header.commits = segment
		.header
		.commits
		.checked_add(1)
		.ok_or_else(|| invalid("feed group count overflows"))?;
	segment.header.last_stamp = stamp;
	segment.header.last_time = time;
	let registration = FileCatalogEntry {
		space_id: DataSpaceId::Database,
		file_id: segment.file,
		companion_of: None,
		role: DataFileRole::WalBackfilled,
		path: CatalogPath::parse(&format!(
			"{directory}/{}/{}.{extension}",
			owner.uuid(),
			segment.header.segment
		))?,
	};
	let count =
		pages.len().checked_add(1).ok_or_else(|| invalid("feed staging page count overflows"))?;
	budget.admit(
		count
			.checked_mul(2 * PAGE_SIZE + std::mem::size_of::<PageNo>() + 512)
			.ok_or_else(|| invalid("feed staging bytes overflow"))?,
	)?;
	let mut final_pages = Vec::new();
	final_pages
		.try_reserve_exact(count)
		.map_err(|_error| invalid("feed staging allocation failed"))?;
	final_pages.resize_with(count, AlignedPage::zeroed);
	encode_header(segment.header, &mut final_pages[0])?;
	for (source, output) in pages.iter().zip(&mut final_pages[1..]) {
		output.as_bytes_mut().copy_from_slice(source.as_bytes());
	}
	let page_numbers = std::iter::once(PageNo::new(0))
		.chain((first..segment.header.pages).map(PageNo::new))
		.collect();
	reserve(images, 1, budget)?;
	images.push(FileImage {
		entry: registration,
		page_numbers,
		pages: final_pages,
		prewritten_page_count: None,
	});
	if append.is_some() {
		let last = segments.last_mut().ok_or_else(|| invalid("feed append segment disappeared"))?;
		*last = segment;
	} else {
		reserve(segments, 1, budget)?;
		segments.push(segment);
	}
	Ok(())
}

/// Reserves complete replacement vector capacity before amortized growth.
pub(super) fn reserve<T>(
	output: &mut Vec<T>,
	additional: usize,
	budget: &mut CodecBudget,
) -> FerrosGrynnResult<()> {
	let required = output
		.len()
		.checked_add(additional)
		.ok_or_else(|| invalid("feed vector count overflows"))?;
	if required <= output.capacity() {
		return Ok(());
	}
	let capacity = output
		.capacity()
		.max(4)
		.checked_mul(2)
		.map(|count| count.max(required))
		.ok_or_else(|| invalid("feed vector capacity overflows"))?;
	budget.admit(
		capacity
			.checked_mul(std::mem::size_of::<T>())
			.ok_or_else(|| invalid("feed vector bytes overflow"))?,
	)?;
	output
		.try_reserve_exact(capacity - output.len())
		.map_err(|_error| invalid("feed vector allocation failed"))
}
