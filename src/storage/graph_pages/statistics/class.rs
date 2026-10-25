//! Exact graph-local class counters after the degree range.

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
	datastore::ClassId,
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	storage::page::{AlignedPage, PAGE_SIZE, PageFlags, PageLsn, PageNo, PageType},
};

/// Configured next page value.
const NEXT_PAGE: usize = 32;
/// Configured count value.
const COUNT: usize = 40;
/// Configured reserved value.
const RESERVED: usize = 44;
/// Configured entries value.
const ENTRIES: usize = 48;
/// Configured entry bytes value.
const ENTRY_BYTES: usize = 16;
/// Configured per page value.
const PER_PAGE: usize = (PAGE_SIZE - ENTRIES) / ENTRY_BYTES;

/// Exact page count and borrowed-entry scratch used by the class encoder.
pub(super) fn encoding_size(count: usize) -> FerrosGrynnResult<(usize, usize)> {
	let scratch = count
		.checked_mul(size_of::<(&ClassId, &u64)>())
		.ok_or_else(|| invalid("class statistics scratch overflows usize"))?;
	Ok((count.div_ceil(PER_PAGE), scratch))
}

/// Encodes class statistics.
pub(in crate::storage) fn encode_class_statistics(
	classes: &BTreeMap<ClassId, u64>,
	first_page: u64,
	page_lsn: PageLsn,
) -> FerrosGrynnResult<Vec<AlignedPage>> {
	let entries = classes.iter().collect::<Vec<_>>();
	let (page_count, _) = encoding_size(entries.len())?;
	let mut pages = Vec::with_capacity(page_count);
	for (index, chunk) in entries.chunks(PER_PAGE).enumerate() {
		let page_no = checked_page_no(index, first_page, "class statistics")?;
		let next = if index + 1 < page_count {
			Some(checked_page_no(index + 1, first_page, "class statistics")?)
		} else {
			None
		};
		pages.push(encode_page(page_no, next, chunk, page_lsn)?);
	}
	Ok(pages)
}

/// Decodes class statistics.
pub(in crate::storage) fn decode_class_statistics<ReadPage>(
	read_page: ReadPage,
	layout: super::StatisticsLayout,
) -> FerrosGrynnResult<BTreeMap<ClassId, u64>>
where
	ReadPage: FnMut(PageNo, &mut AlignedPage) -> FerrosGrynnResult<()>,
{
	decode_class_statistics_range(read_page, layout.class_first, layout.class_pages)
}

/// Decodes class statistics range.
pub(in crate::storage) fn decode_class_statistics_range<ReadPage>(
	read_page: ReadPage,
	first: Option<PageNo>,
	page_count: u32,
) -> FerrosGrynnResult<BTreeMap<ClassId, u64>>
where
	ReadPage: FnMut(PageNo, &mut AlignedPage) -> FerrosGrynnResult<()>,
{
	decode_class_range_bounded(read_page, first, page_count, usize::MAX)
}

/// Decodes source classes under the owning schema's admitted cardinality.
///
/// # Errors
/// Rejects excessive page/entry counts before map growth and all codec errors.
pub(in crate::storage) fn decode_class_statistics_bounded<ReadPage>(
	read_page: ReadPage,
	layout: super::StatisticsLayout,
	maximum: usize,
) -> FerrosGrynnResult<BTreeMap<ClassId, u64>>
where
	ReadPage: FnMut(PageNo, &mut AlignedPage) -> FerrosGrynnResult<()>,
{
	decode_class_range_bounded(read_page, layout.class_first, layout.class_pages, maximum)
}

/// Keeps the legacy reader and schema-bounded source reader on one validator.
fn decode_class_range_bounded<ReadPage>(
	mut read_page: ReadPage,
	first: Option<PageNo>,
	page_count: u32,
	maximum: usize,
) -> FerrosGrynnResult<BTreeMap<ClassId, u64>>
where
	ReadPage: FnMut(PageNo, &mut AlignedPage) -> FerrosGrynnResult<()>,
{
	let count = usize::try_from(page_count)
		.map_err(|_error| invalid("class statistics page count exceeds usize"))?;
	if count > maximum.div_ceil(PER_PAGE) {
		return Err(invalid("class statistics pages exceed the owning schema bound"));
	}
	let Some(first) = first else {
		if count != 0 {
			return Err(invalid("class statistics range has pages without a start"));
		}
		return Ok(BTreeMap::new());
	};
	if count == 0 {
		return Err(invalid("class statistics range has a start without pages"));
	}
	let mut result = BTreeMap::new();
	let mut previous = None;
	let mut page = AlignedPage::zeroed();
	for index in 0..count {
		let page_no = checked_page_no(index, first.get(), "class statistics")?;
		read_page(page_no, &mut page)?;
		let next = if index + 1 < count {
			Some(checked_page_no(index + 1, first.get(), "class statistics")?)
		} else {
			None
		};
		for (class, atom_count) in decode_page(&page, page_no, next)? {
			if previous.is_some_and(|prior| prior >= class) {
				return Err(invalid_page(page_no, "class statistics are not strictly ordered"));
			}
			previous = Some(class);
			if result.len() >= maximum {
				return Err(invalid("class statistics entries exceed the owning schema bound"));
			}
			if result.insert(class, atom_count).is_some() {
				return Err(invalid_page(page_no, "duplicate class statistic"));
			}
		}
	}
	Ok(result)
}

/// Patches accepted class populations without changing keys or page layout.
/// Matched targets are removed so the caller can reject missing classes.
///
/// # Errors
/// Rejects malformed pages and unordered or duplicate class keys.
pub(in crate::storage) fn rewrite_class_statistics_page(
	page: &mut AlignedPage,
	page_no: PageNo,
	expected_next: Option<PageNo>,
	targets: &mut BTreeMap<ClassId, u64>,
) -> FerrosGrynnResult<bool> {
	let entries = decode_page(page, page_no, expected_next)?;
	if entries.windows(2).any(|pair| pair[0].0 >= pair[1].0) {
		return Err(invalid_page(page_no, "class statistics are not strictly ordered"));
	}
	let mut changed = false;
	for (index, (class, previous)) in entries.into_iter().enumerate() {
		if let Some(count) = targets.remove(&class)
			&& count != previous
		{
			write_u64(page.as_bytes_mut(), ENTRIES + index * ENTRY_BYTES + 8, count);
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
	entries: &[(&ClassId, &u64)],
	page_lsn: PageLsn,
) -> FerrosGrynnResult<AlignedPage> {
	let mut page =
		AlignedPage::new(PageType::ClassStatistic, PageFlags::empty(), page_lsn, page_no);
	let bytes = page.as_bytes_mut();
	write_optional_page(bytes, NEXT_PAGE, next);
	write_u32(
		bytes,
		COUNT,
		u32::try_from(entries.len()).map_err(|_error| invalid("class page count exceeds u32"))?,
	);
	for (index, (class, atom_count)) in entries.iter().enumerate() {
		let offset = ENTRIES + index * ENTRY_BYTES;
		write_u32(bytes, offset, class.get());
		write_u64(bytes, offset + 8, **atom_count);
	}
	page.seal()?;
	Ok(page)
}

/// Decodes page.
fn decode_page(
	page: &AlignedPage,
	page_no: PageNo,
	expected_next: Option<PageNo>,
) -> FerrosGrynnResult<Vec<(ClassId, u64)>> {
	let header = page.validate(page_no)?;
	if header.page_type() != PageType::ClassStatistic || !header.flags().is_empty() {
		return Err(invalid_page(page_no, "invalid ClassStatistic page header"));
	}
	let bytes = page.as_bytes();
	if read_optional_page(bytes, NEXT_PAGE) != expected_next {
		return Err(invalid_page(page_no, "class statistics next-page link is invalid"));
	}
	validate_zero(&bytes[RESERVED..ENTRIES], page_no, "class statistics reserved bytes")?;
	let count = usize::try_from(read_u32(bytes, COUNT))
		.map_err(|_error| invalid_page(page_no, "class entry count exceeds usize"))?;
	if count > PER_PAGE {
		return Err(invalid_page(page_no, "class entry count exceeds page capacity"));
	}
	let mut result = Vec::with_capacity(count);
	for index in 0..count {
		let offset = ENTRIES + index * ENTRY_BYTES;
		validate_zero(&bytes[offset + 4..offset + 8], page_no, "class entry reserved bytes")?;
		result.push((ClassId::from_storage(read_u32(bytes, offset))?, read_u64(bytes, offset + 8)));
	}
	validate_zero(
		&bytes[ENTRIES + count * ENTRY_BYTES..],
		page_no,
		"class statistics trailing bytes",
	)?;
	Ok(result)
}

/// Returns the invalid result.
fn invalid(reason: impl Into<String>) -> FerrosGrynnError {
	FerrosGrynnError::invalid_storage_file(reason)
}
