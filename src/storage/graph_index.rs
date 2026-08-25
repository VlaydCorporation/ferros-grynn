//! Format-v1 label dictionary and bulk-built primary B+tree.

use std::collections::BTreeSet;

use super::page::{
	AlignedPage,
	PAGE_HEADER_SIZE,
	PAGE_NO_OFFSET,
	PAGE_SIZE,
	PageFlags,
	PageLsn,
	PageNo,
	PageType,
};
use crate::{
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	values::storage::DiskAtomRef,
};

const LABEL_COUNT_OFFSETS: [usize; 4] = [32, 36, 40, 44];
const LABEL_ROOT_OFFSETS: [usize; 4] = [48, 56, 64, 72];
const LABEL_HASH_ROOT_OFFSET: usize = 80;
const LABEL_TZ_HASH_ROOT_OFFSET: usize = 88;
const LABEL_TZ_ROOT_OFFSET: usize = 96;
const LABEL_HEADER_RESERVED_OFFSET: usize = 104;
const LABEL_ENTRY_NEXT_OFFSET: usize = PAGE_HEADER_SIZE;
const LABEL_ENTRY_COUNT_OFFSET: usize = 40;
const LABEL_ENTRY_RESERVED_OFFSET: usize = 44;
const LABEL_ENTRY_DATA_OFFSET: usize = 48;
const LABEL_ENTRY_FIXED_BYTES: usize = 6;

const LEAF_NEXT_OFFSET: usize = PAGE_HEADER_SIZE;
const LEAF_COUNT_OFFSET: usize = 40;
const LEAF_RESERVED_OFFSET: usize = 42;
const LEAF_OFFSETS_OFFSET: usize = 44;
const INTERNAL_COUNT_OFFSET: usize = PAGE_HEADER_SIZE;
const INTERNAL_KEY_BYTES_OFFSET: usize = 34;
const INTERNAL_OFFSETS_OFFSET: usize = 36;
const MAX_BTREE_DEPTH: usize = 64;
const DISK_ATOM_REF_BYTES: usize = size_of::<u64>();

/// Four dictionary sections persisted by the first local slice.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct LabelDictionary {
	pub(super) node_labels: Vec<String>,
	pub(super) edge_labels: Vec<String>,
	pub(super) property_keys: Vec<String>,
	pub(super) table_names: Vec<String>,
	pub(super) time_zones: Vec<String>,
}

impl LabelDictionary {
	/// Returns the strings of one stable header section.
	fn section(&self, index: usize) -> &[String] {
		match index {
			0 => &self.node_labels,
			1 => &self.edge_labels,
			2 => &self.property_keys,
			3 => &self.table_names,
			_ => &[],
		}
	}

	/// Replaces one decoded stable header section.
	fn set_section(&mut self, index: usize, values: Vec<String>) {
		match index {
			0 => self.node_labels = values,
			1 => self.edge_labels = values,
			2 => self.property_keys = values,
			3 => self.table_names = values,
			_ => {}
		}
	}
}

/// Encodes the complete dictionary as one header followed by section chains.
pub(super) fn encode_label_dictionary(
	dictionary: &LabelDictionary,
	page_lsn: PageLsn,
) -> FerrosGrynnResult<Vec<AlignedPage>> {
	let mut chunks = Vec::new();
	let mut roots = [PageNo::NULL; 4];
	let mut next_page = 1_u64;
	for (section, root) in roots.iter_mut().enumerate() {
		validate_dictionary_section(dictionary.section(section))?;
		let section_chunks = partition_dictionary(dictionary.section(section), 0)?;
		if !section_chunks.is_empty() {
			*root = PageNo::new(next_page);
		}
		for chunk in section_chunks {
			chunks.push((PageNo::new(next_page), section, chunk));
			next_page = next_page
				.checked_add(1)
				.ok_or_else(|| invalid_index("label dictionary page number overflows u64"))?;
		}
	}
	validate_dictionary_section(&dictionary.time_zones)?;
	let time_zone_chunks = partition_dictionary(&dictionary.time_zones, 1)?;
	let time_zone_root = if time_zone_chunks.is_empty() {
		PageNo::NULL
	} else {
		PageNo::new(next_page)
	};
	for chunk in time_zone_chunks {
		chunks.push((PageNo::new(next_page), 4, chunk));
		next_page = next_page
			.checked_add(1)
			.ok_or_else(|| invalid_index("time-zone dictionary page number overflows u64"))?;
	}
	let mut pages = Vec::with_capacity(chunks.len() + 1);
	let mut header =
		AlignedPage::new(PageType::LabelDict, PageFlags::empty(), page_lsn, PageNo::new(0));
	for section in 0..4 {
		write_u32(
			header.as_bytes_mut(),
			LABEL_COUNT_OFFSETS[section],
			u32::try_from(dictionary.section(section).len())
				.map_err(|_error| invalid_index("label dictionary section exceeds u32"))?,
		);
		write_u64(header.as_bytes_mut(), LABEL_ROOT_OFFSETS[section], roots[section].get());
	}
	write_u64(header.as_bytes_mut(), LABEL_HASH_ROOT_OFFSET, PageNo::NULL.get());
	write_u64(header.as_bytes_mut(), LABEL_TZ_HASH_ROOT_OFFSET, PageNo::NULL.get());
	write_u64(header.as_bytes_mut(), LABEL_TZ_ROOT_OFFSET, time_zone_root.get());
	header.seal()?;
	pages.push(header);
	for (index, (page_no, chain, entries)) in chunks.iter().enumerate() {
		let next = chunks
			.get(index + 1)
			.filter(|(_page_no, next_chain, _entries)| next_chain == chain)
			.map(|(page_no, _chain, _entries)| *page_no);
		pages.push(encode_dictionary_entry_page(*page_no, page_lsn, next, entries)?);
	}
	Ok(pages)
}

/// Decodes a dictionary through the same snapshot-aware page source used by
/// graph data.
pub(super) fn decode_label_dictionary(
	mut read_page: impl FnMut(PageNo, &mut AlignedPage) -> FerrosGrynnResult<()>,
) -> FerrosGrynnResult<LabelDictionary> {
	let mut header = AlignedPage::zeroed();
	read_page(PageNo::new(0), &mut header)?;
	let page_header = header.validate(PageNo::new(0))?;
	if page_header.page_type() != PageType::LabelDict || !page_header.flags().is_empty() {
		return Err(invalid_index("invalid LabelDict header"));
	}
	let bytes = header.as_bytes();
	validate_zero(&bytes[LABEL_HEADER_RESERVED_OFFSET..], "LabelDict reserved bytes")?;
	if read_u64(bytes, LABEL_HASH_ROOT_OFFSET) != PageNo::NULL.get()
		|| read_u64(bytes, LABEL_TZ_HASH_ROOT_OFFSET) != PageNo::NULL.get()
	{
		return Err(invalid_index("unsupported LabelDict hash section"));
	}
	let mut dictionary = LabelDictionary::default();
	for section in 0..4 {
		let count = usize::try_from(read_u32(bytes, LABEL_COUNT_OFFSETS[section]))
			.map_err(|_error| invalid_index("label count does not fit usize"))?;
		let root = PageNo::new(read_u64(bytes, LABEL_ROOT_OFFSETS[section]));
		let values = decode_dictionary_chain(&mut read_page, root, Some(count), 0)?;
		dictionary.set_section(section, values);
	}
	dictionary.time_zones = decode_dictionary_chain(
		&mut read_page,
		PageNo::new(read_u64(bytes, LABEL_TZ_ROOT_OFFSET)),
		None,
		1,
	)?;
	Ok(dictionary)
}

/// One canonical primary-index item.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct PrimaryIndexEntry {
	pub(super) key: Vec<u8>,
	pub(super) atom: DiskAtomRef,
}

/// Complete immutable B+tree generation and its root page.
pub(super) struct BuiltPrimaryIndex {
	pub(super) root: PageNo,
	pub(super) pages: Vec<AlignedPage>,
}

/// Bulk-builds a unique B+tree from strictly sorted canonical RecordId bytes.
pub(super) fn build_primary_index(
	entries: &[PrimaryIndexEntry],
	page_lsn: PageLsn,
) -> FerrosGrynnResult<BuiltPrimaryIndex> {
	validate_primary_entries(entries)?;
	let leaf_groups = partition_leaf_entries(entries)?;
	let mut pages = Vec::new();
	let mut level = Vec::with_capacity(leaf_groups.len());
	for (index, group) in leaf_groups.iter().enumerate() {
		let page_no = PageNo::new(
			u64::try_from(index).map_err(|_error| invalid_index("leaf page index exceeds u64"))?,
		);
		let next = (index + 1 < leaf_groups.len()).then(|| PageNo::new(page_no.get() + 1));
		pages.push(encode_unique_leaf(page_no, page_lsn, next, group)?);
		level.push(ChildPage {
			page_no,
			min_key: group.first().map_or_else(Vec::new, |entry| entry.key.clone()),
		});
	}
	while level.len() > 1 {
		let groups = partition_internal_children(&level)?;
		if groups.len() == level.len() {
			return Err(invalid_index("separator key exceeds internal-page capacity"));
		}
		let mut next_level = Vec::with_capacity(groups.len());
		for group in groups {
			if group.len() == 1 {
				next_level.push(group[0].clone());
				continue;
			}
			let page_no = PageNo::new(
				u64::try_from(pages.len())
					.map_err(|_error| invalid_index("B+tree page index exceeds u64"))?,
			);
			pages.push(encode_internal_page(page_no, page_lsn, group)?);
			next_level.push(ChildPage {
				page_no,
				min_key: group[0].min_key.clone(),
			});
		}
		level = next_level;
	}
	let root = level.first().map_or(PageNo::new(0), |child| child.page_no);
	Ok(BuiltPrimaryIndex {
		root,
		pages,
	})
}

/// Performs one logarithmic primary lookup without decoding the key bytes.
pub(super) fn lookup_primary_index(
	mut read_page: impl FnMut(PageNo, &mut AlignedPage) -> FerrosGrynnResult<()>,
	root: PageNo,
	target: &[u8],
) -> FerrosGrynnResult<Option<DiskAtomRef>> {
	let mut page_no = root;
	let mut visited = BTreeSet::new();
	for _depth in 0..MAX_BTREE_DEPTH {
		if page_no == PageNo::NULL || !visited.insert(page_no) {
			return Err(invalid_index("primary B+tree contains a null link or cycle"));
		}
		let mut page = AlignedPage::zeroed();
		read_page(page_no, &mut page)?;
		let header = page.validate(page_no)?;
		match header.page_type() {
			PageType::IndexUniqueBtreeLeaf => return lookup_leaf(&page, target),
			PageType::IndexBtreeInternal => page_no = select_internal_child(&page, target)?,
			_ => return Err(invalid_index("primary B+tree contains an invalid page type")),
		}
	}
	Err(invalid_index("primary B+tree exceeds its maximum depth"))
}

/// Streams every primary-index entry in key order through the linked leaves.
pub(super) fn scan_primary_index(
	read_page: impl FnMut(PageNo, &mut AlignedPage) -> FerrosGrynnResult<()>,
	root: PageNo,
) -> FerrosGrynnResult<Vec<PrimaryIndexEntry>> {
	let mut entries = Vec::new();
	visit_primary_index(read_page, root, |key, atom| {
		entries.push(PrimaryIndexEntry {
			key: key.to_vec(),
			atom,
		});
		Ok(())
	})?;
	Ok(entries)
}

/// Visits primary-index entries in key order without retaining the complete
/// index in memory.
pub(super) fn visit_primary_index(
	mut read_page: impl FnMut(PageNo, &mut AlignedPage) -> FerrosGrynnResult<()>,
	root: PageNo,
	mut visit: impl FnMut(&[u8], DiskAtomRef) -> FerrosGrynnResult<()>,
) -> FerrosGrynnResult<()> {
	let mut page_no = root;
	let mut visited = BTreeSet::new();
	for _depth in 0..MAX_BTREE_DEPTH {
		if page_no == PageNo::NULL || !visited.insert(page_no) {
			return Err(invalid_index("primary B+tree contains a null link or cycle"));
		}
		let mut page = AlignedPage::zeroed();
		read_page(page_no, &mut page)?;
		let header = page.validate(page_no)?;
		match header.page_type() {
			PageType::IndexUniqueBtreeLeaf => break,
			PageType::IndexBtreeInternal => page_no = select_internal_child(&page, &[])?,
			_ => return Err(invalid_index("primary B+tree contains an invalid page type")),
		}
	}

	let mut previous_key = None::<Vec<u8>>;
	visited.clear();
	while page_no != PageNo::NULL {
		if !visited.insert(page_no) {
			return Err(invalid_index("primary B+tree leaf chain contains a cycle"));
		}
		let mut page = AlignedPage::zeroed();
		read_page(page_no, &mut page)?;
		let header = page.validate(page_no)?;
		if header.page_type() != PageType::IndexUniqueBtreeLeaf {
			return Err(invalid_index("primary B+tree leaf link targets an invalid page"));
		}
		let bytes = page.as_bytes();
		let (count, area_start) = validate_leaf_layout(&page)?;
		for index in 0..count {
			let (key, atom) = leaf_entry(bytes, area_start, count, index)?;
			if previous_key.as_deref().is_some_and(|previous| previous >= key) {
				return Err(invalid_index("primary B+tree leaf keys are not strictly ordered"));
			}
			let atom = DiskAtomRef::from_raw(atom)?;
			visit(key, atom)?;
			previous_key = Some(key.to_vec());
		}
		page_no = PageNo::new(read_u64(bytes, LEAF_NEXT_OFFSET));
	}
	Ok(())
}

#[derive(Clone)]
struct ChildPage {
	page_no: PageNo,
	min_key: Vec<u8>,
}

fn validate_dictionary_section(values: &[String]) -> FerrosGrynnResult<()> {
	let mut unique = BTreeSet::new();
	for value in values {
		if value.is_empty()
			|| value.contains('\0')
			|| value.len() > usize::from(u16::MAX)
			|| !unique.insert(value.as_str())
		{
			return Err(invalid_index("label dictionary contains an invalid or duplicate string"));
		}
	}
	Ok(())
}

fn partition_dictionary(
	values: &[String],
	first_id: u32,
) -> FerrosGrynnResult<Vec<Vec<(u32, &str)>>> {
	let mut chunks = Vec::new();
	let mut current = Vec::new();
	let mut used = LABEL_ENTRY_DATA_OFFSET;
	for (index, value) in values.iter().enumerate() {
		let id = first_id
			.checked_add(
				u32::try_from(index)
					.map_err(|_error| invalid_index("dictionary id exceeds u32"))?,
			)
			.ok_or_else(|| invalid_index("dictionary id overflows u32"))?;
		let bytes = LABEL_ENTRY_FIXED_BYTES
			.checked_add(value.len())
			.ok_or_else(|| invalid_index("label entry length overflows usize"))?;
		if bytes > PAGE_SIZE - LABEL_ENTRY_DATA_OFFSET {
			return Err(invalid_index("one label entry exceeds page capacity"));
		}
		if used + bytes > PAGE_SIZE && !current.is_empty() {
			chunks.push(current);
			current = Vec::new();
			used = LABEL_ENTRY_DATA_OFFSET;
		}
		current.push((id, value.as_str()));
		used += bytes;
	}
	if !current.is_empty() {
		chunks.push(current);
	}
	Ok(chunks)
}

fn encode_dictionary_entry_page(
	page_no: PageNo,
	page_lsn: PageLsn,
	next: Option<PageNo>,
	entries: &[(u32, &str)],
) -> FerrosGrynnResult<AlignedPage> {
	let mut page =
		AlignedPage::new(PageType::LabelDictEntry, PageFlags::empty(), page_lsn, page_no);
	write_u64(page.as_bytes_mut(), LABEL_ENTRY_NEXT_OFFSET, next.unwrap_or(PageNo::NULL).get());
	write_u32(
		page.as_bytes_mut(),
		LABEL_ENTRY_COUNT_OFFSET,
		u32::try_from(entries.len())
			.map_err(|_error| invalid_index("label page count exceeds u32"))?,
	);
	let mut offset = LABEL_ENTRY_DATA_OFFSET;
	for (id, value) in entries {
		write_u32(page.as_bytes_mut(), offset, *id);
		write_u16(
			page.as_bytes_mut(),
			offset + size_of::<u32>(),
			u16::try_from(value.len())
				.map_err(|_error| invalid_index("label length exceeds u16"))?,
		);
		offset += LABEL_ENTRY_FIXED_BYTES;
		page.as_bytes_mut()[offset..offset + value.len()].copy_from_slice(value.as_bytes());
		offset += value.len();
	}
	page.seal()?;
	Ok(page)
}

fn decode_dictionary_chain(
	read_page: &mut impl FnMut(PageNo, &mut AlignedPage) -> FerrosGrynnResult<()>,
	root: PageNo,
	expected_count: Option<usize>,
	first_id: u32,
) -> FerrosGrynnResult<Vec<String>> {
	if expected_count == Some(0) {
		return if root == PageNo::NULL {
			Ok(Vec::new())
		} else {
			Err(invalid_index("empty label section has a non-null root"))
		};
	}
	if expected_count.is_none() && root == PageNo::NULL {
		return Ok(Vec::new());
	}
	let mut values = Vec::with_capacity(expected_count.unwrap_or(0));
	let mut page_no = root;
	let mut visited = BTreeSet::new();
	while page_no != PageNo::NULL {
		if !visited.insert(page_no) {
			return Err(invalid_index("label dictionary chain contains a cycle"));
		}
		let mut page = AlignedPage::zeroed();
		read_page(page_no, &mut page)?;
		let header = page.validate(page_no)?;
		if header.page_type() != PageType::LabelDictEntry || !header.flags().is_empty() {
			return Err(invalid_index("invalid LabelDictEntry page"));
		}
		let bytes = page.as_bytes();
		if read_u16(bytes, LABEL_ENTRY_RESERVED_OFFSET) != 0 {
			return Err(invalid_index("LabelDictEntry padding is non-zero"));
		}
		let count = usize::try_from(read_u32(bytes, LABEL_ENTRY_COUNT_OFFSET))
			.map_err(|_error| invalid_index("label page count does not fit usize"))?;
		let mut offset = LABEL_ENTRY_DATA_OFFSET;
		for _ in 0..count {
			let end = offset
				.checked_add(LABEL_ENTRY_FIXED_BYTES)
				.ok_or_else(|| invalid_index("label entry header overflows usize"))?;
			if end > PAGE_SIZE {
				return Err(invalid_index("label entry header exceeds its page"));
			}
			let id = usize::try_from(read_u32(bytes, offset))
				.map_err(|_error| invalid_index("label id does not fit usize"))?;
			let length = usize::from(read_u16(bytes, offset + size_of::<u32>()));
			offset = end;
			let end = offset
				.checked_add(length)
				.ok_or_else(|| invalid_index("label bytes overflow usize"))?;
			let expected_id = first_id
				.checked_add(
					u32::try_from(values.len())
						.map_err(|_error| invalid_index("dictionary position exceeds u32"))?,
				)
				.ok_or_else(|| invalid_index("dictionary id overflows u32"))?;
			if end > PAGE_SIZE || u32::try_from(id).ok() != Some(expected_id) {
				return Err(invalid_index("label ids are not dense or bytes exceed the page"));
			}
			let value = std::str::from_utf8(&bytes[offset..end])
				.map_err(|source| invalid_index(format!("label is not UTF-8: {source}")))?;
			if value.is_empty() || value.contains('\0') {
				return Err(invalid_index("label is empty or contains NUL"));
			}
			values.push(value.to_owned());
			offset = end;
		}
		validate_zero(&bytes[offset..], "LabelDictEntry trailing bytes")?;
		page_no = PageNo::new(read_u64(bytes, LABEL_ENTRY_NEXT_OFFSET));
		if expected_count.is_some_and(|expected| values.len() > expected) {
			return Err(invalid_index("label chain exceeds its declared count"));
		}
	}
	if expected_count.is_some_and(|expected| values.len() != expected) {
		return Err(invalid_index("label chain ended before its declared count"));
	}
	validate_dictionary_section(&values)?;
	Ok(values)
}

fn validate_primary_entries(entries: &[PrimaryIndexEntry]) -> FerrosGrynnResult<()> {
	for (index, entry) in entries.iter().enumerate() {
		if entry.key.is_empty() {
			return Err(invalid_index("primary index contains an empty key"));
		}
		if index > 0 && entries[index - 1].key >= entry.key {
			return Err(invalid_index("primary index keys are not strictly ordered"));
		}
	}
	Ok(())
}

fn partition_leaf_entries(
	entries: &[PrimaryIndexEntry],
) -> FerrosGrynnResult<Vec<&[PrimaryIndexEntry]>> {
	if entries.is_empty() {
		return Ok(vec![entries]);
	}
	let mut groups = Vec::new();
	let mut start = 0;
	while start < entries.len() {
		let mut end = start;
		let mut data_bytes = 0_usize;
		while end < entries.len() {
			let entry_bytes = entries[end]
				.key
				.len()
				.checked_add(DISK_ATOM_REF_BYTES)
				.ok_or_else(|| invalid_index("primary leaf entry length overflows usize"))?;
			let count = end - start + 1;
			let total = LEAF_OFFSETS_OFFSET
				.checked_add(count * size_of::<u16>())
				.and_then(|fixed| fixed.checked_add(data_bytes))
				.and_then(|used| used.checked_add(entry_bytes))
				.ok_or_else(|| invalid_index("primary leaf size overflows usize"))?;
			if total > PAGE_SIZE {
				break;
			}
			data_bytes += entry_bytes;
			end += 1;
		}
		if end == start {
			return Err(invalid_index("one primary key exceeds leaf-page capacity"));
		}
		groups.push(&entries[start..end]);
		start = end;
	}
	Ok(groups)
}

fn encode_unique_leaf(
	page_no: PageNo,
	page_lsn: PageLsn,
	next: Option<PageNo>,
	entries: &[PrimaryIndexEntry],
) -> FerrosGrynnResult<AlignedPage> {
	let count = u16::try_from(entries.len())
		.map_err(|_error| invalid_index("primary leaf entry count exceeds u16"))?;
	let area_start = LEAF_OFFSETS_OFFSET + entries.len() * size_of::<u16>();
	let data_bytes = entries.iter().try_fold(0_usize, |total, entry| {
		total
			.checked_add(entry.key.len() + DISK_ATOM_REF_BYTES)
			.ok_or_else(|| invalid_index("primary leaf data length overflows usize"))
	})?;
	let data_start = PAGE_SIZE
		.checked_sub(data_bytes)
		.filter(|start| *start >= area_start)
		.ok_or_else(|| invalid_index("primary leaf data overlaps its directory"))?;
	let mut page =
		AlignedPage::new(PageType::IndexUniqueBtreeLeaf, PageFlags::LEAF, page_lsn, page_no);
	write_u64(page.as_bytes_mut(), LEAF_NEXT_OFFSET, next.unwrap_or(PageNo::NULL).get());
	write_u16(page.as_bytes_mut(), LEAF_COUNT_OFFSET, count);
	let mut area_offset = 0_usize;
	for (index, entry) in entries.iter().enumerate() {
		let persisted_offset = data_start + area_offset - area_start;
		write_u16(
			page.as_bytes_mut(),
			LEAF_OFFSETS_OFFSET + index * size_of::<u16>(),
			u16::try_from(persisted_offset)
				.map_err(|_error| invalid_index("primary leaf offset exceeds u16"))?,
		);
		let start = data_start + area_offset;
		page.as_bytes_mut()[start..start + entry.key.len()].copy_from_slice(&entry.key);
		write_u64(page.as_bytes_mut(), start + entry.key.len(), entry.atom.raw());
		area_offset += entry.key.len() + DISK_ATOM_REF_BYTES;
	}
	page.seal()?;
	Ok(page)
}

fn partition_internal_children(children: &[ChildPage]) -> FerrosGrynnResult<Vec<&[ChildPage]>> {
	let mut groups = Vec::new();
	let mut start = 0;
	while start < children.len() {
		let mut end = start + 1;
		let mut key_bytes = 0_usize;
		while end < children.len() {
			let next_keys = end - start;
			let candidate_key_bytes = key_bytes
				.checked_add(children[end].min_key.len())
				.ok_or_else(|| invalid_index("internal key bytes overflow usize"))?;
			let total = INTERNAL_OFFSETS_OFFSET
				.checked_add(next_keys * size_of::<u16>())
				.and_then(|used| used.checked_add(candidate_key_bytes))
				.and_then(|used| used.checked_add((next_keys + 1) * size_of::<u64>()))
				.ok_or_else(|| invalid_index("internal page size overflows usize"))?;
			if total > PAGE_SIZE {
				break;
			}
			key_bytes = candidate_key_bytes;
			end += 1;
		}
		groups.push(&children[start..end]);
		start = end;
	}
	Ok(groups)
}

fn encode_internal_page(
	page_no: PageNo,
	page_lsn: PageLsn,
	children: &[ChildPage],
) -> FerrosGrynnResult<AlignedPage> {
	if children.len() < 2 {
		return Err(invalid_index("internal page requires at least two children"));
	}
	let key_count = children.len() - 1;
	let key_bytes = children[1..].iter().try_fold(0_usize, |total, child| {
		total
			.checked_add(child.min_key.len())
			.ok_or_else(|| invalid_index("internal key bytes overflow"))
	})?;
	let key_area = INTERNAL_OFFSETS_OFFSET + key_count * size_of::<u16>();
	let child_area = PAGE_SIZE - children.len() * size_of::<u64>();
	if key_area + key_bytes > child_area {
		return Err(invalid_index("internal page key and child areas overlap"));
	}
	let mut page =
		AlignedPage::new(PageType::IndexBtreeInternal, PageFlags::empty(), page_lsn, page_no);
	write_u16(
		page.as_bytes_mut(),
		INTERNAL_COUNT_OFFSET,
		u16::try_from(key_count)
			.map_err(|_error| invalid_index("internal key count exceeds u16"))?,
	);
	write_u16(
		page.as_bytes_mut(),
		INTERNAL_KEY_BYTES_OFFSET,
		u16::try_from(key_bytes)
			.map_err(|_error| invalid_index("internal key area exceeds u16"))?,
	);
	let mut key_offset = 0_usize;
	for (index, child) in children[1..].iter().enumerate() {
		write_u16(
			page.as_bytes_mut(),
			INTERNAL_OFFSETS_OFFSET + index * size_of::<u16>(),
			u16::try_from(key_offset)
				.map_err(|_error| invalid_index("internal key offset exceeds u16"))?,
		);
		let start = key_area + key_offset;
		page.as_bytes_mut()[start..start + child.min_key.len()].copy_from_slice(&child.min_key);
		key_offset += child.min_key.len();
	}
	for (index, child) in children.iter().enumerate() {
		write_u64(page.as_bytes_mut(), child_area + index * size_of::<u64>(), child.page_no.get());
	}
	page.seal()?;
	Ok(page)
}

fn lookup_leaf(page: &AlignedPage, target: &[u8]) -> FerrosGrynnResult<Option<DiskAtomRef>> {
	let bytes = page.as_bytes();
	let (count, area_start) = validate_leaf_layout(page)?;
	let mut low = 0;
	let mut high = count;
	while low < high {
		let middle = low + (high - low) / 2;
		let (key, _atom) = leaf_entry(bytes, area_start, count, middle)?;
		match key.cmp(target) {
			std::cmp::Ordering::Less => low = middle + 1,
			std::cmp::Ordering::Greater => high = middle,
			std::cmp::Ordering::Equal => {
				let (_key, atom) = leaf_entry(bytes, area_start, count, middle)?;
				return DiskAtomRef::from_raw(atom).map(Some);
			}
		}
	}
	Ok(None)
}

fn validate_leaf_layout(page: &AlignedPage) -> FerrosGrynnResult<(usize, usize)> {
	let bytes = page.as_bytes();
	if page.validate(PageNo::new(read_u64(bytes, PAGE_NO_OFFSET)))?.flags() != PageFlags::LEAF
		|| read_u16(bytes, LEAF_RESERVED_OFFSET) != 0
	{
		return Err(invalid_index("invalid primary leaf flags or padding"));
	}
	let count = usize::from(read_u16(bytes, LEAF_COUNT_OFFSET));
	let area_start = LEAF_OFFSETS_OFFSET
		.checked_add(count * size_of::<u16>())
		.ok_or_else(|| invalid_index("primary leaf directory overflows usize"))?;
	if area_start > PAGE_SIZE {
		return Err(invalid_index("primary leaf directory exceeds its page"));
	}
	Ok((count, area_start))
}

fn leaf_entry(
	bytes: &[u8; PAGE_SIZE],
	area_start: usize,
	count: usize,
	index: usize,
) -> FerrosGrynnResult<(&[u8], u64)> {
	let start = area_start + usize::from(read_u16(bytes, LEAF_OFFSETS_OFFSET + index * 2));
	let end = if index + 1 == count {
		PAGE_SIZE
	} else {
		area_start + usize::from(read_u16(bytes, LEAF_OFFSETS_OFFSET + (index + 1) * 2))
	};
	if start > end || end > PAGE_SIZE || end.saturating_sub(start) <= DISK_ATOM_REF_BYTES {
		return Err(invalid_index("primary leaf entry range is invalid"));
	}
	let atom_offset = end - DISK_ATOM_REF_BYTES;
	Ok((&bytes[start..atom_offset], read_u64(bytes, atom_offset)))
}

fn select_internal_child(page: &AlignedPage, target: &[u8]) -> FerrosGrynnResult<PageNo> {
	let bytes = page.as_bytes();
	let header = page.validate(PageNo::new(read_u64(bytes, PAGE_NO_OFFSET)))?;
	if !header.flags().is_empty() {
		return Err(invalid_index("internal B+tree page has flags"));
	}
	let count = usize::from(read_u16(bytes, INTERNAL_COUNT_OFFSET));
	let key_bytes = usize::from(read_u16(bytes, INTERNAL_KEY_BYTES_OFFSET));
	let key_area = INTERNAL_OFFSETS_OFFSET + count * size_of::<u16>();
	let child_area = PAGE_SIZE
		.checked_sub((count + 1) * size_of::<u64>())
		.ok_or_else(|| invalid_index("internal child area underflows"))?;
	if count == 0 || key_area + key_bytes > child_area {
		return Err(invalid_index("internal B+tree page areas are invalid"));
	}
	let mut child = count;
	let mut low = 0;
	let mut high = count;
	while low < high {
		let middle = low + (high - low) / 2;
		let start = key_area + usize::from(read_u16(bytes, INTERNAL_OFFSETS_OFFSET + middle * 2));
		let end = if middle + 1 == count {
			key_area + key_bytes
		} else {
			key_area + usize::from(read_u16(bytes, INTERNAL_OFFSETS_OFFSET + (middle + 1) * 2))
		};
		if start >= end || end > key_area + key_bytes {
			return Err(invalid_index("internal separator range is invalid"));
		}
		if target < &bytes[start..end] {
			high = middle;
			child = middle;
		} else {
			low = middle + 1;
		}
	}
	Ok(PageNo::new(read_u64(bytes, child_area + child * size_of::<u64>())))
}

fn validate_zero(bytes: &[u8], field: &str) -> FerrosGrynnResult<()> {
	if bytes.iter().any(|byte| *byte != 0) {
		return Err(invalid_index(format!("{field} are non-zero")));
	}
	Ok(())
}

fn read_u16(bytes: &[u8], offset: usize) -> u16 {
	u16::from_le_bytes([bytes[offset], bytes[offset + 1]])
}

fn read_u32(bytes: &[u8], offset: usize) -> u32 {
	u32::from_le_bytes([bytes[offset], bytes[offset + 1], bytes[offset + 2], bytes[offset + 3]])
}

fn read_u64(bytes: &[u8], offset: usize) -> u64 {
	u64::from_le_bytes([
		bytes[offset],
		bytes[offset + 1],
		bytes[offset + 2],
		bytes[offset + 3],
		bytes[offset + 4],
		bytes[offset + 5],
		bytes[offset + 6],
		bytes[offset + 7],
	])
}

fn write_u16(bytes: &mut [u8], offset: usize, value: u16) {
	bytes[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
}

fn write_u32(bytes: &mut [u8], offset: usize, value: u32) {
	bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

fn write_u64(bytes: &mut [u8], offset: usize, value: u64) {
	bytes[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
}

fn invalid_index(message: impl Into<String>) -> FerrosGrynnError {
	FerrosGrynnError::invalid_storage_file(message)
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::values::runtime::graph::AtomKind;

	#[test]
	fn label_dictionary_round_trips_multiple_sections_and_pages() -> FerrosGrynnResult<()> {
		let dictionary = LabelDictionary {
			node_labels: (0..1_200).map(|id| format!("node-{id:04}")).collect(),
			edge_labels: vec!["knows".to_owned()],
			property_keys: vec!["name".to_owned(), "age".to_owned()],
			table_names: vec!["person".to_owned()],
			time_zones: vec!["Europe/Moscow".to_owned(), "America/New_York".to_owned()],
		};
		let pages = encode_label_dictionary(&dictionary, PageLsn::ZERO)?;
		let decoded = decode_label_dictionary(|page_no, output| {
			let page = pages
				.get(page_no.get() as usize)
				.ok_or_else(|| invalid_index("test page missing"))?;
			output.as_bytes_mut().copy_from_slice(page.as_bytes());
			Ok(())
		})?;
		assert_eq!(decoded, dictionary);
		Ok(())
	}

	#[test]
	fn bulk_primary_tree_supports_multiple_levels_and_exact_lookup() -> FerrosGrynnResult<()> {
		let entries = (0..4_000_u64)
			.map(|id| {
				Ok(PrimaryIndexEntry {
					key: format!("person:{id:08}").into_bytes(),
					atom: DiskAtomRef::from_parts(AtomKind::Node, id)?,
				})
			})
			.collect::<FerrosGrynnResult<Vec<_>>>()?;
		let built = build_primary_index(&entries, PageLsn::ZERO)?;
		assert!(built.pages.len() > 1);
		let read = |page_no: PageNo, output: &mut AlignedPage| {
			let page = built
				.pages
				.get(page_no.get() as usize)
				.ok_or_else(|| invalid_index("test page missing"))?;
			output.as_bytes_mut().copy_from_slice(page.as_bytes());
			Ok(())
		};
		assert_eq!(
			lookup_primary_index(read, built.root, b"person:00003999")?.map(DiskAtomRef::slot),
			Some(3_999)
		);
		assert_eq!(lookup_primary_index(read, built.root, b"person:missing")?, None);
		let scanned = scan_primary_index(
			|page_no, output| {
				let page = built
					.pages
					.get(page_no.get() as usize)
					.ok_or_else(|| invalid_index("test page missing"))?;
				output.as_bytes_mut().copy_from_slice(page.as_bytes());
				Ok(())
			},
			built.root,
		)?;
		assert_eq!(scanned, entries);
		Ok(())
	}

	#[test]
	fn primary_tree_rejects_duplicate_keys() -> FerrosGrynnResult<()> {
		let atom = DiskAtomRef::from_parts(AtomKind::Node, 0)?;
		let entries = vec![
			PrimaryIndexEntry {
				key: b"same".to_vec(),
				atom,
			},
			PrimaryIndexEntry {
				key: b"same".to_vec(),
				atom,
			},
		];
		assert!(build_primary_index(&entries, PageLsn::ZERO).is_err());
		Ok(())
	}
}
