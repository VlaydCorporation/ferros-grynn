#[allow(clippy::wildcard_imports, reason = "page codecs use the graph-page facade contract")]
use super::*;

/// Creates one freelist page with an explicit covered range and bitmap.
pub(in crate::storage) fn encode_freelist(
	page_no: PageNo,
	page_lsn: PageLsn,
	first_page_covered: u64,
	next_page: Option<PageNo>,
	bitmap: &[u8],
) -> FerrosGrynnResult<AlignedPage> {
	if bitmap.len() > PAGE_SIZE - FREELIST_BITMAP_OFFSET {
		return Err(invalid_page(page_no.get(), "freelist bitmap is over capacity"));
	}
	let mut page = AlignedPage::new(PageType::Freelist, PageFlags::empty(), page_lsn, page_no);
	let bytes = page.as_bytes_mut();
	write_u64(bytes, FREELIST_FIRST_PAGE_OFFSET, first_page_covered);
	write_optional_page(bytes, FREELIST_NEXT_PAGE_OFFSET, next_page);
	bytes[FREELIST_BITMAP_OFFSET..FREELIST_BITMAP_OFFSET + bitmap.len()].copy_from_slice(bitmap);
	page.seal()?;
	Ok(page)
}

/// Validates and returns the complete fixed-size freelist bitmap.
pub(in crate::storage) fn decode_freelist(
	page: &AlignedPage,
	page_no: PageNo,
) -> FerrosGrynnResult<(u64, Option<PageNo>, &[u8])> {
	let header = page.validate(page_no)?;
	if header.page_type() != PageType::Freelist || !header.flags().is_empty() {
		return Err(invalid_page(page_no.get(), "invalid Freelist page header"));
	}
	let bytes = page.as_bytes();
	Ok((
		read_u64(bytes, FREELIST_FIRST_PAGE_OFFSET),
		read_optional_page(bytes, FREELIST_NEXT_PAGE_OFFSET),
		&bytes[FREELIST_BITMAP_OFFSET..],
	))
}
