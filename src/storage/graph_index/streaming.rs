//! Bounded page-at-a-time writers for immutable import index files.

use std::path::Path;

#[allow(clippy::wildcard_imports, reason = "streaming packers share the private B+tree codec")]
use super::*;

/// Root and exact page count of one streamed immutable index.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::storage) struct StreamedIndex {
	/// Root page within the completed immutable file.
	pub(in crate::storage) root: PageNo,
	/// Exact number of sequential pages written.
	pub(in crate::storage) page_count: u64,
}

/// Writes one unique B+tree without retaining its output pages.
pub(in crate::storage) fn stream_unique_index<Address: PrimaryAddress>(
	records: &SortedIndexRecords,
	scratch_root: &Path,
	page_lsn: PageLsn,
	mut write: impl FnMut(PageNo, &AlignedPage) -> FerrosGrynnResult<()>,
) -> FerrosGrynnResult<StreamedIndex> {
	let leaf_count = count_unique_leaves::<Address>(records)?;
	let mut children = ExternalIndexSorter::new(scratch_root);
	let mut leaf = Vec::<PrimaryIndexEntry<Address>>::new();
	let mut leaf_data_bytes = 0_usize;
	let mut leaf_index = 0_usize;
	let mut previous_key = None::<Vec<u8>>;
	records.visit::<Address>(|key, address| {
		if previous_key.as_deref().is_some_and(|previous| previous >= key) {
			return Err(FerrosGrynnError::graph_constraint_violation(
				"unique index",
				"unique index contains a duplicate key",
			));
		}
		let entry_bytes = unique_entry_bytes(key.len())?;
		if !leaf.is_empty()
			&& !leaf_accepts_bytes(
				leaf.len() + 1,
				leaf_data_bytes,
				entry_bytes,
				MUTABLE_PRIMARY_LEAF_BYTES,
			)? {
			flush_unique_leaf(&mut write, &mut children, leaf_index, leaf_count, &leaf, page_lsn)?;
			leaf_index += 1;
			leaf.clear();
			leaf_data_bytes = 0;
		}
		if !leaf_accepts_bytes(leaf.len() + 1, leaf_data_bytes, entry_bytes, PAGE_SIZE)? {
			return Err(invalid_index("one unique key exceeds leaf-page capacity"));
		}
		leaf_data_bytes = leaf_data_bytes
			.checked_add(entry_bytes)
			.ok_or_else(|| invalid_index("unique leaf data length overflows usize"))?;
		leaf.push(PrimaryIndexEntry {
			key: key.to_vec(),
			atom: address,
		});
		previous_key = Some(key.to_vec());
		Ok(())
	})?;
	flush_unique_leaf(&mut write, &mut children, leaf_index, leaf_count, &leaf, page_lsn)?;
	if leaf_index + 1 != leaf_count {
		return Err(invalid_index("unique leaf sizing pass disagrees with packing pass"));
	}
	if leaf_count == 1 {
		return Ok(StreamedIndex {
			root: PageNo::new(0),
			page_count: 1,
		});
	}
	let children = children.finish()?;
	let (root, page_count) = stream_internal_levels(
		children,
		u64::try_from(leaf_count)
			.map_err(|_error| invalid_index("unique leaf count exceeds u64"))?,
		scratch_root,
		page_lsn,
		&mut write,
	)?;
	Ok(StreamedIndex {
		root,
		page_count,
	})
}

/// Writes one non-unique B+tree while retaining at most one leaf and one
/// posting page.
pub(in crate::storage) fn stream_standard_index<Address: PrimaryAddress + Ord>(
	records: &SortedIndexRecords,
	scratch_root: &Path,
	page_lsn: PageLsn,
	mut write: impl FnMut(PageNo, &AlignedPage) -> FerrosGrynnResult<()>,
) -> FerrosGrynnResult<StreamedIndex> {
	let layout = count_standard_layout::<Address>(records)?;
	let mut children = ExternalIndexSorter::new(scratch_root);
	let mut leaf = Vec::<OwnedScalarLeafEntry<Address>>::new();
	let mut leaf_data_bytes = 0_usize;
	let mut leaf_index = 0_usize;
	let mut next_posting = u64::try_from(layout.leaf_count)
		.map_err(|_error| invalid_index("standard leaf count exceeds u64"))?;
	let mut group = None::<PendingScalarGroup<Address>>;
	records.visit::<Address>(|key, address| {
		if let Some(current) = group.as_mut()
			&& current.key == key
		{
			current.push(address)?;
			return Ok(());
		}
		if group.as_ref().is_some_and(|current| current.key.as_slice() > key) {
			return Err(invalid_index("external-sort scalar keys are not ordered"));
		}
		if let Some(current) = group.take() {
			append_scalar_group(
				current,
				&mut leaf,
				&mut leaf_data_bytes,
				&mut leaf_index,
				layout.leaf_count,
				&mut next_posting,
				&mut children,
				page_lsn,
				&mut write,
			)?;
		}
		group = Some(PendingScalarGroup::new(key, address));
		Ok(())
	})?;
	if let Some(current) = group.take() {
		append_scalar_group(
			current,
			&mut leaf,
			&mut leaf_data_bytes,
			&mut leaf_index,
			layout.leaf_count,
			&mut next_posting,
			&mut children,
			page_lsn,
			&mut write,
		)?;
	}
	flush_standard_leaf(&mut write, &mut children, leaf_index, layout.leaf_count, &leaf, page_lsn)?;
	if leaf_index + 1 != layout.leaf_count
		|| next_posting
			!= u64::try_from(layout.leaf_count + layout.posting_pages)
				.map_err(|_error| invalid_index("standard page count exceeds u64"))?
	{
		return Err(invalid_index("standard sizing pass disagrees with leaf packing pass"));
	}

	let posting_end = write_posting_pages::<Address>(
		records,
		u64::try_from(layout.leaf_count)
			.map_err(|_error| invalid_index("standard leaf count exceeds u64"))?,
		page_lsn,
		&mut write,
	)?;
	let leaf_and_posting = u64::try_from(layout.leaf_count + layout.posting_pages)
		.map_err(|_error| invalid_index("standard page count exceeds u64"))?;
	if posting_end != leaf_and_posting {
		return Err(invalid_index("standard sizing pass disagrees with posting packing pass"));
	}
	if layout.leaf_count == 1 {
		return Ok(StreamedIndex {
			root: PageNo::new(0),
			page_count: leaf_and_posting,
		});
	}
	let children = children.finish()?;
	let (root, page_count) =
		stream_internal_levels(children, leaf_and_posting, scratch_root, page_lsn, &mut write)?;
	Ok(StreamedIndex {
		root,
		page_count,
	})
}

/// Writes one dense label array from an externally sorted address stream.
pub(in crate::storage) fn stream_label_index<Address: PrimaryAddress>(
	records: &SortedIndexRecords,
	kind: AtomKind,
	page_lsn: PageLsn,
	mut write: impl FnMut(PageNo, &AlignedPage) -> FerrosGrynnResult<()>,
) -> FerrosGrynnResult<StreamedIndex> {
	let capacity = super::label_idx::label_atoms_per_page();
	let item_count = records.len()?;
	let page_count = item_count.max(1).div_ceil(capacity);
	let mut page_index = 0_usize;
	let mut atoms = Vec::with_capacity(capacity);
	let mut previous = None::<Address>;
	records.visit::<Address>(|_key, address| {
		if address.kind() != Some(kind)
			|| previous.is_some_and(|previous| previous.order_key() >= address.order_key())
		{
			return Err(invalid_index(
				"label index atoms have the wrong kind or are not strictly ordered",
			));
		}
		if atoms.len() == capacity {
			flush_label_page(&mut write, page_index, page_count, &atoms, page_lsn)?;
			page_index += 1;
			atoms.clear();
		}
		previous = Some(address);
		atoms.push(address);
		Ok(())
	})?;
	flush_label_page(&mut write, page_index, page_count, &atoms, page_lsn)?;
	if page_index + 1 != page_count {
		return Err(invalid_index("label-index sizing pass disagrees with packing pass"));
	}
	Ok(StreamedIndex {
		root: PageNo::new(0),
		page_count: u64::try_from(page_count)
			.map_err(|_error| invalid_index("label-index page count exceeds u64"))?,
	})
}

/// Sizes unique leaves without retaining encoded output pages.
fn count_unique_leaves<Address: PrimaryAddress>(
	records: &SortedIndexRecords,
) -> FerrosGrynnResult<usize> {
	let mut leaves = 0_usize;
	let mut entries = 0_usize;
	let mut data_bytes = 0_usize;
	let mut previous = None::<Vec<u8>>;
	records.visit::<Address>(|key, _address| {
		if previous.as_deref().is_some_and(|previous| previous >= key) {
			return Err(FerrosGrynnError::graph_constraint_violation(
				"unique index",
				"unique index contains a duplicate key",
			));
		}
		let bytes = unique_entry_bytes(key.len())?;
		if entries != 0
			&& !leaf_accepts_bytes(entries + 1, data_bytes, bytes, MUTABLE_PRIMARY_LEAF_BYTES)?
		{
			leaves = leaves
				.checked_add(1)
				.ok_or_else(|| invalid_index("unique leaf count overflows usize"))?;
			entries = 0;
			data_bytes = 0;
		}
		if !leaf_accepts_bytes(entries + 1, data_bytes, bytes, PAGE_SIZE)? {
			return Err(invalid_index("one unique key exceeds leaf-page capacity"));
		}
		entries = entries
			.checked_add(1)
			.ok_or_else(|| invalid_index("unique leaf entry count overflows usize"))?;
		data_bytes = data_bytes
			.checked_add(bytes)
			.ok_or_else(|| invalid_index("unique leaf byte count overflows usize"))?;
		previous = Some(key.to_vec());
		Ok(())
	})?;
	Ok(if entries == 0 && leaves == 0 {
		1
	} else {
		leaves + usize::from(entries != 0)
	})
}

#[derive(Clone, Copy)]
/// Page counts determined by the standard-index sizing pass.
struct StandardLayout {
	/// Number of leaf pages written before postings.
	leaf_count: usize,
	/// Number of overflow posting pages.
	posting_pages: usize,
}

/// Sizes standard leaves and posting chains from the sorted record stream.
fn count_standard_layout<Address: PrimaryAddress + Ord>(
	records: &SortedIndexRecords,
) -> FerrosGrynnResult<StandardLayout> {
	let mut leaf_count = 0_usize;
	let mut leaf_entries = 0_usize;
	let mut leaf_data_bytes = 0_usize;
	let mut posting_pages = 0_usize;
	let mut group_key = None::<Vec<u8>>;
	let mut group_atoms = 0_usize;
	let mut last_atom = None::<Address>;
	let mut finish_group = |key_len: usize, atoms: usize| -> FerrosGrynnResult<()> {
		let bytes = standard_entry_size(key_len, atoms)?;
		if leaf_entries != 0
			&& !leaf_accepts_bytes(
				leaf_entries + 1,
				leaf_data_bytes,
				bytes,
				MUTABLE_STANDARD_LEAF_BYTES,
			)? {
			leaf_count = leaf_count
				.checked_add(1)
				.ok_or_else(|| invalid_index("standard leaf count overflows usize"))?;
			leaf_entries = 0;
			leaf_data_bytes = 0;
		}
		if !leaf_accepts_bytes(leaf_entries + 1, leaf_data_bytes, bytes, PAGE_SIZE)? {
			return Err(invalid_index("one scalar key exceeds leaf-page capacity"));
		}
		leaf_entries = leaf_entries
			.checked_add(1)
			.ok_or_else(|| invalid_index("standard leaf entry count overflows usize"))?;
		leaf_data_bytes = leaf_data_bytes
			.checked_add(bytes)
			.ok_or_else(|| invalid_index("standard leaf byte count overflows usize"))?;
		posting_pages = posting_pages
			.checked_add(
				atoms.saturating_sub(STANDARD_INLINE_ATOMS).div_ceil(POSTING_ATOMS_PER_PAGE),
			)
			.ok_or_else(|| invalid_index("scalar posting page count overflows usize"))?;
		Ok(())
	};
	records.visit::<Address>(|key, address| {
		if group_key.as_deref() == Some(key) {
			if last_atom.is_some_and(|previous| previous >= address) {
				return Err(invalid_index("external-sort atoms are not ordered"));
			}
			group_atoms = group_atoms
				.checked_add(1)
				.ok_or_else(|| invalid_index("standard posting count overflows usize"))?;
			last_atom = Some(address);
			return Ok(());
		}
		if group_key.as_deref().is_some_and(|previous| previous > key) {
			return Err(invalid_index("external-sort scalar keys are not ordered"));
		}
		if let Some(previous) = group_key.take() {
			finish_group(previous.len(), group_atoms)?;
		}
		group_key = Some(key.to_vec());
		group_atoms = 1;
		last_atom = Some(address);
		Ok(())
	})?;
	if let Some(key) = group_key {
		finish_group(key.len(), group_atoms)?;
	}
	if leaf_entries != 0 {
		leaf_count = leaf_count
			.checked_add(1)
			.ok_or_else(|| invalid_index("standard leaf count overflows usize"))?;
	}
	Ok(StandardLayout {
		leaf_count: leaf_count.max(1),
		posting_pages,
	})
}

/// Current non-unique key group retained while streaming sorted records.
struct PendingScalarGroup<Address> {
	/// Canonical scalar key.
	key: Vec<u8>,
	/// Bounded inline prefix of posting addresses.
	inline_atoms: Vec<Address>,
	/// Exact number of addresses in the group.
	total_count: usize,
	/// Last address used to enforce strict ordering.
	last: Address,
}

impl<Address: Copy + Ord> PendingScalarGroup<Address> {
	/// Starts a group with its first address.
	fn new(key: &[u8], address: Address) -> Self {
		Self {
			key: key.to_vec(),
			inline_atoms: vec![address],
			total_count: 1,
			last: address,
		}
	}

	/// Appends one strictly increasing address.
	fn push(&mut self, address: Address) -> FerrosGrynnResult<()> {
		if self.last >= address {
			return Err(invalid_index("external-sort atoms are not ordered"));
		}
		self.total_count = self
			.total_count
			.checked_add(1)
			.ok_or_else(|| invalid_index("scalar posting count overflows usize"))?;
		if self.inline_atoms.len() < STANDARD_INLINE_ATOMS {
			self.inline_atoms.push(address);
		}
		self.last = address;
		Ok(())
	}
}

/// Owned standard-leaf entry retained until one page is flushed.
struct OwnedScalarLeafEntry<Address> {
	/// Canonical scalar key.
	key: Vec<u8>,
	/// Inline posting prefix.
	inline_atoms: Vec<Address>,
	/// Exact posting cardinality.
	total_count: usize,
	/// First overflow posting page, when required.
	posting_root: Option<PageNo>,
}

#[allow(
	clippy::too_many_arguments,
	reason = "one bounded scalar leaf state is advanced atomically"
)]
/// Appends one completed key group, flushing the current leaf when necessary.
fn append_scalar_group<Address: PrimaryAddress + Ord>(
	group: PendingScalarGroup<Address>,
	leaf: &mut Vec<OwnedScalarLeafEntry<Address>>,
	leaf_data_bytes: &mut usize,
	leaf_index: &mut usize,
	leaf_count: usize,
	next_posting: &mut u64,
	children: &mut ExternalIndexSorter,
	page_lsn: PageLsn,
	write: &mut impl FnMut(PageNo, &AlignedPage) -> FerrosGrynnResult<()>,
) -> FerrosGrynnResult<()> {
	let bytes = standard_entry_size(group.key.len(), group.total_count)?;
	if !leaf.is_empty()
		&& !leaf_accepts_bytes(
			leaf.len() + 1,
			*leaf_data_bytes,
			bytes,
			MUTABLE_STANDARD_LEAF_BYTES,
		)? {
		flush_standard_leaf(write, children, *leaf_index, leaf_count, leaf, page_lsn)?;
		*leaf_index += 1;
		leaf.clear();
		*leaf_data_bytes = 0;
	}
	if !leaf_accepts_bytes(leaf.len() + 1, *leaf_data_bytes, bytes, PAGE_SIZE)? {
		return Err(invalid_index("one scalar key exceeds leaf-page capacity"));
	}
	let posting_root =
		(group.total_count > STANDARD_INLINE_ATOMS).then(|| PageNo::new(*next_posting));
	*next_posting = next_posting
		.checked_add(
			u64::try_from(
				group
					.total_count
					.saturating_sub(STANDARD_INLINE_ATOMS)
					.div_ceil(POSTING_ATOMS_PER_PAGE),
			)
			.map_err(|_error| invalid_index("scalar posting page count exceeds u64"))?,
		)
		.ok_or_else(|| invalid_index("scalar posting page number overflows u64"))?;
	*leaf_data_bytes = leaf_data_bytes
		.checked_add(bytes)
		.ok_or_else(|| invalid_index("standard leaf byte count overflows usize"))?;
	leaf.push(OwnedScalarLeafEntry {
		key: group.key,
		inline_atoms: group.inline_atoms,
		total_count: group.total_count,
		posting_root,
	});
	Ok(())
}

/// Encodes and writes one unique leaf, then records its minimum key.
fn flush_unique_leaf<Address: PrimaryAddress>(
	write: &mut impl FnMut(PageNo, &AlignedPage) -> FerrosGrynnResult<()>,
	children: &mut ExternalIndexSorter,
	leaf_index: usize,
	leaf_count: usize,
	leaf: &[PrimaryIndexEntry<Address>],
	page_lsn: PageLsn,
) -> FerrosGrynnResult<()> {
	let page_no = usize_page_no(leaf_index, "unique leaf page index")?;
	let next = (leaf_index + 1 < leaf_count).then(|| PageNo::new(page_no.get() + 1));
	let mut page = Box::new(AlignedPage::zeroed());
	encode_unique_leaf(&mut page, page_no, page_lsn, next, leaf)?;
	write(page_no, &page)?;
	if let Some(first) = leaf.first() {
		children.push_raw(first.key.clone(), page_no.get())?;
	}
	Ok(())
}

/// Encodes and writes one standard leaf, then records its minimum key.
fn flush_standard_leaf<Address: PrimaryAddress>(
	write: &mut impl FnMut(PageNo, &AlignedPage) -> FerrosGrynnResult<()>,
	children: &mut ExternalIndexSorter,
	leaf_index: usize,
	leaf_count: usize,
	leaf: &[OwnedScalarLeafEntry<Address>],
	page_lsn: PageLsn,
) -> FerrosGrynnResult<()> {
	let page_no = usize_page_no(leaf_index, "standard leaf page index")?;
	let next = (leaf_index + 1 < leaf_count).then(|| PageNo::new(page_no.get() + 1));
	let borrowed = leaf
		.iter()
		.map(|entry| ScalarLeafEntry {
			key: &entry.key,
			inline_atoms: &entry.inline_atoms,
			total_count: entry.total_count,
			posting_root: entry.posting_root,
		})
		.collect::<Vec<_>>();
	let mut page = Box::new(AlignedPage::zeroed());
	encode_standard_leaf_parts(&mut page, page_no, page_lsn, next, &borrowed)?;
	write(page_no, &page)?;
	if let Some(first) = leaf.first() {
		children.push_raw(first.key.clone(), page_no.get())?;
	}
	Ok(())
}

/// Streams standard-index overflow postings after the complete leaf range.
fn write_posting_pages<Address: PrimaryAddress + Ord>(
	records: &SortedIndexRecords,
	first_page: u64,
	page_lsn: PageLsn,
	write: &mut impl FnMut(PageNo, &AlignedPage) -> FerrosGrynnResult<()>,
) -> FerrosGrynnResult<u64> {
	let mut page_no = first_page;
	let mut key = None::<Vec<u8>>;
	let mut seen = 0_usize;
	let mut previous = None::<Address>;
	let mut chunk = Vec::with_capacity(POSTING_ATOMS_PER_PAGE);
	let mut flush =
		|chunk: &mut Vec<Address>, next: bool, page_no: &mut u64| -> FerrosGrynnResult<()> {
			if chunk.is_empty() {
				return Ok(());
			}
			let current = PageNo::new(*page_no);
			let next = next.then(|| PageNo::new(current.get() + 1));
			let mut page = Box::new(AlignedPage::zeroed());
			encode_posting_page(&mut page, current, page_lsn, next, chunk)?;
			write(current, &page)?;
			*page_no = page_no
				.checked_add(1)
				.ok_or_else(|| invalid_index("posting page number overflows u64"))?;
			chunk.clear();
			Ok(())
		};
	records.visit::<Address>(|current_key, address| {
		if key.as_deref() != Some(current_key) {
			flush(&mut chunk, false, &mut page_no)?;
			key = Some(current_key.to_vec());
			seen = 0;
			previous = None;
		}
		if previous.is_some_and(|previous| previous >= address) {
			return Err(invalid_index("external-sort atoms are not ordered"));
		}
		if seen >= STANDARD_INLINE_ATOMS {
			if chunk.len() == POSTING_ATOMS_PER_PAGE {
				flush(&mut chunk, true, &mut page_no)?;
			}
			chunk.push(address);
		}
		seen = seen
			.checked_add(1)
			.ok_or_else(|| invalid_index("standard posting count overflows usize"))?;
		previous = Some(address);
		Ok(())
	})?;
	flush(&mut chunk, false, &mut page_no)?;
	Ok(page_no)
}

/// Builds internal B+tree levels bottom-up from externally sorted minima.
fn stream_internal_levels(
	mut current: SortedIndexRecords,
	mut next_page: u64,
	scratch_root: &Path,
	page_lsn: PageLsn,
	write: &mut impl FnMut(PageNo, &AlignedPage) -> FerrosGrynnResult<()>,
) -> FerrosGrynnResult<(PageNo, u64)> {
	while current.len()? > 1 {
		let current_count = current.len()?;
		let mut next = ExternalIndexSorter::new(scratch_root);
		let mut group = Vec::<ChildPage>::new();
		let mut previous_key = None::<Vec<u8>>;
		current.visit_raw(|key, address| {
			if previous_key.as_deref().is_some_and(|previous| previous >= key) {
				return Err(invalid_index("B+tree child minima are not strictly ordered"));
			}
			let child = ChildPage {
				page_no: PageNo::new(address),
				min_key: key.to_vec(),
			};
			if !group.is_empty() && !internal_accepts(&group, &child)? {
				flush_internal_group(write, &mut next, &mut next_page, &group, page_lsn)?;
				group.clear();
			}
			if !internal_accepts(&group, &child)? {
				return Err(invalid_index("separator key exceeds internal-page capacity"));
			}
			group.push(child);
			previous_key = Some(key.to_vec());
			Ok(())
		})?;
		flush_internal_group(write, &mut next, &mut next_page, &group, page_lsn)?;
		let next = next.finish()?;
		if next.len()? >= current_count {
			return Err(invalid_index("separator key prevents progress between B+tree levels"));
		}
		current = next;
	}
	let mut root = None;
	current.visit_raw(|_key, address| {
		root = Some(PageNo::new(address));
		Ok(())
	})?;
	Ok((root.ok_or_else(|| invalid_index("B+tree root disappeared"))?, next_page))
}

/// Encodes one internal page or forwards a singleton child unchanged.
fn flush_internal_group(
	write: &mut impl FnMut(PageNo, &AlignedPage) -> FerrosGrynnResult<()>,
	next: &mut ExternalIndexSorter,
	next_page: &mut u64,
	group: &[ChildPage],
	page_lsn: PageLsn,
) -> FerrosGrynnResult<()> {
	let first = group.first().ok_or_else(|| invalid_index("empty internal-page group"))?;
	if group.len() == 1 {
		return next.push_raw(first.min_key.clone(), first.page_no.get());
	}
	let page_no = PageNo::new(*next_page);
	let mut page = Box::new(AlignedPage::zeroed());
	encode_internal_page(&mut page, page_no, page_lsn, group)?;
	write(page_no, &page)?;
	next.push_raw(first.min_key.clone(), page_no.get())?;
	*next_page = next_page
		.checked_add(1)
		.ok_or_else(|| invalid_index("B+tree page number overflows u64"))?;
	Ok(())
}

/// Returns whether another child and separator fit the current internal page.
fn internal_accepts(group: &[ChildPage], candidate: &ChildPage) -> FerrosGrynnResult<bool> {
	if group.is_empty() {
		return Ok(true);
	}
	let key_count = group.len();
	let key_bytes = group[1..]
		.iter()
		.try_fold(candidate.min_key.len(), |total, child| total.checked_add(child.min_key.len()))
		.ok_or_else(|| invalid_index("internal key bytes overflow usize"))?;
	INTERNAL_OFFSETS_OFFSET
		.checked_add(key_count * size_of::<u16>())
		.and_then(|used| used.checked_add(key_bytes))
		.and_then(|used| used.checked_add((group.len() + 1) * size_of::<u64>()))
		.map(|used| used <= PAGE_SIZE)
		.ok_or_else(|| invalid_index("internal page size overflows usize"))
}

/// Encodes and writes one dense label page.
fn flush_label_page<Address: PrimaryAddress>(
	write: &mut impl FnMut(PageNo, &AlignedPage) -> FerrosGrynnResult<()>,
	page_index: usize,
	page_count: usize,
	atoms: &[Address],
	page_lsn: PageLsn,
) -> FerrosGrynnResult<()> {
	let page_no = usize_page_no(page_index, "label-index page number")?;
	let next = (page_index + 1 < page_count).then(|| PageNo::new(page_no.get() + 1));
	let mut page = Box::new(AlignedPage::zeroed());
	super::label_idx::encode_label_page(&mut page, page_no, page_lsn, next, atoms)?;
	write(page_no, &page)
}

/// Returns whether one more leaf entry fits within the selected byte ceiling.
fn leaf_accepts_bytes(
	count: usize,
	data_bytes: usize,
	entry_bytes: usize,
	limit: usize,
) -> FerrosGrynnResult<bool> {
	LEAF_OFFSETS_OFFSET
		.checked_add(
			count
				.checked_mul(size_of::<u16>())
				.ok_or_else(|| invalid_index("leaf directory size overflows usize"))?,
		)
		.and_then(|used| used.checked_add(data_bytes))
		.and_then(|used| used.checked_add(entry_bytes))
		.map(|used| used <= limit)
		.ok_or_else(|| invalid_index("leaf page size overflows usize"))
}

/// Computes the exact encoded size of one unique leaf entry.
fn unique_entry_bytes(key_len: usize) -> FerrosGrynnResult<usize> {
	key_len
		.checked_add(DISK_ATOM_REF_BYTES)
		.ok_or_else(|| invalid_index("unique leaf entry length overflows usize"))
}

/// Computes the exact encoded size of one standard leaf entry.
fn standard_entry_size(key_len: usize, atom_count: usize) -> FerrosGrynnResult<usize> {
	key_len
		.checked_add(atom_count.min(STANDARD_INLINE_ATOMS) * DISK_ATOM_REF_BYTES)
		.and_then(|bytes| bytes.checked_add(STANDARD_ENTRY_TRAILER_BYTES))
		.ok_or_else(|| invalid_index("scalar leaf entry size overflows usize"))
}

/// Converts a sizing-pass page index into its durable page number.
fn usize_page_no(index: usize, field: &str) -> FerrosGrynnResult<PageNo> {
	u64::try_from(index)
		.map(PageNo::new)
		.map_err(|_error| invalid_index(format!("{field} exceeds u64")))
}
