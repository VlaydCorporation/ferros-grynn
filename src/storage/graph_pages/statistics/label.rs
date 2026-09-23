//! Exact label counters in an independently lazy file.

use std::collections::BTreeMap;

use super::{checked_page_no, codec::*};
use crate::{
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	graph::AtomKind,
	stats::{GraphStatistics, LabelStatistic, LabelStatisticKey},
	storage::{
		graph_index::{LabelDictionary, LabelKind},
		page::{AlignedPage, PAGE_SIZE, PageFlags, PageLsn, PageNo, PageType},
	},
};

const NEXT_PAGE: usize = 32;
const COUNT: usize = 40;
const RESERVED: usize = 44;
const ENTRIES: usize = 48;
const ENTRY_BYTES: usize = 24;
const PER_PAGE: usize = (PAGE_SIZE - ENTRIES) / ENTRY_BYTES;

#[derive(Clone, Copy)]
struct EncodedEntry<'entry> {
	kind: LabelKind,
	label_id: u32,
	statistic: &'entry LabelStatistic,
}

pub(super) fn encode_label_statistics(
	statistics: &GraphStatistics,
	dictionary: &LabelDictionary,
	page_lsn: PageLsn,
) -> FerrosGrynnResult<Vec<AlignedPage>> {
	let mut entries = statistics
		.labels
		.iter()
		.map(|(key, statistic)| {
			let kind = storage_kind(key.kind);
			let label_id = dictionary
				.id(kind, key.label.as_str())
				.ok_or_else(|| invalid("label statistic is absent from label dictionary"))?;
			Ok(EncodedEntry {
				kind,
				label_id,
				statistic,
			})
		})
		.collect::<FerrosGrynnResult<Vec<_>>>()?;
	entries.sort_unstable_by_key(|entry| (entry.kind, entry.label_id));
	let page_count = entries.len().div_ceil(PER_PAGE);
	let mut pages = Vec::with_capacity(page_count);
	for (index, chunk) in entries.chunks(PER_PAGE).enumerate() {
		let page_no = checked_page_no(index, 0, "label statistics")?;
		let next = (index + 1 < page_count).then(|| PageNo::new(page_no.get().saturating_add(1)));
		pages.push(encode_page(page_no, next, chunk, page_lsn)?);
	}
	Ok(pages)
}

pub(in crate::storage) fn decode_label_statistics<ReadPage>(
	mut read_page: ReadPage,
	layout: super::StatisticsLayout,
	dictionary: &LabelDictionary,
) -> FerrosGrynnResult<BTreeMap<LabelStatisticKey, LabelStatistic>>
where
	ReadPage: FnMut(PageNo, &mut AlignedPage) -> FerrosGrynnResult<()>,
{
	let count = usize::try_from(layout.label_pages)
		.map_err(|_error| invalid("label statistics page count exceeds usize"))?;
	let mut result = BTreeMap::new();
	let mut previous = None;
	let mut page = AlignedPage::zeroed();
	for index in 0..count {
		let page_no = checked_page_no(index, 0, "label statistics")?;
		read_page(page_no, &mut page)?;
		let next = (index + 1 < count).then(|| PageNo::new(page_no.get() + 1));
		for (raw, key, statistic) in decode_page(&page, page_no, next, dictionary)? {
			if previous.is_some_and(|prior| prior >= raw) {
				return Err(invalid_page(page_no, "label statistics are not strictly ordered"));
			}
			previous = Some(raw);
			if result.insert(key, statistic).is_some() {
				return Err(invalid_page(page_no, "duplicate label statistic"));
			}
		}
	}
	Ok(result)
}

fn encode_page(
	page_no: PageNo,
	next: Option<PageNo>,
	entries: &[EncodedEntry<'_>],
	page_lsn: PageLsn,
) -> FerrosGrynnResult<AlignedPage> {
	let count =
		u32::try_from(entries.len()).map_err(|_error| invalid("label page count exceeds u32"))?;
	let mut page =
		AlignedPage::new(PageType::LabelStatistic, PageFlags::empty(), page_lsn, page_no);
	let bytes = page.as_bytes_mut();
	write_optional_page(bytes, NEXT_PAGE, next);
	write_u32(bytes, COUNT, count);
	for (index, entry) in entries.iter().enumerate() {
		if entry.statistic.atom_count == 0 && entry.statistic.property_count != 0 {
			return Err(invalid("empty label statistic has a property count"));
		}
		let offset = ENTRIES + index * ENTRY_BYTES;
		bytes[offset] = entry.kind as u8;
		write_u32(bytes, offset + 4, entry.label_id);
		write_u64(bytes, offset + 8, entry.statistic.atom_count);
		write_u64(bytes, offset + 16, entry.statistic.property_count);
	}
	page.seal()?;
	Ok(page)
}

fn decode_page(
	page: &AlignedPage,
	page_no: PageNo,
	expected_next: Option<PageNo>,
	dictionary: &LabelDictionary,
) -> FerrosGrynnResult<Vec<((LabelKind, u32), LabelStatisticKey, LabelStatistic)>> {
	let header = page.validate(page_no)?;
	if header.page_type() != PageType::LabelStatistic || !header.flags().is_empty() {
		return Err(invalid_page(page_no, "invalid LabelStatistic page header"));
	}
	let bytes = page.as_bytes();
	if read_optional_page(bytes, NEXT_PAGE) != expected_next {
		return Err(invalid_page(page_no, "label statistics next-page link is invalid"));
	}
	validate_zero(&bytes[RESERVED..ENTRIES], page_no, "label statistics reserved bytes")?;
	let count = usize::try_from(read_u32(bytes, COUNT))
		.map_err(|_error| invalid_page(page_no, "label entry count exceeds usize"))?;
	if count > PER_PAGE {
		return Err(invalid_page(page_no, "label entry count exceeds page capacity"));
	}
	let mut result = Vec::with_capacity(count);
	for index in 0..count {
		let offset = ENTRIES + index * ENTRY_BYTES;
		let kind = match bytes[offset] {
			0 => LabelKind::Node,
			1 => LabelKind::Edge,
			_ => return Err(invalid_page(page_no, "unknown label statistics kind")),
		};
		validate_zero(&bytes[offset + 1..offset + 4], page_no, "label entry reserved bytes")?;
		let label_id = read_u32(bytes, offset + 4);
		let atom_count = read_u64(bytes, offset + 8);
		let property_count = read_u64(bytes, offset + 16);
		if atom_count == 0 && property_count != 0 {
			return Err(invalid_page(page_no, "empty label statistic has properties"));
		}
		let label = dictionary
			.name(kind, label_id)
			.ok_or_else(|| invalid_page(page_no, "label statistic id is out of range"))?;
		result.push((
			(kind, label_id),
			LabelStatisticKey {
				kind: runtime_kind(kind),
				label: label.into(),
			},
			LabelStatistic {
				atom_count,
				property_count,
			},
		));
	}
	validate_zero(
		&bytes[ENTRIES + count * ENTRY_BYTES..],
		page_no,
		"label statistics trailing bytes",
	)?;
	Ok(result)
}

const fn storage_kind(kind: AtomKind) -> LabelKind {
	match kind {
		AtomKind::Node => LabelKind::Node,
		AtomKind::Edge => LabelKind::Edge,
	}
}

const fn runtime_kind(kind: LabelKind) -> AtomKind {
	match kind {
		LabelKind::Node => AtomKind::Node,
		LabelKind::Edge => AtomKind::Edge,
	}
}

fn invalid(reason: impl Into<String>) -> FerrosGrynnError {
	FerrosGrynnError::invalid_storage_file(reason)
}
