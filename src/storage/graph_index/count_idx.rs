#[allow(clippy::wildcard_imports, reason = "index layers use the graph-index facade contract")]
use super::*;

const TABLE_ID_OFFSET: usize = PAGE_HEADER_SIZE;
const RESERVED_OFFSET: usize = TABLE_ID_OFFSET + size_of::<u32>();
const ATOM_COUNT_OFFSET: usize = RESERVED_OFFSET + size_of::<u32>();
const LAST_UPDATED_LSN_OFFSET: usize = ATOM_COUNT_OFFSET + size_of::<u64>();
const CREATED_AT_LSN_OFFSET: usize = LAST_UPDATED_LSN_OFFSET + size_of::<u64>();
const PAYLOAD_END: usize = CREATED_AT_LSN_OFFSET + size_of::<u64>();

/// Complete logical value stored in one count-index page.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::storage) struct CountIndexValue {
	/// Stable table schema identity.
	pub(in crate::storage) table_id: u32,
	/// Exact number of live table members.
	pub(in crate::storage) atom_count: u64,
	/// WAL position of the latest count update, or zero before publication.
	pub(in crate::storage) last_updated_lsn: u64,
	/// WAL position that created this index, or zero before publication.
	pub(in crate::storage) created_at_lsn: u64,
}

/// Encodes one complete count-index generation.
pub(in crate::storage) fn encode_count_index(
	value: CountIndexValue,
	page_lsn: PageLsn,
) -> FerrosGrynnResult<AlignedPage> {
	if value.table_id == 0 || value.last_updated_lsn < value.created_at_lsn {
		return Err(invalid_index("count index contains invalid identity or LSN order"));
	}
	let mut page =
		AlignedPage::new(PageType::IndexCountSingle, PageFlags::empty(), page_lsn, PageNo::new(0));
	let bytes = page.as_bytes_mut();
	write_u32(bytes, TABLE_ID_OFFSET, value.table_id);
	write_u64(bytes, ATOM_COUNT_OFFSET, value.atom_count);
	write_u64(bytes, LAST_UPDATED_LSN_OFFSET, value.last_updated_lsn);
	write_u64(bytes, CREATED_AT_LSN_OFFSET, value.created_at_lsn);
	page.seal()?;
	Ok(page)
}

/// Decodes and validates one complete count-index generation.
pub(in crate::storage) fn decode_count_index(
	page: &AlignedPage,
) -> FerrosGrynnResult<CountIndexValue> {
	let header = page.validate(PageNo::new(0))?;
	let bytes = page.as_bytes();
	if header.page_type() != PageType::IndexCountSingle
		|| !header.flags().is_empty()
		|| read_u32(bytes, RESERVED_OFFSET) != 0
	{
		return Err(invalid_index("invalid count-index page header or padding"));
	}
	let value = CountIndexValue {
		table_id: read_u32(bytes, TABLE_ID_OFFSET),
		atom_count: read_u64(bytes, ATOM_COUNT_OFFSET),
		last_updated_lsn: read_u64(bytes, LAST_UPDATED_LSN_OFFSET),
		created_at_lsn: read_u64(bytes, CREATED_AT_LSN_OFFSET),
	};
	if value.table_id == 0 || value.last_updated_lsn < value.created_at_lsn {
		return Err(invalid_index("count index contains invalid identity or LSN order"));
	}
	validate_zero(&bytes[PAYLOAD_END..], "count-index trailing bytes")?;
	Ok(value)
}

/// Stamps the payload clock at the same LSN assigned to its WAL page frame.
pub(in crate::storage) fn stamp_count_index_lsn(
	page: &mut AlignedPage,
	frame_lsn: u64,
) -> FerrosGrynnResult<()> {
	let current = decode_count_index(page)?;
	if frame_lsn == 0 || frame_lsn < current.created_at_lsn {
		return Err(invalid_index("Count index update LSN is invalid"));
	}
	write_u64(page.as_bytes_mut(), LAST_UPDATED_LSN_OFFSET, frame_lsn);
	page.seal()
}
