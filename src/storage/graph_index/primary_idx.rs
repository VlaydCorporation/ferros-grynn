#[allow(clippy::wildcard_imports, reason = "index layers use the graph-index facade contract")]
use super::*;

/// One canonical primary-index item.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::storage) struct PrimaryIndexEntry {
	pub(in crate::storage) key: Vec<u8>,
	pub(in crate::storage) atom: DiskAtomRef,
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
