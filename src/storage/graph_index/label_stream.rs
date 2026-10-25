//! Page-at-a-time packing shared by sorted runs and selected label merges.

use super::{PrimaryAddress, SortedIndexRecords, StreamedIndex, invalid_index, label_idx};
use crate::{
	gql_status::FerrosGrynnResult,
	graph::AtomKind,
	storage::page::{AlignedPage, PageLsn, PageNo},
};

/// Retains one address page and validates the exact declared stream length.
pub(in crate::storage) struct LabelIndexWriter<Address, WritePage> {
	/// Required atom kind for every posting.
	kind: AtomKind,
	/// LSN stamped into each completed page.
	page_lsn: PageLsn,
	/// Declared final population, checked independently of page count.
	expected: usize,
	/// Number of accepted postings.
	seen: usize,
	/// Number of pages required by the declared population.
	page_count: usize,
	/// Next page number to write.
	page_index: usize,
	/// Current bounded page of addresses.
	atoms: Vec<Address>,
	/// Last accepted address, including across page boundaries.
	previous: Option<Address>,
	/// Sequential output sink.
	write: WritePage,
}

impl<Address: PrimaryAddress, WritePage> LabelIndexWriter<Address, WritePage>
where
	WritePage: FnMut(PageNo, &AlignedPage) -> FerrosGrynnResult<()>,
{
	/// Starts a stream; callers admit its fixed page scratch before
	/// construction.
	pub(in crate::storage) fn new(
		expected: usize,
		kind: AtomKind,
		page_lsn: PageLsn,
		write: WritePage,
	) -> Self {
		Self {
			kind,
			page_lsn,
			expected,
			seen: 0,
			page_count: expected.max(1).div_ceil(label_idx::label_atoms_per_page()),
			page_index: 0,
			atoms: Vec::with_capacity(label_idx::label_atoms_per_page()),
			previous: None,
			write,
		}
	}

	/// Accepts one strictly ordered posting without exceeding the declared
	/// count.
	pub(in crate::storage) fn push(&mut self, address: Address) -> FerrosGrynnResult<()> {
		if address.kind() != Some(self.kind)
			|| self.previous.is_some_and(|previous| previous.order_key() >= address.order_key())
		{
			return Err(invalid_index(
				"label index atoms have the wrong kind or are not strictly ordered",
			));
		}
		if self.seen == self.expected {
			return Err(invalid_index("label-index stream exceeds its declared population"));
		}
		if self.atoms.len() == label_idx::label_atoms_per_page() {
			self.flush()?;
			self.page_index += 1;
			self.atoms.clear();
		}
		self.previous = Some(address);
		self.atoms.push(address);
		// The admission above bounds this increment by expected, including
		// usize::MAX.
		self.seen += 1;
		Ok(())
	}

	/// Completes the tail, including the format's single empty page.
	pub(in crate::storage) fn finish(mut self) -> FerrosGrynnResult<StreamedIndex> {
		if self.seen != self.expected {
			return Err(invalid_index("label-index stream disagrees with its declared population"));
		}
		self.flush()?;
		Ok(StreamedIndex {
			root: PageNo::new(0),
			page_count: u64::try_from(self.page_count)
				.map_err(|_error| invalid_index("label-index page count exceeds u64"))?,
		})
	}

	/// Encodes into heap-stable aligned storage before calling the output sink.
	fn flush(&mut self) -> FerrosGrynnResult<()> {
		let index = u64::try_from(self.page_index)
			.map_err(|_error| invalid_index("label-index page number exceeds u64"))?;
		let next = if self.page_index + 1 < self.page_count {
			Some(PageNo::new(
				index
					.checked_add(1)
					.ok_or_else(|| invalid_index("label-index next page overflows u64"))?,
			))
		} else {
			None
		};
		let mut page = Box::new(AlignedPage::zeroed());
		label_idx::encode_label_page(
			&mut page,
			PageNo::new(index),
			self.page_lsn,
			next,
			&self.atoms,
		)?;
		(self.write)(PageNo::new(index), &page)
	}
}

/// Writes one dense label array from an externally sorted address stream.
pub(in crate::storage) fn stream_label_index<Address: PrimaryAddress>(
	records: &SortedIndexRecords,
	kind: AtomKind,
	page_lsn: PageLsn,
	write: impl FnMut(PageNo, &AlignedPage) -> FerrosGrynnResult<()>,
) -> FerrosGrynnResult<StreamedIndex> {
	let mut writer = LabelIndexWriter::new(records.len()?, kind, page_lsn, write);
	records.visit::<Address>(|_key, address| writer.push(address))?;
	writer.finish()
}
