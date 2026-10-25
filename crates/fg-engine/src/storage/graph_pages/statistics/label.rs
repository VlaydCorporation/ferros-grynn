//! Exact label counters in an independently lazy file.

use std::collections::BTreeMap;

use super::{
	checked_page_no,
	codec::{
		invalid_page,
		read_optional_page,
		read_u32,
		read_u64,
		validate_zero,
		write_optional_page,
		write_u32,
		write_u64,
	},
};
use crate::{
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	graph::AtomKind,
	stats::{GraphStatistics, LabelStatistic, LabelStatisticKey},
	storage::{
		graph_index::{LabelDictionary, LabelKind},
		page::{AlignedPage, PAGE_SIZE, PageFlags, PageLsn, PageNo, PageType},
	},
};

/// Physical sort key paired with the logical label count restored from disk.
type LabelStatisticEntry = ((LabelKind, u32), LabelStatisticKey, LabelStatistic);

/// Expected source and accepted counters for existing namespace-local keys.
pub(in crate::storage) type LabelStatisticsChanges =
	BTreeMap<(LabelKind, u32), (LabelStatistic, LabelStatistic)>;

/// Configured next page value.
const NEXT_PAGE: usize = 32;
/// Configured count value.
const COUNT: usize = 40;
/// Configured reserved value.
const RESERVED: usize = 44;
/// Configured entries value.
const ENTRIES: usize = 48;
/// Configured entry bytes value.
const ENTRY_BYTES: usize = 24;
/// Configured per page value.
const PER_PAGE: usize = (PAGE_SIZE - ENTRIES) / ENTRY_BYTES;

#[derive(Clone, Copy)]
/// Encoded entry state.
struct EncodedEntry<'entry> {
	/// Kind associated with this encoded entry.
	kind: LabelKind,
	/// Label id associated with this encoded entry.
	label_id: u32,
	/// Statistic associated with this encoded entry.
	statistic: &'entry LabelStatistic,
}

/// Exact page count and entry-vector scratch for label encoding.
pub(super) fn encoding_size(count: usize) -> FerrosGrynnResult<(usize, usize)> {
	let scratch = count
		.checked_mul(size_of::<EncodedEntry<'_>>())
		.ok_or_else(|| invalid("label statistics scratch overflows usize"))?;
	Ok((count.div_ceil(PER_PAGE), scratch))
}

/// Encodes label statistics.
pub(super) fn encode_label_statistics(
	statistics: &GraphStatistics,
	dictionary: &LabelDictionary,
	page_lsn: PageLsn,
) -> FerrosGrynnResult<Vec<AlignedPage>> {
	let mut entries = Vec::new();
	entries
		.try_reserve_exact(statistics.labels.len())
		.map_err(|_error| invalid("label statistics entry allocation failed"))?;
	for (key, statistic) in &statistics.labels {
		let kind = storage_kind(key.kind);
		let label_id = dictionary
			.id(kind, key.label.as_str())
			.ok_or_else(|| invalid("label statistic is absent from label dictionary"))?;
		entries.push(EncodedEntry {
			kind,
			label_id,
			statistic,
		});
	}
	entries.sort_unstable_by_key(|entry| (entry.kind, entry.label_id));
	let (page_count, _) = encoding_size(entries.len())?;
	let mut pages = Vec::with_capacity(page_count);
	for (index, chunk) in entries.chunks(PER_PAGE).enumerate() {
		let page_no = checked_page_no(index, 0, "label statistics")?;
		let next = (index + 1 < page_count).then(|| PageNo::new(page_no.get().saturating_add(1)));
		pages.push(encode_page(page_no, next, chunk, page_lsn)?);
	}
	Ok(pages)
}

/// Decodes label statistics.
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
	// Sparse writers recover dense IDs from this complete logical key set.
	let expected = dictionary
		.node_labels
		.len()
		.checked_add(dictionary.edge_labels.len())
		.ok_or_else(|| invalid("label dictionary size overflows"))?;
	if result.len() != expected {
		return Err(invalid("label statistics do not cover the label dictionary"));
	}
	Ok(result)
}

/// Patches counters without changing labels, page links or entry layout.
/// Matched targets are consumed only after the whole page is validated.
///
/// # Errors
/// Rejects malformed pages, unknown IDs, invalid counters and source
/// mismatches.
pub(in crate::storage) fn rewrite_label_statistics_page(
	page: &mut AlignedPage,
	page_no: PageNo,
	expected_next: Option<PageNo>,
	label_counts: [usize; 2],
	targets: &mut LabelStatisticsChanges,
) -> FerrosGrynnResult<bool> {
	let count = validate_page(page, page_no, expected_next, label_counts)?;
	for index in 0..count {
		let offset = ENTRIES + index * ENTRY_BYTES;
		let bytes = page.as_bytes();
		let key = (read_kind(bytes[offset], page_no)?, read_u32(bytes, offset + 4));
		if let Some((old, new)) = targets.get(&key)
			&& (old.atom_count != read_u64(bytes, offset + 8)
				|| old.property_count != read_u64(bytes, offset + 16)
				|| (new.atom_count == 0 && new.property_count != 0))
		{
			return Err(invalid_page(page_no, "label counter source or replacement is invalid"));
		}
	}
	let mut changed = false;
	for index in 0..count {
		let offset = ENTRIES + index * ENTRY_BYTES;
		let bytes = page.as_bytes();
		let key = (read_kind(bytes[offset], page_no)?, read_u32(bytes, offset + 4));
		if let Some((old, new)) = targets.remove(&key)
			&& old != new
		{
			let bytes = page.as_bytes_mut();
			write_u64(bytes, offset + 8, new.atom_count);
			write_u64(bytes, offset + 16, new.property_count);
			changed = true;
		}
	}
	if changed {
		page.seal()?;
	}
	Ok(changed)
}

/// Encodes page.
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

/// Decodes page.
fn decode_page(
	page: &AlignedPage,
	page_no: PageNo,
	expected_next: Option<PageNo>,
	dictionary: &LabelDictionary,
) -> FerrosGrynnResult<Vec<LabelStatisticEntry>> {
	let count = validate_page(page, page_no, expected_next, [
		dictionary.node_labels.len(),
		dictionary.edge_labels.len(),
	])?;
	let bytes = page.as_bytes();
	let mut result = Vec::with_capacity(count);
	for index in 0..count {
		let offset = ENTRIES + index * ENTRY_BYTES;
		let kind = read_kind(bytes[offset], page_no)?;
		let label_id = read_u32(bytes, offset + 4);
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
				atom_count: read_u64(bytes, offset + 8),
				property_count: read_u64(bytes, offset + 16),
			},
		));
	}
	Ok(result)
}

/// Validates borrowed entries before either decoding or sparse mutation.
///
/// # Errors
/// Rejects invalid checksums, layout, namespace bounds and counter ordering.
fn validate_page(
	page: &AlignedPage,
	page_no: PageNo,
	expected_next: Option<PageNo>,
	label_counts: [usize; 2],
) -> FerrosGrynnResult<usize> {
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
	let mut previous = None;
	for index in 0..count {
		let offset = ENTRIES + index * ENTRY_BYTES;
		let kind = read_kind(bytes[offset], page_no)?;
		validate_zero(&bytes[offset + 1..offset + 4], page_no, "label entry reserved bytes")?;
		let label_id = read_u32(bytes, offset + 4);
		let atom_count = read_u64(bytes, offset + 8);
		let property_count = read_u64(bytes, offset + 16);
		if atom_count == 0 && property_count != 0 {
			return Err(invalid_page(page_no, "empty label statistic has properties"));
		}
		if usize::try_from(label_id).map_or(true, |id| id >= label_counts[kind as usize]) {
			return Err(invalid_page(page_no, "label statistic id is out of range"));
		}
		let key = (kind, label_id);
		if previous.is_some_and(|prior| prior >= key) {
			return Err(invalid_page(page_no, "label statistics are not strictly ordered"));
		}
		previous = Some(key);
	}
	validate_zero(
		&bytes[ENTRIES + count * ENTRY_BYTES..],
		page_no,
		"label statistics trailing bytes",
	)?;
	Ok(count)
}

/// Decodes the closed on-disk label namespace.
fn read_kind(raw: u8, page_no: PageNo) -> FerrosGrynnResult<LabelKind> {
	match raw {
		0 => Ok(LabelKind::Node),
		1 => Ok(LabelKind::Edge),
		_ => Err(invalid_page(page_no, "unknown label statistics kind")),
	}
}

/// Returns the storage kind result.
fn storage_kind(kind: AtomKind) -> LabelKind {
	fg_runtime::__internal::EnumChoice::choose(&(kind), [LabelKind::Node, LabelKind::Edge])
}

/// Returns the runtime kind result.
const fn runtime_kind(kind: LabelKind) -> AtomKind {
	match kind {
		LabelKind::Node => AtomKind::Node,
		LabelKind::Edge => AtomKind::Edge,
	}
}

/// Returns the invalid result.
fn invalid(reason: impl Into<String>) -> FerrosGrynnError {
	FerrosGrynnError::invalid_storage_file(reason)
}
