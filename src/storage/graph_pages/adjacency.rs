#[allow(clippy::wildcard_imports, reason = "page codecs use the graph-page facade contract")]
use super::*;

/// One side of a qualified edge endpoint.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub(in crate::storage) enum EndpointSide {
	/// Source endpoint.
	Source = 0,
	/// Target endpoint.
	Target = 1,
}

/// Cross-level endpoint stored in a qualified overflow page.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::storage) struct QualifiedDiskAtomRef {
	/// Nested graph slot.
	pub(in crate::storage) graph_slot: u32,
	/// Endpoint side retained for a shared overflow chain.
	pub(in crate::storage) side: EndpointSide,
	/// Atom address within the nested graph.
	pub(in crate::storage) atom: crate::values::storage::DiskAtomRef,
}

impl QualifiedDiskAtomRef {
	/// Exact format-v1 entry width including canonical padding.
	pub(super) const ENCODED_BYTES: usize = 16;
}

/// Encodes one plain adjacency-overflow page.
pub(in crate::storage) fn encode_adjacency_overflow(
	page_no: PageNo,
	page_lsn: PageLsn,
	next_page: Option<PageNo>,
	entries: &[crate::values::storage::DiskAtomRef],
) -> FerrosGrynnResult<AlignedPage> {
	if entries.len() > OVERFLOW_REFS_PER_PAGE {
		return Err(invalid_page(page_no.get(), "adjacency overflow page is over capacity"));
	}
	let mut page =
		AlignedPage::new(PageType::AdjacencyOverflow, PageFlags::OVERFLOW, page_lsn, page_no);
	let bytes = page.as_bytes_mut();
	write_optional_page(bytes, OVERFLOW_NEXT_PAGE_OFFSET, next_page);
	write_u32(
		bytes,
		OVERFLOW_COUNT_OFFSET,
		u32::try_from(entries.len())
			.map_err(|_error| invalid_page(page_no.get(), "overflow count exceeds u32"))?,
	);
	for (index, entry) in entries.iter().enumerate() {
		write_u64(bytes, OVERFLOW_ENTRIES_OFFSET + index * size_of::<u64>(), entry.raw());
	}
	page.seal()?;
	Ok(page)
}

/// Decodes one plain adjacency-overflow page.
pub(in crate::storage) fn decode_adjacency_overflow(
	page: &AlignedPage,
	page_no: PageNo,
) -> FerrosGrynnResult<(Option<PageNo>, Vec<crate::values::storage::DiskAtomRef>)> {
	validate_overflow_header(page, page_no, PageType::AdjacencyOverflow)?;
	let bytes = page.as_bytes();
	validate_zero(
		&bytes[OVERFLOW_RESERVED_OFFSET..OVERFLOW_ENTRIES_OFFSET],
		page_no,
		"overflow padding",
	)?;
	let count = usize::try_from(read_u32(bytes, OVERFLOW_COUNT_OFFSET))
		.map_err(|_error| invalid_page(page_no.get(), "overflow count does not fit usize"))?;
	if count > OVERFLOW_REFS_PER_PAGE {
		return Err(invalid_page(page_no.get(), "adjacency overflow count exceeds capacity"));
	}
	let entries = read_required_disk_refs(bytes, OVERFLOW_ENTRIES_OFFSET, count)?;
	let end = OVERFLOW_ENTRIES_OFFSET + count * size_of::<u64>();
	validate_zero(&bytes[end..], page_no, "adjacency overflow trailing bytes")?;
	Ok((read_optional_page(bytes, OVERFLOW_NEXT_PAGE_OFFSET), entries))
}

/// Encodes one cross-level adjacency-overflow page.
pub(in crate::storage) fn encode_cross_level_overflow(
	page_no: PageNo,
	page_lsn: PageLsn,
	next_page: Option<PageNo>,
	entries: &[QualifiedDiskAtomRef],
) -> FerrosGrynnResult<AlignedPage> {
	if entries.len() > CROSS_LEVEL_REFS_PER_PAGE {
		return Err(invalid_page(page_no.get(), "cross-level overflow page is over capacity"));
	}
	let mut page = AlignedPage::new(
		PageType::CrossLevelAdjacencyOverflow,
		PageFlags::OVERFLOW,
		page_lsn,
		page_no,
	);
	let bytes = page.as_bytes_mut();
	write_optional_page(bytes, OVERFLOW_NEXT_PAGE_OFFSET, next_page);
	write_u32(
		bytes,
		OVERFLOW_COUNT_OFFSET,
		u32::try_from(entries.len())
			.map_err(|_error| invalid_page(page_no.get(), "cross-level count exceeds u32"))?,
	);
	for (index, entry) in entries.iter().enumerate() {
		let offset = OVERFLOW_ENTRIES_OFFSET + index * QualifiedDiskAtomRef::ENCODED_BYTES;
		write_u32(bytes, offset, entry.graph_slot);
		bytes[offset + size_of::<u32>()] = entry.side as u8;
		write_u64(bytes, offset + 8, entry.atom.raw());
	}
	page.seal()?;
	Ok(page)
}

/// Decodes one cross-level adjacency-overflow page.
pub(in crate::storage) fn decode_cross_level_overflow(
	page: &AlignedPage,
	page_no: PageNo,
) -> FerrosGrynnResult<(Option<PageNo>, Vec<QualifiedDiskAtomRef>)> {
	validate_overflow_header(page, page_no, PageType::CrossLevelAdjacencyOverflow)?;
	let bytes = page.as_bytes();
	validate_zero(
		&bytes[OVERFLOW_RESERVED_OFFSET..OVERFLOW_ENTRIES_OFFSET],
		page_no,
		"overflow padding",
	)?;
	let count = usize::try_from(read_u32(bytes, OVERFLOW_COUNT_OFFSET))
		.map_err(|_error| invalid_page(page_no.get(), "cross-level count does not fit usize"))?;
	if count > CROSS_LEVEL_REFS_PER_PAGE {
		return Err(invalid_page(page_no.get(), "cross-level overflow count exceeds capacity"));
	}
	let mut entries = Vec::with_capacity(count);
	for index in 0..count {
		let offset = OVERFLOW_ENTRIES_OFFSET + index * QualifiedDiskAtomRef::ENCODED_BYTES;
		let side = match bytes[offset + size_of::<u32>()] {
			0 => EndpointSide::Source,
			1 => EndpointSide::Target,
			_ => return Err(invalid_page(page_no.get(), "cross-level endpoint side is invalid")),
		};
		validate_zero(&bytes[offset + 5..offset + 8], page_no, "cross-level entry padding")?;
		entries.push(QualifiedDiskAtomRef {
			graph_slot: read_u32(bytes, offset),
			side,
			atom: crate::values::storage::DiskAtomRef::from_raw(read_u64(bytes, offset + 8))?,
		});
	}
	let end = OVERFLOW_ENTRIES_OFFSET + count * QualifiedDiskAtomRef::ENCODED_BYTES;
	validate_zero(&bytes[end..], page_no, "cross-level overflow trailing bytes")?;
	Ok((read_optional_page(bytes, OVERFLOW_NEXT_PAGE_OFFSET), entries))
}
