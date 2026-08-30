#[allow(clippy::wildcard_imports, reason = "page codecs use the graph-page facade contract")]
use super::*;

/// One nested-graph directory entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::storage) struct SubgraphEntry {
	pub(in crate::storage) slot: u32,
	pub(in crate::storage) generation: u32,
	pub(in crate::storage) node_capacity: u64,
	pub(in crate::storage) edge_capacity: u64,
	pub(in crate::storage) node_count: u32,
	pub(in crate::storage) edge_count: u32,
}

/// Encodes one subgraph-directory page.
pub(in crate::storage) fn encode_subgraph_directory(
	page_no: PageNo,
	page_lsn: PageLsn,
	next_page: Option<PageNo>,
	entries: &[SubgraphEntry],
) -> FerrosGrynnResult<AlignedPage> {
	if entries.len() > SUBGRAPHS_PER_PAGE {
		return Err(invalid_page(page_no.get(), "subgraph directory page is over capacity"));
	}
	let mut page = AlignedPage::new(PageType::SubgraphDir, PageFlags::empty(), page_lsn, page_no);
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
	page.seal()?;
	Ok(page)
}

/// Decodes one subgraph-directory page.
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
	if count > SUBGRAPHS_PER_PAGE || live > count {
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
