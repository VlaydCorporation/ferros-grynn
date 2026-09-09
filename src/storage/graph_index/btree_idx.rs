#[allow(clippy::wildcard_imports, reason = "index layers use the graph-index facade contract")]
use super::*;

#[derive(Clone)]
pub(super) struct ChildPage {
	pub(super) page_no: PageNo,
	pub(super) min_key: Vec<u8>,
}

pub(super) fn validate_primary_entries(entries: &[PrimaryIndexEntry]) -> FerrosGrynnResult<()> {
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

pub(super) fn validate_scalar_entries(
	entries: &[ScalarIndexEntry],
	mode: ScalarIndexMode,
) -> FerrosGrynnResult<()> {
	for (index, entry) in entries.iter().enumerate() {
		if entry.key.is_empty() || entry.atoms.is_empty() {
			return Err(invalid_index("scalar index contains an empty key or atom set"));
		}
		if index > 0 && entries[index - 1].key >= entry.key {
			return Err(invalid_index("scalar index keys are not strictly ordered"));
		}
		if mode == ScalarIndexMode::Unique && entry.atoms.len() != 1 {
			return Err(FerrosGrynnError::graph_constraint_violation(
				"scalar index",
				"unique scalar index contains a duplicate key",
			));
		}
		if entry.atoms.windows(2).any(|pair| pair[0] >= pair[1]) {
			return Err(invalid_index("scalar index atoms are not strictly ordered"));
		}
	}
	Ok(())
}

pub(super) fn partition_standard_leaf_entries(
	entries: &[ScalarIndexEntry],
) -> FerrosGrynnResult<Vec<&[ScalarIndexEntry]>> {
	partition_standard_leaf_entries_with_limit(entries, PAGE_SIZE)
}

pub(super) fn partition_standard_leaf_entries_with_limit(
	entries: &[ScalarIndexEntry],
	maximum_bytes: usize,
) -> FerrosGrynnResult<Vec<&[ScalarIndexEntry]>> {
	if maximum_bytes > PAGE_SIZE || maximum_bytes < LEAF_OFFSETS_OFFSET {
		return Err(invalid_index("scalar leaf fill limit is outside page bounds"));
	}
	if entries.is_empty() {
		return Ok(vec![entries]);
	}
	let mut groups = Vec::new();
	let mut start = 0;
	while start < entries.len() {
		let mut end = start;
		let mut data_bytes = 0_usize;
		while end < entries.len() {
			let inline = entries[end].atoms.len().min(STANDARD_INLINE_ATOMS);
			let entry_bytes = entries[end]
				.key
				.len()
				.checked_add(inline * DISK_ATOM_REF_BYTES)
				.and_then(|bytes| bytes.checked_add(STANDARD_ENTRY_TRAILER_BYTES))
				.ok_or_else(|| invalid_index("scalar leaf entry length overflows usize"))?;
			let count = end - start + 1;
			let total = LEAF_OFFSETS_OFFSET
				.checked_add(count * size_of::<u16>())
				.and_then(|fixed| fixed.checked_add(data_bytes))
				.and_then(|used| used.checked_add(entry_bytes))
				.ok_or_else(|| invalid_index("scalar leaf size overflows usize"))?;
			if total > PAGE_SIZE {
				if end == start {
					return Err(invalid_index("one scalar key exceeds leaf-page capacity"));
				}
				break;
			}
			if total > maximum_bytes && end > start {
				break;
			}
			data_bytes += entry_bytes;
			end += 1;
			if total > maximum_bytes {
				break;
			}
		}
		groups.push(&entries[start..end]);
		start = end;
	}
	Ok(groups)
}

pub(super) fn encode_standard_leaf(
	page: &mut AlignedPage,
	page_no: PageNo,
	page_lsn: PageLsn,
	next: Option<PageNo>,
	entries: &[ScalarIndexEntry],
	posting_roots: &BTreeMap<&[u8], PageNo>,
) -> FerrosGrynnResult<()> {
	let count = u16::try_from(entries.len())
		.map_err(|_error| invalid_index("scalar leaf entry count exceeds u16"))?;
	let area_start = LEAF_OFFSETS_OFFSET + entries.len() * size_of::<u16>();
	let data_bytes = entries.iter().try_fold(0_usize, |total, entry| {
		let inline = entry.atoms.len().min(STANDARD_INLINE_ATOMS);
		total
			.checked_add(entry.key.len())
			.and_then(|total| total.checked_add(inline * DISK_ATOM_REF_BYTES))
			.and_then(|total| total.checked_add(STANDARD_ENTRY_TRAILER_BYTES))
			.ok_or_else(|| invalid_index("scalar leaf data length overflows usize"))
	})?;
	let data_start = PAGE_SIZE
		.checked_sub(data_bytes)
		.filter(|start| *start >= area_start)
		.ok_or_else(|| invalid_index("scalar leaf data overlaps its directory"))?;
	page.reset(PageType::IndexNonUniqueBtreeLeaf, PageFlags::LEAF, page_lsn, page_no);
	write_u64(page.as_bytes_mut(), LEAF_NEXT_OFFSET, next.unwrap_or(PageNo::NULL).get());
	write_u16(page.as_bytes_mut(), LEAF_COUNT_OFFSET, count);
	let mut area_offset = 0_usize;
	for (index, entry) in entries.iter().enumerate() {
		let persisted_offset = data_start + area_offset - area_start;
		write_u16(
			page.as_bytes_mut(),
			LEAF_OFFSETS_OFFSET + index * size_of::<u16>(),
			u16::try_from(persisted_offset)
				.map_err(|_error| invalid_index("scalar leaf offset exceeds u16"))?,
		);
		let start = data_start + area_offset;
		page.as_bytes_mut()[start..start + entry.key.len()].copy_from_slice(&entry.key);
		let mut offset = start + entry.key.len();
		for atom in entry.atoms.iter().take(STANDARD_INLINE_ATOMS) {
			write_u64(page.as_bytes_mut(), offset, atom.raw());
			offset += DISK_ATOM_REF_BYTES;
		}
		write_u16(
			page.as_bytes_mut(),
			offset,
			u16::try_from(entry.key.len())
				.map_err(|_error| invalid_index("scalar key length exceeds u16"))?,
		);
		write_u16(
			page.as_bytes_mut(),
			offset + size_of::<u16>(),
			u16::try_from(entry.atoms.len().min(STANDARD_INLINE_ATOMS))
				.map_err(|_error| invalid_index("scalar inline count exceeds u16"))?,
		);
		write_u32(
			page.as_bytes_mut(),
			offset + 2 * size_of::<u16>(),
			u32::try_from(entry.atoms.len())
				.map_err(|_error| invalid_index("scalar atom count exceeds u32"))?,
		);
		write_u64(
			page.as_bytes_mut(),
			offset + 2 * size_of::<u16>() + size_of::<u32>(),
			posting_roots.get(entry.key.as_slice()).copied().unwrap_or(PageNo::NULL).get(),
		);
		area_offset = offset + STANDARD_ENTRY_TRAILER_BYTES - data_start;
	}
	page.seal()
}

pub(super) fn encode_posting_page(
	page: &mut AlignedPage,
	page_no: PageNo,
	page_lsn: PageLsn,
	next: Option<PageNo>,
	atoms: &[DiskAtomRef],
) -> FerrosGrynnResult<()> {
	if atoms.is_empty() || atoms.len() > POSTING_ATOMS_PER_PAGE {
		return Err(invalid_index("scalar posting page has an invalid atom count"));
	}
	page.reset(PageType::IndexPostingBtreeLeaf, PageFlags::LEAF, page_lsn, page_no);
	write_u64(page.as_bytes_mut(), POSTING_NEXT_OFFSET, next.unwrap_or(PageNo::NULL).get());
	write_u32(
		page.as_bytes_mut(),
		POSTING_COUNT_OFFSET,
		u32::try_from(atoms.len()).map_err(|_error| invalid_index("posting count exceeds u32"))?,
	);
	for (index, atom) in atoms.iter().enumerate() {
		write_u64(
			page.as_bytes_mut(),
			POSTING_ATOMS_OFFSET + index * DISK_ATOM_REF_BYTES,
			atom.raw(),
		);
	}
	page.seal()
}

pub(super) fn descend_to_leaf(
	read_page: &mut impl FnMut(PageNo, &mut AlignedPage) -> FerrosGrynnResult<()>,
	root: PageNo,
	target: &[u8],
	leaf_type: PageType,
) -> FerrosGrynnResult<PageNo> {
	let mut page_no = root;
	let mut visited = BTreeSet::new();
	for _depth in 0..MAX_BTREE_DEPTH {
		if page_no == PageNo::NULL || !visited.insert(page_no) {
			return Err(invalid_index("scalar B+tree contains a null link or cycle"));
		}
		let mut page = AlignedPage::zeroed();
		read_page(page_no, &mut page)?;
		match page.validate(page_no)?.page_type() {
			page_type if page_type == leaf_type => return Ok(page_no),
			PageType::IndexBtreeInternal => page_no = select_internal_child(&page, target)?,
			_ => return Err(invalid_index("scalar B+tree contains an invalid page type")),
		}
	}
	Err(invalid_index("scalar B+tree exceeds its maximum depth"))
}

pub(super) fn lookup_standard_leaf(
	read_page: &mut impl FnMut(PageNo, &mut AlignedPage) -> FerrosGrynnResult<()>,
	page: &AlignedPage,
	target: &[u8],
) -> FerrosGrynnResult<Vec<DiskAtomRef>> {
	let (count, area_start) = validate_standard_leaf_layout(page)?;
	let mut low = 0;
	let mut high = count;
	while low < high {
		let middle = low + (high - low) / 2;
		let raw = standard_leaf_entry(page.as_bytes(), area_start, count, middle)?;
		match raw.key.cmp(target) {
			std::cmp::Ordering::Less => low = middle + 1,
			std::cmp::Ordering::Greater => high = middle,
			std::cmp::Ordering::Equal => return decode_scalar_atoms(read_page, raw),
		}
	}
	Ok(Vec::new())
}

pub(super) fn decode_standard_leaf_entries(
	read_page: &mut impl FnMut(PageNo, &mut AlignedPage) -> FerrosGrynnResult<()>,
	page: &AlignedPage,
) -> FerrosGrynnResult<Vec<ScalarIndexEntry>> {
	let (count, area_start) = validate_standard_leaf_layout(page)?;
	let mut entries = Vec::with_capacity(count);
	for index in 0..count {
		let raw = standard_leaf_entry(page.as_bytes(), area_start, count, index)?;
		entries.push(ScalarIndexEntry {
			key: raw.key.to_vec(),
			atoms: decode_scalar_atoms(read_page, raw)?,
		});
	}
	Ok(entries)
}

pub(super) struct StandardLeafEntry<'a> {
	key: &'a [u8],
	inline: &'a [u8],
	total_count: usize,
	posting: PageNo,
}

pub(super) fn standard_leaf_entry(
	bytes: &[u8; PAGE_SIZE],
	area_start: usize,
	count: usize,
	index: usize,
) -> FerrosGrynnResult<StandardLeafEntry<'_>> {
	let start = area_start + usize::from(read_u16(bytes, LEAF_OFFSETS_OFFSET + index * 2));
	let end = if index + 1 == count {
		PAGE_SIZE
	} else {
		area_start + usize::from(read_u16(bytes, LEAF_OFFSETS_OFFSET + (index + 1) * 2))
	};
	if start > end || end > PAGE_SIZE || end.saturating_sub(start) <= STANDARD_ENTRY_TRAILER_BYTES {
		return Err(invalid_index("scalar leaf entry range is invalid"));
	}
	let trailer = end - STANDARD_ENTRY_TRAILER_BYTES;
	let key_len = usize::from(read_u16(bytes, trailer));
	let inline_count = usize::from(read_u16(bytes, trailer + size_of::<u16>()));
	let total_count = usize::try_from(read_u32(bytes, trailer + 2 * size_of::<u16>()))
		.map_err(|_error| invalid_index("scalar atom count does not fit usize"))?;
	let posting = PageNo::new(read_u64(bytes, trailer + 2 * size_of::<u16>() + size_of::<u32>()));
	let inline_bytes = inline_count
		.checked_mul(DISK_ATOM_REF_BYTES)
		.ok_or_else(|| invalid_index("scalar inline atom bytes overflow usize"))?;
	if key_len == 0
		|| inline_count > STANDARD_INLINE_ATOMS
		|| inline_count != total_count.min(STANDARD_INLINE_ATOMS)
		|| start.checked_add(key_len).and_then(|end| end.checked_add(inline_bytes)) != Some(trailer)
		|| (total_count > STANDARD_INLINE_ATOMS) != (posting != PageNo::NULL)
	{
		return Err(invalid_index("scalar leaf entry metadata is inconsistent"));
	}
	Ok(StandardLeafEntry {
		key: &bytes[start..start + key_len],
		inline: &bytes[start + key_len..trailer],
		total_count,
		posting,
	})
}

pub(super) fn validate_standard_leaf_layout(
	page: &AlignedPage,
) -> FerrosGrynnResult<(usize, usize)> {
	let bytes = page.as_bytes();
	let header = page.validate(PageNo::new(read_u64(bytes, PAGE_NO_OFFSET)))?;
	if header.page_type() != PageType::IndexNonUniqueBtreeLeaf
		|| header.flags() != PageFlags::LEAF
		|| read_u16(bytes, LEAF_RESERVED_OFFSET) != 0
	{
		return Err(invalid_index("invalid standard scalar leaf header or padding"));
	}
	let count = usize::from(read_u16(bytes, LEAF_COUNT_OFFSET));
	let area_start = LEAF_OFFSETS_OFFSET
		.checked_add(count * size_of::<u16>())
		.ok_or_else(|| invalid_index("scalar leaf directory overflows usize"))?;
	if area_start > PAGE_SIZE {
		return Err(invalid_index("scalar leaf directory exceeds its page"));
	}
	Ok((count, area_start))
}

pub(super) fn decode_scalar_atoms(
	read_page: &mut impl FnMut(PageNo, &mut AlignedPage) -> FerrosGrynnResult<()>,
	entry: StandardLeafEntry<'_>,
) -> FerrosGrynnResult<Vec<DiskAtomRef>> {
	let mut atoms = entry
		.inline
		.chunks_exact(DISK_ATOM_REF_BYTES)
		.map(|bytes| DiskAtomRef::from_raw(read_u64(bytes, 0)))
		.collect::<FerrosGrynnResult<Vec<_>>>()?;
	let mut page_no = entry.posting;
	let mut visited = BTreeSet::new();
	while atoms.len() < entry.total_count {
		if page_no == PageNo::NULL || !visited.insert(page_no) {
			return Err(invalid_index("scalar posting chain ended early or contains a cycle"));
		}
		let mut page = AlignedPage::zeroed();
		read_page(page_no, &mut page)?;
		let header = page.validate(page_no)?;
		let bytes = page.as_bytes();
		if header.page_type() != PageType::IndexPostingBtreeLeaf
			|| header.flags() != PageFlags::LEAF
			|| read_u32(bytes, POSTING_RESERVED_OFFSET) != 0
		{
			return Err(invalid_index("invalid scalar posting page header or padding"));
		}
		let count = usize::try_from(read_u32(bytes, POSTING_COUNT_OFFSET))
			.map_err(|_error| invalid_index("posting count does not fit usize"))?;
		if count == 0 || count > POSTING_ATOMS_PER_PAGE || atoms.len() + count > entry.total_count {
			return Err(invalid_index("scalar posting page count is inconsistent"));
		}
		for index in 0..count {
			atoms.push(DiskAtomRef::from_raw(read_u64(
				bytes,
				POSTING_ATOMS_OFFSET + index * DISK_ATOM_REF_BYTES,
			))?);
		}
		validate_zero(
			&bytes[POSTING_ATOMS_OFFSET + count * DISK_ATOM_REF_BYTES..],
			"scalar posting trailing bytes",
		)?;
		page_no = PageNo::new(read_u64(bytes, POSTING_NEXT_OFFSET));
	}
	if page_no != PageNo::NULL || atoms.windows(2).any(|pair| pair[0] >= pair[1]) {
		return Err(invalid_index("scalar posting chain has a tail or unordered atoms"));
	}
	Ok(atoms)
}

pub(super) fn invalid_bounds(lower: &Bound<&[u8]>, upper: &Bound<&[u8]>) -> bool {
	match (lower, upper) {
		(Bound::Included(lower), Bound::Included(upper)) => lower > upper,
		(Bound::Included(lower) | Bound::Excluded(lower), Bound::Excluded(upper))
		| (Bound::Excluded(lower), Bound::Included(upper)) => lower >= upper,
		_ => false,
	}
}

pub(super) fn key_in_bounds(key: &[u8], lower: &Bound<&[u8]>, upper: &Bound<&[u8]>) -> bool {
	let lower = match lower {
		Bound::Included(lower) => key >= *lower,
		Bound::Excluded(lower) => key > *lower,
		Bound::Unbounded => true,
	};
	lower && !upper_exceeded(key, upper)
}

pub(super) fn upper_exceeded(key: &[u8], upper: &Bound<&[u8]>) -> bool {
	match upper {
		Bound::Included(upper) => key > *upper,
		Bound::Excluded(upper) => key >= *upper,
		Bound::Unbounded => false,
	}
}

pub(super) fn partition_leaf_entries(
	entries: &[PrimaryIndexEntry],
) -> FerrosGrynnResult<Vec<&[PrimaryIndexEntry]>> {
	partition_leaf_entries_with_limit(entries, PAGE_SIZE)
}

pub(super) fn partition_leaf_entries_with_limit(
	entries: &[PrimaryIndexEntry],
	maximum_bytes: usize,
) -> FerrosGrynnResult<Vec<&[PrimaryIndexEntry]>> {
	if maximum_bytes > PAGE_SIZE || maximum_bytes < LEAF_OFFSETS_OFFSET {
		return Err(invalid_index("primary leaf fill limit is outside page bounds"));
	}
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
				if end == start {
					return Err(invalid_index("one primary key exceeds leaf-page capacity"));
				}
				break;
			}
			if total > maximum_bytes && end > start {
				break;
			}
			data_bytes += entry_bytes;
			end += 1;
			if total > maximum_bytes {
				break;
			}
		}
		groups.push(&entries[start..end]);
		start = end;
	}
	Ok(groups)
}

pub(super) fn encode_unique_leaf(
	page: &mut AlignedPage,
	page_no: PageNo,
	page_lsn: PageLsn,
	next: Option<PageNo>,
	entries: &[PrimaryIndexEntry],
) -> FerrosGrynnResult<()> {
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
	page.reset(PageType::IndexUniqueBtreeLeaf, PageFlags::LEAF, page_lsn, page_no);
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
	page.seal()
}

pub(super) fn partition_internal_children(
	children: &[ChildPage],
) -> FerrosGrynnResult<Vec<&[ChildPage]>> {
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

pub(super) fn encode_internal_page(
	page: &mut AlignedPage,
	page_no: PageNo,
	page_lsn: PageLsn,
	children: &[ChildPage],
) -> FerrosGrynnResult<()> {
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
	page.reset(PageType::IndexBtreeInternal, PageFlags::empty(), page_lsn, page_no);
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
	page.seal()
}

/// Appends packed internal levels above an already encoded leaf level and
/// returns the final root page.
pub(super) fn append_internal_levels(
	pages: &mut Vec<AlignedPage>,
	mut level: Vec<ChildPage>,
	page_lsn: PageLsn,
) -> FerrosGrynnResult<PageNo> {
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
			let page_index = pages.len();
			pages.push(AlignedPage::zeroed());
			encode_internal_page(&mut pages[page_index], page_no, page_lsn, group)?;
			next_level.push(ChildPage {
				page_no,
				min_key: group[0].min_key.clone(),
			});
		}
		level = next_level;
	}
	Ok(level.first().map_or(PageNo::new(0), |child| child.page_no))
}

pub(super) fn lookup_leaf(
	page: &AlignedPage,
	target: &[u8],
) -> FerrosGrynnResult<Option<DiskAtomRef>> {
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

pub(super) fn validate_leaf_layout(page: &AlignedPage) -> FerrosGrynnResult<(usize, usize)> {
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

pub(super) fn leaf_entry(
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

pub(super) fn select_internal_child(
	page: &AlignedPage,
	target: &[u8],
) -> FerrosGrynnResult<PageNo> {
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

pub(super) fn validate_zero(bytes: &[u8], field: &str) -> FerrosGrynnResult<()> {
	if bytes.iter().any(|byte| *byte != 0) {
		return Err(invalid_index(format!("{field} are non-zero")));
	}
	Ok(())
}

pub(super) fn read_u16(bytes: &[u8], offset: usize) -> u16 {
	u16::from_le_bytes([bytes[offset], bytes[offset + 1]])
}

pub(super) fn read_u32(bytes: &[u8], offset: usize) -> u32 {
	u32::from_le_bytes([bytes[offset], bytes[offset + 1], bytes[offset + 2], bytes[offset + 3]])
}

pub(super) fn read_u64(bytes: &[u8], offset: usize) -> u64 {
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

pub(super) fn write_u16(bytes: &mut [u8], offset: usize, value: u16) {
	bytes[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
}

pub(super) fn write_u32(bytes: &mut [u8], offset: usize, value: u32) {
	bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

pub(super) fn write_u64(bytes: &mut [u8], offset: usize, value: u64) {
	bytes[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
}

pub(super) fn invalid_index(message: impl Into<String>) -> FerrosGrynnError {
	FerrosGrynnError::invalid_storage_file(message)
}
