//! Reusable page builders for hot files and overflow chains.

use super::super::*;
use crate::values::storage::DiskAtomRef;

#[derive(Default)]
pub(super) struct OverflowBuilder {
	pages: Vec<AlignedPage>,
}

impl OverflowBuilder {
	pub(super) fn append(
		&mut self,
		entries: &[DiskAtomRef],
		page_lsn: PageLsn,
	) -> FerrosGrynnResult<Option<PageNo>> {
		if entries.is_empty() {
			return Ok(None);
		}
		let first = page_no(self.pages.len(), "adjacency")?;
		for (index, chunk) in entries.chunks(OVERFLOW_REFS_PER_PAGE).enumerate() {
			let current = page_no(self.pages.len(), "adjacency")?;
			let next = (index + 1 < entries.len().div_ceil(OVERFLOW_REFS_PER_PAGE))
				.then(|| PageNo::new(current.get() + 1));
			self.pages.push(encode_adjacency_overflow(current, page_lsn, next, chunk)?);
		}
		Ok(Some(first))
	}

	pub(super) fn finish(self) -> Vec<AlignedPage> {
		self.pages
	}
}

#[derive(Default)]
pub(super) struct QualifiedOverflowBuilder {
	pages: Vec<AlignedPage>,
}

impl QualifiedOverflowBuilder {
	pub(super) fn append(
		&mut self,
		entries: &[super::topology::EncodedEndpoint],
		side: EndpointSide,
		page_lsn: PageLsn,
	) -> FerrosGrynnResult<Option<PageNo>> {
		if entries.is_empty() {
			return Ok(None);
		}
		let first = page_no(self.pages.len(), "cross-level adjacency")?;
		let encoded = entries
			.iter()
			.map(|entry| QualifiedDiskAtomRef {
				graph_slot: entry.graph_slot,
				side,
				atom: entry.atom,
			})
			.collect::<Vec<_>>();
		let chunks = encoded.len().div_ceil(CROSS_LEVEL_REFS_PER_PAGE);
		for (index, chunk) in encoded.chunks(CROSS_LEVEL_REFS_PER_PAGE).enumerate() {
			let current = page_no(self.pages.len(), "cross-level adjacency")?;
			let next = (index + 1 < chunks).then(|| PageNo::new(current.get() + 1));
			self.pages.push(encode_cross_level_overflow(current, page_lsn, next, chunk)?);
		}
		Ok(Some(first))
	}

	pub(super) fn finish(self) -> Vec<AlignedPage> {
		self.pages
	}
}

pub(super) fn inline_refs<const N: usize>(entries: &[DiskAtomRef]) -> [Option<DiskAtomRef>; N] {
	let mut result = [None; N];
	for (destination, source) in result.iter_mut().zip(entries) {
		*destination = Some(*source);
	}
	result
}

pub(super) fn subgraph_pages(
	entries: &[SubgraphEntry],
	page_lsn: PageLsn,
) -> FerrosGrynnResult<Vec<AlignedPage>> {
	let chunks = entries.is_empty().then_some(&[][..]).into_iter().chain(
		(!entries.is_empty()).then(|| entries.chunks(SUBGRAPHS_PER_PAGE)).into_iter().flatten(),
	);
	let chunks = chunks.collect::<Vec<_>>();
	let mut pages = Vec::with_capacity(chunks.len());
	for (index, chunk) in chunks.iter().enumerate() {
		let number = page_no(index, "subgraph")?;
		let next = (index + 1 < chunks.len()).then(|| PageNo::new(number.get() + 1));
		pages.push(encode_subgraph_directory(number, page_lsn, next, chunk)?);
	}
	Ok(pages)
}

pub(super) fn page_index(page: PageNo, count: usize) -> FerrosGrynnResult<usize> {
	let index = usize::try_from(page.get())
		.map_err(|_error| invalid_page(page.get(), "page number exceeds usize"))?;
	if index >= count {
		return Err(invalid_page(page.get(), "page reference exceeds its file"));
	}
	Ok(index)
}

pub(super) fn page_no(index: usize, family: &str) -> FerrosGrynnResult<PageNo> {
	u64::try_from(index)
		.map(PageNo::new)
		.map_err(|_error| invalid_page(0, format!("{family} page number exceeds u64")))
}
