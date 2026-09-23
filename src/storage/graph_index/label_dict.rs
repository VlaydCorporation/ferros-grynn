//! Exact graph-local node/edge label dictionary and hash range.

use std::collections::{BTreeMap, BTreeSet};

use rapidhash::v3::{DEFAULT_RAPID_SECRETS, rapidhash_v3_seeded};

use super::*;

const ROOT_PAGE: PageNo = PageNo::new(0);
const NODE_COUNT_OFFSET: usize = 32;
const EDGE_COUNT_OFFSET: usize = 36;
const NODE_FIRST_OFFSET: usize = 40;
const NODE_LAST_OFFSET: usize = 48;
const EDGE_FIRST_OFFSET: usize = 56;
const EDGE_LAST_OFFSET: usize = 64;
const HASH_FIRST_OFFSET: usize = 72;
const HASH_PAGE_COUNT_OFFSET: usize = 80;
const HASH_CAPACITY_OFFSET: usize = 84;
const ROOT_RESERVED_OFFSET: usize = 88;

const ENTRY_NEXT_OFFSET: usize = 32;
const ENTRY_FIRST_ID_OFFSET: usize = 40;
const ENTRY_COUNT_OFFSET: usize = 44;
const ENTRY_USED_OFFSET: usize = 46;
const ENTRY_KIND_OFFSET: usize = 48;
const ENTRY_RESERVED_OFFSET: usize = 49;
const ENTRY_DATA_OFFSET: usize = 56;

const HASH_NEXT_OFFSET: usize = 32;
const HASH_USED_OFFSET: usize = 40;
const HASH_CAPACITY_PAGE_OFFSET: usize = 44;
const HASH_DATA_OFFSET: usize = 48;
const HASH_BUCKET_BYTES: usize = 20;
const HASH_BUCKETS_PER_PAGE: usize = (PAGE_SIZE - HASH_DATA_OFFSET) / HASH_BUCKET_BYTES;
const HASH_TAIL_OFFSET: usize = HASH_DATA_OFFSET + HASH_BUCKETS_PER_PAGE * HASH_BUCKET_BYTES;

/// Namespace encoded in every label entry page and hash key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[repr(u8)]
pub(in crate::storage) enum LabelKind {
	Node = 0,
	Edge = 1,
}

impl LabelKind {
	fn from_raw(raw: u8) -> FerrosGrynnResult<Self> {
		match raw {
			0 => Ok(Self::Node),
			1 => Ok(Self::Edge),
			_ => Err(invalid_index("label entry page contains an unknown kind")),
		}
	}
}

/// Dense graph-local label namespaces.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(in crate::storage) struct LabelDictionary {
	pub(in crate::storage) node_labels: Vec<String>,
	pub(in crate::storage) edge_labels: Vec<String>,
}

impl LabelDictionary {
	/// Builds stable sorted label namespaces from graph occurrences.
	pub(in crate::storage) fn from_labels<'a>(
		labels: impl IntoIterator<Item = (LabelKind, &'a str)>,
	) -> FerrosGrynnResult<Self> {
		let mut nodes = BTreeSet::new();
		let mut edges = BTreeSet::new();
		for (kind, name) in labels {
			validate_name(name)?;
			match kind {
				LabelKind::Node => {
					nodes.insert(name.to_owned());
				}
				LabelKind::Edge => {
					edges.insert(name.to_owned());
				}
			}
		}
		Ok(Self {
			node_labels: nodes.into_iter().collect(),
			edge_labels: edges.into_iter().collect(),
		})
	}

	/// Resolves one runtime label to its namespace-local dense identifier.
	pub(in crate::storage) fn id(&self, kind: LabelKind, name: &str) -> Option<u32> {
		self.labels(kind)
			.binary_search_by(|candidate| candidate.as_str().cmp(name))
			.ok()
			.and_then(|id| u32::try_from(id).ok())
	}

	/// Resolves one namespace-local identifier to its UTF-8 label.
	pub(in crate::storage) fn name(&self, kind: LabelKind, id: u32) -> Option<&str> {
		usize::try_from(id).ok().and_then(|id| self.labels(kind).get(id)).map(String::as_str)
	}

	fn labels(&self, kind: LabelKind) -> &[String] {
		match kind {
			LabelKind::Node => &self.node_labels,
			LabelKind::Edge => &self.edge_labels,
		}
	}
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct NameLocation {
	page: PageNo,
	offset: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct LabelBinding {
	kind: LabelKind,
	id: u32,
	location: NameLocation,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct HashBucket {
	hash: u32,
	binding: LabelBinding,
}

/// Encodes page zero, two disjoint entry chains, and one complete hash range.
pub(in crate::storage) fn encode_label_dictionary(
	dictionary: &LabelDictionary,
	page_lsn: PageLsn,
) -> FerrosGrynnResult<Vec<AlignedPage>> {
	validate_dictionary(dictionary)?;
	let mut pages = vec![AlignedPage::zeroed()];
	let (node_first, node_last, node_locations) =
		append_entries(&mut pages, LabelKind::Node, &dictionary.node_labels, page_lsn)?;
	let (edge_first, edge_last, edge_locations) =
		append_entries(&mut pages, LabelKind::Edge, &dictionary.edge_labels, page_lsn)?;
	let bindings = bindings(node_locations, edge_locations)?;
	let (hash_first, hash_pages, hash_capacity) =
		append_hash(&mut pages, dictionary, &bindings, page_lsn)?;

	let mut root = AlignedPage::new(PageType::LabelDict, PageFlags::empty(), page_lsn, ROOT_PAGE);
	write_u32(root.as_bytes_mut(), NODE_COUNT_OFFSET, u32_len(dictionary.node_labels.len())?);
	write_u32(root.as_bytes_mut(), EDGE_COUNT_OFFSET, u32_len(dictionary.edge_labels.len())?);
	write_u64(root.as_bytes_mut(), NODE_FIRST_OFFSET, node_first.get());
	write_u64(root.as_bytes_mut(), NODE_LAST_OFFSET, node_last.get());
	write_u64(root.as_bytes_mut(), EDGE_FIRST_OFFSET, edge_first.get());
	write_u64(root.as_bytes_mut(), EDGE_LAST_OFFSET, edge_last.get());
	write_u64(root.as_bytes_mut(), HASH_FIRST_OFFSET, hash_first.get());
	write_u32(root.as_bytes_mut(), HASH_PAGE_COUNT_OFFSET, hash_pages);
	write_u32(root.as_bytes_mut(), HASH_CAPACITY_OFFSET, hash_capacity);
	root.seal()?;
	pages[0] = root;
	Ok(pages)
}

/// Decodes and cross-validates both entry chains and every hash bucket.
pub(in crate::storage) fn decode_label_dictionary(
	mut read_page: impl FnMut(PageNo, &mut AlignedPage) -> FerrosGrynnResult<()>,
) -> FerrosGrynnResult<LabelDictionary> {
	let mut root = AlignedPage::zeroed();
	read_page(ROOT_PAGE, &mut root)?;
	let header = root.validate(ROOT_PAGE)?;
	if header.page_type() != PageType::LabelDict || !header.flags().is_empty() {
		return Err(invalid_index("invalid LabelDict root page"));
	}
	let bytes = root.as_bytes();
	validate_zero(&bytes[ROOT_RESERVED_OFFSET..], "LabelDict root reserved bytes")?;
	let node_count = usize_len(read_u32(bytes, NODE_COUNT_OFFSET))?;
	let edge_count = usize_len(read_u32(bytes, EDGE_COUNT_OFFSET))?;
	let (node_labels, node_locations) = decode_entries(
		&mut read_page,
		LabelKind::Node,
		node_count,
		PageNo::new(read_u64(bytes, NODE_FIRST_OFFSET)),
		PageNo::new(read_u64(bytes, NODE_LAST_OFFSET)),
	)?;
	let (edge_labels, edge_locations) = decode_entries(
		&mut read_page,
		LabelKind::Edge,
		edge_count,
		PageNo::new(read_u64(bytes, EDGE_FIRST_OFFSET)),
		PageNo::new(read_u64(bytes, EDGE_LAST_OFFSET)),
	)?;
	let dictionary = LabelDictionary {
		node_labels,
		edge_labels,
	};
	validate_dictionary(&dictionary)?;
	let bindings = bindings(node_locations, edge_locations)?;
	decode_hash(
		&mut read_page,
		&dictionary,
		&bindings,
		PageNo::new(read_u64(bytes, HASH_FIRST_OFFSET)),
		read_u32(bytes, HASH_PAGE_COUNT_OFFSET),
		read_u32(bytes, HASH_CAPACITY_OFFSET),
	)?;
	Ok(dictionary)
}

fn append_entries(
	pages: &mut Vec<AlignedPage>,
	kind: LabelKind,
	labels: &[String],
	page_lsn: PageLsn,
) -> FerrosGrynnResult<(PageNo, PageNo, Vec<NameLocation>)> {
	if labels.is_empty() {
		return Ok((PageNo::NULL, PageNo::NULL, Vec::new()));
	}
	let first = page_no(pages.len())?;
	let first_index = pages.len();
	let mut locations = Vec::with_capacity(labels.len());
	let mut cursor = 0_usize;
	while cursor < labels.len() {
		let number = page_no(pages.len())?;
		let first_id = u32_len(cursor)?;
		let mut page =
			AlignedPage::new(PageType::LabelDictEntry, PageFlags::empty(), page_lsn, number);
		write_u32(page.as_bytes_mut(), ENTRY_FIRST_ID_OFFSET, first_id);
		page.as_bytes_mut()[ENTRY_KIND_OFFSET] = kind as u8;
		let mut offset = ENTRY_DATA_OFFSET;
		let mut count = 0_u16;
		while let Some(name) = labels.get(cursor) {
			let encoded = 2_usize
				.checked_add(name.len())
				.ok_or_else(|| invalid_index("label entry length overflows usize"))?;
			if offset + encoded > PAGE_SIZE {
				break;
			}
			locations.push(NameLocation {
				page: number,
				offset: u32_len(offset)?,
			});
			write_u16(page.as_bytes_mut(), offset, u16_len(name.len())?);
			offset += 2;
			page.as_bytes_mut()[offset..offset + name.len()].copy_from_slice(name.as_bytes());
			offset += name.len();
			cursor += 1;
			count = count
				.checked_add(1)
				.ok_or_else(|| invalid_index("label entry count exceeds u16"))?;
		}
		if count == 0 {
			return Err(invalid_index("one label does not fit an entry page"));
		}
		write_u16(page.as_bytes_mut(), ENTRY_COUNT_OFFSET, count);
		write_u16(page.as_bytes_mut(), ENTRY_USED_OFFSET, u16_len(offset - ENTRY_DATA_OFFSET)?);
		pages.push(page);
	}
	let last_index = pages.len() - 1;
	let last = page_no(last_index)?;
	for index in first_index..=last_index {
		let next = if index == last_index {
			PageNo::NULL
		} else {
			page_no(index + 1)?
		};
		write_u64(pages[index].as_bytes_mut(), ENTRY_NEXT_OFFSET, next.get());
		pages[index].seal()?;
	}
	Ok((first, last, locations))
}

fn decode_entries(
	read_page: &mut impl FnMut(PageNo, &mut AlignedPage) -> FerrosGrynnResult<()>,
	kind: LabelKind,
	expected: usize,
	first: PageNo,
	last: PageNo,
) -> FerrosGrynnResult<(Vec<String>, Vec<NameLocation>)> {
	if expected == 0 {
		return if first == PageNo::NULL && last == PageNo::NULL {
			Ok((Vec::new(), Vec::new()))
		} else {
			Err(invalid_index("empty label namespace has a non-null chain"))
		};
	}
	if first == PageNo::NULL || last == PageNo::NULL {
		return Err(invalid_index("non-empty label namespace has a null chain"));
	}
	let mut labels = Vec::with_capacity(expected);
	let mut locations = Vec::with_capacity(expected);
	let mut visited = BTreeSet::new();
	let mut current = first;
	loop {
		if !visited.insert(current) {
			return Err(invalid_index("label entry chain contains a cycle"));
		}
		let mut page = AlignedPage::zeroed();
		read_page(current, &mut page)?;
		let header = page.validate(current)?;
		if header.page_type() != PageType::LabelDictEntry || !header.flags().is_empty() {
			return Err(invalid_index("invalid LabelDictEntry page"));
		}
		let bytes = page.as_bytes();
		if LabelKind::from_raw(bytes[ENTRY_KIND_OFFSET])? != kind {
			return Err(invalid_index("label entry chains cross namespaces"));
		}
		validate_zero(
			&bytes[ENTRY_RESERVED_OFFSET..ENTRY_DATA_OFFSET],
			"LabelDictEntry reserved bytes",
		)?;
		if usize_len(read_u32(bytes, ENTRY_FIRST_ID_OFFSET))? != labels.len() {
			return Err(invalid_index("label entry IDs are not dense"));
		}
		let count = usize::from(read_u16(bytes, ENTRY_COUNT_OFFSET));
		let used = usize::from(read_u16(bytes, ENTRY_USED_OFFSET));
		if count == 0 || used > PAGE_SIZE - ENTRY_DATA_OFFSET {
			return Err(invalid_index("label entry count or used bytes are invalid"));
		}
		let end = ENTRY_DATA_OFFSET + used;
		let mut offset = ENTRY_DATA_OFFSET;
		for _ in 0..count {
			if offset + 2 > end {
				return Err(invalid_index("label entry header exceeds used bytes"));
			}
			let name_offset = offset;
			let length = usize::from(read_u16(bytes, offset));
			offset += 2;
			let name_end = offset
				.checked_add(length)
				.ok_or_else(|| invalid_index("label entry end overflows usize"))?;
			if name_end > end {
				return Err(invalid_index("label entry exceeds used bytes"));
			}
			let name = std::str::from_utf8(&bytes[offset..name_end])
				.map_err(|_error| invalid_index("label entry is not UTF-8"))?;
			validate_name(name)?;
			labels.push(name.to_owned());
			locations.push(NameLocation {
				page: current,
				offset: u32_len(name_offset)?,
			});
			offset = name_end;
		}
		if offset != end {
			return Err(invalid_index("label used byte count is not exact"));
		}
		validate_zero(&bytes[end..], "LabelDictEntry trailing bytes")?;
		let next = PageNo::new(read_u64(bytes, ENTRY_NEXT_OFFSET));
		if current == last {
			if next != PageNo::NULL {
				return Err(invalid_index("label chain continues past its declared last page"));
			}
			break;
		}
		if next == PageNo::NULL {
			return Err(invalid_index("label chain ended before its declared last page"));
		}
		current = next;
	}
	if labels.len() != expected {
		return Err(invalid_index("label chain count disagrees with its root"));
	}
	Ok((labels, locations))
}

fn bindings(
	nodes: Vec<NameLocation>,
	edges: Vec<NameLocation>,
) -> FerrosGrynnResult<Vec<LabelBinding>> {
	let mut result = Vec::with_capacity(nodes.len() + edges.len());
	for (kind, locations) in [(LabelKind::Node, nodes), (LabelKind::Edge, edges)] {
		for (id, location) in locations.into_iter().enumerate() {
			result.push(LabelBinding {
				kind,
				id: u32_len(id)?,
				location,
			});
		}
	}
	Ok(result)
}

fn append_hash(
	pages: &mut Vec<AlignedPage>,
	dictionary: &LabelDictionary,
	bindings: &[LabelBinding],
	page_lsn: PageLsn,
) -> FerrosGrynnResult<(PageNo, u32, u32)> {
	if bindings.is_empty() {
		return Ok((PageNo::NULL, 0, 0));
	}
	let minimum = bindings
		.len()
		.checked_mul(4)
		.and_then(|value| value.checked_add(2))
		.map(|value| value / 3)
		.ok_or_else(|| invalid_index("label hash capacity overflows usize"))?;
	let page_count = minimum.div_ceil(HASH_BUCKETS_PER_PAGE).max(1);
	let capacity = page_count
		.checked_mul(HASH_BUCKETS_PER_PAGE)
		.ok_or_else(|| invalid_index("label hash range overflows usize"))?;
	let mut buckets = vec![None; capacity];
	for binding in bindings {
		let name = dictionary
			.name(binding.kind, binding.id)
			.ok_or_else(|| invalid_index("label hash binding has no name"))?;
		place_bucket(&mut buckets, HashBucket {
			hash: label_hash(binding.kind, name)?,
			binding: *binding,
		})?;
	}
	let first = page_no(pages.len())?;
	for ordinal in 0..page_count {
		let number = page_no(pages.len())?;
		let next = if ordinal + 1 == page_count {
			PageNo::NULL
		} else {
			page_no(pages.len() + 1)?
		};
		let start = ordinal * HASH_BUCKETS_PER_PAGE;
		let page_buckets = &buckets[start..start + HASH_BUCKETS_PER_PAGE];
		let mut page = AlignedPage::new(PageType::HashIndex, PageFlags::empty(), page_lsn, number);
		write_u64(page.as_bytes_mut(), HASH_NEXT_OFFSET, next.get());
		write_u32(
			page.as_bytes_mut(),
			HASH_USED_OFFSET,
			u32_len(page_buckets.iter().filter(|bucket| bucket.is_some()).count())?,
		);
		write_u32(page.as_bytes_mut(), HASH_CAPACITY_PAGE_OFFSET, u32_len(HASH_BUCKETS_PER_PAGE)?);
		for (slot, bucket) in page_buckets.iter().enumerate() {
			if let Some(bucket) = bucket {
				write_bucket(page.as_bytes_mut(), slot, *bucket)?;
			}
		}
		page.seal()?;
		pages.push(page);
	}
	Ok((first, u32_len(page_count)?, u32_len(capacity)?))
}

fn decode_hash(
	read_page: &mut impl FnMut(PageNo, &mut AlignedPage) -> FerrosGrynnResult<()>,
	dictionary: &LabelDictionary,
	bindings: &[LabelBinding],
	first: PageNo,
	page_count: u32,
	capacity: u32,
) -> FerrosGrynnResult<()> {
	if bindings.is_empty() {
		return if first == PageNo::NULL && page_count == 0 && capacity == 0 {
			Ok(())
		} else {
			Err(invalid_index("empty label dictionary has a non-empty hash range"))
		};
	}
	let pages = usize_len(page_count)?;
	let expected_capacity = pages
		.checked_mul(HASH_BUCKETS_PER_PAGE)
		.ok_or_else(|| invalid_index("label hash capacity overflows usize"))?;
	if first == PageNo::NULL || usize_len(capacity)? != expected_capacity {
		return Err(invalid_index("label hash root or capacity is invalid"));
	}
	let expected: BTreeMap<NameLocation, LabelBinding> =
		bindings.iter().map(|binding| (binding.location, *binding)).collect();
	let mut found = BTreeSet::new();
	let mut current = first;
	for ordinal in 0..pages {
		let mut page = AlignedPage::zeroed();
		read_page(current, &mut page)?;
		let header = page.validate(current)?;
		if header.page_type() != PageType::HashIndex || !header.flags().is_empty() {
			return Err(invalid_index("invalid graph-label hash page"));
		}
		let bytes = page.as_bytes();
		if usize_len(read_u32(bytes, HASH_CAPACITY_PAGE_OFFSET))? != HASH_BUCKETS_PER_PAGE {
			return Err(invalid_index("graph-label hash page capacity is not canonical"));
		}
		validate_zero(&bytes[HASH_TAIL_OFFSET..], "graph-label hash tail")?;
		let mut used = 0_u32;
		for slot in 0..HASH_BUCKETS_PER_PAGE {
			let Some(mut bucket) = read_bucket(bytes, slot)? else {
				continue;
			};
			used = used
				.checked_add(1)
				.ok_or_else(|| invalid_index("label hash used count overflows u32"))?;
			let expected_binding = expected
				.get(&bucket.binding.location)
				.ok_or_else(|| invalid_index("label hash points outside both entry chains"))?;
			bucket.binding.kind = expected_binding.kind;
			if bucket.binding.id != expected_binding.id
				|| !found.insert((bucket.binding.kind, bucket.binding.id))
			{
				return Err(invalid_index("label hash binding is duplicated or cross-kind"));
			}
			let name = dictionary
				.name(bucket.binding.kind, bucket.binding.id)
				.ok_or_else(|| invalid_index("label hash ID exceeds its namespace"))?;
			if bucket.hash != label_hash(bucket.binding.kind, name)? {
				return Err(invalid_index("label hash does not match its canonical key"));
			}
		}
		if used != read_u32(bytes, HASH_USED_OFFSET) {
			return Err(invalid_index("label hash used count is invalid"));
		}
		let next = PageNo::new(read_u64(bytes, HASH_NEXT_OFFSET));
		if ordinal + 1 == pages {
			if next != PageNo::NULL {
				return Err(invalid_index("label hash continues past its page count"));
			}
		} else if next == PageNo::NULL {
			return Err(invalid_index("label hash range ended early"));
		}
		current = next;
	}
	if found.len() != bindings.len() {
		return Err(invalid_index("label hash does not bind every label"));
	}
	Ok(())
}

fn label_hash(kind: LabelKind, name: &str) -> FerrosGrynnResult<u32> {
	let mut key = Vec::with_capacity(3 + name.len());
	key.push(kind as u8);
	key.extend_from_slice(&u16_len(name.len())?.to_le_bytes());
	key.extend_from_slice(name.as_bytes());
	Ok(rapidhash_v3_seeded(&key, &DEFAULT_RAPID_SECRETS) as u32)
}

fn place_bucket(buckets: &mut [Option<HashBucket>], bucket: HashBucket) -> FerrosGrynnResult<()> {
	let start = bucket.hash as usize % buckets.len();
	for distance in 0..buckets.len() {
		let index = (start + distance) % buckets.len();
		if buckets[index].is_none() {
			buckets[index] = Some(bucket);
			return Ok(());
		}
	}
	Err(invalid_index("label hash range has no free bucket"))
}

fn write_bucket(bytes: &mut [u8], slot: usize, bucket: HashBucket) -> FerrosGrynnResult<()> {
	let offset = hash_offset(slot)?;
	write_u32(bytes, offset, bucket.hash);
	write_u32(bytes, offset + 4, bucket.binding.id);
	write_u64(bytes, offset + 8, bucket.binding.location.page.get());
	write_u32(bytes, offset + 16, bucket.binding.location.offset);
	Ok(())
}

fn read_bucket(bytes: &[u8], slot: usize) -> FerrosGrynnResult<Option<HashBucket>> {
	let offset = hash_offset(slot)?;
	let hash = read_u32(bytes, offset);
	let id = read_u32(bytes, offset + 4);
	let raw_page = read_u64(bytes, offset + 8);
	let page = PageNo::new(raw_page);
	let name_offset = read_u32(bytes, offset + 16);
	if raw_page == 0 {
		if hash != 0 || id != 0 || name_offset != 0 {
			return Err(invalid_index("empty label hash bucket is not all-zero"));
		}
		return Ok(None);
	}
	Ok(Some(HashBucket {
		hash,
		binding: LabelBinding {
			kind: LabelKind::Node,
			id,
			location: NameLocation {
				page,
				offset: name_offset,
			},
		},
	}))
}

fn hash_offset(slot: usize) -> FerrosGrynnResult<usize> {
	if slot >= HASH_BUCKETS_PER_PAGE {
		return Err(invalid_index("label hash slot exceeds its page"));
	}
	Ok(HASH_DATA_OFFSET + slot * HASH_BUCKET_BYTES)
}

fn validate_dictionary(dictionary: &LabelDictionary) -> FerrosGrynnResult<()> {
	for labels in [&dictionary.node_labels, &dictionary.edge_labels] {
		let mut previous: Option<&str> = None;
		for label in labels {
			validate_name(label)?;
			if previous.is_some_and(|previous| previous >= label.as_str()) {
				return Err(invalid_index("label namespace is not strictly sorted"));
			}
			previous = Some(label);
		}
	}
	Ok(())
}

fn validate_name(name: &str) -> FerrosGrynnResult<()> {
	if name.is_empty() || name.contains('\0') || name.len() > usize::from(u16::MAX) {
		return Err(invalid_index("label is empty, contains NUL, or exceeds u16"));
	}
	Ok(())
}

fn validate_zero(bytes: &[u8], field: &str) -> FerrosGrynnResult<()> {
	if bytes.iter().any(|byte| *byte != 0) {
		return Err(invalid_index(format!("{field} are non-zero")));
	}
	Ok(())
}

fn page_no(index: usize) -> FerrosGrynnResult<PageNo> {
	u64::try_from(index)
		.map(PageNo::new)
		.map_err(|_error| invalid_index("label dictionary page number exceeds u64"))
}

fn u16_len(value: usize) -> FerrosGrynnResult<u16> {
	u16::try_from(value).map_err(|_error| invalid_index("label dictionary value exceeds u16"))
}

fn u32_len(value: usize) -> FerrosGrynnResult<u32> {
	u32::try_from(value).map_err(|_error| invalid_index("label dictionary value exceeds u32"))
}

fn usize_len(value: u32) -> FerrosGrynnResult<usize> {
	usize::try_from(value).map_err(|_error| invalid_index("label dictionary value exceeds usize"))
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn exact_two_namespace_dictionary_round_trips_and_hashes_kind() -> FerrosGrynnResult<()> {
		let dictionary = LabelDictionary::from_labels([
			(LabelKind::Node, "shared"),
			(LabelKind::Edge, "shared"),
			(LabelKind::Node, "person"),
		])?;
		let pages = encode_label_dictionary(&dictionary, PageLsn::new(7))?;
		let decoded = decode_label_dictionary(|number, output| {
			let index = usize::try_from(number.get())
				.map_err(|_error| invalid_index("test page exceeds usize"))?;
			output.as_bytes_mut().copy_from_slice(
				pages
					.get(index)
					.ok_or_else(|| invalid_index("test label page is absent"))?
					.as_bytes(),
			);
			Ok(())
		})?;
		assert_eq!(decoded, dictionary);
		assert_eq!(decoded.id(LabelKind::Node, "shared"), Some(1));
		assert_eq!(decoded.id(LabelKind::Edge, "shared"), Some(0));
		Ok(())
	}
}
