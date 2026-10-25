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
	datastore::GlobalAtomId,
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	graph::AtomKind,
	values::storage::DiskAtomRef,
};

/// Raw-address contract shared by the global primary and graph-local indexes.
pub(in crate::storage) trait PrimaryAddress: Copy + Eq {
	/// Creates from raw.
	fn from_raw(raw: u64) -> FerrosGrynnResult<Self>;
	/// Returns the raw result.
	fn raw(self) -> u64;
	/// Returns the kind result.
	fn kind(self) -> Option<AtomKind>;
	/// Returns the order key result.
	fn order_key(self) -> u64;
}

impl PrimaryAddress for DiskAtomRef {
	/// Constructs from raw for `DiskAtomRef`.
	fn from_raw(raw: u64) -> FerrosGrynnResult<Self> {
		DiskAtomRef::from_raw(raw)
	}

	/// Returns raw for `DiskAtomRef`.
	fn raw(self) -> u64 {
		self.raw()
	}

	/// Returns kind for `DiskAtomRef`.
	fn kind(self) -> Option<AtomKind> {
		Some(self.kind())
	}

	/// Returns order key for `DiskAtomRef`.
	fn order_key(self) -> u64 {
		self.slot()
	}
}

impl PrimaryAddress for GlobalAtomId {
	/// Constructs from raw for `GlobalAtomId`.
	fn from_raw(raw: u64) -> FerrosGrynnResult<Self> {
		GlobalAtomId::from_storage(raw)
	}

	/// Returns raw for `GlobalAtomId`.
	fn raw(self) -> u64 {
		self.raw()
	}

	/// Returns kind for `GlobalAtomId`.
	fn kind(self) -> Option<AtomKind> {
		self.kind()
	}

	/// Returns order key for `GlobalAtomId`.
	fn order_key(self) -> u64 {
		self.raw()
	}
}

/// Shared B+tree page construction and lookup primitives.
mod btree_idx;
/// Streaming bottom-up packing from externally sorted records.
#[cfg(test)]
mod bulk_build;
/// Single-page table count indexes.
mod count_idx;
/// Bounded-memory Direct-I/O run sorting for generation builds.
mod external_sort;
/// Label and time-zone dictionary pages.
mod label_dict;
/// Per-label sorted atom arrays.
mod label_idx;
/// Shared bounded writer for sorted label-membership streams.
mod label_stream;
/// Primary RecordId index.
mod primary_idx;
#[cfg(test)]
mod profile;
/// Standard and unique scalar indexes.
mod scalar_idx;
/// Page-at-a-time immutable index writers used by atomic import.
mod streaming;

#[allow(
	clippy::wildcard_imports,
	reason = "specialized indexes share one private B+tree codec"
)]
use btree_idx::*;
#[cfg(test)]
pub(super) use bulk_build::build_scalar_index_sorted;
pub(super) use count_idx::{
	CountIndexScope,
	CountIndexValue,
	decode_count_index,
	encode_count_index,
	rewrite_count_index_population,
	stamp_count_index_lsn,
};
pub use external_sort::{
	ExternalIndexSorter,
	SortedIndexRecords,
	check_external_sort_cancelled,
	external_sort_metrics,
	external_sort_run_budget,
	with_external_sort_control,
	with_external_sort_options,
};
pub(super) use external_sort::{
	INDEX_SORT_FAN_IN,
	INDEX_SORT_METADATA_BYTES,
	cleanup_external_index_sort,
	with_external_sort_budget,
};
pub(super) use label_dict::{
	LabelDictionary,
	LabelDictionaryLayout,
	LabelKind,
	decode_label_dictionary,
	encode_label_dictionary,
};
pub(super) use label_idx::{build_label_index, visit_label_index_pages};
pub(super) use label_stream::{LabelIndexWriter, stream_label_index};
#[cfg(test)]
use primary_idx::build_primary_index_with_fill;
pub(super) use primary_idx::{
	BuiltPrimaryIndex,
	PrimaryIndexEntry,
	PrimaryIndexMutation,
	build_primary_index,
	leftmost_primary_leaf,
	lookup_primary_index,
	mutate_primary_index_leaves,
	scan_primary_index,
	visit_primary_index,
	visit_primary_leaf_page,
};
#[cfg(test)]
use scalar_idx::build_standard_scalar_index_with_fill;
pub(super) use scalar_idx::{
	ScalarIndexEntry,
	ScalarIndexMode,
	ScalarIndexMutation,
	build_scalar_index,
	mutate_scalar_index_leaves,
	scan_scalar_index,
	visit_unique_range,
};

/// Changed index pages remain heap-stable through encoding and WAL staging.
pub(super) type IndexLeafPages = Vec<(PageNo, Box<AlignedPage>)>;
#[cfg(test)]
pub(super) use scalar_idx::{apply_scalar_mutations, lookup_scalar_index, range_scalar_index};
pub(super) use streaming::{
	StreamedIndex,
	stream_standard_index,
	stream_unique_index,
	stream_unique_index_with_layout,
	validate_unique_index_key,
};

/// Byte offset of leaf next in its encoded layout.
const LEAF_NEXT_OFFSET: usize = PAGE_HEADER_SIZE;
/// Byte offset of leaf count in its encoded layout.
const LEAF_COUNT_OFFSET: usize = 40;
/// Byte offset of leaf reserved in its encoded layout.
const LEAF_RESERVED_OFFSET: usize = 42;
/// Byte offset of leaf offsets in its encoded layout.
const LEAF_OFFSETS_OFFSET: usize = 44;
/// Byte offset of internal count in its encoded layout.
const INTERNAL_COUNT_OFFSET: usize = PAGE_HEADER_SIZE;
/// Byte offset of internal key bytes in its encoded layout.
const INTERNAL_KEY_BYTES_OFFSET: usize = 34;
/// Byte offset of internal offsets in its encoded layout.
const INTERNAL_OFFSETS_OFFSET: usize = 36;
/// Configured max btree depth value.
const MAX_BTREE_DEPTH: usize = 64;
/// Configured disk atom ref bytes value.
const DISK_ATOM_REF_BYTES: usize = size_of::<u64>();
/// Configured standard entry trailer bytes value.
const STANDARD_ENTRY_TRAILER_BYTES: usize = 16;
/// Configured standard inline atoms value.
const STANDARD_INLINE_ATOMS: usize = 8;
/// Byte offset of posting next in its encoded layout.
const POSTING_NEXT_OFFSET: usize = PAGE_HEADER_SIZE;
/// Byte offset of posting count in its encoded layout.
const POSTING_COUNT_OFFSET: usize = 40;
/// Byte offset of posting reserved in its encoded layout.
const POSTING_RESERVED_OFFSET: usize = 44;
/// Byte offset of posting atoms in its encoded layout.
const POSTING_ATOMS_OFFSET: usize = 48;
/// Configured posting atoms per page value.
const POSTING_ATOMS_PER_PAGE: usize = (PAGE_SIZE - POSTING_ATOMS_OFFSET) / DISK_ATOM_REF_BYTES;
/// Configured mutable primary leaf bytes value.
const MUTABLE_PRIMARY_LEAF_BYTES: usize = PAGE_SIZE * 90 / 100;
/// Configured mutable standard leaf bytes value.
const MUTABLE_STANDARD_LEAF_BYTES: usize = PAGE_SIZE * 80 / 100;

/// Resource and malformed-input regressions before scalar production promotion.
#[cfg(test)]
mod scalar_mutation_tests;

/// Malformed-input regressions before primary production promotion.
#[cfg(test)]
mod primary_mutation_tests;

#[cfg(test)]
/// Regression tests for graph index behavior.
mod tests {
	use std::{
		fs,
		hint::black_box,
		sync::atomic::{AtomicU64, Ordering},
		time::{Duration, Instant},
	};

	use super::{primary_idx::BuiltPrimaryIndex, scalar_idx::BuiltScalarIndex, *};
	use crate::{
		test_assertions::{AssertOption as _, AssertResult as _},
		values::runtime::graph::AtomKind,
	};

	/// Unique external-packer fixture suffix.
	static NEXT_SORT_DIRECTORY: AtomicU64 = AtomicU64::new(0);

	/// Verifies label dictionary round trips multiple sections and pages.
	#[test]
	fn label_dictionary_round_trips_multiple_sections_and_pages() {
		use crate::test_assertions::AssertResult as _;

		let dictionary = LabelDictionary {
			node_labels: (0..1_200).map(|id| format!("node-{id:04}")).collect(),
			edge_labels: vec!["knows".to_owned()],
		};
		let pages = encode_label_dictionary(&dictionary, PageLsn::ZERO).assert_ok();
		let decoded = decode_label_dictionary(|page_no, output| {
			let page = pages
				.get(page_no.get() as usize)
				.ok_or_else(|| invalid_index("test page missing"))?;
			output.as_bytes_mut().copy_from_slice(page.as_bytes());
			Ok(())
		})
		.assert_ok();
		assert_eq!(decoded, dictionary);
	}

	/// Verifies bulk primary tree supports multiple levels and exact lookup.
	#[test]
	fn bulk_primary_tree_supports_multiple_levels_and_exact_lookup() {
		use crate::test_assertions::AssertResult as _;

		let entries = (0..4_000_u64)
			.map(|id| {
				Ok(PrimaryIndexEntry {
					key: format!("person:{id:08}").into_bytes(),
					atom: DiskAtomRef::from_parts(AtomKind::Node, id)?,
				})
			})
			.collect::<FerrosGrynnResult<Vec<_>>>()
			.assert_ok();
		let built = build_primary_index(&entries, PageLsn::ZERO).assert_ok();
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
			lookup_primary_index(read, built.root, b"person:00003999")
				.assert_ok()
				.map(DiskAtomRef::slot),
			Some(3_999)
		);
		assert_eq!(
			lookup_primary_index::<DiskAtomRef>(read, built.root, b"person:missing").assert_ok(),
			None
		);
		let scanned: Vec<PrimaryIndexEntry<DiskAtomRef>> = scan_primary_index(
			|page_no, output| {
				let page = built
					.pages
					.get(page_no.get() as usize)
					.ok_or_else(|| invalid_index("test page missing"))?;
				output.as_bytes_mut().copy_from_slice(page.as_bytes());
				Ok(())
			},
			built.root,
		)
		.assert_ok();
		assert_eq!(scanned, entries);
	}

	/// Verifies global primary preserves atom generation.
	#[test]
	fn global_primary_preserves_atom_generation() {
		use crate::test_assertions::AssertResult as _;

		let atom = GlobalAtomId::from_storage((37_u64 << 41) | (1_u64 << 40) | 0x13).assert_ok();
		let entries = [PrimaryIndexEntry {
			key: b"relationship:knows".to_vec(),
			atom,
		}];
		let built = build_primary_index(&entries, PageLsn::ZERO).assert_ok();
		let decoded = lookup_primary_index(
			|page_no, output| {
				output
					.as_bytes_mut()
					.copy_from_slice(built.pages[page_no.get() as usize].as_bytes());
				Ok(())
			},
			built.root,
			b"relationship:knows",
		)
		.assert_ok();

		assert_eq!(decoded, Some(atom));
		assert_eq!(decoded.map(GlobalAtomId::generation), Some(37));
	}

	/// Verifies primary leaf mutation is atomic and defers separator changes.
	#[test]
	fn primary_leaf_mutation_is_atomic_and_defers_separator_changes() {
		use crate::test_assertions::AssertResult as _;

		let entries = [
			PrimaryIndexEntry {
				key: b"alpha".to_vec(),
				atom: DiskAtomRef::from_parts(AtomKind::Node, 0).assert_ok(),
			},
			PrimaryIndexEntry {
				key: b"zulu".to_vec(),
				atom: DiskAtomRef::from_parts(AtomKind::Node, 1).assert_ok(),
			},
		];
		let mut built = build_primary_index(&entries, PageLsn::ZERO).assert_ok();
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
					atom: entries[0].atom,
					insert: false,
				},
				PrimaryIndexMutation {
					key: b"middle".to_vec(),
					atom: DiskAtomRef::from_parts(AtomKind::Node, 2).assert_ok(),
					insert: true,
				},
			],
			PageLsn::ZERO,
		)
		.assert_ok()
		.assert_some_with("one root leaf needs no separator update");
		for (page_no, page) in pages {
			built.pages[page_no.get() as usize].as_bytes_mut().copy_from_slice(page.as_bytes());
		}
		let scanned: Vec<PrimaryIndexEntry<DiskAtomRef>> = scan_primary_index(
			|page_no, output| {
				output
					.as_bytes_mut()
					.copy_from_slice(built.pages[page_no.get() as usize].as_bytes());
				Ok(())
			},
			built.root,
		)
		.assert_ok();
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
			.collect::<FerrosGrynnResult<Vec<_>>>()
			.assert_ok();
		let built = build_primary_index(&many, PageLsn::ZERO).assert_ok();
		let page = &built.pages[1];
		let (count, area_start) = validate_leaf_layout(page).assert_ok();
		let (separator, atom) = leaf_entry(page.as_bytes(), area_start, count, 0).assert_ok();
		assert!(
			mutate_primary_index_leaves(
				|page_no, output| {
					output
						.as_bytes_mut()
						.copy_from_slice(built.pages[page_no.get() as usize].as_bytes());
					Ok(())
				},
				built.root,
				&[PrimaryIndexMutation::<DiskAtomRef> {
					key: separator.to_vec(),
					atom: DiskAtomRef::from_raw(atom).assert_ok(),
					insert: false,
				}],
				PageLsn::ZERO,
			)
			.assert_ok()
			.is_none(),
			"a changed internal separator must use the bulk fallback"
		);
	}

	/// Verifies primary incremental vs bulk diagnostic.
	#[test]
	#[ignore = "paired profiling diagnostic for the stage-9 Q-05 decision"]
	fn primary_incremental_vs_bulk_diagnostic() -> FerrosGrynnResult<()> {
		/// Primary-index entries shared by the incremental and bulk-build
		/// diagnostic.
		const ENTRY_COUNT: u64 = 50_000;
		/// Paired repetitions collected for the primary-index construction
		/// diagnostic.
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
							atom: DiskAtomRef::from_parts(
								AtomKind::Node,
								ENTRY_COUNT + index as u64,
							)?,
							insert: true,
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

	/// Returns primary benchmark samples for this module.
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

	/// Returns paired benchmark samples for this module.
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

	/// Returns primary benchmark update for this module.
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
			if !mutation.insert {
				return Err(invalid_index("benchmark expects insertions"));
			}
			let atom = mutation.atom;
			let index = entries
				.binary_search_by(|entry| entry.key.cmp(&mutation.key))
				.assert_err_with("benchmark mutation keys are absent");
			entries.insert(index, PrimaryIndexEntry {
				key: mutation.key.clone(),
				atom,
			});
		}
		let rebuilt = build_primary_index_with_fill(&entries, PageLsn::ZERO, fill_percent)?;
		Ok((false, rebuilt.pages.len()))
	}

	/// Returns scalar benchmark update for this module.
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

	/// Verifies primary tree rejects duplicate keys.
	#[test]
	fn primary_tree_rejects_duplicate_keys() {
		use crate::test_assertions::AssertResult as _;

		let atom = DiskAtomRef::from_parts(AtomKind::Node, 0).assert_ok();
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
	}

	/// Verifies standard scalar tree round trips postings and bounded ranges.
	#[test]
	fn standard_scalar_tree_round_trips_postings_and_bounded_ranges() {
		use crate::test_assertions::AssertResult as _;

		let repeated = (0..4_100_u64)
			.map(|slot| DiskAtomRef::from_parts(AtomKind::Node, slot))
			.collect::<FerrosGrynnResult<Vec<_>>>()
			.assert_ok();
		let entries = vec![
			ScalarIndexEntry {
				key: b"alpha".to_vec(),
				atoms: vec![DiskAtomRef::from_parts(AtomKind::Edge, 1).assert_ok()],
			},
			ScalarIndexEntry {
				key: b"middle".to_vec(),
				atoms: repeated.clone(),
			},
			ScalarIndexEntry {
				key: b"zulu".to_vec(),
				atoms: vec![DiskAtomRef::from_parts(AtomKind::Edge, 2).assert_ok()],
			},
		];
		let built =
			build_scalar_index(&entries, ScalarIndexMode::Standard, PageLsn::ZERO).assert_ok();
		let read = |page_no: PageNo, output: &mut AlignedPage| {
			let page = built
				.pages
				.get(page_no.get() as usize)
				.ok_or_else(|| invalid_index("test scalar page missing"))?;
			output.as_bytes_mut().copy_from_slice(page.as_bytes());
			Ok(())
		};
		assert_eq!(
			lookup_scalar_index::<DiskAtomRef>(
				read,
				built.root,
				ScalarIndexMode::Standard,
				b"middle",
			)
			.assert_ok(),
			repeated
		);
		let range = range_scalar_index::<DiskAtomRef>(
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
		)
		.assert_ok();
		assert_eq!(range.len(), 4_100);
	}

	/// Verifies unique scalar tree rejects more than one atom per key.
	#[test]
	fn unique_scalar_tree_rejects_more_than_one_atom_per_key() {
		use crate::test_assertions::AssertResult as _;

		let entries = vec![ScalarIndexEntry {
			key: b"duplicate".to_vec(),
			atoms: vec![
				DiskAtomRef::from_parts(AtomKind::Node, 1).assert_ok(),
				DiskAtomRef::from_parts(AtomKind::Node, 2).assert_ok(),
			],
		}];
		assert!(build_scalar_index(&entries, ScalarIndexMode::Unique, PageLsn::ZERO).is_err());
	}

	/// Verifies scalar leaf mutation allows atomic unique key swap.
	#[test]
	fn scalar_leaf_mutation_allows_atomic_unique_key_swap() {
		use crate::test_assertions::AssertResult as _;

		let first = DiskAtomRef::from_parts(AtomKind::Node, 1).assert_ok();
		let second = DiskAtomRef::from_parts(AtomKind::Node, 2).assert_ok();
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
		)
		.assert_ok();
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
		)
		.assert_ok()
		.assert_some_with("a two-key root leaf swap needs no separator update");
		for (page_no, page) in pages {
			built.pages[page_no.get() as usize] = *page;
		}
		assert_eq!(
			lookup_scalar_index::<DiskAtomRef>(
				|page_no, output| {
					output
						.as_bytes_mut()
						.copy_from_slice(built.pages[page_no.get() as usize].as_bytes());
					Ok(())
				},
				built.root,
				ScalarIndexMode::Unique,
				b"alpha",
			)
			.assert_ok(),
			vec![second]
		);
	}

	/// Verifies unique scalar range descends to its lower bound.
	#[test]
	fn unique_scalar_range_descends_to_its_lower_bound() {
		use crate::test_assertions::AssertResult as _;

		let entries = (0..4_000_u64)
			.map(|slot| {
				Ok(ScalarIndexEntry {
					key: format!("{slot:08}").into_bytes(),
					atoms: vec![DiskAtomRef::from_parts(AtomKind::Node, slot)?],
				})
			})
			.collect::<FerrosGrynnResult<Vec<_>>>()
			.assert_ok();
		let built =
			build_scalar_index(&entries, ScalarIndexMode::Unique, PageLsn::ZERO).assert_ok();
		assert!(built.root != PageNo::new(0));
		let mut first_leaf_read = false;
		let atoms = range_scalar_index::<DiskAtomRef>(
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
		)
		.assert_ok();
		assert_eq!(
			atoms.iter().map(|atom| atom.slot()).collect::<Vec<_>>(),
			(3_990..=3_995).collect::<Vec<_>>(),
		);
		assert!(!first_leaf_read);
	}

	/// Codec sizing follows greedy name packing and complete hash-page
	/// boundaries.
	#[test]
	fn label_dictionary_sizing_matches_encoded_pages() {
		use crate::test_assertions::AssertResult as _;

		for count in [0, 1, 611, 612, 613, 1_225] {
			let names = (0..count).map(|id| format!("name-{id:04}")).collect::<Vec<_>>();
			let dictionary = LabelDictionary::from_labels(
				names.iter().map(|name| (LabelKind::Node, name.as_str())),
			)
			.assert_ok();
			let measured = LabelDictionaryLayout::measure(&dictionary).assert_ok();
			let pages = encode_label_dictionary(&dictionary, PageLsn::ZERO).assert_ok();
			assert_eq!(measured.pages, pages.len());
			assert_eq!(pages.capacity(), pages.len());
		}
		let dictionary = LabelDictionary {
			node_labels: (0..3).map(|id| format!("{id}{}", "x".repeat(9_000))).collect(),
			edge_labels: Vec::new(),
		};
		let measured = LabelDictionaryLayout::measure(&dictionary).assert_ok();
		assert_eq!(measured.pages, 5);
		assert_eq!(
			encode_label_dictionary(&dictionary, PageLsn::ZERO).assert_ok().len(),
			measured.pages
		);
	}

	/// Verifies label index round trips multiple pages and rejects wrong kind.
	#[test]
	fn label_index_round_trips_multiple_pages_and_rejects_wrong_kind() {
		use crate::test_assertions::AssertResult as _;

		let atoms = (0..4_100_u64)
			.map(|slot| DiskAtomRef::from_parts(AtomKind::Node, slot))
			.collect::<FerrosGrynnResult<Vec<_>>>()
			.assert_ok();
		let pages = build_label_index(&atoms, AtomKind::Node, PageLsn::ZERO).assert_ok();
		assert!(pages.len() > 1);
		let mut decoded = Vec::<DiskAtomRef>::new();
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
		)
		.assert_ok();
		assert_eq!(decoded, atoms);
		assert!(build_label_index(&atoms, AtomKind::Edge, PageLsn::ZERO).is_err());
	}

	/// Stream packing preserves bulk bytes at empty, full and tail boundaries.
	#[test]
	fn label_stream_writer_matches_bulk_page_boundaries() {
		use crate::test_assertions::AssertResult as _;

		let capacity = label_idx::label_atoms_per_page();
		for count in [0, 1, capacity, capacity + 1, capacity * 2, capacity * 2 + 1] {
			for kind in [AtomKind::Node, AtomKind::Edge] {
				let atoms = (0..count)
					.map(|slot| DiskAtomRef::from_parts(kind, slot as u64))
					.collect::<FerrosGrynnResult<Vec<_>>>()
					.assert_ok();
				let lsn = PageLsn::new(17);
				let expected = build_label_index(&atoms, kind, lsn).assert_ok();
				let mut written = 0;
				let mut writer = label_stream::LabelIndexWriter::new(
					count,
					kind,
					lsn,
					|no, page: &AlignedPage| {
						assert_eq!(no.get() as usize, written);
						assert_eq!(page.as_bytes(), expected[written].as_bytes());
						page.validate(no)?;
						written += 1;
						Ok(())
					},
				);
				for atom in atoms {
					writer.push(atom).assert_ok();
				}
				let built = writer.finish().assert_ok();
				assert_eq!(built.root, PageNo::new(0));
				assert_eq!(built.page_count as usize, expected.len());
				assert_eq!(written, expected.len());
			}
		}
	}

	/// Exact population checks reject short tails within the same page count.
	#[test]
	fn label_stream_writer_rejects_count_order_kind_and_write_failures() {
		use crate::test_assertions::AssertResult as _;

		let first = DiskAtomRef::from_parts(AtomKind::Node, 1).assert_ok();
		let second = DiskAtomRef::from_parts(AtomKind::Node, 2).assert_ok();
		let edge = DiskAtomRef::from_parts(AtomKind::Edge, 3).assert_ok();
		let mut short = label_stream::LabelIndexWriter::new(
			2,
			AtomKind::Node,
			PageLsn::ZERO,
			|_no, _page: &AlignedPage| Ok(()),
		);
		short.push(first).assert_ok();
		assert!(short.finish().is_err());
		let mut excess = label_stream::LabelIndexWriter::new(
			1,
			AtomKind::Node,
			PageLsn::ZERO,
			|_no, _page: &AlignedPage| Ok(()),
		);
		excess.push(first).assert_ok();
		assert!(excess.push(second).is_err());
		for (left, right) in [(first, first), (second, first), (first, edge)] {
			let mut invalid = label_stream::LabelIndexWriter::new(
				2,
				AtomKind::Node,
				PageLsn::ZERO,
				|_no, _page: &AlignedPage| Ok(()),
			);
			invalid.push(left).assert_ok();
			assert!(invalid.push(right).is_err());
		}
		let failing = label_stream::LabelIndexWriter::<DiskAtomRef, _>::new(
			0,
			AtomKind::Node,
			PageLsn::ZERO,
			|_no, _page: &AlignedPage| Err(invalid_index("injected label writer failure")),
		);
		assert!(failing.finish().is_err());
	}

	/// Verifies count index round trips and rejects trailing bytes.
	#[test]
	fn count_index_round_trips_and_rejects_trailing_bytes() {
		use crate::test_assertions::AssertResult as _;

		let value = CountIndexValue {
			class_id: 7,
			scope: CountIndexScope::Base,
			atom_count: 42,
			last_updated_lsn: 11,
			created_at_lsn: 3,
		};
		let mut page = encode_count_index(value, PageLsn::ZERO).assert_ok();
		assert_eq!(decode_count_index(&page).assert_ok(), value);
		page.as_bytes_mut()[72] = 1;
		page.seal().assert_ok();
		assert!(decode_count_index(&page).is_err());
	}

	/// Population rewrites preserve WAL clocks and reject foreign owners.
	#[test]
	fn count_index_population_rewrite_preserves_clocks_and_checks_owner() {
		use crate::test_assertions::AssertResult as _;
		let value = CountIndexValue {
			class_id: 7,
			scope: CountIndexScope::Base,
			atom_count: 42,
			last_updated_lsn: 11,
			created_at_lsn: 3,
		};
		let mut page = Box::new(encode_count_index(value, PageLsn::new(11)).assert_ok());
		rewrite_count_index_population(&mut page, 7, CountIndexScope::Base, 0).assert_ok();
		assert_eq!(decode_count_index(&page).assert_ok(), CountIndexValue {
			atom_count: 0,
			..value
		});
		let saved = page.as_bytes().to_vec();
		assert!(rewrite_count_index_population(&mut page, 8, CountIndexScope::Base, 2).is_err());
		assert!(
			rewrite_count_index_population(
				&mut page,
				7,
				CountIndexScope::Effective(crate::graph::GraphId::new(1).assert_some()),
				2
			)
			.is_err()
		);
		assert_eq!(page.as_bytes().as_slice(), saved);
	}

	/// Verifies count index wal stamp updates payload and checksum.
	#[test]
	fn count_index_wal_stamp_updates_payload_and_checksum() {
		use crate::test_assertions::AssertResult as _;

		let mut page = encode_count_index(
			CountIndexValue {
				class_id: 7,
				scope: CountIndexScope::Base,
				atom_count: 42,
				last_updated_lsn: 0,
				created_at_lsn: 0,
			},
			PageLsn::ZERO,
		)
		.assert_ok();
		stamp_count_index_lsn(&mut page, 19).assert_ok();
		assert_eq!(decode_count_index(&page).assert_ok().last_updated_lsn, 19);
		assert!(stamp_count_index_lsn(&mut page, 0).is_err());
	}

	/// Verifies external runs feed streaming standard packer.
	#[test]
	fn external_runs_feed_streaming_standard_packer() {
		use crate::test_assertions::AssertResult as _;

		let root = std::env::current_dir()
			.map_err(|source| {
				FerrosGrynnError::storage_io_failure("resolve external-packer test directory")
					.with_technical_source(source)
			})
			.assert_ok()
			.join(format!(
				".ferros-grynn-packer-{}-{}",
				std::process::id(),
				NEXT_SORT_DIRECTORY.fetch_add(1, Ordering::Relaxed)
			));
		fs::create_dir(&root)
			.map_err(|source| {
				FerrosGrynnError::storage_io_failure("create external-packer test directory")
					.with_technical_source(source)
			})
			.assert_ok();
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
			let atoms = lookup_scalar_index::<DiskAtomRef>(
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
		let test_result: FerrosGrynnResult<()> = result;
		test_result.assert_ok();
	}

	/// Verifies page at a time standard writer streams postings and internal
	/// levels.
	#[test]
	fn page_at_a_time_standard_writer_streams_postings_and_internal_levels() {
		use crate::test_assertions::AssertResult as _;

		let root = std::env::current_dir()
			.map_err(|source| {
				FerrosGrynnError::storage_io_failure("resolve streamed-index test directory")
					.with_technical_source(source)
			})
			.assert_ok()
			.join(format!(
				".ferros-grynn-streamed-index-{}-{}",
				std::process::id(),
				NEXT_SORT_DIRECTORY.fetch_add(1, Ordering::Relaxed)
			));
		fs::create_dir(&root)
			.map_err(|source| {
				FerrosGrynnError::storage_io_failure("create streamed-index test directory")
					.with_technical_source(source)
			})
			.assert_ok();
		let result = (|| {
			let mut sorter = ExternalIndexSorter::with_budget(&root, 160);
			for slot in (0..6_000_u64).rev() {
				sorter.push(
					format!("key-{:04}", slot % 257).into_bytes(),
					DiskAtomRef::from_parts(AtomKind::Node, slot)?,
				)?;
			}
			let records = sorter.finish()?;
			let mut pages = Vec::new();
			let built = stream_standard_index::<DiskAtomRef>(
				&records,
				&root,
				PageLsn::ZERO,
				|page_no, source| {
					if page_no.get() as usize != pages.len() {
						return Err(invalid_index("streamed pages are not sequential"));
					}
					let mut page = AlignedPage::zeroed();
					page.as_bytes_mut().copy_from_slice(source.as_bytes());
					pages.push(page);
					Ok(())
				},
			)?;
			assert_eq!(built.page_count as usize, pages.len());
			let atoms = lookup_scalar_index::<DiskAtomRef>(
				|page_no, output| {
					output.as_bytes_mut().copy_from_slice(pages[page_no.get() as usize].as_bytes());
					Ok(())
				},
				built.root,
				ScalarIndexMode::Standard,
				b"key-0001",
			)?;
			assert_eq!(atoms.len(), 24);
			assert!(atoms.windows(2).all(|pair| pair[0] < pair[1]));
			Ok(())
		})();
		drop(fs::remove_dir_all(&root));
		let test_result: FerrosGrynnResult<()> = result;
		test_result.assert_ok();
	}

	/// Verifies page at a time unique and label writers round trip.
	#[test]
	fn page_at_a_time_unique_and_label_writers_round_trip() {
		use crate::test_assertions::AssertResult as _;

		let root = std::env::current_dir()
			.map_err(|source| {
				FerrosGrynnError::storage_io_failure("resolve streamed-index test directory")
					.with_technical_source(source)
			})
			.assert_ok()
			.join(format!(
				".ferros-grynn-streamed-index-{}-{}",
				std::process::id(),
				NEXT_SORT_DIRECTORY.fetch_add(1, Ordering::Relaxed)
			));
		fs::create_dir(&root)
			.map_err(|source| {
				FerrosGrynnError::storage_io_failure("create streamed-index test directory")
					.with_technical_source(source)
			})
			.assert_ok();
		let result = (|| {
			let mut unique = ExternalIndexSorter::with_budget(&root, 320);
			for slot in (0..2_000_u64).rev() {
				unique.push(
					format!("key-{slot:04}").into_bytes(),
					DiskAtomRef::from_parts(AtomKind::Node, slot)?,
				)?;
			}
			let records = unique.finish()?;
			let mut pages = Vec::new();
			let built = stream_unique_index::<DiskAtomRef>(
				&records,
				&root,
				PageLsn::ZERO,
				|page_no, source| {
					assert_eq!(page_no.get() as usize, pages.len());
					let mut page = AlignedPage::zeroed();
					page.as_bytes_mut().copy_from_slice(source.as_bytes());
					pages.push(page);
					Ok(())
				},
			)?;
			assert_eq!(built.page_count as usize, pages.len());
			assert_eq!(
				lookup_scalar_index::<DiskAtomRef>(
					|page_no, output| {
						output
							.as_bytes_mut()
							.copy_from_slice(pages[page_no.get() as usize].as_bytes());
						Ok(())
					},
					built.root,
					ScalarIndexMode::Unique,
					b"key-0042",
				)?,
				vec![DiskAtomRef::from_parts(AtomKind::Node, 42)?]
			);

			let mut labels = ExternalIndexSorter::with_budget(&root, 320);
			for slot in (0..5_000_u64).rev() {
				labels.push(
					slot.to_be_bytes().to_vec(),
					DiskAtomRef::from_parts(AtomKind::Node, slot)?,
				)?;
			}
			let records = labels.finish()?;
			let mut label_pages = Vec::new();
			let built = stream_label_index::<DiskAtomRef>(
				&records,
				AtomKind::Node,
				PageLsn::ZERO,
				|page_no, source| {
					assert_eq!(page_no.get() as usize, label_pages.len());
					let mut page = AlignedPage::zeroed();
					page.as_bytes_mut().copy_from_slice(source.as_bytes());
					label_pages.push(page);
					Ok(())
				},
			)?;
			assert_eq!(built.page_count as usize, label_pages.len());
			let mut decoded = Vec::new();
			visit_label_index_pages::<DiskAtomRef>(
				|page_no, output| {
					output
						.as_bytes_mut()
						.copy_from_slice(label_pages[page_no.get() as usize].as_bytes());
					Ok(())
				},
				AtomKind::Node,
				|atom| {
					decoded.push(atom);
					Ok(())
				},
			)?;
			assert_eq!(decoded.len(), 5_000);
			assert!(decoded.windows(2).all(|pair| pair[0] < pair[1]));
			Ok(())
		})();
		drop(fs::remove_dir_all(&root));
		let test_result: FerrosGrynnResult<()> = result;
		test_result.assert_ok();
	}
}
