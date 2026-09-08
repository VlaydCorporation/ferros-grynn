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
#[cfg(test)]
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

	let leaf_groups = partition_standard_leaf_entries(entries)?;
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
