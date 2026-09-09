#[allow(clippy::wildcard_imports, reason = "index layers use the graph-index facade contract")]
use super::*;

/// Closed physical representation of one scalar B+tree.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::storage) enum ScalarIndexMode {
	/// Multiple atoms may share one key and spill into posting pages.
	Standard,
	/// Every key maps to exactly one atom.
	Unique,
}

/// One sorted scalar-index key and its sorted, non-empty atom set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::storage) struct ScalarIndexEntry {
	pub(in crate::storage) key: Vec<u8>,
	pub(in crate::storage) atoms: Vec<DiskAtomRef>,
}

/// Complete immutable scalar B+tree generation.
pub(in crate::storage) struct BuiltScalarIndex {
	pub(in crate::storage) root: PageNo,
	pub(in crate::storage) pages: Vec<AlignedPage>,
}

/// Bulk-builds a standard or unique scalar B+tree from canonical entries.
pub(in crate::storage) fn build_scalar_index(
	entries: &[ScalarIndexEntry],
	mode: ScalarIndexMode,
	page_lsn: PageLsn,
) -> FerrosGrynnResult<BuiltScalarIndex> {
	validate_scalar_entries(entries, mode)?;
	if mode == ScalarIndexMode::Unique {
		let primary = entries
			.iter()
			.map(|entry| PrimaryIndexEntry {
				key: entry.key.clone(),
				atom: entry.atoms[0],
			})
			.collect::<Vec<_>>();
		let built = build_primary_index(&primary, page_lsn)?;
		return Ok(BuiltScalarIndex {
			root: built.root,
			pages: built.pages,
		});
	}
	build_standard_scalar_index(entries, page_lsn, MUTABLE_STANDARD_LEAF_BYTES)
}

fn build_standard_scalar_index(
	entries: &[ScalarIndexEntry],
	page_lsn: PageLsn,
	maximum_leaf_bytes: usize,
) -> FerrosGrynnResult<BuiltScalarIndex> {
	let leaf_groups = partition_standard_leaf_entries_with_limit(entries, maximum_leaf_bytes)?;
	let leaf_count = leaf_groups.len();
	let mut next_page = u64::try_from(leaf_count)
		.map_err(|_error| invalid_index("scalar leaf count exceeds u64"))?;
	let mut posting_roots = BTreeMap::new();
	let mut posting_pages = Vec::new();
	for entry in entries {
		if entry.atoms.len() <= STANDARD_INLINE_ATOMS {
			continue;
		}
		let root = PageNo::new(next_page);
		posting_roots.insert(entry.key.as_slice(), root);
		let chunks =
			entry.atoms[STANDARD_INLINE_ATOMS..].chunks(POSTING_ATOMS_PER_PAGE).collect::<Vec<_>>();
		for (index, chunk) in chunks.iter().enumerate() {
			let page_no = PageNo::new(next_page);
			next_page = next_page
				.checked_add(1)
				.ok_or_else(|| invalid_index("scalar posting page number overflows u64"))?;
			let next = (index + 1 < chunks.len()).then(|| PageNo::new(next_page));
			let page_index = posting_pages.len();
			posting_pages.push(AlignedPage::zeroed());
			encode_posting_page(&mut posting_pages[page_index], page_no, page_lsn, next, chunk)?;
		}
	}

	let mut pages = Vec::new();
	let mut level = Vec::with_capacity(leaf_count);
	for (index, group) in leaf_groups.iter().enumerate() {
		let page_no = PageNo::new(
			u64::try_from(index)
				.map_err(|_error| invalid_index("scalar leaf index exceeds u64"))?,
		);
		let next = (index + 1 < leaf_count).then(|| PageNo::new(page_no.get() + 1));
		let page_index = pages.len();
		pages.push(AlignedPage::zeroed());
		encode_standard_leaf(
			&mut pages[page_index],
			page_no,
			page_lsn,
			next,
			group,
			&posting_roots,
		)?;
		level.push(ChildPage {
			page_no,
			min_key: group.first().map_or_else(Vec::new, |entry| entry.key.clone()),
		});
	}
	pages.extend(posting_pages);
	let root = append_internal_levels(&mut pages, level, page_lsn)?;
	Ok(BuiltScalarIndex {
		root,
		pages,
	})
}

#[cfg(test)]
pub(super) fn build_standard_scalar_index_with_fill(
	entries: &[ScalarIndexEntry],
	page_lsn: PageLsn,
	fill_percent: usize,
) -> FerrosGrynnResult<BuiltScalarIndex> {
	validate_scalar_entries(entries, ScalarIndexMode::Standard)?;
	if !(1..=100).contains(&fill_percent) {
		return Err(invalid_index("scalar leaf fill percent must be between 1 and 100"));
	}
	build_standard_scalar_index(entries, page_lsn, PAGE_SIZE.saturating_mul(fill_percent) / 100)
}

/// One atom insertion or removal for a canonical scalar key.
pub(in crate::storage) struct ScalarIndexMutation {
	pub(in crate::storage) key: Vec<u8>,
	pub(in crate::storage) atom: DiskAtomRef,
	pub(in crate::storage) insert: bool,
}

/// Re-encodes only scalar leaves whose separators and posting layout remain
/// stable. `None` requests the existing bulk builder.
pub(in crate::storage) fn mutate_scalar_index_leaves(
	mut read_page: impl FnMut(PageNo, &mut AlignedPage) -> FerrosGrynnResult<()>,
	root: PageNo,
	mode: ScalarIndexMode,
	mutations: &[ScalarIndexMutation],
	page_lsn: PageLsn,
) -> FerrosGrynnResult<Option<Vec<(PageNo, AlignedPage)>>> {
	let leaf_type = match mode {
		ScalarIndexMode::Standard => PageType::IndexNonUniqueBtreeLeaf,
		ScalarIndexMode::Unique => PageType::IndexUniqueBtreeLeaf,
	};
	let mut seen = BTreeSet::new();
	let mut grouped = BTreeMap::<PageNo, Vec<&ScalarIndexMutation>>::new();
	for mutation in mutations {
		if mutation.key.is_empty()
			|| !seen.insert((mutation.key.as_slice(), mutation.atom, mutation.insert))
		{
			return Err(invalid_index("scalar mutations must be non-empty and unique"));
		}
		let leaf = descend_to_leaf(&mut read_page, root, &mutation.key, leaf_type)?;
		grouped.entry(leaf).or_default().push(mutation);
	}

	let mut pages = Vec::with_capacity(grouped.len());
	for (page_no, changes) in grouped {
		let mut current = AlignedPage::zeroed();
		read_page(page_no, &mut current)?;
		if current.validate(page_no)?.page_type() != leaf_type {
			return Err(invalid_index("scalar mutation did not reach its expected leaf"));
		}
		let next = PageNo::new(read_u64(current.as_bytes(), LEAF_NEXT_OFFSET));
		let mut entries = match mode {
			ScalarIndexMode::Standard => decode_standard_leaf_entries(&mut read_page, &current)?,
			ScalarIndexMode::Unique => decode_unique_leaf_entries(&current)?,
		};
		let previous_first = entries.first().map(|entry| entry.key.clone());
		apply_scalar_mutations(&mut entries, changes, mode)?;
		if page_no != root
			&& (entries.is_empty()
				|| previous_first.as_deref() != entries.first().map(|entry| entry.key.as_slice()))
		{
			return Ok(None);
		}
		let mut page = AlignedPage::zeroed();
		match mode {
			ScalarIndexMode::Standard => {
				if entries.iter().any(|entry| entry.atoms.len() > STANDARD_INLINE_ATOMS)
					|| partition_standard_leaf_entries(&entries)?.len() != 1
				{
					return Ok(None);
				}
				encode_standard_leaf(
					&mut page,
					page_no,
					page_lsn,
					(next != PageNo::NULL).then_some(next),
					&entries,
					&BTreeMap::new(),
				)?;
			}
			ScalarIndexMode::Unique => {
				let unique = entries
					.iter()
					.map(|entry| PrimaryIndexEntry {
						key: entry.key.clone(),
						atom: entry.atoms[0],
					})
					.collect::<Vec<_>>();
				if partition_leaf_entries(&unique)?.len() != 1 {
					return Ok(None);
				}
				encode_unique_leaf(
					&mut page,
					page_no,
					page_lsn,
					(next != PageNo::NULL).then_some(next),
					&unique,
				)?;
			}
		}
		pages.push((page_no, page));
	}
	Ok(Some(pages))
}

/// Applies removals before insertions so a valid unique-key swap is atomic.
pub(in crate::storage) fn apply_scalar_mutations<'mutation>(
	entries: &mut Vec<ScalarIndexEntry>,
	mutations: impl IntoIterator<Item = &'mutation ScalarIndexMutation>,
	mode: ScalarIndexMode,
) -> FerrosGrynnResult<()> {
	let mutations = mutations.into_iter().collect::<Vec<_>>();
	for insert in [false, true] {
		for mutation in mutations.iter().filter(|mutation| mutation.insert == insert) {
			apply_scalar_mutation(entries, mutation, mode)?;
		}
	}
	Ok(())
}

/// Reads every scalar entry in canonical key order.
pub(in crate::storage) fn scan_scalar_index(
	mut read_page: impl FnMut(PageNo, &mut AlignedPage) -> FerrosGrynnResult<()>,
	root: PageNo,
	mode: ScalarIndexMode,
) -> FerrosGrynnResult<Vec<ScalarIndexEntry>> {
	if mode == ScalarIndexMode::Unique {
		return scan_primary_index(read_page, root).map(|entries| {
			entries
				.into_iter()
				.map(|entry| ScalarIndexEntry {
					key: entry.key,
					atoms: vec![entry.atom],
				})
				.collect()
		});
	}
	let mut page_no =
		descend_to_leaf(&mut read_page, root, &[], PageType::IndexNonUniqueBtreeLeaf)?;
	let mut visited = BTreeSet::new();
	let mut entries = Vec::new();
	while page_no != PageNo::NULL {
		if !visited.insert(page_no) {
			return Err(invalid_index("scalar B+tree leaf chain contains a cycle"));
		}
		let mut page = AlignedPage::zeroed();
		read_page(page_no, &mut page)?;
		for entry in decode_standard_leaf_entries(&mut read_page, &page)? {
			if entries.last().is_some_and(|previous: &ScalarIndexEntry| previous.key >= entry.key) {
				return Err(invalid_index("scalar B+tree keys are not strictly ordered"));
			}
			entries.push(entry);
		}
		page_no = PageNo::new(read_u64(page.as_bytes(), LEAF_NEXT_OFFSET));
	}
	Ok(entries)
}

fn decode_unique_leaf_entries(page: &AlignedPage) -> FerrosGrynnResult<Vec<ScalarIndexEntry>> {
	let (count, area_start) = validate_leaf_layout(page)?;
	(0..count)
		.map(|index| {
			let (key, atom) = leaf_entry(page.as_bytes(), area_start, count, index)?;
			Ok(ScalarIndexEntry {
				key: key.to_vec(),
				atoms: vec![DiskAtomRef::from_raw(atom)?],
			})
		})
		.collect()
}

fn apply_scalar_mutation(
	entries: &mut Vec<ScalarIndexEntry>,
	mutation: &ScalarIndexMutation,
	mode: ScalarIndexMode,
) -> FerrosGrynnResult<()> {
	match entries.binary_search_by(|entry| entry.key.cmp(&mutation.key)) {
		Ok(_index) if mutation.insert && mode == ScalarIndexMode::Unique => {
			Err(FerrosGrynnError::graph_constraint_violation(
				"scalar index",
				"unique scalar index contains a duplicate key",
			))
		}
		Ok(index) if mutation.insert => {
			let atoms = &mut entries[index].atoms;
			match atoms.binary_search(&mutation.atom) {
				Ok(_index) => Err(FerrosGrynnError::invalid_storage_file(
					"scalar mutation inserts a duplicate atom reference",
				)),
				Err(position) => {
					atoms.insert(position, mutation.atom);
					Ok(())
				}
			}
		}
		Ok(index) => {
			let atoms = &mut entries[index].atoms;
			let position = atoms.binary_search(&mutation.atom).map_err(|_index| {
				FerrosGrynnError::invalid_storage_file(
					"scalar mutation removes an absent atom reference",
				)
			})?;
			atoms.remove(position);
			if atoms.is_empty() {
				entries.remove(index);
			}
			Ok(())
		}
		Err(index) if mutation.insert => {
			entries.insert(index, ScalarIndexEntry {
				key: mutation.key.clone(),
				atoms: vec![mutation.atom],
			});
			Ok(())
		}
		Err(_index) => {
			Err(FerrosGrynnError::invalid_storage_file("scalar mutation removes an absent key"))
		}
	}
}

/// Looks up one exact scalar key and validates every referenced posting page.
pub(in crate::storage) fn lookup_scalar_index(
	mut read_page: impl FnMut(PageNo, &mut AlignedPage) -> FerrosGrynnResult<()>,
	root: PageNo,
	mode: ScalarIndexMode,
	target: &[u8],
) -> FerrosGrynnResult<Vec<DiskAtomRef>> {
	if mode == ScalarIndexMode::Unique {
		return lookup_primary_index(read_page, root, target)
			.map(|atom| atom.into_iter().collect());
	}
	let leaf = descend_to_leaf(&mut read_page, root, target, PageType::IndexNonUniqueBtreeLeaf)?;
	let mut page = AlignedPage::zeroed();
	read_page(leaf, &mut page)?;
	lookup_standard_leaf(&mut read_page, &page, target)
}

/// Returns scalar-index atoms in key order for one bounded range.
pub(in crate::storage) fn range_scalar_index(
	mut read_page: impl FnMut(PageNo, &mut AlignedPage) -> FerrosGrynnResult<()>,
	root: PageNo,
	mode: ScalarIndexMode,
	lower: Bound<&[u8]>,
	upper: Bound<&[u8]>,
) -> FerrosGrynnResult<Vec<DiskAtomRef>> {
	if invalid_bounds(&lower, &upper) {
		return Ok(Vec::new());
	}
	if mode == ScalarIndexMode::Unique {
		return range_unique_index(&mut read_page, root, &lower, &upper);
	}

	let target = match lower {
		Bound::Included(key) | Bound::Excluded(key) => key,
		Bound::Unbounded => &[],
	};
	let mut page_no =
		descend_to_leaf(&mut read_page, root, target, PageType::IndexNonUniqueBtreeLeaf)?;
	let mut visited = BTreeSet::new();
	let mut result = Vec::new();
	while page_no != PageNo::NULL {
		if !visited.insert(page_no) {
			return Err(invalid_index("scalar B+tree leaf chain contains a cycle"));
		}
		let mut page = AlignedPage::zeroed();
		read_page(page_no, &mut page)?;
		let entries = decode_standard_leaf_entries(&mut read_page, &page)?;
		for entry in entries {
			if upper_exceeded(&entry.key, &upper) {
				return Ok(result);
			}
			if key_in_bounds(&entry.key, &lower, &upper) {
				result.extend(entry.atoms);
			}
		}
		page_no = PageNo::new(read_u64(page.as_bytes(), LEAF_NEXT_OFFSET));
	}
	Ok(result)
}

fn range_unique_index(
	read_page: &mut impl FnMut(PageNo, &mut AlignedPage) -> FerrosGrynnResult<()>,
	root: PageNo,
	lower: &Bound<&[u8]>,
	upper: &Bound<&[u8]>,
) -> FerrosGrynnResult<Vec<DiskAtomRef>> {
	let target = match lower {
		Bound::Included(key) | Bound::Excluded(key) => *key,
		Bound::Unbounded => &[],
	};
	let mut page_no = descend_to_leaf(read_page, root, target, PageType::IndexUniqueBtreeLeaf)?;
	let mut visited = BTreeSet::new();
	let mut previous_key = None::<Vec<u8>>;
	let mut result = Vec::new();
	while page_no != PageNo::NULL {
		if !visited.insert(page_no) {
			return Err(invalid_index("unique scalar B+tree leaf chain contains a cycle"));
		}
		let mut page = AlignedPage::zeroed();
		read_page(page_no, &mut page)?;
		let header = page.validate(page_no)?;
		if header.page_type() != PageType::IndexUniqueBtreeLeaf {
			return Err(invalid_index("unique scalar B+tree leaf link targets an invalid page"));
		}
		let bytes = page.as_bytes();
		let (count, area_start) = validate_leaf_layout(&page)?;
		for index in 0..count {
			let (key, atom) = leaf_entry(bytes, area_start, count, index)?;
			if previous_key.as_deref().is_some_and(|previous| previous >= key) {
				return Err(invalid_index(
					"unique scalar B+tree leaf keys are not strictly ordered",
				));
			}
			if upper_exceeded(key, upper) {
				return Ok(result);
			}
			if key_in_bounds(key, lower, upper) {
				result.push(DiskAtomRef::from_raw(atom)?);
			}
			previous_key = Some(key.to_vec());
		}
		page_no = PageNo::new(read_u64(bytes, LEAF_NEXT_OFFSET));
	}
	Ok(result)
}
