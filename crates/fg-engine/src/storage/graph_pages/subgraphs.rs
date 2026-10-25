#[allow(clippy::wildcard_imports, reason = "page codecs use the graph-page facade contract")]
use super::*;

/// One nested-graph directory entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::storage) struct SubgraphEntry {
	/// Slot associated with this subgraph entry.
	pub(in crate::storage) slot: u32,
	/// Generation identifying this version of the data.
	pub(in crate::storage) generation: u32,
	/// Node capacity associated with this subgraph entry.
	pub(in crate::storage) node_capacity: u64,
	/// Edge capacity associated with this subgraph entry.
	pub(in crate::storage) edge_capacity: u64,
	/// Node count associated with this subgraph entry.
	pub(in crate::storage) node_count: u32,
	/// Edge count associated with this subgraph entry.
	pub(in crate::storage) edge_count: u32,
}

/// Encodes one subgraph-directory page.
pub(in crate::storage) fn encode_subgraph_directory(
	page_no: PageNo,
	page_lsn: PageLsn,
	next_page: Option<PageNo>,
	entries: &[SubgraphEntry],
) -> FerrosGrynnResult<AlignedPage> {
	let mut page = AlignedPage::zeroed();
	encode_subgraph_directory_into(&mut page, page_no, page_lsn, next_page, entries)?;
	Ok(page)
}

/// Fills final heap-backed storage without returning a populated page by value.
///
/// # Errors
/// Rejects over-capacity input before resetting the destination, and preserves
/// count and page-sealing failures.
pub(in crate::storage) fn encode_subgraph_directory_into(
	page: &mut AlignedPage,
	page_no: PageNo,
	page_lsn: PageLsn,
	next_page: Option<PageNo>,
	entries: &[SubgraphEntry],
) -> FerrosGrynnResult<()> {
	if entries.len() > SUBGRAPHS_PER_PAGE {
		return Err(invalid_page(page_no.get(), "subgraph directory page is over capacity"));
	}
	page.reset(PageType::SubgraphDir, PageFlags::empty(), page_lsn, page_no);
	let bytes = page.as_bytes_mut();
	write_optional_page(bytes, SUBGRAPH_NEXT_PAGE_OFFSET, next_page);
	write_u32(
		bytes,
		SUBGRAPH_ENTRY_COUNT_OFFSET,
		u32::try_from(entries.len())
			.map_err(|_error| invalid_page(page_no.get(), "subgraph count exceeds u32"))?,
	);
	write_u32(
		bytes,
		SUBGRAPH_LIVE_COUNT_OFFSET,
		u32::try_from(entries.len())
			.map_err(|_error| invalid_page(page_no.get(), "live subgraph count exceeds u32"))?,
	);
	for (index, entry) in entries.iter().enumerate() {
		let offset = SUBGRAPH_ENTRIES_OFFSET + index * SUBGRAPH_ENTRY_BYTES;
		write_u32(bytes, offset, entry.slot);
		write_u32(bytes, offset + 4, entry.generation);
		write_u64(bytes, offset + 8, entry.node_capacity);
		write_u64(bytes, offset + 16, entry.edge_capacity);
		write_u32(bytes, offset + 24, entry.node_count);
		write_u32(bytes, offset + 28, entry.edge_count);
	}
	page.seal()
}

/// Decodes one subgraph-directory page.
///
/// # Errors
/// Rejects malformed framing, padding, lengths and inconsistent live counts
/// before allocating entries.
pub(in crate::storage) fn decode_subgraph_directory(
	page: &AlignedPage,
	page_no: PageNo,
) -> FerrosGrynnResult<(Option<PageNo>, Vec<SubgraphEntry>)> {
	let header = page.validate(page_no)?;
	if header.page_type() != PageType::SubgraphDir || !header.flags().is_empty() {
		return Err(invalid_page(page_no.get(), "invalid SubgraphDir page header"));
	}
	let bytes = page.as_bytes();
	validate_zero(
		&bytes[SUBGRAPH_RESERVED_OFFSET..SUBGRAPH_ENTRIES_OFFSET],
		page_no,
		"subgraph padding",
	)?;
	let count = usize::try_from(read_u32(bytes, SUBGRAPH_ENTRY_COUNT_OFFSET))
		.map_err(|_error| invalid_page(page_no.get(), "subgraph count does not fit usize"))?;
	let live = usize::try_from(read_u32(bytes, SUBGRAPH_LIVE_COUNT_OFFSET))
		.map_err(|_error| invalid_page(page_no.get(), "live subgraph count does not fit usize"))?;
	// Every format-v1 entry maps a live container; there is no tombstone
	// encoding.
	if count > SUBGRAPHS_PER_PAGE || live != count {
		return Err(invalid_page(page_no.get(), "subgraph directory counts are invalid"));
	}
	let mut entries = Vec::with_capacity(count);
	for index in 0..count {
		let offset = SUBGRAPH_ENTRIES_OFFSET + index * SUBGRAPH_ENTRY_BYTES;
		entries.push(SubgraphEntry {
			slot: read_u32(bytes, offset),
			generation: read_u32(bytes, offset + 4),
			node_capacity: read_u64(bytes, offset + 8),
			edge_capacity: read_u64(bytes, offset + 16),
			node_count: read_u32(bytes, offset + 24),
			edge_count: read_u32(bytes, offset + 28),
		});
	}
	let end = SUBGRAPH_ENTRIES_OFFSET + count * SUBGRAPH_ENTRY_BYTES;
	validate_zero(&bytes[end..], page_no, "subgraph directory trailing bytes")?;
	Ok((read_optional_page(bytes, SUBGRAPH_NEXT_PAGE_OFFSET), entries))
}

#[cfg(test)]
/// Destination stability and exact codec compatibility at the in-place
/// boundary.
mod tests {
	use super::*;
	use crate::test_assertions::{AssertOption as _, AssertResult as _};

	/// Capacity failure preserves the destination; valid entries keep v1 bytes.
	#[test]
	fn subgraph_in_place_encoding_preserves_bytes_and_rejects_before_reset() {
		let entry = SubgraphEntry {
			slot: 2,
			generation: 3,
			node_capacity: 7,
			edge_capacity: 5,
			node_count: 4,
			edge_count: 2,
		};
		let number = PageNo::new(9);
		let mut page =
			AlignedPage::new(PageType::NodeHot, PageFlags::empty(), PageLsn::ZERO, number);
		let before = page.as_bytes().to_vec();
		let error = encode_subgraph_directory_into(&mut page, number, PageLsn::ZERO, None, &vec![
				entry;
				SUBGRAPHS_PER_PAGE
					+ 1
			])
		.err()
		.assert_some();
		assert!(matches!(
			error.status().status(),
			crate::gql_status::Status::InvalidStoragePage { .. }
		));
		assert_eq!(page.as_bytes().as_slice(), before);
		encode_subgraph_directory_into(
			&mut page,
			number,
			PageLsn::new(11),
			Some(PageNo::new(4)),
			&[entry],
		)
		.assert_ok();
		let legacy =
			encode_subgraph_directory(number, PageLsn::new(11), Some(PageNo::new(4)), &[entry])
				.assert_ok();
		assert_eq!(page.as_bytes(), legacy.as_bytes());
		assert_eq!(
			decode_subgraph_directory(&page, number).assert_ok(),
			(Some(PageNo::new(4)), vec![entry])
		);
		assert_eq!(&page.as_bytes()[SUBGRAPH_ENTRIES_OFFSET..SUBGRAPH_ENTRIES_OFFSET + 8], &[
			2, 0, 0, 0, 3, 0, 0, 0
		]);
	}
}
