//! Native performance gate for the synchronous page source.
//!
//! Every backend/workload/repetition owns a fresh file written through
//! [`DirectFile`] and closed before measurement. Backend order rotates between
//! repetitions so mmap or buffered reads cannot warm a later Direct-I/O run.

use std::{
	collections::HashMap,
	env,
	error::Error,
	fs::{File, OpenOptions},
	hint::black_box,
	io,
	mem::size_of_val,
	num::NonZeroUsize,
	path::{Path, PathBuf},
	sync::atomic::{AtomicU64, Ordering},
	time::{Duration, Instant},
};

use memmap2::{Mmap, MmapMut, MmapOptions};
use sysinfo::{ProcessesToUpdate, System, get_current_pid};

use super::{
	buffer_pool::{BufferPool, CacheSourceId, PageCacheKey},
	direct::{DirectFile, SYNC_BATCH_BYTES, SYNC_BATCH_PAGES},
	page::{AlignedPage, PAGE_HEADER_SIZE, PAGE_SIZE, PageFlags, PageLsn, PageNo, PageType},
};
use crate::gql_status::{FerrosGrynnError, FerrosGrynnResult};

/// Binary kibibyte used by batch-size candidates.
const KIBIBYTE: usize = 1024;
/// Binary mebibyte used by configuration and reports.
const MEBIBYTE_BYTES: usize = 1024 * KIBIBYTE;
/// Normative minimum dataset size.
const DEFAULT_DATASET_MIB: usize = 1024;
/// Default production pool budget: 4096 pages, or 64 MiB.
const DEFAULT_POOL_PAGES: usize = 4096;
/// Minimum dataset-to-pool ratio.
const DATASET_POOL_MULTIPLIER: usize = 4;
/// Number of independently created files measured for every result.
const DEFAULT_REPETITIONS: usize = 3;
/// Number of complete oversized-working-set passes.
const WORKING_SET_PASSES: usize = 2;
/// Resident-set passes used for stable hit distributions.
const RESIDENT_PASSES: usize = 16;
/// Fraction of the pool retained as the scan hot set.
const HOT_SET_DIVISOR: usize = 4;
/// Number of unique scan extents between rotating hot-set extents.
const SCAN_REVISIT_INTERVAL: usize = 4;
/// Fixed seed for deterministic Fisher-Yates orders.
const RANDOM_SEED: u64 = 0x4647_5354_4f52_4147;
/// Smallest accepted production/raw miss throughput ratio.
const MIN_RAW_DIRECT_RATIO: f64 = 0.90;
/// Largest accepted production/raw miss p99 ratio.
const MAX_RAW_DIRECT_P99_RATIO: f64 = 1.25;
/// Smallest accepted batched production/mmap first-touch ratio.
const MIN_MMAP_BATCH_RATIO: f64 = 0.90;
/// Largest accepted extra resident-hit p99 over mmap.
const MAX_RESIDENT_P99_OVERHEAD: Duration = Duration::from_nanos(500);
/// Smallest accepted hot-set hit ratio during a scan.
const MIN_SCAN_HIT_RATIO: f64 = 0.99;
/// Largest accepted process RSS delta relative to the pool budget.
const MAX_RSS_RATIO: f64 = 1.10;
/// Batch throughput must remain within this fraction of the maximum median.
const MIN_CALIBRATED_THROUGHPUT_RATIO: f64 = 0.95;
/// Batch p99 may not exceed the maximum-throughput candidate by more than 25%.
const MAX_CALIBRATED_P99_RATIO: f64 = 1.25;
/// Calibration uses a bounded dataset while preserving repeated extents.
const CALIBRATION_DATASET_MIB: usize = 256;
/// Candidate extent sizes required by the stage-2 plan.
const BATCH_CANDIDATE_KIB: [usize; 5] = [16, 64, 256, 1024, 2048];
/// Largest calibrated extent, also the minimum calibration dataset.
const MAX_BATCH_BYTES: usize = 2048 * KIBIBYTE;
/// Nearest-rank percentile denominator.
const PERCENTILE_DENOMINATOR: usize = 100;
/// Stable resident-hit workload label.
const RESIDENT_WORKLOAD: &str = "resident hit";
/// Stable sparse-random workload label.
const SPARSE_RANDOM_WORKLOAD: &str = "sparse random QD1";
/// Stable full-random workload label.
const RANDOM_FULL_WORKLOAD: &str = "random full coverage";
/// Stable scalar-scan workload label.
const SEQUENTIAL_SCALAR_WORKLOAD: &str = "sequential scalar QD1";
/// Stable batched-scan workload label.
const SEQUENTIAL_BATCH_WORKLOAD: &str = "sequential batched";
/// Stable oversized-working-set workload label.
const WORKING_SET_WORKLOAD: &str = "working set 4x pool";
/// Stable scan-resistance workload label.
const SCAN_HOT_WORKLOAD: &str = "scan with fixed hot set";
/// Optional non-normative dataset-size override.
const DATASET_MIB_ENV: &str = "FG_STAGE2_BENCH_MIB";
/// Optional non-normative pool-size override.
const POOL_PAGES_ENV: &str = "FG_STAGE2_POOL_PAGES";
/// Optional non-normative repetition-count override.
const REPETITIONS_ENV: &str = "FG_STAGE2_REPETITIONS";
/// Unique suffix source for benchmark files within one process.
static NEXT_BENCH_FILE: AtomicU64 = AtomicU64::new(0);

/// Four independently measured read paths.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum ReadBackend {
	/// Direct transport only, without page validation.
	RawDirect,
	/// Production Direct I/O through BufferPool with full validation.
	PooledDirect,
	/// Ordinary cached positional file reads with full validation.
	Buffered,
	/// Benchmark-only read-only mapping with first-access validation.
	Mmap,
}

impl ReadBackend {
	/// Stable backend order for aggregation and reports.
	const ALL: [Self; 4] = [Self::RawDirect, Self::PooledDirect, Self::Buffered, Self::Mmap];

	/// Returns a stable report and temporary-file label.
	const fn label(self) -> &'static str {
		match self {
			Self::RawDirect => "raw-direct",
			Self::PooledDirect => "direct-pool",
			Self::Buffered => "buffered",
			Self::Mmap => "mmap",
		}
	}
}

/// Rotating orders used by successive repetitions.
const READ_BACKEND_ORDERS: [[ReadBackend; 4]; 3] = [
	[ReadBackend::RawDirect, ReadBackend::PooledDirect, ReadBackend::Buffered, ReadBackend::Mmap],
	[ReadBackend::Mmap, ReadBackend::Buffered, ReadBackend::PooledDirect, ReadBackend::RawDirect],
	[ReadBackend::Buffered, ReadBackend::RawDirect, ReadBackend::Mmap, ReadBackend::PooledDirect],
];

/// Independently measured durable write paths.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum WriteBackend {
	/// Direct page writes followed by one `sync_data`.
	Direct,
	/// Cached positional writes followed by one `sync_data`.
	Buffered,
	/// Mapped copies followed by one synchronous mapping flush.
	Mmap,
}

impl WriteBackend {
	/// Stable write-backend order.
	const ALL: [Self; 3] = [Self::Direct, Self::Buffered, Self::Mmap];

	/// Returns a stable report and temporary-file label.
	const fn label(self) -> &'static str {
		match self {
			Self::Direct => "direct",
			Self::Buffered => "buffered",
			Self::Mmap => "mmap",
		}
	}
}

/// Rotating durable-write orders.
const WRITE_BACKEND_ORDERS: [[WriteBackend; 3]; 3] = [
	[WriteBackend::Direct, WriteBackend::Buffered, WriteBackend::Mmap],
	[WriteBackend::Mmap, WriteBackend::Buffered, WriteBackend::Direct],
	[WriteBackend::Buffered, WriteBackend::Direct, WriteBackend::Mmap],
];

/// Whether useful work is a marker lookup or complete page processing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MetricUnit {
	/// Resident access consumes one marker byte and is reported as operations.
	Marker,
	/// Storage access consumes every byte in each complete page.
	Page,
}

impl MetricUnit {
	/// Returns useful bytes accounted for each logical page.
	const fn bytes_per_page(self) -> u64 {
		match self {
			Self::Marker => 1,
			Self::Page => PAGE_SIZE as u64,
		}
	}
}

/// Explicit benchmark configuration and normative-state marker.
struct BenchmarkConfig {
	/// Exact non-zero pool capacity.
	pool_pages: NonZeroUsize,
	/// Full dataset size in complete pages.
	dataset_pages: u64,
	/// Fresh-file repetitions per measurement.
	repetitions: usize,
	/// Whether no development override weakened the normative profile.
	normative: bool,
}

impl BenchmarkConfig {
	/// Loads optional development overrides while retaining normative defaults.
	fn from_environment() -> io::Result<Self> {
		let pool_override = optional_usize(POOL_PAGES_ENV)?;
		let dataset_override = optional_usize(DATASET_MIB_ENV)?;
		let repetitions_override = optional_usize(REPETITIONS_ENV)?;
		let pool_pages = NonZeroUsize::new(pool_override.unwrap_or(DEFAULT_POOL_PAGES))
			.ok_or_else(|| io::Error::other("benchmark pool capacity must be non-zero"))?;
		let repetitions = repetitions_override.unwrap_or(DEFAULT_REPETITIONS);
		if repetitions == 0 {
			return Err(io::Error::other("benchmark repetition count must be non-zero"));
		}
		let requested_mib = dataset_override.unwrap_or(DEFAULT_DATASET_MIB);
		let requested_bytes = requested_mib
			.checked_mul(MEBIBYTE_BYTES)
			.ok_or_else(|| io::Error::other("benchmark dataset byte count overflow"))?;
		let pool_floor = pool_pages
			.get()
			.checked_mul(PAGE_SIZE)
			.and_then(|bytes| bytes.checked_mul(DATASET_POOL_MULTIPLIER))
			.ok_or_else(|| io::Error::other("pool-derived dataset size overflow"))?;
		let dataset_bytes = requested_bytes.max(pool_floor).max(MAX_BATCH_BYTES);
		if !dataset_bytes.is_multiple_of(PAGE_SIZE) {
			return Err(io::Error::other("benchmark dataset is not page-aligned"));
		}
		let dataset_pages = u64::try_from(dataset_bytes / PAGE_SIZE)
			.map_err(|source| conversion_error("dataset page count does not fit u64", source))?;
		Ok(Self {
			pool_pages,
			dataset_pages,
			repetitions,
			normative: pool_override.is_none()
				&& dataset_override.is_none()
				&& repetitions_override.is_none(),
		})
	}
}

/// One measured result including latency distribution.
#[derive(Debug, Clone, Copy)]
struct Metrics {
	/// Number of timed syscalls, extent accesses, or marker lookups.
	operations: u64,
	/// Actual useful bytes consumed by those operations.
	useful_bytes: u64,
	/// Wall-clock duration for the complete measured region.
	elapsed: Duration,
	/// Nearest-rank median latency.
	p50: Duration,
	/// Nearest-rank 95th-percentile latency.
	p95: Duration,
	/// Nearest-rank 99th-percentile latency.
	p99: Duration,
}

impl Metrics {
	/// Returns useful throughput in MiB/s.
	#[allow(
		clippy::cast_precision_loss,
		reason = "benchmark counters need only human-readable decimal precision"
	)]
	fn throughput_mib_per_second(self) -> f64 {
		self.useful_bytes as f64 / MEBIBYTE_BYTES as f64 / self.elapsed.as_secs_f64()
	}

	/// Returns measured operations per second.
	#[allow(
		clippy::cast_precision_loss,
		reason = "benchmark operation counts need only human-readable decimal precision"
	)]
	fn operations_per_second(self) -> f64 {
		self.operations as f64 / self.elapsed.as_secs_f64()
	}

	/// Returns average nanoseconds per measured operation.
	#[allow(
		clippy::cast_precision_loss,
		reason = "benchmark operation counts need only human-readable decimal precision"
	)]
	fn nanoseconds_per_operation(self) -> f64 {
		self.elapsed.as_nanos() as f64 / self.operations as f64
	}

	/// Returns p99 latency in microseconds.
	fn p99_micros(self) -> f64 {
		self.p99.as_secs_f64() * 1_000_000.0
	}
}

/// One read sample before aggregation.
#[derive(Debug, Clone, Copy)]
struct ReadSample {
	/// Timed distribution.
	metrics: Metrics,
	/// Process RSS growth observed while the backend was live.
	rss_delta: u64,
	/// Hot accesses served by the pool during the measured scan.
	hot_hits: u64,
}

/// Median read result for one backend and workload.
struct BackendResult {
	/// Measured backend.
	backend: ReadBackend,
	/// Median repetition selected by elapsed time.
	metrics: Metrics,
	/// Maximum RSS delta across repetitions.
	rss_delta: u64,
	/// Minimum scan hot-hit ratio across repetitions, when applicable.
	hot_hit_ratio: Option<f64>,
}

/// Complete four-backend result for one access pattern.
struct ReadComparison {
	/// Stable workload label.
	name: &'static str,
	/// Correct accounting and report unit.
	unit: MetricUnit,
	/// Median result for every backend.
	backends: Vec<BackendResult>,
}

impl ReadComparison {
	/// Returns the result for one required backend.
	fn backend(&self, backend: ReadBackend) -> io::Result<&BackendResult> {
		self.backends.iter().find(|result| result.backend == backend).ok_or_else(|| {
			io::Error::other(format!("missing {} benchmark result", backend.label()))
		})
	}
}

/// Input and accounting contract for one read workload.
struct ReadWorkload {
	/// Stable report and gate label.
	name: &'static str,
	/// Dataset length required by the access order.
	page_count: u64,
	/// Untimed accesses that establish the intended resident state.
	preload: Vec<PageNo>,
	/// Timed logical page order.
	order: Vec<PageNo>,
	/// Pages processed by one timed operation.
	extent_pages: NonZeroUsize,
	/// Useful-byte accounting semantics.
	unit: MetricUnit,
	/// Number of explicit hot-set references in `order`.
	hot_accesses: u64,
}

/// One calibrated extent candidate.
struct CalibrationResult {
	/// Candidate extent size in bytes.
	bytes: usize,
	/// Median raw Direct-I/O distribution.
	metrics: Metrics,
}

/// Calibration matrix and chosen candidate.
struct BatchCalibration {
	/// All required candidate results.
	candidates: Vec<CalibrationResult>,
	/// Smallest candidate satisfying throughput and p99 criteria.
	selected_bytes: usize,
}

/// Aggregated durable-write result.
struct WriteResult {
	/// Measured write backend.
	backend: WriteBackend,
	/// Median repetition.
	metrics: Metrics,
}

/// RAII-owned unique benchmark path.
struct BenchPath(PathBuf);

impl BenchPath {
	/// Allocates a collision-resistant path in the workspace.
	fn new(label: &str) -> io::Result<Self> {
		let suffix = NEXT_BENCH_FILE.fetch_add(1, Ordering::Relaxed);
		let name = format!(".ferros-grynn-stage2-{label}-{}-{suffix}.fgb", std::process::id());
		Ok(Self(env::current_dir()?.join(name)))
	}

	/// Returns the owned filesystem path.
	fn as_path(&self) -> &Path {
		&self.0
	}
}

impl Drop for BenchPath {
	/// Removes the benchmark file after every later-declared handle drops.
	fn drop(&mut self) {
		drop(std::fs::remove_file(&self.0));
	}
}

/// Read-only mapping of one immutable benchmark file.
struct MmapPageSource {
	/// Complete immutable mapping.
	mapping: Mmap,
	/// Number of complete pages.
	page_count: u64,
	/// Pages already validated during this mapping lifetime.
	validated: Vec<bool>,
}

impl MmapPageSource {
	/// Maps a completed fresh dataset read-only.
	fn open(path: &Path, page_count: u64) -> io::Result<Self> {
		let file = File::open(path)?;
		let expected_length = checked_byte_length(page_count)?;
		if file.metadata()?.len() != expected_length {
			return Err(io::Error::other("mmap benchmark file has an unexpected length"));
		}
		// SAFETY: this benchmark owns the unique path; the Direct writer has
		// closed, the length is fixed and verified, and no handle mutates or
		// truncates the file during this mapping lifetime.
		let mapping = unsafe { MmapOptions::new().map(&file)? };
		let page_count_usize = usize::try_from(page_count)
			.map_err(|source| conversion_error("mmap page count does not fit usize", source))?;
		Ok(Self {
			mapping,
			page_count,
			validated: vec![false; page_count_usize],
		})
	}

	/// Validates a mapped page once and returns its deterministic marker.
	fn read_marker(&mut self, page_no: PageNo) -> Result<u8, Box<dyn Error>> {
		if page_no.get() >= self.page_count {
			return Err(io::Error::other("mmap page is out of range").into());
		}
		let start = page_byte_offset_usize(page_no)?;
		let end = start
			.checked_add(PAGE_SIZE)
			.ok_or_else(|| io::Error::other("mmap page end overflow"))?;
		let bytes: &[u8; PAGE_SIZE] = self
			.mapping
			.get(start..end)
			.ok_or_else(|| io::Error::other("mmap page is truncated"))?
			.try_into()
			.map_err(|_source| io::Error::other("mmap page length is invalid"))?;
		let index = usize::try_from(page_no.get())
			.map_err(|source| conversion_error("mmap page number does not fit usize", source))?;
		let validated = self
			.validated
			.get_mut(index)
			.ok_or_else(|| io::Error::other("mmap validation index is out of range"))?;
		if !*validated {
			let header = super::page::validate_page_bytes(bytes, page_no)?;
			validate_benchmark_version(page_no, header.page_lsn())?;
			*validated = true;
		}
		Ok(bytes[PAGE_HEADER_SIZE])
	}
}

/// Cached positional reader used as the ordinary-file baseline.
struct BufferedPageSource {
	/// Buffered operating-system file handle.
	file: File,
	/// Number of complete pages.
	page_count: u64,
}

impl BufferedPageSource {
	/// Opens and checks one immutable benchmark dataset.
	fn open(path: &Path, page_count: u64) -> io::Result<Self> {
		let file = File::open(path)?;
		if file.metadata()?.len() != checked_byte_length(page_count)? {
			return Err(io::Error::other("buffered benchmark file has an unexpected length"));
		}
		Ok(Self {
			file,
			page_count,
		})
	}

	/// Reads and validates one contiguous extent into reusable aligned storage.
	fn read_extent(
		&self,
		pages: &[PageNo],
		scratch: &mut [AlignedPage],
	) -> Result<(), Box<dyn Error>> {
		let first = first_contiguous_page(pages, self.page_count)?;
		let output = scratch
			.get_mut(..pages.len())
			.ok_or_else(|| io::Error::other("buffered scratch space is too small"))?;
		buffered_read_exact_at(
			&self.file,
			AlignedPage::slice_as_bytes_mut(output),
			checked_page_offset(first)?,
		)?;
		for (page_no, page) in pages.iter().copied().zip(output) {
			let header = page.validate_mut(page_no)?;
			validate_benchmark_version(page_no, header.page_lsn())?;
			black_box(page.as_bytes()[PAGE_HEADER_SIZE]);
		}
		Ok(())
	}
}

/// Corrected stage-2 gate and report.
#[test]
#[ignore = "creates many fresh large files and must run natively in release mode"]
fn disk_storage_stage2_gate() -> Result<(), Box<dyn Error>> {
	let config = BenchmarkConfig::from_environment()?;
	let workloads = read_workloads(&config)?;
	let mut reads = Vec::with_capacity(workloads.len());
	for (ordinal, workload) in workloads.iter().enumerate() {
		reads.push(run_read_workload(&config, workload, ordinal)?);
	}
	let calibration = calibrate_batch_size(&config)?;
	let writes = run_durable_writes(&config)?;
	print_report(&config, &reads, &calibration, &writes)?;
	apply_gate(&config, &reads, &calibration, &writes)
}

/// Profiles only the comparable sequential batch paths with the normal
/// fresh-file and rotating-order controls.
#[test]
#[ignore = "native release-mode probe for storage batch tuning"]
fn disk_storage_stage2_batch_probe() -> Result<(), Box<dyn Error>> {
	let config = BenchmarkConfig::from_environment()?;
	let extent_pages = NonZeroUsize::new(SYNC_BATCH_PAGES)
		.ok_or_else(|| io::Error::other("configured batch extent must be non-zero"))?;
	let workload = ReadWorkload {
		name: SEQUENTIAL_BATCH_WORKLOAD,
		page_count: config.dataset_pages,
		preload: Vec::new(),
		order: sequential_order(config.dataset_pages)?,
		extent_pages,
		unit: MetricUnit::Page,
		hot_accesses: 0,
	};
	let comparison = run_read_workload(&config, &workload, 0)?;
	let raw = comparison.backend(ReadBackend::RawDirect)?.metrics;
	let pooled = comparison.backend(ReadBackend::PooledDirect)?.metrics;
	println!(
		"stage2_batch_probe dataset_pages={} pool_pages={} repetitions={} raw_mib_s={:.2} \
		 pooled_mib_s={:.2} pooled_raw_ratio={:.4}",
		config.dataset_pages,
		config.pool_pages,
		config.repetitions,
		raw.throughput_mib_per_second(),
		pooled.throughput_mib_per_second(),
		throughput_ratio(pooled, raw),
	);
	Ok(())
}

/// Builds every required deterministic workload.
fn read_workloads(config: &BenchmarkConfig) -> Result<Vec<ReadWorkload>, Box<dyn Error>> {
	let resident_pages = u64::try_from(config.pool_pages.get())
		.map_err(|source| conversion_error("pool page count does not fit u64", source))?;
	let resident_preload = sequential_order(resident_pages)?;
	let resident_order = random_order(resident_pages)?.repeat(RESIDENT_PASSES);
	let random_full = random_order(config.dataset_pages)?;
	let working_pages = config
		.pool_pages
		.get()
		.checked_mul(DATASET_POOL_MULTIPLIER)
		.ok_or_else(|| io::Error::other("working-set page count overflow"))?;
	if working_pages > random_full.len() {
		return Err(io::Error::other("dataset is smaller than the 4x working set").into());
	}
	let working_order = random_full[..working_pages].repeat(WORKING_SET_PASSES);
	let hot_pages = config.pool_pages.get() / HOT_SET_DIVISOR;
	let scan_extent_pages = SYNC_BATCH_PAGES.min(hot_pages);
	let scan_extent = NonZeroUsize::new(scan_extent_pages)
		.ok_or_else(|| io::Error::other("scan hot set must be non-empty"))?;
	let hot_pages = hot_pages / scan_extent_pages * scan_extent_pages;
	let hot_pages = u64::try_from(hot_pages)
		.map_err(|source| conversion_error("hot-set page count does not fit u64", source))?;
	let (scan_order, hot_accesses) = scan_hot_order(config.dataset_pages, hot_pages, scan_extent)?;
	let scalar_extent = NonZeroUsize::MIN;
	let batch_extent = NonZeroUsize::new(SYNC_BATCH_PAGES)
		.ok_or_else(|| io::Error::other("configured batch extent must be non-zero"))?;

	Ok(vec![
		ReadWorkload {
			name: RESIDENT_WORKLOAD,
			page_count: resident_pages,
			preload: resident_preload,
			order: resident_order,
			extent_pages: scalar_extent,
			unit: MetricUnit::Marker,
			hot_accesses: 0,
		},
		ReadWorkload {
			name: SPARSE_RANDOM_WORKLOAD,
			page_count: config.dataset_pages,
			preload: Vec::new(),
			order: sparse_random_order(config.dataset_pages, config.pool_pages)?,
			extent_pages: scalar_extent,
			unit: MetricUnit::Page,
			hot_accesses: 0,
		},
		ReadWorkload {
			name: RANDOM_FULL_WORKLOAD,
			page_count: config.dataset_pages,
			preload: Vec::new(),
			order: random_full,
			extent_pages: scalar_extent,
			unit: MetricUnit::Page,
			hot_accesses: 0,
		},
		ReadWorkload {
			name: SEQUENTIAL_SCALAR_WORKLOAD,
			page_count: config.dataset_pages,
			preload: Vec::new(),
			order: sequential_order(config.dataset_pages)?,
			extent_pages: scalar_extent,
			unit: MetricUnit::Page,
			hot_accesses: 0,
		},
		ReadWorkload {
			name: SEQUENTIAL_BATCH_WORKLOAD,
			page_count: config.dataset_pages,
			preload: Vec::new(),
			order: sequential_order(config.dataset_pages)?,
			extent_pages: batch_extent,
			unit: MetricUnit::Page,
			hot_accesses: 0,
		},
		ReadWorkload {
			name: WORKING_SET_WORKLOAD,
			page_count: config.dataset_pages,
			preload: Vec::new(),
			order: working_order,
			extent_pages: scalar_extent,
			unit: MetricUnit::Page,
			hot_accesses: 0,
		},
		ReadWorkload {
			name: SCAN_HOT_WORKLOAD,
			page_count: config.dataset_pages,
			preload: sequential_order(hot_pages)?.repeat(2),
			order: scan_order,
			extent_pages: scan_extent,
			unit: MetricUnit::Page,
			hot_accesses,
		},
	])
}

/// Measures every backend with a fresh file and rotating order.
fn run_read_workload(
	config: &BenchmarkConfig,
	workload: &ReadWorkload,
	ordinal: usize,
) -> Result<ReadComparison, Box<dyn Error>> {
	let mut samples = HashMap::<ReadBackend, Vec<ReadSample>>::new();
	for repetition in 0..config.repetitions {
		let order = READ_BACKEND_ORDERS[(repetition + ordinal) % READ_BACKEND_ORDERS.len()];
		for backend in order {
			let sample = measure_fresh_read(config, workload, backend)?;
			samples.entry(backend).or_default().push(sample);
		}
	}

	let mut backends = Vec::with_capacity(ReadBackend::ALL.len());
	for backend in ReadBackend::ALL {
		let backend_samples = samples
			.remove(&backend)
			.ok_or_else(|| io::Error::other(format!("missing {} samples", backend.label())))?;
		backends.push(aggregate_read_samples(backend, workload.hot_accesses, backend_samples)?);
	}
	Ok(ReadComparison {
		name: workload.name,
		unit: workload.unit,
		backends,
	})
}

/// Creates and measures one isolated backend file.
fn measure_fresh_read(
	config: &BenchmarkConfig,
	workload: &ReadWorkload,
	backend: ReadBackend,
) -> Result<ReadSample, Box<dyn Error>> {
	let path = BenchPath::new(backend.label())?;
	create_dataset(path.as_path(), workload.page_count)?;
	let extent_pages = workload.extent_pages.get();
	let source_id = CacheSourceId::new(1);

	match backend {
		ReadBackend::RawDirect => {
			let direct = DirectFile::open_read_only(path.as_path())?;
			let mut scratch = aligned_pages(extent_pages);
			consume_raw_direct(&direct, &workload.preload, extent_pages, &mut scratch)?;
			let metrics = measure_order(&workload.order, extent_pages, workload.unit, |pages| {
				read_raw_direct_extent(&direct, pages, &mut scratch)
			})?;
			Ok(ReadSample {
				metrics,
				rss_delta: 0,
				hot_hits: 0,
			})
		}
		ReadBackend::PooledDirect => {
			let direct = DirectFile::open_read_only(path.as_path())?;
			let rss_before = process_rss()?;
			let pool = BufferPool::new(config.pool_pages);
			let mut page_scratch = aligned_pages(extent_pages);
			let mut key_scratch = Vec::with_capacity(extent_pages);
			consume_pool(
				&pool,
				&direct,
				source_id,
				&workload.preload,
				extent_pages,
				&mut page_scratch,
				&mut key_scratch,
			)?;
			let stats_before = pool.stats();
			let metrics = measure_order(&workload.order, extent_pages, workload.unit, |pages| {
				read_pool_extent(
					&pool,
					&direct,
					source_id,
					pages,
					&mut page_scratch,
					&mut key_scratch,
				)
			})?;
			let stats_after = pool.stats();
			if stats_before.page_buffer_allocations() != config.pool_pages.get()
				|| stats_after.page_buffer_allocations() != stats_before.page_buffer_allocations()
			{
				return Err(io::Error::other(
					"page-buffer allocation count changed after pool initialization",
				)
				.into());
			}
			let rss_delta = process_rss()?.saturating_sub(rss_before);
			Ok(ReadSample {
				metrics,
				rss_delta,
				hot_hits: stats_after.hits().saturating_sub(stats_before.hits()),
			})
		}
		ReadBackend::Buffered => {
			let source = BufferedPageSource::open(path.as_path(), workload.page_count)?;
			let mut scratch = aligned_pages(extent_pages);
			consume_buffered(&source, &workload.preload, extent_pages, &mut scratch)?;
			let metrics = measure_order(&workload.order, extent_pages, workload.unit, |pages| {
				source.read_extent(pages, &mut scratch)
			})?;
			Ok(ReadSample {
				metrics,
				rss_delta: 0,
				hot_hits: 0,
			})
		}
		ReadBackend::Mmap => {
			let rss_before = process_rss()?;
			let mut source = MmapPageSource::open(path.as_path(), workload.page_count)?;
			consume_mmap(&mut source, &workload.preload, extent_pages)?;
			let metrics = measure_order(&workload.order, extent_pages, workload.unit, |pages| {
				for page_no in pages.iter().copied() {
					black_box(source.read_marker(page_no)?);
				}
				Ok(())
			})?;
			let rss_delta = process_rss()?.saturating_sub(rss_before);
			Ok(ReadSample {
				metrics,
				rss_delta,
				hot_hits: 0,
			})
		}
	}
}

/// Selects a median distribution and conservative auxiliary values.
fn aggregate_read_samples(
	backend: ReadBackend,
	hot_accesses: u64,
	mut samples: Vec<ReadSample>,
) -> io::Result<BackendResult> {
	if samples.is_empty() {
		return Err(io::Error::other("cannot aggregate an empty sample set"));
	}
	samples.sort_unstable_by_key(|sample| sample.metrics.elapsed);
	let metrics = samples[samples.len() / 2].metrics;
	let rss_delta = samples.iter().map(|sample| sample.rss_delta).max().unwrap_or(0);
	let hot_hit_ratio = if backend == ReadBackend::PooledDirect && hot_accesses > 0 {
		#[allow(
			clippy::cast_precision_loss,
			reason = "bounded benchmark counters need only ratio precision"
		)]
		let ratio = samples
			.iter()
			.map(|sample| sample.hot_hits as f64 / hot_accesses as f64)
			.min_by(f64::total_cmp);
		ratio
	} else {
		None
	};
	Ok(BackendResult {
		backend,
		metrics,
		rss_delta,
		hot_hit_ratio,
	})
}

/// Preloads raw Direct pages without including them in measured latency.
fn consume_raw_direct(
	direct: &DirectFile,
	order: &[PageNo],
	extent_pages: usize,
	scratch: &mut [AlignedPage],
) -> Result<(), Box<dyn Error>> {
	for pages in order.chunks(extent_pages) {
		read_raw_direct_extent(direct, pages, scratch)?;
	}
	Ok(())
}

/// Reads one raw Direct extent and consumes its marker bytes.
fn read_raw_direct_extent(
	direct: &DirectFile,
	pages: &[PageNo],
	scratch: &mut [AlignedPage],
) -> Result<(), Box<dyn Error>> {
	let first = first_contiguous_page(pages, direct.page_count())?;
	let output = scratch
		.get_mut(..pages.len())
		.ok_or_else(|| io::Error::other("raw Direct scratch space is too small"))?;
	if pages.len() == 1 {
		direct.read_page_raw(first, &mut output[0])?;
	} else {
		direct.read_pages_raw(first, output)?;
	}
	for page in output {
		black_box(page.as_bytes()[PAGE_HEADER_SIZE]);
	}
	Ok(())
}

/// Preloads production pages without timing them.
fn consume_pool(
	pool: &BufferPool,
	direct: &DirectFile,
	source_id: CacheSourceId,
	order: &[PageNo],
	extent_pages: usize,
	page_scratch: &mut [AlignedPage],
	key_scratch: &mut Vec<PageCacheKey>,
) -> Result<(), Box<dyn Error>> {
	for pages in order.chunks(extent_pages) {
		read_pool_extent(pool, direct, source_id, pages, page_scratch, key_scratch)?;
	}
	Ok(())
}

/// Reads one production extent with scalar or batched BufferPool access.
fn read_pool_extent(
	pool: &BufferPool,
	direct: &DirectFile,
	source_id: CacheSourceId,
	pages: &[PageNo],
	page_scratch: &mut [AlignedPage],
	key_scratch: &mut Vec<PageCacheKey>,
) -> Result<(), Box<dyn Error>> {
	if pages.len() == 1 {
		let page_no = pages[0];
		let key = benchmark_key(source_id, page_no)?;
		let guard = pool.get_or_load(key, |page| direct.read_page(page_no, page))?;
		black_box(guard.as_bytes()[PAGE_HEADER_SIZE]);
		return Ok(());
	}
	first_contiguous_page(pages, direct.page_count())?;
	key_scratch.clear();
	for page_no in pages.iter().copied() {
		key_scratch.push(benchmark_key(source_id, page_no)?);
	}
	let guards =
		pool.get_or_load_batch(key_scratch, page_scratch, |loaded_source, first, output| {
			if loaded_source != source_id {
				return Err(FerrosGrynnError::storage_io_failure(
					"benchmark batch requested an unknown page source",
				));
			}
			direct.read_pages_raw(first, output)
		})?;
	for guard in guards {
		black_box(guard.as_bytes()[PAGE_HEADER_SIZE]);
	}
	Ok(())
}

/// Preloads buffered pages without timing them.
fn consume_buffered(
	source: &BufferedPageSource,
	order: &[PageNo],
	extent_pages: usize,
	scratch: &mut [AlignedPage],
) -> Result<(), Box<dyn Error>> {
	for pages in order.chunks(extent_pages) {
		source.read_extent(pages, scratch)?;
	}
	Ok(())
}

/// Preloads mapped pages without timing them.
fn consume_mmap(
	source: &mut MmapPageSource,
	order: &[PageNo],
	extent_pages: usize,
) -> Result<(), Box<dyn Error>> {
	for pages in order.chunks(extent_pages) {
		for page_no in pages.iter().copied() {
			black_box(source.read_marker(page_no)?);
		}
	}
	Ok(())
}

/// Measures logical page order with actual useful-byte accounting.
fn measure_order<Read>(
	order: &[PageNo],
	extent_pages: usize,
	unit: MetricUnit,
	mut read: Read,
) -> Result<Metrics, Box<dyn Error>>
where
	Read: FnMut(&[PageNo]) -> Result<(), Box<dyn Error>>,
{
	if order.is_empty() || extent_pages == 0 {
		return Err(io::Error::other("measured order and extent must be non-empty").into());
	}
	let operation_capacity = order.len().div_ceil(extent_pages);
	let mut latencies = Vec::with_capacity(operation_capacity);
	let started = Instant::now();
	for pages in order.chunks(extent_pages) {
		let operation_started = Instant::now();
		read(black_box(pages))?;
		latencies.push(operation_started.elapsed());
	}
	let elapsed = started.elapsed();
	let page_count = u64::try_from(order.len())
		.map_err(|source| conversion_error("measured page count does not fit u64", source))?;
	let useful_bytes = page_count
		.checked_mul(unit.bytes_per_page())
		.ok_or_else(|| io::Error::other("measured useful-byte count overflow"))?;
	metrics_from_latencies(useful_bytes, elapsed, &mut latencies).map_err(Into::into)
}

/// Builds a complete metric distribution from operation latencies.
fn metrics_from_latencies(
	useful_bytes: u64,
	elapsed: Duration,
	latencies: &mut [Duration],
) -> io::Result<Metrics> {
	if latencies.is_empty() {
		return Err(io::Error::other("cannot calculate metrics from no operations"));
	}
	latencies.sort_unstable();
	let operations = u64::try_from(latencies.len())
		.map_err(|source| conversion_error("operation count does not fit u64", source))?;
	Ok(Metrics {
		operations,
		useful_bytes,
		elapsed,
		p50: percentile(latencies, 50)?,
		p95: percentile(latencies, 95)?,
		p99: percentile(latencies, 99)?,
	})
}

/// Returns one nearest-rank percentile from sorted non-empty samples.
fn percentile(sorted: &[Duration], numerator: usize) -> io::Result<Duration> {
	if sorted.is_empty() || numerator == 0 || numerator > PERCENTILE_DENOMINATOR {
		return Err(io::Error::other("invalid percentile request"));
	}
	let rank = sorted
		.len()
		.checked_mul(numerator)
		.and_then(|value| value.checked_add(PERCENTILE_DENOMINATOR - 1))
		.map(|value| value / PERCENTILE_DENOMINATOR)
		.ok_or_else(|| io::Error::other("percentile rank overflow"))?;
	sorted
		.get(rank - 1)
		.copied()
		.ok_or_else(|| io::Error::other("percentile rank is outside the sample"))
}

/// Calibrates all required raw Direct extent sizes on fresh files.
fn calibrate_batch_size(config: &BenchmarkConfig) -> Result<BatchCalibration, Box<dyn Error>> {
	let calibration_pages = calibration_page_count(config.dataset_pages)?;
	let order = sequential_order(calibration_pages)?;
	let mut candidates = Vec::with_capacity(BATCH_CANDIDATE_KIB.len());
	for kib in BATCH_CANDIDATE_KIB {
		let bytes = kib
			.checked_mul(KIBIBYTE)
			.ok_or_else(|| io::Error::other("calibration extent byte count overflow"))?;
		if !bytes.is_multiple_of(PAGE_SIZE) {
			return Err(io::Error::other("calibration extent is not page-aligned").into());
		}
		let extent_pages = bytes / PAGE_SIZE;
		let mut samples = Vec::with_capacity(config.repetitions);
		for _ in 0..config.repetitions {
			let path = BenchPath::new("calibration")?;
			create_dataset(path.as_path(), calibration_pages)?;
			let direct = DirectFile::open_read_only(path.as_path())?;
			let mut scratch = aligned_pages(extent_pages);
			samples.push(measure_order(&order, extent_pages, MetricUnit::Page, |pages| {
				read_raw_direct_extent(&direct, pages, &mut scratch)
			})?);
		}
		samples.sort_unstable_by_key(|metrics| metrics.elapsed);
		candidates.push(CalibrationResult {
			bytes,
			metrics: samples[samples.len() / 2],
		});
	}
	let selected_bytes = select_batch_candidate(&candidates)?;
	Ok(BatchCalibration {
		candidates,
		selected_bytes,
	})
}

/// Selects the smallest candidate within throughput and p99 limits.
fn select_batch_candidate(candidates: &[CalibrationResult]) -> io::Result<usize> {
	let fastest = candidates
		.iter()
		.max_by(|left, right| {
			left.metrics
				.throughput_mib_per_second()
				.total_cmp(&right.metrics.throughput_mib_per_second())
		})
		.ok_or_else(|| io::Error::other("batch calibration has no candidates"))?;
	let minimum_throughput =
		fastest.metrics.throughput_mib_per_second() * MIN_CALIBRATED_THROUGHPUT_RATIO;
	let maximum_p99 = fastest.metrics.p99.as_secs_f64() * MAX_CALIBRATED_P99_RATIO;
	candidates
		.iter()
		.filter(|candidate| {
			candidate.metrics.throughput_mib_per_second() >= minimum_throughput
				&& candidate.metrics.p99.as_secs_f64() <= maximum_p99
		})
		.map(|candidate| candidate.bytes)
		.min()
		.ok_or_else(|| io::Error::other("no batch-size candidate satisfies calibration limits"))
}

/// Measures durable writes with prebuilt pages and one barrier per backend.
fn run_durable_writes(config: &BenchmarkConfig) -> Result<Vec<WriteResult>, Box<dyn Error>> {
	let pages = prepared_write_pages(config.pool_pages.get())?;
	let mut samples = HashMap::<WriteBackend, Vec<Metrics>>::new();
	for repetition in 0..config.repetitions {
		let order = WRITE_BACKEND_ORDERS[repetition % WRITE_BACKEND_ORDERS.len()];
		for backend in order {
			samples.entry(backend).or_default().push(measure_fresh_write(&pages, backend)?);
		}
	}
	let mut results = Vec::with_capacity(WriteBackend::ALL.len());
	for backend in WriteBackend::ALL {
		let mut backend_samples = samples.remove(&backend).ok_or_else(|| {
			io::Error::other(format!("missing {} write samples", backend.label()))
		})?;
		backend_samples.sort_unstable_by_key(|metrics| metrics.elapsed);
		results.push(WriteResult {
			backend,
			metrics: backend_samples[backend_samples.len() / 2],
		});
	}
	Ok(results)
}

/// Creates one fresh write target and times only overwrite plus one barrier.
fn measure_fresh_write(
	pages: &[AlignedPage],
	backend: WriteBackend,
) -> Result<Metrics, Box<dyn Error>> {
	let path = BenchPath::new(backend.label())?;
	create_dataset_from_pages(path.as_path(), pages)?;
	match backend {
		WriteBackend::Direct => {
			let mut file = DirectFile::open_read_write(path.as_path())?;
			measure_writes(
				pages,
				SYNC_BATCH_PAGES,
				&mut file,
				|file, page_no, pages| file.write_pages(page_no, pages).map_err(Into::into),
				|file| file.sync_data().map_err(Into::into),
			)
		}
		WriteBackend::Buffered => {
			let mut file = OpenOptions::new().write(true).open(path.as_path())?;
			measure_writes(
				pages,
				SYNC_BATCH_PAGES,
				&mut file,
				|file, page_no, pages| {
					buffered_write_all_at(
						file,
						AlignedPage::slice_as_bytes(pages),
						checked_page_offset(page_no)?,
					)?;
					Ok(())
				},
				|file| file.sync_data().map_err(Into::into),
			)
		}
		WriteBackend::Mmap => {
			let file = OpenOptions::new().read(true).write(true).open(path.as_path())?;
			// SAFETY: the unique benchmark path has a fixed verified length; no
			// other handle accesses it while this mutable mapping is live.
			let mut mapping = unsafe { MmapOptions::new().map_mut(&file)? };
			let metrics = measure_writes(
				pages,
				SYNC_BATCH_PAGES,
				&mut mapping,
				copy_pages_to_mapping,
				|mapping| mapping.flush().map_err(Into::into),
			)?;
			drop(mapping);
			drop(file);
			Ok(metrics)
		}
	}
}

/// Measures prepared writes and includes the caller's single durability
/// barrier.
fn measure_writes<State, Write, Barrier>(
	pages: &[AlignedPage],
	extent_pages: usize,
	state: &mut State,
	mut write: Write,
	barrier: Barrier,
) -> Result<Metrics, Box<dyn Error>>
where
	Write: FnMut(&mut State, PageNo, &[AlignedPage]) -> Result<(), Box<dyn Error>>,
	Barrier: FnOnce(&mut State) -> Result<(), Box<dyn Error>>,
{
	if pages.is_empty() || extent_pages == 0 {
		return Err(io::Error::other("durable write pages and extent must not be empty").into());
	}
	let mut latencies = Vec::with_capacity(pages.len().div_ceil(extent_pages));
	let started = Instant::now();
	for (extent_index, extent) in pages.chunks(extent_pages).enumerate() {
		let operation_started = Instant::now();
		let index = extent_index
			.checked_mul(extent_pages)
			.ok_or_else(|| io::Error::other("write extent page index overflow"))?;
		let page_no = u64::try_from(index)
			.map(PageNo::new)
			.map_err(|source| conversion_error("write page number does not fit u64", source))?;
		write(state, page_no, extent)?;
		latencies.push(operation_started.elapsed());
	}
	barrier(state)?;
	let elapsed = started.elapsed();
	let page_count = u64::try_from(pages.len())
		.map_err(|source| conversion_error("write page count does not fit u64", source))?;
	let useful_bytes = page_count
		.checked_mul(PAGE_SIZE as u64)
		.ok_or_else(|| io::Error::other("write useful-byte count overflow"))?;
	metrics_from_latencies(useful_bytes, elapsed, &mut latencies).map_err(Into::into)
}

/// Builds valid write pages before any timed region.
fn prepared_write_pages(page_count: usize) -> Result<Vec<AlignedPage>, Box<dyn Error>> {
	let mut pages = Vec::with_capacity(page_count);
	for index in 0..page_count {
		let page_no = u64::try_from(index)
			.map(PageNo::new)
			.map_err(|source| conversion_error("prepared page number does not fit u64", source))?;
		pages.push(benchmark_page(page_no)?);
	}
	Ok(pages)
}

/// Creates a valid immutable dataset with one explicit sync barrier.
fn create_dataset(path: &Path, page_count: u64) -> Result<(), Box<dyn Error>> {
	let file = DirectFile::create_new(path)?;
	file.resize_pages(page_count)?;
	let mut pages = aligned_pages(SYNC_BATCH_PAGES);
	let mut first_page = 0_u64;
	while first_page < page_count {
		let remaining = page_count - first_page;
		let extent_len =
			usize::try_from(remaining.min(SYNC_BATCH_PAGES as u64)).map_err(|source| {
				conversion_error("dataset extent length does not fit usize", source)
			})?;
		for (index, page) in pages[..extent_len].iter_mut().enumerate() {
			let index = u64::try_from(index).map_err(|source| {
				conversion_error("dataset page index does not fit u64", source)
			})?;
			*page = benchmark_page(PageNo::new(first_page + index))?;
		}
		file.write_pages(PageNo::new(first_page), &pages[..extent_len])?;
		first_page += extent_len as u64;
	}
	file.sync_data()?;
	drop(file);
	Ok(())
}

/// Creates a target from already prepared pages through the Direct writer.
fn create_dataset_from_pages(path: &Path, pages: &[AlignedPage]) -> Result<(), Box<dyn Error>> {
	let page_count = u64::try_from(pages.len())
		.map_err(|source| conversion_error("target page count does not fit u64", source))?;
	let file = DirectFile::create_new(path)?;
	file.resize_pages(page_count)?;
	for (extent_index, extent) in pages.chunks(SYNC_BATCH_PAGES).enumerate() {
		let index = extent_index
			.checked_mul(SYNC_BATCH_PAGES)
			.ok_or_else(|| io::Error::other("target extent page index overflow"))?;
		let page_no = u64::try_from(index)
			.map(PageNo::new)
			.map_err(|source| conversion_error("target page number does not fit u64", source))?;
		file.write_pages(page_no, extent)?;
	}
	file.sync_data()?;
	drop(file);
	Ok(())
}

/// Creates one sealed benchmark page with a deterministic payload marker.
fn benchmark_page(page_no: PageNo) -> Result<AlignedPage, Box<dyn Error>> {
	let page_lsn = benchmark_lsn(page_no)?;
	let mut page =
		AlignedPage::new(PageType::StorageSuperblock, PageFlags::empty(), page_lsn, page_no);
	page.as_bytes_mut()[PAGE_HEADER_SIZE] = page_no.get().to_le_bytes()[0];
	page.seal()?;
	Ok(page)
}

/// Creates the exact cache key encoded by a benchmark page.
fn benchmark_key(source_id: CacheSourceId, page_no: PageNo) -> FerrosGrynnResult<PageCacheKey> {
	Ok(PageCacheKey::new(source_id, page_no, benchmark_lsn(page_no)?))
}

/// Returns the deterministic non-zero LSN for one benchmark page.
fn benchmark_lsn(page_no: PageNo) -> FerrosGrynnResult<PageLsn> {
	page_no.get().checked_add(1).map(PageLsn::new).ok_or_else(|| {
		FerrosGrynnError::invalid_storage_page(page_no.get(), "benchmark LSN overflow")
	})
}

/// Validates that a page carries the deterministic benchmark version.
fn validate_benchmark_version(page_no: PageNo, actual: PageLsn) -> FerrosGrynnResult<()> {
	let expected = benchmark_lsn(page_no)?;
	if actual != expected {
		return Err(FerrosGrynnError::invalid_storage_page(
			page_no.get(),
			format!("benchmark page LSN {} does not match {}", actual.get(), expected.get()),
		));
	}
	Ok(())
}

/// Allocates reusable contiguous aligned page storage.
fn aligned_pages(count: usize) -> Vec<AlignedPage> {
	(0..count).map(|_| AlignedPage::zeroed()).collect()
}

/// Creates monotonic page order without narrowing casts.
fn sequential_order(page_count: u64) -> io::Result<Vec<PageNo>> {
	let capacity = usize::try_from(page_count)
		.map_err(|source| conversion_error("page count does not fit usize", source))?;
	let mut pages = Vec::with_capacity(capacity);
	pages.extend((0..page_count).map(PageNo::new));
	Ok(pages)
}

/// Creates deterministic Fisher-Yates order without a runtime dependency.
fn random_order(page_count: u64) -> io::Result<Vec<PageNo>> {
	let mut pages = sequential_order(page_count)?;
	shuffle(&mut pages, RANDOM_SEED)?;
	Ok(pages)
}

/// Creates one widely spaced random QD1 sample across the full file.
fn sparse_random_order(page_count: u64, pool_pages: NonZeroUsize) -> io::Result<Vec<PageNo>> {
	let sample_count = u64::try_from(pool_pages.get())
		.map_err(|source| conversion_error("sparse sample count does not fit u64", source))?;
	if page_count < sample_count {
		return Err(io::Error::other("dataset is smaller than sparse sample"));
	}
	let stride = page_count / sample_count;
	let mut pages = (0..sample_count).map(|index| PageNo::new(index * stride)).collect::<Vec<_>>();
	shuffle(&mut pages, RANDOM_SEED ^ page_count)?;
	Ok(pages)
}

/// Deterministically shuffles one page vector.
fn shuffle(pages: &mut [PageNo], mut state: u64) -> io::Result<()> {
	for index in (1..pages.len()).rev() {
		state ^= state << 13;
		state ^= state >> 7;
		state ^= state << 17;
		let upper = u64::try_from(index + 1)
			.map_err(|source| conversion_error("shuffle bound does not fit u64", source))?;
		let selected = usize::try_from(state % upper)
			.map_err(|source| conversion_error("shuffle index does not fit usize", source))?;
		pages.swap(index, selected);
	}
	Ok(())
}

/// Builds a batched unique scan interleaved with batched fixed-hot-set hits.
fn scan_hot_order(
	page_count: u64,
	hot_pages: u64,
	extent_pages: NonZeroUsize,
) -> io::Result<(Vec<PageNo>, u64)> {
	let extent_pages = u64::try_from(extent_pages.get())
		.map_err(|source| conversion_error("scan extent does not fit u64", source))?;
	if hot_pages == 0 || !hot_pages.is_multiple_of(extent_pages) {
		return Err(io::Error::other("scan hot set must contain complete extents"));
	}
	if page_count <= hot_pages {
		return Err(io::Error::other("scan dataset must exceed its hot set"));
	}
	let scan_pages = usize::try_from(page_count - hot_pages)
		.map_err(|source| conversion_error("scan page count does not fit usize", source))?;
	let extent_pages_usize = usize::try_from(extent_pages)
		.map_err(|source| conversion_error("scan extent does not fit usize", source))?;
	let extra = scan_pages / extent_pages_usize / SCAN_REVISIT_INTERVAL * extent_pages_usize;
	let mut order = Vec::with_capacity(scan_pages.saturating_add(extra));
	let mut scan_start = hot_pages;
	let mut hot_start = 0_u64;
	let mut hot_accesses = 0_u64;
	let mut scanned_extents = 0_usize;
	while scan_start.checked_add(extent_pages).is_some_and(|end| end <= page_count) {
		let scan_end = scan_start + extent_pages;
		order.extend((scan_start..scan_end).map(PageNo::new));
		scan_start = scan_end;
		scanned_extents += 1;
		if scanned_extents.is_multiple_of(SCAN_REVISIT_INTERVAL) {
			let hot_end = hot_start + extent_pages;
			order.extend((hot_start..hot_end).map(PageNo::new));
			hot_accesses += extent_pages;
			hot_start = if hot_end == hot_pages {
				0
			} else {
				hot_end
			};
		}
	}
	order.extend((scan_start..page_count).map(PageNo::new));
	Ok((order, hot_accesses))
}

/// Checks that one operation contains only adjacent physical pages.
fn first_contiguous_page(pages: &[PageNo], file_page_count: u64) -> io::Result<PageNo> {
	let first =
		pages.first().copied().ok_or_else(|| io::Error::other("page extent must not be empty"))?;
	for pair in pages.windows(2) {
		if pair[0].get().checked_add(1) != Some(pair[1].get()) {
			return Err(io::Error::other("page extent is not physically contiguous"));
		}
	}
	let count = u64::try_from(pages.len())
		.map_err(|source| conversion_error("extent page count does not fit u64", source))?;
	let end =
		first.get().checked_add(count).ok_or_else(|| io::Error::other("page extent overflow"))?;
	if end > file_page_count {
		return Err(io::Error::other("page extent is outside the file"));
	}
	Ok(first)
}

/// Copies one complete sealed page into a mutable mapping.
fn copy_pages_to_mapping(
	mapping: &mut MmapMut,
	first_page: PageNo,
	pages: &[AlignedPage],
) -> Result<(), Box<dyn Error>> {
	let start = page_byte_offset_usize(first_page)?;
	let end = start
		.checked_add(size_of_val(pages))
		.ok_or_else(|| io::Error::other("mutable mmap extent end overflow"))?;
	let destination = mapping
		.get_mut(start..end)
		.ok_or_else(|| io::Error::other("mutable mmap extent is out of range"))?;
	destination.copy_from_slice(AlignedPage::slice_as_bytes(pages));
	Ok(())
}

/// Returns the bounded page count used for batch calibration.
fn calibration_page_count(dataset_pages: u64) -> io::Result<u64> {
	let bytes = CALIBRATION_DATASET_MIB
		.checked_mul(MEBIBYTE_BYTES)
		.ok_or_else(|| io::Error::other("calibration dataset size overflow"))?;
	let pages = u64::try_from(bytes / PAGE_SIZE)
		.map_err(|source| conversion_error("calibration page count does not fit u64", source))?;
	Ok(dataset_pages.min(pages))
}

/// Reads the current process resident-set size in bytes.
fn process_rss() -> io::Result<u64> {
	let pid = get_current_pid().map_err(io::Error::other)?;
	let mut system = System::new();
	system.refresh_processes(ProcessesToUpdate::Some(&[pid]), false);
	system
		.process(pid)
		.map(sysinfo::Process::memory)
		.ok_or_else(|| io::Error::other("current process is absent from sysinfo"))
}

/// Prints a stable report for capture with `--nocapture`.
fn print_report(
	config: &BenchmarkConfig,
	reads: &[ReadComparison],
	calibration: &BatchCalibration,
	writes: &[WriteResult],
) -> io::Result<()> {
	println!(
		"stage2 dataset_pages={} pool_pages={} repetitions={} normative={} target={}",
		config.dataset_pages,
		config.pool_pages,
		config.repetitions,
		config.normative,
		std::env::consts::OS
	);
	println!(
		"| workload | backend | MiB/s | ops/s | ns/op | p50 us | p95 us | p99 us | RSS delta |"
	);
	println!("|---|---|---:|---:|---:|---:|---:|---:|---:|");
	for comparison in reads {
		for result in &comparison.backends {
			let throughput = if comparison.unit == MetricUnit::Page {
				format!("{:.2}", result.metrics.throughput_mib_per_second())
			} else {
				"-".to_owned()
			};
			println!(
				"| {} | {} | {} | {:.0} | {:.1} | {:.3} | {:.3} | {:.3} | {} |",
				comparison.name,
				result.backend.label(),
				throughput,
				result.metrics.operations_per_second(),
				result.metrics.nanoseconds_per_operation(),
				result.metrics.p50.as_secs_f64() * 1_000_000.0,
				result.metrics.p95.as_secs_f64() * 1_000_000.0,
				result.metrics.p99_micros(),
				result.rss_delta,
			);
		}
		if let Some(hit_ratio) = comparison.backend(ReadBackend::PooledDirect)?.hot_hit_ratio {
			println!("scan_hot_hit_ratio={hit_ratio:.5}");
		}
	}
	println!("| batch KiB | raw Direct MiB/s | p99 us |");
	println!("|---:|---:|---:|");
	for candidate in &calibration.candidates {
		println!(
			"| {} | {:.2} | {:.3} |",
			candidate.bytes / KIBIBYTE,
			candidate.metrics.throughput_mib_per_second(),
			candidate.metrics.p99_micros(),
		);
	}
	println!(
		"calibrated_batch_kib={} configured_batch_kib={}",
		calibration.selected_bytes / KIBIBYTE,
		SYNC_BATCH_BYTES / KIBIBYTE
	);
	println!("| durable backend | MiB/s | p99 us |");
	println!("|---|---:|---:|");
	for result in writes {
		println!(
			"| {} | {:.2} | {:.3} |",
			result.backend.label(),
			result.metrics.throughput_mib_per_second(),
			result.metrics.p99_micros(),
		);
	}
	println!(
		"memory_scope=Direct RSS is gated; mmap RSS is reported but system file cache remains \
		 unbounded"
	);
	Ok(())
}

/// Applies only comparable gates from the revised stage-2 contract.
fn apply_gate(
	config: &BenchmarkConfig,
	reads: &[ReadComparison],
	calibration: &BatchCalibration,
	writes: &[WriteResult],
) -> Result<(), Box<dyn Error>> {
	if !config.normative {
		println!("stage2_gate=skipped because a non-normative benchmark override is active");
		return Ok(());
	}
	let mut failures = Vec::new();
	let sparse = comparison(reads, SPARSE_RANDOM_WORKLOAD)?;
	let raw_sparse = sparse.backend(ReadBackend::RawDirect)?.metrics;
	let pool_sparse = sparse.backend(ReadBackend::PooledDirect)?.metrics;
	if throughput_ratio(pool_sparse, raw_sparse) < MIN_RAW_DIRECT_RATIO {
		failures.push("single-page miss throughput/raw Direct");
	}
	if latency_ratio(pool_sparse.p99, raw_sparse.p99) > MAX_RAW_DIRECT_P99_RATIO {
		failures.push("single-page miss p99/raw Direct");
	}

	let resident = comparison(reads, RESIDENT_WORKLOAD)?;
	let pool_resident = resident.backend(ReadBackend::PooledDirect)?.metrics.p99;
	let mmap_resident = resident.backend(ReadBackend::Mmap)?.metrics.p99;
	if pool_resident.saturating_sub(mmap_resident) > MAX_RESIDENT_P99_OVERHEAD {
		failures.push("resident-hit p99 overhead");
	}

	let sequential = comparison(reads, SEQUENTIAL_BATCH_WORKLOAD)?;
	let pool_sequential = sequential.backend(ReadBackend::PooledDirect)?.metrics;
	let mmap_sequential = sequential.backend(ReadBackend::Mmap)?.metrics;
	if throughput_ratio(pool_sequential, mmap_sequential) < MIN_MMAP_BATCH_RATIO {
		failures.push("batched sequential throughput/mmap");
	}

	let scan = comparison(reads, SCAN_HOT_WORKLOAD)?;
	let pool_scan = scan.backend(ReadBackend::PooledDirect)?;
	let raw_scan = scan.backend(ReadBackend::RawDirect)?;
	if pool_scan.hot_hit_ratio.unwrap_or(0.0) < MIN_SCAN_HIT_RATIO {
		failures.push("scan hot-set hit ratio");
	}
	if throughput_ratio(pool_scan.metrics, raw_scan.metrics) < MIN_RAW_DIRECT_RATIO {
		failures.push("scan-resistant throughput/raw batched Direct");
	}

	let direct_write = write_result(writes, WriteBackend::Direct)?.metrics;
	let mmap_write = write_result(writes, WriteBackend::Mmap)?.metrics;
	if throughput_ratio(direct_write, mmap_write) < MIN_MMAP_BATCH_RATIO {
		failures.push("durable write throughput/mmap");
	}

	let pool_rss = reads
		.iter()
		.filter_map(|result| result.backend(ReadBackend::PooledDirect).ok())
		.map(|result| result.rss_delta)
		.max()
		.unwrap_or(0);
	let pool_budget = config
		.pool_pages
		.get()
		.checked_mul(PAGE_SIZE)
		.ok_or_else(|| io::Error::other("pool budget overflow"))?;
	#[allow(
		clippy::cast_precision_loss,
		reason = "bounded RSS and budget counters need only ratio precision"
	)]
	let rss_ratio = pool_rss as f64 / pool_budget as f64;
	if rss_ratio > MAX_RSS_RATIO {
		failures.push("Direct storage RSS");
	}
	if calibration.selected_bytes != SYNC_BATCH_BYTES {
		failures.push("configured batch extent calibration");
	}

	if failures.is_empty() {
		println!("stage2_gate=passed");
		return Ok(());
	}
	Err(io::Error::other(format!("stage-2 gate failed: {}", failures.join(", "))).into())
}

/// Returns one named read comparison.
fn comparison<'a>(reads: &'a [ReadComparison], name: &str) -> io::Result<&'a ReadComparison> {
	reads
		.iter()
		.find(|comparison| comparison.name == name)
		.ok_or_else(|| io::Error::other(format!("missing {name} comparison")))
}

/// Returns one named durable-write result.
fn write_result(writes: &[WriteResult], backend: WriteBackend) -> io::Result<&WriteResult> {
	writes
		.iter()
		.find(|result| result.backend == backend)
		.ok_or_else(|| io::Error::other(format!("missing {} write result", backend.label())))
}

/// Returns measured throughput ratio.
fn throughput_ratio(numerator: Metrics, denominator: Metrics) -> f64 {
	numerator.throughput_mib_per_second() / denominator.throughput_mib_per_second()
}

/// Returns latency ratio while handling a zero-duration clock sample.
fn latency_ratio(numerator: Duration, denominator: Duration) -> f64 {
	if denominator.is_zero() {
		return f64::INFINITY;
	}
	numerator.as_secs_f64() / denominator.as_secs_f64()
}

/// Parses one optional positive integer environment override.
fn optional_usize(name: &'static str) -> io::Result<Option<usize>> {
	match env::var(name) {
		Ok(value) => value
			.parse::<usize>()
			.map(Some)
			.map_err(|source| io::Error::other(format!("invalid {name}: {source}"))),
		Err(env::VarError::NotPresent) => Ok(None),
		Err(env::VarError::NotUnicode(_value)) => {
			Err(io::Error::other(format!("{name} is not valid Unicode")))
		}
	}
}

/// Converts a page number to a checked byte offset.
fn checked_page_offset(page_no: PageNo) -> io::Result<u64> {
	page_no
		.get()
		.checked_mul(PAGE_SIZE as u64)
		.ok_or_else(|| io::Error::other("page byte offset overflow"))
}

/// Converts a page count to a checked file length.
fn checked_byte_length(page_count: u64) -> io::Result<u64> {
	page_count
		.checked_mul(PAGE_SIZE as u64)
		.ok_or_else(|| io::Error::other("file byte length overflow"))
}

/// Converts a page number to a checked platform slice offset.
fn page_byte_offset_usize(page_no: PageNo) -> io::Result<usize> {
	usize::try_from(checked_page_offset(page_no)?)
		.map_err(|source| conversion_error("page byte offset does not fit usize", source))
}

/// Performs one cached positional read on Linux.
#[cfg(target_os = "linux")]
fn buffered_read_at(file: &File, buffer: &mut [u8], offset: u64) -> io::Result<usize> {
	use std::os::unix::fs::FileExt;

	file.read_at(buffer, offset)
}

/// Performs one cached positional read on Windows.
#[cfg(windows)]
fn buffered_read_at(file: &File, buffer: &mut [u8], offset: u64) -> io::Result<usize> {
	use std::os::windows::fs::FileExt;

	file.seek_read(buffer, offset)
}

/// Rejects unsupported benchmark targets consistently with DirectFile.
#[cfg(not(any(target_os = "linux", windows)))]
fn buffered_read_at(_file: &File, _buffer: &mut [u8], _offset: u64) -> io::Result<usize> {
	Err(io::Error::new(io::ErrorKind::Unsupported, "storage benchmark target is unsupported"))
}

/// Performs one cached positional write on Linux.
#[cfg(target_os = "linux")]
fn buffered_write_at(file: &File, buffer: &[u8], offset: u64) -> io::Result<usize> {
	use std::os::unix::fs::FileExt;

	file.write_at(buffer, offset)
}

/// Performs one cached positional write on Windows.
#[cfg(windows)]
fn buffered_write_at(file: &File, buffer: &[u8], offset: u64) -> io::Result<usize> {
	use std::os::windows::fs::FileExt;

	file.seek_write(buffer, offset)
}

/// Rejects unsupported benchmark targets consistently with DirectFile.
#[cfg(not(any(target_os = "linux", windows)))]
fn buffered_write_at(_file: &File, _buffer: &[u8], _offset: u64) -> io::Result<usize> {
	Err(io::Error::new(io::ErrorKind::Unsupported, "storage benchmark target is unsupported"))
}

/// Completes one cached positional read, preserving short-I/O semantics.
fn buffered_read_exact_at(file: &File, mut buffer: &mut [u8], mut offset: u64) -> io::Result<()> {
	while !buffer.is_empty() {
		match buffered_read_at(file, buffer, offset) {
			Ok(0) => return Err(io::Error::from(io::ErrorKind::UnexpectedEof)),
			Ok(read) => {
				let (_, rest) = buffer.split_at_mut(read);
				buffer = rest;
				offset = offset
					.checked_add(read as u64)
					.ok_or_else(|| io::Error::other("buffered read offset overflow"))?;
			}
			Err(source) if source.kind() == io::ErrorKind::Interrupted => {}
			Err(source) => return Err(source),
		}
	}
	Ok(())
}

/// Completes one cached positional write, preserving short-I/O semantics.
fn buffered_write_all_at(file: &File, mut buffer: &[u8], mut offset: u64) -> io::Result<()> {
	while !buffer.is_empty() {
		match buffered_write_at(file, buffer, offset) {
			Ok(0) => return Err(io::Error::from(io::ErrorKind::WriteZero)),
			Ok(written) => {
				buffer = &buffer[written..];
				offset = offset
					.checked_add(written as u64)
					.ok_or_else(|| io::Error::other("buffered write offset overflow"))?;
			}
			Err(source) if source.kind() == io::ErrorKind::Interrupted => {}
			Err(source) => return Err(source),
		}
	}
	Ok(())
}

/// Preserves cause text for platform conversion failures.
fn conversion_error(context: &'static str, source: impl Error) -> io::Error {
	io::Error::other(format!("{context}: {source}"))
}

const _: () = {
	assert!(SYNC_BATCH_BYTES.is_multiple_of(PAGE_SIZE));
	assert!(SYNC_BATCH_PAGES > 0);
};

#[cfg(test)]
#[allow(
	clippy::panic_in_result_fn,
	reason = "fallible test setup returns Result while assertions report contract failures"
)]
mod tests {
	use super::*;

	#[test]
	fn resident_metrics_account_for_marker_bytes_not_pages() -> Result<(), Box<dyn Error>> {
		let order = [PageNo::new(0), PageNo::new(1), PageNo::new(2)];
		let metrics = measure_order(&order, 1, MetricUnit::Marker, |_pages| Ok(()))?;
		assert_eq!(metrics.operations, 3);
		assert_eq!(metrics.useful_bytes, 3);
		Ok(())
	}

	#[test]
	fn backend_orders_measure_every_backend_once() {
		for order in READ_BACKEND_ORDERS {
			for backend in ReadBackend::ALL {
				assert_eq!(order.iter().filter(|candidate| **candidate == backend).count(), 1);
			}
		}
	}

	#[test]
	fn batch_selection_uses_smallest_candidate_within_limits() -> io::Result<()> {
		let metric = |throughput_bytes: u64, elapsed_millis: u64, p99_millis: u64| Metrics {
			operations: 1,
			useful_bytes: throughput_bytes,
			elapsed: Duration::from_millis(elapsed_millis),
			p50: Duration::from_millis(1),
			p95: Duration::from_millis(1),
			p99: Duration::from_millis(p99_millis),
		};
		let candidates = [
			CalibrationResult {
				bytes: 64 * KIBIBYTE,
				metrics: metric(90, 1, 1),
			},
			CalibrationResult {
				bytes: 256 * KIBIBYTE,
				metrics: metric(96, 1, 1),
			},
			CalibrationResult {
				bytes: 1024 * KIBIBYTE,
				metrics: metric(100, 1, 1),
			},
		];
		assert_eq!(select_batch_candidate(&candidates)?, 256 * KIBIBYTE);
		Ok(())
	}

	#[test]
	fn scan_hot_order_keeps_every_operation_contiguous() -> io::Result<()> {
		let extent =
			NonZeroUsize::new(4).ok_or_else(|| io::Error::other("test extent must be non-zero"))?;
		let (order, hot_accesses) = scan_hot_order(40, 8, extent)?;
		for pages in order.chunks(extent.get()) {
			first_contiguous_page(pages, 40)?;
		}
		assert_eq!(hot_accesses, 8);
		assert_eq!(order.len(), 40);
		Ok(())
	}
}
