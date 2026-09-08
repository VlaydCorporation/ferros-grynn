//! Streaming bottom-up B+tree packing from externally sorted records.

#[allow(
	clippy::wildcard_imports,
	reason = "bulk packers share the graph-index codec contract"
)]
use super::{primary_idx::BuiltPrimaryIndex, scalar_idx::BuiltScalarIndex, *};

/// Packs a unique B+tree without retaining all sorted records in memory.
pub(in crate::storage) fn build_primary_index_sorted(
	records: &SortedIndexRecords,
	page_lsn: PageLsn,
) -> FerrosGrynnResult<BuiltPrimaryIndex> {
	let mut pages = Vec::new();
	let mut level = Vec::new();
	let mut leaf = Vec::new();
	let mut leaf_data_bytes = 0_usize;
	let mut previous_key = None::<Vec<u8>>;
	records.visit(|key, atom| {
		if previous_key.as_deref().is_some_and(|previous| previous >= key) {
			return Err(FerrosGrynnError::graph_constraint_violation(
				"primary index",
				"unique index contains a duplicate key",
			));
		}
		let entry_bytes = key
			.len()
			.checked_add(DISK_ATOM_REF_BYTES)
			.ok_or_else(|| invalid_index("primary leaf entry length overflows usize"))?;
		if !leaf.is_empty() && !leaf_accepts(leaf.len() + 1, leaf_data_bytes, entry_bytes)? {
			flush_unique_leaf(&mut pages, &mut level, &leaf, page_lsn, true)?;
			leaf.clear();
			leaf_data_bytes = 0;
		}
		if !leaf_accepts(leaf.len() + 1, leaf_data_bytes, entry_bytes)? {
			return Err(invalid_index("one primary key exceeds leaf-page capacity"));
		}
		leaf_data_bytes = leaf_data_bytes
			.checked_add(entry_bytes)
			.ok_or_else(|| invalid_index("primary leaf data length overflows usize"))?;
		leaf.push(PrimaryIndexEntry {
			key: key.to_vec(),
			atom,
		});
		previous_key = Some(key.to_vec());
		Ok(())
	})?;
	flush_unique_leaf(&mut pages, &mut level, &leaf, page_lsn, false)?;
	let root = append_internal_levels(&mut pages, level, page_lsn)?;
	Ok(BuiltPrimaryIndex {
		root,
		pages,
	})
}

/// Packs a standard or unique scalar B+tree directly from `(key, atom)` order.
pub(in crate::storage) fn build_scalar_index_sorted(
	records: &SortedIndexRecords,
	mode: ScalarIndexMode,
	page_lsn: PageLsn,
) -> FerrosGrynnResult<BuiltScalarIndex> {
	if mode == ScalarIndexMode::Unique {
		let built = build_primary_index_sorted(records, page_lsn)?;
		return Ok(BuiltScalarIndex {
			root: built.root,
			pages: built.pages,
		});
	}

	let leaf_count = count_standard_leaves(records)?;
	let mut pages = Vec::with_capacity(leaf_count);
	pages.resize_with(leaf_count, AlignedPage::zeroed);
	let mut level = Vec::new();
	let mut leaf = Vec::new();
	let mut leaf_data_bytes = 0_usize;
	let mut leaf_index = 0_usize;
	let mut group = None::<ScalarIndexEntry>;
	records.visit(|key, atom| {
		if let Some(entry) = group.as_mut()
			&& entry.key == key
		{
			if entry.atoms.last().is_some_and(|previous| *previous >= atom) {
				return Err(invalid_index("external-sort atoms are not ordered"));
			}
			// ponytail: one posting list is retained while its output pages
			// are already retained by the generation image; stream it
			// separately only if a single ultra-common key becomes the
			// measured memory ceiling.
			entry.atoms.push(atom);
			return Ok(());
		}
		if group.as_ref().is_some_and(|entry| entry.key.as_slice() > key) {
			return Err(invalid_index("external-sort scalar keys are not ordered"));
		}
		if let Some(entry) = group.take() {
			append_standard_entry(
				&mut pages,
				&mut level,
				&mut leaf,
				&mut leaf_data_bytes,
				&mut leaf_index,
				leaf_count,
				entry,
				page_lsn,
			)?;
		}
		group = Some(ScalarIndexEntry {
			key: key.to_vec(),
			atoms: vec![atom],
		});
		Ok(())
	})?;
	if let Some(entry) = group {
		append_standard_entry(
			&mut pages,
			&mut level,
			&mut leaf,
			&mut leaf_data_bytes,
			&mut leaf_index,
			leaf_count,
			entry,
			page_lsn,
		)?;
	}
	flush_standard_leaf(&mut pages, &mut level, leaf_index, leaf_count, &leaf, page_lsn)?;
	if leaf_index.checked_add(1) != Some(leaf_count) {
		return Err(invalid_index("scalar leaf sizing pass disagrees with packing pass"));
	}
	let root = append_internal_levels(&mut pages, level, page_lsn)?;
	Ok(BuiltScalarIndex {
		root,
		pages,
	})
}

/// Adds one complete posting group to the current leaf, flushing it first when
/// its inline representation no longer fits.
fn append_standard_entry(
	pages: &mut Vec<AlignedPage>,
	level: &mut Vec<ChildPage>,
	leaf: &mut Vec<ScalarIndexEntry>,
	leaf_data_bytes: &mut usize,
	leaf_index: &mut usize,
	leaf_count: usize,
	entry: ScalarIndexEntry,
	page_lsn: PageLsn,
) -> FerrosGrynnResult<()> {
	let entry_bytes = standard_entry_bytes(&entry)?;
	if !leaf.is_empty() && !leaf_accepts(leaf.len() + 1, *leaf_data_bytes, entry_bytes)? {
		flush_standard_leaf(pages, level, *leaf_index, leaf_count, leaf, page_lsn)?;
		*leaf_index = leaf_index
			.checked_add(1)
			.ok_or_else(|| invalid_index("scalar leaf index overflows usize"))?;
		leaf.clear();
		*leaf_data_bytes = 0;
	}
	if !leaf_accepts(leaf.len() + 1, *leaf_data_bytes, entry_bytes)? {
		return Err(invalid_index("one scalar key exceeds leaf-page capacity"));
	}
	*leaf_data_bytes = leaf_data_bytes
		.checked_add(entry_bytes)
		.ok_or_else(|| invalid_index("scalar leaf data length overflows usize"))?;
	leaf.push(entry);
	Ok(())
}

/// Encodes one unique leaf after the caller has established whether it has a
/// successor.
fn flush_unique_leaf(
	pages: &mut Vec<AlignedPage>,
	level: &mut Vec<ChildPage>,
	leaf: &[PrimaryIndexEntry],
	page_lsn: PageLsn,
	has_next: bool,
) -> FerrosGrynnResult<()> {
	if leaf.is_empty() && !pages.is_empty() {
		return Ok(());
	}
	let page_no = page_number(pages.len(), "primary leaf page index")?;
	let next = has_next.then(|| PageNo::new(page_no.get() + 1));
	let page_index = pages.len();
	pages.push(AlignedPage::zeroed());
	encode_unique_leaf(&mut pages[page_index], page_no, page_lsn, next, leaf)?;
	level.push(ChildPage {
		page_no,
		min_key: leaf.first().map_or_else(Vec::new, |entry| entry.key.clone()),
	});
	Ok(())
}

/// Encodes one preallocated standard leaf and appends its posting pages after
/// the complete contiguous leaf range.
fn flush_standard_leaf(
	pages: &mut Vec<AlignedPage>,
	level: &mut Vec<ChildPage>,
	leaf_index: usize,
	leaf_count: usize,
	leaf: &[ScalarIndexEntry],
	page_lsn: PageLsn,
) -> FerrosGrynnResult<()> {
	if leaf_index >= leaf_count || pages.len() < leaf_count {
		return Err(invalid_index("scalar leaf index exceeds its sizing pass"));
	}
	let page_no = page_number(leaf_index, "scalar leaf page index")?;
	let mut next_posting = page_number(pages.len(), "scalar posting page index")?.get();
	let mut posting_roots = BTreeMap::new();
	for entry in leaf.iter().filter(|entry| entry.atoms.len() > STANDARD_INLINE_ATOMS) {
		posting_roots.insert(entry.key.as_slice(), PageNo::new(next_posting));
		let pages = entry
			.atoms
			.len()
			.saturating_sub(STANDARD_INLINE_ATOMS)
			.div_ceil(POSTING_ATOMS_PER_PAGE);
		next_posting = next_posting
			.checked_add(
				u64::try_from(pages)
					.map_err(|_error| invalid_index("scalar posting count exceeds u64"))?,
			)
			.ok_or_else(|| invalid_index("scalar posting page number overflows u64"))?;
	}
	let next = if leaf_index + 1 < leaf_count {
		Some(page_number(leaf_index + 1, "next scalar leaf page")?)
	} else {
		None
	};
	encode_standard_leaf(&mut pages[leaf_index], page_no, page_lsn, next, leaf, &posting_roots)?;
	level.push(ChildPage {
		page_no,
		min_key: leaf.first().map_or_else(Vec::new, |entry| entry.key.clone()),
	});
	for entry in leaf {
		let chunks = entry.atoms[entry.atoms.len().min(STANDARD_INLINE_ATOMS)..]
			.chunks(POSTING_ATOMS_PER_PAGE);
		let chunk_count = chunks.len();
		for (index, chunk) in chunks.enumerate() {
			let posting_page = page_number(pages.len(), "scalar posting page index")?;
			let next = (index + 1 < chunk_count).then(|| PageNo::new(posting_page.get() + 1));
			let page_index = pages.len();
			pages.push(AlignedPage::zeroed());
			encode_posting_page(&mut pages[page_index], posting_page, page_lsn, next, chunk)?;
		}
	}
	Ok(())
}

/// Replays the sorted stream to determine the exact contiguous leaf range
/// before posting pages receive physical page numbers.
fn count_standard_leaves(records: &SortedIndexRecords) -> FerrosGrynnResult<usize> {
	let mut leaf_count = 0_usize;
	let mut leaf_entries = 0_usize;
	let mut leaf_data_bytes = 0_usize;
	let mut group_key = None::<Vec<u8>>;
	let mut group_atoms = 0_usize;
	let mut last_atom = None::<DiskAtomRef>;
	records.visit(|key, atom| {
		if group_key.as_deref() == Some(key) {
			if last_atom.is_some_and(|previous| previous >= atom) {
				return Err(invalid_index("external-sort atoms are not ordered"));
			}
			group_atoms = group_atoms
				.checked_add(1)
				.ok_or_else(|| invalid_index("scalar posting count overflows usize"))?;
			last_atom = Some(atom);
			return Ok(());
		}
		if group_key.as_deref().is_some_and(|previous| previous > key) {
			return Err(invalid_index("external-sort scalar keys are not ordered"));
		}
		if let Some(previous) = group_key.take() {
			account_standard_group(
				previous.len(),
				group_atoms,
				&mut leaf_count,
				&mut leaf_entries,
				&mut leaf_data_bytes,
			)?;
		}
		group_key = Some(key.to_vec());
		group_atoms = 1;
		last_atom = Some(atom);
		Ok(())
	})?;
	if let Some(key) = group_key {
		account_standard_group(
			key.len(),
			group_atoms,
			&mut leaf_count,
			&mut leaf_entries,
			&mut leaf_data_bytes,
		)?;
	}
	if leaf_entries != 0 {
		leaf_count = leaf_count
			.checked_add(1)
			.ok_or_else(|| invalid_index("scalar leaf count overflows usize"))?;
	}
	Ok(leaf_count.max(1))
}

/// Accounts for one completed key group in the first packing pass.
fn account_standard_group(
	key_len: usize,
	atom_count: usize,
	leaf_count: &mut usize,
	leaf_entries: &mut usize,
	leaf_data_bytes: &mut usize,
) -> FerrosGrynnResult<()> {
	let entry_bytes = standard_entry_size(key_len, atom_count)?;
	if *leaf_entries != 0 && !leaf_accepts(*leaf_entries + 1, *leaf_data_bytes, entry_bytes)? {
		*leaf_count = leaf_count
			.checked_add(1)
			.ok_or_else(|| invalid_index("scalar leaf count overflows usize"))?;
		*leaf_entries = 0;
		*leaf_data_bytes = 0;
	}
	if !leaf_accepts(*leaf_entries + 1, *leaf_data_bytes, entry_bytes)? {
		return Err(invalid_index("one scalar key exceeds leaf-page capacity"));
	}
	*leaf_entries += 1;
	*leaf_data_bytes = leaf_data_bytes
		.checked_add(entry_bytes)
		.ok_or_else(|| invalid_index("scalar leaf data length overflows usize"))?;
	Ok(())
}

/// Returns whether one candidate entry fits the leaf directory and data area.
fn leaf_accepts(count: usize, data_bytes: usize, entry_bytes: usize) -> FerrosGrynnResult<bool> {
	LEAF_OFFSETS_OFFSET
		.checked_add(
			count
				.checked_mul(size_of::<u16>())
				.ok_or_else(|| invalid_index("leaf directory size overflows usize"))?,
		)
		.and_then(|used| used.checked_add(data_bytes))
		.and_then(|used| used.checked_add(entry_bytes))
		.map(|used| used <= PAGE_SIZE)
		.ok_or_else(|| invalid_index("leaf page size overflows usize"))
}

/// Computes the inline leaf bytes for one standard posting group.
fn standard_entry_bytes(entry: &ScalarIndexEntry) -> FerrosGrynnResult<usize> {
	standard_entry_size(entry.key.len(), entry.atoms.len())
}

/// Computes one standard entry size without retaining its posting list.
fn standard_entry_size(key_len: usize, atom_count: usize) -> FerrosGrynnResult<usize> {
	key_len
		.checked_add(atom_count.min(STANDARD_INLINE_ATOMS) * DISK_ATOM_REF_BYTES)
		.and_then(|bytes| bytes.checked_add(STANDARD_ENTRY_TRAILER_BYTES))
		.ok_or_else(|| invalid_index("scalar leaf entry size overflows usize"))
}

/// Converts one output-vector index into a persistent page number.
fn page_number(index: usize, field: &str) -> FerrosGrynnResult<PageNo> {
	u64::try_from(index)
		.map(PageNo::new)
		.map_err(|_error| invalid_index(format!("{field} exceeds u64")))
}
