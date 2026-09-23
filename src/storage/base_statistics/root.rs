//! Eager metadata page for database-global Base statistics.

use crate::{
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	stats::BaseStatistics,
	storage::page::{AlignedPage, PAGE_SIZE, PageFlags, PageLsn, PageNo, PageType},
};

const LAYOUT_VERSION: u32 = 1;
const REVISION: usize = 32;
const ANALYZED_REVISION: usize = 40;
const DIRTY_ROWS: usize = 48;
const CLASS_FIRST: usize = 56;
const PROPERTY_FIRST: usize = 64;
const CLASS_PAGES: usize = 72;
const PROPERTY_PAGES: usize = 76;
const VERSION: usize = 80;
const RESERVED: usize = 84;

/// Exact page ranges declared by the Base statistics root.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(in crate::storage) struct BaseStatisticsLayout {
	pub(super) class_first: Option<PageNo>,
	pub(super) property_first: Option<PageNo>,
	pub(super) class_pages: u32,
	pub(super) property_pages: u32,
}

impl BaseStatisticsLayout {
	pub(super) fn from_counts(classes: usize, properties: usize) -> FerrosGrynnResult<Self> {
		let class_pages = count(classes, "class")?;
		let property_pages = count(properties, "property")?;
		Ok(Self {
			class_first: (class_pages != 0).then(|| PageNo::new(1)),
			property_first: (property_pages != 0).then(|| PageNo::new(0)),
			class_pages,
			property_pages,
		})
	}

	pub(super) const fn class_file_len(self) -> u64 {
		1 + self.class_pages as u64
	}

	pub(super) const fn property_file_len(self) -> u64 {
		self.property_pages as u64
	}
}

pub(super) fn encode_base_statistics_root(
	page: &mut AlignedPage,
	statistics: &BaseStatistics,
	layout: BaseStatisticsLayout,
	page_lsn: PageLsn,
) -> FerrosGrynnResult<()> {
	validate_metadata(statistics)?;
	validate_starts(layout)?;
	page.reset(PageType::StatisticsRoot, PageFlags::empty(), page_lsn, PageNo::new(0));
	let bytes = page.as_bytes_mut();
	write_u64(bytes, REVISION, statistics.revision);
	write_u64(bytes, ANALYZED_REVISION, statistics.analyzed_revision);
	write_u64(bytes, DIRTY_ROWS, statistics.dirty_rows);
	write_optional_page(bytes, CLASS_FIRST, layout.class_first);
	write_optional_page(bytes, PROPERTY_FIRST, layout.property_first);
	write_u32(bytes, CLASS_PAGES, layout.class_pages);
	write_u32(bytes, PROPERTY_PAGES, layout.property_pages);
	write_u32(bytes, VERSION, LAYOUT_VERSION);
	page.seal()
}

/// Decodes the eager Base root and validates both physical file lengths.
pub(in crate::storage) fn decode_base_statistics_root(
	page: &AlignedPage,
	class_file_pages: u64,
	property_file_pages: u64,
) -> FerrosGrynnResult<(BaseStatistics, BaseStatisticsLayout)> {
	let page_no = PageNo::new(0);
	let header = page.validate(page_no)?;
	if header.page_type() != PageType::StatisticsRoot || !header.flags().is_empty() {
		return Err(invalid_page("invalid Base StatisticsRoot page header"));
	}
	let bytes = page.as_bytes();
	if read_u32(bytes, VERSION) != LAYOUT_VERSION {
		return Err(FerrosGrynnError::unsupported_storage_format(
			u64::from(read_u32(bytes, VERSION)),
			u64::from(LAYOUT_VERSION),
		));
	}
	if bytes[RESERVED..PAGE_SIZE].iter().any(|byte| *byte != 0) {
		return Err(invalid_page("Base statistics reserved bytes are non-zero"));
	}
	let statistics = BaseStatistics {
		revision: read_u64(bytes, REVISION),
		analyzed_revision: read_u64(bytes, ANALYZED_REVISION),
		dirty_rows: read_u64(bytes, DIRTY_ROWS),
		..BaseStatistics::default()
	};
	validate_metadata(&statistics)?;
	let layout = BaseStatisticsLayout {
		class_first: read_optional_page(bytes, CLASS_FIRST),
		property_first: read_optional_page(bytes, PROPERTY_FIRST),
		class_pages: read_u32(bytes, CLASS_PAGES),
		property_pages: read_u32(bytes, PROPERTY_PAGES),
	};
	validate_starts(layout)?;
	if layout.class_file_len() != class_file_pages
		|| layout.property_file_len() != property_file_pages
	{
		return Err(invalid("Base statistics directory disagrees with physical file lengths"));
	}
	Ok((statistics, layout))
}

fn validate_metadata(statistics: &BaseStatistics) -> FerrosGrynnResult<()> {
	if statistics.analyzed_revision > statistics.revision {
		return Err(invalid("Base statistics analysis revision exceeds exact revision"));
	}
	Ok(())
}

fn validate_starts(layout: BaseStatisticsLayout) -> FerrosGrynnResult<()> {
	for (first, pages, expected, name) in [
		(layout.class_first, layout.class_pages, 1, "class"),
		(layout.property_first, layout.property_pages, 0, "property"),
	] {
		match (first, pages) {
			(None, 0) => {}
			(Some(page), count) if count != 0 && page.get() == expected => {}
			_ => return Err(invalid(format!("Base statistics {name} range start is invalid"))),
		}
	}
	Ok(())
}

fn count(value: usize, family: &str) -> FerrosGrynnResult<u32> {
	u32::try_from(value)
		.map_err(|_error| invalid(format!("Base {family} statistics exceed u32 pages")))
}

fn write_optional_page(bytes: &mut [u8], offset: usize, page: Option<PageNo>) {
	write_u64(bytes, offset, page.unwrap_or(PageNo::NULL).get());
}

fn read_optional_page(bytes: &[u8], offset: usize) -> Option<PageNo> {
	let page = PageNo::new(read_u64(bytes, offset));
	(page != PageNo::NULL).then_some(page)
}

fn write_u32(bytes: &mut [u8], offset: usize, value: u32) {
	bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

fn write_u64(bytes: &mut [u8], offset: usize, value: u64) {
	bytes[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
}

fn read_u32(bytes: &[u8], offset: usize) -> u32 {
	let mut value = [0; 4];
	value.copy_from_slice(&bytes[offset..offset + 4]);
	u32::from_le_bytes(value)
}

fn read_u64(bytes: &[u8], offset: usize) -> u64 {
	let mut value = [0; 8];
	value.copy_from_slice(&bytes[offset..offset + 8]);
	u64::from_le_bytes(value)
}

fn invalid_page(reason: impl Into<String>) -> FerrosGrynnError {
	FerrosGrynnError::invalid_storage_page(0, reason)
}

fn invalid(reason: impl Into<String>) -> FerrosGrynnError {
	FerrosGrynnError::invalid_storage_file(reason)
}
