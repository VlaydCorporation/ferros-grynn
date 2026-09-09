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
/// Streaming bottom-up packing from externally sorted records.
mod bulk_build;
/// Single-page table count indexes.
mod count_idx;
/// Bounded-memory Direct-I/O run sorting for generation builds.
mod external_sort;
/// Label and time-zone dictionary pages.
mod label_dict;
/// Per-label sorted atom arrays.
mod label_idx;
/// Primary RecordId index.
mod primary_idx;
/// Standard and unique scalar indexes.
mod scalar_idx;

#[allow(
	clippy::wildcard_imports,
	reason = "specialized indexes share one private B+tree codec"
)]
use btree_idx::*;
pub(super) use bulk_build::{build_primary_index_sorted, build_scalar_index_sorted};
pub(super) use count_idx::{
	CountIndexValue,
	decode_count_index,
	encode_count_index,
	stamp_count_index_lsn,
};
pub(super) use external_sort::{
	ExternalIndexSorter,
	SortedIndexRecords,
	cleanup_external_index_sort,
};
pub(super) use label_dict::{LabelDictionary, decode_label_dictionary, encode_label_dictionary};
pub(super) use label_idx::{build_label_index, visit_label_index_pages};
#[cfg(test)]
use primary_idx::build_primary_index_with_fill;
pub(super) use primary_idx::{
	PrimaryIndexEntry,
	PrimaryIndexMutation,
	apply_primary_mutations,
	build_primary_index,
	leftmost_primary_leaf,
	lookup_primary_index,
	mutate_primary_index_leaves,
	scan_primary_index,
	visit_primary_leaf_page,
};
#[cfg(test)]
use scalar_idx::build_standard_scalar_index_with_fill;
pub(super) use scalar_idx::{
	ScalarIndexEntry,
	ScalarIndexMode,
	ScalarIndexMutation,
	apply_scalar_mutations,
	build_scalar_index,
	lookup_scalar_index,
	mutate_scalar_index_leaves,
	range_scalar_index,
	scan_scalar_index,
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
const MUTABLE_PRIMARY_LEAF_BYTES: usize = PAGE_SIZE * 90 / 100;
const MUTABLE_STANDARD_LEAF_BYTES: usize = PAGE_SIZE * 80 / 100;

#[cfg(test)]
mod tests {
	use std::{
		fs,
		hint::black_box,
		sync::atomic::{AtomicU64, Ordering},
		time::{Duration, Instant},
	};

	use super::{primary_idx::BuiltPrimaryIndex, scalar_idx::BuiltScalarIndex, *};
	use crate::values::runtime::graph::AtomKind;

	/// Unique external-packer fixture suffix.
	static NEXT_SORT_DIRECTORY: AtomicU64 = AtomicU64::new(0);

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
	fn primary_leaf_mutation_is_atomic_and_defers_separator_changes() -> FerrosGrynnResult<()> {
		let entries = [
			PrimaryIndexEntry {
				key: b"alpha".to_vec(),
				atom: DiskAtomRef::from_parts(AtomKind::Node, 0)?,
			},
			PrimaryIndexEntry {
				key: b"zulu".to_vec(),
				atom: DiskAtomRef::from_parts(AtomKind::Node, 1)?,
			},
		];
		let mut built = build_primary_index(&entries, PageLsn::ZERO)?;
		let pages = mutate_primary_index_leaves(
			|page_no, output| {
				output
					.as_bytes_mut()
					.copy_from_slice(built.pages[page_no.get() as usize].as_bytes());
				Ok(())
			},
			built.root,
			&[
				PrimaryIndexMutation {
					key: b"alpha".to_vec(),
					atom: None,
				},
				PrimaryIndexMutation {
					key: b"middle".to_vec(),
					atom: Some(DiskAtomRef::from_parts(AtomKind::Node, 2)?),
				},
			],
			PageLsn::ZERO,
		)?
		.expect("one root leaf needs no separator update");
		for (page_no, page) in pages {
			built.pages[page_no.get() as usize] = page;
		}
		let scanned = scan_primary_index(
			|page_no, output| {
				output
					.as_bytes_mut()
					.copy_from_slice(built.pages[page_no.get() as usize].as_bytes());
				Ok(())
			},
			built.root,
		)?;
		assert_eq!(scanned.iter().map(|entry| entry.key.as_slice()).collect::<Vec<_>>(), vec![
			b"middle".as_slice(),
			b"zulu".as_slice()
		]);

		let many = (0..2_000_u64)
			.map(|slot| {
				Ok(PrimaryIndexEntry {
					key: format!("record:{slot:08}").into_bytes(),
					atom: DiskAtomRef::from_parts(AtomKind::Node, slot)?,
				})
			})
			.collect::<FerrosGrynnResult<Vec<_>>>()?;
		let built = build_primary_index(&many, PageLsn::ZERO)?;
		let page = &built.pages[1];
		let (count, area_start) = validate_leaf_layout(page)?;
		let (separator, _atom) = leaf_entry(page.as_bytes(), area_start, count, 0)?;
		assert!(
			mutate_primary_index_leaves(
				|page_no, output| {
					output
						.as_bytes_mut()
						.copy_from_slice(built.pages[page_no.get() as usize].as_bytes());
					Ok(())
				},
				built.root,
				&[PrimaryIndexMutation {
					key: separator.to_vec(),
					atom: None,
				}],
				PageLsn::ZERO,
			)?
			.is_none(),
			"a changed internal separator must use the bulk fallback"
		);
		Ok(())
	}

	#[test]
	#[ignore = "paired profiling diagnostic for the stage-9 Q-05 decision"]
	fn primary_incremental_vs_bulk_diagnostic() -> FerrosGrynnResult<()> {
		const ENTRY_COUNT: u64 = 50_000;
		const RUNS: usize = 7;
		let entries = (0..ENTRY_COUNT)
			.map(|ordinal| {
				Ok(PrimaryIndexEntry {
					key: format!("person:{:012}", ordinal * 2).into_bytes(),
					atom: DiskAtomRef::from_parts(AtomKind::Node, ordinal)?,
				})
			})
			.collect::<FerrosGrynnResult<Vec<_>>>()?;

		for fill_percent in [100_usize, 90, 80] {
			let built = build_primary_index_with_fill(&entries, PageLsn::ZERO, fill_percent)?;
			for batch in [1_usize, 32, 256] {
				let mutations = (0..batch)
					.map(|index| {
						let ordinal = (index as u64 * 7_919 + 123) % ENTRY_COUNT;
						Ok(PrimaryIndexMutation {
							key: format!("person:{:012}", ordinal * 2 + 1).into_bytes(),
							atom: Some(DiskAtomRef::from_parts(
								AtomKind::Node,
								ENTRY_COUNT + index as u64,
							)?),
						})
					})
					.collect::<FerrosGrynnResult<Vec<_>>>()?;
				let (fast_path, incremental_pages) =
					primary_benchmark_update(&built, &mutations, fill_percent, true)?;
				let (_bulk_path, bulk_pages) =
					primary_benchmark_update(&built, &mutations, fill_percent, false)?;
				let (incremental, bulk) =
					primary_benchmark_samples(&built, &mutations, fill_percent, RUNS)?;
				println!(
					"q05_primary,fill={fill_percent},batch={batch},base_pages={},\
					 fast_path={fast_path},incremental_pages={incremental_pages},\
					 bulk_pages={bulk_pages},incremental_us={:.3},bulk_us={:.3},ratio={:.4}",
					built.pages.len(),
					incremental.as_secs_f64() * 1_000_000.0,
					bulk.as_secs_f64() * 1_000_000.0,
					incremental.as_secs_f64() / bulk.as_secs_f64(),
				);
			}
		}

		let scalar_entries = (0..ENTRY_COUNT)
			.map(|ordinal| {
				Ok(ScalarIndexEntry {
					key: format!("{:012}", ordinal * 2).into_bytes(),
					atoms: vec![DiskAtomRef::from_parts(AtomKind::Node, ordinal)?],
				})
			})
			.collect::<FerrosGrynnResult<Vec<_>>>()?;
		for fill_percent in [100_usize, 90, 80] {
			let built = build_standard_scalar_index_with_fill(
				&scalar_entries,
				PageLsn::ZERO,
				fill_percent,
			)?;
			for batch in [1_usize, 32, 256] {
				let mut mutations = Vec::with_capacity(batch * 2);
				for index in 0..batch {
					let ordinal = (index as u64 * 7_919 + 123) % ENTRY_COUNT;
					let atom = DiskAtomRef::from_parts(AtomKind::Node, ordinal)?;
					mutations.push(ScalarIndexMutation {
						key: format!("{:012}", ordinal * 2).into_bytes(),
						atom,
						insert: false,
					});
					mutations.push(ScalarIndexMutation {
						key: format!("{:012}", ordinal * 2 + 1).into_bytes(),
						atom,
						insert: true,
					});
				}
				let (fast_path, incremental_pages) =
					scalar_benchmark_update(&built, &mutations, fill_percent, true)?;
				let (_bulk_path, bulk_pages) =
					scalar_benchmark_update(&built, &mutations, fill_percent, false)?;
				let (incremental, bulk) = paired_benchmark_samples(
					RUNS,
					|| scalar_benchmark_update(&built, &mutations, fill_percent, true),
					|| scalar_benchmark_update(&built, &mutations, fill_percent, false),
				)?;
				println!(
					"q05_standard,fill={fill_percent},batch={batch},base_pages={},\
					 fast_path={fast_path},incremental_pages={incremental_pages},\
					 bulk_pages={bulk_pages},incremental_us={:.3},bulk_us={:.3},ratio={:.4}",
					built.pages.len(),
					incremental.as_secs_f64() * 1_000_000.0,
					bulk.as_secs_f64() * 1_000_000.0,
					incremental.as_secs_f64() / bulk.as_secs_f64(),
				);
			}
		}
		Ok(())
	}

	fn primary_benchmark_samples(
		built: &BuiltPrimaryIndex,
		mutations: &[PrimaryIndexMutation],
		fill_percent: usize,
		runs: usize,
	) -> FerrosGrynnResult<(Duration, Duration)> {
		paired_benchmark_samples(
			runs,
			|| primary_benchmark_update(built, mutations, fill_percent, true),
			|| primary_benchmark_update(built, mutations, fill_percent, false),
		)
	}

	fn paired_benchmark_samples<Incremental, Bulk, IncrementalOutput, BulkOutput>(
		runs: usize,
		mut incremental_run: Incremental,
		mut bulk_run: Bulk,
	) -> FerrosGrynnResult<(Duration, Duration)>
	where
		Incremental: FnMut() -> FerrosGrynnResult<IncrementalOutput>,
		Bulk: FnMut() -> FerrosGrynnResult<BulkOutput>,
	{
		let mut incremental = Vec::with_capacity(runs);
		let mut bulk = Vec::with_capacity(runs);
		for run in 0..runs {
			for use_incremental in [run % 2 == 0, run % 2 != 0] {
				let started = Instant::now();
				if use_incremental {
					black_box(incremental_run()?);
					incremental.push(started.elapsed());
				} else {
					black_box(bulk_run()?);
					bulk.push(started.elapsed());
				}
			}
		}
		incremental.sort_unstable();
		bulk.sort_unstable();
		Ok((incremental[incremental.len() / 2], bulk[bulk.len() / 2]))
	}

	fn primary_benchmark_update(
		built: &BuiltPrimaryIndex,
		mutations: &[PrimaryIndexMutation],
		fill_percent: usize,
		incremental: bool,
	) -> FerrosGrynnResult<(bool, usize)> {
		let read_page = |page_no: PageNo, output: &mut AlignedPage| {
			let page = built
				.pages
				.get(page_no.get() as usize)
				.ok_or_else(|| invalid_index("benchmark page missing"))?;
			output.as_bytes_mut().copy_from_slice(page.as_bytes());
			Ok(())
		};
		if incremental
			&& let Some(pages) =
				mutate_primary_index_leaves(read_page, built.root, mutations, PageLsn::ZERO)?
		{
			return Ok((true, pages.len()));
		}

		let mut entries = scan_primary_index(read_page, built.root)?;
		for mutation in mutations {
			let atom =
				mutation.atom.ok_or_else(|| invalid_index("benchmark expects insertions"))?;
			let index = entries
				.binary_search_by(|entry| entry.key.cmp(&mutation.key))
				.expect_err("benchmark mutation keys are absent");
			entries.insert(index, PrimaryIndexEntry {
				key: mutation.key.clone(),
				atom,
			});
		}
		let rebuilt = build_primary_index_with_fill(&entries, PageLsn::ZERO, fill_percent)?;
		Ok((false, rebuilt.pages.len()))
	}

	fn scalar_benchmark_update(
		built: &BuiltScalarIndex,
		mutations: &[ScalarIndexMutation],
		fill_percent: usize,
		incremental: bool,
	) -> FerrosGrynnResult<(bool, usize)> {
		let read_page = |page_no: PageNo, output: &mut AlignedPage| {
			let page = built
				.pages
				.get(page_no.get() as usize)
				.ok_or_else(|| invalid_index("scalar benchmark page missing"))?;
			output.as_bytes_mut().copy_from_slice(page.as_bytes());
			Ok(())
		};
		if incremental
			&& let Some(pages) = mutate_scalar_index_leaves(
				read_page,
				built.root,
				ScalarIndexMode::Standard,
				mutations,
				PageLsn::ZERO,
			)? {
			return Ok((true, pages.len()));
		}

		let mut entries = scan_scalar_index(read_page, built.root, ScalarIndexMode::Standard)?;
		apply_scalar_mutations(&mut entries, mutations, ScalarIndexMode::Standard)?;
		let rebuilt = build_standard_scalar_index_with_fill(&entries, PageLsn::ZERO, fill_percent)?;
		Ok((false, rebuilt.pages.len()))
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
	fn scalar_leaf_mutation_allows_atomic_unique_key_swap() -> FerrosGrynnResult<()> {
		let first = DiskAtomRef::from_parts(AtomKind::Node, 1)?;
		let second = DiskAtomRef::from_parts(AtomKind::Node, 2)?;
		let mut built = build_scalar_index(
			&[
				ScalarIndexEntry {
					key: b"alpha".to_vec(),
					atoms: vec![first],
				},
				ScalarIndexEntry {
					key: b"beta".to_vec(),
					atoms: vec![second],
				},
			],
			ScalarIndexMode::Unique,
			PageLsn::ZERO,
		)?;
		let pages = mutate_scalar_index_leaves(
			|page_no, output| {
				output
					.as_bytes_mut()
					.copy_from_slice(built.pages[page_no.get() as usize].as_bytes());
				Ok(())
			},
			built.root,
			ScalarIndexMode::Unique,
			&[
				ScalarIndexMutation {
					key: b"alpha".to_vec(),
					atom: first,
					insert: false,
				},
				ScalarIndexMutation {
					key: b"beta".to_vec(),
					atom: second,
					insert: false,
				},
				ScalarIndexMutation {
					key: b"beta".to_vec(),
					atom: first,
					insert: true,
				},
				ScalarIndexMutation {
					key: b"alpha".to_vec(),
					atom: second,
					insert: true,
				},
			],
			PageLsn::ZERO,
		)?
		.expect("a two-key root leaf swap needs no separator update");
		for (page_no, page) in pages {
			built.pages[page_no.get() as usize] = page;
		}
		assert_eq!(
			lookup_scalar_index(
				|page_no, output| {
					output
						.as_bytes_mut()
						.copy_from_slice(built.pages[page_no.get() as usize].as_bytes());
					Ok(())
				},
				built.root,
				ScalarIndexMode::Unique,
				b"alpha",
			)?,
			vec![second]
		);
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

	#[test]
	fn label_index_round_trips_multiple_pages_and_rejects_wrong_kind() -> FerrosGrynnResult<()> {
		let atoms = (0..4_100_u64)
			.map(|slot| DiskAtomRef::from_parts(AtomKind::Node, slot))
			.collect::<FerrosGrynnResult<Vec<_>>>()?;
		let pages = build_label_index(&atoms, AtomKind::Node, PageLsn::ZERO)?;
		assert!(pages.len() > 1);
		let mut decoded = Vec::new();
		visit_label_index_pages(
			|page_no, output| {
				let page = pages
					.get(page_no.get() as usize)
					.ok_or_else(|| invalid_index("test label-index page missing"))?;
				output.as_bytes_mut().copy_from_slice(page.as_bytes());
				Ok(())
			},
			AtomKind::Node,
			|atom| {
				decoded.push(atom);
				Ok(())
			},
		)?;
		assert_eq!(decoded, atoms);
		assert!(build_label_index(&atoms, AtomKind::Edge, PageLsn::ZERO).is_err());
		Ok(())
	}

	#[test]
	fn count_index_round_trips_and_rejects_trailing_bytes() -> FerrosGrynnResult<()> {
		let value = CountIndexValue {
			table_id: 7,
			atom_count: 42,
			last_updated_lsn: 11,
			created_at_lsn: 3,
		};
		let mut page = encode_count_index(value, PageLsn::ZERO)?;
		assert_eq!(decode_count_index(&page)?, value);
		page.as_bytes_mut()[64] = 1;
		page.seal()?;
		assert!(decode_count_index(&page).is_err());
		Ok(())
	}

	#[test]
	fn count_index_wal_stamp_updates_payload_and_checksum() -> FerrosGrynnResult<()> {
		let mut page = encode_count_index(
			CountIndexValue {
				table_id: 7,
				atom_count: 42,
				last_updated_lsn: 0,
				created_at_lsn: 0,
			},
			PageLsn::ZERO,
		)?;
		stamp_count_index_lsn(&mut page, 19)?;
		assert_eq!(decode_count_index(&page)?.last_updated_lsn, 19);
		assert!(stamp_count_index_lsn(&mut page, 0).is_err());
		Ok(())
	}

	#[test]
	fn external_runs_feed_streaming_standard_packer() -> FerrosGrynnResult<()> {
		let root = std::env::current_dir()
			.map_err(|source| {
				FerrosGrynnError::storage_io_failure("resolve external-packer test directory")
					.with_technical_source(source)
			})?
			.join(format!(
				".ferros-grynn-packer-{}-{}",
				std::process::id(),
				NEXT_SORT_DIRECTORY.fetch_add(1, Ordering::Relaxed)
			));
		fs::create_dir(&root).map_err(|source| {
			FerrosGrynnError::storage_io_failure("create external-packer test directory")
				.with_technical_source(source)
		})?;
		let result = (|| {
			let mut sorter = ExternalIndexSorter::with_budget(&root, 160);
			for slot in (0..4_100_u64).rev() {
				sorter.push(
					format!("key-{:04}", slot % 3).into_bytes(),
					DiskAtomRef::from_parts(AtomKind::Node, slot)?,
				)?;
			}
			let records = sorter.finish()?;
			let built =
				build_scalar_index_sorted(&records, ScalarIndexMode::Standard, PageLsn::ZERO)?;
			let atoms = lookup_scalar_index(
				|page_no, output| {
					let page = built
						.pages
						.get(page_no.get() as usize)
						.ok_or_else(|| invalid_index("test streaming page missing"))?;
					output.as_bytes_mut().copy_from_slice(page.as_bytes());
					Ok(())
				},
				built.root,
				ScalarIndexMode::Standard,
				b"key-0001",
			)?;
			assert_eq!(atoms.len(), 1_367);
			assert!(atoms.windows(2).all(|pair| pair[0] < pair[1]));
			let duplicate_atom = DiskAtomRef::from_parts(AtomKind::Node, 1)?;
			let mut duplicates = ExternalIndexSorter::with_budget(&root, usize::MAX);
			duplicates.push(b"duplicate".to_vec(), duplicate_atom)?;
			duplicates.push(b"duplicate".to_vec(), duplicate_atom)?;
			assert!(
				build_scalar_index_sorted(
					&duplicates.finish()?,
					ScalarIndexMode::Standard,
					PageLsn::ZERO,
				)
				.is_err()
			);
			Ok(())
		})();
		drop(fs::remove_dir_all(&root));
		result
	}
}
