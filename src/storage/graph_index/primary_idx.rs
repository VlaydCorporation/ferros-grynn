#[allow(clippy::wildcard_imports, reason = "index layers use the graph-index facade contract")]
use super::*;

/// One canonical primary-index item.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::storage) struct PrimaryIndexEntry {
	pub(in crate::storage) key: Vec<u8>,
	pub(in crate::storage) atom: DiskAtomRef,
}

/// One primary-index insertion or removal applied by point DML.
pub(in crate::storage) struct PrimaryIndexMutation {
	pub(in crate::storage) key: Vec<u8>,
	pub(in crate::storage) atom: Option<DiskAtomRef>,
}

/// Complete immutable B+tree generation and its root page.
pub(in crate::storage) struct BuiltPrimaryIndex {
	pub(in crate::storage) root: PageNo,
	pub(in crate::storage) pages: Vec<AlignedPage>,
}

/// Bulk-builds a unique B+tree from strictly sorted canonical RecordId bytes.
pub(in crate::storage) fn build_primary_index(
	entries: &[PrimaryIndexEntry],
	page_lsn: PageLsn,
) -> FerrosGrynnResult<BuiltPrimaryIndex> {
	build_primary_index_with_leaf_limit(entries, page_lsn, MUTABLE_PRIMARY_LEAF_BYTES)
}

fn build_primary_index_with_leaf_limit(
	entries: &[PrimaryIndexEntry],
	page_lsn: PageLsn,
	maximum_leaf_bytes: usize,
) -> FerrosGrynnResult<BuiltPrimaryIndex> {
	validate_primary_entries(entries)?;
	let leaf_groups = partition_leaf_entries_with_limit(entries, maximum_leaf_bytes)?;
	let mut pages = Vec::new();
	let mut level = Vec::with_capacity(leaf_groups.len());
	for (index, group) in leaf_groups.iter().enumerate() {
		let page_no = PageNo::new(
			u64::try_from(index).map_err(|_error| invalid_index("leaf page index exceeds u64"))?,
		);
		let next = (index + 1 < leaf_groups.len()).then(|| PageNo::new(page_no.get() + 1));
		let page_index = pages.len();
		pages.push(AlignedPage::zeroed());
		encode_unique_leaf(&mut pages[page_index], page_no, page_lsn, next, group)?;
		level.push(ChildPage {
			page_no,
			min_key: group.first().map_or_else(Vec::new, |entry| entry.key.clone()),
		});
	}
	let root = append_internal_levels(&mut pages, level, page_lsn)?;
	Ok(BuiltPrimaryIndex {
		root,
		pages,
	})
}

#[cfg(test)]
pub(super) fn build_primary_index_with_fill(
	entries: &[PrimaryIndexEntry],
	page_lsn: PageLsn,
	fill_percent: usize,
) -> FerrosGrynnResult<BuiltPrimaryIndex> {
	if !(1..=100).contains(&fill_percent) {
		return Err(invalid_index("primary leaf fill percent must be between 1 and 100"));
	}
	build_primary_index_with_leaf_limit(
		entries,
		page_lsn,
		PAGE_SIZE.saturating_mul(fill_percent) / 100,
	)
}

/// Performs one logarithmic primary lookup without decoding the key bytes.
pub(in crate::storage) fn lookup_primary_index(
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
pub(in crate::storage) fn scan_primary_index(
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
pub(in crate::storage) fn visit_primary_index(
	mut read_page: impl FnMut(PageNo, &mut AlignedPage) -> FerrosGrynnResult<()>,
	root: PageNo,
	mut visit: impl FnMut(&[u8], DiskAtomRef) -> FerrosGrynnResult<()>,
) -> FerrosGrynnResult<()> {
	let mut page_no = leftmost_primary_leaf(&mut read_page, root)?;

	let mut previous_key = None::<Vec<u8>>;
	let mut visited = BTreeSet::new();
	while page_no != PageNo::NULL {
		if !visited.insert(page_no) {
			return Err(invalid_index("primary B+tree leaf chain contains a cycle"));
		}
		let mut page = AlignedPage::zeroed();
		read_page(page_no, &mut page)?;
		page_no = visit_primary_leaf_page(page_no, &page, &mut previous_key, &mut visit)?;
	}
	Ok(())
}

/// Re-encodes only leaves whose separators remain stable.
///
/// `None` requests the existing bulk builder because a split, an empty
/// non-root leaf, or a changed internal separator is required.
pub(in crate::storage) fn mutate_primary_index_leaves(
	mut read_page: impl FnMut(PageNo, &mut AlignedPage) -> FerrosGrynnResult<()>,
	root: PageNo,
	mutations: &[PrimaryIndexMutation],
	page_lsn: PageLsn,
) -> FerrosGrynnResult<Option<Vec<(PageNo, AlignedPage)>>> {
	let mut unique = BTreeSet::new();
	let mut grouped = BTreeMap::<PageNo, Vec<&PrimaryIndexMutation>>::new();
	for mutation in mutations {
		if mutation.key.is_empty()
			|| !unique.insert((mutation.key.as_slice(), mutation.atom.is_some()))
		{
			return Err(invalid_index("primary mutation keys must be non-empty and unique"));
		}
		let leaf =
			descend_to_leaf(&mut read_page, root, &mutation.key, PageType::IndexUniqueBtreeLeaf)?;
		grouped.entry(leaf).or_default().push(mutation);
	}

	let mut pages = Vec::with_capacity(grouped.len());
	for (page_no, changes) in grouped {
		let mut current = AlignedPage::zeroed();
		read_page(page_no, &mut current)?;
		let header = current.validate(page_no)?;
		if header.page_type() != PageType::IndexUniqueBtreeLeaf {
			return Err(invalid_index("primary mutation did not reach a unique leaf"));
		}
		let next = PageNo::new(read_u64(current.as_bytes(), LEAF_NEXT_OFFSET));
		let (count, area_start) = validate_leaf_layout(&current)?;
		let mut entries = (0..count)
			.map(|index| {
				let (key, atom) = leaf_entry(current.as_bytes(), area_start, count, index)?;
				Ok(PrimaryIndexEntry {
					key: key.to_vec(),
					atom: DiskAtomRef::from_raw(atom)?,
				})
			})
			.collect::<FerrosGrynnResult<Vec<_>>>()?;
		let previous_first = entries.first().map(|entry| entry.key.clone());
		apply_primary_mutations(&mut entries, changes)?;
		if page_no != root
			&& (entries.is_empty()
				|| previous_first.as_deref() != entries.first().map(|entry| entry.key.as_slice()))
		{
			return Ok(None);
		}
		if partition_leaf_entries(&entries)?.len() != 1 {
			return Ok(None);
		}
		let mut page = AlignedPage::zeroed();
		encode_unique_leaf(
			&mut page,
			page_no,
			page_lsn,
			(next != PageNo::NULL).then_some(next),
			&entries,
		)?;
		pages.push((page_no, page));
	}
	Ok(Some(pages))
}

/// Applies removals before insertions so one stable `RecordId` can be reused
/// atomically with a new generation.
pub(in crate::storage) fn apply_primary_mutations<'mutation>(
	entries: &mut Vec<PrimaryIndexEntry>,
	mutations: impl IntoIterator<Item = &'mutation PrimaryIndexMutation>,
) -> FerrosGrynnResult<()> {
	let mutations = mutations.into_iter().collect::<Vec<_>>();
	for insert in [false, true] {
		for mutation in mutations.iter().filter(|mutation| mutation.atom.is_some() == insert) {
			match entries.binary_search_by(|entry| entry.key.cmp(&mutation.key)) {
				Ok(_index) if insert => {
					return Err(FerrosGrynnError::graph_constraint_violation(
						"record id",
						"primary mutation inserts a duplicate RecordId",
					));
				}
				Ok(index) => {
					entries.remove(index);
				}
				Err(index) if insert => {
					entries.insert(index, PrimaryIndexEntry {
						key: mutation.key.clone(),
						atom: mutation.atom.ok_or_else(|| {
							invalid_index("primary insertion has no atom reference")
						})?,
					});
				}
				Err(_index) => {
					return Err(invalid_index("primary mutation removes an absent RecordId"));
				}
			}
		}
	}
	Ok(())
}

/// Resolves the first leaf while retaining point-read semantics for internal
/// index pages.
pub(in crate::storage) fn leftmost_primary_leaf(
	mut read_page: impl FnMut(PageNo, &mut AlignedPage) -> FerrosGrynnResult<()>,
	root: PageNo,
) -> FerrosGrynnResult<PageNo> {
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
			PageType::IndexUniqueBtreeLeaf => return Ok(page_no),
			PageType::IndexBtreeInternal => page_no = select_internal_child(&page, &[])?,
			_ => return Err(invalid_index("primary B+tree contains an invalid page type")),
		}
	}
	Err(invalid_index("primary B+tree exceeds its maximum depth"))
}

/// Validates and visits one unique leaf, returning its exact successor.
pub(in crate::storage) fn visit_primary_leaf_page(
	page_no: PageNo,
	page: &AlignedPage,
	previous_key: &mut Option<Vec<u8>>,
	visit: &mut impl FnMut(&[u8], DiskAtomRef) -> FerrosGrynnResult<()>,
) -> FerrosGrynnResult<PageNo> {
	let header = page.validate(page_no)?;
	if header.page_type() != PageType::IndexUniqueBtreeLeaf {
		return Err(invalid_index("primary B+tree leaf link targets an invalid page"));
	}
	let bytes = page.as_bytes();
	let (count, area_start) = validate_leaf_layout(page)?;
	for index in 0..count {
		let (key, atom) = leaf_entry(bytes, area_start, count, index)?;
		if previous_key.as_deref().is_some_and(|previous| previous >= key) {
			return Err(invalid_index("primary B+tree leaf keys are not strictly ordered"));
		}
		visit(key, DiskAtomRef::from_raw(atom)?)?;
		*previous_key = Some(key.to_vec());
	}
	Ok(PageNo::new(read_u64(bytes, LEAF_NEXT_OFFSET)))
}
