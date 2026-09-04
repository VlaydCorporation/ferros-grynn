//! Format-v1 label dictionary and bulk-built primary B+tree.

use std::{
	collections::{BTreeMap, BTreeSet},
	ops::Bound,
};

use super::page::{
	AlignedPage,
	PAGE_HEADER_SIZE,
	PAGE_NO_OFFSET,
	PAGE_SIZE,
	PageFlags,
	PageLsn,
	PageNo,
	PageType,
};
use crate::{
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	values::storage::DiskAtomRef,
};

/// Shared B+tree page construction and lookup primitives.
mod btree_idx;
/// Label and time-zone dictionary pages.
mod label_dict;
/// Primary RecordId index.
mod primary_idx;
/// Standard and unique scalar indexes.
mod scalar_idx;

#[allow(
	clippy::wildcard_imports,
	reason = "specialized indexes share one private B+tree codec"
)]
use btree_idx::*;
pub(super) use label_dict::{LabelDictionary, decode_label_dictionary, encode_label_dictionary};
pub(super) use primary_idx::{
	PrimaryIndexEntry,
	build_primary_index,
	leftmost_primary_leaf,
	lookup_primary_index,
	scan_primary_index,
	visit_primary_leaf_page,
};
pub(super) use scalar_idx::{
	ScalarIndexEntry,
	ScalarIndexMode,
	build_scalar_index,
	lookup_scalar_index,
	range_scalar_index,
};

const LABEL_COUNT_OFFSETS: [usize; 4] = [32, 36, 40, 44];
const LABEL_ROOT_OFFSETS: [usize; 4] = [48, 56, 64, 72];
const LABEL_HASH_ROOT_OFFSET: usize = 80;
const LABEL_TZ_HASH_ROOT_OFFSET: usize = 88;
const LABEL_TZ_ROOT_OFFSET: usize = 96;
const LABEL_HEADER_RESERVED_OFFSET: usize = 104;
const LABEL_ENTRY_NEXT_OFFSET: usize = PAGE_HEADER_SIZE;
const LABEL_ENTRY_COUNT_OFFSET: usize = 40;
const LABEL_ENTRY_RESERVED_OFFSET: usize = 44;
const LABEL_ENTRY_DATA_OFFSET: usize = 48;
const LABEL_ENTRY_FIXED_BYTES: usize = 6;

const LEAF_NEXT_OFFSET: usize = PAGE_HEADER_SIZE;
const LEAF_COUNT_OFFSET: usize = 40;
const LEAF_RESERVED_OFFSET: usize = 42;
const LEAF_OFFSETS_OFFSET: usize = 44;
const INTERNAL_COUNT_OFFSET: usize = PAGE_HEADER_SIZE;
const INTERNAL_KEY_BYTES_OFFSET: usize = 34;
const INTERNAL_OFFSETS_OFFSET: usize = 36;
const MAX_BTREE_DEPTH: usize = 64;
const DISK_ATOM_REF_BYTES: usize = size_of::<u64>();
const STANDARD_ENTRY_TRAILER_BYTES: usize = 16;
const STANDARD_INLINE_ATOMS: usize = 8;
const POSTING_NEXT_OFFSET: usize = PAGE_HEADER_SIZE;
const POSTING_COUNT_OFFSET: usize = 40;
const POSTING_RESERVED_OFFSET: usize = 44;
const POSTING_ATOMS_OFFSET: usize = 48;
const POSTING_ATOMS_PER_PAGE: usize = (PAGE_SIZE - POSTING_ATOMS_OFFSET) / DISK_ATOM_REF_BYTES;

#[cfg(test)]
mod tests {
	use super::*;
	use crate::values::runtime::graph::AtomKind;

	#[test]
	fn label_dictionary_round_trips_multiple_sections_and_pages() -> FerrosGrynnResult<()> {
		let dictionary = LabelDictionary {
			node_labels: (0..1_200).map(|id| format!("node-{id:04}")).collect(),
			edge_labels: vec!["knows".to_owned()],
			property_keys: vec!["name".to_owned(), "age".to_owned()],
			table_names: vec!["person".to_owned()],
			time_zones: vec!["Europe/Moscow".to_owned(), "America/New_York".to_owned()],
		};
		let pages = encode_label_dictionary(&dictionary, PageLsn::ZERO)?;
		let decoded = decode_label_dictionary(|page_no, output| {
			let page = pages
				.get(page_no.get() as usize)
				.ok_or_else(|| invalid_index("test page missing"))?;
			output.as_bytes_mut().copy_from_slice(page.as_bytes());
			Ok(())
		})?;
		assert_eq!(decoded, dictionary);
		Ok(())
	}

	#[test]
	fn bulk_primary_tree_supports_multiple_levels_and_exact_lookup() -> FerrosGrynnResult<()> {
		let entries = (0..4_000_u64)
			.map(|id| {
				Ok(PrimaryIndexEntry {
					key: format!("person:{id:08}").into_bytes(),
					atom: DiskAtomRef::from_parts(AtomKind::Node, id)?,
				})
			})
			.collect::<FerrosGrynnResult<Vec<_>>>()?;
		let built = build_primary_index(&entries, PageLsn::ZERO)?;
		assert!(built.pages.len() > 1);
		let read = |page_no: PageNo, output: &mut AlignedPage| {
			let page = built
				.pages
				.get(page_no.get() as usize)
				.ok_or_else(|| invalid_index("test page missing"))?;
			output.as_bytes_mut().copy_from_slice(page.as_bytes());
			Ok(())
		};
		assert_eq!(
			lookup_primary_index(read, built.root, b"person:00003999")?.map(DiskAtomRef::slot),
			Some(3_999)
		);
		assert_eq!(lookup_primary_index(read, built.root, b"person:missing")?, None);
		let scanned = scan_primary_index(
			|page_no, output| {
				let page = built
					.pages
					.get(page_no.get() as usize)
					.ok_or_else(|| invalid_index("test page missing"))?;
				output.as_bytes_mut().copy_from_slice(page.as_bytes());
				Ok(())
			},
			built.root,
		)?;
		assert_eq!(scanned, entries);
		Ok(())
	}

	#[test]
	fn primary_tree_rejects_duplicate_keys() -> FerrosGrynnResult<()> {
		let atom = DiskAtomRef::from_parts(AtomKind::Node, 0)?;
		let entries = vec![
			PrimaryIndexEntry {
				key: b"same".to_vec(),
				atom,
			},
			PrimaryIndexEntry {
				key: b"same".to_vec(),
				atom,
			},
		];
		assert!(build_primary_index(&entries, PageLsn::ZERO).is_err());
		Ok(())
	}

	#[test]
	fn standard_scalar_tree_round_trips_postings_and_bounded_ranges() -> FerrosGrynnResult<()> {
		let repeated = (0..4_100_u64)
			.map(|slot| DiskAtomRef::from_parts(AtomKind::Node, slot))
			.collect::<FerrosGrynnResult<Vec<_>>>()?;
		let entries = vec![
			ScalarIndexEntry {
				key: b"alpha".to_vec(),
				atoms: vec![DiskAtomRef::from_parts(AtomKind::Edge, 1)?],
			},
			ScalarIndexEntry {
				key: b"middle".to_vec(),
				atoms: repeated.clone(),
			},
			ScalarIndexEntry {
				key: b"zulu".to_vec(),
				atoms: vec![DiskAtomRef::from_parts(AtomKind::Edge, 2)?],
			},
		];
		let built = build_scalar_index(&entries, ScalarIndexMode::Standard, PageLsn::ZERO)?;
		let read = |page_no: PageNo, output: &mut AlignedPage| {
			let page = built
				.pages
				.get(page_no.get() as usize)
				.ok_or_else(|| invalid_index("test scalar page missing"))?;
			output.as_bytes_mut().copy_from_slice(page.as_bytes());
			Ok(())
		};
		assert_eq!(
			lookup_scalar_index(read, built.root, ScalarIndexMode::Standard, b"middle")?,
			repeated
		);
		let range = range_scalar_index(
			|page_no, output| {
				let page = built
					.pages
					.get(page_no.get() as usize)
					.ok_or_else(|| invalid_index("test scalar page missing"))?;
				output.as_bytes_mut().copy_from_slice(page.as_bytes());
				Ok(())
			},
			built.root,
			ScalarIndexMode::Standard,
			Bound::Included(b"middle"),
			Bound::Excluded(b"zulu"),
		)?;
		assert_eq!(range.len(), 4_100);
		Ok(())
	}

	#[test]
	fn unique_scalar_tree_rejects_more_than_one_atom_per_key() -> FerrosGrynnResult<()> {
		let entries = vec![ScalarIndexEntry {
			key: b"duplicate".to_vec(),
			atoms: vec![
				DiskAtomRef::from_parts(AtomKind::Node, 1)?,
				DiskAtomRef::from_parts(AtomKind::Node, 2)?,
			],
		}];
		assert!(build_scalar_index(&entries, ScalarIndexMode::Unique, PageLsn::ZERO).is_err());
		Ok(())
	}

	#[test]
	fn unique_scalar_range_descends_to_its_lower_bound() -> FerrosGrynnResult<()> {
		let entries = (0..4_000_u64)
			.map(|slot| {
				Ok(ScalarIndexEntry {
					key: format!("{slot:08}").into_bytes(),
					atoms: vec![DiskAtomRef::from_parts(AtomKind::Node, slot)?],
				})
			})
			.collect::<FerrosGrynnResult<Vec<_>>>()?;
		let built = build_scalar_index(&entries, ScalarIndexMode::Unique, PageLsn::ZERO)?;
		assert!(built.root != PageNo::new(0));
		let mut first_leaf_read = false;
		let atoms = range_scalar_index(
			|page_no, output| {
				first_leaf_read |= page_no == PageNo::new(0);
				let page = built
					.pages
					.get(page_no.get() as usize)
					.ok_or_else(|| invalid_index("test scalar page missing"))?;
				output.as_bytes_mut().copy_from_slice(page.as_bytes());
				Ok(())
			},
			built.root,
			ScalarIndexMode::Unique,
			Bound::Included(b"00003990"),
			Bound::Included(b"00003995"),
		)?;
		assert_eq!(
			atoms.iter().map(|atom| atom.slot()).collect::<Vec<_>>(),
			(3_990..=3_995).collect::<Vec<_>>(),
		);
		assert!(!first_leaf_read);
		Ok(())
	}
}
