//! Ignored stage-10 profile comparing retained-page and streamed index output.

use std::{
	cmp,
	error::Error,
	fs::{self, OpenOptions},
	hint::black_box,
	io::{self, Write},
	ops::Range,
	path::{Path, PathBuf},
	time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use super::{
	ChildPage,
	ExternalIndexSorter,
	LEAF_OFFSETS_OFFSET,
	PrimaryIndexEntry,
	ScalarIndexMode,
	SortedIndexRecords,
	build_scalar_index_sorted,
	partition_internal_children,
	partition_leaf_entries,
	scan_scalar_index,
	stream_standard_index,
};
use crate::{
	graph::AtomKind,
	storage::{
		direct::DirectFile,
		page::{AlignedPage, PAGE_SIZE, PageLsn, PageNo},
	},
	values::{
		runtime::{PropertyDictionary, Type, Value},
		storage::{DiskAtomRef, EncodeContext, encode_index_key},
	},
};

/// Number of atoms sharing one scalar key in the representative posting
/// workload.
const POSTING_GROUP_SIZE: usize = 16;
/// Paired samples required by the stage-10 performance gate.
const PROFILE_RUNS: usize = 7;
/// Paired samples for the format-freeze key-layout diagnostic.
const KEY_LAYOUT_RUNS: usize = 15;

/// Measures the only compression candidate that can preserve point lookup
/// without a new dependency or whole-page decompression.
#[test]
#[ignore = "stage-19 Q-20 key-layout format-freeze diagnostic"]
fn stage19_index_key_layout_profile() -> Result<(), Box<dyn Error>> {
	let items = environment_usize("FG_Q20_KEYS", 100_000)?;
	for workload in KeyWorkload::ALL {
		let entries = workload.entries(items)?;
		let full = full_key_layout(&entries)?;
		let prefix = prefix_key_layout(&entries)?;
		let mut full_samples = Vec::with_capacity(KEY_LAYOUT_RUNS);
		let mut prefix_samples = Vec::with_capacity(KEY_LAYOUT_RUNS);
		for run in 0..KEY_LAYOUT_RUNS {
			let (full_elapsed, prefix_elapsed) = if run % 2 == 0 {
				(
					measure(|| full_key_layout(black_box(&entries))),
					measure(|| prefix_key_layout(black_box(&entries))),
				)
			} else {
				let prefix = measure(|| prefix_key_layout(black_box(&entries)));
				let full = measure(|| full_key_layout(black_box(&entries)));
				(full, prefix)
			};
			full_samples.push(full_elapsed?);
			prefix_samples.push(prefix_elapsed?);
		}
		let full_ns = median_nanos(&mut full_samples);
		let prefix_ns = median_nanos(&mut prefix_samples);
		let row = format!(
			"{},{},{items},{},{},{},{},{full_ns},{prefix_ns}",
			SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis(),
			workload.name(),
			full.pages,
			prefix.pages,
			full.used_bytes,
			prefix.used_bytes,
		);
		write_q20_csv(&row)?;
		println!("FG_Q20_KEY_LAYOUT,{row}");
	}
	Ok(())
}

/// Returns one timed, fallible layout-planning result.
fn measure(
	operation: impl FnOnce() -> crate::gql_status::FerrosGrynnResult<LayoutSample>,
) -> Result<Duration, Box<dyn Error>> {
	let started = Instant::now();
	black_box(operation()?);
	Ok(started.elapsed())
}

/// Computes the exact page count and occupied bytes of the frozen full-key
/// leaf layout.
fn full_key_layout(
	entries: &[PrimaryIndexEntry],
) -> crate::gql_status::FerrosGrynnResult<LayoutSample> {
	let groups = partition_leaf_entries(entries)?;
	let used_bytes = groups.iter().try_fold(0_usize, |total, group| {
		let keys = group.iter().try_fold(0_usize, |bytes, entry| {
			bytes
				.checked_add(entry.key.len() + size_of::<u64>())
				.ok_or_else(|| super::invalid_index("full-key diagnostic bytes overflow"))
		})?;
		total
			.checked_add(LEAF_OFFSETS_OFFSET + group.len() * size_of::<u16>() + keys)
			.ok_or_else(|| super::invalid_index("full-key diagnostic total overflows"))
	})?;
	let minimum_keys = groups
		.iter()
		.map(|group| group.first().map_or_else(Vec::new, |entry| entry.key.clone()))
		.collect();
	Ok(LayoutSample {
		pages: tree_page_count(minimum_keys)?,
		used_bytes,
	})
}

/// Computes the exact size of a candidate layout that stores one common
/// prefix per leaf and suffixes in the existing offset directory.
fn prefix_key_layout(
	entries: &[PrimaryIndexEntry],
) -> crate::gql_status::FerrosGrynnResult<LayoutSample> {
	let groups = partition_prefix_leaves(entries)?;
	let mut used_bytes = 0_usize;
	let mut minimum_keys = Vec::with_capacity(groups.len());
	for group in &groups {
		let entries = &entries[group.entries.clone()];
		let suffix_bytes = entries.iter().try_fold(0_usize, |total, entry| {
			total
				.checked_add(entry.key.len() - group.prefix_len + size_of::<u64>())
				.ok_or_else(|| super::invalid_index("prefix diagnostic bytes overflow"))
		})?;
		used_bytes = used_bytes
			.checked_add(
				LEAF_OFFSETS_OFFSET
					+ entries.len() * size_of::<u16>()
					+ group.prefix_len
					+ suffix_bytes,
			)
			.ok_or_else(|| super::invalid_index("prefix diagnostic total overflows"))?;
		minimum_keys.push(entries.first().map_or_else(Vec::new, |entry| entry.key.clone()));
	}
	Ok(LayoutSample {
		pages: tree_page_count(minimum_keys)?,
		used_bytes,
	})
}

/// Greedily packs sorted unique entries using the candidate prefix layout.
fn partition_prefix_leaves(
	entries: &[PrimaryIndexEntry],
) -> crate::gql_status::FerrosGrynnResult<Vec<PrefixLeafGroup>> {
	if entries.is_empty() {
		return Ok(vec![PrefixLeafGroup {
			entries: 0..0,
			prefix_len: 0,
		}]);
	}
	let mut groups = Vec::new();
	let mut start = 0;
	while start < entries.len() {
		let mut end = start;
		let mut key_bytes = 0_usize;
		let mut prefix_len = entries[start].key.len();
		while end < entries.len() {
			prefix_len = common_prefix_len(&entries[start].key[..prefix_len], &entries[end].key);
			key_bytes = key_bytes
				.checked_add(entries[end].key.len())
				.ok_or_else(|| super::invalid_index("prefix diagnostic key bytes overflow"))?;
			let count = end - start + 1;
			let suffix_bytes = key_bytes
				.checked_sub(count * prefix_len)
				.ok_or_else(|| super::invalid_index("prefix diagnostic suffix underflow"))?;
			let used = LEAF_OFFSETS_OFFSET
				.checked_add(count * size_of::<u16>())
				.and_then(|bytes| bytes.checked_add(prefix_len))
				.and_then(|bytes| bytes.checked_add(suffix_bytes))
				.and_then(|bytes| bytes.checked_add(count * size_of::<u64>()))
				.ok_or_else(|| super::invalid_index("prefix diagnostic leaf bytes overflow"))?;
			if used > PAGE_SIZE {
				if end == start {
					return Err(super::invalid_index("one prefixed key exceeds leaf capacity"));
				}
				break;
			}
			end += 1;
		}
		let last = end - 1;
		groups.push(PrefixLeafGroup {
			entries: start..end,
			prefix_len: common_prefix_len(&entries[start].key, &entries[last].key),
		});
		start = end;
	}
	Ok(groups)
}

/// Counts unchanged full-key internal levels above candidate leaf groups.
fn tree_page_count(mut minimum_keys: Vec<Vec<u8>>) -> crate::gql_status::FerrosGrynnResult<usize> {
	let mut pages = minimum_keys.len();
	let mut next_page = u64::try_from(pages)
		.map_err(|_error| super::invalid_index("diagnostic page count exceeds u64"))?;
	while minimum_keys.len() > 1 {
		let level = minimum_keys
			.into_iter()
			.map(|min_key| {
				let page_no = PageNo::new(next_page);
				next_page += 1;
				ChildPage {
					page_no,
					min_key,
				}
			})
			.collect::<Vec<_>>();
		let groups = partition_internal_children(&level)?;
		if groups.len() == level.len() {
			return Err(super::invalid_index("diagnostic internal key exceeds page capacity"));
		}
		minimum_keys = groups.iter().map(|group| group[0].min_key.clone()).collect();
		pages += groups.iter().filter(|group| group.len() > 1).count();
	}
	Ok(pages)
}

/// Returns the byte prefix shared by two keys.
fn common_prefix_len(left: &[u8], right: &[u8]) -> usize {
	left.iter().zip(right).take_while(|(left, right)| left == right).count()
}

/// Appends one Q-20 measurement row under the stage-19 artifact directory.
fn write_q20_csv(row: &str) -> Result<(), Box<dyn Error>> {
	let path = std::env::var_os("FG_Q20_OUTPUT")
		.map(PathBuf::from)
		.unwrap_or_else(|| PathBuf::from("target/windows-ga-core-stage19/q20_key_layout.csv"));
	if let Some(parent) = path.parent() {
		fs::create_dir_all(parent)?;
	}
	let missing = !path.exists();
	let mut output = OpenOptions::new().create(true).append(true).open(path)?;
	if missing {
		writeln!(
			output,
			"timestamp_ms,workload,items,full_pages,prefix_pages,full_used_bytes,\
			 prefix_used_bytes,full_pack_ns,prefix_pack_ns"
		)?;
	}
	writeln!(output, "{row}")?;
	Ok(())
}

/// One page-count and occupied-byte result.
#[derive(Debug)]
struct LayoutSample {
	/// Leaf and unchanged internal pages.
	pages: usize,
	/// Bytes occupied inside all leaf pages.
	used_bytes: usize,
}

/// One candidate prefix-compressed leaf.
struct PrefixLeafGroup {
	/// Half-open range of sorted source entries.
	entries: Range<usize>,
	/// Prefix shared by every key in the page.
	prefix_len: usize,
}

/// Representative canonical key distributions used by Q-20.
#[derive(Clone, Copy)]
enum KeyWorkload {
	/// One long common string prefix.
	CommonString,
	/// Two fixed string components followed by a varying integer.
	Composite,
	/// Many shorter page-local prefixes.
	MixedString,
}

impl KeyWorkload {
	/// Stable diagnostic order.
	const ALL: [Self; 3] = [Self::CommonString, Self::Composite, Self::MixedString];

	/// Returns the CSV name.
	const fn name(self) -> &'static str {
		match self {
			Self::CommonString => "common_string",
			Self::Composite => "composite",
			Self::MixedString => "mixed_string",
		}
	}

	/// Builds sorted unique entries through the production order-key codec.
	fn entries(self, count: usize) -> Result<Vec<PrimaryIndexEntry>, Box<dyn Error>> {
		let mut context = EncodeContext::new(PropertyDictionary::new());
		let mut entries = Vec::with_capacity(count);
		for ordinal in 0..count {
			let atom = DiskAtomRef::from_parts(AtomKind::Node, u64::try_from(ordinal)?)?;
			let key = match self {
				Self::CommonString => {
					let value = Value::from(format!("tenant/europe/customer/{ordinal:012}"));
					encode_index_key(&[(&value, &Type::String)], &mut context)?
				}
				Self::Composite => {
					let tenant = Value::from("tenant/europe");
					let region = Value::from("eu-west-1");
					let ordinal = Value::from(i64::try_from(ordinal)?);
					encode_index_key(
						&[
							(&tenant, &Type::String),
							(&region, &Type::String),
							(&ordinal, &Type::Int),
						],
						&mut context,
					)?
				}
				Self::MixedString => {
					let tenant = ordinal % cmp::max(count / 24, 1);
					let value = Value::from(format!("tenant/{tenant:06}/customer/{ordinal:012}"));
					encode_index_key(&[(&value, &Type::String)], &mut context)?
				}
			};
			entries.push(PrimaryIndexEntry {
				key,
				atom,
			});
		}
		entries.sort_unstable_by(|left, right| left.key.cmp(&right.key));
		Ok(entries)
	}
}

/// Compares the former retained `Vec<AlignedPage>` builder with the production
/// page-at-a-time writer under identical sorted input and Direct-I/O
/// durability.
#[test]
#[ignore = "stage-10 paired builder performance and retained-output diagnostic"]
fn stage10_streaming_index_profile() {
	use crate::test_assertions::AssertResult as _;

	let items = environment_usize("FG_IMPORT_BUILDER_ITEMS", 50_000).assert_ok();
	let directory = ProfileDirectory::create().assert_ok();
	let records = build_records(directory.path(), items).assert_ok();

	let buffered_warmup = directory.path().join("buffered-warmup.fgidx");
	let streaming_warmup = directory.path().join("streaming-warmup.fgidx");
	let buffered = run_buffered(&records, &buffered_warmup).assert_ok();
	let streaming = run_streaming(&records, directory.path(), &streaming_warmup).assert_ok();
	assert_eq!(
		scan_file(&buffered_warmup, buffered.root).assert_ok(),
		scan_file(&streaming_warmup, streaming.root).assert_ok()
	);
	fs::remove_file(buffered_warmup).assert_ok();
	fs::remove_file(streaming_warmup).assert_ok();

	let mut buffered_samples = Vec::with_capacity(PROFILE_RUNS);
	let mut streaming_samples = Vec::with_capacity(PROFILE_RUNS);
	for run in 0..PROFILE_RUNS {
		let buffered_path = directory.path().join(format!("buffered-{run}.fgidx"));
		let streaming_path = directory.path().join(format!("streaming-{run}.fgidx"));
		let (buffered, streaming) = if run % 2 == 0 {
			(
				run_buffered(&records, &buffered_path).assert_ok(),
				run_streaming(&records, directory.path(), &streaming_path).assert_ok(),
			)
		} else {
			let streaming = run_streaming(&records, directory.path(), &streaming_path).assert_ok();
			let buffered = run_buffered(&records, &buffered_path).assert_ok();
			(buffered, streaming)
		};
		buffered_samples.push(buffered.elapsed);
		streaming_samples.push(streaming.elapsed);
		fs::remove_file(buffered_path).assert_ok();
		fs::remove_file(streaming_path).assert_ok();
	}

	let buffered_ns = median_nanos(&mut buffered_samples);
	let streaming_ns = median_nanos(&mut streaming_samples);
	let ratio_ppm =
		streaming_ns.saturating_mul(1_000_000).checked_div(buffered_ns.max(1)).unwrap_or(0);
	let retained_output_bytes = u128::from(buffered.page_count)
		.saturating_mul(u128::try_from(PAGE_SIZE).unwrap_or(u128::MAX));
	let row = format!(
		"{},{items},{},{},{buffered_ns},{streaming_ns},{ratio_ppm},{retained_output_bytes}",
		SystemTime::now().duration_since(UNIX_EPOCH).assert_ok().as_millis(),
		buffered.page_count,
		streaming.page_count,
	);
	write_csv(&row).assert_ok();
	println!("FG_IMPORT_BUILDER,{row}");
}

/// Decodes a generated standard index so different valid page layouts are
/// compared by their canonical logical contents.
fn scan_file(
	path: &Path,
	root: PageNo,
) -> Result<Vec<super::ScalarIndexEntry<DiskAtomRef>>, Box<dyn Error>> {
	let file = DirectFile::open_read_only(path)?;
	Ok(scan_scalar_index(
		|page_no, output: &mut AlignedPage| {
			file.read_page(page_no, output)?;
			Ok(())
		},
		root,
		ScalarIndexMode::Standard,
	)?)
}

/// Builds one replayable sorted scalar stream outside the measured output
/// phase.
fn build_records(root: &Path, items: usize) -> Result<SortedIndexRecords, Box<dyn Error>> {
	let mut sorter = ExternalIndexSorter::new(root);
	for ordinal in 0..items {
		let slot = u64::try_from(ordinal)?;
		let group = ordinal / POSTING_GROUP_SIZE;
		sorter.push(
			format!("{group:012}").into_bytes(),
			DiskAtomRef::from_parts(AtomKind::Node, slot)?,
		)?;
	}
	Ok(sorter.finish()?)
}

/// Executes the previous output-retaining packer and writes every completed
/// page.
fn run_buffered(
	records: &SortedIndexRecords,
	path: &Path,
) -> Result<BuilderSample, Box<dyn Error>> {
	let started = Instant::now();
	let built = build_scalar_index_sorted(records, ScalarIndexMode::Standard, PageLsn::ZERO)?;
	let file = DirectFile::create_new(path)?;
	for (index, page) in built.pages.iter().enumerate() {
		file.write_page(PageNo::new(u64::try_from(index)?), page)?;
	}
	file.sync_data()?;
	Ok(BuilderSample {
		elapsed: started.elapsed(),
		root: built.root,
		page_count: u64::try_from(built.pages.len())?,
	})
}

/// Executes the page-at-a-time packer and writes each page without retaining
/// output.
fn run_streaming(
	records: &SortedIndexRecords,
	scratch_root: &Path,
	path: &Path,
) -> Result<BuilderSample, Box<dyn Error>> {
	let started = Instant::now();
	let file = DirectFile::create_new(path)?;
	let built = stream_standard_index::<DiskAtomRef>(
		records,
		scratch_root,
		PageLsn::ZERO,
		|page_no, page| file.write_page(page_no, page),
	)?;
	file.sync_data()?;
	Ok(BuilderSample {
		elapsed: started.elapsed(),
		root: built.root,
		page_count: built.page_count,
	})
}

/// Returns the median sample duration in nanoseconds.
fn median_nanos(samples: &mut [Duration]) -> u128 {
	samples.sort_unstable();
	samples[samples.len() / 2].as_nanos()
}

/// Reads one positive integer override or returns the deterministic default.
fn environment_usize(name: &str, default: usize) -> Result<usize, Box<dyn Error>> {
	match std::env::var(name) {
		Ok(value) => value.parse::<usize>().ok().filter(|value| *value > 0).ok_or_else(|| {
			io::Error::new(io::ErrorKind::InvalidInput, format!("{name} must be positive")).into()
		}),
		Err(std::env::VarError::NotPresent) => Ok(default),
		Err(source) => Err(source.into()),
	}
}

/// Appends one normalized result row below the standard target directory.
fn write_csv(row: &str) -> Result<(), Box<dyn Error>> {
	let path = std::env::var_os("FG_IMPORT_BUILDER_OUTPUT")
		.map(PathBuf::from)
		.unwrap_or_else(|| PathBuf::from("target/import-stage-10/builder_results.csv"));
	if let Some(parent) = path.parent() {
		fs::create_dir_all(parent)?;
	}
	let missing = !path.exists();
	let mut output = OpenOptions::new().create(true).append(true).open(path)?;
	if missing {
		writeln!(
			output,
			"timestamp_ms,items,buffered_pages,streaming_pages,buffered_ns,streaming_ns,\
			 stream_over_buffered_ppm,buffered_retained_output_bytes"
		)?;
	}
	writeln!(output, "{row}")?;
	Ok(())
}

/// One measured builder result with enough metadata for equality checks.
struct BuilderSample {
	/// Wall-clock duration including pack, Direct writes, and sync.
	elapsed: Duration,
	/// B+tree root produced by the packer.
	root: PageNo,
	/// Exact number of emitted pages.
	page_count: u64,
}

/// Unique temporary directory whose contents are unreachable production data.
struct ProfileDirectory {
	/// Owned path removed after the ignored diagnostic exits.
	path: PathBuf,
}

impl ProfileDirectory {
	/// Creates a collision-free profile directory in the operating-system temp
	/// root.
	fn create() -> Result<Self, Box<dyn Error>> {
		let path =
			std::env::temp_dir().join(format!("fg-stage10-index-profile-{}", uuid::Uuid::now_v7()));
		fs::create_dir(&path)?;
		Ok(Self {
			path,
		})
	}

	/// Returns the owned scratch/output root.
	fn path(&self) -> &Path {
		&self.path
	}
}

impl Drop for ProfileDirectory {
	/// Removes only the uniquely owned unreachable profile directory.
	fn drop(&mut self) {
		drop(fs::remove_dir_all(&self.path));
	}
}
