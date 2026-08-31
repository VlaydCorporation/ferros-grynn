//! Native performance gate for the synchronous page source.
//!
//! Every workload repetition owns one fresh immutable file written through
//! [`DirectFile`] and closed before measurement. All backends read that same
//! file, while backend order rotates between repetitions to control warm-cache
//! and SSD-position bias.

use std::{
	collections::HashMap,
	env,
	error::Error,
	fs::{File, OpenOptions},
	hint::black_box,
	io,
	mem::{size_of, size_of_val},
	num::NonZeroUsize,
	path::{Path, PathBuf},
	sync::{
		Arc,
		atomic::{AtomicU64, Ordering},
	},
	time::{Duration, Instant},
};

use memmap2::{Mmap, MmapMut, MmapOptions};
use parking_lot::Mutex;
use rapidhash::{HashMapExt, RapidHashMap};
use sysinfo::{ProcessesToUpdate, System, get_current_pid};

use super::{
	buffer_pool::{BufferPool, PageCacheKey, PageCacheVersion},
	direct::{DirectFile, SYNC_BATCH_BYTES, SYNC_BATCH_PAGES},
	page::{AlignedPage, PAGE_HEADER_SIZE, PAGE_SIZE, PageFlags, PageLsn, PageNo, PageType},
	types::{CheckpointGeneration, DataFileId, DataFileKind, GraphId, Lsn, PageKey},
};
use crate::{
	datastore::{Datastore, GraphWriteView, LocalOptions},
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	graph::{Graph, GraphRead, Mutable},
	values::{Bytes, PropertyMap},
};

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
/// Maximum sparse-miss sample size relative to the configured pool.
const SPARSE_SAMPLE_POOL_MULTIPLIER: usize = 4;
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
/// Rotates SSD temperature/GC position across calibration candidates.
const BATCH_CANDIDATE_ORDERS: [[usize; 5]; 3] = [[0, 1, 2, 3, 4], [4, 2, 0, 3, 1], [3, 1, 4, 2, 0]];
/// Largest calibrated extent, also the minimum calibration dataset.
const MAX_BATCH_BYTES: usize = 2048 * KIBIBYTE;
/// Nearest-rank percentile denominator.
const PERCENTILE_DENOMINATOR: usize = 100;
/// Stable resident-hit workload label.
const RESIDENT_WORKLOAD: &str = "resident hit";
/// Stable sparse-random workload label.
const SPARSE_RANDOM_WORKLOAD: &str = "sparse random QD1";
/// Disjoint page offset used to pre-initialize every pool frame before sparse
/// misses.
const SPARSE_PRELOAD_PAGE_OFFSET: u64 = 1;
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
/// Total incompressible property payload used by the stage-7 probe.
const STAGE7_PAYLOAD_MIB_ENV: &str = "FG_STAGE7_PAYLOAD_MIB";
/// Payload committed per transaction by the segmented stage-7 probe.
const STAGE7_BATCH_MIB_ENV: &str = "FG_STAGE7_BATCH_MIB";
/// Selects either one atomic transaction or the current segmented fallback.
const STAGE7_MODE_ENV: &str = "FG_STAGE7_MODE";
/// Selects large property bytes or the current dirty-target charge model.
const STAGE7_WORKLOAD_ENV: &str = "FG_STAGE7_WORKLOAD";
/// Optional exact atom count used to shrink the dirty-target probe.
const STAGE7_ATOMS_ENV: &str = "FG_STAGE7_ATOMS";
/// Transaction size required by the stage-7 plan before spill is considered.
const DEFAULT_STAGE7_PAYLOAD_MIB: usize = 320;
/// Current bounded fallback used by bulk-style workloads.
const DEFAULT_STAGE7_BATCH_MIB: usize = 64;
/// Logical charge assigned to one dirty atom by the current transaction limit.
const DIRTY_ATOM_CHARGE_BYTES: usize = 256;
/// Unique suffix source for benchmark files within one process.
static NEXT_BENCH_FILE: AtomicU64 = AtomicU64::new(0);

/// Five independently measured read paths.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum ReadBackend {
	/// Direct transport only, without page validation.
	RawDirect,
	/// Direct transport with production page validation but without caching.
	ValidatedDirect,
	/// Production Direct I/O through BufferPool with full validation.
	PooledDirect,
	/// Ordinary cached positional file reads with full validation.
	Buffered,
	/// Benchmark-only read-only mapping with first-access validation.
	Mmap,
}

impl ReadBackend {
	/// Stable backend order for aggregation and reports.
	const ALL: [Self; 5] =
		[Self::RawDirect, Self::ValidatedDirect, Self::PooledDirect, Self::Buffered, Self::Mmap];

	/// Returns a stable report and temporary-file label.
	const fn label(self) -> &'static str {
		match self {
			Self::RawDirect => "raw-direct",
			Self::ValidatedDirect => "validated-direct",
			Self::PooledDirect => "direct-pool",
			Self::Buffered => "buffered",
			Self::Mmap => "mmap",
		}
	}
}

/// Rotating orders used by successive repetitions.
const READ_BACKEND_ORDERS: [[ReadBackend; 5]; 3] = [
	[
		ReadBackend::RawDirect,
		ReadBackend::ValidatedDirect,
		ReadBackend::Buffered,
		ReadBackend::Mmap,
		ReadBackend::PooledDirect,
	],
	[
		ReadBackend::PooledDirect,
		ReadBackend::RawDirect,
		ReadBackend::Mmap,
		ReadBackend::ValidatedDirect,
		ReadBackend::Buffered,
	],
	[
		ReadBackend::Buffered,
		ReadBackend::PooledDirect,
		ReadBackend::ValidatedDirect,
		ReadBackend::Mmap,
		ReadBackend::RawDirect,
	],
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
	[WriteBackend::Mmap, WriteBackend::Direct, WriteBackend::Buffered],
	[WriteBackend::Buffered, WriteBackend::Mmap, WriteBackend::Direct],
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

/// Complete backend result for one access pattern.
struct ReadComparison {
	/// Stable workload label.
	name: &'static str,
	/// Correct accounting and report unit.
	unit: MetricUnit,
	/// Median result for every backend.
	backends: Vec<BackendResult>,
	/// Median of same-repetition pool/raw throughput ratios.
	paired_pool_raw_throughput_ratio: f64,
	/// Median of same-repetition pool/raw p99 ratios.
	paired_pool_raw_p99_ratio: f64,
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

/// RAII-owned unique local-database directory used by large transaction probes.
struct BenchDirectory(PathBuf);

impl BenchDirectory {
	/// Returns a collision-resistant directory path in the workspace.
	fn path(label: &str) -> io::Result<PathBuf> {
		let suffix = NEXT_BENCH_FILE.fetch_add(1, Ordering::Relaxed);
		let name = format!(".fg-{label}-{}-{suffix}", std::process::id());
		Ok(env::current_dir()?.join(name))
	}
}

impl Drop for BenchDirectory {
	fn drop(&mut self) {
		drop(std::fs::remove_dir_all(&self.0));
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
			let header = page.validate(page_no)?;
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

/// Profiles the sparse QD1 miss path while separating validation from cache
/// metadata.
#[test]
#[ignore = "native release-mode probe for sparse Direct-I/O misses"]
fn disk_storage_stage3_sparse_probe() -> Result<(), Box<dyn Error>> {
	let config = BenchmarkConfig::from_environment()?;
	let workload = read_workloads(&config)?
		.into_iter()
		.find(|workload| workload.name == SPARSE_RANDOM_WORKLOAD)
		.ok_or_else(|| io::Error::other("sparse benchmark workload is missing"))?;
	let comparison = run_read_workload(&config, &workload, 0)?;
	let raw = comparison.backend(ReadBackend::RawDirect)?.metrics;
	let validated = comparison.backend(ReadBackend::ValidatedDirect)?.metrics;
	let pooled = comparison.backend(ReadBackend::PooledDirect)?.metrics;
	println!(
		"stage3_sparse_probe dataset_pages={} pool_pages={} repetitions={} raw_mib_s={:.2} \
		 validated_mib_s={:.2} pooled_mib_s={:.2} validated_raw_ratio={:.4} \
		 pooled_validated_ratio={:.4} pooled_raw_ratio={:.4} pooled_raw_p99_ratio={:.4} \
		 paired_pooled_raw_ratio={:.4} paired_pooled_raw_p99_ratio={:.4}",
		config.dataset_pages,
		config.pool_pages,
		config.repetitions,
		raw.throughput_mib_per_second(),
		validated.throughput_mib_per_second(),
		pooled.throughput_mib_per_second(),
		throughput_ratio(validated, raw),
		throughput_ratio(pooled, validated),
		throughput_ratio(pooled, raw),
		latency_ratio(pooled.p99, raw.p99),
		comparison.paired_pool_raw_throughput_ratio,
		comparison.paired_pool_raw_p99_ratio,
	);
	Ok(())
}

/// Profiles the fully resident scalar hit path against a warmed mapping.
#[test]
#[ignore = "native release-mode probe for resident page-cache hits"]
fn disk_storage_stage3_resident_probe() -> Result<(), Box<dyn Error>> {
	let config = BenchmarkConfig::from_environment()?;
	let workload = read_workloads(&config)?
		.into_iter()
		.find(|workload| workload.name == RESIDENT_WORKLOAD)
		.ok_or_else(|| io::Error::other("resident benchmark workload is missing"))?;
	let comparison = run_read_workload(&config, &workload, 0)?;
	let pooled = comparison.backend(ReadBackend::PooledDirect)?.metrics;
	let mapped = comparison.backend(ReadBackend::Mmap)?.metrics;
	println!(
		"stage3_resident_probe pool_pages={} repetitions={} pooled_ops_s={:.0} mmap_ops_s={:.0} \
		 pooled_mmap_ratio={:.4} pooled_p99_us={:.3} mmap_p99_us={:.3}",
		config.pool_pages,
		config.repetitions,
		pooled.operations_per_second(),
		mapped.operations_per_second(),
		pooled.operations_per_second() / mapped.operations_per_second(),
		pooled.p99_micros(),
		mapped.p99_micros(),
	);
	Ok(())
}

/// Compares the current cache index with compact ordered and dense lookup
/// candidates on the real cache key, including replacement churn.
#[test]
#[ignore = "native release-mode probe for buffer-pool cache-index layout"]
fn disk_storage_stage3_cache_index_probe() -> Result<(), Box<dyn Error>> {
	const LOOKUP_SAMPLES: usize = 5;
	const MIN_LOOKUPS: usize = 1_000_000;
	const CHURN_OPERATIONS: usize = 32_768;

	fn rank(key: PageCacheKey) -> (PageKey, PageCacheVersion) {
		(key.page_key(), key.version())
	}

	fn keys(entry_count: usize, file_count: usize) -> io::Result<Vec<PageCacheKey>> {
		let file_ids = (0..file_count)
			.map(|index| {
				let instance = u64::try_from(index + 1).map_err(|source| {
					conversion_error("cache-index file id does not fit u64", source)
				})?;
				DataFileId::new(instance, DataFileKind::Index)
					.ok_or_else(|| io::Error::other("cache-index file id is outside format-v1"))
			})
			.collect::<io::Result<Vec<_>>>()?;
		(0..entry_count)
			.map(|index| {
				let page_no = u64::try_from(index / file_count).map_err(|source| {
					conversion_error("cache-index page number does not fit u64", source)
				})?;
				Ok(PageCacheKey::new(
					PageKey::new(
						GraphId::SYSTEM,
						file_ids[index % file_count],
						PageNo::new(page_no),
					),
					PageCacheVersion::Home {
						generation: CheckpointGeneration::new(1),
						ceiling: Lsn::new(1),
					},
				))
			})
			.collect()
	}

	fn lookup_order(entries: &[PageCacheKey]) -> io::Result<Vec<PageCacheKey>> {
		let page_count = u64::try_from(entries.len()).map_err(|source| {
			conversion_error("cache-index entry count does not fit u64", source)
		})?;
		random_order(page_count)?
			.into_iter()
			.map(|page_no| {
				let index = usize::try_from(page_no.get()).map_err(|source| {
					conversion_error("cache-index lookup index does not fit usize", source)
				})?;
				entries
					.get(index)
					.copied()
					.ok_or_else(|| io::Error::other("cache-index lookup index is outside entries"))
			})
			.collect()
	}

	fn median_lookup_ns(
		order: &[PageCacheKey],
		mut lookup: impl FnMut(PageCacheKey) -> Option<usize>,
	) -> io::Result<f64> {
		if order.is_empty() {
			return Err(io::Error::other("cache-index lookup order must not be empty"));
		}
		let rounds = MIN_LOOKUPS.div_ceil(order.len());
		let operations = rounds
			.checked_mul(order.len())
			.ok_or_else(|| io::Error::other("cache-index lookup count overflow"))?;
		let operations = u32::try_from(operations).map_err(|source| {
			conversion_error("cache-index lookup count does not fit u32", source)
		})?;
		let mut samples = [0.0; LOOKUP_SAMPLES];
		for sample in &mut samples {
			for key in order.iter().copied() {
				black_box(lookup(black_box(key)));
			}
			let started = Instant::now();
			for _ in 0..rounds {
				for key in order.iter().copied() {
					black_box(lookup(black_box(key)));
				}
			}
			*sample = started.elapsed().as_secs_f64() * 1_000_000_000.0 / f64::from(operations);
		}
		samples.sort_unstable_by(f64::total_cmp);
		Ok(samples[LOOKUP_SAMPLES / 2])
	}

	println!("cache_index_probe key_bytes={}", size_of::<PageCacheKey>());
	for entry_count in [64, 1_024, 4_096] {
		for file_count in [1, 8] {
			let entries = keys(entry_count, file_count)?;
			let order = lookup_order(&entries)?;

			let mut hash = RapidHashMap::with_capacity(entry_count);
			hash.extend(entries.iter().copied().zip(0..entry_count));
			let hash = Mutex::new(hash);
			let hash_ns = median_lookup_ns(&order, |key| hash.lock().get(&key).copied())?;

			let mut ordered = entries.iter().copied().zip(0..entry_count).collect::<Vec<_>>();
			ordered.sort_unstable_by_key(|(key, _)| rank(*key));
			let ordered = Mutex::new(ordered);
			let ordered_ns = median_lookup_ns(&order, |key| {
				let entries = ordered.lock();
				if entries.len() <= 64 {
					entries.iter().find(|(candidate, _)| *candidate == key).map(|(_, value)| *value)
				} else {
					entries
						.binary_search_by_key(&rank(key), |(candidate, _)| rank(*candidate))
						.ok()
						.map(|index| entries[index].1)
				}
			})?;

			let dense_ns = if file_count == 1 {
				let dense = entries
					.iter()
					.copied()
					.enumerate()
					.map(|(frame, key)| (key.version(), frame))
					.collect::<Vec<_>>();
				let dense = Mutex::new(dense);
				Some(median_lookup_ns(&order, |key| {
					let index = usize::try_from(key.page_no().get()).ok()?;
					dense
						.lock()
						.get(index)
						.filter(|(version, _)| *version == key.version())
						.map(|(_, frame)| *frame)
				})?)
			} else {
				None
			};

			println!(
				"cache_index_probe entries={entry_count} files={file_count} hash_ns={hash_ns:.2} \
				 ordered_ns={ordered_ns:.2} dense_one_source_ns={dense_ns:?}"
			);
		}
	}

	let initial = keys(4_096, 1)?;
	let replacements =
		keys(4_096 + CHURN_OPERATIONS, 1)?.into_iter().skip(4_096).collect::<Vec<_>>();
	let mut active = initial.clone();
	let mut hash = RapidHashMap::with_capacity(initial.len());
	hash.extend(initial.iter().copied().zip(0..initial.len()));
	let hash = Mutex::new(hash);
	let started = Instant::now();
	for (operation, replacement) in replacements.iter().copied().enumerate() {
		let slot = operation % active.len();
		let old = std::mem::replace(&mut active[slot], replacement);
		let mut entries = hash.lock();
		black_box(entries.remove(&old));
		black_box(entries.insert(replacement, slot));
	}
	let churn_operations = u32::try_from(CHURN_OPERATIONS)
		.map_err(|source| conversion_error("cache-index churn count does not fit u32", source))?;
	let hash_churn_ns =
		started.elapsed().as_secs_f64() * 1_000_000_000.0 / f64::from(churn_operations);

	let mut active = initial.clone();
	let mut ordered = initial.iter().copied().zip(0..initial.len()).collect::<Vec<_>>();
	ordered.sort_unstable_by_key(|(key, _)| rank(*key));
	let ordered = Mutex::new(ordered);
	let started = Instant::now();
	for (operation, replacement) in replacements.iter().copied().enumerate() {
		let slot = operation % active.len();
		let old = std::mem::replace(&mut active[slot], replacement);
		let mut entries = ordered.lock();
		let Ok(old_index) =
			entries.binary_search_by_key(&rank(old), |(candidate, _)| rank(*candidate))
		else {
			return Err(io::Error::other("ordered cache-index churn lost an active key").into());
		};
		black_box(entries.remove(old_index));
		let Err(insertion) =
			entries.binary_search_by_key(&rank(replacement), |(candidate, _)| rank(*candidate))
		else {
			return Err(io::Error::other(
				"ordered cache-index churn generated a duplicate replacement key",
			)
			.into());
		};
		entries.insert(insertion, (replacement, slot));
	}
	let ordered_churn_ns =
		started.elapsed().as_secs_f64() * 1_000_000_000.0 / f64::from(churn_operations);
	println!(
		"cache_index_probe churn_entries=4096 hash_ns={hash_churn_ns:.2} \
		 ordered_ns={ordered_churn_ns:.2}"
	);

	let entries = keys(4_096, 1)?;
	let order = lookup_order(&entries)?;
	let pages = (0..entries.len())
		.map(|index| {
			let mut page = AlignedPage::zeroed();
			page.as_bytes_mut()[PAGE_HEADER_SIZE] = index.to_le_bytes()[0];
			Arc::new(Box::new(page))
		})
		.collect::<Vec<_>>();
	let mut hash = RapidHashMap::with_capacity(entries.len());
	hash.extend(entries.iter().copied().zip(0..entries.len()));
	let hash = Mutex::new(hash);
	let hash_payload_ns = median_lookup_ns(&order, |key| {
		let frame = hash.lock().get(&key).copied()?;
		let page = Arc::clone(pages.get(frame)?);
		Some(usize::from(page.as_bytes()[PAGE_HEADER_SIZE]))
	})?;
	let mut ordered = entries.iter().copied().zip(0..entries.len()).collect::<Vec<_>>();
	ordered.sort_unstable_by_key(|(key, _)| rank(*key));
	let ordered = Mutex::new(ordered);
	let ordered_payload_ns = median_lookup_ns(&order, |key| {
		let entries = ordered.lock();
		let frame = entries
			.binary_search_by_key(&rank(key), |(candidate, _)| rank(*candidate))
			.ok()
			.map(|index| entries[index].1)?;
		let page = Arc::clone(pages.get(frame)?);
		Some(usize::from(page.as_bytes()[PAGE_HEADER_SIZE]))
	})?;
	let dense = Mutex::new(
		entries
			.iter()
			.copied()
			.enumerate()
			.map(|(frame, key)| (key.version(), frame))
			.collect::<Vec<_>>(),
	);
	let dense_payload_ns = median_lookup_ns(&order, |key| {
		let index = usize::try_from(key.page_no().get()).ok()?;
		let frame = dense
			.lock()
			.get(index)
			.filter(|(version, _)| *version == key.version())
			.map(|(_, frame)| *frame)?;
		let page = Arc::clone(pages.get(frame)?);
		Some(usize::from(page.as_bytes()[PAGE_HEADER_SIZE]))
	})?;
	println!(
		"cache_index_probe payload_entries=4096 hash_ns={hash_payload_ns:.2} \
		 ordered_ns={ordered_payload_ns:.2} dense_one_source_ns={dense_payload_ns:.2}"
	);
	Ok(())
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

/// Measures whether transaction-private spill has a material workload on the
/// current local storage path before production spill code is introduced.
#[test]
#[ignore = "allocates and writes more than 256 MiB; run natively in release mode"]
fn disk_storage_stage7_spill_probe() -> Result<(), Box<dyn Error>> {
	let payload_mib = optional_usize(STAGE7_PAYLOAD_MIB_ENV)?.unwrap_or(DEFAULT_STAGE7_PAYLOAD_MIB);
	let batch_mib = optional_usize(STAGE7_BATCH_MIB_ENV)?.unwrap_or(DEFAULT_STAGE7_BATCH_MIB);
	if payload_mib == 0 || batch_mib == 0 {
		return Err(io::Error::other("stage-7 payload and batch sizes must be non-zero").into());
	}
	let mode = env::var(STAGE7_MODE_ENV).unwrap_or_else(|_| "atomic".to_owned());
	if mode != "atomic" && mode != "segmented" {
		return Err(io::Error::other("FG_STAGE7_MODE must be 'atomic' or 'segmented'").into());
	}
	let workload = env::var(STAGE7_WORKLOAD_ENV).unwrap_or_else(|_| "properties".to_owned());
	if workload != "properties" && workload != "atoms" {
		return Err(io::Error::other("FG_STAGE7_WORKLOAD must be 'properties' or 'atoms'").into());
	}
	let large_properties = workload == "properties";
	let exact_atoms = optional_usize(STAGE7_ATOMS_ENV)?;
	if exact_atoms == Some(0) {
		return Err(io::Error::other("FG_STAGE7_ATOMS must be non-zero").into());
	}

	let directory_path = BenchDirectory::path("s7")?;
	let store = Datastore::create_local(&directory_path, LocalOptions::default())?;
	let _directory = BenchDirectory(directory_path);
	let payload_key = store.intern_property("stage7_payload")?;
	let mut create = store.begin();
	let graph = create
		.create_graph("graph:stage7-spill".parse()?, Graph::<Mutable>::new().freeze().into())?;
	create.commit()?;

	let baseline_rss = process_rss()?;
	let started = Instant::now();
	let mut staged_elapsed = Duration::ZERO;
	let mut commit_elapsed = Duration::ZERO;
	let mut max_staged_rss = baseline_rss;
	let mut committed_mib = 0_usize;
	let mut committed_nodes = 0_usize;
	let mut attempted_mib = 0_usize;
	let mut attempted_nodes = 0_usize;
	let mut commit_status = "committed".to_owned();
	let batch_limit = if mode == "atomic" {
		payload_mib
	} else {
		batch_mib.min(payload_mib)
	};

	while committed_mib < payload_mib {
		let current_mib = batch_limit.min(payload_mib - committed_mib);
		let current_nodes = if large_properties {
			current_mib
		} else if let Some(exact_atoms) = exact_atoms {
			exact_atoms
		} else {
			current_mib
				.checked_mul(MEBIBYTE_BYTES / DIRTY_ATOM_CHARGE_BYTES)
				.ok_or_else(|| io::Error::other("stage-7 dirty-atom count overflow"))?
		};
		let next_committed_nodes = committed_nodes
			.checked_add(current_nodes)
			.ok_or_else(|| io::Error::other("stage-7 committed node count overflow"))?;
		attempted_mib = committed_mib
			.checked_add(current_mib)
			.ok_or_else(|| io::Error::other("stage-7 attempted MiB count overflow"))?;
		attempted_nodes = next_committed_nodes;
		let stage_started = Instant::now();
		let mut transaction = store.begin();
		let view = transaction.edit_graph(&graph)?;
		let GraphWriteView::Graph(graph_write) = view else {
			return Err(io::Error::other("stage-7 benchmark graph changed family").into());
		};
		for offset in 0..current_nodes {
			let mut properties = PropertyMap::new();
			if large_properties {
				let ordinal = committed_nodes
					.checked_add(offset)
					.ok_or_else(|| io::Error::other("stage-7 payload ordinal overflow"))?;
				properties.insert(payload_key, Bytes::from(incompressible_mebibyte(ordinal)?));
			}
			graph_write.add_node(None, properties)?;
		}
		staged_elapsed += stage_started.elapsed();
		max_staged_rss = max_staged_rss.max(process_rss()?);

		let commit_started = Instant::now();
		let failed = match transaction.commit() {
			Ok(_) => {
				committed_mib += current_mib;
				committed_nodes = next_committed_nodes;
				false
			}
			Err(error) => {
				commit_status = error.status().code().to_string();
				if commit_status != "FG-54F07" {
					return Err(error.into());
				}
				true
			}
		};
		commit_elapsed += commit_started.elapsed();
		if failed || mode == "atomic" {
			break;
		}
	}

	let after_rss = process_rss()?;
	let read = store.begin();
	let visible_nodes = graph.read(&read)?.node_count()?;
	let expected_nodes = committed_nodes;
	if visible_nodes != expected_nodes {
		return Err(io::Error::other(format!(
			"stage-7 visibility mismatch: expected {expected_nodes} nodes, found {visible_nodes}",
		))
		.into());
	}
	let logical_attempted_mib =
		stage7_logical_mib(large_properties, attempted_mib, attempted_nodes);
	let logical_committed_mib =
		stage7_logical_mib(large_properties, committed_mib, committed_nodes);

	println!(
		"stage7 workload={workload} mode={mode} payload_mib={payload_mib} batch_mib={batch_limit} \
		 logical_attempted_mib={logical_attempted_mib:.2} \
		 logical_committed_mib={logical_committed_mib:.2} attempted_nodes={attempted_nodes} \
		 committed_nodes={committed_nodes} status={commit_status} staged_ms={:.3} commit_ms={:.3} \
		 total_ms={:.3} staged_rss_delta_mib={:.2} after_rss_delta_mib={:.2}",
		staged_elapsed.as_secs_f64() * 1_000.0,
		commit_elapsed.as_secs_f64() * 1_000.0,
		started.elapsed().as_secs_f64() * 1_000.0,
		bytes_to_mib(max_staged_rss.saturating_sub(baseline_rss)),
		bytes_to_mib(after_rss.saturating_sub(baseline_rss)),
	);
	Ok(())
}

/// Produces deterministic bytes that do not collapse under property
/// compression and therefore exercise the physical transaction limit.
fn incompressible_mebibyte(ordinal: usize) -> io::Result<Vec<u8>> {
	let mut value = vec![0_u8; MEBIBYTE_BYTES];
	let mut state = RANDOM_SEED
		^ u64::try_from(ordinal).map_err(|source| {
			conversion_error("stage-7 payload ordinal does not fit u64", source)
		})?;
	for bytes in value.chunks_exact_mut(size_of::<u64>()) {
		state ^= state << 13;
		state ^= state >> 7;
		state ^= state << 17;
		bytes.copy_from_slice(&state.to_le_bytes());
	}
	Ok(value)
}

/// Converts the benchmark's logical charge into a diagnostic MiB value.
#[allow(clippy::cast_precision_loss, reason = "stage-7 output needs only two-decimal reporting")]
fn stage7_logical_mib(large_properties: bool, committed_mib: usize, nodes: usize) -> f64 {
	if large_properties {
		committed_mib as f64
	} else {
		nodes as f64 * DIRTY_ATOM_CHARGE_BYTES as f64 / MEBIBYTE_BYTES as f64
	}
}

/// Converts an exact byte counter into a human-readable MiB value.
#[allow(clippy::cast_precision_loss, reason = "benchmark RSS needs only two-decimal reporting")]
fn bytes_to_mib(bytes: u64) -> f64 {
	bytes as f64 / MEBIBYTE_BYTES as f64
}

/// Builds every required deterministic workload.
fn read_workloads(config: &BenchmarkConfig) -> Result<Vec<ReadWorkload>, Box<dyn Error>> {
	let resident_pages = u64::try_from(config.pool_pages.get())
		.map_err(|source| conversion_error("pool page count does not fit u64", source))?;
	let resident_preload = sequential_order(resident_pages)?;
	let resident_order = random_order(resident_pages)?.repeat(RESIDENT_PASSES);
	let sparse_order = sparse_random_order(config.dataset_pages, config.pool_pages)?;
	let sparse_preload = sparse_preload_order(&sparse_order, config.dataset_pages)?;
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
			preload: sparse_preload,
			order: sparse_order,
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

/// Measures every backend against the same immutable file with rotating order.
fn run_read_workload(
	config: &BenchmarkConfig,
	workload: &ReadWorkload,
	ordinal: usize,
) -> Result<ReadComparison, Box<dyn Error>> {
	let mut samples = HashMap::<ReadBackend, Vec<ReadSample>>::new();
	for repetition in 0..config.repetitions {
		let path = BenchPath::new("shared-read")?;
		create_dataset(path.as_path(), workload.page_count)?;
		let order = READ_BACKEND_ORDERS[(repetition + ordinal) % READ_BACKEND_ORDERS.len()];
		for backend in order {
			let sample = measure_read(config, workload, backend, path.as_path())?;
			samples.entry(backend).or_default().push(sample);
		}
	}
	let (paired_pool_raw_throughput_ratio, paired_pool_raw_p99_ratio) =
		paired_pool_raw_ratios(&samples)?;

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
		paired_pool_raw_throughput_ratio,
		paired_pool_raw_p99_ratio,
	})
}

/// Returns medians of pool/raw ratios while preserving repetition pairing.
fn paired_pool_raw_ratios(
	samples: &HashMap<ReadBackend, Vec<ReadSample>>,
) -> io::Result<(f64, f64)> {
	let raw = samples
		.get(&ReadBackend::RawDirect)
		.ok_or_else(|| io::Error::other("missing raw Direct samples for pairing"))?;
	let pooled = samples
		.get(&ReadBackend::PooledDirect)
		.ok_or_else(|| io::Error::other("missing pooled Direct samples for pairing"))?;
	if raw.len() != pooled.len() || raw.is_empty() {
		return Err(io::Error::other("raw and pooled Direct samples cannot be paired"));
	}
	let mut throughput = raw
		.iter()
		.zip(pooled)
		.map(|(raw, pooled)| throughput_ratio(pooled.metrics, raw.metrics))
		.collect::<Vec<_>>();
	let mut p99 = raw
		.iter()
		.zip(pooled)
		.map(|(raw, pooled)| latency_ratio(pooled.metrics.p99, raw.metrics.p99))
		.collect::<Vec<_>>();
	throughput.sort_unstable_by(f64::total_cmp);
	p99.sort_unstable_by(f64::total_cmp);
	Ok((throughput[throughput.len() / 2], p99[p99.len() / 2]))
}

/// Measures one backend against an already-created immutable dataset.
fn measure_read(
	config: &BenchmarkConfig,
	workload: &ReadWorkload,
	backend: ReadBackend,
	path: &Path,
) -> Result<ReadSample, Box<dyn Error>> {
	let extent_pages = workload.extent_pages.get();
	let source_id = DataFileId::new(1, DataFileKind::SystemCatalog)
		.ok_or_else(|| io::Error::other("benchmark source identity is invalid"))?;

	match backend {
		ReadBackend::RawDirect => {
			let direct = DirectFile::open_read_only(path)?;
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
		ReadBackend::ValidatedDirect => {
			let direct = DirectFile::open_read_only(path)?;
			let mut scratch = aligned_pages(extent_pages);
			consume_validated_direct(&direct, &workload.preload, extent_pages, &mut scratch)?;
			let metrics = measure_order(&workload.order, extent_pages, workload.unit, |pages| {
				read_validated_direct_extent(&direct, pages, &mut scratch)
			})?;
			Ok(ReadSample {
				metrics,
				rss_delta: 0,
				hot_hits: 0,
			})
		}
		ReadBackend::PooledDirect => {
			let direct = DirectFile::open_read_only(path)?;
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
			let source = BufferedPageSource::open(path, workload.page_count)?;
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
			let mut source = MmapPageSource::open(path, workload.page_count)?;
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

/// Preloads validated Direct pages without including them in measured latency.
fn consume_validated_direct(
	direct: &DirectFile,
	order: &[PageNo],
	extent_pages: usize,
	scratch: &mut [AlignedPage],
) -> Result<(), Box<dyn Error>> {
	for pages in order.chunks(extent_pages) {
		read_validated_direct_extent(direct, pages, scratch)?;
	}
	Ok(())
}

/// Reads and validates one Direct extent without BufferPool metadata or copies.
fn read_validated_direct_extent(
	direct: &DirectFile,
	pages: &[PageNo],
	scratch: &mut [AlignedPage],
) -> Result<(), Box<dyn Error>> {
	let first = first_contiguous_page(pages, direct.page_count())?;
	let output = scratch
		.get_mut(..pages.len())
		.ok_or_else(|| io::Error::other("validated Direct scratch space is too small"))?;
	if pages.len() == 1 {
		let header = direct.read_page(first, &mut output[0])?;
		validate_benchmark_version(first, header.page_lsn())?;
		black_box(output[0].as_bytes()[PAGE_HEADER_SIZE]);
		return Ok(());
	}
	direct.read_pages_raw(first, output)?;
	for (page_no, page) in pages.iter().copied().zip(output) {
		let header = page.validate(page_no)?;
		validate_benchmark_version(page_no, header.page_lsn())?;
		black_box(page.as_bytes()[PAGE_HEADER_SIZE]);
	}
	Ok(())
}

/// Preloads production pages without timing them.
fn consume_pool(
	pool: &BufferPool,
	direct: &DirectFile,
	source_id: DataFileId,
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
	source_id: DataFileId,
	pages: &[PageNo],
	page_scratch: &mut [AlignedPage],
	key_scratch: &mut Vec<PageCacheKey>,
) -> Result<(), Box<dyn Error>> {
	if pages.len() == 1 {
		let page_no = pages[0];
		let key = benchmark_key(source_id, page_no);
		let guard = pool.get_or_load(key, |page| direct.read_page(page_no, page))?;
		black_box(guard.as_bytes()[PAGE_HEADER_SIZE]);
		return Ok(());
	}
	first_contiguous_page(pages, direct.page_count())?;
	key_scratch.clear();
	for page_no in pages.iter().copied() {
		key_scratch.push(benchmark_key(source_id, page_no));
	}
	let guards = pool.get_or_load_batch(key_scratch, page_scratch, |first, output| {
		if first.graph_id() != GraphId::SYSTEM || first.file_id() != source_id {
			return Err(FerrosGrynnError::storage_io_failure(
				"benchmark batch requested an unknown page source",
			));
		}
		direct.read_pages_raw(first.page_no(), output)
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
	let mut sample_sets: [Vec<Metrics>; BATCH_CANDIDATE_KIB.len()] =
		std::array::from_fn(|_index| Vec::with_capacity(config.repetitions));
	for repetition in 0..config.repetitions {
		for candidate in BATCH_CANDIDATE_ORDERS[repetition % BATCH_CANDIDATE_ORDERS.len()] {
			let kib = BATCH_CANDIDATE_KIB[candidate];
			let bytes = kib
				.checked_mul(KIBIBYTE)
				.ok_or_else(|| io::Error::other("calibration extent byte count overflow"))?;
			if !bytes.is_multiple_of(PAGE_SIZE) {
				return Err(io::Error::other("calibration extent is not page-aligned").into());
			}
			let extent_pages = bytes / PAGE_SIZE;
			let path = BenchPath::new("calibration")?;
			create_dataset(path.as_path(), calibration_pages)?;
			let direct = DirectFile::open_read_only(path.as_path())?;
			let mut scratch = aligned_pages(extent_pages);
			sample_sets[candidate].push(measure_order(
				&order,
				extent_pages,
				MetricUnit::Page,
				|pages| read_raw_direct_extent(&direct, pages, &mut scratch),
			)?);
		}
	}
	let mut candidates = Vec::with_capacity(BATCH_CANDIDATE_KIB.len());
	for (kib, mut samples) in BATCH_CANDIDATE_KIB.into_iter().zip(sample_sets) {
		let bytes = kib
			.checked_mul(KIBIBYTE)
			.ok_or_else(|| io::Error::other("calibration extent byte count overflow"))?;
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
fn benchmark_key(source_id: DataFileId, page_no: PageNo) -> PageCacheKey {
	PageCacheKey::new(PageKey::new(GraphId::SYSTEM, source_id, page_no), PageCacheVersion::Home {
		generation: CheckpointGeneration::ZERO,
		ceiling: Lsn::new(u64::MAX),
	})
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
	let pool_page_count = u64::try_from(pool_pages.get())
		.map_err(|source| conversion_error("sparse sample count does not fit u64", source))?;
	let desired_sample_count = pool_pages
		.get()
		.checked_mul(SPARSE_SAMPLE_POOL_MULTIPLIER)
		.ok_or_else(|| io::Error::other("sparse sample count overflow"))?;
	let desired_sample_count = u64::try_from(desired_sample_count)
		.map_err(|source| conversion_error("sparse sample count does not fit u64", source))?;
	let minimum_page_count = pool_page_count
		.checked_mul(2)
		.ok_or_else(|| io::Error::other("sparse minimum dataset size overflow"))?;
	if page_count < minimum_page_count {
		return Err(io::Error::other("dataset is too small for disjoint sparse preload"));
	}
	let sample_count = desired_sample_count.min(page_count / 2);
	let stride = page_count / sample_count;
	let mut pages = (0..sample_count).map(|index| PageNo::new(index * stride)).collect::<Vec<_>>();
	shuffle(&mut pages, RANDOM_SEED ^ page_count)?;
	Ok(pages)
}

/// Builds a disjoint warm-up set so sparse misses measure frame reuse, not
/// first-touch allocation.
fn sparse_preload_order(order: &[PageNo], page_count: u64) -> io::Result<Vec<PageNo>> {
	order
		.iter()
		.map(|page_no| {
			page_no
				.get()
				.checked_add(SPARSE_PRELOAD_PAGE_OFFSET)
				.filter(|page_no| *page_no < page_count)
				.map(PageNo::new)
				.ok_or_else(|| io::Error::other("sparse preload page is outside the dataset"))
		})
		.collect()
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
		println!(
			"paired_pool_raw workload={:?} throughput_ratio={:.5} p99_ratio={:.5}",
			comparison.name,
			comparison.paired_pool_raw_throughput_ratio,
			comparison.paired_pool_raw_p99_ratio,
		);
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
	if sparse.paired_pool_raw_throughput_ratio < MIN_RAW_DIRECT_RATIO {
		failures.push("single-page miss throughput/raw Direct");
	}
	if sparse.paired_pool_raw_p99_ratio > MAX_RAW_DIRECT_P99_RATIO {
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
	if pool_scan.hot_hit_ratio.unwrap_or(0.0) < MIN_SCAN_HIT_RATIO {
		failures.push("scan hot-set hit ratio");
	}
	if scan.paired_pool_raw_throughput_ratio < MIN_RAW_DIRECT_RATIO {
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
		let read_position_sum = |backend| {
			READ_BACKEND_ORDERS
				.iter()
				.map(|order| {
					order
						.iter()
						.position(|candidate| *candidate == backend)
						.map_or(0, |index| index + 1)
				})
				.sum::<usize>()
		};
		assert_eq!(
			read_position_sum(ReadBackend::RawDirect),
			read_position_sum(ReadBackend::PooledDirect)
		);

		for order in WRITE_BACKEND_ORDERS {
			for backend in WriteBackend::ALL {
				assert_eq!(order.iter().filter(|candidate| **candidate == backend).count(), 1);
			}
		}
		let write_position_sum = |backend| {
			WRITE_BACKEND_ORDERS
				.iter()
				.map(|order| {
					order
						.iter()
						.position(|candidate| *candidate == backend)
						.map_or(0, |index| index + 1)
				})
				.sum::<usize>()
		};
		assert_eq!(
			write_position_sum(WriteBackend::Direct),
			write_position_sum(WriteBackend::Mmap)
		);
	}

	#[test]
	fn direct_ratios_keep_same_repetition_pairs() -> io::Result<()> {
		let sample = |elapsed_micros, p99_micros| ReadSample {
			metrics: Metrics {
				operations: 1,
				useful_bytes: 1,
				elapsed: Duration::from_micros(elapsed_micros),
				p50: Duration::ZERO,
				p95: Duration::ZERO,
				p99: Duration::from_micros(p99_micros),
			},
			rss_delta: 0,
			hot_hits: 0,
		};
		let samples = HashMap::from([
			(ReadBackend::RawDirect, vec![sample(10, 10), sample(100, 100), sample(1_000, 1_000)]),
			(ReadBackend::PooledDirect, vec![sample(20, 20), sample(50, 50), sample(1_000, 1_000)]),
		]);
		let (throughput, p99) = paired_pool_raw_ratios(&samples)?;
		assert!((throughput - 1.0).abs() < f64::EPSILON);
		assert!((p99 - 1.0).abs() < f64::EPSILON);
		Ok(())
	}

	#[test]
	fn sparse_preload_is_disjoint_and_in_bounds() -> io::Result<()> {
		let pool_pages =
			NonZeroUsize::new(4).ok_or_else(|| io::Error::other("test pool must be non-zero"))?;
		let order = sparse_random_order(64, pool_pages)?;
		let preload = sparse_preload_order(&order, 64)?;
		assert_eq!(order.len(), 16);
		assert_eq!(preload.len(), order.len());
		assert!(preload.iter().all(|page_no| page_no.get() < 64));
		assert!(preload.iter().all(|page_no| !order.contains(page_no)));
		Ok(())
	}

	#[test]
	fn batch_orders_measure_every_candidate_once() {
		for order in BATCH_CANDIDATE_ORDERS {
			for candidate in 0..BATCH_CANDIDATE_KIB.len() {
				assert_eq!(order.iter().filter(|entry| **entry == candidate).count(), 1);
			}
		}
		let position_sum = |candidate| {
			BATCH_CANDIDATE_ORDERS
				.iter()
				.map(|order| {
					order.iter().position(|entry| *entry == candidate).map_or(0, |index| index + 1)
				})
				.sum::<usize>()
		};
		for candidate in 1..BATCH_CANDIDATE_KIB.len() {
			assert_eq!(position_sum(candidate), position_sum(0));
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
