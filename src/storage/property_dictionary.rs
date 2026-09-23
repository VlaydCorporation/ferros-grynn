//! Append-only page codec for the database-global property-key dictionary.

use std::{collections::BTreeSet, mem::size_of};

use rapidhash::v3::{DEFAULT_RAPID_SECRETS, rapidhash_v3_seeded};

use super::page::{AlignedPage, PAGE_HEADER_SIZE, PAGE_SIZE, PageFlags, PageLsn, PageNo, PageType};
use crate::{
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	values::{PropertyDictionary, PropertyKeyId, Text},
};

/// Fixed page number of the property-dictionary root.
const ROOT_PAGE: PageNo = PageNo::new(0);
/// Root offset of the number of stable dictionary entries.
const ROOT_ENTRY_COUNT_OFFSET: usize = PAGE_HEADER_SIZE;
/// Root offset of the reachable entry-page count.
const ROOT_ENTRY_PAGE_COUNT_OFFSET: usize = ROOT_ENTRY_COUNT_OFFSET + 4;
/// Root offset of the first reachable entry page.
const ROOT_ENTRY_FIRST_PAGE_OFFSET: usize = ROOT_ENTRY_PAGE_COUNT_OFFSET + 4;
/// Root offset of the last reachable entry page.
const ROOT_ENTRY_LAST_PAGE_OFFSET: usize = ROOT_ENTRY_FIRST_PAGE_OFFSET + 8;
/// Root offset of the current hash-range first page.
const ROOT_HASH_FIRST_PAGE_OFFSET: usize = ROOT_ENTRY_LAST_PAGE_OFFSET + 8;
/// Root offset of the current hash-range page count.
const ROOT_HASH_PAGE_COUNT_OFFSET: usize = ROOT_HASH_FIRST_PAGE_OFFSET + 8;
/// Root offset of the logical hash-bucket capacity.
const ROOT_HASH_CAPACITY_OFFSET: usize = ROOT_HASH_PAGE_COUNT_OFFSET + 4;
/// First root byte reserved as zero for format evolution.
const ROOT_RESERVED_OFFSET: usize = ROOT_HASH_CAPACITY_OFFSET + 4;

/// Entry-page offset of the next linked page number.
const ENTRY_NEXT_PAGE_OFFSET: usize = PAGE_HEADER_SIZE;
/// Entry-page offset of the first dense property ID.
const ENTRY_FIRST_ID_OFFSET: usize = ENTRY_NEXT_PAGE_OFFSET + 8;
/// Entry-page offset of the encoded name count.
const ENTRY_COUNT_OFFSET: usize = ENTRY_FIRST_ID_OFFSET + 4;
/// Entry-page offset of the used payload byte count.
const ENTRY_USED_BYTES_OFFSET: usize = ENTRY_COUNT_OFFSET + 2;
/// Entry-page offset at which length-prefixed names begin.
const ENTRY_DATA_OFFSET: usize = ENTRY_USED_BYTES_OFFSET + 2;
/// Maximum bytes available for names in one entry page.
const ENTRY_DATA_BYTES: usize = PAGE_SIZE - ENTRY_DATA_OFFSET;

/// Hash-page offset of the next page in the current range.
const HASH_NEXT_PAGE_OFFSET: usize = PAGE_HEADER_SIZE;
/// Hash-page offset of the number of occupied buckets.
const HASH_USED_OFFSET: usize = HASH_NEXT_PAGE_OFFSET + 8;
/// Hash-page offset of the fixed bucket capacity.
const HASH_CAPACITY_OFFSET: usize = HASH_USED_OFFSET + 4;
/// Hash-page offset at which fixed-size buckets begin.
const HASH_DATA_OFFSET: usize = HASH_CAPACITY_OFFSET + 4;
/// Encoded bytes in one `{hash, id, page, offset}` bucket.
const HASH_BUCKET_BYTES: usize = 20;
/// Number of complete hash buckets fitting in one page.
const HASH_BUCKETS_PER_PAGE: usize = (PAGE_SIZE - HASH_DATA_OFFSET) / HASH_BUCKET_BYTES;
/// First trailing byte that must remain zero.
const HASH_TAIL_OFFSET: usize = HASH_DATA_OFFSET + HASH_BUCKETS_PER_PAGE * HASH_BUCKET_BYTES;

/// Complete physical image of one property dictionary generation.
pub(super) struct BuiltPropertyDictionary {
	/// Reachable and obsolete append-only pages below the published ceiling.
	pub(super) pages: Vec<AlignedPage>,
}

#[derive(Debug, Clone, Copy)]
/// Decoded root pointers and counts for one published dictionary version.
struct Root {
	/// Number of dense property-key IDs.
	entry_count: u32,
	/// Number of pages in the reachable entry chain.
	entry_page_count: u32,
	/// First page in the reachable entry chain.
	entry_first_page: PageNo,
	/// Last page in the reachable entry chain.
	entry_last_page: PageNo,
	/// First page of the current replacement hash range.
	hash_first_page: PageNo,
	/// Number of pages in the current hash range.
	hash_page_count: u32,
	/// Total number of buckets in the current hash range.
	hash_capacity: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// Physical location of one length-prefixed property name.
struct NameLocation {
	/// Entry page containing the encoded name.
	page: PageNo,
	/// Offset of the name-length field within the page.
	offset: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// One occupied open-addressing hash bucket.
struct HashBucket {
	/// Portable low 32 bits of seeded RapidHash v3.
	hash: u32,
	/// Stable dense property-key identity.
	id: PropertyKeyId,
	/// Full-name location used for collision verification.
	location: NameLocation,
}

/// Fully decoded dictionary plus physical metadata needed by append.
struct Decoded {
	/// Reconstructed runtime dictionary.
	dictionary: PropertyDictionary,
	/// Validated root metadata.
	root: Root,
	/// Name locations in dense ID order.
	locations: Vec<NameLocation>,
}

/// Builds the first append-only property dictionary image.
pub(super) fn build_property_dictionary(
	dictionary: &PropertyDictionary,
	page_lsn: PageLsn,
) -> FerrosGrynnResult<BuiltPropertyDictionary> {
	let mut pages = vec![AlignedPage::zeroed()];
	let names = dictionary_names(dictionary)?;
	let (entry_first_page, entry_last_page, locations, entry_page_count) =
		append_entry_pages(&mut pages, 0, &names, page_lsn)?;
	let (hash_first_page, hash_page_count, hash_capacity) =
		append_hash_range(&mut pages, &names, &locations, page_lsn)?;
	let root = Root {
		entry_count: u32_len(names.len(), "property dictionary entry count")?,
		entry_page_count,
		entry_first_page,
		entry_last_page,
		hash_first_page,
		hash_page_count,
		hash_capacity,
	};
	pages[0] = encode_root(root, page_lsn)?;
	Ok(BuiltPropertyDictionary {
		pages,
	})
}

/// Appends new names and a replacement hash range without reusing old pages.
///
/// Existing dictionary IDs and bytes must be an exact prefix of `dictionary`.
pub(super) fn extend_property_dictionary(
	mut image: BuiltPropertyDictionary,
	dictionary: &PropertyDictionary,
	page_lsn: PageLsn,
) -> FerrosGrynnResult<BuiltPropertyDictionary> {
	let ceiling = u64::try_from(image.pages.len())
		.map_err(|_error| invalid("property dictionary page count exceeds u64"))?;
	let decoded = decode_property_dictionary(&image.pages, ceiling)?;
	let names = dictionary_names(dictionary)?;
	let old_names = dictionary_names(&decoded.dictionary)?;
	if names.len() < old_names.len() || names[..old_names.len()] != old_names {
		return Err(invalid("property dictionary update does not preserve its stable ID prefix"));
	}
	if names.len() == old_names.len() {
		return Ok(image);
	}

	let first_new_page = page_no(image.pages.len())?;
	let (new_first, new_last, new_locations, new_page_count) = append_entry_pages(
		&mut image.pages,
		u32_len(old_names.len(), "property dictionary first appended ID")?,
		&names[old_names.len()..],
		page_lsn,
	)?;
	if new_first != first_new_page {
		return Err(invalid("property dictionary append did not begin at the file tail"));
	}
	let old_last_index = page_index(decoded.root.entry_last_page, image.pages.len())?;
	write_u64(image.pages[old_last_index].as_bytes_mut(), ENTRY_NEXT_PAGE_OFFSET, new_first.get());
	image.pages[old_last_index].seal()?;

	let mut locations = decoded.locations;
	locations.extend(new_locations);
	let (hash_first_page, hash_page_count, hash_capacity) =
		append_hash_range(&mut image.pages, &names, &locations, page_lsn)?;
	let root = Root {
		entry_count: u32_len(names.len(), "property dictionary entry count")?,
		entry_page_count: decoded
			.root
			.entry_page_count
			.checked_add(new_page_count)
			.ok_or_else(|| invalid("property dictionary entry page count overflows u32"))?,
		entry_first_page: decoded.root.entry_first_page,
		entry_last_page: new_last,
		hash_first_page,
		hash_page_count,
		hash_capacity,
	};
	image.pages[0] = encode_root(root, page_lsn)?;
	Ok(image)
}

/// Rebuilds the runtime dictionary from a snapshot-visible page ceiling.
pub(super) fn open_property_dictionary(
	pages: &[AlignedPage],
	page_ceiling: u64,
) -> FerrosGrynnResult<PropertyDictionary> {
	decode_property_dictionary(pages, page_ceiling).map(|decoded| decoded.dictionary)
}

/// Decodes and validates all entry and hash pages reachable below a ceiling.
fn decode_property_dictionary(
	pages: &[AlignedPage],
	page_ceiling: u64,
) -> FerrosGrynnResult<Decoded> {
	let ceiling = usize::try_from(page_ceiling)
		.map_err(|_error| invalid("property dictionary page ceiling exceeds usize"))?;
	if ceiling == 0 || ceiling > pages.len() {
		return Err(invalid("property dictionary page ceiling exceeds its file"));
	}
	let root = decode_root(&pages[0])?;
	validate_root_shape(root)?;
	validate_root_ceiling(root, ceiling)?;

	let mut dictionary = PropertyDictionary::new();
	let mut locations = Vec::new();
	let mut entry_pages = BTreeSet::new();
	let mut next = root.entry_first_page;
	let mut expected_id = 0_u32;
	for page_ordinal in 0..root.entry_page_count {
		if next == PageNo::NULL || !entry_pages.insert(next) {
			return Err(invalid("property dictionary entry chain is null or cyclic"));
		}
		let index = page_index(next, ceiling)?;
		let page = &pages[index];
		let header = page.validate(next)?;
		if header.page_type() != PageType::PropertyDictionaryEntry || !header.flags().is_empty() {
			return Err(invalid("property dictionary entry page has invalid type or flags"));
		}
		let bytes = page.as_bytes();
		let first_id = read_u32(bytes, ENTRY_FIRST_ID_OFFSET);
		let count = u32::from(read_u16(bytes, ENTRY_COUNT_OFFSET));
		let used_bytes = usize::from(read_u16(bytes, ENTRY_USED_BYTES_OFFSET));
		if first_id != expected_id || count == 0 || used_bytes > ENTRY_DATA_BYTES {
			return Err(invalid("property dictionary entry-page header is inconsistent"));
		}
		let mut offset = ENTRY_DATA_OFFSET;
		let used_end = ENTRY_DATA_OFFSET + used_bytes;
		for _ in 0..count {
			let length_end = offset
				.checked_add(2)
				.ok_or_else(|| invalid("property dictionary name length overflows usize"))?;
			if length_end > used_end {
				return Err(invalid("property dictionary name length exceeds used bytes"));
			}
			let name_len = usize::from(read_u16(bytes, offset));
			let name_end = length_end
				.checked_add(name_len)
				.ok_or_else(|| invalid("property dictionary name range overflows usize"))?;
			if name_end > used_end {
				return Err(invalid("property dictionary name exceeds used bytes"));
			}
			let name = std::str::from_utf8(&bytes[length_end..name_end])
				.map_err(|_error| invalid("property dictionary name is not UTF-8"))?;
			let id = dictionary.intern(name)?;
			if id.get() != expected_id {
				return Err(invalid("property dictionary contains a duplicate name"));
			}
			locations.push(NameLocation {
				page: next,
				offset: u32::try_from(offset)
					.map_err(|_error| invalid("property dictionary name offset exceeds u32"))?,
			});
			expected_id = expected_id
				.checked_add(1)
				.ok_or_else(|| invalid("property dictionary ID range overflows u32"))?;
			offset = name_end;
		}
		if offset != used_end || bytes[used_end..].iter().any(|byte| *byte != 0) {
			return Err(invalid("property dictionary entry used bytes or zero tail are invalid"));
		}
		let following = PageNo::new(read_u64(bytes, ENTRY_NEXT_PAGE_OFFSET));
		if page_ordinal + 1 == root.entry_page_count {
			if next != root.entry_last_page || following != PageNo::NULL {
				return Err(invalid("property dictionary entry chain has an invalid tail"));
			}
		} else if following == PageNo::NULL {
			return Err(invalid("property dictionary entry chain ended early"));
		}
		next = following;
	}
	if expected_id != root.entry_count || locations.len() != root.entry_count as usize {
		return Err(invalid("property dictionary root count disagrees with its entry chain"));
	}

	let buckets = decode_hash_range(pages, ceiling, root, &locations, &dictionary)?;
	for (id, name) in dictionary.iter() {
		if find_bucket(&buckets, property_hash(name.as_str().as_bytes()), name.as_str(), |id| {
			dictionary.resolve(id).map(Text::as_str)
		}) != Some(id)
		{
			return Err(invalid("property dictionary hash range cannot resolve an entry"));
		}
	}
	Ok(Decoded {
		dictionary,
		root,
		locations,
	})
}

/// Encodes and seals the exact dictionary-root page.
fn encode_root(root: Root, page_lsn: PageLsn) -> FerrosGrynnResult<AlignedPage> {
	validate_root_shape(root)?;
	let mut page =
		AlignedPage::new(PageType::PropertyDictionaryRoot, PageFlags::empty(), page_lsn, ROOT_PAGE);
	let bytes = page.as_bytes_mut();
	write_u32(bytes, ROOT_ENTRY_COUNT_OFFSET, root.entry_count);
	write_u32(bytes, ROOT_ENTRY_PAGE_COUNT_OFFSET, root.entry_page_count);
	write_u64(bytes, ROOT_ENTRY_FIRST_PAGE_OFFSET, root.entry_first_page.get());
	write_u64(bytes, ROOT_ENTRY_LAST_PAGE_OFFSET, root.entry_last_page.get());
	write_u64(bytes, ROOT_HASH_FIRST_PAGE_OFFSET, root.hash_first_page.get());
	write_u32(bytes, ROOT_HASH_PAGE_COUNT_OFFSET, root.hash_page_count);
	write_u32(bytes, ROOT_HASH_CAPACITY_OFFSET, root.hash_capacity);
	page.seal()?;
	Ok(page)
}

/// Decodes the exact dictionary-root page and its zero tail.
fn decode_root(page: &AlignedPage) -> FerrosGrynnResult<Root> {
	let header = page.validate(ROOT_PAGE)?;
	if header.page_type() != PageType::PropertyDictionaryRoot || !header.flags().is_empty() {
		return Err(invalid("property dictionary root has invalid type or flags"));
	}
	let bytes = page.as_bytes();
	if bytes[ROOT_RESERVED_OFFSET..].iter().any(|byte| *byte != 0) {
		return Err(invalid("property dictionary root reserved bytes are non-zero"));
	}
	Ok(Root {
		entry_count: read_u32(bytes, ROOT_ENTRY_COUNT_OFFSET),
		entry_page_count: read_u32(bytes, ROOT_ENTRY_PAGE_COUNT_OFFSET),
		entry_first_page: PageNo::new(read_u64(bytes, ROOT_ENTRY_FIRST_PAGE_OFFSET)),
		entry_last_page: PageNo::new(read_u64(bytes, ROOT_ENTRY_LAST_PAGE_OFFSET)),
		hash_first_page: PageNo::new(read_u64(bytes, ROOT_HASH_FIRST_PAGE_OFFSET)),
		hash_page_count: read_u32(bytes, ROOT_HASH_PAGE_COUNT_OFFSET),
		hash_capacity: read_u32(bytes, ROOT_HASH_CAPACITY_OFFSET),
	})
}

/// Validates empty and non-empty root pointer/count combinations.
fn validate_root_shape(root: Root) -> FerrosGrynnResult<()> {
	let entries_empty = root.entry_count == 0;
	if entries_empty
		!= (root.entry_page_count == 0
			&& root.entry_first_page == PageNo::NULL
			&& root.entry_last_page == PageNo::NULL)
	{
		return Err(invalid("property dictionary entry roots disagree with its count"));
	}
	if !entries_empty
		&& (root.entry_page_count == 0
			|| root.entry_first_page == PageNo::NULL
			|| root.entry_last_page == PageNo::NULL)
	{
		return Err(invalid("non-empty property dictionary has no entry chain"));
	}
	if entries_empty
		!= (root.hash_page_count == 0
			&& root.hash_capacity == 0
			&& root.hash_first_page == PageNo::NULL)
	{
		return Err(invalid("property dictionary hash roots disagree with its count"));
	}
	if !entries_empty
		&& (root.hash_page_count == 0
			|| root.hash_capacity == 0
			|| root.hash_first_page == PageNo::NULL)
	{
		return Err(invalid("non-empty property dictionary has no hash range"));
	}
	Ok(())
}

/// Bounds root counts and pointers by the already materialized snapshot image.
fn validate_root_ceiling(root: Root, ceiling: usize) -> FerrosGrynnResult<()> {
	let available =
		ceiling.checked_sub(1).ok_or_else(|| invalid("property dictionary has no root page"))?;
	let entry_pages = usize::try_from(root.entry_page_count)
		.map_err(|_error| invalid("property dictionary entry-page count exceeds usize"))?;
	let hash_pages = usize::try_from(root.hash_page_count)
		.map_err(|_error| invalid("property dictionary hash-page count exceeds usize"))?;
	if entry_pages > available
		|| hash_pages > available
		|| entry_pages.checked_add(hash_pages).is_none_or(|reachable| reachable > available)
	{
		return Err(invalid("property dictionary reachable pages exceed its ceiling"));
	}
	let max_entries = entry_pages
		.checked_mul(ENTRY_DATA_BYTES / size_of::<u16>())
		.ok_or_else(|| invalid("property dictionary entry capacity overflows usize"))?;
	if usize::try_from(root.entry_count).map_or(true, |count| count > max_entries) {
		return Err(invalid("property dictionary entry count exceeds its page capacity"));
	}
	for page in [root.entry_first_page, root.entry_last_page, root.hash_first_page] {
		if page != PageNo::NULL
			&& usize::try_from(page.get()).map_or(true, |index| index == 0 || index >= ceiling)
		{
			return Err(invalid("property dictionary root pointer exceeds its ceiling"));
		}
	}
	Ok(())
}

/// Appends a linked run of entry pages and returns stable name locations.
fn append_entry_pages(
	pages: &mut Vec<AlignedPage>,
	first_id: u32,
	names: &[Text],
	page_lsn: PageLsn,
) -> FerrosGrynnResult<(PageNo, PageNo, Vec<NameLocation>, u32)> {
	if names.is_empty() {
		return Ok((PageNo::NULL, PageNo::NULL, Vec::new(), 0));
	}
	let first_page = page_no(pages.len())?;
	let mut locations = Vec::with_capacity(names.len());
	let mut cursor = 0_usize;
	let mut next_id = first_id;
	let mut built = Vec::new();
	while cursor < names.len() {
		let page_number = page_no(pages.len() + built.len())?;
		let first_page_id = next_id;
		let mut page = AlignedPage::new(
			PageType::PropertyDictionaryEntry,
			PageFlags::empty(),
			page_lsn,
			page_number,
		);
		let mut offset = ENTRY_DATA_OFFSET;
		let mut count = 0_u16;
		while cursor < names.len() {
			let name = names[cursor].as_str().as_bytes();
			let length = u16::try_from(name.len())
				.map_err(|_error| invalid("property key exceeds u16 bytes"))?;
			let end = offset
				.checked_add(2 + name.len())
				.ok_or_else(|| invalid("property dictionary entry range overflows usize"))?;
			if end > PAGE_SIZE {
				if count == 0 {
					return Err(invalid(
						"property key does not fit an empty dictionary entry page",
					));
				}
				break;
			}
			write_u16(page.as_bytes_mut(), offset, length);
			page.as_bytes_mut()[offset + 2..end].copy_from_slice(name);
			locations.push(NameLocation {
				page: page_number,
				offset: u32::try_from(offset)
					.map_err(|_error| invalid("property dictionary name offset exceeds u32"))?,
			});
			offset = end;
			cursor += 1;
			count = count
				.checked_add(1)
				.ok_or_else(|| invalid("property dictionary entry count exceeds u16"))?;
			next_id = next_id
				.checked_add(1)
				.ok_or_else(|| invalid("property dictionary ID clock is exhausted"))?;
		}
		write_u32(page.as_bytes_mut(), ENTRY_FIRST_ID_OFFSET, first_page_id);
		write_u16(page.as_bytes_mut(), ENTRY_COUNT_OFFSET, count);
		write_u16(
			page.as_bytes_mut(),
			ENTRY_USED_BYTES_OFFSET,
			u16::try_from(offset - ENTRY_DATA_OFFSET)
				.map_err(|_error| invalid("property dictionary used bytes exceed u16"))?,
		);
		built.push(page);
	}
	for index in 0..built.len() {
		let next = if index + 1 == built.len() {
			PageNo::NULL
		} else {
			page_no(pages.len() + index + 1)?
		};
		write_u64(built[index].as_bytes_mut(), ENTRY_NEXT_PAGE_OFFSET, next.get());
		built[index].seal()?;
	}
	let last_page = page_no(pages.len() + built.len() - 1)?;
	let page_count = u32_len(built.len(), "property dictionary entry page count")?;
	pages.extend(built);
	Ok((first_page, last_page, locations, page_count))
}

/// Appends a complete replacement hash range for all current names.
fn append_hash_range(
	pages: &mut Vec<AlignedPage>,
	names: &[Text],
	locations: &[NameLocation],
	page_lsn: PageLsn,
) -> FerrosGrynnResult<(PageNo, u32, u32)> {
	if names.is_empty() {
		return Ok((PageNo::NULL, 0, 0));
	}
	if names.len() != locations.len() {
		return Err(invalid("property dictionary names and locations disagree"));
	}
	let minimum_capacity = names
		.len()
		.checked_mul(4)
		.and_then(|value| value.checked_add(2))
		.map(|value| value / 3)
		.ok_or_else(|| invalid("property dictionary hash capacity overflows usize"))?;
	let page_count = minimum_capacity.div_ceil(HASH_BUCKETS_PER_PAGE).max(1);
	let capacity = page_count
		.checked_mul(HASH_BUCKETS_PER_PAGE)
		.ok_or_else(|| invalid("property dictionary hash range overflows usize"))?;
	let mut buckets = vec![None; capacity];
	for (raw_id, (name, location)) in names.iter().zip(locations).enumerate() {
		let id = PropertyKeyId::new(
			u32::try_from(raw_id).map_err(|_error| invalid("property key ID exceeds u32"))?,
		);
		place_bucket(&mut buckets, HashBucket {
			hash: property_hash(name.as_str().as_bytes()),
			id,
			location: *location,
		})?;
	}

	let first_page = page_no(pages.len())?;
	for page_offset in 0..page_count {
		let page_number = page_no(pages.len())?;
		let next = if page_offset + 1 == page_count {
			PageNo::NULL
		} else {
			page_no(pages.len() + 1)?
		};
		let range_start = page_offset * HASH_BUCKETS_PER_PAGE;
		let range_end = range_start + HASH_BUCKETS_PER_PAGE;
		let page_buckets = &buckets[range_start..range_end];
		let used = page_buckets.iter().filter(|bucket| bucket.is_some()).count();
		let mut page =
			AlignedPage::new(PageType::HashIndex, PageFlags::empty(), page_lsn, page_number);
		write_u64(page.as_bytes_mut(), HASH_NEXT_PAGE_OFFSET, next.get());
		write_u32(
			page.as_bytes_mut(),
			HASH_USED_OFFSET,
			u32_len(used, "property dictionary hash used count")?,
		);
		write_u32(page.as_bytes_mut(), HASH_CAPACITY_OFFSET, HASH_BUCKETS_PER_PAGE as u32);
		for (slot, bucket) in page_buckets.iter().enumerate() {
			if let Some(bucket) = bucket {
				write_bucket(page.as_bytes_mut(), slot, *bucket)?;
			}
		}
		page.seal()?;
		pages.push(page);
	}
	Ok((
		first_page,
		u32_len(page_count, "property dictionary hash page count")?,
		u32_len(capacity, "property dictionary hash capacity")?,
	))
}

/// Decodes the current hash range and validates every ID/name binding.
fn decode_hash_range(
	pages: &[AlignedPage],
	ceiling: usize,
	root: Root,
	locations: &[NameLocation],
	dictionary: &PropertyDictionary,
) -> FerrosGrynnResult<Vec<Option<HashBucket>>> {
	if root.entry_count == 0 {
		return Ok(Vec::new());
	}
	let expected_capacity = usize::try_from(root.hash_page_count)
		.ok()
		.and_then(|count| count.checked_mul(HASH_BUCKETS_PER_PAGE))
		.ok_or_else(|| invalid("property dictionary hash capacity overflows usize"))?;
	if usize::try_from(root.hash_capacity).ok() != Some(expected_capacity) {
		return Err(invalid("property dictionary root hash capacity is not canonical"));
	}
	let mut pages_seen = BTreeSet::new();
	let mut buckets = Vec::with_capacity(expected_capacity);
	let mut ids = BTreeSet::new();
	let mut next = root.hash_first_page;
	for page_ordinal in 0..root.hash_page_count {
		if next == PageNo::NULL || !pages_seen.insert(next) {
			return Err(invalid("property dictionary hash range is null or cyclic"));
		}
		let index = page_index(next, ceiling)?;
		let page = &pages[index];
		let header = page.validate(next)?;
		if header.page_type() != PageType::HashIndex || !header.flags().is_empty() {
			return Err(invalid("property dictionary hash page has invalid type or flags"));
		}
		let bytes = page.as_bytes();
		let used = read_u32(bytes, HASH_USED_OFFSET);
		if read_u32(bytes, HASH_CAPACITY_OFFSET) != HASH_BUCKETS_PER_PAGE as u32
			|| bytes[HASH_TAIL_OFFSET..].iter().any(|byte| *byte != 0)
		{
			return Err(invalid("property dictionary hash page capacity or tail is invalid"));
		}
		let mut actual_used = 0_u32;
		for slot in 0..HASH_BUCKETS_PER_PAGE {
			let bucket = read_bucket(bytes, slot)?;
			if let Some(bucket) = bucket {
				actual_used = actual_used
					.checked_add(1)
					.ok_or_else(|| invalid("property dictionary hash used count overflows"))?;
				let id_index = usize::try_from(bucket.id.get())
					.map_err(|_error| invalid("property dictionary hash ID exceeds usize"))?;
				let expected_location = locations
					.get(id_index)
					.ok_or_else(|| invalid("property dictionary hash ID exceeds entry count"))?;
				let name = dictionary
					.resolve(bucket.id)
					.ok_or_else(|| invalid("property dictionary hash name is absent"))?;
				if bucket.location != *expected_location
					|| bucket.hash != property_hash(name.as_str().as_bytes())
					|| !ids.insert(bucket.id)
				{
					return Err(invalid("property dictionary hash bucket binding is invalid"));
				}
			}
			buckets.push(bucket);
		}
		if actual_used != used {
			return Err(invalid("property dictionary hash used count is invalid"));
		}
		let following = PageNo::new(read_u64(bytes, HASH_NEXT_PAGE_OFFSET));
		if page_ordinal + 1 == root.hash_page_count {
			if following != PageNo::NULL {
				return Err(invalid("property dictionary hash range continues past its count"));
			}
		} else if following == PageNo::NULL {
			return Err(invalid("property dictionary hash range ended early"));
		}
		next = following;
	}
	if ids.len() != root.entry_count as usize {
		return Err(invalid("property dictionary hash range does not bind every entry"));
	}
	Ok(buckets)
}

/// Inserts one bucket using linear probing over the logical multi-page range.
fn place_bucket(buckets: &mut [Option<HashBucket>], bucket: HashBucket) -> FerrosGrynnResult<()> {
	let start = bucket.hash as usize % buckets.len();
	for distance in 0..buckets.len() {
		let index = (start + distance) % buckets.len();
		if buckets[index].is_none() {
			buckets[index] = Some(bucket);
			return Ok(());
		}
	}
	Err(invalid("property dictionary hash range has no empty bucket"))
}

/// Resolves one name by hash and mandatory full UTF-8 byte comparison.
fn find_bucket<'a>(
	buckets: &[Option<HashBucket>],
	hash: u32,
	name: &str,
	mut name_by_id: impl FnMut(PropertyKeyId) -> Option<&'a str>,
) -> Option<PropertyKeyId> {
	if buckets.is_empty() {
		return None;
	}
	let start = hash as usize % buckets.len();
	for distance in 0..buckets.len() {
		let bucket = buckets[(start + distance) % buckets.len()]?;
		if bucket.hash == hash && name_by_id(bucket.id) == Some(name) {
			return Some(bucket.id);
		}
	}
	None
}

/// Writes one occupied bucket at a checked page-local slot.
fn write_bucket(bytes: &mut [u8], slot: usize, bucket: HashBucket) -> FerrosGrynnResult<()> {
	let offset = hash_bucket_offset(slot)?;
	write_u32(bytes, offset, bucket.hash);
	write_u32(bytes, offset + 4, bucket.id.get());
	write_u64(bytes, offset + 8, bucket.location.page.get());
	write_u32(bytes, offset + 16, bucket.location.offset);
	Ok(())
}

/// Reads one bucket and enforces the all-zero empty encoding.
fn read_bucket(bytes: &[u8], slot: usize) -> FerrosGrynnResult<Option<HashBucket>> {
	let offset = hash_bucket_offset(slot)?;
	let hash = read_u32(bytes, offset);
	let id = read_u32(bytes, offset + 4);
	let page = read_u64(bytes, offset + 8);
	let name_offset = read_u32(bytes, offset + 16);
	if page == 0 {
		if hash != 0 || id != 0 || name_offset != 0 {
			return Err(invalid("empty property dictionary hash bucket is not zero"));
		}
		return Ok(None);
	}
	Ok(Some(HashBucket {
		hash,
		id: PropertyKeyId::new(id),
		location: NameLocation {
			page: PageNo::new(page),
			offset: name_offset,
		},
	}))
}

/// Converts a checked page-local bucket slot to its byte offset.
fn hash_bucket_offset(slot: usize) -> FerrosGrynnResult<usize> {
	if slot >= HASH_BUCKETS_PER_PAGE {
		return Err(invalid("property dictionary hash slot exceeds its page"));
	}
	HASH_DATA_OFFSET
		.checked_add(slot * HASH_BUCKET_BYTES)
		.ok_or_else(|| invalid("property dictionary hash offset overflows usize"))
}

/// Collects names in dense stable-ID order and rejects holes.
fn dictionary_names(dictionary: &PropertyDictionary) -> FerrosGrynnResult<Vec<Text>> {
	dictionary
		.iter()
		.enumerate()
		.map(|(expected, (id, name))| {
			if usize::try_from(id.get()).ok() != Some(expected) {
				return Err(invalid("runtime property dictionary IDs are not dense"));
			}
			Ok(name.clone())
		})
		.collect()
}

/// Computes the normative portable low-32 RapidHash v3 property hash.
fn property_hash(name: &[u8]) -> u32 {
	rapidhash_v3_seeded(name, &DEFAULT_RAPID_SECRETS) as u32
}

/// Converts an in-memory page index to the durable page-number domain.
fn page_no(index: usize) -> FerrosGrynnResult<PageNo> {
	u64::try_from(index)
		.map(PageNo::new)
		.map_err(|_error| invalid("property dictionary page number exceeds u64"))
}

/// Resolves a non-root page number below the snapshot-visible ceiling.
fn page_index(page: PageNo, ceiling: usize) -> FerrosGrynnResult<usize> {
	let index = usize::try_from(page.get())
		.map_err(|_error| invalid("property dictionary page number exceeds usize"))?;
	if index == 0 || index >= ceiling {
		return Err(invalid("property dictionary page reference exceeds its ceiling"));
	}
	Ok(index)
}

/// Converts a collection length to its durable `u32` field.
fn u32_len(value: usize, field: &str) -> FerrosGrynnResult<u32> {
	u32::try_from(value).map_err(|_error| invalid(format!("{field} exceeds u32")))
}

/// Writes one little-endian `u16` at an exact format offset.
fn write_u16(bytes: &mut [u8], offset: usize, value: u16) {
	bytes[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
}

/// Writes one little-endian `u32` at an exact format offset.
fn write_u32(bytes: &mut [u8], offset: usize, value: u32) {
	bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

/// Writes one little-endian `u64` at an exact format offset.
fn write_u64(bytes: &mut [u8], offset: usize, value: u64) {
	bytes[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
}

/// Reads one little-endian `u16` from an exact format field.
fn read_u16(bytes: &[u8], offset: usize) -> u16 {
	let mut value = [0; 2];
	value.copy_from_slice(&bytes[offset..offset + 2]);
	u16::from_le_bytes(value)
}

/// Reads one little-endian `u32` from an exact format field.
fn read_u32(bytes: &[u8], offset: usize) -> u32 {
	let mut value = [0; 4];
	value.copy_from_slice(&bytes[offset..offset + 4]);
	u32::from_le_bytes(value)
}

/// Reads one little-endian `u64` from an exact format field.
fn read_u64(bytes: &[u8], offset: usize) -> u64 {
	let mut value = [0; 8];
	value.copy_from_slice(&bytes[offset..offset + 8]);
	u64::from_le_bytes(value)
}

/// Creates the typed corruption error used by this codec.
fn invalid(reason: impl Into<String>) -> FerrosGrynnError {
	FerrosGrynnError::invalid_storage_file(reason)
}

#[cfg(test)]
mod tests {
	use super::*;

	fn dictionary(
		names: impl IntoIterator<Item = String>,
	) -> FerrosGrynnResult<PropertyDictionary> {
		let mut dictionary = PropertyDictionary::new();
		for name in names {
			dictionary.intern(name)?;
		}
		Ok(dictionary)
	}

	fn copied_pages(pages: &[AlignedPage]) -> Vec<AlignedPage> {
		pages
			.iter()
			.map(|source| {
				let mut page = AlignedPage::zeroed();
				page.as_bytes_mut().copy_from_slice(source.as_bytes());
				page
			})
			.collect()
	}

	#[test]
	fn root_entry_and_hash_pages_have_exact_offsets() -> FerrosGrynnResult<()> {
		let dictionary = dictionary(["name".to_owned(), "weight".to_owned()])?;
		let image = build_property_dictionary(&dictionary, PageLsn::new(7))?;
		let root = image.pages[0].as_bytes();
		assert_eq!(read_u32(root, ROOT_ENTRY_COUNT_OFFSET), 2);
		assert_eq!(read_u32(root, ROOT_ENTRY_PAGE_COUNT_OFFSET), 1);
		assert_eq!(read_u64(root, ROOT_ENTRY_FIRST_PAGE_OFFSET), 1);
		assert_eq!(read_u64(root, ROOT_ENTRY_LAST_PAGE_OFFSET), 1);
		assert_eq!(read_u64(root, ROOT_HASH_FIRST_PAGE_OFFSET), 2);
		assert_eq!(read_u32(root, ROOT_HASH_PAGE_COUNT_OFFSET), 1);
		assert_eq!(read_u32(root, ROOT_HASH_CAPACITY_OFFSET), HASH_BUCKETS_PER_PAGE as u32);

		let entry = image.pages[1].as_bytes();
		assert_eq!(read_u64(entry, ENTRY_NEXT_PAGE_OFFSET), PageNo::NULL.get());
		assert_eq!(read_u32(entry, ENTRY_FIRST_ID_OFFSET), 0);
		assert_eq!(read_u16(entry, ENTRY_COUNT_OFFSET), 2);
		assert_eq!(read_u16(entry, ENTRY_USED_BYTES_OFFSET), 14);
		assert_eq!(&entry[ENTRY_DATA_OFFSET..ENTRY_DATA_OFFSET + 6], b"\x04\0name");

		let reopened = open_property_dictionary(&image.pages, image.pages.len() as u64)?;
		assert_eq!(reopened.get("name").map(PropertyKeyId::get), Some(0));
		assert_eq!(reopened.get("weight").map(PropertyKeyId::get), Some(1));
		Ok(())
	}

	#[test]
	fn resize_appends_entries_and_replacement_hash_range() -> FerrosGrynnResult<()> {
		let original = dictionary((0..8).map(|id| format!("key-{id}")))?;
		let image = build_property_dictionary(&original, PageLsn::new(1))?;
		let old_len = image.pages.len();
		let mut expanded = original.clone();
		for id in 8..900 {
			expanded.intern(format!("key-{id}"))?;
		}
		let image = extend_property_dictionary(image, &expanded, PageLsn::new(2))?;
		assert!(image.pages.len() > old_len);
		let root = decode_root(&image.pages[0])?;
		assert!(root.hash_first_page.get() >= old_len as u64);
		assert!(root.hash_page_count >= 2);
		let reopened = open_property_dictionary(&image.pages, image.pages.len() as u64)?;
		assert_eq!(reopened.len(), 900);
		assert_eq!(reopened.get("key-0").map(PropertyKeyId::get), Some(0));
		assert_eq!(reopened.get("key-899").map(PropertyKeyId::get), Some(899));
		Ok(())
	}

	#[test]
	fn collision_resolution_compares_complete_name_bytes() -> FerrosGrynnResult<()> {
		let locations = [
			NameLocation {
				page: PageNo::new(1),
				offset: 48,
			},
			NameLocation {
				page: PageNo::new(1),
				offset: 55,
			},
		];
		let names = ["first", "second"];
		let mut buckets = vec![None; 8];
		for (id, location) in locations.into_iter().enumerate() {
			place_bucket(&mut buckets, HashBucket {
				hash: 7,
				id: PropertyKeyId::new(
					u32::try_from(id)
						.map_err(|_error| invalid("test property key ID exceeds u32"))?,
				),
				location,
			})?;
		}
		assert_eq!(
			find_bucket(&buckets, 7, "second", |id| names.get(id.get() as usize).copied()),
			Some(PropertyKeyId::new(1))
		);
		assert_eq!(
			find_bucket(&buckets, 7, "missing", |id| { names.get(id.get() as usize).copied() }),
			None
		);
		Ok(())
	}

	#[test]
	fn rejects_duplicate_utf8_invalid_tail_and_truncated_ceiling() -> FerrosGrynnResult<()> {
		let duplicate_source = dictionary(["alpha".to_owned(), "bravo".to_owned()])?;
		let mut image = build_property_dictionary(&duplicate_source, PageLsn::ZERO)?;
		let second_offset = ENTRY_DATA_OFFSET + 2 + "alpha".len();
		write_u16(image.pages[1].as_bytes_mut(), second_offset, 5);
		image.pages[1].as_bytes_mut()[second_offset + 2..second_offset + 7]
			.copy_from_slice(b"alpha");
		image.pages[1].seal()?;
		assert!(open_property_dictionary(&image.pages, image.pages.len() as u64).is_err());

		let valid = dictionary(["valid".to_owned()])?;
		let mut invalid_utf8 = build_property_dictionary(&valid, PageLsn::ZERO)?;
		invalid_utf8.pages[1].as_bytes_mut()[ENTRY_DATA_OFFSET + 2] = 0xff;
		invalid_utf8.pages[1].seal()?;
		assert!(
			open_property_dictionary(
				&invalid_utf8.pages,
				u64::try_from(invalid_utf8.pages.len())
					.map_err(|_error| invalid("test page count exceeds u64"))?,
			)
			.is_err()
		);

		let mut image = build_property_dictionary(&valid, PageLsn::ZERO)?;
		image.pages[1].as_bytes_mut()[PAGE_SIZE - 1] = 1;
		image.pages[1].seal()?;
		assert!(
			open_property_dictionary(
				&image.pages,
				u64::try_from(image.pages.len())
					.map_err(|_error| invalid("test page count exceeds u64"))?,
			)
			.is_err()
		);
		assert!(open_property_dictionary(&image.pages, 1).is_err());
		Ok(())
	}

	#[test]
	fn rejects_root_counts_that_exceed_the_materialized_ceiling() -> FerrosGrynnResult<()> {
		let dictionary = dictionary(["name".to_owned()])?;
		let image = build_property_dictionary(&dictionary, PageLsn::ZERO)?;
		let mut root = decode_root(&image.pages[0])?;
		root.hash_page_count = u32::MAX;
		root.hash_capacity = u32::MAX;
		assert!(validate_root_ceiling(root, image.pages.len()).is_err());

		let mut root = decode_root(&image.pages[0])?;
		root.entry_count = u32::MAX;
		assert!(validate_root_ceiling(root, image.pages.len()).is_err());
		Ok(())
	}

	#[test]
	fn rejects_wrong_types_flags_reserved_bytes_and_checksum() -> FerrosGrynnResult<()> {
		let dictionary = dictionary(["name".to_owned()])?;
		let image = build_property_dictionary(&dictionary, PageLsn::ZERO)?;

		let mut wrong_root_type = copied_pages(&image.pages);
		wrong_root_type[0].reset(
			PageType::PropertyDictionaryEntry,
			PageFlags::empty(),
			PageLsn::ZERO,
			ROOT_PAGE,
		);
		assert!(open_property_dictionary(&wrong_root_type, wrong_root_type.len() as u64).is_err());

		let mut wrong_entry_flags = copied_pages(&image.pages);
		wrong_entry_flags[1].reset(
			PageType::PropertyDictionaryEntry,
			PageFlags::DIRTY,
			PageLsn::ZERO,
			PageNo::new(1),
		);
		assert!(
			open_property_dictionary(&wrong_entry_flags, wrong_entry_flags.len() as u64).is_err()
		);

		let mut reserved = copied_pages(&image.pages);
		reserved[0].as_bytes_mut()[ROOT_RESERVED_OFFSET] = 1;
		reserved[0].seal()?;
		assert!(open_property_dictionary(&reserved, reserved.len() as u64).is_err());

		let mut checksum = copied_pages(&image.pages);
		checksum[1].as_bytes_mut()[ENTRY_DATA_OFFSET + 2] ^= 1;
		assert!(open_property_dictionary(&checksum, checksum.len() as u64).is_err());
		Ok(())
	}
}
