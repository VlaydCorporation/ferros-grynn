//! Reachable-page validation and sparse append plans for existing dictionaries.

use std::collections::BTreeSet;

use super::{
	ENTRY_NEXT_PAGE_OFFSET,
	HASH_BUCKETS_PER_PAGE,
	HashBucket,
	ROOT_PAGE,
	append_entry_pages,
	append_hash_range,
	decode_property_dictionary_from,
	dictionary_names,
	encode_hash_page,
	encode_root,
	invalid,
	page_number_at,
	place_bucket,
	property_hash,
	u32_len,
	write_u64,
};
use crate::{
	datastore::DataSpaceId,
	gql_status::FerrosGrynnResult,
	storage::{
		page::{AlignedPage, PageLsn, PageNo},
		types::{DataFileId, PageKey},
		wal::TxHandle,
	},
	values::{PropertyDictionary, PropertyKeyId},
};

#[cfg(test)]
/// Sparse plans are checked against the independent full-image control.
mod tests;

/// Owned replacements prepared before the common data/catalog WAL barrier.
pub(in crate::storage) struct PropertyDictionaryUpdate {
	/// Existing root/tail/hash patches and newly appended pages.
	pages: Vec<(PageNo, Box<AlignedPage>)>,
	/// Published ceiling including every new entry or replacement hash page.
	pub(in crate::storage) page_ceiling: u64,
}

impl PropertyDictionaryUpdate {
	/// Stages the already prepared sparse page set in the caller's WAL
	/// transaction.
	///
	/// # Errors
	/// Propagates page staging failures before the caller's publication
	/// barrier.
	pub(in crate::storage) fn stage(
		&self,
		tx: &mut TxHandle,
		file: DataFileId,
	) -> FerrosGrynnResult<()> {
		for (number, page) in &self.pages {
			tx.stage_page(PageKey::new(DataSpaceId::Database, file, *number), page)?;
		}
		Ok(())
	}
}

/// Validates the stable prefix and prepares only pages changed by an append.
///
/// # Errors
/// Propagates source I/O, malformed codecs, changed IDs and address overflow.
pub(in crate::storage) fn plan_property_dictionary_update<ReadPage>(
	read_page: &mut ReadPage,
	page_ceiling: u64,
	dictionary: &PropertyDictionary,
	page_lsn: PageLsn,
) -> FerrosGrynnResult<PropertyDictionaryUpdate>
where
	ReadPage: FnMut(PageNo, &mut AlignedPage) -> FerrosGrynnResult<()>,
{
	// shortcut: validation walks all reachable keys; add snapshot-bound
	// validation metadata only if profiling identifies this cost.
	let decoded = decode_property_dictionary_from(read_page, page_ceiling)?;
	let names = dictionary_names(dictionary)?;
	let old_len = decoded.dictionary.len();
	if names.len() < old_len
		|| decoded.dictionary.iter().any(|(id, name)| dictionary.resolve(id) != Some(name))
	{
		return Err(invalid("property dictionary update does not preserve its stable ID prefix"));
	}
	let mut result = PropertyDictionaryUpdate {
		pages: Vec::new(),
		page_ceiling,
	};
	if names.len() == old_len {
		return Ok(result);
	}
	let start = usize::try_from(page_ceiling)
		.map_err(|_error| invalid("property dictionary page ceiling exceeds usize"))?;
	let mut appended = Vec::new();
	let (new_first, new_last, new_locations, new_page_count) = append_entry_pages(
		&mut appended,
		start,
		u32_len(old_len, "property dictionary first appended ID")?,
		&names[old_len..],
		page_lsn,
	)?;
	let mut root = decoded.root;
	if root.entry_count == 0 {
		root.entry_first_page = new_first;
	} else {
		let mut tail = Box::new(AlignedPage::zeroed());
		read_page(root.entry_last_page, &mut tail)?;
		// Source bytes were validated under the caller's publication lock.
		write_u64(tail.as_bytes_mut(), ENTRY_NEXT_PAGE_OFFSET, new_first.get());
		tail.seal()?;
		result.pages.push((root.entry_last_page, tail));
	}
	root.entry_count = u32_len(names.len(), "property dictionary entry count")?;
	root.entry_page_count = root
		.entry_page_count
		.checked_add(new_page_count)
		.ok_or_else(|| invalid("property dictionary entry page count overflows u32"))?;
	root.entry_last_page = new_last;
	let required = names
		.len()
		.checked_mul(4)
		.ok_or_else(|| invalid("property dictionary hash load overflows usize"))?;
	let available = decoded
		.hash_buckets
		.len()
		.checked_mul(3)
		.ok_or_else(|| invalid("property dictionary hash capacity overflows usize"))?;
	if required <= available {
		let mut buckets = decoded.hash_buckets;
		let mut changed = BTreeSet::new();
		for (offset, location) in new_locations.iter().enumerate() {
			let id = old_len
				.checked_add(offset)
				.ok_or_else(|| invalid("property dictionary appended ID overflows usize"))?;
			let slot = place_bucket(&mut buckets, HashBucket {
				hash: property_hash(names[id].as_str().as_bytes()),
				id: PropertyKeyId::new(u32_len(id, "property dictionary appended ID")?),
				location: *location,
			})?;
			changed.insert(slot / HASH_BUCKETS_PER_PAGE);
		}
		for ordinal in changed {
			let number = decoded.hash_pages[ordinal];
			let next = decoded.hash_pages.get(ordinal + 1).copied().unwrap_or(PageNo::NULL);
			let first = ordinal * HASH_BUCKETS_PER_PAGE;
			let mut page = Box::new(AlignedPage::zeroed());
			encode_hash_page(
				&mut page,
				number,
				next,
				&buckets[first..first + HASH_BUCKETS_PER_PAGE],
				page_lsn,
			)?;
			result.pages.push((number, page));
		}
	} else {
		let mut locations = decoded.locations;
		locations.extend(new_locations);
		let hash_start = start
			.checked_add(appended.len())
			.ok_or_else(|| invalid("property dictionary hash address overflows usize"))?;
		let (first, count, capacity) =
			append_hash_range(&mut appended, hash_start, &names, &locations, page_lsn)?;
		root.hash_first_page = first;
		root.hash_page_count = count;
		root.hash_capacity = capacity;
	}
	result.page_ceiling = page_number_at(start, appended.len())?.get();
	for (ordinal, page) in appended.into_iter().enumerate() {
		result.pages.push((page_number_at(start, ordinal)?, Box::new(page)));
	}
	result.pages.push((ROOT_PAGE, Box::new(encode_root(root, page_lsn)?)));
	Ok(result)
}
