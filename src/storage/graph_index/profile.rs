//! Ignored stage-10 profile comparing retained-page and streamed index output.

use std::{
	error::Error,
	fs::{self, OpenOptions},
	io::{self, Write},
	path::{Path, PathBuf},
	time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use super::{
	ExternalIndexSorter,
	ScalarIndexMode,
	SortedIndexRecords,
	build_scalar_index_sorted,
	scan_scalar_index,
	stream_standard_index,
};
use crate::{
	graph::AtomKind,
	storage::{
		direct::DirectFile,
		page::{AlignedPage, PAGE_SIZE, PageLsn, PageNo},
	},
	values::storage::DiskAtomRef,
};

/// Number of atoms sharing one scalar key in the representative posting
/// workload.
const POSTING_GROUP_SIZE: usize = 16;
/// Paired samples required by the stage-10 performance gate.
const PROFILE_RUNS: usize = 7;

/// Compares the former retained `Vec<AlignedPage>` builder with the production
/// page-at-a-time writer under identical sorted input and Direct-I/O
/// durability.
#[test]
#[ignore = "stage-10 paired builder performance and retained-output diagnostic"]
fn stage10_streaming_index_profile() -> Result<(), Box<dyn Error>> {
	let items = environment_usize("FG_IMPORT_BUILDER_ITEMS", 50_000)?;
	let directory = ProfileDirectory::create()?;
	let records = build_records(directory.path(), items)?;

	let buffered_warmup = directory.path().join("buffered-warmup.fgidx");
	let streaming_warmup = directory.path().join("streaming-warmup.fgidx");
	let buffered = run_buffered(&records, &buffered_warmup)?;
	let streaming = run_streaming(&records, directory.path(), &streaming_warmup)?;
	assert_eq!(
		scan_file(&buffered_warmup, buffered.root)?,
		scan_file(&streaming_warmup, streaming.root)?
	);
	fs::remove_file(buffered_warmup)?;
	fs::remove_file(streaming_warmup)?;

	let mut buffered_samples = Vec::with_capacity(PROFILE_RUNS);
	let mut streaming_samples = Vec::with_capacity(PROFILE_RUNS);
	for run in 0..PROFILE_RUNS {
		let buffered_path = directory.path().join(format!("buffered-{run}.fgidx"));
		let streaming_path = directory.path().join(format!("streaming-{run}.fgidx"));
		let (buffered, streaming) = if run % 2 == 0 {
			(
				run_buffered(&records, &buffered_path)?,
				run_streaming(&records, directory.path(), &streaming_path)?,
			)
		} else {
			let streaming = run_streaming(&records, directory.path(), &streaming_path)?;
			let buffered = run_buffered(&records, &buffered_path)?;
			(buffered, streaming)
		};
		buffered_samples.push(buffered.elapsed);
		streaming_samples.push(streaming.elapsed);
		fs::remove_file(buffered_path)?;
		fs::remove_file(streaming_path)?;
	}

	let buffered_ns = median_nanos(&mut buffered_samples);
	let streaming_ns = median_nanos(&mut streaming_samples);
	let ratio_ppm =
		streaming_ns.saturating_mul(1_000_000).checked_div(buffered_ns.max(1)).unwrap_or(0);
	let retained_output_bytes = u128::from(buffered.page_count)
		.saturating_mul(u128::try_from(PAGE_SIZE).unwrap_or(u128::MAX));
	let row = format!(
		"{},{items},{},{},{buffered_ns},{streaming_ns},{ratio_ppm},{retained_output_bytes}",
		SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis(),
		buffered.page_count,
		streaming.page_count,
	);
	write_csv(&row)?;
	println!("FG_IMPORT_BUILDER,{row}");
	Ok(())
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
