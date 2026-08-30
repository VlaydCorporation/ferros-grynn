#[allow(clippy::wildcard_imports, reason = "index layers use the graph-index facade contract")]
use super::*;

/// Four dictionary sections persisted by the first local slice.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(in crate::storage) struct LabelDictionary {
	pub(in crate::storage) node_labels: Vec<String>,
	pub(in crate::storage) edge_labels: Vec<String>,
	pub(in crate::storage) property_keys: Vec<String>,
	pub(in crate::storage) table_names: Vec<String>,
	pub(in crate::storage) time_zones: Vec<String>,
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
pub(in crate::storage) fn encode_label_dictionary(
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
pub(in crate::storage) fn decode_label_dictionary(
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
