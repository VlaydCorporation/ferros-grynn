#[allow(clippy::wildcard_imports, reason = "index layers use the graph-index facade contract")]
use super::*;
use crate::values::runtime::graph::AtomKind;

const NEXT_PAGE_OFFSET: usize = PAGE_HEADER_SIZE;
const COUNT_OFFSET: usize = NEXT_PAGE_OFFSET + size_of::<u64>();
const RESERVED_OFFSET: usize = COUNT_OFFSET + size_of::<u32>();
const ATOMS_OFFSET: usize = RESERVED_OFFSET + size_of::<u32>();
const ATOMS_PER_PAGE: usize = (PAGE_SIZE - ATOMS_OFFSET) / size_of::<u64>();

/// Packs one exact label membership into immutable sorted-array pages.
pub(in crate::storage) fn build_label_index(
	atoms: &[DiskAtomRef],
	kind: AtomKind,
	page_lsn: PageLsn,
) -> FerrosGrynnResult<Vec<AlignedPage>> {
	if atoms.iter().any(|atom| atom.kind() != kind)
		|| atoms.windows(2).any(|pair| pair[0].slot() >= pair[1].slot())
	{
		return Err(invalid_index(
			"label index atoms have the wrong kind or are not strictly ordered",
		));
	}
	let page_count = atoms.len().max(1).div_ceil(ATOMS_PER_PAGE);
	let mut pages = Vec::with_capacity(page_count);
	for index in 0..page_count {
		let page_no = PageNo::new(
			u64::try_from(index)
				.map_err(|_error| invalid_index("label-index page number exceeds u64"))?,
		);
		let start = index
			.checked_mul(ATOMS_PER_PAGE)
			.ok_or_else(|| invalid_index("label-index page offset overflows usize"))?;
		let end = start.saturating_add(ATOMS_PER_PAGE).min(atoms.len());
		let chunk = &atoms[start..end];
		let mut page =
			AlignedPage::new(PageType::IndexLabelArray, PageFlags::empty(), page_lsn, page_no);
		let next = (index + 1 < page_count).then(|| PageNo::new(page_no.get() + 1));
		write_u64(page.as_bytes_mut(), NEXT_PAGE_OFFSET, next.unwrap_or(PageNo::NULL).get());
		write_u32(
			page.as_bytes_mut(),
			COUNT_OFFSET,
			u32::try_from(chunk.len())
				.map_err(|_error| invalid_index("label-index page count exceeds u32"))?,
		);
		for (offset, atom) in chunk.iter().enumerate() {
			write_u64(page.as_bytes_mut(), ATOMS_OFFSET + offset * size_of::<u64>(), atom.raw());
		}
		page.seal()?;
		pages.push(page);
	}
	Ok(pages)
}

/// Visits one exact label membership in ascending slot order.
pub(in crate::storage) fn visit_label_index_pages(
	mut read_page: impl FnMut(PageNo, &mut AlignedPage) -> FerrosGrynnResult<()>,
	kind: AtomKind,
	mut visit: impl FnMut(DiskAtomRef) -> FerrosGrynnResult<()>,
) -> FerrosGrynnResult<()> {
	let mut page_no = PageNo::new(0);
	let mut previous = None::<DiskAtomRef>;
	loop {
		let mut page = AlignedPage::zeroed();
		read_page(page_no, &mut page)?;
		let header = page.validate(page_no)?;
		let bytes = page.as_bytes();
		if header.page_type() != PageType::IndexLabelArray
			|| !header.flags().is_empty()
			|| read_u32(bytes, RESERVED_OFFSET) != 0
		{
			return Err(invalid_index("invalid label-index page header or padding"));
		}
		let count = usize::try_from(read_u32(bytes, COUNT_OFFSET))
			.map_err(|_error| invalid_index("label-index page count does not fit usize"))?;
		if count > ATOMS_PER_PAGE {
			return Err(invalid_index("label-index page count exceeds its capacity"));
		}
		for index in 0..count {
			let atom = DiskAtomRef::from_raw(read_u64(bytes, ATOMS_OFFSET + index * 8))?;
			if atom.kind() != kind
				|| previous.is_some_and(|previous| previous.slot() >= atom.slot())
			{
				return Err(invalid_index(
					"label index atoms have the wrong kind or are not strictly ordered",
				));
			}
			previous = Some(atom);
			visit(atom)?;
		}
		validate_zero(
			&bytes[ATOMS_OFFSET + count * size_of::<u64>()..],
			"label-index trailing bytes",
		)?;
		let next = PageNo::new(read_u64(bytes, NEXT_PAGE_OFFSET));
		if next == PageNo::NULL {
			return Ok(());
		}
		let expected = page_no
			.get()
			.checked_add(1)
			.ok_or_else(|| invalid_index("label-index page chain overflows u64"))?;
		if count != ATOMS_PER_PAGE || next.get() != expected {
			return Err(invalid_index("label-index page chain is not densely packed"));
		}
		page_no = next;
	}
}
