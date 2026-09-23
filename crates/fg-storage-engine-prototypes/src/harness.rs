//! Reproducible aggregate-timing harness for storage-engine prototypes.

#[cfg(all(windows, feature = "native-async-io"))]
use std::cell::RefCell;
use std::{
	cell::Cell,
	error::Error,
	fs::{File, OpenOptions},
	hint::black_box,
	io::{self, Write},
	num::NonZeroUsize,
	path::{Path, PathBuf},
	ptr::NonNull,
	sync::{
		Arc,
		Barrier,
		atomic::{AtomicBool, AtomicU64, Ordering},
	},
	thread,
	time::{Duration, Instant},
};

#[cfg(all(windows, feature = "native-async-io"))]
use ferros_grynn::storage_engine_prototype_api::{
	direct::DirectFileOpenMode,
	page_io::{FilePageExtent, FrameRead, IoRingSubmissionStats, WindowsIoRing},
};
use ferros_grynn::{
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	storage_engine_prototype_api::{
		buffer_pool::{BufferPool, PageCacheKey, PageCacheVersion},
		direct::DirectFile,
		hybrid::{
			HybridReadEngine,
			MAX_READ_SCOPE_OPERATIONS,
			ReadScope,
			ResolvedFrameHandle,
			ResolvedPageTable,
		},
		page::{
			AlignedPage,
			PAGE_HEADER_SIZE,
			PAGE_SIZE,
			PageFlags,
			PageHeader,
			PageLsn,
			PageNo,
			PageType,
			validate_page_bytes,
		},
		types::{CheckpointGeneration, DataFileId, DataFileKind, GraphId, Lsn, PageKey},
	},
};
use memmap2::{Mmap, MmapOptions};
use parking_lot::Mutex;
use rapidhash::{HashMapExt, RapidHashMap};
use sysinfo::{ProcessesToUpdate, System, get_current_pid};
#[cfg(windows)]
use windows::Win32::{
	Foundation::FILETIME,
	System::{
		ProcessStatus::{GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS},
		Threading::{GetCurrentProcess, GetProcessTimes},
	},
};

#[cfg(all(windows, feature = "native-async-io"))]
use super::policy::VmPrototypeReadCache;
#[cfg(all(windows, feature = "native-async-io"))]
use super::windows_transport::ScatterReader;
#[cfg(windows)]
use super::windows_vm::{HomeSourceSpec, VmCache, VmResolvedHandle, probe_vm_lifecycle};
use super::{
	atomic_arena::{AtomicArenaBufferPool, CounterMode, DenseLocatorLayout, SourceBoundLocator},
	policy::{PrototypeReadCache, PrototypeScanCursor, ReadIntent, ReadPolicy},
	prefetch::PrototypePrefetchCursor,
};

/// Artifact schema revision.
const CSV_SCHEMA_VERSION: u32 = 2;
/// Default 1 GiB screening dataset.
const DEFAULT_DATASET_PAGES: u64 = 65_536;
/// Default total frame budget matching the stage-3 baseline.
const DEFAULT_POOL_PAGES: usize = 4_096;
/// Default resident operations per candidate and repetition.
const DEFAULT_RESIDENT_OPERATIONS: u64 = 2_000_000;
/// Default normalized-latency batch size.
const DEFAULT_LATENCY_BATCH: usize = 1_024;
/// Logical accesses amortized by one resident page pin.
const RESIDENT_RUN_LENGTHS: [usize; 6] = [1, 4, 16, 32, 64, 128];
/// Run lengths retained in the process-isolated finalist gate.
const RESIDENT_ISOLATED_RUN_LENGTHS: [usize; 6] = [1, 4, 16, 32, 64, 128];
/// Default Direct-I/O extent size.
const DEFAULT_EXTENT_PAGES: usize = 128;
/// Immutable bounded prefetch-depth screening order.
const PREFETCH_DEPTHS: [usize; 5] = [0, 1, 2, 4, 8];
/// Immutable admission-policy comparison order.
const READ_POLICIES: [ReadPolicy; 3] =
	[ReadPolicy::CacheAll, ReadPolicy::RocksLikeBypass, ReadPolicy::InnodbLikeCold];
/// Immutable interleaved standalone/direct-to-frame transport order.
const TRANSPORT_CANDIDATES: [&str; 13] = [
	"T0_raw",
	"atomic_scratch_copy_publish",
	"T0_validated",
	"T1_ioring_extent",
	"atomic_ioring_frame_direct",
	"T3_scatter",
	"atomic_scatter_direct",
	"vm_scatter_direct",
	"T2_ioring_frame_requested_qd1",
	"T2_ioring_frame_requested_qd8",
	"T2_ioring_frame_requested_qd32",
	"T2_ioring_frame_requested_qd128",
	"H11_production_scatter_p1",
];
/// Unique benchmark file suffix source.
static NEXT_DATASET: AtomicU64 = AtomicU64::new(1);

/// One required non-dominated/mandatory combination-screening case.
#[cfg(all(windows, feature = "native-async-io"))]
#[derive(Debug, Clone, Copy)]
enum CombinationCandidate {
	/// Raw synchronous Direct I/O control measured in the same rotation.
	RawDirect,
	/// Synchronous Direct I/O plus checksum validation control measured in the
	/// same rotation.
	ValidatedDirect,
	/// Atomic arena under one explicit admission policy.
	AtomicPolicy(ReadPolicy),
	/// Fixed-address VM cache under one explicit admission policy.
	VmPolicy(ReadPolicy),
	/// Rocks-like non-admitting scatter scan with bounded look-ahead.
	BypassScatter {
		/// Requested extent look-ahead depth.
		depth: usize,
	},
	/// InnoDB-like cold Atomic arena filled directly by scatter.
	AtomicColdScatter {
		/// Requested extent look-ahead depth.
		depth: usize,
	},
	/// InnoDB-like cold Atomic arena filled by one IoRing SQE per frame.
	AtomicColdIoRing {
		/// Requested extent look-ahead depth.
		depth: usize,
	},
}

#[cfg(all(windows, feature = "native-async-io"))]
impl CombinationCandidate {
	/// Stable raw-CSV label.
	fn label(self) -> String {
		match self {
			Self::RawDirect => "T0_raw".to_owned(),
			Self::ValidatedDirect => "T0_validated".to_owned(),
			Self::AtomicPolicy(policy) => format!("R2_atomic+{}", policy_label(policy)),
			Self::VmPolicy(policy) => format!("R4_vm+{}", policy_label(policy)),
			Self::BypassScatter {
				depth,
			} => format!("A1_rocks_bypass+T3_scatter+P{depth}"),
			Self::AtomicColdScatter {
				depth,
			} => format!("R2_atomic+A2_innodb_cold+T3_scatter+P{depth}"),
			Self::AtomicColdIoRing {
				depth,
			} => format!("R2_atomic+A2_innodb_cold+T2_frame_ioring+P{depth}"),
		}
	}
}

// Reusable scatter request state owned by one prefetch worker thread.
#[cfg(all(windows, feature = "native-async-io"))]
std::thread_local! {
	static PREFETCH_SCATTER: RefCell<Option<ScatterReader>> = const { RefCell::new(None) };
}

/// Shared inventory of preallocated extent buffers used by one prefetch scan.
struct ReusableBufferPool<Buffer> {
	/// Shared available-set stack and immutable allocation count.
	shared: Arc<ReusableBufferPoolShared<Buffer>>,
}

/// State shared between the prefetch worker and pull consumer.
struct ReusableBufferPoolShared<Buffer> {
	/// Buffers not currently owned by a worker, queue entry, or consumer.
	available: Mutex<Vec<Buffer>>,
	/// Exact number of sets allocated before timing begins.
	total_sets: usize,
}

impl<Buffer> ReusableBufferPool<Buffer> {
	/// Allocates exactly `set_count` reusable buffers through one factory.
	fn with_factory<Factory>(set_count: usize, mut factory: Factory) -> Self
	where
		Factory: FnMut() -> Buffer,
	{
		let available = (0..set_count).map(|_| factory()).collect();
		Self {
			shared: Arc::new(ReusableBufferPoolShared {
				available: Mutex::new(available),
				total_sets: set_count,
			}),
		}
	}

	/// Acquires one set; exhaustion means the configured software bound was
	/// violated.
	fn take(&self) -> FerrosGrynnResult<ReusableBufferLease<Buffer>> {
		let buffer = self.shared.available.lock().pop().ok_or_else(|| {
			FerrosGrynnError::storage_io_failure("prefetch reusable buffer pool exhausted")
		})?;
		Ok(ReusableBufferLease {
			buffer: Some(buffer),
			pool: self.clone(),
		})
	}

	/// Returns the number of sets currently available for reuse.
	fn available_sets(&self) -> usize {
		self.shared.available.lock().len()
	}

	/// Verifies that successful cursor completion returned every allocated set.
	fn ensure_all_returned(&self) -> FerrosGrynnResult<()> {
		if self.available_sets() != self.shared.total_sets {
			return Err(FerrosGrynnError::storage_io_failure(
				"prefetch cursor completed without returning every reusable buffer",
			));
		}
		Ok(())
	}
}

impl<Buffer> Clone for ReusableBufferPool<Buffer> {
	fn clone(&self) -> Self {
		Self {
			shared: Arc::clone(&self.shared),
		}
	}
}

/// RAII ownership of one reusable extent buffer.
struct ReusableBufferLease<Buffer> {
	/// Buffer returned to the pool on every drop path.
	buffer: Option<Buffer>,
	/// Pool that owns the buffer allocation.
	pool: ReusableBufferPool<Buffer>,
}

impl<Buffer> ReusableBufferLease<Buffer> {
	/// Borrows the reusable buffer for consumer validation.
	fn buffer(&self) -> FerrosGrynnResult<&Buffer> {
		self.buffer.as_ref().ok_or_else(|| {
			FerrosGrynnError::storage_io_failure("prefetch buffer lease was already returned")
		})
	}

	/// Mutably borrows the reusable buffer for transport fill.
	fn buffer_mut(&mut self) -> FerrosGrynnResult<&mut Buffer> {
		self.buffer.as_mut().ok_or_else(|| {
			FerrosGrynnError::storage_io_failure("prefetch buffer lease was already returned")
		})
	}
}

impl<Buffer> Drop for ReusableBufferLease<Buffer> {
	fn drop(&mut self) {
		if let Some(buffer) = self.buffer.take() {
			self.pool.shared.available.lock().push(buffer);
		}
	}
}

/// Environment-controlled screening configuration.
#[derive(Debug, Clone, Copy)]
struct HarnessConfig {
	/// Number of immutable pages in the generated dataset.
	dataset_pages: u64,
	/// Shared resident frame budget.
	pool_pages: usize,
	/// Resident operations per candidate.
	resident_operations: u64,
	/// Independent paired repetitions.
	repetitions: usize,
	/// Pages in one sequential transport extent.
	extent_pages: usize,
	/// Operations represented by one latency sample.
	latency_batch: usize,
	/// Whether expensive concurrency and workload sweeps run.
	full: bool,
}

impl HarnessConfig {
	/// Reads bounded overrides without changing normative production settings.
	fn from_env() -> Result<Self, Box<dyn Error>> {
		let dataset_pages = env_parse("FG_STORAGE_PROTO_PAGES", DEFAULT_DATASET_PAGES)?;
		let pool_pages = env_parse("FG_STORAGE_PROTO_POOL_PAGES", DEFAULT_POOL_PAGES)?;
		let resident_operations =
			env_parse("FG_STORAGE_PROTO_OPERATIONS", DEFAULT_RESIDENT_OPERATIONS)?;
		let repetitions = env_parse("FG_STORAGE_PROTO_REPETITIONS", 9_usize)?;
		let extent_pages = env_parse("FG_STORAGE_PROTO_EXTENT_PAGES", DEFAULT_EXTENT_PAGES)?;
		let latency_batch = env_parse("FG_STORAGE_PROTO_LATENCY_BATCH", DEFAULT_LATENCY_BATCH)?;
		let full = std::env::var("FG_STORAGE_PROTO_FULL").is_ok_and(|value| value == "1");
		if dataset_pages == 0
			|| pool_pages == 0
			|| resident_operations == 0
			|| repetitions == 0
			|| extent_pages == 0
			|| latency_batch == 0
		{
			return Err(io::Error::other("prototype harness parameters must be non-zero").into());
		}
		Ok(Self {
			dataset_pages,
			pool_pages,
			resident_operations,
			repetitions,
			extent_pages,
			latency_batch,
			full,
		})
	}
}

/// Returns the screening or finalist repetition count for I/O phases.
fn io_repetitions(config: HarnessConfig) -> usize {
	if config.full {
		config.repetitions.min(7)
	} else {
		config.repetitions.min(5)
	}
}

/// One measured aggregate and normalized batch distribution.
#[derive(Debug, Clone, Copy)]
struct Measurement {
	/// Complete measured wall time.
	elapsed: Duration,
	/// Process kernel plus user CPU time consumed during the measured wall
	/// scope.
	cpu_time: Duration,
	/// Number of logical operations.
	operations: u64,
	/// Useful bytes processed by those operations.
	useful_bytes: u64,
	/// Median normalized batch latency.
	p50_ns: u64,
	/// 95th-percentile normalized batch latency.
	p95_ns: u64,
	/// 99th-percentile normalized batch latency.
	p99_ns: u64,
	/// Process RSS delta across the candidate-specific observation scope.
	rss_delta: u64,
}

/// Aggregate software-submission accounting for one frame-IoRing scan.
#[cfg(all(windows, feature = "native-async-io"))]
#[derive(Debug, Clone, Copy)]
struct IoRingProbeStats {
	/// Runtime submission-queue capacity reported by Windows.
	submission_capacity: u32,
	/// Caller-requested SQE limit passed to every scan chunk.
	requested_sqe_batch_limit: usize,
	/// Requested limit clamped to the runtime ring capacity.
	effective_sqe_batch_limit: usize,
	/// Successful native submit calls across the scan.
	submit_calls: usize,
	/// SQEs accepted across all native submits.
	submitted_sqes: usize,
	/// CQEs drained before every chunk returned.
	completed_cqes: usize,
}

#[cfg(all(windows, feature = "native-async-io"))]
impl IoRingProbeStats {
	/// Starts accounting from the first successful measured submission.
	fn from_submission(stats: IoRingSubmissionStats) -> Self {
		Self {
			submission_capacity: stats.submission_capacity(),
			requested_sqe_batch_limit: stats.requested_sqe_batch_limit(),
			effective_sqe_batch_limit: stats.effective_sqe_batch_limit(),
			submit_calls: stats.submit_calls(),
			submitted_sqes: stats.submitted_sqes(),
			completed_cqes: stats.completed_cqes(),
		}
	}

	/// Adds one later scan chunk after checking its fixed ring limits.
	fn merge(&mut self, stats: IoRingSubmissionStats) -> Result<(), Box<dyn Error>> {
		if self.submission_capacity != stats.submission_capacity()
			|| self.requested_sqe_batch_limit != stats.requested_sqe_batch_limit()
			|| self.effective_sqe_batch_limit != stats.effective_sqe_batch_limit()
		{
			return Err(io::Error::other("IoRing submission limits changed during one scan").into());
		}
		self.submit_calls = self.submit_calls.saturating_add(stats.submit_calls());
		self.submitted_sqes = self.submitted_sqes.saturating_add(stats.submitted_sqes());
		self.completed_cqes = self.completed_cqes.saturating_add(stats.completed_cqes());
		Ok(())
	}
}

impl Measurement {
	/// Returns aggregate throughput in operations per second.
	fn operations_per_second(self) -> f64 {
		self.operations as f64 / self.elapsed.as_secs_f64()
	}

	/// Returns useful throughput in MiB/s.
	fn mib_per_second(self) -> f64 {
		self.useful_bytes as f64 / (1024.0 * 1024.0) / self.elapsed.as_secs_f64()
	}

	/// Returns process CPU time divided by measured wall time.
	fn cpu_wall_ratio(self) -> f64 {
		let wall_seconds = self.elapsed.as_secs_f64();
		if wall_seconds == 0.0 {
			return 0.0;
		}
		self.cpu_time.as_secs_f64() / wall_seconds
	}
}

/// Wall/process-CPU deltas captured around one measured aggregate scope.
#[derive(Debug, Clone, Copy)]
struct MeasurementTiming {
	/// Complete measured wall time.
	elapsed: Duration,
	/// Process kernel plus user CPU time consumed during the same scope.
	cpu_time: Duration,
}

/// Paired wall/process-CPU timer for aggregate measurements.
struct MeasurementTimer {
	/// Wall-clock start instant.
	wall_started: Instant,
	/// Process CPU time observed immediately before the wall start.
	cpu_started: Duration,
}

/// Cumulative Windows process-memory counters around one measured scan.
#[cfg(windows)]
#[derive(Debug, Clone, Copy)]
struct ProcessMemorySnapshot {
	/// Total process page faults since process start.
	page_fault_count: u32,
	/// Current resident working-set bytes.
	working_set_bytes: u64,
}

impl MeasurementTimer {
	/// Captures the process-CPU snapshot immediately before the wall start.
	fn start() -> io::Result<Self> {
		let cpu_started = process_cpu_time()?;
		Ok(Self {
			wall_started: Instant::now(),
			cpu_started,
		})
	}

	/// Captures wall and process-CPU deltas at the end of the measured scope.
	fn finish(self) -> io::Result<MeasurementTiming> {
		let elapsed = self.wall_started.elapsed();
		let cpu_finished = process_cpu_time()?;
		Ok(MeasurementTiming {
			elapsed,
			cpu_time: cpu_finished.saturating_sub(self.cpu_started),
		})
	}
}

/// Metadata accompanying one raw measurement row.
#[derive(Debug, Clone, Copy)]
struct SampleMeta<'label> {
	/// Screening phase.
	phase: &'label str,
	/// Stable workload name.
	workload: &'label str,
	/// Stable candidate name.
	candidate: &'label str,
	/// Repetition index.
	repetition: usize,
	/// Candidate order within the repetition.
	order_position: usize,
	/// Dataset pages.
	dataset_pages: u64,
	/// Total pool pages.
	pool_pages: usize,
	/// Resident hot-set pages.
	hot_pages: usize,
	/// Source-file count.
	file_count: usize,
	/// Worker count.
	threads: usize,
	/// Requested software submission batch; never inferred device occupancy.
	queue_depth: usize,
	/// Pages per transport extent.
	extent_pages: usize,
	/// Prefetch look-ahead depth.
	prefetch_depth: usize,
	/// Reserved VM address bytes.
	vm_reserved_bytes: usize,
	/// Committed VM bytes.
	vm_committed_bytes: usize,
	/// Optional hit ratio.
	hit_ratio: f64,
}

/// Append-only artifacts retained under `target/storage-engine-prototype`.
struct Artifacts {
	/// Raw CSV samples.
	csv: File,
	/// Commands, environment, skips, and human-readable rows.
	log: File,
}

impl Artifacts {
	/// Opens the stable artifact directory and writes headers once.
	fn open(config: HarnessConfig) -> io::Result<Self> {
		let directory = artifact_directory();
		std::fs::create_dir_all(&directory)?;
		let csv_path = directory.join("results.csv");
		let needs_header = !csv_path.exists() || csv_path.metadata()?.len() == 0;
		let mut csv = OpenOptions::new().create(true).append(true).open(csv_path)?;
		if needs_header {
			writeln!(
				csv,
				"schema_version,phase,workload,candidate,repetition,order_position,dataset_pages,\
				 pool_pages,hot_pages,file_count,threads,queue_depth,extent_pages,prefetch_depth,\
				 operations,useful_bytes,elapsed_ns,cpu_time_ns,cpu_wall_ratio,ops_per_second,\
				 mib_per_second,p50_ns_per_op,p95_ns_per_op,p99_ns_per_op,rss_delta_bytes,\
				 vm_reserved_bytes,vm_committed_bytes,hit_ratio,status"
			)?;
		}
		let mut log =
			OpenOptions::new().create(true).append(true).open(directory.join("run.log"))?;
		writeln!(log, "\n=== run {} ===", unixish_timestamp())?;
		writeln!(log, "config={config:?}")?;
		writeln!(
			log,
			"candidate_selector_env={}",
			std::env::var("FG_STORAGE_PROTO_ONLY_CANDIDATE")
				.unwrap_or_else(|_error| "all".to_owned())
		)?;
		write_environment(&mut log)?;
		#[cfg(windows)]
		writeln!(
			log,
			"cpu_time_contract=cpu_time_ns is the GetProcessTimes kernel+user delta over the same \
			 aggregate wall scope; cpu_wall_ratio=cpu_time_ns/elapsed_ns; VM lifecycle subphase \
			 rows record zero because their wall breakdown is produced inside the platform probe"
		)?;
		#[cfg(not(windows))]
		writeln!(
			log,
			"cpu_time_contract=unsupported on this platform; cpu_time_ns and cpu_wall_ratio are \
			 explicitly zero and must not be interpreted as idle CPU"
		)?;
		Ok(Self {
			csv,
			log,
		})
	}

	/// Appends one successful raw sample and a concise log row.
	fn sample(&mut self, meta: SampleMeta<'_>, measurement: Measurement) -> io::Result<()> {
		let row = [
			CSV_SCHEMA_VERSION.to_string(),
			meta.phase.to_owned(),
			meta.workload.to_owned(),
			meta.candidate.to_owned(),
			meta.repetition.to_string(),
			meta.order_position.to_string(),
			meta.dataset_pages.to_string(),
			meta.pool_pages.to_string(),
			meta.hot_pages.to_string(),
			meta.file_count.to_string(),
			meta.threads.to_string(),
			meta.queue_depth.to_string(),
			meta.extent_pages.to_string(),
			meta.prefetch_depth.to_string(),
			measurement.operations.to_string(),
			measurement.useful_bytes.to_string(),
			measurement.elapsed.as_nanos().to_string(),
			measurement.cpu_time.as_nanos().to_string(),
			format!("{:.6}", measurement.cpu_wall_ratio()),
			format!("{:.3}", measurement.operations_per_second()),
			format!("{:.3}", measurement.mib_per_second()),
			measurement.p50_ns.to_string(),
			measurement.p95_ns.to_string(),
			measurement.p99_ns.to_string(),
			measurement.rss_delta.to_string(),
			meta.vm_reserved_bytes.to_string(),
			meta.vm_committed_bytes.to_string(),
			format!("{:.6}", meta.hit_ratio),
			"ok".to_owned(),
		];
		writeln!(self.csv, "{}", row.join(","))?;
		writeln!(
			self.log,
			"{} {} {} rep={} {:.3} Mops/s {:.1} MiB/s p99={} ns cpu={} ns cpu/wall={:.3} rss={} B",
			meta.phase,
			meta.workload,
			meta.candidate,
			meta.repetition,
			measurement.operations_per_second() / 1_000_000.0,
			measurement.mib_per_second(),
			measurement.p99_ns,
			measurement.cpu_time.as_nanos(),
			measurement.cpu_wall_ratio(),
			measurement.rss_delta,
		)
	}

	/// Records a deliberate platform/tool skip.
	fn skip(&mut self, phase: &str, candidate: &str, reason: &str) -> io::Result<()> {
		writeln!(self.log, "SKIP phase={phase} candidate={candidate}: {reason}")
	}

	/// Records the optional candidate filter applied before fixture creation.
	fn selector(&mut self, phase: &str, candidate: Option<&str>) -> io::Result<()> {
		writeln!(
			self.log,
			"candidate_selector phase={phase} requested={}",
			candidate.unwrap_or("all")
		)
	}
}

/// Opens the compact page-fault result stream and writes its header once.
#[cfg(windows)]
fn open_page_fault_csv() -> io::Result<File> {
	let path = artifact_directory().join("page-fault-results.csv");
	let needs_header = !path.exists() || path.metadata()?.len() == 0;
	let mut csv = OpenOptions::new().create(true).append(true).open(path)?;
	if needs_header {
		writeln!(
			csv,
			"schema_version,candidate,repetition,dataset_pages,dataset_bytes,ram_bytes,\
			 dataset_ram_ratio,extent_pages,elapsed_ns,cpu_time_ns,cpu_wall_ratio,mib_per_second,\
			 page_fault_count_before,page_fault_count_after,page_fault_delta,\
			 page_faults_per_db_page,working_set_before_bytes,working_set_after_bytes,status"
		)?;
	}
	Ok(csv)
}

/// Appends one page-fault observation outside the measured scan scope.
#[cfg(windows)]
fn record_page_fault_sample(
	csv: &mut File,
	candidate: &str,
	repetition: usize,
	config: HarnessConfig,
	extent_pages: usize,
	ram_bytes: u64,
	measurement: Measurement,
	before: ProcessMemorySnapshot,
	after: ProcessMemorySnapshot,
) -> io::Result<()> {
	let dataset_bytes = config.dataset_pages.saturating_mul(PAGE_SIZE as u64);
	let page_fault_delta = after.page_fault_count.wrapping_sub(before.page_fault_count);
	writeln!(
		csv,
		"1,{candidate},{repetition},{},{dataset_bytes},{ram_bytes},{:.6},{},{},{},{:.6},{:.3},{},\
		 {},{page_fault_delta},{:.6},{},{},ok",
		config.dataset_pages,
		dataset_bytes as f64 / ram_bytes.max(1) as f64,
		extent_pages,
		measurement.elapsed.as_nanos(),
		measurement.cpu_time.as_nanos(),
		measurement.cpu_wall_ratio(),
		measurement.mib_per_second(),
		before.page_fault_count,
		after.page_fault_count,
		f64::from(page_fault_delta) / config.dataset_pages as f64,
		before.working_set_bytes,
		after.working_set_bytes,
	)
}

/// Immutable benchmark dataset removed when its harness test ends.
struct Dataset {
	/// Unique target-local file path.
	path: PathBuf,
	/// Complete page count.
	page_count: u64,
	/// Stable physical source identity used by cache keys.
	source_id: DataFileId,
	/// Headers derived from the immutable generated pages before any measured
	/// load.
	headers: Vec<PageHeader>,
}

impl Dataset {
	/// Creates and durably seals deterministic pages before any timing.
	fn create(label: &str, page_count: u64) -> Result<Self, Box<dyn Error>> {
		let suffix = NEXT_DATASET.fetch_add(1, Ordering::Relaxed);
		let path = artifact_directory()
			.join(format!("dataset-{label}-{}-{suffix}.fgd", std::process::id()));
		std::fs::create_dir_all(artifact_directory())?;
		let file = DirectFile::create_new(&path)?;
		file.resize_pages(page_count)?;
		let mut headers = Vec::with_capacity(usize::try_from(page_count)?);
		let mut first_page = 0_u64;
		while first_page < page_count {
			let count = usize::try_from((page_count - first_page).min(128))?;
			let mut pages = Vec::with_capacity(count);
			for offset in 0..count {
				let page_no = PageNo::new(first_page + offset as u64);
				let page = benchmark_page(page_no)?;
				headers.push(page.validate(page_no)?);
				pages.push(page);
			}
			file.write_pages(PageNo::new(first_page), &pages)?;
			first_page += count as u64;
		}
		file.sync_data()?;
		drop(file);
		let source_id = DataFileId::new(suffix, DataFileKind::NodesHot)
			.ok_or_else(|| io::Error::other("prototype source identity exhausted"))?;
		Ok(Self {
			path,
			page_count,
			source_id,
			headers,
		})
	}

	/// Creates the exact immutable key for one page in this dataset.
	fn key(&self, page_no: PageNo) -> PageCacheKey {
		PageCacheKey::new(PageKey::for_database(self.source_id, page_no), PageCacheVersion::Home {
			generation: CheckpointGeneration::ZERO,
			ceiling: Lsn::new(u64::MAX),
		})
	}

	/// Returns the header sealed into one immutable generated page.
	///
	/// Prototype loaders return this contract value after raw I/O so the
	/// cache's publication boundary, rather than the loader, performs the only
	/// checksum validation of the bytes that were actually read.
	fn expected_header(&self, page_no: PageNo) -> FerrosGrynnResult<PageHeader> {
		let index = usize::try_from(page_no.get()).map_err(|_source| {
			FerrosGrynnError::invalid_storage_page(
				page_no.get(),
				"prototype page number does not fit the dataset header table",
			)
		})?;
		self.headers.get(index).copied().ok_or_else(|| {
			FerrosGrynnError::invalid_storage_page(
				page_no.get(),
				"prototype page is outside the immutable dataset header table",
			)
		})
	}
}

impl Drop for Dataset {
	/// Removes only this exact generated file.
	fn drop(&mut self) {
		drop(std::fs::remove_file(&self.path));
	}
}

/// Fully prevalidated mmap benchmark control.
struct MmapControl {
	/// Immutable mapping of the generated file.
	mapping: Mmap,
	/// Number of complete pages.
	page_count: u64,
}

impl MmapControl {
	/// Maps and validates every page before resident timing.
	fn open(dataset: &Dataset) -> Result<Self, Box<dyn Error>> {
		let control = Self::map_unvalidated(dataset)?;
		control.validate_all()?;
		Ok(control)
	}

	/// Maps the immutable file without touching its page contents.
	fn map_unvalidated(dataset: &Dataset) -> Result<Self, Box<dyn Error>> {
		let file = File::open(&dataset.path)?;
		// SAFETY: the read-only file remains immutable for the mapping
		// lifetime, and `Mmap` owns the mapping independently of the local
		// file handle.
		let mapping = unsafe { MmapOptions::new().map(&file)? };
		if mapping.len() != usize::try_from(dataset.page_count)?.saturating_mul(PAGE_SIZE) {
			return Err(io::Error::other("mmap dataset length mismatch").into());
		}
		Ok(Self {
			mapping,
			page_count: dataset.page_count,
		})
	}

	/// Validates every mapped page in physical order.
	fn validate_all(&self) -> FerrosGrynnResult<()> {
		for raw_page in 0..self.page_count {
			let page_no = PageNo::new(raw_page);
			validate_page_bytes(
				mapping_page(&self.mapping, page_no).map_err(|error| {
					FerrosGrynnError::storage_io_failure(format!(
						"mmap page lookup failed: {error}"
					))
				})?,
				page_no,
			)?;
		}
		Ok(())
	}

	/// Reads one resident marker without validation or mutable metadata.
	fn marker(&self, page_no: PageNo) -> u8 {
		self.marker_at(page_no, 0)
	}

	/// Reads one resident marker at a deterministic field-like page offset.
	fn marker_at(&self, page_no: PageNo, logical_offset: usize) -> u8 {
		debug_assert!(page_no.get() < self.page_count);
		let start = page_no.get() as usize * PAGE_SIZE;
		self.mapping[start + resident_marker_offset(logical_offset)]
	}
}

/// Candidate set for the paired resident comparison.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ResidentCandidate {
	/// Empty loop/harness control.
	Harness,
	/// Fully prevalidated mmap access.
	Mmap,
	/// Current production Clock-Pro pool.
	ClockPro,
	/// Atomic arena with generic sharded lookup.
	Atomic,
	/// Atomic arena with a cached locator.
	AtomicResolved,
	/// Atomic arena with sharded hit counters enabled.
	AtomicCounted,
	/// Fixed VM home address with source lookup.
	#[cfg(windows)]
	Vm,
	/// Fixed VM home resolved address.
	#[cfg(windows)]
	VmResolved,
}

impl ResidentCandidate {
	/// Stable CSV label.
	const fn label(self) -> &'static str {
		match self {
			Self::Harness => "harness",
			Self::Mmap => "R0_mmap",
			Self::ClockPro => "R1_clockpro",
			Self::Atomic => "R2_atomic",
			Self::AtomicResolved => "R3_atomic_resolved",
			Self::AtomicCounted => "R2_atomic_counted",
			#[cfg(windows)]
			Self::Vm => "R4_vm",
			#[cfg(windows)]
			Self::VmResolved => "R5_vm_resolved",
		}
	}
}

/// Candidates for the source-bound locator and batch-pin experiment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ResidentNextCandidate {
	/// Direct pointer access to the prevalidated mapping.
	Mmap,
	/// Cached frame handle with a pin and drop for every logical access.
	ResolvedPerAccess,
	/// Flat source-bound locator with a pin and drop for every access.
	DenseFlatPerAccess,
	/// Sparse two-level locator with a pin and drop for every access.
	DenseTwoLevelPerAccess,
	/// Cached frame handle pinned once for a physical run.
	ResolvedRun,
	/// Flat source-bound locator pinned once for a physical run.
	DenseFlatRun,
	/// Sparse two-level locator pinned once for a physical run.
	DenseTwoLevelRun,
	/// Cached frame handle borrowed inside one cursor-scoped read epoch.
	EpochResolved,
	/// Flat source-bound locator borrowed inside one cursor-scoped read epoch.
	EpochDenseFlat,
}

impl ResidentNextCandidate {
	/// Stable CSV and profiler label.
	const fn label(self) -> &'static str {
		match self {
			Self::Mmap => "R0_mmap",
			Self::ResolvedPerAccess => "R3_resolved_per_access",
			Self::DenseFlatPerAccess => "R6_dense_flat_per_access",
			Self::DenseTwoLevelPerAccess => "R7_dense_two_level_per_access",
			Self::ResolvedRun => "R3_resolved_run",
			Self::DenseFlatRun => "R6_dense_flat_run",
			Self::DenseTwoLevelRun => "R7_dense_two_level_run",
			Self::EpochResolved => "R8_epoch_resolved",
			Self::EpochDenseFlat => "R8_epoch_dense_flat",
		}
	}
}

/// Stable order used for paired resident-next measurements.
const RESIDENT_NEXT_CANDIDATES: [ResidentNextCandidate; 9] = [
	ResidentNextCandidate::Mmap,
	ResidentNextCandidate::ResolvedPerAccess,
	ResidentNextCandidate::DenseFlatPerAccess,
	ResidentNextCandidate::DenseTwoLevelPerAccess,
	ResidentNextCandidate::ResolvedRun,
	ResidentNextCandidate::DenseFlatRun,
	ResidentNextCandidate::DenseTwoLevelRun,
	ResidentNextCandidate::EpochResolved,
	ResidentNextCandidate::EpochDenseFlat,
];

/// Candidate fixtures permitted in the process-isolated crossover probe.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ResidentIsolatedCandidate {
	/// Fully prevalidated mapping without an arena fixture in the process.
	Mmap,
	/// Production-candidate arena with one ordinary pin per physical page.
	ProductionGeneric,
	/// Production-candidate arena borrowed through its bounded read scope.
	ProductionScope,
	/// Production-candidate arena resolved once per bounded read window.
	ProductionWindow,
	/// Independent resolved pins with the original guard-side notification.
	NotifyResolved,
	/// Flat source-bound locator without a mapping fixture in the process.
	DenseFlatRun,
	/// Cached frame handles borrowed inside one cursor-scoped read epoch.
	EpochResolved,
	/// Flat source-bound locator borrowed inside one cursor-scoped read epoch.
	EpochDenseFlat,
	/// Page addresses resolved once and retained for the whole read epoch.
	EpochPageTable,
	/// Independent resolved pins with capacity waiting moved to the miss side.
	MissBackoffResolved,
}

impl ResidentIsolatedCandidate {
	/// Stable raw-CSV and selector label.
	const fn label(self) -> &'static str {
		match self {
			Self::Mmap => "R0_mmap",
			Self::ProductionGeneric => "H11_production_r8_generic",
			Self::ProductionScope => "H11_production_r8_scope",
			Self::ProductionWindow => "H11_production_r8_window",
			Self::NotifyResolved => "R3_notify_resolved",
			Self::DenseFlatRun => "R6_dense_flat_run",
			Self::EpochResolved => "R8_epoch_resolved",
			Self::EpochDenseFlat => "R8_epoch_dense_flat",
			Self::EpochPageTable => "R8_epoch_page_table",
			Self::MissBackoffResolved => "R9_miss_backoff_resolved",
		}
	}
}

/// Exact process-isolated candidates; all other resident variants remain in
/// the combined exploratory probe.
const RESIDENT_ISOLATED_CANDIDATES: [ResidentIsolatedCandidate; 10] = [
	ResidentIsolatedCandidate::Mmap,
	ResidentIsolatedCandidate::ProductionGeneric,
	ResidentIsolatedCandidate::ProductionScope,
	ResidentIsolatedCandidate::ProductionWindow,
	ResidentIsolatedCandidate::NotifyResolved,
	ResidentIsolatedCandidate::DenseFlatRun,
	ResidentIsolatedCandidate::EpochResolved,
	ResidentIsolatedCandidate::EpochDenseFlat,
	ResidentIsolatedCandidate::EpochPageTable,
	ResidentIsolatedCandidate::MissBackoffResolved,
];

/// Requires one exact fixture selector so one process never constructs both
/// resident engines under comparison.
fn require_resident_isolated_candidate(
	requested: Option<&str>,
) -> io::Result<ResidentIsolatedCandidate> {
	let requested = requested.ok_or_else(|| {
		io::Error::other(
			"FG_STORAGE_PROTO_ONLY_CANDIDATE is required for the isolated resident probe",
		)
	})?;
	RESIDENT_ISOLATED_CANDIDATES
		.iter()
		.copied()
		.find(|candidate| candidate.label() == requested)
		.ok_or_else(|| io::Error::other("isolated resident candidate was not validated"))
}

/// Memory attributed to one candidate's fixture construction and preload,
/// including resolved handles when that candidate owns them.
#[derive(Debug, Clone, Copy, Default)]
struct CandidateFootprint {
	/// Process resident-byte increase.
	rss_delta: u64,
	/// Fixed virtual-address bytes reserved by a VM candidate.
	vm_reserved_bytes: usize,
	/// Physical bytes committed by a VM candidate.
	vm_committed_bytes: usize,
}

impl CandidateFootprint {
	/// Adds one separately measured resident allocation while preserving VM
	/// accounting from the shared base cache.
	const fn with_additional_rss(mut self, additional_rss: u64) -> Self {
		self.rss_delta = self.rss_delta.saturating_add(additional_rss);
		self
	}
}

/// Returns the platform-applicable resident candidate set in stable order.
fn resident_candidates() -> Vec<ResidentCandidate> {
	let mut candidates = vec![
		ResidentCandidate::Harness,
		ResidentCandidate::Mmap,
		ResidentCandidate::ClockPro,
		ResidentCandidate::Atomic,
		ResidentCandidate::AtomicResolved,
		ResidentCandidate::AtomicCounted,
	];
	#[cfg(windows)]
	{
		candidates.push(ResidentCandidate::Vm);
		candidates.push(ResidentCandidate::VmResolved);
	}
	candidates
}

/// Captures an RSS delta around complete fixture construction and preload.
fn build_resident_fixture<Fixture, Build>(build: Build) -> Result<(Fixture, u64), Box<dyn Error>>
where
	Build: FnOnce() -> Result<Fixture, Box<dyn Error>>,
{
	let rss_before = process_rss()?;
	let fixture = build()?;
	let rss_after = process_rss()?;
	Ok((fixture, rss_after.saturating_sub(rss_before)))
}

/// Measures the resident engines with aggregate timing and batch percentiles.
#[test]
#[ignore = "storage-engine prototype benchmark; run explicitly in release/bench profile"]
fn storage_engine_prototype_resident_probe() -> Result<(), Box<dyn Error>> {
	let config = HarnessConfig::from_env()?;
	let mut artifacts = Artifacts::open(config)?;
	let mut candidates = resident_candidates();
	let supported = candidates.iter().map(|candidate| candidate.label()).collect::<Vec<_>>();
	let selector = candidate_selector_from_env("resident", &supported)?;
	if let Some(selected) = selector.as_deref() {
		candidates.retain(|candidate| candidate.label() == selected);
	}
	artifacts.selector("resident", selector.as_deref())?;
	writeln!(artifacts.log, "resident_fixture_isolation candidate_only={}", selector.is_some())?;
	let dataset = Dataset::create("resident", config.dataset_pages)?;
	let direct = DirectFile::open_read_only(&dataset.path)?;
	let hot_pages = config.pool_pages.min(usize::try_from(config.dataset_pages)?);
	let capacity = NonZeroUsize::new(config.pool_pages).ok_or("zero pool")?;

	let mmap = if candidates.contains(&ResidentCandidate::Mmap) {
		let (control, rss_delta) = build_resident_fixture(|| MmapControl::open(&dataset))?;
		Some((control, CandidateFootprint {
			rss_delta,
			..CandidateFootprint::default()
		}))
	} else {
		None
	};
	let clock = if candidates.contains(&ResidentCandidate::ClockPro) {
		let (pool, rss_delta) = build_resident_fixture(|| {
			let pool = BufferPool::new(capacity);
			for raw_page in 0..hot_pages {
				let page_no = PageNo::new(raw_page as u64);
				drop(pool.get_or_load(dataset.key(page_no), |page| {
					read_validated(&direct, page_no, page)
				})?);
			}
			Ok(pool)
		})?;
		Some((pool, CandidateFootprint {
			rss_delta,
			..CandidateFootprint::default()
		}))
	} else {
		None
	};
	let needs_atomic = candidates.iter().any(|candidate| {
		matches!(candidate, ResidentCandidate::Atomic | ResidentCandidate::AtomicResolved)
	});
	let atomic = if needs_atomic {
		let (pool, rss_delta) = build_resident_fixture(|| {
			let pool = AtomicArenaBufferPool::new(capacity);
			for raw_page in 0..hot_pages {
				let page_no = PageNo::new(raw_page as u64);
				drop(pool.get_or_load(dataset.key(page_no), |page| {
					read_raw_reported(&dataset, &direct, page_no, page)
				})?);
			}
			Ok(pool)
		})?;
		let base_footprint = CandidateFootprint {
			rss_delta,
			..CandidateFootprint::default()
		};
		let (handles, resolved_footprint) =
			if candidates.contains(&ResidentCandidate::AtomicResolved) {
				let (handles, handles_rss_delta) = build_resident_fixture(|| {
					Ok((0..hot_pages)
						.map(|raw_page| {
							pool.resolve(dataset.key(PageNo::new(raw_page as u64)))
								.ok_or_else(|| io::Error::other("atomic preload did not resolve"))
						})
						.collect::<io::Result<Vec<_>>>()?)
				})?;
				(handles, base_footprint.with_additional_rss(handles_rss_delta))
			} else {
				(Vec::new(), base_footprint)
			};
		Some((pool, handles, base_footprint, resolved_footprint))
	} else {
		None
	};
	let counted = if candidates.contains(&ResidentCandidate::AtomicCounted) {
		let (pool, rss_delta) = build_resident_fixture(|| {
			let pool = AtomicArenaBufferPool::with_counters(capacity, CounterMode::Sharded);
			for raw_page in 0..hot_pages {
				let page_no = PageNo::new(raw_page as u64);
				drop(pool.get_or_load(dataset.key(page_no), |page| {
					read_raw_reported(&dataset, &direct, page_no, page)
				})?);
			}
			Ok(pool)
		})?;
		Some((pool, CandidateFootprint {
			rss_delta,
			..CandidateFootprint::default()
		}))
	} else {
		None
	};

	#[cfg(windows)]
	let vm = if candidates
		.iter()
		.any(|candidate| matches!(candidate, ResidentCandidate::Vm | ResidentCandidate::VmResolved))
	{
		let (cache, rss_delta) = build_resident_fixture(|| {
			let source = HomeSourceSpec::from_key(
				dataset.key(PageNo::new(0)),
				NonZeroUsize::new(usize::try_from(dataset.page_count)?).ok_or("empty dataset")?,
			)?;
			let cache = VmCache::new(&[source], capacity, 0)?;
			for raw_page in 0..hot_pages {
				let page_no = PageNo::new(raw_page as u64);
				drop(cache.get_or_load(dataset.key(page_no), |page| {
					read_raw_reported(&dataset, &direct, page_no, page)
				})?);
			}
			Ok(cache)
		})?;
		let base_footprint = CandidateFootprint {
			rss_delta,
			vm_reserved_bytes: cache.reserved_bytes(),
			vm_committed_bytes: cache.committed_bytes(),
		};
		let (handles, resolved_footprint) = if candidates.contains(&ResidentCandidate::VmResolved) {
			let (handles, handles_rss_delta) = build_resident_fixture(|| {
				Ok((0..hot_pages)
					.map(|raw_page| {
						cache
							.resolve(dataset.key(PageNo::new(raw_page as u64)))
							.ok_or_else(|| io::Error::other("VM preload did not resolve"))
					})
					.collect::<io::Result<Vec<_>>>()?)
			})?;
			(handles, base_footprint.with_additional_rss(handles_rss_delta))
		} else {
			(Vec::new(), base_footprint)
		};
		Some((cache, handles, base_footprint, resolved_footprint))
	} else {
		None
	};

	let candidate_order = candidates.clone();
	for repetition in 0..config.repetitions {
		let mut repetition_candidates = candidate_order.clone();
		let candidate_count = repetition_candidates.len();
		repetition_candidates.rotate_left(repetition % candidate_count);
		for (order_position, candidate) in repetition_candidates.iter().copied().enumerate() {
			let (mut measurement, footprint) = match candidate {
				ResidentCandidate::Harness => measure_operations(
					config.resident_operations,
					config.latency_batch,
					0,
					|operation| {
						black_box(operation);
						Ok(())
					},
				)
				.map(|measurement| (measurement, CandidateFootprint::default()))?,
				ResidentCandidate::Mmap => {
					let (mmap, footprint) = mmap
						.as_ref()
						.ok_or_else(|| io::Error::other("mmap resident fixture is absent"))?;
					measure_operations(
						config.resident_operations,
						config.latency_batch,
						PAGE_SIZE as u64,
						|operation| {
							black_box(mmap.marker(resident_page(operation, hot_pages)));
							Ok(())
						},
					)
					.map(|measurement| (measurement, *footprint))?
				}
				ResidentCandidate::ClockPro => {
					let (clock, footprint) = clock
						.as_ref()
						.ok_or_else(|| io::Error::other("Clock-Pro resident fixture is absent"))?;
					measure_operations(
						config.resident_operations,
						config.latency_batch,
						PAGE_SIZE as u64,
						|operation| {
							let page_no = resident_page(operation, hot_pages);
							let guard = clock.get_or_load(dataset.key(page_no), |_| {
								Err(FerrosGrynnError::storage_io_failure("Clock-Pro resident miss"))
							})?;
							black_box(guard.as_bytes()[PAGE_HEADER_SIZE]);
							Ok(())
						},
					)
					.map(|measurement| (measurement, *footprint))?
				}
				ResidentCandidate::Atomic => {
					let (atomic, _handles, footprint, _resolved_footprint) = atomic
						.as_ref()
						.ok_or_else(|| io::Error::other("atomic resident fixture is absent"))?;
					measure_operations(
						config.resident_operations,
						config.latency_batch,
						PAGE_SIZE as u64,
						|operation| {
							let page_no = resident_page(operation, hot_pages);
							let guard = atomic.get_or_load(dataset.key(page_no), |_| {
								Err(FerrosGrynnError::storage_io_failure("atomic resident miss"))
							})?;
							black_box(guard.as_bytes()[PAGE_HEADER_SIZE]);
							Ok(())
						},
					)
					.map(|measurement| (measurement, *footprint))?
				}
				ResidentCandidate::AtomicResolved => {
					let (atomic, atomic_handles, _footprint, resolved_footprint) = atomic
						.as_ref()
						.ok_or_else(|| io::Error::other("resolved atomic fixture is absent"))?;
					measure_operations(
						config.resident_operations,
						config.latency_batch,
						PAGE_SIZE as u64,
						|operation| {
							let index = resident_page(operation, hot_pages).get() as usize;
							let guard =
								atomic.get_resolved(&atomic_handles[index])?.ok_or_else(|| {
									FerrosGrynnError::storage_io_failure("stale atomic handle")
								})?;
							black_box(guard.as_bytes()[PAGE_HEADER_SIZE]);
							Ok(())
						},
					)
					.map(|measurement| (measurement, *resolved_footprint))?
				}
				ResidentCandidate::AtomicCounted => {
					let (counted, footprint) = counted
						.as_ref()
						.ok_or_else(|| io::Error::other("counted atomic fixture is absent"))?;
					measure_operations(
						config.resident_operations,
						config.latency_batch,
						PAGE_SIZE as u64,
						|operation| {
							let page_no = resident_page(operation, hot_pages);
							let guard = counted.get_or_load(dataset.key(page_no), |_| {
								Err(FerrosGrynnError::storage_io_failure("counted resident miss"))
							})?;
							black_box(guard.as_bytes()[PAGE_HEADER_SIZE]);
							Ok(())
						},
					)
					.map(|measurement| (measurement, *footprint))?
				}
				#[cfg(windows)]
				ResidentCandidate::Vm => {
					let (vm, _handles, footprint, _resolved_footprint) = vm
						.as_ref()
						.ok_or_else(|| io::Error::other("VM resident fixture is absent"))?;
					measure_operations(
						config.resident_operations,
						config.latency_batch,
						PAGE_SIZE as u64,
						|operation| {
							let page_no = resident_page(operation, hot_pages);
							let guard = vm.get_or_load(dataset.key(page_no), |_| {
								Err(FerrosGrynnError::storage_io_failure("VM resident miss"))
							})?;
							black_box(guard.as_bytes()[PAGE_HEADER_SIZE]);
							Ok(())
						},
					)
					.map(|measurement| (measurement, *footprint))?
				}
				#[cfg(windows)]
				ResidentCandidate::VmResolved => {
					let (vm, vm_handles, _footprint, resolved_footprint) = vm
						.as_ref()
						.ok_or_else(|| io::Error::other("resolved VM fixture is absent"))?;
					measure_operations(
						config.resident_operations,
						config.latency_batch,
						PAGE_SIZE as u64,
						|operation| {
							let index = resident_page(operation, hot_pages).get() as usize;
							let guard = vm.get_resolved(&vm_handles[index])?.ok_or_else(|| {
								FerrosGrynnError::storage_io_failure("stale VM handle")
							})?;
							black_box(guard.as_bytes()[PAGE_HEADER_SIZE]);
							Ok(())
						},
					)
					.map(|measurement| (measurement, *resolved_footprint))?
				}
			};
			measurement.rss_delta = footprint.rss_delta;
			artifacts.sample(
				SampleMeta {
					phase: "resident",
					workload: "uniform_hot_set",
					candidate: candidate.label(),
					repetition,
					order_position,
					dataset_pages: config.dataset_pages,
					pool_pages: config.pool_pages,
					hot_pages,
					file_count: 1,
					threads: 1,
					queue_depth: 0,
					extent_pages: 1,
					prefetch_depth: 0,
					vm_reserved_bytes: footprint.vm_reserved_bytes,
					vm_committed_bytes: footprint.vm_committed_bytes,
					hit_ratio: 1.0,
				},
				measurement,
			)?;
		}
	}

	#[cfg(all(windows, target_arch = "x86_64"))]
	if let Some((atomic, handles, _footprint, _resolved_footprint)) =
		atomic.as_ref().filter(|(_pool, handles, _, _)| {
			!handles.is_empty() && candidates.contains(&ResidentCandidate::AtomicResolved)
		}) {
		match tsc_resolved_probe(atomic, handles, hot_pages, &mut artifacts) {
			Ok(true) => {}
			Ok(false) => artifacts.skip(
				"resident",
				"tsc",
				"RDTSCP, invariant TSC, or CPU affinity unavailable",
			)?,
			Err(error) => artifacts.skip("resident", "tsc", &error.to_string())?,
		}
	}
	if config.full {
		run_resident_concurrency(
			config,
			&dataset,
			&candidates,
			mmap.as_ref().map(|(fixture, _footprint)| fixture),
			clock.as_ref().map(|(fixture, _footprint)| fixture),
			atomic.as_ref().map(|(fixture, _handles, _footprint, _resolved)| fixture),
			atomic.as_ref().map_or(&[], |(_fixture, handles, _footprint, _resolved)| handles),
			#[cfg(windows)]
			vm.as_ref().map(|(fixture, _handles, _footprint, _resolved)| fixture),
			#[cfg(windows)]
			vm.as_ref().map_or(&[], |(_fixture, handles, _footprint, _resolved)| handles),
			&mut artifacts,
		)?;
	}
	writeln!(
		artifacts.log,
		"resident_rss_scope=before shared engine construction through completed preload; R3/R5 \
		 additionally add the separately sampled process RSS delta of their resolved handle Vec; \
		 CSV rss_delta_bytes is this candidate footprint rather than the later steady-state \
		 timing-block delta; R1 Clock-Pro validates inside its loader because that production API \
		 owns no later checksum boundary, while prototype Atomic/VM loaders return immutable \
		 Dataset headers and publication performs the only validation of bytes read"
	)?;
	Ok(())
}

/// Compares compact source-local resolution and one-pin physical runs against
/// the existing resolved handle and mmap control.
#[test]
#[ignore = "storage-engine prototype benchmark; run explicitly in release/bench profile"]
fn storage_engine_prototype_resident_next_probe() -> Result<(), Box<dyn Error>> {
	let config = HarnessConfig::from_env()?;
	let mut artifacts = Artifacts::open(config)?;
	let supported =
		RESIDENT_NEXT_CANDIDATES.iter().map(|candidate| candidate.label()).collect::<Vec<_>>();
	let selector = candidate_selector_from_env("resident_next", &supported)?;
	let mut candidates = RESIDENT_NEXT_CANDIDATES.to_vec();
	if let Some(selected) = selector.as_deref() {
		candidates.retain(|candidate| candidate.label() == selected);
	}
	artifacts.selector("resident_next", selector.as_deref())?;

	let dataset = Dataset::create("resident-next", config.dataset_pages)?;
	let direct = DirectFile::open_read_only(&dataset.path)?;
	let hot_pages = config.pool_pages.min(usize::try_from(config.dataset_pages)?);
	let capacity = NonZeroUsize::new(hot_pages).ok_or("zero resident hot set")?;
	let source_pages =
		NonZeroUsize::new(usize::try_from(dataset.page_count)?).ok_or("zero source pages")?;
	let mmap = MmapControl::open(&dataset)?;
	let pool = AtomicArenaBufferPool::new(capacity);
	for raw_page in 0..hot_pages {
		let page_no = PageNo::new(raw_page as u64);
		drop(pool.get_or_load(dataset.key(page_no), |page| {
			read_raw_reported(&dataset, &direct, page_no, page)
		})?);
	}

	let handles_rss_before = process_rss()?;
	let handles_timer = MeasurementTimer::start()?;
	let handles = (0..hot_pages)
		.map(|raw_page| {
			pool.resolve(dataset.key(PageNo::new(raw_page as u64)))
				.ok_or_else(|| io::Error::other("resident page did not resolve"))
		})
		.collect::<io::Result<Vec<_>>>()?;
	let handles_timing = handles_timer.finish()?;
	let handles_rss = process_rss()?.saturating_sub(handles_rss_before);
	artifacts.sample(
		resident_next_meta(
			"resident_next",
			"resolve_once",
			"R3_resolved_handle_build",
			0,
			0,
			config,
			hot_pages,
			1,
		),
		aggregate_measurement(handles_timing, hot_pages as u64, 0, handles_rss),
	)?;

	let flat_rss_before = process_rss()?;
	let flat_timer = MeasurementTimer::start()?;
	let flat = SourceBoundLocator::new(
		&pool,
		dataset.key(PageNo::new(0)),
		source_pages,
		DenseLocatorLayout::Flat,
	)?;
	for raw_page in 0..hot_pages {
		if !flat.refresh(&pool, dataset.key(PageNo::new(raw_page as u64))) {
			return Err(io::Error::other("flat dense locator failed to resolve preload").into());
		}
	}
	let flat_timing = flat_timer.finish()?;
	let flat_rss = process_rss()?.saturating_sub(flat_rss_before);
	artifacts.sample(
		resident_next_meta(
			"resident_next",
			"resolve_once",
			"R6_dense_flat_build",
			0,
			0,
			config,
			hot_pages,
			1,
		),
		aggregate_measurement(flat_timing, hot_pages as u64, 0, flat_rss),
	)?;

	let two_level_rss_before = process_rss()?;
	let two_level_timer = MeasurementTimer::start()?;
	let two_level = SourceBoundLocator::new(
		&pool,
		dataset.key(PageNo::new(0)),
		source_pages,
		DenseLocatorLayout::TwoLevel,
	)?;
	for raw_page in 0..hot_pages {
		if !two_level.refresh(&pool, dataset.key(PageNo::new(raw_page as u64))) {
			return Err(io::Error::other("two-level locator failed to resolve preload").into());
		}
	}
	let two_level_timing = two_level_timer.finish()?;
	let two_level_rss = process_rss()?.saturating_sub(two_level_rss_before);
	artifacts.sample(
		resident_next_meta(
			"resident_next",
			"resolve_once",
			"R7_dense_two_level_build",
			0,
			0,
			config,
			hot_pages,
			1,
		),
		aggregate_measurement(two_level_timing, hot_pages as u64, 0, two_level_rss),
	)?;

	for run_length in RESIDENT_RUN_LENGTHS {
		for repetition in 0..config.repetitions {
			for (order_position, candidate) in
				rotated_order(&candidates, repetition + run_length).into_iter().enumerate()
			{
				let measurement = measure_resident_next_candidate(
					candidate, config, run_length, hot_pages, &dataset, &mmap, &pool, &handles,
					&flat, &two_level,
				)?;
				artifacts.sample(
					resident_next_meta(
						"resident_next",
						"warm_physical_run",
						candidate.label(),
						repetition,
						order_position,
						config,
						hot_pages,
						run_length,
					),
					measurement,
				)?;
			}
		}
	}
	writeln!(
		artifacts.log,
		"resident_next_contract=operations are logical field/edge marker reads; extent_pages \
		 records the logical accesses sharing one physical page; per_access candidates pin/drop \
		 each marker; run candidates reuse one existing AtomicPageGuard for the whole page run"
	)?;
	Ok(())
}

/// Builds one resident-next metadata row without changing the shared CSV
/// schema.
fn resident_next_meta<'label>(
	phase: &'label str,
	workload: &'label str,
	candidate: &'label str,
	repetition: usize,
	order_position: usize,
	config: HarnessConfig,
	hot_pages: usize,
	run_length: usize,
) -> SampleMeta<'label> {
	SampleMeta {
		phase,
		workload,
		candidate,
		repetition,
		order_position,
		dataset_pages: config.dataset_pages,
		pool_pages: config.pool_pages,
		hot_pages,
		file_count: 1,
		threads: 1,
		queue_depth: 0,
		extent_pages: run_length,
		prefetch_depth: 0,
		vm_reserved_bytes: 0,
		vm_committed_bytes: 0,
		hit_ratio: 1.0,
	}
}

/// Measures one resident-next candidate while keeping logical operation count
/// identical across per-access and one-pin-run variants.
#[allow(clippy::too_many_arguments, reason = "benchmark controls remain explicit and monomorphic")]
fn measure_resident_next_candidate(
	candidate: ResidentNextCandidate,
	config: HarnessConfig,
	run_length: usize,
	hot_pages: usize,
	dataset: &Dataset,
	mmap: &MmapControl,
	pool: &AtomicArenaBufferPool,
	handles: &[super::atomic_arena::ResolvedFrameHandle],
	flat: &SourceBoundLocator,
	two_level: &SourceBoundLocator,
) -> Result<Measurement, Box<dyn Error>> {
	let epoch = if matches!(
		candidate,
		ResidentNextCandidate::EpochResolved | ResidentNextCandidate::EpochDenseFlat
	) {
		Some(pool.read_epoch()?)
	} else {
		None
	};
	measure_resident_runs(
		config.resident_operations,
		config.latency_batch,
		run_length,
		|run, logical_count| {
			let page_no = resident_page(run, hot_pages);
			let index = page_no.get() as usize;
			let key = dataset.key(page_no);
			match candidate {
				ResidentNextCandidate::Mmap => {
					for logical in 0..logical_count {
						black_box(mmap.marker_at(page_no, logical));
					}
				}
				ResidentNextCandidate::ResolvedPerAccess => {
					for logical in 0..logical_count {
						let guard = pool.get_resolved(&handles[index])?.ok_or_else(|| {
							FerrosGrynnError::storage_io_failure("resolved handle became stale")
						})?;
						black_box(guard.as_bytes()[resident_marker_offset(logical)]);
					}
				}
				ResidentNextCandidate::DenseFlatPerAccess => {
					for logical in 0..logical_count {
						let guard = flat.get_resolved(pool, key)?.ok_or_else(|| {
							FerrosGrynnError::storage_io_failure("flat locator became stale")
						})?;
						black_box(guard.as_bytes()[resident_marker_offset(logical)]);
					}
				}
				ResidentNextCandidate::DenseTwoLevelPerAccess => {
					for logical in 0..logical_count {
						let guard = two_level.get_resolved(pool, key)?.ok_or_else(|| {
							FerrosGrynnError::storage_io_failure("two-level locator became stale")
						})?;
						black_box(guard.as_bytes()[resident_marker_offset(logical)]);
					}
				}
				ResidentNextCandidate::ResolvedRun => {
					let guard = pool.get_resolved(&handles[index])?.ok_or_else(|| {
						FerrosGrynnError::storage_io_failure("resolved run handle became stale")
					})?;
					for logical in 0..logical_count {
						black_box(guard.as_bytes()[resident_marker_offset(logical)]);
					}
				}
				ResidentNextCandidate::DenseFlatRun => {
					let guard = flat.get_resolved(pool, key)?.ok_or_else(|| {
						FerrosGrynnError::storage_io_failure("flat run locator became stale")
					})?;
					for logical in 0..logical_count {
						black_box(guard.as_bytes()[resident_marker_offset(logical)]);
					}
				}
				ResidentNextCandidate::DenseTwoLevelRun => {
					let guard = two_level.get_resolved(pool, key)?.ok_or_else(|| {
						FerrosGrynnError::storage_io_failure("two-level run locator became stale")
					})?;
					for logical in 0..logical_count {
						black_box(guard.as_bytes()[resident_marker_offset(logical)]);
					}
				}
				ResidentNextCandidate::EpochResolved => {
					let epoch = epoch.as_ref().ok_or_else(|| {
						FerrosGrynnError::storage_io_failure("resolved epoch was not entered")
					})?;
					let page = epoch.get_resolved(&handles[index]).ok_or_else(|| {
						FerrosGrynnError::storage_io_failure("epoch resolved handle became stale")
					})?;
					for logical in 0..logical_count {
						black_box(page.as_bytes()[resident_marker_offset(logical)]);
					}
				}
				ResidentNextCandidate::EpochDenseFlat => {
					let epoch = epoch.as_ref().ok_or_else(|| {
						FerrosGrynnError::storage_io_failure("dense epoch was not entered")
					})?;
					let page = flat.get_epoch_resolved(epoch, key).ok_or_else(|| {
						FerrosGrynnError::storage_io_failure("epoch flat locator became stale")
					})?;
					for logical in 0..logical_count {
						black_box(page.as_bytes()[resident_marker_offset(logical)]);
					}
				}
			}
			Ok(())
		},
	)
}

/// Measures one resident finalist in a process that constructs only its
/// selected engine fixture. External orchestration supplies paired
/// repetition/order indices.
#[test]
#[ignore = "storage-engine prototype benchmark; run explicitly with --profile profiling"]
fn storage_engine_prototype_resident_isolated_process_probe() -> Result<(), Box<dyn Error>> {
	let config = HarnessConfig::from_env()?;
	let supported =
		RESIDENT_ISOLATED_CANDIDATES.iter().map(|candidate| candidate.label()).collect::<Vec<_>>();
	let selector = candidate_selector_from_env("resident_isolated_process", &supported)?;
	let candidate = require_resident_isolated_candidate(selector.as_deref())?;
	let repetition_base = env_parse("FG_STORAGE_PROTO_REPETITION_BASE", 0_usize)?;
	let order_position = env_parse("FG_STORAGE_PROTO_ORDER_POSITION", 0_usize)?;
	let mut artifacts = Artifacts::open(config)?;
	artifacts.selector("resident_isolated_process", selector.as_deref())?;
	let affinity_mask = env_parse("FG_STORAGE_PROTO_AFFINITY_MASK", 1_usize)?;
	#[cfg(windows)]
	let _affinity = pin_current_thread(affinity_mask)?;
	writeln!(
		artifacts.log,
		"resident_isolated_process candidate={} repetition_base={} order_position={} \
		 fixture_scope=selected engine only affinity_mask={affinity_mask}",
		candidate.label(),
		repetition_base,
		order_position,
	)?;

	let dataset = Dataset::create("resident-isolated-process", config.dataset_pages)?;
	let hot_pages = config.pool_pages.min(usize::try_from(config.dataset_pages)?);
	match candidate {
		ResidentIsolatedCandidate::Mmap => {
			let mmap = MmapControl::open(&dataset)?;
			record_isolated_resident_runs(
				&mut artifacts,
				config,
				candidate,
				hot_pages,
				repetition_base,
				order_position,
				|run, logical_count| {
					let page_no = resident_page(run, hot_pages);
					for logical in 0..logical_count {
						black_box(mmap.marker_at(page_no, logical));
					}
					Ok(())
				},
			)?;
		}
		ResidentIsolatedCandidate::ProductionGeneric
		| ResidentIsolatedCandidate::ProductionScope
		| ResidentIsolatedCandidate::ProductionWindow => {
			let direct = DirectFile::open_read_only(&dataset.path)?;
			let capacity = NonZeroUsize::new(hot_pages).ok_or("zero resident hot set")?;
			let engine = HybridReadEngine::new(capacity);
			for raw_page in 0..hot_pages {
				let page_no = PageNo::new(raw_page as u64);
				drop(engine.get_or_load(dataset.key(page_no), |page| {
					read_raw_reported(&dataset, &direct, page_no, page)
				})?);
			}
			let handles = (0..hot_pages)
				.map(|raw_page| {
					engine
						.resolve(dataset.key(PageNo::new(raw_page as u64)))
						.ok_or_else(|| io::Error::other("production page did not resolve"))
				})
				.collect::<io::Result<Vec<_>>>()?;
			match candidate {
				ResidentIsolatedCandidate::ProductionGeneric => {
					record_isolated_resident_runs(
						&mut artifacts,
						config,
						candidate,
						hot_pages,
						repetition_base,
						order_position,
						|run, logical_count| {
							let page_no = resident_page(run, hot_pages);
							let guard =
								engine.get_if_resident(dataset.key(page_no))?.ok_or_else(|| {
									FerrosGrynnError::storage_io_failure(
										"production generic resident page became stale",
									)
								})?;
							for logical in 0..logical_count {
								black_box(guard.as_bytes()[resident_marker_offset(logical)]);
							}
							Ok(())
						},
					)?;
				}
				ResidentIsolatedCandidate::ProductionScope => {
					let mut scope = engine.read_scope()?;
					record_isolated_resident_runs(
						&mut artifacts,
						config,
						candidate,
						hot_pages,
						repetition_base,
						order_position,
						|run, logical_count| {
							let page_no = resident_page(run, hot_pages);
							let page = scope
								.get_resolved(&handles[page_no.get() as usize])?
								.ok_or_else(|| {
									FerrosGrynnError::storage_io_failure(
										"production scoped page became stale",
									)
								})?;
							for logical in 0..logical_count {
								black_box(page.as_bytes()[resident_marker_offset(logical)]);
							}
							Ok(())
						},
					)?;
				}
				ResidentIsolatedCandidate::ProductionWindow => {
					let mut scope = engine.read_scope()?;
					let mut table = if hot_pages <= MAX_READ_SCOPE_OPERATIONS {
						scope.prepare_resolved_table(&handles[..hot_pages])?
					} else {
						scope.prepare_resolved_table(&[])?
					};
					record_isolated_production_windows(
						&mut artifacts,
						config,
						candidate,
						hot_pages,
						repetition_base,
						order_position,
						&mut scope,
						&handles,
						&mut table,
					)?;
				}
				ResidentIsolatedCandidate::Mmap
				| ResidentIsolatedCandidate::NotifyResolved
				| ResidentIsolatedCandidate::DenseFlatRun
				| ResidentIsolatedCandidate::EpochResolved
				| ResidentIsolatedCandidate::EpochDenseFlat
				| ResidentIsolatedCandidate::EpochPageTable
				| ResidentIsolatedCandidate::MissBackoffResolved => {
					return Err(io::Error::other(
						"non-production candidate entered the production fixture branch",
					)
					.into());
				}
			}
		}
		ResidentIsolatedCandidate::DenseFlatRun
		| ResidentIsolatedCandidate::NotifyResolved
		| ResidentIsolatedCandidate::EpochResolved
		| ResidentIsolatedCandidate::EpochDenseFlat
		| ResidentIsolatedCandidate::EpochPageTable
		| ResidentIsolatedCandidate::MissBackoffResolved => {
			let direct = DirectFile::open_read_only(&dataset.path)?;
			let capacity = NonZeroUsize::new(hot_pages).ok_or("zero resident hot set")?;
			let source_pages = NonZeroUsize::new(usize::try_from(dataset.page_count)?)
				.ok_or("zero source pages")?;
			let pool = if candidate == ResidentIsolatedCandidate::MissBackoffResolved {
				AtomicArenaBufferPool::with_miss_side_backoff(capacity)
			} else {
				AtomicArenaBufferPool::new(capacity)
			};
			for raw_page in 0..hot_pages {
				let page_no = PageNo::new(raw_page as u64);
				drop(pool.get_or_load(dataset.key(page_no), |page| {
					read_raw_reported(&dataset, &direct, page_no, page)
				})?);
			}
			let build_flat = || -> Result<SourceBoundLocator, Box<dyn Error>> {
				let flat = SourceBoundLocator::new(
					&pool,
					dataset.key(PageNo::new(0)),
					source_pages,
					DenseLocatorLayout::Flat,
				)?;
				for raw_page in 0..hot_pages {
					if !flat.refresh(&pool, dataset.key(PageNo::new(raw_page as u64))) {
						return Err(io::Error::other(
							"isolated flat locator failed to resolve preload",
						)
						.into());
					}
				}
				Ok(flat)
			};
			match candidate {
				ResidentIsolatedCandidate::DenseFlatRun => {
					let flat = build_flat()?;
					record_isolated_resident_runs(
						&mut artifacts,
						config,
						candidate,
						hot_pages,
						repetition_base,
						order_position,
						|run, logical_count| {
							let page_no = resident_page(run, hot_pages);
							let key = dataset.key(page_no);
							let guard = flat.get_resolved(&pool, key)?.ok_or_else(|| {
								FerrosGrynnError::storage_io_failure(
									"isolated flat locator became stale",
								)
							})?;
							for logical in 0..logical_count {
								black_box(guard.as_bytes()[resident_marker_offset(logical)]);
							}
							Ok(())
						},
					)?;
				}
				ResidentIsolatedCandidate::EpochDenseFlat => {
					let flat = build_flat()?;
					let epoch = pool.read_epoch()?;
					record_isolated_resident_runs(
						&mut artifacts,
						config,
						candidate,
						hot_pages,
						repetition_base,
						order_position,
						|run, logical_count| {
							let page_no = resident_page(run, hot_pages);
							let page = flat
								.get_epoch_resolved(&epoch, dataset.key(page_no))
								.ok_or_else(|| {
									FerrosGrynnError::storage_io_failure(
										"isolated epoch flat locator became stale",
									)
								})?;
							for logical in 0..logical_count {
								black_box(page.as_bytes()[resident_marker_offset(logical)]);
							}
							Ok(())
						},
					)?;
				}
				ResidentIsolatedCandidate::EpochResolved
				| ResidentIsolatedCandidate::EpochPageTable
				| ResidentIsolatedCandidate::NotifyResolved
				| ResidentIsolatedCandidate::MissBackoffResolved => {
					let handles = (0..hot_pages)
						.map(|raw_page| {
							pool.resolve(dataset.key(PageNo::new(raw_page as u64)))
								.ok_or_else(|| io::Error::other("isolated page did not resolve"))
						})
						.collect::<io::Result<Vec<_>>>()?;
					match candidate {
						ResidentIsolatedCandidate::EpochResolved => {
							let epoch = pool.read_epoch()?;
							record_isolated_resident_runs(
								&mut artifacts,
								config,
								candidate,
								hot_pages,
								repetition_base,
								order_position,
								|run, logical_count| {
									let page_no = resident_page(run, hot_pages);
									let page = epoch
										.get_resolved(&handles[page_no.get() as usize])
										.ok_or_else(|| {
											FerrosGrynnError::storage_io_failure(
												"isolated epoch handle became stale",
											)
										})?;
									for logical in 0..logical_count {
										black_box(page.as_bytes()[resident_marker_offset(logical)]);
									}
									Ok(())
								},
							)?;
						}
						ResidentIsolatedCandidate::EpochPageTable => {
							let epoch = pool.read_epoch()?;
							let pages = handles
								.iter()
								.map(|handle| {
									epoch.get_resolved(handle).ok_or_else(|| {
										io::Error::other("isolated epoch page table became stale")
									})
								})
								.collect::<io::Result<Vec<_>>>()?;
							record_isolated_resident_runs(
								&mut artifacts,
								config,
								candidate,
								hot_pages,
								repetition_base,
								order_position,
								|run, logical_count| {
									let page_no = resident_page(run, hot_pages);
									let page = &pages[page_no.get() as usize];
									for logical in 0..logical_count {
										black_box(page.as_bytes()[resident_marker_offset(logical)]);
									}
									Ok(())
								},
							)?;
						}
						ResidentIsolatedCandidate::NotifyResolved
						| ResidentIsolatedCandidate::MissBackoffResolved => {
							record_isolated_resident_runs(
								&mut artifacts,
								config,
								candidate,
								hot_pages,
								repetition_base,
								order_position,
								|run, logical_count| {
									let page_no = resident_page(run, hot_pages);
									let guard = pool
										.get_resolved(&handles[page_no.get() as usize])?
										.ok_or_else(|| {
											FerrosGrynnError::storage_io_failure(
												"isolated R9 handle became stale",
											)
										})?;
									for logical in 0..logical_count {
										black_box(
											guard.as_bytes()[resident_marker_offset(logical)],
										);
									}
									Ok(())
								},
							)?;
						}
						ResidentIsolatedCandidate::Mmap
						| ResidentIsolatedCandidate::ProductionGeneric
						| ResidentIsolatedCandidate::ProductionScope
						| ResidentIsolatedCandidate::ProductionWindow
						| ResidentIsolatedCandidate::DenseFlatRun
						| ResidentIsolatedCandidate::EpochDenseFlat => {
							return Err(io::Error::other(
								"isolated candidate entered the resolved-fixture branch",
							)
							.into());
						}
					}
				}
				ResidentIsolatedCandidate::Mmap => {
					return Err(io::Error::other("mmap entered the arena fixture branch").into());
				}
				ResidentIsolatedCandidate::ProductionGeneric
				| ResidentIsolatedCandidate::ProductionScope
				| ResidentIsolatedCandidate::ProductionWindow => {
					return Err(io::Error::other(
						"production candidate entered the prototype arena fixture branch",
					)
					.into());
				}
			}
		}
	}
	Ok(())
}

/// Measures one resident finalist with a separate read scope per worker.
#[test]
#[ignore = "storage-engine prototype benchmark; run explicitly with --profile profiling"]
fn storage_engine_prototype_resident_isolated_concurrency_probe() -> Result<(), Box<dyn Error>> {
	let config = HarnessConfig::from_env()?;
	let supported = [
		ResidentIsolatedCandidate::Mmap.label(),
		ResidentIsolatedCandidate::ProductionWindow.label(),
	];
	let selector = candidate_selector_from_env("resident_isolated_concurrency", &supported)?;
	let candidate = require_resident_isolated_candidate(selector.as_deref())?;
	let threads = env_parse("FG_STORAGE_PROTO_THREADS", 1_usize)?;
	if ![1_usize, 2, 4, 8, 12].contains(&threads) {
		return Err(
			io::Error::other("resident concurrency threads must be 1, 2, 4, 8, or 12").into()
		);
	}
	if config.resident_operations < threads as u64 {
		return Err(io::Error::other("resident operations must cover every worker").into());
	}
	if isolated_resident_run_length_from_env()?.is_some_and(|run_length| run_length != 1) {
		return Err(io::Error::other("resident concurrency supports only run length 1").into());
	}
	let worker_affinity_masks = worker_affinity_masks_from_env(threads)?;
	let repetition_base = env_parse("FG_STORAGE_PROTO_REPETITION_BASE", 0_usize)?;
	let order_position = env_parse("FG_STORAGE_PROTO_ORDER_POSITION", 0_usize)?;
	let mut artifacts = Artifacts::open(config)?;
	artifacts.selector("resident_isolated_concurrency", selector.as_deref())?;
	writeln!(
		artifacts.log,
		"resident_isolated_concurrency threads={threads} \
		 worker_affinity_masks={worker_affinity_masks:?}"
	)?;
	let dataset = Dataset::create("resident-isolated-concurrency", config.dataset_pages)?;
	let hot_pages = config.pool_pages.min(usize::try_from(config.dataset_pages)?);

	match candidate {
		ResidentIsolatedCandidate::Mmap => {
			let mmap = MmapControl::open(&dataset)?;
			for local_repetition in 0..config.repetitions {
				let repetition = repetition_base
					.checked_add(local_repetition)
					.ok_or_else(|| io::Error::other("concurrency repetition index overflow"))?;
				let measurement = measure_mmap_resident_concurrency(
					&mmap,
					hot_pages,
					config.resident_operations,
					threads,
					worker_affinity_masks.as_deref(),
				)?;
				record_isolated_concurrency_sample(
					&mut artifacts,
					config,
					candidate,
					hot_pages,
					threads,
					repetition,
					order_position,
					measurement,
				)?;
			}
		}
		ResidentIsolatedCandidate::ProductionWindow => {
			let direct = DirectFile::open_read_only(&dataset.path)?;
			let capacity = NonZeroUsize::new(hot_pages).ok_or("zero resident hot set")?;
			let engine = HybridReadEngine::new(capacity);
			for raw_page in 0..hot_pages {
				let page_no = PageNo::new(raw_page as u64);
				drop(engine.get_or_load(dataset.key(page_no), |page| {
					read_raw_reported(&dataset, &direct, page_no, page)
				})?);
			}
			let handles = (0..hot_pages)
				.map(|raw_page| {
					engine
						.resolve(dataset.key(PageNo::new(raw_page as u64)))
						.ok_or_else(|| io::Error::other("production page did not resolve"))
				})
				.collect::<io::Result<Vec<_>>>()?;
			for local_repetition in 0..config.repetitions {
				let repetition = repetition_base
					.checked_add(local_repetition)
					.ok_or_else(|| io::Error::other("concurrency repetition index overflow"))?;
				let measurement = measure_production_resident_concurrency(
					&engine,
					&handles,
					hot_pages,
					config.resident_operations,
					threads,
					worker_affinity_masks.as_deref(),
				)?;
				record_isolated_concurrency_sample(
					&mut artifacts,
					config,
					candidate,
					hot_pages,
					threads,
					repetition,
					order_position,
					measurement,
				)?;
			}
		}
		_ => {
			return Err(io::Error::other("unsupported resident concurrency candidate").into());
		}
	}
	Ok(())
}

/// Measures one resident finalist across one shared 1/4/8-file hot-set budget.
#[test]
#[ignore = "storage-engine prototype benchmark; run explicitly with --profile profiling"]
fn storage_engine_prototype_resident_isolated_multifile_probe() -> Result<(), Box<dyn Error>> {
	let config = HarnessConfig::from_env()?;
	let supported = [
		ResidentIsolatedCandidate::Mmap.label(),
		ResidentIsolatedCandidate::ProductionWindow.label(),
	];
	let selector = candidate_selector_from_env("resident_isolated_multifile", &supported)?;
	let candidate = require_resident_isolated_candidate(selector.as_deref())?;
	let file_count = env_parse("FG_STORAGE_PROTO_FILE_COUNT", 1_usize)?;
	if ![1_usize, 4, 8].contains(&file_count) {
		return Err(io::Error::other("resident multi-file count must be 1, 4, or 8").into());
	}
	if config.pool_pages < file_count
		|| config.pool_pages % file_count != 0
		|| usize::try_from(config.dataset_pages)? < config.pool_pages
		|| config.dataset_pages % file_count as u64 != 0
	{
		return Err(io::Error::other(
			"resident multi-file pages and pool must be divisible by and cover every file",
		)
		.into());
	}
	let repetition_base = env_parse("FG_STORAGE_PROTO_REPETITION_BASE", 0_usize)?;
	let order_position = env_parse("FG_STORAGE_PROTO_ORDER_POSITION", 0_usize)?;
	let affinity_mask = env_parse("FG_STORAGE_PROTO_AFFINITY_MASK", 1_usize)?;
	#[cfg(windows)]
	let _affinity = pin_current_thread(affinity_mask)?;
	let mut artifacts = Artifacts::open(config)?;
	artifacts.selector("resident_isolated_multifile", selector.as_deref())?;
	writeln!(
		artifacts.log,
		"resident_isolated_multifile candidate={} files={file_count} shared_hot_pages={} \
		 affinity_mask={affinity_mask}",
		candidate.label(),
		config.pool_pages,
	)?;

	let pages_per_file = usize::try_from(config.dataset_pages)? / file_count;
	let hot_pages_per_file = config.pool_pages / file_count;
	let datasets = (0..file_count)
		.map(|file_index| {
			Dataset::create(
				&format!("resident-isolated-multifile-{file_index}"),
				pages_per_file as u64,
			)
		})
		.collect::<Result<Vec<_>, _>>()?;
	let table_layout = (0..file_count)
		.flat_map(|file_index| {
			(0..hot_pages_per_file).step_by(MAX_READ_SCOPE_OPERATIONS).map(move |first_page| {
				(
					file_index,
					first_page,
					(hot_pages_per_file - first_page).min(MAX_READ_SCOPE_OPERATIONS),
				)
			})
		})
		.collect::<Vec<_>>();

	match candidate {
		ResidentIsolatedCandidate::Mmap => {
			let mappings = datasets.iter().map(MmapControl::open).collect::<Result<Vec<_>, _>>()?;
			let tables = table_layout
				.iter()
				.map(|&(file_index, first_page, table_pages)| {
					(first_page..first_page + table_pages)
						.map(|raw_page| {
							mapping_page(
								&mappings[file_index].mapping,
								PageNo::new(raw_page as u64),
							)
							.map(NonNull::from)
						})
						.collect::<io::Result<Vec<_>>>()
				})
				.collect::<io::Result<Vec<_>>>()?;
			for local_repetition in 0..config.repetitions {
				let repetition =
					repetition_base.checked_add(local_repetition).ok_or_else(|| {
						io::Error::other("resident multi-file repetition index overflow")
					})?;
				let measurement = measure_resident_runs(
					config.resident_operations,
					config.latency_batch,
					MAX_READ_SCOPE_OPERATIONS,
					|run, logical_count| {
						let table = &tables[run as usize % tables.len()];
						let logical_start = run * MAX_READ_SCOPE_OPERATIONS as u64;
						for offset in 0..logical_count {
							let page_index =
								resident_page(logical_start + offset as u64, table.len()).get()
									as usize;
							let page = table[page_index];
							// SAFETY: every pointer was derived from an
							// immutable complete page in a live `Mmap`;
							// `mappings` outlives all tables and the timed
							// loop.
							black_box(unsafe { page.as_ref() }[resident_marker_offset(offset)]);
						}
						Ok(())
					},
				)?;
				record_isolated_multifile_sample(
					&mut artifacts,
					config,
					candidate,
					file_count,
					repetition,
					order_position,
					measurement,
				)?;
			}
		}
		ResidentIsolatedCandidate::ProductionWindow => {
			let directs = datasets
				.iter()
				.map(|dataset| DirectFile::open_read_only(&dataset.path))
				.collect::<Result<Vec<_>, _>>()?;
			let engine = HybridReadEngine::new(
				NonZeroUsize::new(config.pool_pages).ok_or("zero multi-file resident budget")?,
			);
			let mut handles = Vec::with_capacity(file_count);
			for (dataset, direct) in datasets.iter().zip(&directs) {
				let mut source_handles = Vec::with_capacity(hot_pages_per_file);
				for raw_page in 0..hot_pages_per_file {
					let page_no = PageNo::new(raw_page as u64);
					drop(engine.get_or_load(dataset.key(page_no), |page| {
						read_raw_reported(dataset, direct, page_no, page)
					})?);
					source_handles.push(engine.resolve(dataset.key(page_no)).ok_or_else(|| {
						io::Error::other("production multi-file page did not resolve")
					})?);
				}
				handles.push(source_handles);
			}
			let resident_count = engine.resident_count();
			if resident_count != config.pool_pages {
				return Err(io::Error::other(format!(
					"production multi-file preload retained {resident_count} of {} shared hot \
					 pages",
					config.pool_pages
				))
				.into());
			}
			let mut scope = engine.read_scope()?;
			let mut tables = handles
				.iter()
				.flat_map(|source_handles| {
					source_handles
						.chunks(MAX_READ_SCOPE_OPERATIONS)
						.map(|chunk| scope.prepare_resolved_table(chunk))
				})
				.collect::<FerrosGrynnResult<Vec<_>>>()?;
			for local_repetition in 0..config.repetitions {
				let repetition =
					repetition_base.checked_add(local_repetition).ok_or_else(|| {
						io::Error::other("resident multi-file repetition index overflow")
					})?;
				let measurement = measure_resident_runs(
					config.resident_operations,
					config.latency_batch,
					MAX_READ_SCOPE_OPERATIONS,
					|run, logical_count| {
						let table_index = run as usize % tables.len();
						let window = scope
							.get_resolved_window(&mut tables[table_index], logical_count)?
							.ok_or_else(|| {
								FerrosGrynnError::storage_io_failure(
									"production multi-file table became stale",
								)
							})?;
						let logical_start = run * MAX_READ_SCOPE_OPERATIONS as u64;
						for offset in 0..logical_count {
							let page_index =
								resident_page(logical_start + offset as u64, window.len()).get()
									as usize;
							let page = window.page(page_index).ok_or_else(|| {
								FerrosGrynnError::storage_io_failure(
									"production multi-file window lost a page",
								)
							})?;
							black_box(page.as_bytes()[resident_marker_offset(offset)]);
						}
						Ok(())
					},
				)?;
				record_isolated_multifile_sample(
					&mut artifacts,
					config,
					candidate,
					file_count,
					repetition,
					order_position,
					measurement,
				)?;
			}
		}
		_ => return Err(io::Error::other("unsupported resident multi-file candidate").into()),
	}
	Ok(())
}

/// Records one process-isolated shared-budget multi-file result.
#[allow(clippy::too_many_arguments, reason = "benchmark metadata remains explicit")]
fn record_isolated_multifile_sample(
	artifacts: &mut Artifacts,
	config: HarnessConfig,
	candidate: ResidentIsolatedCandidate,
	file_count: usize,
	repetition: usize,
	order_position: usize,
	measurement: Measurement,
) -> io::Result<()> {
	artifacts.sample(
		SampleMeta {
			phase: "hybrid_h11_multifile",
			workload: "source_local_resident_windows",
			candidate: candidate.label(),
			repetition,
			order_position,
			dataset_pages: config.dataset_pages,
			pool_pages: config.pool_pages,
			hot_pages: config.pool_pages,
			file_count,
			threads: 1,
			queue_depth: 0,
			extent_pages: MAX_READ_SCOPE_OPERATIONS,
			prefetch_depth: 0,
			vm_reserved_bytes: 0,
			vm_committed_bytes: 0,
			hit_ratio: 1.0,
		},
		measurement,
	)
}

/// Records one aggregate isolated concurrency result.
#[allow(clippy::too_many_arguments, reason = "benchmark metadata remains explicit")]
fn record_isolated_concurrency_sample(
	artifacts: &mut Artifacts,
	config: HarnessConfig,
	candidate: ResidentIsolatedCandidate,
	hot_pages: usize,
	threads: usize,
	repetition: usize,
	order_position: usize,
	measurement: Measurement,
) -> Result<(), Box<dyn Error>> {
	artifacts.sample(
		SampleMeta {
			phase: "hybrid_h11_concurrency",
			workload: "concurrent_hot_set",
			candidate: candidate.label(),
			repetition,
			order_position,
			dataset_pages: config.dataset_pages,
			pool_pages: config.pool_pages,
			hot_pages,
			file_count: 1,
			threads,
			queue_depth: 0,
			extent_pages: 1,
			prefetch_depth: 0,
			vm_reserved_bytes: 0,
			vm_committed_bytes: 0,
			hit_ratio: 1.0,
		},
		measurement,
	)?;
	Ok(())
}

/// Reads an optional exact run length for focused resident profiling.
fn isolated_resident_run_length_from_env() -> Result<Option<usize>, Box<dyn Error>> {
	let selected = match std::env::var("FG_STORAGE_PROTO_RUN_LENGTH") {
		Ok(value) => Some(value.parse::<usize>()?),
		Err(std::env::VarError::NotPresent) => None,
		Err(error) => return Err(error.into()),
	};
	if selected.is_some_and(|run_length| !RESIDENT_ISOLATED_RUN_LENGTHS.contains(&run_length)) {
		return Err(io::Error::other(format!(
			"unsupported FG_STORAGE_PROTO_RUN_LENGTH {selected:?}; supported: \
			 {RESIDENT_ISOLATED_RUN_LENGTHS:?}"
		))
		.into());
	}
	Ok(selected)
}

/// Reads one exact nonzero Windows affinity mask per concurrency worker.
///
/// # Errors
///
/// Returns an error for malformed masks, a count different from `threads`, a
/// zero mask, or an affinity request on a non-Windows target.
fn worker_affinity_masks_from_env(threads: usize) -> Result<Option<Vec<usize>>, Box<dyn Error>> {
	let value = match std::env::var("FG_STORAGE_PROTO_WORKER_AFFINITY_MASKS") {
		Ok(value) => value,
		Err(std::env::VarError::NotPresent) => return Ok(None),
		Err(error) => return Err(error.into()),
	};
	#[cfg(not(windows))]
	return Err(
		io::Error::other(format!("worker affinity is Windows-only; requested {value:?}")).into()
	);
	#[cfg(windows)]
	{
		let masks = value
			.split(',')
			.map(|mask| {
				mask.trim().parse::<usize>().map_err(|error| {
					io::Error::other(format!("invalid worker affinity mask {mask:?}: {error}"))
				})
			})
			.collect::<io::Result<Vec<_>>>()?;
		if masks.len() != threads {
			return Err(io::Error::other(format!(
				"worker affinity mask count {} does not match threads {threads}",
				masks.len()
			))
			.into());
		}
		if masks.contains(&0) {
			return Err(io::Error::other("worker affinity masks must be nonzero").into());
		}
		Ok(Some(masks))
	}
}

/// Records the common run-length matrix for one already-isolated resident
/// fixture without moving setup into the timed scope.
#[allow(clippy::too_many_arguments, reason = "paired benchmark metadata remains explicit")]
fn record_isolated_resident_runs<Access>(
	artifacts: &mut Artifacts,
	config: HarnessConfig,
	candidate: ResidentIsolatedCandidate,
	hot_pages: usize,
	repetition_base: usize,
	order_position: usize,
	mut access: Access,
) -> Result<(), Box<dyn Error>>
where
	Access: FnMut(u64, usize) -> Result<(), Box<dyn Error>>,
{
	let selected_run_length = isolated_resident_run_length_from_env()?;
	for run_length in RESIDENT_ISOLATED_RUN_LENGTHS {
		if selected_run_length.is_some_and(|selected| selected != run_length) {
			continue;
		}
		for local_repetition in 0..config.repetitions {
			let repetition = repetition_base
				.checked_add(local_repetition)
				.ok_or_else(|| io::Error::other("isolated repetition index overflow"))?;
			let measurement = measure_resident_runs(
				config.resident_operations,
				config.latency_batch,
				run_length,
				&mut access,
			)?;
			artifacts.sample(
				resident_next_meta(
					"resident_isolated_process",
					"warm_physical_run",
					candidate.label(),
					repetition,
					order_position,
					config,
					hot_pages,
					run_length,
				),
				measurement,
			)?;
		}
	}
	Ok(())
}

/// Records production R8 reads that resolve one complete bounded page table
/// before consuming its pages.
#[allow(clippy::too_many_arguments, reason = "paired benchmark metadata remains explicit")]
fn record_isolated_production_windows(
	artifacts: &mut Artifacts,
	config: HarnessConfig,
	candidate: ResidentIsolatedCandidate,
	hot_pages: usize,
	repetition_base: usize,
	order_position: usize,
	scope: &mut ReadScope<'_>,
	handles: &[ResolvedFrameHandle],
	table: &mut ResolvedPageTable,
) -> Result<(), Box<dyn Error>> {
	let selected_run_length = isolated_resident_run_length_from_env()?;
	for run_length in RESIDENT_ISOLATED_RUN_LENGTHS {
		if selected_run_length.is_some_and(|selected| selected != run_length) {
			continue;
		}
		for local_repetition in 0..config.repetitions {
			let repetition = repetition_base
				.checked_add(local_repetition)
				.ok_or_else(|| io::Error::other("isolated repetition index overflow"))?;
			let measurement = measure_production_resident_windows(
				config.resident_operations,
				config.latency_batch,
				run_length,
				hot_pages,
				scope,
				handles,
				table,
			)?;
			artifacts.sample(
				resident_next_meta(
					"resident_isolated_process",
					"warm_physical_run",
					candidate.label(),
					repetition,
					order_position,
					config,
					hot_pages,
					run_length,
				),
				measurement,
			)?;
		}
	}
	Ok(())
}

/// Screens CacheAll, Rocks-like bypass, and bounded cold admission.
#[test]
#[ignore = "storage-engine prototype benchmark; run explicitly in release/bench profile"]
fn storage_engine_prototype_policy_probe() -> Result<(), Box<dyn Error>> {
	let config = HarnessConfig::from_env()?;
	let mut artifacts = Artifacts::open(config)?;
	let dataset = Dataset::create("policy", config.dataset_pages)?;
	let direct = DirectFile::open_read_only(&dataset.path)?;
	let pool = NonZeroUsize::new(config.pool_pages).ok_or("zero pool")?;
	let scan_pages = usize::try_from(config.dataset_pages)?
		.min(config.pool_pages.saturating_mul(4).max(config.extent_pages));
	for repetition in 0..io_repetitions(config) {
		for (order_position, policy) in
			rotated_order(&READ_POLICIES, repetition).into_iter().enumerate()
		{
			let rss_before = process_rss()?;
			let cache = PrototypeReadCache::for_policy(pool, policy);
			let cursor = PrototypeScanCursor::with_policy(
				ReadIntent::SequentialScan {
					scan_id: repetition as u64,
					expected_pages: scan_pages as u64,
					one_shot: true,
				},
				Some(policy),
			);
			let loads = AtomicU64::new(0);
			let measurement = measure_operations(
				scan_pages as u64,
				config.latency_batch.min(scan_pages).max(1),
				PAGE_SIZE as u64,
				|operation| {
					let page_no = PageNo::new(operation % scan_pages as u64);
					let key = dataset.key(page_no);
					cache.read_with(
						&cursor,
						key,
						|page| {
							loads.fetch_add(1, Ordering::Relaxed);
							read_raw_reported(&dataset, &direct, page_no, page)
						},
						|page| {
							black_box(page.as_bytes()[PAGE_HEADER_SIZE]);
							Ok(())
						},
					)?;
					Ok(())
				},
			)?;
			let measurement = finish_fixture_rss(measurement, rss_before)?;
			let load_count = loads.load(Ordering::Relaxed);
			let hit_ratio = 1.0 - load_count as f64 / scan_pages as f64;
			artifacts.sample(
				SampleMeta {
					phase: "policy",
					workload: "one_shot_sequential",
					candidate: policy_label(policy),
					repetition,
					order_position,
					dataset_pages: scan_pages as u64,
					pool_pages: config.pool_pages,
					hot_pages: 0,
					file_count: 1,
					threads: 1,
					queue_depth: 1,
					extent_pages: 1,
					prefetch_depth: 0,
					vm_reserved_bytes: 0,
					vm_committed_bytes: 0,
					hit_ratio,
				},
				measurement,
			)?;
			if policy == ReadPolicy::RocksLikeBypass && cache.hot_resident_count() != 0 {
				return Err(io::Error::other("bypass polluted the resident pool").into());
			}
			if cache.cold_resident_count() > cache.cold_capacity() {
				return Err(io::Error::other("cold admission exceeded its budget").into());
			}
		}
	}
	writeln!(
		artifacts.log,
		"policy_loader_contract=raw Direct bytes plus immutable Dataset expected header; each \
		 policy performs the only loaded-byte checksum at its authoritative publish or bypass \
		 consumption boundary; rss_delta starts before the candidate cache/cursor and excludes \
		 the immutable Dataset/Direct control"
	)?;
	Ok(())
}

/// Decomposes the resident and miss paths without an `Instant` per operation.
#[test]
#[ignore = "storage-engine component benchmark; run explicitly in release/bench profile"]
fn storage_engine_prototype_component_probe() -> Result<(), Box<dyn Error>> {
	let config = HarnessConfig::from_env()?;
	let mut artifacts = Artifacts::open(config)?;
	let minimum_pages = config.pool_pages.saturating_mul(2).max(config.extent_pages).max(1);
	let component_pages = config.dataset_pages.min(minimum_pages as u64).max(1);
	let dataset = Dataset::create("components", component_pages)?;
	let direct = DirectFile::open_read_only(&dataset.path)?;
	let lookup_pages = config.pool_pages.min(usize::try_from(component_pages)?).max(1);
	let capacity = NonZeroUsize::new(lookup_pages).ok_or("zero component capacity")?;
	let atomic = AtomicArenaBufferPool::new(capacity);
	let keys =
		(0..lookup_pages).map(|index| dataset.key(PageNo::new(index as u64))).collect::<Vec<_>>();
	for (index, key) in keys.iter().copied().enumerate() {
		drop(atomic.get_or_load(key, |page| {
			read_raw_reported(&dataset, &direct, PageNo::new(index as u64), page)
		})?);
	}
	let handles = keys
		.iter()
		.copied()
		.map(|key| {
			atomic.resolve(key).ok_or_else(|| io::Error::other("component preload did not resolve"))
		})
		.collect::<io::Result<Vec<_>>>()?;
	let mut hash = RapidHashMap::with_capacity(keys.len());
	for (index, key) in keys.iter().copied().enumerate() {
		hash.insert(key, index);
	}

	let extent_pages = config.extent_pages.min(usize::try_from(component_pages)?).max(1);
	let sealed_page = benchmark_page(PageNo::new(0))?;
	let mut raw_extent = aligned_pages(extent_pages);
	direct.read_pages_raw(PageNo::new(0), &mut raw_extent)?;
	validate_extent(PageNo::new(0), &raw_extent)?;
	let component_operations = config.resident_operations;
	let page_validation_operations = component_operations.min(65_536);
	let extent_operations = component_operations.min(65_536).div_ceil(extent_pages as u64).max(64);
	let lifecycle_batch = extent_pages.min(8).min(lookup_pages).max(1);
	let repetitions = config.repetitions.min(5);

	for repetition in 0..repetitions {
		let mut order_position = 0;
		let measurement =
			measure_operations(component_operations, config.latency_batch, 0, |operation| {
				let key = keys[operation as usize % keys.len()];
				let Some(frame) = hash.get(&key).copied() else {
					return Err(io::Error::other("RapidHashMap component key disappeared").into());
				};
				black_box(frame);
				Ok(())
			})?;
		record_component_sample(
			&mut artifacts,
			config,
			component_pages,
			"lookup",
			"rapid_hash_get",
			repetition,
			order_position,
			measurement,
		)?;
		order_position += 1;

		let measurement =
			measure_operations(component_operations, config.latency_batch, 0, |operation| {
				let key = keys[operation as usize % keys.len()];
				let handle = atomic
					.resolve(key)
					.ok_or_else(|| io::Error::other("atomic shard lookup missed"))?;
				black_box(handle);
				Ok(())
			})?;
		record_component_sample(
			&mut artifacts,
			config,
			component_pages,
			"lookup",
			"atomic_shard_resolve",
			repetition,
			order_position,
			measurement,
		)?;
		order_position += 1;

		let measurement =
			measure_operations(component_operations, config.latency_batch, 0, |operation| {
				let handle = handles[operation as usize % handles.len()];
				let guard = atomic.get_resolved(&handle)?.ok_or_else(|| {
					FerrosGrynnError::storage_io_failure("resolved pin became stale")
				})?;
				black_box(guard.as_bytes()[PAGE_HEADER_SIZE]);
				drop(guard);
				Ok(())
			})?;
		record_component_sample(
			&mut artifacts,
			config,
			component_pages,
			"guard",
			"atomic_pin_usage_borrowed_guard",
			repetition,
			order_position,
			measurement,
		)?;
		order_position += 1;

		let page = &sealed_page;
		let measurement = measure_operations(
			page_validation_operations,
			config.latency_batch,
			PAGE_SIZE as u64,
			|_operation| {
				black_box(page.validate(PageNo::new(0))?.checksum());
				Ok(())
			},
		)?;
		record_component_sample(
			&mut artifacts,
			config,
			component_pages,
			"checksum",
			"checksum_page_validate",
			repetition,
			order_position,
			measurement,
		)?;
		order_position += 1;

		let measurement = measure_operations(
			extent_operations,
			config.latency_batch.min(extent_operations as usize).max(1),
			(extent_pages * PAGE_SIZE) as u64,
			|_operation| {
				validate_extent(PageNo::new(0), &raw_extent)?;
				black_box(raw_extent[0].as_bytes()[PAGE_HEADER_SIZE]);
				Ok(())
			},
		)?;
		record_component_sample(
			&mut artifacts,
			config,
			component_pages,
			"checksum",
			"checksum_extent_validate",
			repetition,
			order_position,
			measurement,
		)?;
		order_position += 1;

		let mut contiguous_destination = aligned_pages(extent_pages);
		let measurement = measure_operations(
			extent_operations,
			config.latency_batch.min(extent_operations as usize).max(1),
			(extent_pages * PAGE_SIZE) as u64,
			|operation| {
				AlignedPage::slice_as_bytes_mut(&mut contiguous_destination)
					.copy_from_slice(AlignedPage::slice_as_bytes(&raw_extent));
				black_box(
					contiguous_destination[operation as usize % extent_pages].as_bytes()
						[PAGE_HEADER_SIZE],
				);
				Ok(())
			},
		)?;
		record_component_sample(
			&mut artifacts,
			config,
			component_pages,
			"memcpy",
			"memcpy_contiguous_extent",
			repetition,
			order_position,
			measurement,
		)?;
		order_position += 1;

		let scattered_source = raw_extent
			.iter()
			.map(|page| {
				let mut copied = AlignedPage::zeroed();
				copied.as_bytes_mut().copy_from_slice(page.as_bytes());
				Box::new(copied)
			})
			.collect::<Vec<_>>();
		let mut scattered_destination =
			(0..extent_pages).map(|_| Box::new(AlignedPage::zeroed())).collect::<Vec<_>>();
		let measurement = measure_operations(
			extent_operations,
			config.latency_batch.min(extent_operations as usize).max(1),
			(extent_pages * PAGE_SIZE) as u64,
			|operation| {
				for (source, destination) in
					scattered_source.iter().zip(scattered_destination.iter_mut())
				{
					destination.as_bytes_mut().copy_from_slice(source.as_bytes());
				}
				black_box(
					scattered_destination[operation as usize % extent_pages].as_bytes()
						[PAGE_HEADER_SIZE],
				);
				Ok(())
			},
		)?;
		record_component_sample(
			&mut artifacts,
			config,
			component_pages,
			"memcpy",
			"memcpy_scattered_frames",
			repetition,
			order_position,
			measurement,
		)?;
		order_position += 1;

		let measurement = measure_operations(
			extent_operations,
			config.latency_batch.min(extent_operations as usize).max(1),
			(extent_pages * PAGE_SIZE) as u64,
			|_operation| {
				validate_extent(PageNo::new(0), &raw_extent)?;
				black_box(raw_extent[0].as_bytes()[PAGE_HEADER_SIZE]);
				Ok(())
			},
		)?;
		record_component_sample(
			&mut artifacts,
			config,
			component_pages,
			"validation",
			"validation_only_preread_extent",
			repetition,
			order_position,
			measurement,
		)?;
		order_position += 1;

		for (workload, candidate, measurement) in [
			(
				"reservation",
				"reserve_unique_signal_create",
				measure_reserve_unique(
					&dataset,
					lookup_pages,
					lifecycle_batch,
					config.latency_batch,
				)?,
			),
			(
				"reservation",
				"abort_prepared_signal_complete",
				measure_abort_prepared(
					&dataset,
					lookup_pages,
					lifecycle_batch,
					config.latency_batch,
				)?,
			),
			(
				"reservation",
				"publish_prepared_signal_complete",
				measure_publish_prepared(
					&dataset,
					lookup_pages,
					lifecycle_batch,
					config.latency_batch,
				)?,
			),
		] {
			record_component_sample(
				&mut artifacts,
				config,
				component_pages,
				workload,
				candidate,
				repetition,
				order_position,
				measurement,
			)?;
			order_position += 1;
		}

		let signal_iterations = component_operations.clamp(1, 1_024);
		let measurement = measure_coalesced_signal_wait(&dataset, &sealed_page, signal_iterations)?;
		record_component_sample(
			&mut artifacts,
			config,
			component_pages,
			"signal",
			"signal_wait_coalesced_cycle",
			repetition,
			order_position,
			measurement,
		)?;
		order_position += 1;

		let memory_misses = component_operations.clamp(1, 8_192);
		let measurement = measure_full_miss_memory(&dataset, &sealed_page, memory_misses)?;
		record_component_sample(
			&mut artifacts,
			config,
			component_pages,
			"miss",
			"full_miss_memory_loader",
			repetition,
			order_position,
			measurement,
		)?;
		order_position += 1;

		let direct_misses = component_operations.min(component_pages).clamp(1, 1_024);
		let measurement = measure_full_miss_direct(&dataset, &direct, direct_misses)?;
		record_component_sample(
			&mut artifacts,
			config,
			component_pages,
			"miss",
			"full_miss_direct_validated",
			repetition,
			order_position,
			measurement,
		)?;
		order_position += 1;

		#[cfg(windows)]
		{
			let vm_iterations = component_operations.clamp(1, 256);
			for (candidate, measurement) in measure_vm_lifecycle(vm_iterations)? {
				record_component_sample(
					&mut artifacts,
					config,
					component_pages,
					"vm_lifecycle",
					candidate,
					repetition,
					order_position,
					measurement,
				)?;
				order_position += 1;
			}
		}
	}
	writeln!(
		artifacts.log,
		"component_note=checksum rows include normative header validation; signal rows exercise \
		 the public coalesced-miss protocol because LoadSignal is intentionally private; prepared \
		 publication includes the single authoritative checksum validation boundary; VM lifecycle \
		 percentiles use at most 256 isolated iterations per repetition and report zero RSS \
		 because every reservation is released before a process delta could be attributed to one \
		 phase"
	)?;
	Ok(())
}

/// Exercises access patterns derived from the normative graph and index hot
/// paths.
#[test]
#[ignore = "storage-engine analytical workload benchmark; run explicitly in release/bench profile"]
fn storage_engine_prototype_workload_probe() -> Result<(), Box<dyn Error>> {
	let config = HarnessConfig::from_env()?;
	if config.pool_pages < 2 || config.dataset_pages < 2 {
		return Err(io::Error::other(
			"analytical workloads require at least two pool and dataset pages",
		)
		.into());
	}
	let mut artifacts = Artifacts::open(config)?;
	let maximum_hot_pages =
		config.pool_pages.saturating_mul(2).min(usize::try_from(config.dataset_pages)?).max(1);
	let first_file_pages = config
		.pool_pages
		.saturating_mul(4)
		.max(maximum_hot_pages)
		.max(config.extent_pages)
		.min(usize::try_from(config.dataset_pages)?)
		.max(2);
	let quarter_pages = maximum_hot_pages.div_ceil(4).max(1);
	let eighth_pages = maximum_hot_pages.div_ceil(8).max(1);
	let file_page_counts = [
		first_file_pages,
		quarter_pages,
		quarter_pages,
		quarter_pages,
		eighth_pages,
		eighth_pages,
		eighth_pages,
		eighth_pages,
	];
	let mut datasets = Vec::with_capacity(file_page_counts.len());
	for (index, page_count) in file_page_counts.into_iter().enumerate() {
		datasets.push(Dataset::create(&format!("workload-{index}"), page_count as u64)?);
	}
	let directs = datasets
		.iter()
		.map(|dataset| DirectFile::open_read_only(&dataset.path))
		.collect::<FerrosGrynnResult<Vec<_>>>()?;
	let repetitions = config.repetitions.min(if config.full {
		5
	} else {
		1
	});
	let workload_operations = config
		.resident_operations
		.min(if config.full {
			131_072
		} else {
			32_768
		})
		.max(1);

	for repetition in 0..repetitions {
		for (order_position, policy) in
			rotated_order(&READ_POLICIES, repetition).into_iter().enumerate()
		{
			let (measurement, hit_ratio, root_pages) = measure_btree_hot_roots_and_leaf_scan(
				&datasets[0],
				&directs[0],
				config.pool_pages,
				policy,
				config.latency_batch,
			)?;
			let candidate = match policy {
				ReadPolicy::CacheAll => "btree_A0_cache_all",
				ReadPolicy::RocksLikeBypass => "btree_A1_rocks_bypass",
				ReadPolicy::InnodbLikeCold => "btree_A2_innodb_cold",
			};
			artifacts.sample(
				SampleMeta {
					phase: "workload",
					workload: "btree_hot_roots_leaf_scan",
					candidate,
					repetition,
					order_position,
					dataset_pages: datasets[0].page_count,
					pool_pages: config.pool_pages,
					hot_pages: root_pages,
					file_count: 1,
					threads: 1,
					queue_depth: 1,
					extent_pages: 1,
					prefetch_depth: 0,
					vm_reserved_bytes: 0,
					vm_committed_bytes: 0,
					hit_ratio,
				},
				measurement,
			)?;
		}

		let graph_hot_pages =
			config.pool_pages.div_ceil(2).min(first_file_pages.saturating_sub(1)).max(1);
		let graph_cold_pages = first_file_pages.saturating_sub(graph_hot_pages).max(1);
		let graph_preload = (0..graph_hot_pages)
			.map(|page| WorkloadAccess {
				file_index: 0,
				page_no: PageNo::new(page as u64),
				hot: true,
			})
			.collect::<Vec<_>>();
		let graph_accesses = (0..workload_operations)
			.map(|operation| {
				if operation % 8 == 7 {
					WorkloadAccess {
						file_index: 0,
						page_no: PageNo::new(
							graph_hot_pages as u64
								+ operation.div_ceil(8) % graph_cold_pages as u64,
						),
						hot: false,
					}
				} else {
					WorkloadAccess {
						file_index: 0,
						page_no: graph_hot_page(operation, graph_hot_pages),
						hot: true,
					}
				}
			})
			.collect::<Vec<_>>();
		let (measurement, hit_ratio) = measure_atomic_accesses(
			&datasets[..1],
			&directs[..1],
			config.pool_pages,
			&graph_preload,
			&graph_accesses,
			config.latency_batch,
		)?;
		artifacts.sample(
			SampleMeta {
				phase: "workload",
				workload: "graph_frontier_hot_sparse_cold",
				candidate: "R2_atomic_cache_all",
				repetition,
				order_position: 0,
				dataset_pages: datasets[0].page_count,
				pool_pages: config.pool_pages,
				hot_pages: graph_hot_pages,
				file_count: 1,
				threads: 1,
				queue_depth: 1,
				extent_pages: 1,
				prefetch_depth: 0,
				vm_reserved_bytes: 0,
				vm_committed_bytes: 0,
				hit_ratio,
			},
			measurement,
		)?;

		let property_hot_pages =
			config.pool_pages.div_ceil(8).min(first_file_pages.saturating_sub(1)).max(1);
		let overflow_pages = first_file_pages.saturating_sub(property_hot_pages).max(1);
		let property_preload = (0..property_hot_pages)
			.map(|page| WorkloadAccess {
				file_index: 0,
				page_no: PageNo::new(page as u64),
				hot: true,
			})
			.collect::<Vec<_>>();
		let property_accesses = (0..workload_operations)
			.map(|operation| {
				if operation % 8 == 7 {
					WorkloadAccess {
						file_index: 0,
						page_no: PageNo::new(
							property_hot_pages as u64
								+ operation.div_ceil(8) % overflow_pages as u64,
						),
						hot: false,
					}
				} else {
					let run = operation / 8;
					let offset = operation % 4;
					WorkloadAccess {
						file_index: 0,
						page_no: PageNo::new(
							(run.wrapping_mul(17) + offset) % property_hot_pages as u64,
						),
						hot: true,
					}
				}
			})
			.collect::<Vec<_>>();
		let (measurement, hit_ratio) = measure_atomic_accesses(
			&datasets[..1],
			&directs[..1],
			config.pool_pages,
			&property_preload,
			&property_accesses,
			config.latency_batch,
		)?;
		artifacts.sample(
			SampleMeta {
				phase: "workload",
				workload: "properties_short_runs_overflow",
				candidate: "R2_atomic_cache_all",
				repetition,
				order_position: 0,
				dataset_pages: datasets[0].page_count,
				pool_pages: config.pool_pages,
				hot_pages: property_hot_pages,
				file_count: 1,
				threads: 1,
				queue_depth: 1,
				extent_pages: 1,
				prefetch_depth: 0,
				vm_reserved_bytes: 0,
				vm_committed_bytes: 0,
				hit_ratio,
			},
			measurement,
		)?;

		let ratio_cases = [
			("pool_0_50x_hot", config.pool_pages.saturating_mul(2)),
			("pool_0_75x_hot", config.pool_pages.saturating_mul(4).div_ceil(3)),
			("pool_1_00x_hot", config.pool_pages),
			("pool_1_25x_hot", config.pool_pages.saturating_mul(4).div_ceil(5)),
			("pool_1_50x_hot", config.pool_pages.saturating_mul(2).div_ceil(3)),
		];
		for file_count in [1_usize, 4, 8] {
			let active_dataset_pages =
				datasets[..file_count].iter().map(|dataset| dataset.page_count).sum::<u64>();
			for (order_position, (candidate, requested_hot_pages)) in
				ratio_cases.into_iter().enumerate()
			{
				let hot_pages = requested_hot_pages.min(maximum_hot_pages).max(1);
				let preload = (0..hot_pages)
					.map(|global| multifile_access(global, file_count, true))
					.collect::<Vec<_>>();
				let accesses = (0..workload_operations)
					.map(|operation| {
						let global = resident_page(operation, hot_pages).get() as usize;
						multifile_access(global, file_count, true)
					})
					.collect::<Vec<_>>();
				let (measurement, hit_ratio) = measure_atomic_accesses(
					&datasets[..file_count],
					&directs[..file_count],
					config.pool_pages,
					&preload,
					&accesses,
					config.latency_batch,
				)?;
				artifacts.sample(
					SampleMeta {
						phase: "workload",
						workload: "multifile_hot_set_ratio",
						candidate,
						repetition,
						order_position,
						dataset_pages: active_dataset_pages,
						pool_pages: config.pool_pages,
						hot_pages,
						file_count,
						threads: 1,
						queue_depth: 1,
						extent_pages: 1,
						prefetch_depth: 0,
						vm_reserved_bytes: 0,
						vm_committed_bytes: 0,
						hit_ratio,
					},
					measurement,
				)?;
			}
		}
	}
	writeln!(
		artifacts.log,
		"workload_note=A2 rows include current cold-metadata maintenance; hot-set ratio is \
		 derived from pool_pages/hot_pages and file_count is the shared-budget source count; \
		 Atomic/policy loaders return immutable Dataset headers after raw I/O and publication \
		 owns loaded-byte checksum validation; rss_delta starts before candidate arenas/caches \
		 and excludes immutable Dataset/Direct controls"
	)?;
	Ok(())
}

/// Compares the surviving R8 design with R2 and mmap on mixed database access
/// shapes while every repetition owns a disjoint cold page range.
#[cfg(all(windows, feature = "native-async-io"))]
#[test]
#[ignore = "mixed storage-engine finalist benchmark; run explicitly with --profile profiling"]
fn storage_engine_prototype_mixed_finalist_probe() -> Result<(), Box<dyn Error>> {
	let config = HarnessConfig::from_env()?;
	if config.pool_pages < 8 || config.dataset_pages <= config.pool_pages as u64 {
		return Err(io::Error::other(
			"mixed finalists require at least eight pool pages and a larger dataset",
		)
		.into());
	}
	let mut artifacts = Artifacts::open(config)?;
	let repetitions = io_repetitions(config);
	let root_pages = config.pool_pages.min(8);
	let leaf_pages = config
		.pool_pages
		.saturating_mul(2)
		.min(
			usize::try_from(config.dataset_pages)?
				.saturating_sub(root_pages)
				.checked_div(repetitions)
				.ok_or("zero mixed repetition count")?,
		)
		.max(1);
	let per_auxiliary_file = config.pool_pages.div_ceil(4).max(1);
	let mut datasets = Vec::with_capacity(8);
	datasets.push(Dataset::create("mixed-finalist-0", config.dataset_pages)?);
	for file_index in 1..8 {
		datasets.push(Dataset::create(
			&format!("mixed-finalist-{file_index}"),
			per_auxiliary_file as u64,
		)?);
	}
	let directs = datasets
		.iter()
		.map(|dataset| DirectFile::open_read_only(&dataset.path))
		.collect::<FerrosGrynnResult<Vec<_>>>()?;
	let scatter_file = Arc::new(DirectFile::open_read_write_with_mode(
		&datasets[0].path,
		DirectFileOpenMode::Overlapped,
	)?);

	let graph_hot_pages = config.pool_pages.div_ceil(2);
	let property_hot_pages = config.pool_pages.div_ceil(8);
	let cold_start = graph_hot_pages.max(property_hot_pages);
	let cold_pages = usize::try_from(config.dataset_pages)?.saturating_sub(cold_start);
	let cold_region_pages = cold_pages / 2;
	let maximum_operations =
		cold_region_pages.checked_div(repetitions).unwrap_or(0).saturating_mul(8);
	let workload_operations =
		usize::try_from(config.resident_operations)?.min(32_768).min(maximum_operations).max(8);
	let multifile_operations =
		usize::try_from(config.resident_operations)?.min(2_000_000).max(workload_operations);
	let cold_reads_per_repetition = workload_operations.div_ceil(8);
	if cold_reads_per_repetition.saturating_mul(repetitions) > cold_region_pages {
		return Err(io::Error::other("dataset cannot provide disjoint mixed cold ranges").into());
	}
	let graph_preload = contiguous_hot_preload(graph_hot_pages);
	let property_preload = contiguous_hot_preload(property_hot_pages);

	for repetition in 0..repetitions {
		let leaf_first = PageNo::new((root_pages + repetition * leaf_pages) as u64);
		let btree_candidates = [
			"R2_roots+A1_sync_bypass",
			"H11_production_r8_scope+A1_scatter+P1",
			"R0_mmap_hot_roots",
		];
		for (order_position, candidate) in
			rotated_order(&btree_candidates, repetition).into_iter().enumerate()
		{
			let (measurement, threads, extent_pages, prefetch_depth) = match candidate {
				"R2_roots+A1_sync_bypass" => (
					measure_btree_sync_bypass(
						&datasets[0],
						&directs[0],
						root_pages,
						leaf_first,
						leaf_pages,
						config.latency_batch,
					)?,
					1,
					1,
					0,
				),
				"H11_production_r8_scope+A1_scatter+P1" => {
					let (measurement, extent_pages) = measure_btree_hybrid_scatter(
						&datasets[0],
						&directs[0],
						Arc::clone(&scatter_file),
						config.pool_pages,
						root_pages,
						leaf_first,
						leaf_pages,
					)?;
					(measurement, 2, extent_pages, 1)
				}
				"R0_mmap_hot_roots" => (
					measure_btree_mmap(
						&datasets[0],
						root_pages,
						leaf_first,
						leaf_pages,
						config.latency_batch,
					)?,
					1,
					1,
					0,
				),
				_ => return Err(io::Error::other("unknown mixed B+tree candidate").into()),
			};
			artifacts.sample(
				SampleMeta {
					phase: "mixed_finalist",
					workload: "btree_hot_roots_disjoint_leaf_scan",
					candidate,
					repetition,
					order_position,
					dataset_pages: datasets[0].page_count,
					pool_pages: config.pool_pages,
					hot_pages: root_pages,
					file_count: 1,
					threads,
					queue_depth: 1,
					extent_pages,
					prefetch_depth,
					vm_reserved_bytes: 0,
					vm_committed_bytes: 0,
					hit_ratio: root_pages.min(3) as f64 / (root_pages.min(3) + 1) as f64,
				},
				measurement,
			)?;
		}

		let graph_cold_first = cold_start + repetition * cold_reads_per_repetition;
		let graph_accesses = mixed_hot_cold_accesses(
			workload_operations,
			graph_hot_pages,
			graph_cold_first,
			cold_reads_per_repetition,
		);
		record_mixed_access_pair(
			&mut artifacts,
			config,
			&datasets[..1],
			&directs[..1],
			"graph_frontier_epoch_hot_sparse_direct",
			repetition,
			&graph_preload,
			&graph_accesses,
			Some(&scatter_file),
		)?;

		let property_cold_first =
			cold_start + cold_region_pages + repetition * cold_reads_per_repetition;
		let property_accesses = mixed_property_accesses(
			workload_operations,
			property_hot_pages,
			property_cold_first,
			cold_reads_per_repetition,
		);
		record_mixed_access_pair(
			&mut artifacts,
			config,
			&datasets[..1],
			&directs[..1],
			"properties_epoch_hot_sparse_overflow",
			repetition,
			&property_preload,
			&property_accesses,
			Some(&scatter_file),
		)?;

		for file_count in [1_usize, 4, 8] {
			let hot_pages = config.pool_pages;
			let preload = (0..hot_pages)
				.map(|global| multifile_access(global, file_count, true))
				.collect::<Vec<_>>();
			let accesses = (0..multifile_operations)
				.map(|operation| {
					multifile_access(
						resident_page(operation as u64, hot_pages).get() as usize,
						file_count,
						true,
					)
				})
				.collect::<Vec<_>>();
			record_mixed_access_pair(
				&mut artifacts,
				config,
				&datasets[..file_count],
				&directs[..file_count],
				"multifile_common_budget_resident",
				repetition,
				&preload,
				&accesses,
				None,
			)?;
		}
	}
	writeln!(
		artifacts.log,
		"mixed_finalist_note=cold page ranges are disjoint across repetitions; R8 retains only \
		 the declared hot table inside one read epoch and validates sparse cold pages through \
		 reusable synchronous Direct scratch; B+tree R8 validates scatter extents before exposing \
		 any leaf; mmap prevalidates only declared hot pages and validates each disjoint cold \
		 page in-scope"
	)?;
	Ok(())
}

/// Compares the production hybrid fixed-hot scan with raw Direct and mmap in
/// an isolated process. External orchestration rotates candidates and supplies
/// paired repetition/order indices.
#[cfg(all(windows, feature = "native-async-io"))]
#[test]
#[ignore = "fixed-hot production benchmark; run explicitly with --profile profiling"]
fn storage_engine_prototype_fixed_hot_scan_probe() -> Result<(), Box<dyn Error>> {
	const CANDIDATES: [&str; 3] =
		["T0_raw_fixed_hot", "H11_production_r8_hot+scatter_p1", "R0_mmap_fixed_hot"];

	let config = HarnessConfig::from_env()?;
	let selector = candidate_selector_from_env("fixed_hot_scan", &CANDIDATES)?;
	let candidate = selector
		.as_deref()
		.ok_or_else(|| io::Error::other("fixed-hot process probe requires one exact candidate"))?;
	let repetition = env_parse("FG_STORAGE_PROTO_REPETITION_BASE", 0_usize)?;
	let order_position = env_parse("FG_STORAGE_PROTO_ORDER_POSITION", 0_usize)?;
	let hot_pages = config.pool_pages / 4 / config.extent_pages * config.extent_pages;
	if hot_pages == 0 || config.dataset_pages <= hot_pages as u64 {
		return Err(io::Error::other(
			"fixed-hot scan requires one complete hot extent and a larger dataset",
		)
		.into());
	}
	let runs = fixed_hot_scan_runs(
		usize::try_from(config.dataset_pages)?,
		hot_pages,
		config.extent_pages,
	)?;
	let hot_accesses =
		runs.iter().filter(|(_, _, hot)| *hot).map(|(_, pages, _)| *pages as u64).sum::<u64>();
	let mut artifacts = Artifacts::open(config)?;
	artifacts.selector("fixed_hot_scan", selector.as_deref())?;
	let dataset = Dataset::create("fixed-hot-scan", config.dataset_pages)?;

	let (measurement, hit_ratio, threads, prefetch_depth) = match candidate {
		"T0_raw_fixed_hot" => {
			(measure_fixed_hot_raw(&dataset, &runs, config.extent_pages)?, 0.0, 1, 0)
		}
		"H11_production_r8_hot+scatter_p1" => {
			let (measurement, hot_hits) = measure_fixed_hot_hybrid(
				&dataset,
				&runs,
				hot_pages,
				config.pool_pages,
				config.extent_pages,
			)?;
			(measurement, hot_hits as f64 / hot_accesses as f64, 2, 1)
		}
		"R0_mmap_fixed_hot" => (measure_fixed_hot_mmap(&dataset, &runs, hot_pages)?, 1.0, 1, 0),
		_ => return Err(io::Error::other("unknown fixed-hot candidate").into()),
	};
	artifacts.sample(
		SampleMeta {
			phase: "fixed_hot_finalist",
			workload: "scan_with_fixed_hot_set",
			candidate,
			repetition,
			order_position,
			dataset_pages: config.dataset_pages,
			pool_pages: config.pool_pages,
			hot_pages,
			file_count: 1,
			threads,
			queue_depth: 1,
			extent_pages: config.extent_pages,
			prefetch_depth,
			vm_reserved_bytes: 0,
			vm_committed_bytes: 0,
			hit_ratio,
		},
		measurement,
	)?;
	writeln!(
		artifacts.log,
		"fixed_hot_note=one cold run contains four transport extents followed by one rotating hot \
		 extent; production cold pages use Scatter P1 without admission and hot pages use \
		 reusable resolved tables; hit_ratio counts only declared hot accesses"
	)?;
	Ok(())
}

/// Screens synchronous Direct, forced extent IoRing, frame IoRing, scatter,
/// and direct-to-frame Atomic/VM combinations.
#[cfg(all(windows, feature = "native-async-io"))]
#[test]
#[ignore = "native Windows storage transport benchmark; run explicitly"]
fn storage_engine_prototype_transport_probe() -> Result<(), Box<dyn Error>> {
	let config = HarnessConfig::from_env()?;
	let mut artifacts = Artifacts::open(config)?;
	let mut candidates = TRANSPORT_CANDIDATES.to_vec();
	let selector = candidate_selector_from_env("transport", &candidates)?;
	if let Some(selected) = selector.as_deref() {
		candidates.retain(|candidate| *candidate == selected);
	}
	artifacts.selector("transport", selector.as_deref())?;
	let dataset = Dataset::create("transport", config.dataset_pages)?;
	let file = Arc::new(DirectFile::open_read_write_with_mode(
		&dataset.path,
		DirectFileOpenMode::Overlapped,
	)?);
	let scan_pages = usize::try_from(config.dataset_pages)?;
	let extent_pages = config.extent_pages.min(scan_pages).max(1);
	let hybrid = HybridReadEngine::new(
		NonZeroUsize::new(config.pool_pages).ok_or("zero hybrid transport budget")?,
	);
	for repetition in 0..io_repetitions(config) {
		for (order_position, candidate) in
			rotated_order(&candidates, repetition).into_iter().enumerate()
		{
			if is_direct_to_frame_candidate(candidate) {
				let (measurement, reserved, committed) = measure_direct_to_frame(
					candidate,
					&dataset,
					Arc::clone(&file),
					config.pool_pages,
					extent_pages,
					scan_pages,
				)?;
				artifacts.sample(
					SampleMeta {
						phase: "combination",
						workload: "sequential_batched",
						candidate,
						repetition,
						order_position,
						dataset_pages: scan_pages as u64,
						pool_pages: config.pool_pages,
						hot_pages: 0,
						file_count: 1,
						threads: 1,
						queue_depth: if candidate == "atomic_ioring_frame_direct" {
							extent_pages
						} else {
							1
						},
						extent_pages,
						prefetch_depth: 0,
						vm_reserved_bytes: reserved,
						vm_committed_bytes: committed,
						hit_ratio: 0.0,
					},
					measurement,
				)?;
				continue;
			}

			let requested_qd = requested_ioring_qd(candidate);
			if requested_qd.is_some_and(|queue_depth| queue_depth > scan_pages) {
				artifacts.skip(
					"transport",
					candidate,
					"requested SQE batch exceeds the dataset page count",
				)?;
				continue;
			}
			let (measurement, measured_extent_pages, prefetch_depth, threads) = if candidate
				== "H11_production_scatter_p1"
			{
				(
					measure_hybrid_scatter(&hybrid, Arc::clone(&file), scan_pages)?,
					hybrid.scatter_extent_capacity(),
					1,
					2,
				)
			} else if let Some(queue_depth) = requested_qd {
				let (measurement, stats) =
					measure_frame_ioring_requested_qd(&file, queue_depth, scan_pages)?;
				writeln!(
					artifacts.log,
					"ioring_submission candidate={candidate} repetition={repetition} \
					 requested_sqe_batch={} effective_sqe_batch={} ring_capacity={} \
					 submit_calls={} submitted_sqes={} completed_cqes={}",
					stats.requested_sqe_batch_limit,
					stats.effective_sqe_batch_limit,
					stats.submission_capacity,
					stats.submit_calls,
					stats.submitted_sqes,
					stats.completed_cqes,
				)?;
				(measurement, queue_depth, 0, 1)
			} else {
				(measure_transport(candidate, &file, extent_pages, scan_pages)?, extent_pages, 0, 1)
			};
			artifacts.sample(
				SampleMeta {
					phase: "transport",
					workload: "sequential_batched",
					candidate,
					repetition,
					order_position,
					dataset_pages: scan_pages as u64,
					pool_pages: config.pool_pages,
					hot_pages: 0,
					file_count: 1,
					threads,
					queue_depth: requested_qd.unwrap_or(1),
					extent_pages: measured_extent_pages,
					prefetch_depth,
					vm_reserved_bytes: 0,
					vm_committed_bytes: 0,
					hit_ratio: 0.0,
				},
				measurement,
			)?;
		}
	}
	writeln!(
		artifacts.log,
		"transport_note=queue_depth is the requested software SQE batch for T2 and the requested \
		 extent-frame batch for atomic_ioring_frame_direct, never measured device queue \
		 occupancy; T2 effective batches are logged separately and scatter submits one request \
		 per extent; standalone and direct-to-frame candidates share one independently rotated \
		 order; rss_delta includes each candidate's owned buffers, transport context, and arena"
	)?;
	writeln!(
		artifacts.log,
		"vm_lifecycle_note=isolated reserve/commit/first-touch/decommit/recommit rows are emitted \
		 by storage_engine_prototype_component_probe"
	)?;
	Ok(())
}

/// Measures bounded prefetch depths for contiguous Direct and scatter reads.
#[test]
#[ignore = "storage-engine prototype benchmark; run explicitly in release/bench profile"]
fn storage_engine_prototype_prefetch_probe() -> Result<(), Box<dyn Error>> {
	let config = HarnessConfig::from_env()?;
	let mut artifacts = Artifacts::open(config)?;
	let dataset = Dataset::create("prefetch", config.dataset_pages)?;
	let scan_pages = usize::try_from(config.dataset_pages)?;
	let extent_pages = config.extent_pages.min(scan_pages).max(1);
	let extents = extent_inputs(scan_pages, extent_pages);
	let direct_file = Arc::new(DirectFile::open_read_only(&dataset.path)?);
	#[cfg(all(windows, feature = "native-async-io"))]
	let scatter_file = Arc::new(DirectFile::open_read_write_with_mode(
		&dataset.path,
		DirectFileOpenMode::Overlapped,
	)?);
	let candidates = prefetch_candidates();
	for repetition in 0..io_repetitions(config) {
		for (order_position, (candidate, depth)) in
			rotated_order(&candidates, repetition).into_iter().enumerate()
		{
			let measurement = match candidate {
				"T0_direct" => {
					measure_direct_prefetch(Arc::clone(&direct_file), &extents, depth, scan_pages)?
				}
				#[cfg(all(windows, feature = "native-async-io"))]
				"T3_scatter_reusable_worker_context" => measure_scatter_prefetch(
					Arc::clone(&scatter_file),
					&extents,
					depth,
					scan_pages,
				)?,
				_ => return Err(io::Error::other("unknown prefetch candidate").into()),
			};
			artifacts.sample(
				SampleMeta {
					phase: "prefetch",
					workload: "sequential_batched",
					candidate,
					repetition,
					order_position,
					dataset_pages: scan_pages as u64,
					pool_pages: config.pool_pages,
					hot_pages: 0,
					file_count: 1,
					threads: 2_usize.min(depth + 1),
					queue_depth: 1,
					extent_pages,
					prefetch_depth: depth,
					vm_reserved_bytes: 0,
					vm_committed_bytes: 0,
					hit_ratio: 0.0,
				},
				measurement,
			)?;
		}
	}
	writeln!(
		artifacts.log,
		"prefetch_latency_scope=aggregate scan only; p50/p95/p99 CSV fields are zero because \
		 per-extent timing would perturb the pipeline; each repetition rotates the immutable base \
		 interleaved transport/depth order independently; rss_delta is the post-scan process \
		 delta from before reusable-set allocation until after cursor join/drop while those sets \
		 remain owned, excludes immutable Dataset/file controls, and is not peak RSS; depth 0 \
		 owns one max-extent set and depth N owns exactly N+1 sets, with recycling verified after \
		 successful finish and by deterministic pool tests"
	)?;
	#[cfg(all(windows, feature = "native-async-io"))]
	writeln!(
		artifacts.log,
		"scatter_prefetch_context_scope=one reusable ScatterReader event/OVERLAPPED context per \
		 scan; the non-Send context is constructed and retained for the worker thread lifetime"
	)?;
	Ok(())
}

/// Compares fully validated Direct I/O with mmap while recording Windows page
/// faults. The generated file is written through NO_BUFFERING, and Direct runs
/// first so it cannot prepopulate the system file cache used by mmap.
#[cfg(windows)]
#[test]
#[ignore = "large Windows page-fault benchmark; run explicitly in profiling profile"]
fn storage_engine_prototype_page_fault_probe() -> Result<(), Box<dyn Error>> {
	let config = HarnessConfig::from_env()?;
	let mut artifacts = Artifacts::open(config)?;
	let mut candidates = vec!["T0_raw_preallocated", "T0_validated_preallocated"];
	#[cfg(feature = "native-async-io")]
	candidates.push("H11_production_scatter_p1");
	candidates.push("R0_mmap_validated_first_touch");
	let selector = candidate_selector_from_env("page_fault", &candidates)?;
	if let Some(selected) = selector.as_deref() {
		candidates.retain(|candidate| *candidate == selected);
	}
	artifacts.selector("page_fault", selector.as_deref())?;

	let dataset = Dataset::create("page-fault", config.dataset_pages)?;
	let direct = DirectFile::open_read_only(&dataset.path)?;
	#[cfg(feature = "native-async-io")]
	let overlapped = Arc::new(DirectFile::open_read_write_with_mode(
		&dataset.path,
		DirectFileOpenMode::Overlapped,
	)?);
	#[cfg(feature = "native-async-io")]
	let hybrid = HybridReadEngine::new(
		NonZeroUsize::new(config.pool_pages)
			.ok_or_else(|| io::Error::other("page-fault probe requires a nonzero pool"))?,
	);
	let mmap = MmapControl::map_unvalidated(&dataset)?;
	let scan_pages = usize::try_from(config.dataset_pages)?;
	let extent_pages = config.extent_pages.min(scan_pages).max(1);
	let extents = extent_inputs(scan_pages, extent_pages);
	let mut scratch = aligned_pages(maximum_extent_pages(&extents)?);
	let ram_bytes = System::new_all().total_memory();
	let dataset_bytes = config.dataset_pages.saturating_mul(PAGE_SIZE as u64);
	let mut page_fault_csv = open_page_fault_csv()?;
	wait_for_profiler_gate()?;

	for repetition in 0..io_repetitions(config) {
		for (order_position, candidate) in candidates.iter().copied().enumerate() {
			let before = process_memory_snapshot()?;
			let (mut measurement, prefetch_depth) = match candidate {
				"T0_raw_preallocated" => {
					(measure_raw_direct_scan(&direct, &extents, &mut scratch, scan_pages)?, 0)
				}
				"T0_validated_preallocated" => {
					(measure_validated_direct_scan(&direct, &extents, &mut scratch, scan_pages)?, 0)
				}
				#[cfg(feature = "native-async-io")]
				"H11_production_scatter_p1" => {
					(measure_hybrid_scatter(&hybrid, Arc::clone(&overlapped), scan_pages)?, 1)
				}
				"R0_mmap_validated_first_touch" => (measure_validated_mmap_scan(&mmap)?, 0),
				_ => return Err(io::Error::other("unknown page-fault candidate").into()),
			};
			let after = process_memory_snapshot()?;
			measurement.rss_delta =
				after.working_set_bytes.saturating_sub(before.working_set_bytes);
			record_page_fault_sample(
				&mut page_fault_csv,
				candidate,
				repetition,
				config,
				extent_pages,
				ram_bytes,
				measurement,
				before,
				after,
			)?;
			artifacts.sample(
				SampleMeta {
					phase: "page_fault",
					workload: "full_validated_scan",
					candidate,
					repetition,
					order_position,
					dataset_pages: config.dataset_pages,
					pool_pages: 0,
					hot_pages: 0,
					file_count: 1,
					threads: if prefetch_depth == 0 {
						1
					} else {
						2
					},
					queue_depth: 1,
					extent_pages,
					prefetch_depth,
					vm_reserved_bytes: 0,
					vm_committed_bytes: 0,
					hit_ratio: 0.0,
				},
				measurement,
			)?;
			writeln!(
				artifacts.log,
				"page_fault candidate={candidate} repetition={repetition} faults={} \
				 working_set_before={} working_set_after={}",
				after.page_fault_count.wrapping_sub(before.page_fault_count),
				before.working_set_bytes,
				after.working_set_bytes,
			)?;
		}
	}
	writeln!(
		artifacts.log,
		"page_fault_contract=dataset_bytes={dataset_bytes} ram_bytes={ram_bytes} ratio={:.6}; \
		 PageFaultCount is the cumulative process total and does not distinguish soft from hard \
		 faults; candidate order is fixed raw/validated/scatter Direct before mmap because \
		 NO_BUFFERING Direct reads do not populate mmap's system file cache",
		dataset_bytes as f64 / ram_bytes.max(1) as f64,
	)?;
	Ok(())
}

/// Screens the mandatory compatible engine, policy, transport, and prefetch
/// combinations without constructing the full Cartesian product.
#[cfg(all(windows, feature = "native-async-io"))]
#[test]
#[ignore = "native Windows storage combination benchmark; run explicitly in release profile"]
fn storage_engine_prototype_combination_probe() -> Result<(), Box<dyn Error>> {
	let config = HarnessConfig::from_env()?;
	let mut artifacts = Artifacts::open(config)?;
	let dataset = Dataset::create("combinations", config.dataset_pages)?;
	let direct = DirectFile::open_read_only(&dataset.path)?;
	let overlapped = Arc::new(DirectFile::open_read_write_with_mode(
		&dataset.path,
		DirectFileOpenMode::Overlapped,
	)?);
	let pool = NonZeroUsize::new(config.pool_pages).ok_or("zero pool")?;
	let policy_scan_pages = usize::try_from(config.dataset_pages)?
		.min(config.pool_pages.saturating_mul(4).max(config.extent_pages));
	let transport_scan_pages = usize::try_from(config.dataset_pages)?;
	let extent_pages = config.extent_pages.min(transport_scan_pages).max(1);
	let candidates = combination_candidates();

	for repetition in 0..io_repetitions(config) {
		for (order_position, candidate) in
			rotated_order(&candidates, repetition).into_iter().enumerate()
		{
			let label = candidate.label();
			let (measurement, hit_ratio, prefetch_depth, queue_depth, vm_reserved, vm_committed) =
				match candidate {
					CombinationCandidate::RawDirect => (
						measure_transport(
							"T0_raw",
							&overlapped,
							extent_pages,
							transport_scan_pages,
						)?,
						0.0,
						0,
						1,
						0,
						0,
					),
					CombinationCandidate::ValidatedDirect => (
						measure_transport(
							"T0_validated",
							&overlapped,
							extent_pages,
							transport_scan_pages,
						)?,
						0.0,
						0,
						1,
						0,
						0,
					),
					CombinationCandidate::AtomicPolicy(policy) => {
						let (measurement, hit_ratio) = measure_atomic_policy_combination(
							config,
							&dataset,
							&direct,
							pool,
							policy,
							policy_scan_pages,
							repetition,
						)?;
						(measurement, hit_ratio, 0, 1, 0, 0)
					}
					CombinationCandidate::VmPolicy(policy) => {
						let (measurement, hit_ratio, reserved, committed) =
							measure_vm_policy_combination(
								config,
								&dataset,
								&direct,
								pool,
								policy,
								policy_scan_pages,
								repetition,
							)?;
						(measurement, hit_ratio, 0, 1, reserved, committed)
					}
					CombinationCandidate::BypassScatter {
						depth,
					} => (
						measure_scatter_prefetch(
							Arc::clone(&overlapped),
							&extent_inputs(transport_scan_pages, extent_pages),
							depth,
							transport_scan_pages,
						)?,
						0.0,
						depth,
						1,
						0,
						0,
					),
					CombinationCandidate::AtomicColdScatter {
						depth,
					} => (
						measure_atomic_cold_direct_prefetch(
							&dataset,
							Arc::clone(&overlapped),
							config.pool_pages,
							extent_pages,
							transport_scan_pages,
							depth,
							ColdDirectTransport::Scatter,
						)?,
						0.0,
						depth,
						1,
						0,
						0,
					),
					CombinationCandidate::AtomicColdIoRing {
						depth,
					} => (
						measure_atomic_cold_direct_prefetch(
							&dataset,
							Arc::clone(&overlapped),
							config.pool_pages,
							extent_pages,
							transport_scan_pages,
							depth,
							ColdDirectTransport::FrameIoRing,
						)?,
						0.0,
						depth,
						extent_pages,
						0,
						0,
					),
				};
			artifacts.sample(
				SampleMeta {
					phase: "combination",
					workload: "one_shot_sequential",
					candidate: &label,
					repetition,
					order_position,
					dataset_pages: match candidate {
						CombinationCandidate::RawDirect
						| CombinationCandidate::ValidatedDirect
						| CombinationCandidate::BypassScatter {
							..
						}
						| CombinationCandidate::AtomicColdScatter {
							..
						}
						| CombinationCandidate::AtomicColdIoRing {
							..
						} => transport_scan_pages as u64,
						CombinationCandidate::AtomicPolicy(_)
						| CombinationCandidate::VmPolicy(_) => policy_scan_pages as u64,
					},
					pool_pages: config.pool_pages,
					hot_pages: 0,
					file_count: 1,
					threads: if prefetch_depth == 0 {
						1
					} else {
						2
					},
					queue_depth,
					extent_pages: if matches!(
						candidate,
						CombinationCandidate::AtomicPolicy(_) | CombinationCandidate::VmPolicy(_)
					) {
						1
					} else {
						extent_pages
					},
					prefetch_depth,
					vm_reserved_bytes: vm_reserved,
					vm_committed_bytes: vm_committed,
					hit_ratio,
				},
				measurement,
			)?;
		}
	}

	writeln!(
		artifacts.log,
		"combination_order=one immutable interleaved mandatory-candidate base is independently \
		 rotated for each repetition; no full Cartesian product is constructed"
	)?;
	writeln!(
		artifacts.log,
		"combination_budget=buffer_pool_pages is one global budget; A2 direct-to-frame owns an \
		 inactive 5/8 hot Atomic arena plus the measured 3/8 cold arena; cold resident/loading \
		 frames never exceed that partition"
	)?;
	writeln!(
		artifacts.log,
		"combination_prefetch=depth 0 is the synchronous direct-to-frame control; depth N uses \
		 one scoped worker, an ordered consumer, and a bounded sync_channel of N-1 completed \
		 extents, so at most N+1 extents can be worker/queue/consumer-owned; checksum validation \
		 and all-or-none publication occur only in the consumer"
	)?;
	writeln!(
		artifacts.log,
		"combination_bypass=A1 scatter reads into exactly one reusable set at depth 0 or N+1 \
		 reusable sets at depth N; no resident arena is admitted or polluted; rss_delta begins \
		 before candidate-owned engines/buffers and ends while they remain alive"
	)?;
	Ok(())
}

/// Direct transport used to fill the A2 cold arena.
#[cfg(all(windows, feature = "native-async-io"))]
#[derive(Debug, Clone, Copy)]
enum ColdDirectTransport {
	/// One `ReadFileScatter` request per extent.
	Scatter,
	/// One IoRing SQE per database frame.
	FrameIoRing,
}

/// Returns the exact mandatory combination base, interleaved by engine and
/// then by prefetch depth.
#[cfg(all(windows, feature = "native-async-io"))]
fn combination_candidates() -> Vec<CombinationCandidate> {
	let mut candidates =
		Vec::with_capacity(2 + READ_POLICIES.len() * 2 + PREFETCH_DEPTHS.len() * 3);
	candidates.push(CombinationCandidate::RawDirect);
	candidates.push(CombinationCandidate::ValidatedDirect);
	for policy in READ_POLICIES {
		candidates.push(CombinationCandidate::AtomicPolicy(policy));
		candidates.push(CombinationCandidate::VmPolicy(policy));
	}
	for depth in PREFETCH_DEPTHS {
		candidates.push(CombinationCandidate::BypassScatter {
			depth,
		});
		candidates.push(CombinationCandidate::AtomicColdScatter {
			depth,
		});
		candidates.push(CombinationCandidate::AtomicColdIoRing {
			depth,
		});
	}
	candidates
}

/// Runs one ordered bounded producer/consumer pipeline and always joins its
/// scoped worker before returning.
fn run_scoped_ordered_prefetch<Inputs, Input, Output, Loader, Consumer>(
	inputs: Inputs,
	depth: NonZeroUsize,
	mut loader: Loader,
	mut consumer: Consumer,
) -> FerrosGrynnResult<usize>
where
	Inputs: IntoIterator<Item = Input>,
	Inputs::IntoIter: Send,
	Input: Send,
	Output: Send,
	Loader: FnMut(Input) -> FerrosGrynnResult<Output> + Send,
	Consumer: FnMut(Output) -> FerrosGrynnResult<()>,
{
	let cancelled = AtomicBool::new(false);
	thread::scope(|scope| {
		let (sender, receiver) = std::sync::mpsc::sync_channel(depth.get().saturating_sub(1));
		let worker_cancelled = &cancelled;
		let inputs = inputs.into_iter();
		let worker = scope.spawn(move || -> FerrosGrynnResult<()> {
			for input in inputs {
				if worker_cancelled.load(Ordering::Acquire) {
					break;
				}
				let loaded = loader(input);
				let terminal = loaded.is_err();
				if sender.send(loaded).is_err() || terminal {
					break;
				}
			}
			Ok(())
		});

		let mut consumed = 0_usize;
		let mut consumer_result = Ok(());
		while let Ok(loaded) = receiver.recv() {
			match loaded.and_then(&mut consumer) {
				Ok(()) => consumed = consumed.saturating_add(1),
				Err(error) => {
					consumer_result = Err(error);
					break;
				}
			}
		}
		cancelled.store(true, Ordering::Release);
		drop(receiver);
		let worker_result = worker.join().map_err(|payload| {
			drop(payload);
			FerrosGrynnError::storage_io_failure(
				"scoped direct-to-frame prefetch worker panicked during join",
			)
		})?;
		consumer_result?;
		worker_result?;
		Ok(consumed)
	})
}

/// Measures one Atomic-arena admission policy with the same sequential input.
#[cfg(all(windows, feature = "native-async-io"))]
#[allow(clippy::too_many_arguments, reason = "benchmark dimensions remain explicit inputs")]
fn measure_atomic_policy_combination(
	config: HarnessConfig,
	dataset: &Dataset,
	direct: &DirectFile,
	pool: NonZeroUsize,
	policy: ReadPolicy,
	scan_pages: usize,
	repetition: usize,
) -> Result<(Measurement, f64), Box<dyn Error>> {
	let rss_before = process_rss()?;
	let cache = PrototypeReadCache::for_policy(pool, policy);
	let cursor = PrototypeScanCursor::with_policy(
		ReadIntent::SequentialScan {
			scan_id: repetition as u64,
			expected_pages: scan_pages as u64,
			one_shot: true,
		},
		Some(policy),
	);
	let loads = AtomicU64::new(0);
	let measurement = measure_operations(
		scan_pages as u64,
		config.latency_batch.min(scan_pages).max(1),
		PAGE_SIZE as u64,
		|operation| {
			let page_no = PageNo::new(operation % scan_pages as u64);
			let key = dataset.key(page_no);
			cache.read_with(
				&cursor,
				key,
				|page| {
					loads.fetch_add(1, Ordering::Relaxed);
					read_raw_reported(dataset, direct, page_no, page)
				},
				|page| {
					black_box(page.as_bytes()[PAGE_HEADER_SIZE]);
					Ok(())
				},
			)?;
			Ok(())
		},
	)?;
	let measurement = finish_fixture_rss(measurement, rss_before)?;
	if policy == ReadPolicy::RocksLikeBypass && cache.hot_resident_count() != 0 {
		return Err(io::Error::other("Atomic bypass combination polluted the hot arena").into());
	}
	if cache.cold_resident_count() > cache.cold_capacity() {
		return Err(io::Error::other("Atomic cold combination exceeded its 3/8 budget").into());
	}
	let hit_ratio = 1.0 - loads.load(Ordering::Relaxed) as f64 / scan_pages as f64;
	Ok((measurement, hit_ratio))
}

/// Measures one fixed-address VM admission policy with the same sequential
/// input and explicit reserved/committed accounting.
#[cfg(all(windows, feature = "native-async-io"))]
#[allow(clippy::too_many_arguments, reason = "benchmark dimensions remain explicit inputs")]
fn measure_vm_policy_combination(
	config: HarnessConfig,
	dataset: &Dataset,
	direct: &DirectFile,
	pool: NonZeroUsize,
	policy: ReadPolicy,
	scan_pages: usize,
	repetition: usize,
) -> Result<(Measurement, f64, usize, usize), Box<dyn Error>> {
	let source_pages =
		NonZeroUsize::new(usize::try_from(dataset.page_count)?).ok_or("empty VM policy dataset")?;
	let source = HomeSourceSpec::from_key(dataset.key(PageNo::new(0)), source_pages)?;
	let home_range_bytes = source_pages
		.get()
		.checked_mul(PAGE_SIZE)
		.ok_or_else(|| io::Error::other("VM policy address-range byte count overflow"))?;
	let tier_count = if policy == ReadPolicy::InnodbLikeCold && pool.get() >= 3 {
		2
	} else {
		1
	};
	let reserved_bytes = home_range_bytes
		.checked_mul(tier_count)
		.ok_or_else(|| io::Error::other("VM policy reserved byte count overflow"))?;
	let rss_before = process_rss()?;
	let cache = VmPrototypeReadCache::for_policy(&[source], pool, policy)?;
	let cursor = PrototypeScanCursor::with_policy(
		ReadIntent::SequentialScan {
			scan_id: repetition as u64,
			expected_pages: scan_pages as u64,
			one_shot: true,
		},
		Some(policy),
	);
	let loads = AtomicU64::new(0);
	let measurement = measure_operations(
		scan_pages as u64,
		config.latency_batch.min(scan_pages).max(1),
		PAGE_SIZE as u64,
		|operation| {
			let page_no = PageNo::new(operation % scan_pages as u64);
			let key = dataset.key(page_no);
			cache.read_with(
				&cursor,
				key,
				|page| {
					loads.fetch_add(1, Ordering::Relaxed);
					read_raw_reported(dataset, direct, page_no, page)
				},
				|page| {
					black_box(page.as_bytes()[PAGE_HEADER_SIZE]);
					Ok(())
				},
			)?;
			Ok(())
		},
	)?;
	let measurement = finish_fixture_rss(measurement, rss_before)?;
	if policy == ReadPolicy::RocksLikeBypass && cache.hot_resident_count() != 0 {
		return Err(io::Error::other("VM bypass combination polluted the hot cache").into());
	}
	if cache.cold_resident_count() > cache.cold_capacity() {
		return Err(io::Error::other("VM cold combination exceeded its 3/8 budget").into());
	}
	let committed_bytes = cache.committed_bytes();
	let hit_ratio = 1.0 - loads.load(Ordering::Relaxed) as f64 / scan_pages as f64;
	Ok((measurement, hit_ratio, reserved_bytes, committed_bytes))
}

/// Measures an A2 cold partition filled directly by scatter or frame IoRing,
/// with synchronous depth zero and a scoped bounded pipeline otherwise.
#[cfg(all(windows, feature = "native-async-io"))]
#[allow(clippy::too_many_arguments, reason = "benchmark dimensions remain explicit inputs")]
fn measure_atomic_cold_direct_prefetch(
	dataset: &Dataset,
	file: Arc<DirectFile>,
	pool_pages: usize,
	extent_pages: usize,
	scan_pages: usize,
	depth: usize,
	transport: ColdDirectTransport,
) -> Result<Measurement, Box<dyn Error>> {
	let cold_capacity = pool_pages.saturating_mul(3) / 8;
	if cold_capacity == 0 {
		return Err(
			io::Error::other("A2 direct-to-frame requires a nonzero 3/8 cold budget").into()
		);
	}
	let hot_capacity = pool_pages.saturating_sub(cold_capacity).max(1);
	let extent_pages = extent_pages.min(cold_capacity).min(scan_pages).max(1);
	let extents = extent_inputs(scan_pages, extent_pages);
	let rss_before = process_rss()?;
	let hot =
		AtomicArenaBufferPool::new(NonZeroUsize::new(hot_capacity).ok_or("zero A2 hot capacity")?);
	let cold = AtomicArenaBufferPool::new(
		NonZeroUsize::new(cold_capacity).ok_or("zero A2 cold capacity")?,
	);

	let timer = MeasurementTimer::start()?;
	if depth == 0 {
		match transport {
			ColdDirectTransport::Scatter => {
				let mut scatter = ScatterReader::new()?;
				for (first, count) in extents.iter().copied() {
					let keys = extent_keys(dataset, first, count);
					let mut batch = cold.reserve_batch(&keys)?;
					let mut pages = batch.pages_mut();
					scatter.read_raw(&file, first, &mut pages)?;
					drop(pages);
					let guards = batch.publish()?;
					black_box(guards[0].as_bytes()[PAGE_HEADER_SIZE]);
					drop(guards);
				}
			}
			ColdDirectTransport::FrameIoRing => {
				let mut ring = WindowsIoRing::new()?;
				for (first, count) in extents.iter().copied() {
					let keys = extent_keys(dataset, first, count);
					let mut batch = cold.reserve_batch(&keys)?;
					let mut pages = batch.pages_mut();
					let mut reads = pages
						.iter_mut()
						.enumerate()
						.map(|(index, page)| {
							FrameRead::new(&file, PageNo::new(first.get() + index as u64), page)
						})
						.collect::<Vec<_>>();
					ring.read_frames(&mut reads)?;
					drop(reads);
					drop(pages);
					let guards = batch.publish()?;
					black_box(guards[0].as_bytes()[PAGE_HEADER_SIZE]);
					drop(guards);
				}
			}
		}
	} else {
		let depth = NonZeroUsize::new(depth).ok_or("zero A2 prefetch depth")?;
		let consumed = match transport {
			ColdDirectTransport::Scatter => {
				let worker_file = Arc::clone(&file);
				run_scoped_ordered_prefetch(
					extents.iter().copied(),
					depth,
					|(first, count)| {
						let keys = extent_keys(dataset, first, count);
						let mut batch = cold.reserve_batch(&keys)?;
						PREFETCH_SCATTER.with(|slot| {
							let mut slot = slot.borrow_mut();
							if slot.is_none() {
								*slot = Some(ScatterReader::new()?);
							}
							let scatter = slot.as_mut().ok_or_else(|| {
								FerrosGrynnError::storage_io_failure(
									"cold scatter worker context was not initialized",
								)
							})?;
							let mut pages = batch.pages_mut();
							scatter.read_raw(&worker_file, first, &mut pages)?;
							drop(pages);
							Ok(batch)
						})
					},
					|batch| {
						let guards = batch.publish()?;
						black_box(guards[0].as_bytes()[PAGE_HEADER_SIZE]);
						drop(guards);
						Ok(())
					},
				)?
			}
			ColdDirectTransport::FrameIoRing => {
				let worker_file = Arc::clone(&file);
				let cold_ref = &cold;
				let dataset_ref = dataset;
				let mut ring = WindowsIoRing::new()?;
				run_scoped_ordered_prefetch(
					extents.iter().copied(),
					depth,
					move |(first, count)| {
						let keys = extent_keys(dataset_ref, first, count);
						let mut batch = cold_ref.reserve_batch(&keys)?;
						let mut pages = batch.pages_mut();
						let mut reads = pages
							.iter_mut()
							.enumerate()
							.map(|(index, page)| {
								FrameRead::new(
									&worker_file,
									PageNo::new(first.get() + index as u64),
									page,
								)
							})
							.collect::<Vec<_>>();
						ring.read_frames(&mut reads)?;
						drop(reads);
						drop(pages);
						Ok(batch)
					},
					|batch| {
						let guards = batch.publish()?;
						black_box(guards[0].as_bytes()[PAGE_HEADER_SIZE]);
						drop(guards);
						Ok(())
					},
				)?
			}
		};
		if consumed != extents.len() {
			return Err(io::Error::other("A2 prefetch did not consume every extent").into());
		}
	}
	let timing = timer.finish()?;
	if cold.resident_count() > cold_capacity {
		return Err(io::Error::other("A2 direct-to-frame exceeded its cold frame budget").into());
	}
	black_box(hot.capacity());
	let rss_after = process_rss()?;
	Ok(aggregate_measurement(
		timing,
		extents.len() as u64,
		scan_pages as u64 * PAGE_SIZE as u64,
		rss_after.saturating_sub(rss_before),
	))
}

/// Builds exact page keys for one contiguous extent.
#[cfg(all(windows, feature = "native-async-io"))]
fn extent_keys(dataset: &Dataset, first: PageNo, count: usize) -> Vec<PageCacheKey> {
	(0..count).map(|offset| dataset.key(PageNo::new(first.get() + offset as u64))).collect()
}

/// Measures one complete synchronous Direct scan into a preallocated extent.
fn measure_raw_direct_scan(
	file: &DirectFile,
	extents: &[(PageNo, usize)],
	pages: &mut [AlignedPage],
	scan_pages: usize,
) -> Result<Measurement, Box<dyn Error>> {
	let timer = MeasurementTimer::start()?;
	for (first, count) in extents.iter().copied() {
		file.read_pages_raw(first, &mut pages[..count])?;
		black_box(pages[0].as_bytes()[PAGE_HEADER_SIZE]);
	}
	Ok(aggregate_measurement(
		timer.finish()?,
		extents.len() as u64,
		scan_pages as u64 * PAGE_SIZE as u64,
		0,
	))
}

/// Measures one complete synchronous Direct scan into a preallocated extent.
fn measure_validated_direct_scan(
	file: &DirectFile,
	extents: &[(PageNo, usize)],
	pages: &mut [AlignedPage],
	scan_pages: usize,
) -> Result<Measurement, Box<dyn Error>> {
	let timer = MeasurementTimer::start()?;
	for (first, count) in extents.iter().copied() {
		file.read_pages_raw(first, &mut pages[..count])?;
		validate_extent(first, &pages[..count])?;
		black_box(pages[0].as_bytes()[PAGE_HEADER_SIZE]);
	}
	Ok(aggregate_measurement(
		timer.finish()?,
		extents.len() as u64,
		scan_pages as u64 * PAGE_SIZE as u64,
		0,
	))
}

/// Measures one complete mapped scan including checksum validation and faults.
#[cfg(windows)]
fn measure_validated_mmap_scan(control: &MmapControl) -> Result<Measurement, Box<dyn Error>> {
	let timer = MeasurementTimer::start()?;
	control.validate_all()?;
	Ok(aggregate_measurement(
		timer.finish()?,
		control.page_count,
		control.page_count.saturating_mul(PAGE_SIZE as u64),
		0,
	))
}

/// Measures one complete contiguous Direct prefetch scan.
fn measure_direct_prefetch(
	file: Arc<DirectFile>,
	extents: &[(PageNo, usize)],
	depth: usize,
	scan_pages: usize,
) -> Result<Measurement, Box<dyn Error>> {
	let rss_before = process_rss()?;
	let max_extent_pages = maximum_extent_pages(extents)?;
	if depth == 0 {
		let mut pages = aligned_pages(max_extent_pages);
		let mut measurement =
			measure_validated_direct_scan(&file, extents, &mut pages, scan_pages)?;
		let rss_after = process_rss()?;
		measurement.rss_delta = rss_after.saturating_sub(rss_before);
		return Ok(measurement);
	} else {
		let set_count = depth
			.checked_add(1)
			.ok_or_else(|| io::Error::other("direct prefetch buffer-set count overflow"))?;
		let pool = ReusableBufferPool::with_factory(set_count, || aligned_pages(max_extent_pages));
		let worker_file = Arc::clone(&file);
		let worker_pool = pool.clone();
		let timer = MeasurementTimer::start()?;
		let mut cursor = PrototypePrefetchCursor::new(
			extents.to_vec(),
			NonZeroUsize::new(depth).ok_or("zero direct prefetch depth")?,
			move |(first, count)| {
				let mut buffer = worker_pool.take()?;
				worker_file.read_pages_raw(first, &mut buffer.buffer_mut()?[..count])?;
				Ok((first, count, buffer))
			},
		);
		while let Some(result) = cursor.next() {
			let (first, count, buffer) = result?;
			let pages = &buffer.buffer()?[..count];
			validate_extent(first, pages)?;
			black_box(pages[0].as_bytes()[PAGE_HEADER_SIZE]);
			drop(buffer);
		}
		cursor.finish()?;
		let timing = timer.finish()?;
		pool.ensure_all_returned()?;
		let rss_after = process_rss()?;
		return Ok(aggregate_measurement(
			timing,
			extents.len() as u64,
			scan_pages as u64 * PAGE_SIZE as u64,
			rss_after.saturating_sub(rss_before),
		));
	}
}

/// Measures one candidate's complete sequential scan.
#[cfg(all(windows, feature = "native-async-io"))]
fn measure_transport(
	candidate: &str,
	file: &Arc<DirectFile>,
	extent_pages: usize,
	scan_pages: usize,
) -> Result<Measurement, Box<dyn Error>> {
	let rss_before = process_rss()?;
	let mut contiguous = if candidate == "T3_scatter" {
		Vec::new()
	} else {
		aligned_pages(extent_pages)
	};
	let mut disjoint = if candidate == "T3_scatter" {
		(0..extent_pages).map(|_| Box::new(AlignedPage::zeroed())).collect::<Vec<_>>()
	} else {
		Vec::new()
	};
	let mut ring = (candidate == "T1_ioring_extent").then(WindowsIoRing::new).transpose()?;
	let mut scatter = (candidate == "T3_scatter").then(ScatterReader::new).transpose()?;
	let extent_count = scan_pages.div_ceil(extent_pages);
	let measurement = measure_variable_operations(extent_count as u64, 1, |operation| {
		let first_raw = operation as usize * extent_pages;
		let count = (scan_pages - first_raw).min(extent_pages);
		let first = PageNo::new(first_raw as u64);
		match candidate {
			"T0_raw" => file.read_pages_raw(first, &mut contiguous[..count])?,
			"T0_validated" => {
				file.read_pages_raw(first, &mut contiguous[..count])?;
				validate_extent(first, &contiguous[..count])?;
			}
			"T1_ioring_extent" => {
				let extent = FilePageExtent::new(Arc::clone(file), first, 0, count);
				ring.as_mut()
					.ok_or_else(|| io::Error::other("extent IoRing context is absent"))?
					.read_extents_forced(std::slice::from_ref(&extent), &mut contiguous[..count])?;
				validate_extent(first, &contiguous[..count])?;
			}
			"T3_scatter" => {
				let mut pages = disjoint[..count].iter_mut().map(Box::as_mut).collect::<Vec<_>>();
				scatter
					.as_mut()
					.ok_or_else(|| io::Error::other("scatter context is absent"))?
					.read_and_validate(file, first, &mut pages)?;
			}
			_ => return Err(io::Error::other("unknown transport candidate").into()),
		}
		Ok((count * PAGE_SIZE) as u64)
	})?;
	finish_fixture_rss(measurement, rss_before)
}

/// Measures the production Rocks-like scatter path, including worker startup,
/// ordered consumption, cancellation-safe join, and checksum validation.
#[cfg(all(windows, feature = "native-async-io"))]
fn measure_hybrid_scatter(
	engine: &HybridReadEngine,
	file: Arc<DirectFile>,
	scan_pages: usize,
) -> Result<Measurement, Box<dyn Error>> {
	let rss_before = process_rss()?;
	let extent_pages = engine.scatter_extent_capacity();
	if extent_pages < 2 {
		return Err(io::Error::other("hybrid transport budget cannot hold scatter P1").into());
	}
	let extent_count = scan_pages.div_ceil(extent_pages);
	let timer = MeasurementTimer::start()?;
	let mut cursor = engine.scatter_cursor(file, PageNo::new(0), scan_pages)?;
	let mut completed_pages = 0_usize;
	let mut samples = Vec::with_capacity(extent_count);
	loop {
		let extent_started = Instant::now();
		let Some(extent) = cursor.next_extent() else {
			break;
		};
		let extent = extent?;
		if extent.first_page() != PageNo::new(u64::try_from(completed_pages)?) {
			return Err(io::Error::other("production scatter cursor changed page order").into());
		}
		let page_count = extent.pages().len();
		for page in extent.pages() {
			black_box(page.as_bytes()[PAGE_HEADER_SIZE]);
		}
		completed_pages = completed_pages
			.checked_add(page_count)
			.ok_or_else(|| io::Error::other("production scatter page count overflow"))?;
		samples.push(u64::try_from(extent_started.elapsed().as_nanos()).unwrap_or(u64::MAX));
	}
	cursor.finish()?;
	let timing = timer.finish()?;
	if completed_pages != scan_pages {
		return Err(io::Error::other("production scatter cursor returned a partial scan").into());
	}
	let rss_after = process_rss()?;
	let mut measurement = aggregate_measurement(
		timing,
		extent_count as u64,
		scan_pages as u64 * PAGE_SIZE as u64,
		rss_after.saturating_sub(rss_before),
	);
	samples.sort_unstable();
	measurement.p50_ns = percentile(&samples, 50);
	measurement.p95_ns = percentile(&samples, 95);
	measurement.p99_ns = percentile(&samples, 99);
	Ok(measurement)
}

/// Returns the explicit requested frame-SQE batch encoded by a candidate.
#[cfg(all(windows, feature = "native-async-io"))]
fn requested_ioring_qd(candidate: &str) -> Option<usize> {
	match candidate {
		"T2_ioring_frame_requested_qd1" => Some(1),
		"T2_ioring_frame_requested_qd8" => Some(8),
		"T2_ioring_frame_requested_qd32" => Some(32),
		"T2_ioring_frame_requested_qd128" => Some(128),
		_ => None,
	}
}

/// Measures one IoRing SQE per frame at an explicit requested submission batch.
#[cfg(all(windows, feature = "native-async-io"))]
fn measure_frame_ioring_requested_qd(
	file: &Arc<DirectFile>,
	requested_qd: usize,
	scan_pages: usize,
) -> Result<(Measurement, IoRingProbeStats), Box<dyn Error>> {
	let rss_before = process_rss()?;
	let mut frames = (0..requested_qd).map(|_| Box::new(AlignedPage::zeroed())).collect::<Vec<_>>();
	let mut ring = WindowsIoRing::new()?;
	let submission_count = scan_pages.div_ceil(requested_qd);
	let requested_limit = NonZeroUsize::new(requested_qd).ok_or("zero requested IoRing batch")?;
	let mut aggregate_stats: Option<IoRingProbeStats> = None;
	let measurement = measure_variable_operations(submission_count as u64, 1, |operation| {
		let first_raw = operation as usize * requested_qd;
		let count = (scan_pages - first_raw).min(requested_qd);
		let first = PageNo::new(first_raw as u64);
		let mut reads = frames[..count]
			.iter_mut()
			.enumerate()
			.map(|(index, frame)| {
				FrameRead::new(file, PageNo::new(first.get() + index as u64), frame)
			})
			.collect::<Vec<_>>();
		let stats = ring.read_frames_measured(&mut reads, requested_limit)?;
		if let Some(aggregate) = &mut aggregate_stats {
			aggregate.merge(stats)?;
		} else {
			aggregate_stats = Some(IoRingProbeStats::from_submission(stats));
		}
		for read in &reads {
			read.validate()?;
		}
		Ok((count * PAGE_SIZE) as u64)
	})?;
	let stats =
		aggregate_stats.ok_or_else(|| io::Error::other("IoRing scan submitted no frames"))?;
	if stats.submitted_sqes != scan_pages || stats.completed_cqes != scan_pages {
		return Err(io::Error::other("IoRing scan accounting does not cover every frame").into());
	}
	let measurement = finish_fixture_rss(measurement, rss_before)?;
	Ok((measurement, stats))
}

/// Measures direct-to-frame all-or-none publication combinations.
#[cfg(all(windows, feature = "native-async-io"))]
fn measure_direct_to_frame(
	candidate: &str,
	dataset: &Dataset,
	file: Arc<DirectFile>,
	pool_pages: usize,
	extent_pages: usize,
	scan_pages: usize,
) -> Result<(Measurement, usize, usize), Box<dyn Error>> {
	let rss_before = process_rss()?;
	let capacity = NonZeroUsize::new(pool_pages).ok_or("zero pool")?;
	let extent_count = scan_pages.div_ceil(extent_pages);
	match candidate {
		"atomic_scratch_copy_publish" => {
			let atomic = AtomicArenaBufferPool::new(capacity);
			let mut scratch = aligned_pages(extent_pages);
			let measurement = measure_variable_operations(extent_count as u64, 1, |operation| {
				let first_raw = operation as usize * extent_pages;
				let count = (scan_pages - first_raw).min(extent_pages);
				let first = PageNo::new(first_raw as u64);
				file.read_pages_raw(first, &mut scratch[..count])?;
				let keys = (0..count)
					.map(|offset| dataset.key(PageNo::new(first.get() + offset as u64)))
					.collect::<Vec<_>>();
				let mut batch = atomic.reserve_batch(&keys)?;
				for (source, target) in scratch[..count].iter().zip(batch.pages_mut()) {
					target.as_bytes_mut().copy_from_slice(source.as_bytes());
				}
				drop(batch.publish()?);
				Ok((count * PAGE_SIZE) as u64)
			})?;
			let measurement = finish_fixture_rss(measurement, rss_before)?;
			Ok((measurement, 0, 0))
		}
		"atomic_scatter_direct" => {
			let atomic = AtomicArenaBufferPool::new(capacity);
			let mut scatter = ScatterReader::new()?;
			let measurement = measure_variable_operations(extent_count as u64, 1, |operation| {
				let first_raw = operation as usize * extent_pages;
				let count = (scan_pages - first_raw).min(extent_pages);
				let first = PageNo::new(first_raw as u64);
				let keys = (0..count)
					.map(|offset| dataset.key(PageNo::new(first.get() + offset as u64)))
					.collect::<Vec<_>>();
				let mut batch = atomic.reserve_batch(&keys)?;
				let mut pages = batch.pages_mut();
				scatter.read_raw(&file, first, &mut pages)?;
				drop(pages);
				drop(batch.publish()?);
				Ok((count * PAGE_SIZE) as u64)
			})?;
			let measurement = finish_fixture_rss(measurement, rss_before)?;
			Ok((measurement, 0, 0))
		}
		"atomic_ioring_frame_direct" => {
			let atomic = AtomicArenaBufferPool::new(capacity);
			let mut ring = WindowsIoRing::new()?;
			let measurement = measure_variable_operations(extent_count as u64, 1, |operation| {
				let first_raw = operation as usize * extent_pages;
				let count = (scan_pages - first_raw).min(extent_pages);
				let first = PageNo::new(first_raw as u64);
				let keys = (0..count)
					.map(|offset| dataset.key(PageNo::new(first.get() + offset as u64)))
					.collect::<Vec<_>>();
				let mut batch = atomic.reserve_batch(&keys)?;
				let mut pages = batch.pages_mut();
				let mut reads = pages
					.iter_mut()
					.enumerate()
					.map(|(index, page)| {
						FrameRead::new(&file, PageNo::new(first.get() + index as u64), page)
					})
					.collect::<Vec<_>>();
				ring.read_frames(&mut reads)?;
				drop(reads);
				drop(pages);
				drop(batch.publish()?);
				Ok((count * PAGE_SIZE) as u64)
			})?;
			let measurement = finish_fixture_rss(measurement, rss_before)?;
			Ok((measurement, 0, 0))
		}
		"vm_scatter_direct" => {
			let source = HomeSourceSpec::from_key(
				dataset.key(PageNo::new(0)),
				NonZeroUsize::new(usize::try_from(dataset.page_count)?).ok_or("empty dataset")?,
			)?;
			let vm = VmCache::new(&[source], capacity, 0)?;
			let mut scatter = ScatterReader::new()?;
			let measurement = measure_variable_operations(extent_count as u64, 1, |operation| {
				let first_raw = operation as usize * extent_pages;
				let count = (scan_pages - first_raw).min(extent_pages);
				let first = PageNo::new(first_raw as u64);
				let keys = (0..count)
					.map(|offset| dataset.key(PageNo::new(first.get() + offset as u64)))
					.collect::<Vec<_>>();
				let mut batch = vm.reserve_home_batch(&keys)?;
				let mut pages = batch.pages_mut()?;
				scatter.read_raw(&file, first, &mut pages)?;
				drop(pages);
				drop(batch.publish()?);
				Ok((count * PAGE_SIZE) as u64)
			})?;
			let reserved = vm.reserved_bytes();
			let committed = vm.committed_bytes();
			let measurement = finish_fixture_rss(measurement, rss_before)?;
			Ok((measurement, reserved, committed))
		}
		_ => Err(io::Error::other("unknown direct-to-frame candidate").into()),
	}
}

/// Measures scatter transport while validation remains on the pull consumer.
#[cfg(all(windows, feature = "native-async-io"))]
fn measure_scatter_prefetch(
	file: Arc<DirectFile>,
	extents: &[(PageNo, usize)],
	depth: usize,
	scan_pages: usize,
) -> Result<Measurement, Box<dyn Error>> {
	let rss_before = process_rss()?;
	let max_extent_pages = maximum_extent_pages(extents)?;
	if depth == 0 {
		let mut frames =
			(0..max_extent_pages).map(|_| Box::new(AlignedPage::zeroed())).collect::<Vec<_>>();
		let mut scatter = ScatterReader::new()?;
		let timer = MeasurementTimer::start()?;
		for (first, count) in extents.iter().copied() {
			let mut pages = frames[..count].iter_mut().map(Box::as_mut).collect::<Vec<_>>();
			scatter.read_raw(&file, first, &mut pages)?;
			drop(pages);
			for (offset, page) in frames[..count].iter().enumerate() {
				page.validate(PageNo::new(first.get() + offset as u64))?;
			}
			black_box(frames[0].as_bytes()[PAGE_HEADER_SIZE]);
		}
		let timing = timer.finish()?;
		let rss_after = process_rss()?;
		return Ok(aggregate_measurement(
			timing,
			extents.len() as u64,
			scan_pages as u64 * PAGE_SIZE as u64,
			rss_after.saturating_sub(rss_before),
		));
	} else {
		let set_count = depth
			.checked_add(1)
			.ok_or_else(|| io::Error::other("scatter prefetch buffer-set count overflow"))?;
		let pool = ReusableBufferPool::with_factory(set_count, || {
			(0..max_extent_pages).map(|_| Box::new(AlignedPage::zeroed())).collect::<Vec<_>>()
		});
		let worker_file = Arc::clone(&file);
		let worker_pool = pool.clone();
		let timer = MeasurementTimer::start()?;
		let mut cursor = PrototypePrefetchCursor::new(
			extents.to_vec(),
			NonZeroUsize::new(depth).ok_or("zero scatter prefetch depth")?,
			move |(first, count)| {
				PREFETCH_SCATTER.with(|slot| {
					let mut slot = slot.borrow_mut();
					if slot.is_none() {
						*slot = Some(ScatterReader::new()?);
					}
					let scatter = slot.as_mut().ok_or_else(|| {
						FerrosGrynnError::storage_io_failure(
							"scatter worker context was not initialized",
						)
					})?;
					let mut buffer = worker_pool.take()?;
					let mut pages = buffer.buffer_mut()?[..count]
						.iter_mut()
						.map(Box::as_mut)
						.collect::<Vec<_>>();
					scatter.read_raw(&worker_file, first, &mut pages)?;
					drop(pages);
					Ok((first, count, buffer))
				})
			},
		);
		while let Some(result) = cursor.next() {
			let (first, count, buffer) = result?;
			for (offset, page) in buffer.buffer()?[..count].iter().enumerate() {
				page.validate(PageNo::new(first.get() + offset as u64))?;
			}
			black_box(buffer.buffer()?[0].as_bytes()[PAGE_HEADER_SIZE]);
			drop(buffer);
		}
		cursor.finish()?;
		let timing = timer.finish()?;
		pool.ensure_all_returned()?;
		let rss_after = process_rss()?;
		return Ok(aggregate_measurement(
			timing,
			extents.len() as u64,
			scan_pages as u64 * PAGE_SIZE as u64,
			rss_after.saturating_sub(rss_before),
		));
	}
}

/// One already-preloaded candidate in the concurrency comparison.
#[derive(Clone, Copy)]
enum ConcurrentResidentCandidate<'fixture> {
	/// Fully resident read-only mapping.
	Mmap(&'fixture MmapControl),
	/// Production Clock-Pro pool.
	ClockPro(&'fixture BufferPool),
	/// Atomic arena with directory lookup.
	Atomic(&'fixture AtomicArenaBufferPool),
	/// Atomic arena with pre-resolved locators.
	AtomicResolved(
		&'fixture AtomicArenaBufferPool,
		&'fixture [super::atomic_arena::ResolvedFrameHandle],
	),
	/// Fixed-address VM cache with source lookup.
	#[cfg(windows)]
	Vm(&'fixture VmCache),
	/// Fixed-address VM cache with pre-resolved addresses.
	#[cfg(windows)]
	VmResolved(&'fixture VmCache, &'fixture [VmResolvedHandle]),
}

impl ConcurrentResidentCandidate<'_> {
	/// Stable candidate label shared with the single-thread resident rows.
	const fn label(self) -> &'static str {
		match self {
			Self::Mmap(_) => "R0_mmap",
			Self::ClockPro(_) => "R1_clockpro",
			Self::Atomic(_) => "R2_atomic",
			Self::AtomicResolved(_, _) => "R3_atomic_resolved",
			#[cfg(windows)]
			Self::Vm(_) => "R4_vm",
			#[cfg(windows)]
			Self::VmResolved(_, _) => "R5_vm_resolved",
		}
	}

	/// Performs one already-resident marker read.
	fn access(self, dataset: &Dataset, page_no: PageNo) -> FerrosGrynnResult<()> {
		match self {
			Self::Mmap(mmap) => {
				black_box(mmap.marker(page_no));
			}
			Self::ClockPro(clock) => {
				let guard = clock.get_or_load(dataset.key(page_no), |_| {
					Err(FerrosGrynnError::storage_io_failure("concurrent Clock-Pro miss"))
				})?;
				black_box(guard.as_bytes()[PAGE_HEADER_SIZE]);
			}
			Self::Atomic(atomic) => {
				let guard = atomic.get_or_load(dataset.key(page_no), |_| {
					Err(FerrosGrynnError::storage_io_failure("concurrent atomic miss"))
				})?;
				black_box(guard.as_bytes()[PAGE_HEADER_SIZE]);
			}
			Self::AtomicResolved(atomic, handles) => {
				let guard =
					atomic.get_resolved(&handles[page_no.get() as usize])?.ok_or_else(|| {
						FerrosGrynnError::storage_io_failure(
							"concurrent resolved atomic handle became stale",
						)
					})?;
				black_box(guard.as_bytes()[PAGE_HEADER_SIZE]);
			}
			#[cfg(windows)]
			Self::Vm(vm) => {
				let guard = vm.get_or_load(dataset.key(page_no), |_| {
					Err(FerrosGrynnError::storage_io_failure("concurrent VM miss"))
				})?;
				black_box(guard.as_bytes()[PAGE_HEADER_SIZE]);
			}
			#[cfg(windows)]
			Self::VmResolved(vm, handles) => {
				let guard =
					vm.get_resolved(&handles[page_no.get() as usize])?.ok_or_else(|| {
						FerrosGrynnError::storage_io_failure(
							"concurrent resolved VM handle became stale",
						)
					})?;
				black_box(guard.as_bytes()[PAGE_HEADER_SIZE]);
			}
		}
		Ok(())
	}

	/// Returns current VM address-space accounting when applicable.
	fn vm_bytes(self) -> (usize, usize) {
		match self {
			#[cfg(windows)]
			Self::Vm(vm) | Self::VmResolved(vm, _) => (vm.reserved_bytes(), vm.committed_bytes()),
			_ => (0, 0),
		}
	}
}

/// Runs paired, order-rotated total-throughput concurrency probes. Worker
/// construction and readiness are excluded: the main participant confirms all
/// workers at the first barrier, then starts timing immediately before
/// releasing the second barrier.
#[allow(clippy::too_many_arguments, reason = "platform fixtures remain explicit benchmark inputs")]
fn run_resident_concurrency(
	config: HarnessConfig,
	dataset: &Dataset,
	selected: &[ResidentCandidate],
	mmap: Option<&MmapControl>,
	clock: Option<&BufferPool>,
	atomic: Option<&AtomicArenaBufferPool>,
	atomic_handles: &[super::atomic_arena::ResolvedFrameHandle],
	#[cfg(windows)] vm: Option<&VmCache>,
	#[cfg(windows)] vm_handles: &[VmResolvedHandle],
	artifacts: &mut Artifacts,
) -> Result<(), Box<dyn Error>> {
	let hot_pages = config.pool_pages.min(usize::try_from(config.dataset_pages)?);
	let mut candidates = Vec::new();
	if selected.contains(&ResidentCandidate::Mmap) {
		candidates.push(ConcurrentResidentCandidate::Mmap(
			mmap.ok_or_else(|| io::Error::other("concurrent mmap fixture is absent"))?,
		));
	}
	if selected.contains(&ResidentCandidate::ClockPro) {
		candidates.push(ConcurrentResidentCandidate::ClockPro(
			clock.ok_or_else(|| io::Error::other("concurrent Clock-Pro fixture is absent"))?,
		));
	}
	if selected.contains(&ResidentCandidate::Atomic) {
		candidates.push(ConcurrentResidentCandidate::Atomic(
			atomic.ok_or_else(|| io::Error::other("concurrent atomic fixture is absent"))?,
		));
	}
	if selected.contains(&ResidentCandidate::AtomicResolved) {
		candidates.push(ConcurrentResidentCandidate::AtomicResolved(
			atomic
				.ok_or_else(|| io::Error::other("concurrent resolved atomic fixture is absent"))?,
			atomic_handles,
		));
	}
	#[cfg(windows)]
	if selected.contains(&ResidentCandidate::Vm) {
		candidates.push(ConcurrentResidentCandidate::Vm(
			vm.ok_or_else(|| io::Error::other("concurrent VM fixture is absent"))?,
		));
	}
	#[cfg(windows)]
	if selected.contains(&ResidentCandidate::VmResolved) {
		candidates.push(ConcurrentResidentCandidate::VmResolved(
			vm.ok_or_else(|| io::Error::other("concurrent resolved VM fixture is absent"))?,
			vm_handles,
		));
	}
	if candidates.is_empty() {
		return artifacts
			.skip("resident", "concurrency", "selected candidate has no concurrency probe")
			.map_err(Into::into);
	}

	let candidate_order = candidates;
	for threads in [1_usize, 2, 4, 8, 12] {
		for repetition in 0..config.repetitions {
			let mut repetition_candidates = candidate_order.clone();
			let candidate_count = repetition_candidates.len();
			repetition_candidates.rotate_left(repetition % candidate_count);
			for (order_position, candidate) in repetition_candidates.iter().copied().enumerate() {
				let operations_per_worker =
					config.resident_operations.max(threads as u64) / threads as u64;
				let readiness = Barrier::new(threads + 1);
				let timed_start = Barrier::new(threads + 1);
				let timing = thread::scope(|scope| -> Result<MeasurementTiming, Box<dyn Error>> {
					let mut workers = Vec::with_capacity(threads);
					for worker in 0..threads {
						let readiness_ref = &readiness;
						let timed_start_ref = &timed_start;
						workers.push(scope.spawn(move || -> FerrosGrynnResult<()> {
							readiness_ref.wait();
							timed_start_ref.wait();
							for operation in 0..operations_per_worker {
								let page_no = resident_page(operation + worker as u64, hot_pages);
								candidate.access(dataset, page_no)?;
							}
							Ok(())
						}));
					}
					readiness.wait();
					let timer = MeasurementTimer::start()?;
					timed_start.wait();
					for worker in workers {
						worker
							.join()
							.map_err(|_panic| io::Error::other("concurrency worker panicked"))??;
					}
					Ok(timer.finish()?)
				})?;
				let operations = operations_per_worker * threads as u64;
				let measurement =
					aggregate_measurement(timing, operations, operations * PAGE_SIZE as u64, 0);
				let (vm_reserved_bytes, vm_committed_bytes) = candidate.vm_bytes();
				artifacts.sample(
					SampleMeta {
						phase: "resident",
						workload: "concurrent_hot_set",
						candidate: candidate.label(),
						repetition,
						order_position,
						dataset_pages: config.dataset_pages,
						pool_pages: config.pool_pages,
						hot_pages,
						file_count: 1,
						threads,
						queue_depth: 0,
						extent_pages: 1,
						prefetch_depth: 0,
						vm_reserved_bytes,
						vm_committed_bytes,
						hit_ratio: 1.0,
					},
					measurement,
				)?;
			}
		}
	}
	Ok(())
}

/// One logical page access in an analytical storage workload.
#[derive(Debug, Clone, Copy)]
struct WorkloadAccess {
	/// Index into the workload's source-file slice.
	file_index: usize,
	/// Physical page within that source.
	page_no: PageNo,
	/// Whether the page belongs to the workload's declared hot set.
	hot: bool,
}

/// Writes one component row using the existing compact CSV schema.
#[allow(clippy::too_many_arguments, reason = "CSV dimensions are explicit experiment inputs")]
fn record_component_sample(
	artifacts: &mut Artifacts,
	config: HarnessConfig,
	dataset_pages: u64,
	workload: &str,
	candidate: &str,
	repetition: usize,
	order_position: usize,
	measurement: Measurement,
) -> io::Result<()> {
	artifacts.sample(
		SampleMeta {
			phase: "component",
			workload,
			candidate,
			repetition,
			order_position,
			dataset_pages,
			pool_pages: config.pool_pages,
			hot_pages: 0,
			file_count: 1,
			threads: 1,
			queue_depth: 0,
			extent_pages: 1,
			prefetch_depth: 0,
			vm_reserved_bytes: 0,
			vm_committed_bytes: 0,
			hit_ratio: 0.0,
		},
		measurement,
	)
}

/// Aggregates the five isolated fixed-address VM lifecycle phases.
///
/// # Errors
///
/// Returns the first Windows reserve, commit, decommit, or release failure.
#[cfg(windows)]
fn measure_vm_lifecycle(
	iterations: u64,
) -> Result<Vec<(&'static str, Measurement)>, Box<dyn Error>> {
	let sample_capacity = usize::try_from(iterations)?;
	let mut samples: [Vec<u64>; 5] =
		std::array::from_fn(|_phase| Vec::with_capacity(sample_capacity));
	for _iteration in 0..iterations {
		let lifecycle = probe_vm_lifecycle()?;
		for (phase_samples, duration) in samples.iter_mut().zip([
			lifecycle.reserve,
			lifecycle.commit,
			lifecycle.first_touch,
			lifecycle.decommit,
			lifecycle.recommit,
		]) {
			phase_samples.push(u64::try_from(duration.as_nanos()).unwrap_or(u64::MAX));
		}
	}

	let labels = ["vm_reserve", "vm_commit", "vm_first_touch", "vm_decommit", "vm_recommit"];
	let mut measurements = Vec::with_capacity(labels.len());
	for (phase, (candidate, mut phase_samples)) in labels.into_iter().zip(samples).enumerate() {
		phase_samples.sort_unstable();
		let elapsed_ns = phase_samples.iter().copied().fold(0_u64, u64::saturating_add);
		measurements.push((candidate, Measurement {
			elapsed: Duration::from_nanos(elapsed_ns),
			cpu_time: Duration::ZERO,
			operations: iterations,
			useful_bytes: if phase == 2 {
				iterations.saturating_mul(PAGE_SIZE as u64)
			} else {
				0
			},
			p50_ns: percentile(&phase_samples, 50),
			p95_ns: percentile(&phase_samples, 95),
			p99_ns: percentile(&phase_samples, 99),
			rss_delta: 0,
		}));
	}
	Ok(measurements)
}

/// Measures cold frame selection, directory insertion, and signal creation.
fn measure_reserve_unique(
	dataset: &Dataset,
	capacity: usize,
	batch_pages: usize,
	latency_batch: usize,
) -> Result<Measurement, Box<dyn Error>> {
	let pool = AtomicArenaBufferPool::new(NonZeroUsize::new(capacity).ok_or("zero capacity")?);
	let batch_count = capacity / batch_pages;
	let keys = (0..batch_count * batch_pages)
		.map(|index| dataset.key(PageNo::new(index as u64)))
		.collect::<Vec<_>>();
	let mut reservations = Vec::with_capacity(batch_count);
	let measurement = measure_operations(
		batch_count as u64,
		latency_batch.min(batch_count).max(1),
		0,
		|operation| {
			let start = operation as usize * batch_pages;
			reservations.push(pool.reserve_batch(&keys[start..start + batch_pages])?);
			Ok(())
		},
	)?;
	drop(reservations);
	Ok(measurement)
}

/// Measures rollback and error-signal completion for prepared reservations.
fn measure_abort_prepared(
	dataset: &Dataset,
	capacity: usize,
	batch_pages: usize,
	latency_batch: usize,
) -> Result<Measurement, Box<dyn Error>> {
	let pool = AtomicArenaBufferPool::new(NonZeroUsize::new(capacity).ok_or("zero capacity")?);
	let batch_count = capacity / batch_pages;
	let keys = (0..batch_count * batch_pages)
		.map(|index| dataset.key(PageNo::new(index as u64)))
		.collect::<Vec<_>>();
	let mut reservations = Vec::with_capacity(batch_count);
	for chunk in keys.chunks(batch_pages) {
		reservations.push(pool.reserve_batch(chunk)?);
	}
	measure_operations(batch_count as u64, latency_batch.min(batch_count).max(1), 0, |_operation| {
		let reservation = reservations
			.pop()
			.ok_or_else(|| io::Error::other("prepared abort reservation missing"))?;
		drop(reservation);
		Ok(())
	})
}

/// Measures all-or-none publication and the returned borrowed-guard drops.
fn measure_publish_prepared(
	dataset: &Dataset,
	capacity: usize,
	batch_pages: usize,
	latency_batch: usize,
) -> Result<Measurement, Box<dyn Error>> {
	let pool = AtomicArenaBufferPool::new(NonZeroUsize::new(capacity).ok_or("zero capacity")?);
	let batch_count = capacity / batch_pages;
	let keys = (0..batch_count * batch_pages)
		.map(|index| dataset.key(PageNo::new(index as u64)))
		.collect::<Vec<_>>();
	let mut prepared = Vec::with_capacity(batch_count);
	for chunk in keys.chunks(batch_pages) {
		let mut reservation = pool.reserve_batch(chunk)?;
		{
			let mut pages = reservation.pages_mut();
			for (key, target) in chunk.iter().copied().zip(pages.iter_mut()) {
				let source = benchmark_page(key.page_no())?;
				target.as_bytes_mut().copy_from_slice(source.as_bytes());
			}
		}
		prepared.push(reservation);
	}
	measure_operations(
		batch_count as u64,
		latency_batch.min(batch_count).max(1),
		(batch_pages * PAGE_SIZE) as u64,
		|_operation| {
			let reservation =
				prepared.pop().ok_or_else(|| io::Error::other("prepared publication missing"))?;
			let guards = reservation.publish()?;
			black_box(guards.len());
			drop(guards);
			Ok(())
		},
	)
}

/// Measures a real coalesced waiter from loading publication to wakeup.
fn measure_coalesced_signal_wait(
	dataset: &Dataset,
	sealed_page: &AlignedPage,
	iterations: u64,
) -> Result<Measurement, Box<dyn Error>> {
	let pool = Arc::new(AtomicArenaBufferPool::new(NonZeroUsize::MIN));
	let key = dataset.key(PageNo::new(0));
	let mut copied = AlignedPage::zeroed();
	copied.as_bytes_mut().copy_from_slice(sealed_page.as_bytes());
	let source = Arc::new(copied);
	let expected_header = dataset.expected_header(PageNo::new(0))?;
	let start = Barrier::new(3);
	let ready = AtomicU64::new(0);
	let waiter_done = AtomicU64::new(0);
	let cancelled = AtomicBool::new(false);
	let rss_before = process_rss()?;
	let timing = thread::scope(|scope| -> Result<MeasurementTiming, Box<dyn Error>> {
		let loader_pool = Arc::clone(&pool);
		let loader_source = Arc::clone(&source);
		let loader_start = &start;
		let loader_ready = &ready;
		let loader_waiter_done = &waiter_done;
		let loader_cancelled = &cancelled;
		let loader = scope.spawn(move || -> FerrosGrynnResult<()> {
			loader_start.wait();
			let result = (|| {
				for iteration in 0..iterations {
					let expected = iteration + 1;
					let guard = loader_pool.get_or_load_before_publish(
						key,
						|target| {
							target.as_bytes_mut().copy_from_slice(loader_source.as_bytes());
							Ok(expected_header)
						},
						|| {
							loader_ready.store(expected, Ordering::Release);
							while loader_pool.slow_path_counts().1 < expected {
								if loader_cancelled.load(Ordering::Acquire) {
									return Err(FerrosGrynnError::storage_io_failure(
										"coalesced waiter cancelled before registration",
									));
								}
								thread::yield_now();
							}
							Ok(())
						},
					)?;
					drop(guard);
					while loader_waiter_done.load(Ordering::Acquire) < expected {
						if loader_cancelled.load(Ordering::Acquire) {
							return Err(FerrosGrynnError::storage_io_failure(
								"coalesced waiter cancelled after publication",
							));
						}
						thread::yield_now();
					}
					if !loader_pool.remove_resident(key)? {
						return Err(FerrosGrynnError::storage_io_failure(
							"coalesced signal cycle lost its resident page",
						));
					}
				}
				Ok(())
			})();
			if result.is_err() {
				loader_cancelled.store(true, Ordering::Release);
			}
			result
		});
		let waiter_pool = Arc::clone(&pool);
		let waiter_start = &start;
		let waiter_ready = &ready;
		let waiter_finished = &waiter_done;
		let waiter_cancelled = &cancelled;
		let waiter = scope.spawn(move || -> FerrosGrynnResult<()> {
			waiter_start.wait();
			let result = (|| {
				for iteration in 0..iterations {
					let expected = iteration + 1;
					while waiter_ready.load(Ordering::Acquire) < expected {
						if waiter_cancelled.load(Ordering::Acquire) {
							return Err(FerrosGrynnError::storage_io_failure(
								"coalesced loader cancelled before publication",
							));
						}
						thread::yield_now();
					}
					let guard = waiter_pool.get_or_load(key, |_| {
						Err(FerrosGrynnError::storage_io_failure(
							"coalesced waiter unexpectedly became the loader",
						))
					})?;
					black_box(guard.as_bytes()[PAGE_HEADER_SIZE]);
					drop(guard);
					waiter_finished.store(expected, Ordering::Release);
				}
				Ok(())
			})();
			if result.is_err() {
				waiter_cancelled.store(true, Ordering::Release);
			}
			result
		});
		let timer = MeasurementTimer::start()?;
		start.wait();
		loader.join().map_err(|_panic| io::Error::other("signal loader panicked"))??;
		waiter.join().map_err(|_panic| io::Error::other("signal waiter panicked"))??;
		Ok(timer.finish()?)
	})?;
	let rss_after = process_rss()?;
	Ok(aggregate_measurement(timing, iterations, 0, rss_after.saturating_sub(rss_before)))
}

/// Measures a complete memory-backed miss followed by explicit test eviction.
fn measure_full_miss_memory(
	dataset: &Dataset,
	sealed_page: &AlignedPage,
	iterations: u64,
) -> Result<Measurement, Box<dyn Error>> {
	let pool = AtomicArenaBufferPool::new(NonZeroUsize::MIN);
	let key = dataset.key(PageNo::new(0));
	let expected_header = dataset.expected_header(PageNo::new(0))?;
	measure_operations(iterations, usize::try_from(iterations.min(64))?, PAGE_SIZE as u64, |_op| {
		let guard = pool.get_or_load(key, |target| {
			target.as_bytes_mut().copy_from_slice(sealed_page.as_bytes());
			Ok(expected_header)
		})?;
		black_box(guard.as_bytes()[PAGE_HEADER_SIZE]);
		drop(guard);
		if !pool.remove_resident(key)? {
			return Err(io::Error::other("memory miss page was not resident").into());
		}
		Ok(())
	})
}

/// Measures a complete validated Direct-I/O miss followed by test eviction.
fn measure_full_miss_direct(
	dataset: &Dataset,
	direct: &DirectFile,
	iterations: u64,
) -> Result<Measurement, Box<dyn Error>> {
	let pool = AtomicArenaBufferPool::new(NonZeroUsize::MIN);
	measure_operations(iterations, usize::try_from(iterations.min(16))?, PAGE_SIZE as u64, |op| {
		let page_no = PageNo::new(op % dataset.page_count);
		let key = dataset.key(page_no);
		let guard =
			pool.get_or_load(key, |target| read_raw_reported(dataset, direct, page_no, target))?;
		black_box(guard.as_bytes()[PAGE_HEADER_SIZE]);
		drop(guard);
		if !pool.remove_resident(key)? {
			return Err(io::Error::other("Direct miss page was not resident").into());
		}
		Ok(())
	})
}

/// Measures an Atomic arena against a deterministic logical access stream.
fn measure_atomic_accesses(
	datasets: &[Dataset],
	directs: &[DirectFile],
	pool_pages: usize,
	preload: &[WorkloadAccess],
	accesses: &[WorkloadAccess],
	latency_batch: usize,
) -> Result<(Measurement, f64), Box<dyn Error>> {
	if datasets.len() != directs.len() || accesses.is_empty() {
		return Err(io::Error::other("analytical source and access sets are inconsistent").into());
	}
	let rss_before = process_rss()?;
	let pool = AtomicArenaBufferPool::new(NonZeroUsize::new(pool_pages).ok_or("zero pool")?);
	for access in preload {
		let dataset = &datasets[access.file_index];
		let direct = &directs[access.file_index];
		drop(pool.get_or_load(dataset.key(access.page_no), |target| {
			read_raw_reported(dataset, direct, access.page_no, target)
		})?);
	}
	let loads = Cell::new(0_u64);
	let hot_misses = Cell::new(0_u64);
	let hot_accesses = accesses.iter().filter(|access| access.hot).count() as u64;
	let measurement = measure_operations(
		accesses.len() as u64,
		latency_batch.min(accesses.len()).max(1),
		PAGE_SIZE as u64,
		|operation| {
			let access = accesses[operation as usize];
			let dataset = &datasets[access.file_index];
			let direct = &directs[access.file_index];
			let guard = pool.get_or_load(dataset.key(access.page_no), |target| {
				loads.set(loads.get() + 1);
				if access.hot {
					hot_misses.set(hot_misses.get() + 1);
				}
				read_raw_reported(dataset, direct, access.page_no, target)
			})?;
			black_box(guard.as_bytes()[PAGE_HEADER_SIZE]);
			Ok(())
		},
	)?;
	let measurement = finish_fixture_rss(measurement, rss_before)?;
	let hit_ratio = if hot_accesses == 0 {
		1.0 - loads.get() as f64 / accesses.len() as f64
	} else {
		1.0 - hot_misses.get() as f64 / hot_accesses as f64
	};
	Ok((measurement, hit_ratio))
}

/// Builds the contiguous per-source hot set required by the retained page
/// table candidate.
fn contiguous_hot_preload(hot_pages: usize) -> Vec<WorkloadAccess> {
	(0..hot_pages)
		.map(|page| WorkloadAccess {
			file_index: 0,
			page_no: PageNo::new(page as u64),
			hot: true,
		})
		.collect()
}

/// Builds a graph-like stream with seven hot frontier reads followed by one
/// unique cold transition.
fn mixed_hot_cold_accesses(
	operations: usize,
	hot_pages: usize,
	cold_first: usize,
	cold_pages: usize,
) -> Vec<WorkloadAccess> {
	(0..operations)
		.map(|operation| {
			if operation % 8 == 7 {
				WorkloadAccess {
					file_index: 0,
					page_no: permuted_cold_page(cold_first, operation / 8, cold_pages),
					hot: false,
				}
			} else {
				WorkloadAccess {
					file_index: 0,
					page_no: graph_hot_page(operation as u64, hot_pages),
					hot: true,
				}
			}
		})
		.collect()
}

/// Builds short property runs interrupted by unique overflow-page reads.
fn mixed_property_accesses(
	operations: usize,
	hot_pages: usize,
	cold_first: usize,
	cold_pages: usize,
) -> Vec<WorkloadAccess> {
	(0..operations)
		.map(|operation| {
			if operation % 8 == 7 {
				WorkloadAccess {
					file_index: 0,
					page_no: permuted_cold_page(cold_first, operation / 8, cold_pages),
					hot: false,
				}
			} else {
				let run = operation / 8;
				let offset = operation % 4;
				WorkloadAccess {
					file_index: 0,
					page_no: PageNo::new((run.wrapping_mul(17) + offset) as u64 % hot_pages as u64),
					hot: true,
				}
			}
		})
		.collect()
}

/// Permutes one bounded cold range without repeats or sequential read-ahead.
fn permuted_cold_page(first: usize, ordinal: usize, page_count: usize) -> PageNo {
	const STRIDE_PRIME: u64 = 15_485_863;
	debug_assert!(page_count > 0 && (page_count as u64) < STRIDE_PRIME);
	let count = page_count as u64;
	PageNo::new(first as u64 + ordinal as u64 * (STRIDE_PRIME % count) % count)
}

/// Records R2, retained R8, and mmap against one identical access stream.
#[allow(clippy::too_many_arguments, reason = "mixed benchmark dimensions remain explicit")]
fn record_mixed_access_pair(
	artifacts: &mut Artifacts,
	config: HarnessConfig,
	datasets: &[Dataset],
	directs: &[DirectFile],
	workload: &str,
	repetition: usize,
	preload: &[WorkloadAccess],
	accesses: &[WorkloadAccess],
	ioring_file: Option<&Arc<DirectFile>>,
) -> Result<(), Box<dyn Error>> {
	let has_cold_reads = accesses.iter().any(|access| !access.hot);
	let r8_label = if has_cold_reads {
		"R8_epoch_hot+A1_direct_bypass"
	} else {
		"R8_epoch_page_table"
	};
	let mmap_label = if has_cold_reads {
		"R0_mmap_hot_cold"
	} else {
		"R0_mmap_resident"
	};
	let mut candidates = vec!["R2_atomic_cache_all", r8_label];
	#[cfg(all(windows, feature = "native-async-io"))]
	if has_cold_reads && ioring_file.is_some() {
		candidates.push("H11_production_r8_scope+T2_ioring_qd32");
	}
	candidates.push(mmap_label);
	for (order_position, candidate) in
		rotated_order(&candidates, repetition).into_iter().enumerate()
	{
		let (measurement, hit_ratio) = match candidate {
			"R2_atomic_cache_all" => measure_atomic_accesses(
				datasets,
				directs,
				config.pool_pages,
				preload,
				accesses,
				config.latency_batch,
			)?,
			label if label == r8_label => measure_epoch_hot_bypass_accesses(
				datasets,
				directs,
				config.pool_pages,
				preload,
				accesses,
				config.latency_batch,
			)?,
			#[cfg(all(windows, feature = "native-async-io"))]
			"H11_production_r8_scope+T2_ioring_qd32" => measure_epoch_hot_ioring_accesses(
				datasets,
				directs,
				ioring_file.ok_or("IoRing mixed candidate has no overlapped file")?,
				config.pool_pages,
				preload,
				accesses,
				config.latency_batch,
			)?,
			label if label == mmap_label => {
				measure_mmap_hot_cold_accesses(datasets, preload, accesses, config.latency_batch)?
			}
			_ => return Err(io::Error::other("unknown mixed access candidate").into()),
		};
		artifacts.sample(
			SampleMeta {
				phase: "mixed_finalist",
				workload,
				candidate,
				repetition,
				order_position,
				dataset_pages: datasets.iter().map(|dataset| dataset.page_count).sum(),
				pool_pages: config.pool_pages,
				hot_pages: preload.len(),
				file_count: datasets.len(),
				threads: 1,
				queue_depth: if candidate == "H11_production_r8_scope+T2_ioring_qd32" {
					32
				} else {
					1
				},
				extent_pages: 1,
				prefetch_depth: 0,
				vm_reserved_bytes: 0,
				vm_committed_bytes: 0,
				hit_ratio,
			},
			measurement,
		)?;
	}
	Ok(())
}

/// Measures R8 with one cursor-owned hot page table and non-admitting Direct
/// reads for sparse cold transitions.
fn measure_epoch_hot_bypass_accesses(
	datasets: &[Dataset],
	directs: &[DirectFile],
	pool_pages: usize,
	preload: &[WorkloadAccess],
	accesses: &[WorkloadAccess],
	latency_batch: usize,
) -> Result<(Measurement, f64), Box<dyn Error>> {
	if datasets.len() != directs.len() || datasets.is_empty() || accesses.is_empty() {
		return Err(io::Error::other("R8 mixed source and access sets are inconsistent").into());
	}
	let rss_before = process_rss()?;
	let pool = AtomicArenaBufferPool::new(NonZeroUsize::new(pool_pages).ok_or("zero pool")?);
	let mut handles = (0..datasets.len()).map(|_| Vec::new()).collect::<Vec<_>>();
	for access in preload {
		if !access.hot {
			return Err(io::Error::other("R8 preload contains a cold page").into());
		}
		let dataset = datasets
			.get(access.file_index)
			.ok_or_else(|| io::Error::other("R8 preload source is outside the dataset set"))?;
		let direct = &directs[access.file_index];
		let expected_index = handles[access.file_index].len();
		if usize::try_from(access.page_no.get())? != expected_index {
			return Err(io::Error::other("R8 hot preload must be contiguous per source").into());
		}
		drop(pool.get_or_load(dataset.key(access.page_no), |target| {
			read_raw_reported(dataset, direct, access.page_no, target)
		})?);
		handles[access.file_index].push(
			pool.resolve(dataset.key(access.page_no))
				.ok_or_else(|| io::Error::other("R8 hot preload did not resolve"))?,
		);
	}
	let epoch = pool.read_epoch()?;
	let mut source_offsets = Vec::with_capacity(handles.len() + 1);
	let mut page_count = 0_usize;
	for source in &handles {
		source_offsets.push(page_count);
		page_count = page_count
			.checked_add(source.len())
			.ok_or_else(|| io::Error::other("R8 hot page table size overflow"))?;
	}
	let page_table = handles
		.iter()
		.flatten()
		.map(|handle| {
			epoch
				.get_resolved(handle)
				.ok_or_else(|| io::Error::other("R8 hot page table became stale"))
		})
		.collect::<io::Result<Vec<_>>>()?;
	let resolved_hot_indices = accesses
		.iter()
		.map(|access| {
			if !access.hot {
				return Ok(usize::MAX);
			}
			let source_start = *source_offsets
				.get(access.file_index)
				.ok_or_else(|| io::Error::other("R8 hot source has no page-table offset"))?;
			let page_index = usize::try_from(access.page_no.get())
				.map_err(|_source| io::Error::other("R8 hot page number does not fit usize"))?;
			source_start
				.checked_add(page_index)
				.ok_or_else(|| io::Error::other("R8 hot page-table index overflow"))
		})
		.collect::<io::Result<Vec<_>>>()?;
	let mut scratch = AlignedPage::zeroed();
	let measurement = measure_operations(
		accesses.len() as u64,
		latency_batch.min(accesses.len()).max(1),
		PAGE_SIZE as u64,
		|operation| {
			let operation = operation as usize;
			let access = accesses[operation];
			if access.hot {
				let page = page_table
					.get(resolved_hot_indices[operation])
					.ok_or_else(|| io::Error::other("R8 hot access is outside the page table"))?;
				black_box(page.as_bytes()[PAGE_HEADER_SIZE]);
			} else {
				let direct = directs
					.get(access.file_index)
					.ok_or_else(|| io::Error::other("R8 cold source is outside the file set"))?;
				read_validated(direct, access.page_no, &mut scratch)?;
				black_box(scratch.as_bytes()[PAGE_HEADER_SIZE]);
			}
			Ok(())
		},
	)?;
	Ok((finish_fixture_rss(measurement, rss_before)?, 1.0))
}

/// Measures the production R8 scope while its frame-oriented IoRing fetches
/// the known sparse misses of each cursor window with QD32.
#[cfg(all(windows, feature = "native-async-io"))]
#[allow(clippy::too_many_arguments, reason = "mixed IoRing dimensions remain explicit")]
fn measure_epoch_hot_ioring_accesses(
	datasets: &[Dataset],
	directs: &[DirectFile],
	file: &Arc<DirectFile>,
	pool_pages: usize,
	preload: &[WorkloadAccess],
	accesses: &[WorkloadAccess],
	latency_batch: usize,
) -> Result<(Measurement, f64), Box<dyn Error>> {
	const REQUESTED_QD: usize = 32;
	if datasets.len() != 1
		|| directs.len() != 1
		|| accesses.is_empty()
		|| accesses.iter().any(|access| access.file_index != 0)
	{
		return Err(io::Error::other("mixed IoRing currently requires one non-empty source").into());
	}
	let rss_before = process_rss()?;
	let dataset = &datasets[0];
	let direct = &directs[0];
	let pool = HybridReadEngine::new(NonZeroUsize::new(pool_pages).ok_or("zero pool")?);
	let effective_qd = pool.sparse_window_capacity().min(REQUESTED_QD);
	if effective_qd == 0 {
		return Err(io::Error::other("mixed IoRing transient budget has no sparse frame").into());
	}
	let cursor_window = effective_qd * 8;
	let mut handles = Vec::with_capacity(preload.len());
	for access in preload {
		if !access.hot || usize::try_from(access.page_no.get())? != handles.len() {
			return Err(io::Error::other("mixed IoRing hot preload must be contiguous").into());
		}
		drop(pool.get_or_load(dataset.key(access.page_no), |target| {
			read_raw_reported(dataset, direct, access.page_no, target)
		})?);
		handles.push(
			pool.resolve(dataset.key(access.page_no))
				.ok_or_else(|| io::Error::other("mixed IoRing hot page did not resolve"))?,
		);
	}
	let mut scope = pool.read_scope()?;
	let mut cold_slots = vec![None; accesses.len()];
	let mut cold_reads_by_window = Vec::with_capacity(accesses.len().div_ceil(cursor_window));
	for (window_index, window) in accesses.chunks(cursor_window).enumerate() {
		let mut reads = Vec::with_capacity(effective_qd);
		for (offset, access) in window.iter().enumerate() {
			if !access.hot {
				if reads.len() == effective_qd {
					return Err(io::Error::other("mixed cursor window exceeds IoRing QD32").into());
				}
				cold_slots[window_index * cursor_window + offset] = Some(reads.len());
				reads.push((Arc::clone(file), access.page_no));
			}
		}
		cold_reads_by_window.push(reads);
	}
	let mut cold_batch = None;
	let measurement = measure_operations(
		accesses.len() as u64,
		latency_batch.min(accesses.len()).max(1),
		PAGE_SIZE as u64,
		|operation| {
			let operation = operation as usize;
			if operation % cursor_window == 0 {
				let reads = &cold_reads_by_window[operation / cursor_window];
				cold_batch = if reads.is_empty() {
					None
				} else {
					Some(pool.read_sparse_ioring(reads)?)
				};
			}
			let access = accesses[operation];
			if access.hot {
				let handle =
					handles.get(usize::try_from(access.page_no.get())?).ok_or_else(|| {
						io::Error::other("mixed IoRing hot page is outside the resolved handles")
					})?;
				let page = scope
					.get_resolved(handle)?
					.ok_or_else(|| io::Error::other("mixed IoRing hot handle became stale"))?;
				black_box(page.as_bytes()[PAGE_HEADER_SIZE]);
			} else {
				let slot = cold_slots[operation]
					.ok_or_else(|| io::Error::other("mixed IoRing cold page has no frame slot"))?;
				let page = cold_batch
					.as_ref()
					.and_then(|batch| batch.pages().get(slot))
					.ok_or_else(|| io::Error::other("mixed IoRing cold page has no batch frame"))?;
				black_box(page.as_bytes()[PAGE_HEADER_SIZE]);
			}
			Ok(())
		},
	)?;
	Ok((finish_fixture_rss(measurement, rss_before)?, 1.0))
}

/// Measures mmap with only the declared hot pages prefaulted and validated;
/// every cold access keeps checksum work inside the measured path.
fn measure_mmap_hot_cold_accesses(
	datasets: &[Dataset],
	preload: &[WorkloadAccess],
	accesses: &[WorkloadAccess],
	latency_batch: usize,
) -> Result<(Measurement, f64), Box<dyn Error>> {
	if datasets.is_empty() || accesses.is_empty() {
		return Err(io::Error::other("mmap mixed source and access sets are empty").into());
	}
	let rss_before = process_rss()?;
	let mappings =
		datasets.iter().map(MmapControl::map_unvalidated).collect::<Result<Vec<_>, _>>()?;
	for access in preload {
		let mapping = mappings
			.get(access.file_index)
			.ok_or_else(|| io::Error::other("mmap preload source is outside the mapping set"))?;
		validate_page_bytes(mapping_page(&mapping.mapping, access.page_no)?, access.page_no)?;
	}
	let measurement = measure_operations(
		accesses.len() as u64,
		latency_batch.min(accesses.len()).max(1),
		PAGE_SIZE as u64,
		|operation| {
			let access = accesses[operation as usize];
			let mapping = mappings
				.get(access.file_index)
				.ok_or_else(|| io::Error::other("mmap access source is outside the mapping set"))?;
			if access.hot {
				black_box(mapping.marker(access.page_no));
			} else {
				validate_page_bytes(
					mapping_page(&mapping.mapping, access.page_no)?,
					access.page_no,
				)?;
			}
			Ok(())
		},
	)?;
	Ok((finish_fixture_rss(measurement, rss_before)?, 1.0))
}

/// Measures conventional pinned R2 roots plus non-admitting one-page Direct
/// reads for a disjoint leaf range.
fn measure_btree_sync_bypass(
	dataset: &Dataset,
	direct: &DirectFile,
	root_pages: usize,
	leaf_first: PageNo,
	leaf_pages: usize,
	latency_batch: usize,
) -> Result<Measurement, Box<dyn Error>> {
	let rss_before = process_rss()?;
	let roots = AtomicArenaBufferPool::new(NonZeroUsize::new(root_pages).ok_or("zero roots")?);
	for root in 0..root_pages {
		let page_no = PageNo::new(root as u64);
		drop(roots.get_or_load(dataset.key(page_no), |target| {
			read_raw_reported(dataset, direct, page_no, target)
		})?);
	}
	let root_depth = root_pages.min(3);
	let mut scratch = AlignedPage::zeroed();
	let measurement = measure_operations(
		leaf_pages as u64,
		latency_batch.min(leaf_pages).max(1),
		((root_depth + 1) * PAGE_SIZE) as u64,
		|operation| {
			for root in 0..root_depth {
				let page_no = PageNo::new(root as u64);
				let guard = roots.get_or_load(dataset.key(page_no), |_| {
					Err(FerrosGrynnError::storage_io_failure("B+tree root stopped being resident"))
				})?;
				black_box(guard.as_bytes()[PAGE_HEADER_SIZE]);
			}
			let leaf_no = PageNo::new(leaf_first.get() + operation);
			read_validated(direct, leaf_no, &mut scratch)?;
			black_box(scratch.as_bytes()[PAGE_HEADER_SIZE]);
			Ok(())
		},
	)?;
	finish_fixture_rss(measurement, rss_before)
}

/// Measures production R8 roots while the production scatter cursor reads and
/// validates a disjoint leaf range in bounded extent order.
#[cfg(all(windows, feature = "native-async-io"))]
#[allow(clippy::too_many_arguments, reason = "mixed B+tree dimensions remain explicit")]
fn measure_btree_hybrid_scatter(
	dataset: &Dataset,
	direct: &DirectFile,
	file: Arc<DirectFile>,
	pool_pages: usize,
	root_pages: usize,
	leaf_first: PageNo,
	leaf_pages: usize,
) -> Result<(Measurement, usize), Box<dyn Error>> {
	let rss_before = process_rss()?;
	let engine = HybridReadEngine::new(NonZeroUsize::new(pool_pages).ok_or("zero pool")?);
	let mut handles = Vec::with_capacity(root_pages);
	for root in 0..root_pages {
		let page_no = PageNo::new(root as u64);
		drop(engine.get_or_load(dataset.key(page_no), |target| {
			read_raw_reported(dataset, direct, page_no, target)
		})?);
		handles.push(
			engine
				.resolve(dataset.key(page_no))
				.ok_or_else(|| io::Error::other("production B+tree root did not resolve"))?,
		);
	}
	let root_depth = root_pages.min(3);
	let leaves_per_scope = MAX_READ_SCOPE_OPERATIONS.checked_div(root_depth).unwrap_or(0);
	if leaves_per_scope == 0 {
		return Err(io::Error::other("B+tree scope cannot cover one root path").into());
	}
	let extent_pages = engine.scatter_extent_capacity().min(leaf_pages);
	let timer = MeasurementTimer::start()?;
	let mut cursor = engine.scatter_cursor(file, leaf_first, leaf_pages)?;
	let mut completed_pages = 0_usize;
	let mut samples = Vec::with_capacity(leaf_pages.div_ceil(extent_pages));
	loop {
		let extent_started = Instant::now();
		let Some(extent) = cursor.next_extent() else {
			break;
		};
		let extent = extent?;
		let extent_page_count = extent.pages().len();
		for leaves in extent.pages().chunks(leaves_per_scope) {
			let mut scope = engine.read_scope()?;
			let mut table = scope.prepare_resolved_table(&handles[..root_depth])?;
			let roots = scope
				.get_resolved_window(&mut table, root_depth)?
				.ok_or_else(|| io::Error::other("production B+tree root table became stale"))?;
			for page in leaves {
				for root in 0..root_depth {
					let root = roots.page(root).ok_or_else(|| {
						io::Error::other("production B+tree root window lost a frame")
					})?;
					black_box(root.as_bytes()[PAGE_HEADER_SIZE]);
				}
				black_box(page.as_bytes()[PAGE_HEADER_SIZE]);
			}
			completed_pages = completed_pages
				.checked_add(leaves.len())
				.ok_or_else(|| io::Error::other("B+tree leaf count overflow"))?;
		}
		samples.push(
			u64::try_from(extent_started.elapsed().as_nanos() / extent_page_count as u128)
				.unwrap_or(u64::MAX),
		);
	}
	cursor.finish()?;
	let timing = timer.finish()?;
	if completed_pages != leaf_pages {
		return Err(io::Error::other("production B+tree scan returned a partial leaf range").into());
	}
	let rss_delta = process_rss()?.saturating_sub(rss_before);
	let mut measurement = aggregate_measurement(
		timing,
		leaf_pages as u64,
		leaf_pages as u64 * (root_depth + 1) as u64 * PAGE_SIZE as u64,
		rss_delta,
	);
	samples.sort_unstable();
	measurement.p50_ns = percentile(&samples, 50);
	measurement.p95_ns = percentile(&samples, 95);
	measurement.p99_ns = percentile(&samples, 99);
	Ok((measurement, extent_pages))
}

/// Measures mmap with hot roots prefaulted and every leaf checksum retained
/// inside the timed disjoint scan.
fn measure_btree_mmap(
	dataset: &Dataset,
	root_pages: usize,
	leaf_first: PageNo,
	leaf_pages: usize,
	latency_batch: usize,
) -> Result<Measurement, Box<dyn Error>> {
	let rss_before = process_rss()?;
	let mapping = MmapControl::map_unvalidated(dataset)?;
	for root in 0..root_pages {
		let page_no = PageNo::new(root as u64);
		validate_page_bytes(mapping_page(&mapping.mapping, page_no)?, page_no)?;
	}
	let root_depth = root_pages.min(3);
	let measurement = measure_operations(
		leaf_pages as u64,
		latency_batch.min(leaf_pages).max(1),
		((root_depth + 1) * PAGE_SIZE) as u64,
		|operation| {
			for root in 0..root_depth {
				black_box(mapping.marker(PageNo::new(root as u64)));
			}
			let leaf_no = PageNo::new(leaf_first.get() + operation);
			validate_page_bytes(mapping_page(&mapping.mapping, leaf_no)?, leaf_no)?;
			Ok(())
		},
	)?;
	finish_fixture_rss(measurement, rss_before)
}

/// Builds four contiguous cold extents followed by one rotating hot extent.
#[cfg(all(windows, feature = "native-async-io"))]
fn fixed_hot_scan_runs(
	page_count: usize,
	hot_pages: usize,
	extent_pages: usize,
) -> Result<Vec<(PageNo, usize, bool)>, Box<dyn Error>> {
	if extent_pages == 0
		|| hot_pages == 0
		|| !hot_pages.is_multiple_of(extent_pages)
		|| page_count <= hot_pages
	{
		return Err(io::Error::other("invalid fixed-hot scan dimensions").into());
	}
	let cold_run_pages = extent_pages
		.checked_mul(4)
		.ok_or_else(|| io::Error::other("fixed-hot cold-run size overflow"))?;
	let mut runs = Vec::new();
	let mut cold_first = hot_pages;
	let mut hot_first = 0_usize;
	while cold_first < page_count {
		let cold_pages = (page_count - cold_first).min(cold_run_pages);
		runs.push((PageNo::new(cold_first as u64), cold_pages, false));
		cold_first += cold_pages;
		if cold_pages == cold_run_pages {
			runs.push((PageNo::new(hot_first as u64), extent_pages, true));
			hot_first = (hot_first + extent_pages) % hot_pages;
		}
	}
	Ok(runs)
}

/// Times variable-sized physical runs while reporting latency per logical page.
#[cfg(all(windows, feature = "native-async-io"))]
fn measure_fixed_hot_runs<Consume>(
	runs: &[(PageNo, usize, bool)],
	rss_before: u64,
	mut consume: Consume,
) -> Result<Measurement, Box<dyn Error>>
where
	Consume: FnMut(PageNo, usize, bool) -> Result<(), Box<dyn Error>>,
{
	let operations = runs.iter().map(|(_, pages, _)| *pages as u64).sum::<u64>();
	if operations == 0 {
		return Err(io::Error::other("fixed-hot scan contains no logical pages").into());
	}
	let mut samples = Vec::with_capacity(runs.len());
	let timer = MeasurementTimer::start()?;
	for &(first, pages, hot) in runs {
		let started = Instant::now();
		consume(first, pages, hot)?;
		samples
			.push(u64::try_from(started.elapsed().as_nanos() / pages as u128).unwrap_or(u64::MAX));
	}
	let timing = timer.finish()?;
	let mut measurement =
		aggregate_measurement(timing, operations, operations.saturating_mul(PAGE_SIZE as u64), 0);
	samples.sort_unstable();
	measurement.p50_ns = percentile(&samples, 50);
	measurement.p95_ns = percentile(&samples, 95);
	measurement.p99_ns = percentile(&samples, 99);
	finish_fixture_rss(measurement, rss_before)
}

/// Measures calibrated raw Direct reads for the complete fixed-hot order.
#[cfg(all(windows, feature = "native-async-io"))]
fn measure_fixed_hot_raw(
	dataset: &Dataset,
	runs: &[(PageNo, usize, bool)],
	extent_pages: usize,
) -> Result<Measurement, Box<dyn Error>> {
	let rss_before = process_rss()?;
	let direct = DirectFile::open_read_only(&dataset.path)?;
	let mut scratch = aligned_pages(extent_pages);
	measure_fixed_hot_runs(runs, rss_before, |first, pages, _hot| {
		let mut completed = 0_usize;
		while completed < pages {
			let count = (pages - completed).min(extent_pages);
			let page_no = PageNo::new(first.get() + completed as u64);
			let output = &mut scratch[..count];
			direct.read_pages_raw(page_no, output)?;
			for page in output {
				black_box(page.as_bytes()[PAGE_HEADER_SIZE]);
			}
			completed += count;
		}
		Ok(())
	})
}

/// Measures production R8 hot windows combined with non-admitting Scatter P1
/// for cold scan runs.
#[cfg(all(windows, feature = "native-async-io"))]
fn measure_fixed_hot_hybrid(
	dataset: &Dataset,
	runs: &[(PageNo, usize, bool)],
	hot_pages: usize,
	pool_pages: usize,
	extent_pages: usize,
) -> Result<(Measurement, u64), Box<dyn Error>> {
	let rss_before = process_rss()?;
	let preload = DirectFile::open_read_only(&dataset.path)?;
	let scatter = Arc::new(DirectFile::open_read_write_with_mode(
		&dataset.path,
		DirectFileOpenMode::Overlapped,
	)?);
	let engine = HybridReadEngine::new(NonZeroUsize::new(pool_pages).ok_or("zero pool")?);
	for raw_page in 0..hot_pages {
		let page_no = PageNo::new(raw_page as u64);
		drop(engine.get_or_load(dataset.key(page_no), |target| {
			read_raw_reported(dataset, &preload, page_no, target)
		})?);
	}
	let prepare_scope = engine.read_scope()?;
	let mut tables = Vec::with_capacity(hot_pages / extent_pages);
	for first in (0..hot_pages).step_by(extent_pages) {
		let handles = (first..first + extent_pages)
			.map(|raw_page| {
				let page_no = PageNo::new(raw_page as u64);
				engine.resolve(dataset.key(page_no)).ok_or_else(|| {
					io::Error::other("fixed-hot preload did not resolve a resident page")
				})
			})
			.collect::<io::Result<Vec<_>>>()?;
		tables.push(prepare_scope.prepare_resolved_table(&handles)?);
	}
	drop(prepare_scope);
	let slow_path_before = engine.slow_path_counts();
	let mut scope = engine.read_scope()?;
	let mut hot_hits = 0_u64;
	let measurement = measure_fixed_hot_runs(runs, rss_before, |first, pages, hot| {
		if hot {
			let table_index = usize::try_from(first.get())? / extent_pages;
			let table = tables
				.get_mut(table_index)
				.ok_or_else(|| io::Error::other("fixed-hot table index is outside the hot set"))?;
			let window = scope
				.get_resolved_window(table, pages)?
				.ok_or_else(|| io::Error::other("fixed-hot resident table became stale"))?;
			for offset in 0..window.len() {
				let page = window
					.page(offset)
					.ok_or_else(|| io::Error::other("fixed-hot resident window lost a page"))?;
				black_box(page.as_bytes()[PAGE_HEADER_SIZE]);
			}
			hot_hits = hot_hits.saturating_add(window.len() as u64);
			return Ok(());
		}

		let mut cursor = engine.scatter_cursor(Arc::clone(&scatter), first, pages)?;
		let mut completed = 0_usize;
		while let Some(extent) = cursor.next_extent() {
			let extent = extent?;
			for page in extent.pages() {
				black_box(page.as_bytes()[PAGE_HEADER_SIZE]);
			}
			completed = completed.saturating_add(extent.pages().len());
		}
		cursor.finish()?;
		if completed != pages {
			return Err(io::Error::other("fixed-hot scatter returned a partial cold run").into());
		}
		Ok(())
	})?;
	if engine.resident_count() != hot_pages || engine.slow_path_counts() != slow_path_before {
		return Err(io::Error::other("fixed-hot bypass changed the resident hot set").into());
	}
	Ok((measurement, hot_hits))
}

/// Measures mmap with only the fixed hot set prefaulted and prevalidated.
#[cfg(all(windows, feature = "native-async-io"))]
fn measure_fixed_hot_mmap(
	dataset: &Dataset,
	runs: &[(PageNo, usize, bool)],
	hot_pages: usize,
) -> Result<Measurement, Box<dyn Error>> {
	let rss_before = process_rss()?;
	let mapping = MmapControl::map_unvalidated(dataset)?;
	for raw_page in 0..hot_pages {
		let page_no = PageNo::new(raw_page as u64);
		validate_page_bytes(mapping_page(&mapping.mapping, page_no)?, page_no)?;
	}
	measure_fixed_hot_runs(runs, rss_before, |first, pages, hot| {
		for offset in 0..pages {
			let page_no = PageNo::new(first.get() + offset as u64);
			if hot {
				black_box(mapping.marker(page_no));
			} else {
				validate_page_bytes(mapping_page(&mapping.mapping, page_no)?, page_no)?;
			}
		}
		Ok(())
	})
}

/// Builds one global hot-set position for an interleaved source count.
fn multifile_access(global_page: usize, file_count: usize, hot: bool) -> WorkloadAccess {
	WorkloadAccess {
		file_index: global_page % file_count,
		page_no: PageNo::new((global_page / file_count) as u64),
		hot,
	}
}

/// Measures cache-all roots combined with a selectable leaf admission policy.
fn measure_btree_hot_roots_and_leaf_scan(
	dataset: &Dataset,
	direct: &DirectFile,
	pool_pages: usize,
	leaf_policy: ReadPolicy,
	latency_batch: usize,
) -> Result<(Measurement, f64, usize), Box<dyn Error>> {
	let rss_before = process_rss()?;
	let root_pages = pool_pages.min(8).min(usize::try_from(dataset.page_count)? - 1).max(1);
	let leaf_budget = pool_pages.saturating_sub(root_pages).max(1);
	let roots = AtomicArenaBufferPool::new(NonZeroUsize::new(root_pages).ok_or("zero roots")?);
	for page in 0..root_pages {
		let page_no = PageNo::new(page as u64);
		drop(roots.get_or_load(dataset.key(page_no), |target| {
			read_raw_reported(dataset, direct, page_no, target)
		})?);
	}
	let leaves = PrototypeReadCache::for_policy(
		NonZeroUsize::new(leaf_budget).ok_or("zero leaf budget")?,
		leaf_policy,
	);
	let leaf_pages = usize::try_from(dataset.page_count)?
		.saturating_sub(root_pages)
		.min(pool_pages.saturating_mul(4))
		.max(1);
	let cursor = PrototypeScanCursor::with_policy(
		ReadIntent::SequentialScan {
			scan_id: 1,
			expected_pages: leaf_pages as u64,
			one_shot: leaf_policy == ReadPolicy::RocksLikeBypass,
		},
		Some(leaf_policy),
	);
	let leaf_loads = Cell::new(0_u64);
	let root_depth = root_pages.min(3);
	let measurement = measure_operations(
		leaf_pages as u64,
		latency_batch.min(leaf_pages).max(1),
		((root_depth + 1) * PAGE_SIZE) as u64,
		|operation| {
			for root in 0..root_depth {
				let page_no = PageNo::new(root as u64);
				let guard = roots.get_or_load(dataset.key(page_no), |_| {
					Err(FerrosGrynnError::storage_io_failure("B+tree root stopped being resident"))
				})?;
				black_box(guard.as_bytes()[PAGE_HEADER_SIZE]);
			}
			let leaf_no = PageNo::new(root_pages as u64 + operation);
			leaves.read_with(
				&cursor,
				dataset.key(leaf_no),
				|target| {
					leaf_loads.set(leaf_loads.get() + 1);
					read_raw_reported(dataset, direct, leaf_no, target)
				},
				|page| {
					black_box(page.as_bytes()[PAGE_HEADER_SIZE]);
					Ok(())
				},
			)?;
			Ok(())
		},
	)?;
	let measurement = finish_fixture_rss(measurement, rss_before)?;
	let page_accesses = leaf_pages.saturating_mul(root_depth + 1);
	let hit_ratio = 1.0 - leaf_loads.get() as f64 / page_accesses as f64;
	Ok((measurement, hit_ratio, root_pages))
}

/// Builds an aggregate-only measurement for concurrent or pipelined work.
const fn aggregate_measurement(
	timing: MeasurementTiming,
	operations: u64,
	useful_bytes: u64,
	rss_delta: u64,
) -> Measurement {
	Measurement {
		elapsed: timing.elapsed,
		cpu_time: timing.cpu_time,
		operations,
		useful_bytes,
		p50_ns: 0,
		p95_ns: 0,
		p99_ns: 0,
		rss_delta,
	}
}

/// Replaces the timing-block RSS delta with the candidate fixture footprint.
fn finish_fixture_rss(
	mut measurement: Measurement,
	rss_before_construction: u64,
) -> Result<Measurement, Box<dyn Error>> {
	let rss_after_measurement = process_rss()?;
	measurement.rss_delta = rss_after_measurement.saturating_sub(rss_before_construction);
	Ok(measurement)
}

/// Measures variable-sized operations while timing only normalized batches.
#[cfg(all(windows, feature = "native-async-io"))]
fn measure_variable_operations<Operation>(
	operations: u64,
	batch_size: usize,
	mut operation: Operation,
) -> Result<Measurement, Box<dyn Error>>
where
	Operation: FnMut(u64) -> Result<u64, Box<dyn Error>>,
{
	let rss_before = process_rss()?;
	let mut samples = Vec::with_capacity((operations as usize).div_ceil(batch_size));
	let mut useful_bytes = 0_u64;
	let timer = MeasurementTimer::start()?;
	let mut completed = 0_u64;
	while completed < operations {
		let count = (operations - completed).min(batch_size as u64);
		let batch_started = Instant::now();
		for offset in 0..count {
			useful_bytes = useful_bytes.saturating_add(operation(completed + offset)?);
		}
		let elapsed = batch_started.elapsed().as_nanos();
		samples.push(u64::try_from(elapsed / u128::from(count)).unwrap_or(u64::MAX));
		completed += count;
	}
	let timing = timer.finish()?;
	let rss_after = process_rss()?;
	samples.sort_unstable();
	Ok(Measurement {
		elapsed: timing.elapsed,
		cpu_time: timing.cpu_time,
		operations,
		useful_bytes,
		p50_ns: percentile(&samples, 50),
		p95_ns: percentile(&samples, 95),
		p99_ns: percentile(&samples, 99),
		rss_delta: rss_after.saturating_sub(rss_before),
	})
}

/// Measures one aggregate block while sampling only normalized batches.
fn measure_operations<Operation>(
	operations: u64,
	batch_size: usize,
	useful_bytes_per_operation: u64,
	mut operation: Operation,
) -> Result<Measurement, Box<dyn Error>>
where
	Operation: FnMut(u64) -> Result<(), Box<dyn Error>>,
{
	let rss_before = process_rss()?;
	let mut samples = Vec::with_capacity((operations as usize).div_ceil(batch_size));
	let timer = MeasurementTimer::start()?;
	let mut completed = 0_u64;
	while completed < operations {
		let count = (operations - completed).min(batch_size as u64);
		let batch_started = Instant::now();
		for offset in 0..count {
			operation(completed + offset)?;
		}
		let elapsed = batch_started.elapsed().as_nanos();
		samples.push(u64::try_from(elapsed / u128::from(count)).unwrap_or(u64::MAX));
		completed += count;
	}
	let timing = timer.finish()?;
	let rss_after = process_rss()?;
	samples.sort_unstable();
	Ok(Measurement {
		elapsed: timing.elapsed,
		cpu_time: timing.cpu_time,
		operations,
		useful_bytes: operations.saturating_mul(useful_bytes_per_operation),
		p50_ns: percentile(&samples, 50),
		p95_ns: percentile(&samples, 95),
		p99_ns: percentile(&samples, 99),
		rss_delta: rss_after.saturating_sub(rss_before),
	})
}

/// Measures logical operations grouped into physical page runs. Timing samples
/// are normalized by logical reads, not by the number of acquired pins.
fn measure_resident_runs<Run>(
	logical_operations: u64,
	latency_batch: usize,
	run_length: usize,
	mut run: Run,
) -> Result<Measurement, Box<dyn Error>>
where
	Run: FnMut(u64, usize) -> Result<(), Box<dyn Error>>,
{
	let rss_before = process_rss()?;
	let total_runs = logical_operations.div_ceil(run_length as u64);
	let sample_runs = latency_batch.div_ceil(run_length).max(1) as u64;
	let mut samples = Vec::with_capacity(total_runs.div_ceil(sample_runs) as usize);
	let timer = MeasurementTimer::start()?;
	let mut completed_runs = 0_u64;
	let mut completed_operations = 0_u64;
	while completed_runs < total_runs {
		let runs_in_sample = (total_runs - completed_runs).min(sample_runs);
		let sample_started = Instant::now();
		let sample_operations_before = completed_operations;
		for offset in 0..runs_in_sample {
			let remaining = logical_operations - completed_operations;
			let logical_count = remaining.min(run_length as u64) as usize;
			run(completed_runs + offset, logical_count)?;
			completed_operations += logical_count as u64;
		}
		let sample_operations = completed_operations - sample_operations_before;
		let elapsed = sample_started.elapsed().as_nanos();
		samples.push(u64::try_from(elapsed / u128::from(sample_operations)).unwrap_or(u64::MAX));
		completed_runs += runs_in_sample;
	}
	let timing = timer.finish()?;
	let rss_after = process_rss()?;
	samples.sort_unstable();
	Ok(Measurement {
		elapsed: timing.elapsed,
		cpu_time: timing.cpu_time,
		operations: logical_operations,
		useful_bytes: logical_operations.saturating_mul(PAGE_SIZE as u64),
		p50_ns: percentile(&samples, 50),
		p95_ns: percentile(&samples, 95),
		p99_ns: percentile(&samples, 99),
		rss_delta: rss_after.saturating_sub(rss_before),
	})
}

/// Measures production R8 after resolving each bounded cursor window once.
fn measure_production_resident_windows(
	logical_operations: u64,
	latency_batch: usize,
	run_length: usize,
	hot_pages: usize,
	scope: &mut ReadScope<'_>,
	handles: &[ResolvedFrameHandle],
	table: &mut ResolvedPageTable,
) -> Result<Measurement, Box<dyn Error>> {
	let rss_before = process_rss()?;
	let total_runs = logical_operations.div_ceil(run_length as u64);
	let sample_runs = latency_batch.div_ceil(run_length).max(1) as u64;
	let mut samples = Vec::with_capacity(total_runs.div_ceil(sample_runs) as usize);
	let mut window_handles = Vec::with_capacity(MAX_READ_SCOPE_OPERATIONS);
	let use_complete_hot_table = hot_pages <= MAX_READ_SCOPE_OPERATIONS;
	let timer = MeasurementTimer::start()?;
	let mut completed_runs = 0_u64;
	let mut completed_operations = 0_u64;
	while completed_runs < total_runs {
		let runs_in_sample = (total_runs - completed_runs).min(sample_runs);
		let sample_started = Instant::now();
		let sample_operations_before = completed_operations;
		let mut sample_offset = 0_u64;
		while sample_offset < runs_in_sample {
			let runs_in_window = usize::try_from(
				(runs_in_sample - sample_offset).min(MAX_READ_SCOPE_OPERATIONS as u64),
			)?;
			window_handles.clear();
			if !use_complete_hot_table {
				for offset in 0..runs_in_window as u64 {
					let page_no = resident_page(completed_runs + sample_offset + offset, hot_pages);
					window_handles.push(handles[page_no.get() as usize]);
				}
			}
			if !use_complete_hot_table {
				scope.update_resolved_table_owned(table, &mut window_handles)?;
			}
			let window = scope.get_resolved_window(table, runs_in_window)?.ok_or_else(|| {
				FerrosGrynnError::storage_io_failure("production window page became stale")
			})?;
			for offset in 0..runs_in_window {
				let page_index = if use_complete_hot_table {
					let run = completed_runs + sample_offset + offset as u64;
					resident_page(run, hot_pages).get() as usize
				} else {
					offset
				};
				let page = window.page(page_index).ok_or_else(|| {
					FerrosGrynnError::storage_io_failure(
						"production window lost its prepared frame",
					)
				})?;
				let remaining = logical_operations - completed_operations;
				let logical_count = remaining.min(run_length as u64);
				for logical in 0..logical_count as usize {
					black_box(page.as_bytes()[resident_marker_offset(logical)]);
				}
				completed_operations += logical_count;
			}
			drop(window);
			sample_offset += runs_in_window as u64;
		}
		let sample_operations = completed_operations - sample_operations_before;
		let elapsed = sample_started.elapsed().as_nanos();
		samples.push(u64::try_from(elapsed / u128::from(sample_operations)).unwrap_or(u64::MAX));
		completed_runs += runs_in_sample;
	}
	let timing = timer.finish()?;
	let rss_after = process_rss()?;
	samples.sort_unstable();
	Ok(Measurement {
		elapsed: timing.elapsed,
		cpu_time: timing.cpu_time,
		operations: logical_operations,
		useful_bytes: logical_operations.saturating_mul(PAGE_SIZE as u64),
		p50_ns: percentile(&samples, 50),
		p95_ns: percentile(&samples, 95),
		p99_ns: percentile(&samples, 99),
		rss_delta: rss_after.saturating_sub(rss_before),
	})
}

/// Measures read-only mmap markers after every worker reaches the start gate.
fn measure_mmap_resident_concurrency(
	mmap: &MmapControl,
	hot_pages: usize,
	operations: u64,
	threads: usize,
	worker_affinity_masks: Option<&[usize]>,
) -> Result<Measurement, Box<dyn Error>> {
	#[cfg(not(windows))]
	let _ = worker_affinity_masks;
	let readiness = Barrier::new(threads + 1);
	let timed_start = Barrier::new(threads + 1);
	let timing = thread::scope(|scope| -> Result<MeasurementTiming, Box<dyn Error>> {
		let mut workers = Vec::with_capacity(threads);
		for worker in 0..threads {
			let readiness_ref = &readiness;
			let timed_start_ref = &timed_start;
			let (start, count) = partition_worker_operations(operations, threads, worker);
			workers.push(scope.spawn(move || -> io::Result<()> {
				#[cfg(windows)]
				let affinity = worker_affinity_masks.map(|masks| pin_current_thread(masks[worker])).transpose();
				readiness_ref.wait();
				timed_start_ref.wait();
				#[cfg(windows)]
				let _affinity = affinity?;
				for operation in start..start + count {
					black_box(mmap.marker(resident_page(operation, hot_pages)));
				}
				Ok(())
			}));
		}
		readiness.wait();
		let timer = MeasurementTimer::start()?;
		timed_start.wait();
		for worker in workers {
			worker
				.join()
				.map_err(|_panic| io::Error::other("mmap concurrency worker panicked"))??;
		}
		Ok(timer.finish()?)
	})?;
	Ok(aggregate_measurement(timing, operations, operations.saturating_mul(PAGE_SIZE as u64), 0))
}

/// Measures production R8 windows with independent worker-owned read scopes.
fn measure_production_resident_concurrency(
	engine: &HybridReadEngine,
	handles: &[ResolvedFrameHandle],
	hot_pages: usize,
	operations: u64,
	threads: usize,
	worker_affinity_masks: Option<&[usize]>,
) -> Result<Measurement, Box<dyn Error>> {
	#[cfg(not(windows))]
	let _ = worker_affinity_masks;
	let readiness = Barrier::new(threads + 1);
	let timed_start = Barrier::new(threads + 1);
	let timing = thread::scope(|scope| -> Result<MeasurementTiming, Box<dyn Error>> {
		let mut workers = Vec::with_capacity(threads);
		for worker in 0..threads {
			let readiness_ref = &readiness;
			let timed_start_ref = &timed_start;
			let (start, count) = partition_worker_operations(operations, threads, worker);
			workers.push(scope.spawn(move || -> FerrosGrynnResult<()> {
				#[cfg(windows)]
				let affinity = worker_affinity_masks.map(|masks| pin_current_thread(masks[worker])).transpose();
				let prepared = (|| {
					let scope = engine.read_scope()?;
					let table = if hot_pages <= MAX_READ_SCOPE_OPERATIONS {
						scope.prepare_resolved_table(&handles[..hot_pages])?
					} else {
						scope.prepare_resolved_table(&[])?
					};
					Ok::<_, FerrosGrynnError>((scope, table))
				})();
				readiness_ref.wait();
				timed_start_ref.wait();
				#[cfg(windows)]
				let _affinity = affinity.map_err(|error| {
					FerrosGrynnError::storage_io_failure(format!("worker affinity failed: {error}"))
				})?;
				let (mut scope, mut table) = prepared?;
				consume_production_resident_operations(
					start, count, hot_pages, &mut scope, handles, &mut table,
				)
			}));
		}
		readiness.wait();
		let timer = MeasurementTimer::start()?;
		timed_start.wait();
		for worker in workers {
			worker
				.join()
				.map_err(|_panic| io::Error::other("production concurrency worker panicked"))??;
		}
		Ok(timer.finish()?)
	})?;
	Ok(aggregate_measurement(timing, operations, operations.saturating_mul(PAGE_SIZE as u64), 0))
}

/// Consumes one worker's operation range through bounded production windows.
fn consume_production_resident_operations(
	start: u64,
	operations: u64,
	hot_pages: usize,
	scope: &mut ReadScope<'_>,
	handles: &[ResolvedFrameHandle],
	table: &mut ResolvedPageTable,
) -> FerrosGrynnResult<()> {
	let use_complete_hot_table = hot_pages <= MAX_READ_SCOPE_OPERATIONS;
	let mut window_handles = Vec::with_capacity(MAX_READ_SCOPE_OPERATIONS);
	let mut completed = 0_u64;
	while completed < operations {
		let window_len =
			usize::try_from((operations - completed).min(MAX_READ_SCOPE_OPERATIONS as u64))
				.map_err(|error| FerrosGrynnError::storage_io_failure(error.to_string()))?;
		window_handles.clear();
		if !use_complete_hot_table {
			for offset in 0..window_len as u64 {
				let page_no = resident_page(start + completed + offset, hot_pages);
				window_handles.push(handles[page_no.get() as usize]);
			}
			scope.update_resolved_table_owned(table, &mut window_handles)?;
		}
		let window = scope.get_resolved_window(table, window_len)?.ok_or_else(|| {
			FerrosGrynnError::storage_io_failure("concurrent production window became stale")
		})?;
		for offset in 0..window_len {
			let page_index = if use_complete_hot_table {
				resident_page(start + completed + offset as u64, hot_pages).get() as usize
			} else {
				offset
			};
			let page = window.page(page_index).ok_or_else(|| {
				FerrosGrynnError::storage_io_failure("concurrent production window lost its frame")
			})?;
			black_box(page.as_bytes()[PAGE_HEADER_SIZE]);
		}
		drop(window);
		completed += window_len as u64;
	}
	Ok(())
}

/// Splits a fixed operation count into contiguous, non-overlapping ranges.
fn partition_worker_operations(operations: u64, threads: usize, worker: usize) -> (u64, u64) {
	let threads = threads as u64;
	let worker = worker as u64;
	let common = operations / threads;
	let remainder = operations % threads;
	let count = common + u64::from(worker < remainder);
	let start = worker * common + worker.min(remainder);
	(start, count)
}

/// Returns one nearest-rank percentile from sorted integer samples.
fn percentile(sorted: &[u64], percentile: usize) -> u64 {
	if sorted.is_empty() {
		return 0;
	}
	let rank = sorted.len().saturating_mul(percentile).div_ceil(100).saturating_sub(1);
	sorted[rank.min(sorted.len() - 1)]
}

/// Maps a deterministic operation number to a hot page.
fn resident_page(operation: u64, hot_pages: usize) -> PageNo {
	PageNo::new(splitmix64(operation) % hot_pages as u64)
}

/// Selects a deterministic cache-line-spaced marker inside one DB page.
fn resident_marker_offset(logical_offset: usize) -> usize {
	PAGE_HEADER_SIZE + logical_offset.saturating_mul(64) % (PAGE_SIZE - PAGE_HEADER_SIZE)
}

/// Applies the complete SplitMix64 output finalizer to decorrelate sequential
/// operation identities before range reduction.
const fn splitmix64(value: u64) -> u64 {
	let mixed = value.wrapping_add(0x9e37_79b9_7f4a_7c15);
	let mixed = (mixed ^ (mixed >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
	let mixed = (mixed ^ (mixed >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
	mixed ^ (mixed >> 31)
}

/// Preserves the deliberately structured hot-frontier generator used only by
/// the analytical graph workload.
fn graph_hot_page(operation: u64, hot_pages: usize) -> PageNo {
	let mixed = operation.wrapping_mul(0x9e37_79b9_7f4a_7c15).rotate_left(17);
	PageNo::new(mixed % hot_pages as u64)
}

/// Reads and validates one Direct-I/O page.
fn read_validated(
	file: &DirectFile,
	page_no: PageNo,
	page: &mut AlignedPage,
) -> FerrosGrynnResult<PageHeader> {
	file.read_page_raw(page_no, page)?;
	page.validate(page_no)
}

/// Reads raw bytes and returns the immutable dataset's previously sealed
/// header without checksumming the destination in the loader.
///
/// Atomic, VM, and policy prototypes validate the destination exactly once at
/// their authoritative publication or bypass-consumption boundary. Clock-Pro
/// intentionally keeps `read_validated` because its production loader result
/// is itself the only checksum boundary.
fn read_raw_reported(
	dataset: &Dataset,
	file: &DirectFile,
	page_no: PageNo,
	page: &mut AlignedPage,
) -> FerrosGrynnResult<PageHeader> {
	file.read_page_raw(page_no, page)?;
	dataset.expected_header(page_no)
}

/// Validates one completed extent without publishing any of its pages.
fn validate_extent(first: PageNo, pages: &[AlignedPage]) -> FerrosGrynnResult<()> {
	for (offset, page) in pages.iter().enumerate() {
		page.validate(PageNo::new(first.get() + offset as u64))?;
	}
	Ok(())
}

/// Creates reusable zeroed aligned pages.
fn aligned_pages(count: usize) -> Vec<AlignedPage> {
	(0..count).map(|_| AlignedPage::zeroed()).collect()
}

/// Creates deterministic sealed benchmark content.
fn benchmark_page(page_no: PageNo) -> FerrosGrynnResult<AlignedPage> {
	let page_lsn = page_no.get().checked_add(1).map(PageLsn::new).ok_or_else(|| {
		FerrosGrynnError::invalid_storage_page(page_no.get(), "prototype benchmark LSN overflow")
	})?;
	let mut page =
		AlignedPage::new(PageType::StorageSuperblock, PageFlags::empty(), page_lsn, page_no);
	page.as_bytes_mut()[PAGE_HEADER_SIZE] = page_no.get().to_le_bytes()[0];
	page.seal()?;
	Ok(page)
}

/// Returns one mapped complete page after checked byte arithmetic.
fn mapping_page(mapping: &Mmap, page_no: PageNo) -> io::Result<&[u8; PAGE_SIZE]> {
	let start = usize::try_from(page_no.get())
		.map_err(|_source| io::Error::other("mmap page number does not fit usize"))?
		.checked_mul(PAGE_SIZE)
		.ok_or_else(|| io::Error::other("mmap page offset overflow"))?;
	let end =
		start.checked_add(PAGE_SIZE).ok_or_else(|| io::Error::other("mmap page end overflow"))?;
	mapping
		.get(start..end)
		.ok_or_else(|| io::Error::other("mmap page outside mapping"))?
		.try_into()
		.map_err(|_source| io::Error::other("mmap page slice length mismatch"))
}

/// Builds sequential extent inputs preserving the final short extent.
fn extent_inputs(scan_pages: usize, extent_pages: usize) -> Vec<(PageNo, usize)> {
	(0..scan_pages)
		.step_by(extent_pages)
		.map(|first| (PageNo::new(first as u64), (scan_pages - first).min(extent_pages)))
		.collect()
}

/// Returns the reusable page count required by the largest scan extent.
fn maximum_extent_pages(extents: &[(PageNo, usize)]) -> io::Result<usize> {
	let maximum = extents
		.iter()
		.map(|(_first, count)| *count)
		.max()
		.ok_or_else(|| io::Error::other("prefetch scan contains no extents"))?;
	if maximum == 0 {
		return Err(io::Error::other("prefetch scan contains a zero-sized extent"));
	}
	Ok(maximum)
}

/// Returns one repetition's rotation without mutating its paired base order.
fn rotated_order<T>(base: &[T], repetition: usize) -> Vec<T>
where
	T: Copy,
{
	let mut order = base.to_vec();
	if !order.is_empty() {
		let candidate_count = order.len();
		order.rotate_left(repetition % candidate_count);
	}
	order
}

/// Builds the immutable per-repetition prefetch base, interleaved by transport.
fn prefetch_candidates() -> Vec<(&'static str, usize)> {
	let mut candidates = Vec::with_capacity(PREFETCH_DEPTHS.len().saturating_mul(2));
	for depth in PREFETCH_DEPTHS {
		candidates.push(("T0_direct", depth));
		#[cfg(all(windows, feature = "native-async-io"))]
		candidates.push(("T3_scatter_reusable_worker_context", depth));
	}
	candidates
}

/// Identifies candidates whose transport writes directly into reserved frames.
#[cfg(all(windows, feature = "native-async-io"))]
fn is_direct_to_frame_candidate(candidate: &str) -> bool {
	matches!(
		candidate,
		"atomic_scratch_copy_publish"
			| "atomic_scatter_direct"
			| "atomic_ioring_frame_direct"
			| "vm_scatter_direct"
	)
}

/// Stable admission-policy label.
const fn policy_label(policy: ReadPolicy) -> &'static str {
	match policy {
		ReadPolicy::CacheAll => "A0_cache_all",
		ReadPolicy::RocksLikeBypass => "A1_rocks_bypass",
		ReadPolicy::InnodbLikeCold => "A2_innodb_cold",
	}
}

/// Reads cumulative process kernel plus user CPU time from Windows.
#[cfg(windows)]
fn process_cpu_time() -> io::Result<Duration> {
	let mut creation = FILETIME::default();
	let mut exit = FILETIME::default();
	let mut kernel = FILETIME::default();
	let mut user = FILETIME::default();
	// SAFETY: `GetCurrentProcess` only returns the documented pseudo-handle for
	// the calling process and transfers no ownership.
	let process = unsafe { GetCurrentProcess() };
	// SAFETY: `process` is the current-process pseudo-handle and every output
	// pointer references a distinct live writable `FILETIME`.
	unsafe {
		GetProcessTimes(process, &raw mut creation, &raw mut exit, &raw mut kernel, &raw mut user)
	}
	.map_err(|error| io::Error::other(format!("GetProcessTimes failed: {error}")))?;
	let ticks = filetime_ticks(kernel).saturating_add(filetime_ticks(user));
	Ok(Duration::from_nanos(ticks.saturating_mul(100)))
}

/// Converts one Windows 100-nanosecond `FILETIME` counter to ticks.
#[cfg(windows)]
fn filetime_ticks(value: FILETIME) -> u64 {
	(u64::from(value.dwHighDateTime) << 32) | u64::from(value.dwLowDateTime)
}

/// Reads cumulative process page faults and current working-set bytes.
#[cfg(windows)]
fn process_memory_snapshot() -> io::Result<ProcessMemorySnapshot> {
	let mut counters = PROCESS_MEMORY_COUNTERS::default();
	counters.cb = u32::try_from(std::mem::size_of::<PROCESS_MEMORY_COUNTERS>())
		.map_err(|_error| io::Error::other("process memory counter size exceeds u32"))?;
	// SAFETY: `GetCurrentProcess` returns the documented non-owning
	// pseudo-handle.
	let process = unsafe { GetCurrentProcess() };
	let counter_bytes = counters.cb;
	// SAFETY: `process` identifies the current process and `counters` is a
	// live, correctly sized writable `PROCESS_MEMORY_COUNTERS` value.
	unsafe { GetProcessMemoryInfo(process, &raw mut counters, counter_bytes) }
		.map_err(|error| io::Error::other(format!("GetProcessMemoryInfo failed: {error}")))?;
	Ok(ProcessMemorySnapshot {
		page_fault_count: counters.PageFaultCount,
		working_set_bytes: counters.WorkingSetSize as u64,
	})
}

/// Records unsupported process CPU time explicitly as zero off Windows.
#[cfg(not(windows))]
#[allow(clippy::unnecessary_wraps, reason = "keeps the cross-platform timer contract uniform")]
fn process_cpu_time() -> io::Result<Duration> {
	Ok(Duration::ZERO)
}

/// Reads current process resident bytes.
fn process_rss() -> io::Result<u64> {
	let pid = get_current_pid().map_err(io::Error::other)?;
	let mut system = System::new();
	system.refresh_processes(ProcessesToUpdate::Some(&[pid]), false);
	system
		.process(pid)
		.map(sysinfo::Process::memory)
		.ok_or_else(|| io::Error::other("current process absent from sysinfo"))
}

/// Parses one optional environment value.
fn env_parse<T>(name: &str, default: T) -> Result<T, Box<dyn Error>>
where
	T: std::str::FromStr,
	T::Err: Error + 'static,
{
	match std::env::var(name) {
		Ok(value) => Ok(value.parse()?),
		Err(std::env::VarError::NotPresent) => Ok(default),
		Err(error) => Err(error.into()),
	}
}

/// Validates one optional exact candidate label against a phase's complete
/// supported set.
fn validate_candidate_selector<'candidate>(
	phase: &str,
	requested: Option<&'candidate str>,
	supported: &[&str],
) -> io::Result<Option<&'candidate str>> {
	let Some(candidate) = requested else {
		return Ok(None);
	};
	if supported.contains(&candidate) {
		return Ok(Some(candidate));
	}
	Err(io::Error::other(format!(
		"unknown FG_STORAGE_PROTO_ONLY_CANDIDATE for {phase}: {candidate:?}; supported: {}",
		supported.join("|")
	)))
}

/// Reads and validates the exact optional candidate selector for one phase.
fn candidate_selector_from_env(
	phase: &str,
	supported: &[&str],
) -> Result<Option<String>, Box<dyn Error>> {
	match std::env::var("FG_STORAGE_PROTO_ONLY_CANDIDATE") {
		Ok(candidate) => {
			validate_candidate_selector(phase, Some(&candidate), supported)?;
			Ok(Some(candidate))
		}
		Err(std::env::VarError::NotPresent) => Ok(None),
		Err(error) => Err(error.into()),
	}
}

/// Optionally pauses a large Windows probe after setup so WPR can start before
/// the measured candidate runs.
#[cfg(windows)]
fn wait_for_profiler_gate() -> Result<(), Box<dyn Error>> {
	let Ok(ready_path) = std::env::var("FG_STORAGE_PROTO_PROFILER_READY") else {
		return Ok(());
	};
	let go_path = std::env::var("FG_STORAGE_PROTO_PROFILER_GO").map_err(|error| {
		io::Error::other(format!("profiler ready path was set without a go path: {error}"))
	})?;
	let ready_path = PathBuf::from(ready_path);
	let go_path = PathBuf::from(go_path);
	if ready_path == go_path {
		return Err(io::Error::other("profiler ready and go paths must differ").into());
	}
	if let Some(parent) = ready_path.parent() {
		std::fs::create_dir_all(parent)?;
	}
	std::fs::write(&ready_path, std::process::id().to_string())?;
	let deadline = Instant::now() + Duration::from_secs(300);
	while !go_path.exists() {
		if Instant::now() >= deadline {
			return Err(io::Error::new(
				io::ErrorKind::TimedOut,
				"profiler go signal was not received",
			)
			.into());
		}
		thread::sleep(Duration::from_millis(10));
	}
	Ok(())
}

/// Stable artifact root.
fn artifact_directory() -> PathBuf {
	Path::new(env!("CARGO_MANIFEST_DIR"))
		.join("..")
		.join("..")
		.join("target")
		.join("storage-engine-prototype")
}

/// Human-readable timestamp without an external date dependency.
fn unixish_timestamp() -> u64 {
	std::time::SystemTime::now()
		.duration_since(std::time::UNIX_EPOCH)
		.map_or(0, |duration| duration.as_secs())
}

/// Writes reproducibility information available without privileged tooling.
fn write_environment(log: &mut File) -> io::Result<()> {
	let mut system = System::new_all();
	system.refresh_all();
	let cpu = system.cpus().first().map_or("unknown", sysinfo::Cpu::brand);
	writeln!(
		log,
		"os={} arch={} cpu={cpu:?} ram_bytes={} allocator={} processes={}",
		std::env::consts::OS,
		std::env::consts::ARCH,
		system.total_memory(),
		if cfg!(target_env = "msvc") {
			"mimalloc-pprof"
		} else {
			"tikv-jemallocator"
		},
		system.processes().len(),
	)?;
	for command in [["rustc", "--version"], ["cargo", "--version"]] {
		let output = std::process::Command::new(command[0]).arg(command[1]).output();
		match output {
			Ok(output) => writeln!(log, "{}", String::from_utf8_lossy(&output.stdout).trim())?,
			Err(error) => writeln!(log, "SKIP {}: {error}", command[0])?,
		}
	}
	#[cfg(windows)]
	match std::process::Command::new("powercfg").arg("/getactivescheme").output() {
		Ok(output) => {
			writeln!(log, "power_scheme={}", String::from_utf8_lossy(&output.stdout).trim())?;
		}
		Err(error) => writeln!(log, "SKIP power mode: {error}")?,
	}
	Ok(())
}

/// Checks the maximum extended CPUID leaf, RDTSCP, and invariant-TSC bits.
#[cfg(all(windows, target_arch = "x86_64"))]
const fn supports_fenced_tsc(maximum_extended_leaf: u32, feature_edx: u32, power_edx: u32) -> bool {
	maximum_extended_leaf >= 0x8000_0007
		&& feature_edx & (1 << 27) != 0
		&& power_edx & (1 << 8) != 0
}

/// Restores the current thread's previous Windows processor-affinity mask.
#[cfg(windows)]
struct ThreadAffinityRestore {
	/// Current-thread pseudo-handle accepted by the affinity API.
	thread: windows::Win32::Foundation::HANDLE,
	/// Exact nonzero mask returned when affinity was changed.
	mask: usize,
}

#[cfg(windows)]
impl Drop for ThreadAffinityRestore {
	fn drop(&mut self) {
		use windows::Win32::System::Threading::SetThreadAffinityMask;

		// SAFETY: restores the exact nonzero mask returned by Windows for the
		// same current-thread pseudo-handle.
		unsafe {
			SetThreadAffinityMask(self.thread, self.mask);
		}
	}
}

/// Pins the calling Windows thread until the returned guard is dropped.
#[cfg(windows)]
fn pin_current_thread(mask: usize) -> io::Result<ThreadAffinityRestore> {
	use windows::Win32::System::Threading::{GetCurrentThread, SetThreadAffinityMask};

	if mask == 0 {
		return Err(io::Error::other("thread affinity mask must be nonzero"));
	}
	// SAFETY: returns the documented non-owning pseudo-handle for this thread.
	let thread = unsafe { GetCurrentThread() };
	// SAFETY: `thread` identifies the calling thread and `mask` is nonzero;
	// Windows validates whether the selected processors belong to the process.
	let previous = unsafe { SetThreadAffinityMask(thread, mask) };
	if previous == 0 {
		return Err(io::Error::last_os_error());
	}
	Ok(ThreadAffinityRestore {
		thread,
		mask: previous,
	})
}

/// Measures fenced TSC overhead and resolved-hit cycles on one pinned CPU.
#[cfg(all(windows, target_arch = "x86_64"))]
fn tsc_resolved_probe(
	pool: &AtomicArenaBufferPool,
	handles: &[super::atomic_arena::ResolvedFrameHandle],
	hot_pages: usize,
	artifacts: &mut Artifacts,
) -> Result<bool, Box<dyn Error>> {
	use std::arch::x86_64::{__cpuid, __rdtscp, _mm_lfence};

	#[inline(always)]
	fn fenced_tsc(aux: &mut u32) -> u64 {
		let aux = std::ptr::from_mut(aux);
		// SAFETY: the fence has no memory operands.
		unsafe { _mm_lfence() };
		// SAFETY: `aux` points to one writable local `u32` for the intrinsic's
		// CPU identifier.
		let cycles = unsafe { __rdtscp(aux) };
		// SAFETY: the fence has no memory operands.
		unsafe { _mm_lfence() };
		cycles
	}

	let maximum = __cpuid(0x8000_0000).eax;
	if maximum < 0x8000_0007 {
		return Ok(false);
	}
	let feature_edx = __cpuid(0x8000_0001).edx;
	let power_edx = __cpuid(0x8000_0007).edx;
	if !supports_fenced_tsc(maximum, feature_edx, power_edx) {
		return Ok(false);
	}
	let Ok(_restore) = pin_current_thread(1) else {
		return Ok(false);
	};
	let mut empty = Vec::with_capacity(10_000);
	let mut resolved = Vec::with_capacity(10_000);
	for operation in 0..10_000_u64 {
		let mut aux = 0_u32;
		let started = fenced_tsc(&mut aux);
		black_box(operation);
		let ended = fenced_tsc(&mut aux);
		empty.push(ended.saturating_sub(started));

		let handle = handles[resident_page(operation, hot_pages).get() as usize];
		let started = fenced_tsc(&mut aux);
		let guard = pool
			.get_resolved(&handle)?
			.ok_or_else(|| FerrosGrynnError::storage_io_failure("TSC handle became stale"))?;
		black_box(guard.as_bytes()[PAGE_HEADER_SIZE]);
		drop(guard);
		let ended = fenced_tsc(&mut aux);
		resolved.push(ended.saturating_sub(started));
	}
	empty.sort_unstable();
	resolved.sort_unstable();
	writeln!(
		artifacts.log,
		"tsc rdtscp=true invariant=true affinity_mask=1 empty_median_cycles={} \
		 resolved_median_cycles={} raw_values_not_overhead_subtracted=true",
		percentile(&empty, 50),
		percentile(&resolved, 50),
	)?;
	Ok(true)
}

#[cfg(test)]
mod harness_contract_tests {
	use super::*;

	/// Owned output used to prove scoped-pipeline cancellation drops every
	/// worker, queue, and consumer value before join returns.
	struct ScopedDropToken {
		/// Number of outputs whose destructor ran.
		dropped: Arc<AtomicU64>,
		/// Input ordinal retained for the consumer's deterministic failure.
		ordinal: u64,
	}

	impl Drop for ScopedDropToken {
		fn drop(&mut self) {
			self.dropped.fetch_add(1, Ordering::Relaxed);
		}
	}

	/// CPU/wall accounting retains the paired aggregate durations and derives
	/// the process-wide utilization ratio without affecting throughput fields.
	#[test]
	fn measurement_retains_cpu_time_and_derives_wall_ratio() {
		let measurement = aggregate_measurement(
			MeasurementTiming {
				elapsed: Duration::from_nanos(10),
				cpu_time: Duration::from_nanos(25),
			},
			2,
			32,
			0,
		);

		assert_eq!(measurement.elapsed, Duration::from_nanos(10));
		assert_eq!(measurement.cpu_time, Duration::from_nanos(25));
		assert_eq!(measurement.cpu_wall_ratio().to_bits(), 2.5_f64.to_bits());
	}

	/// Candidate filters accept only exact phase-local labels.
	#[test]
	fn candidate_selector_is_exact_and_phase_local() {
		let supported = ["R0_mmap", "R2_atomic"];
		assert!(
			matches!(
				validate_candidate_selector("resident", Some("R2_atomic"), &supported),
				Ok(Some("R2_atomic"))
			),
			"exact resident label must be accepted"
		);
		assert!(
			matches!(validate_candidate_selector("resident", None, &supported), Ok(None)),
			"absent selector must keep the complete candidate set"
		);
		assert!(
			validate_candidate_selector("resident", Some("T0_raw"), &supported).is_err(),
			"transport label must not be accepted by the resident phase"
		);
		assert!(
			validate_candidate_selector("resident", Some("r2_atomic"), &supported).is_err(),
			"candidate matching must remain case-sensitive and exact"
		);
	}

	/// Process-isolated resident runs require exactly one of the two finalist
	/// fixtures and never silently construct both.
	#[test]
	fn isolated_resident_candidate_requires_exact_finalist() {
		assert!(matches!(
			require_resident_isolated_candidate(Some("R0_mmap")),
			Ok(ResidentIsolatedCandidate::Mmap)
		));
		assert!(matches!(
			require_resident_isolated_candidate(Some("H11_production_r8_generic")),
			Ok(ResidentIsolatedCandidate::ProductionGeneric)
		));
		assert!(matches!(
			require_resident_isolated_candidate(Some("H11_production_r8_scope")),
			Ok(ResidentIsolatedCandidate::ProductionScope)
		));
		assert!(matches!(
			require_resident_isolated_candidate(Some("H11_production_r8_window")),
			Ok(ResidentIsolatedCandidate::ProductionWindow)
		));
		assert!(matches!(
			require_resident_isolated_candidate(Some("R3_notify_resolved")),
			Ok(ResidentIsolatedCandidate::NotifyResolved)
		));
		assert!(matches!(
			require_resident_isolated_candidate(Some("R6_dense_flat_run")),
			Ok(ResidentIsolatedCandidate::DenseFlatRun)
		));
		assert!(matches!(
			require_resident_isolated_candidate(Some("R8_epoch_resolved")),
			Ok(ResidentIsolatedCandidate::EpochResolved)
		));
		assert!(matches!(
			require_resident_isolated_candidate(Some("R8_epoch_dense_flat")),
			Ok(ResidentIsolatedCandidate::EpochDenseFlat)
		));
		assert!(matches!(
			require_resident_isolated_candidate(Some("R8_epoch_page_table")),
			Ok(ResidentIsolatedCandidate::EpochPageTable)
		));
		assert!(matches!(
			require_resident_isolated_candidate(Some("R9_miss_backoff_resolved")),
			Ok(ResidentIsolatedCandidate::MissBackoffResolved)
		));
		assert!(require_resident_isolated_candidate(None).is_err());
		assert!(require_resident_isolated_candidate(Some("R3_resolved_run")).is_err());
	}

	/// Every paired repetition derives its order from the immutable base rather
	/// than accumulating rotations from earlier repetitions.
	#[test]
	fn paired_rotation_is_independent_per_repetition() {
		assert_eq!(rotated_order(&READ_POLICIES, 0), READ_POLICIES);
		assert_eq!(rotated_order(&READ_POLICIES, 1), [
			ReadPolicy::RocksLikeBypass,
			ReadPolicy::InnodbLikeCold,
			ReadPolicy::CacheAll,
		]);
		assert_eq!(rotated_order(&READ_POLICIES, 4), rotated_order(&READ_POLICIES, 1));
		assert_eq!(
			READ_POLICIES,
			[ReadPolicy::CacheAll, ReadPolicy::RocksLikeBypass, ReadPolicy::InnodbLikeCold,],
			"rotating a repetition must not mutate the paired base"
		);
	}

	/// I/O screening remains five runs by default while an explicit full run
	/// permits the seven paired repetitions required for finalists.
	#[test]
	fn io_repetition_limit_distinguishes_screening_and_finalists() {
		let screening = HarnessConfig {
			dataset_pages: 1,
			pool_pages: 1,
			resident_operations: 1,
			repetitions: 9,
			extent_pages: 1,
			latency_batch: 1,
			full: false,
		};
		assert_eq!(io_repetitions(screening), 5);
		assert_eq!(
			io_repetitions(HarnessConfig {
				full: true,
				..screening
			}),
			7
		);
		assert_eq!(
			io_repetitions(HarnessConfig {
				repetitions: 3,
				full: true,
				..screening
			}),
			3
		);
	}

	/// Standalone/combination and Direct/scatter pairs remain interleaved
	/// before an independent repetition rotation.
	#[test]
	fn candidate_bases_are_interleaved_before_rotation() {
		assert_eq!(TRANSPORT_CANDIDATES[0], "T0_raw");
		assert_eq!(TRANSPORT_CANDIDATES[1], "atomic_scratch_copy_publish");
		assert_eq!(rotated_order(&TRANSPORT_CANDIDATES, 1)[0], TRANSPORT_CANDIDATES[1]);
		assert_eq!(TRANSPORT_CANDIDATES[0], "T0_raw", "transport base was mutated");

		let prefetch = prefetch_candidates();
		assert_eq!(prefetch[0], ("T0_direct", 0));
		#[cfg(all(windows, feature = "native-async-io"))]
		{
			assert_eq!(prefetch[1], ("T3_scatter_reusable_worker_context", 0));
			assert_eq!(prefetch[2], ("T0_direct", 1));
		}
		let rotated = rotated_order(&prefetch, 1);
		assert_eq!(rotated[0], prefetch[1]);
		assert_eq!(prefetch[0], ("T0_direct", 0), "prefetch base was mutated");

		#[cfg(all(windows, feature = "native-async-io"))]
		{
			let combinations = combination_candidates();
			assert_eq!(combinations.len(), 2 + READ_POLICIES.len() * 2 + PREFETCH_DEPTHS.len() * 3);
			assert_eq!(combinations[0].label(), "T0_raw");
			assert_eq!(combinations[1].label(), "T0_validated");
			assert_eq!(rotated_order(&combinations, 1)[0].label(), "T0_validated");
		}
	}

	/// A resolved candidate adds only its handle allocation to the shared base
	/// engine while retaining VM address-space accounting.
	#[test]
	fn resolved_footprint_extends_shared_engine_footprint() {
		let base = CandidateFootprint {
			rss_delta: 1_024,
			vm_reserved_bytes: 16_384,
			vm_committed_bytes: 8_192,
		};
		let resolved = base.with_additional_rss(512);

		assert_eq!(resolved.rss_delta, 1_536);
		assert_eq!(resolved.vm_reserved_bytes, base.vm_reserved_bytes);
		assert_eq!(resolved.vm_committed_bytes, base.vm_committed_bytes);
		assert_eq!(base.rss_delta, 1_024, "shared base footprint was mutated");
	}

	/// The prefetch pool allocates only its configured set count and returns
	/// every lease, including a set reused after intermediate release.
	#[test]
	fn reusable_prefetch_buffers_allocate_once_and_recycle() -> Result<(), Box<dyn Error>> {
		let mut allocations = 0_usize;
		let pool = ReusableBufferPool::with_factory(3, || {
			allocations += 1;
			vec![0_u8; 16]
		});
		assert_eq!(allocations, 3);
		assert_eq!(pool.available_sets(), 3);

		let first = pool.take()?;
		let second = pool.take()?;
		let third = pool.take()?;
		assert_eq!(pool.available_sets(), 0);
		assert!(pool.take().is_err(), "a fourth buffer exceeded the software bound");

		drop(second);
		let recycled = pool.take()?;
		assert_eq!(allocations, 3, "recycling allocated an additional buffer");
		drop(first);
		drop(third);
		drop(recycled);
		pool.ensure_all_returned()?;
		assert_eq!(pool.available_sets(), 3);
		Ok(())
	}

	/// A real bounded cursor transfers leases across the worker channel and
	/// finishes with the original allocation count completely recycled.
	#[test]
	fn reusable_prefetch_cursor_returns_every_transferred_set() -> Result<(), Box<dyn Error>> {
		let mut allocations = 0_usize;
		let pool = ReusableBufferPool::with_factory(3, || {
			allocations += 1;
			vec![0_u8; 1]
		});
		let worker_pool = pool.clone();
		let depth = NonZeroUsize::new(2).ok_or("zero test prefetch depth")?;
		let mut cursor = PrototypePrefetchCursor::new(0_u8..64, depth, move |value| {
			let mut buffer = worker_pool.take()?;
			buffer.buffer_mut()?[0] = value;
			Ok((value, buffer))
		});
		for expected in 0_u8..64 {
			let (actual, buffer) = cursor.next().ok_or("prefetch test ended early")??;
			assert_eq!(actual, expected);
			assert_eq!(buffer.buffer()?[0], expected);
			drop(buffer);
		}
		assert!(cursor.next().is_none());
		cursor.finish()?;
		pool.ensure_all_returned()?;
		assert_eq!(allocations, 3, "cursor recycling allocated a fourth set");
		Ok(())
	}

	/// A consumer error cancels blocked production, drops every transferred
	/// output, and joins before returning the first error.
	#[test]
	fn scoped_prefetch_consumer_error_cancels_and_drops_all_outputs() -> Result<(), Box<dyn Error>>
	{
		let created = Arc::new(AtomicU64::new(0));
		let dropped = Arc::new(AtomicU64::new(0));
		let worker_created = Arc::clone(&created);
		let worker_dropped = Arc::clone(&dropped);
		let result = run_scoped_ordered_prefetch(
			0_u64..100,
			NonZeroUsize::new(2).ok_or("zero scoped test depth")?,
			move |ordinal| {
				worker_created.fetch_add(1, Ordering::Relaxed);
				Ok(ScopedDropToken {
					dropped: Arc::clone(&worker_dropped),
					ordinal,
				})
			},
			|token| {
				if token.ordinal == 0 {
					return Err(FerrosGrynnError::storage_io_failure(
						"intentional scoped consumer failure",
					));
				}
				Ok(())
			},
		);
		assert!(result.is_err(), "consumer failure must be returned");
		assert_eq!(
			created.load(Ordering::Relaxed),
			dropped.load(Ordering::Relaxed),
			"every created output must be dropped before the scoped join returns"
		);
		assert!(
			created.load(Ordering::Relaxed) < 100,
			"bounded cancellation must stop the worker before exhausting the input"
		);
		Ok(())
	}

	/// Checksum failure in a queued direct-to-frame extent rolls back every
	/// reservation without publishing the earlier valid page.
	#[test]
	fn scoped_direct_to_frame_validation_failure_publishes_no_page() -> Result<(), Box<dyn Error>> {
		let file_id = DataFileId::new(1, DataFileKind::NodesHot)
			.ok_or("test data-file identity is out of range")?;
		let graph_id = GraphId::new(1).ok_or("test graph identity is zero")?;
		let keys = [0_u64, 1].map(|page_no| {
			PageCacheKey::new(
				PageKey::for_graph(graph_id, file_id, PageNo::new(page_no)),
				PageCacheVersion::Home {
					generation: CheckpointGeneration::new(1),
					ceiling: Lsn::new(100),
				},
			)
		});
		let pool = AtomicArenaBufferPool::new(
			NonZeroUsize::new(keys.len()).ok_or("zero direct-to-frame test pool")?,
		);
		let result = run_scoped_ordered_prefetch(
			[()],
			NonZeroUsize::MIN,
			|()| {
				let mut batch = pool.reserve_batch(&keys)?;
				let mut pages = batch.pages_mut();
				for (key, page) in keys.iter().zip(pages.iter_mut()) {
					let source = benchmark_page(key.page_no())?;
					page.as_bytes_mut().copy_from_slice(source.as_bytes());
				}
				pages[1].as_bytes_mut()[PAGE_HEADER_SIZE] ^= 1;
				drop(pages);
				Ok(batch)
			},
			|batch| {
				drop(batch.publish()?);
				Ok(())
			},
		);
		assert!(result.is_err(), "corrupt extent must fail publication");
		assert_eq!(pool.resident_count(), 0);
		for key in keys {
			assert!(!pool.is_mapped(key), "failed extent left a mapped frame");
		}
		Ok(())
	}

	/// SplitMix64 range reduction touches representative hot sets uniformly
	/// enough for calibration without asserting cache-policy outcomes.
	#[test]
	fn resident_page_distribution_is_decorrelated_and_complete() -> Result<(), Box<dyn Error>> {
		const SAMPLES_PER_BUCKET: usize = 128;
		for hot_pages in [5_462_usize, 8_192] {
			let mut buckets = vec![0_usize; hot_pages];
			for operation in 0..hot_pages.saturating_mul(SAMPLES_PER_BUCKET) {
				let bucket = resident_page(operation as u64, hot_pages).get() as usize;
				buckets[bucket] += 1;
			}
			let minimum = *buckets
				.iter()
				.min()
				.ok_or_else(|| io::Error::other("uniformity test created no buckets"))?;
			let maximum = *buckets
				.iter()
				.max()
				.ok_or_else(|| io::Error::other("uniformity test created no buckets"))?;
			assert!(minimum > 0, "SplitMix64 did not touch every one of {hot_pages} pages");
			assert!(
				maximum.saturating_sub(minimum) <= SAMPLES_PER_BUCKET,
				"SplitMix64 bucket imbalance exceeded the deterministic calibration bound: \
				 minimum={minimum}, maximum={maximum}, pages={hot_pages}"
			);
		}
		Ok(())
	}

	/// The readiness phase cannot release measured work; only the second
	/// barrier permits workers to advance into the timed phase.
	#[test]
	fn concurrency_start_uses_distinct_readiness_and_timed_release() {
		const WORKERS: usize = 2;

		let readiness = Barrier::new(WORKERS + 1);
		let timed_start = Barrier::new(WORKERS + 1);
		let progressed = AtomicU64::new(0);
		thread::scope(|scope| {
			let mut workers = Vec::with_capacity(WORKERS);
			for _worker in 0..WORKERS {
				let readiness_ref = &readiness;
				let timed_start_ref = &timed_start;
				let progressed_ref = &progressed;
				workers.push(scope.spawn(move || {
					readiness_ref.wait();
					timed_start_ref.wait();
					progressed_ref.fetch_add(1, Ordering::Release);
				}));
			}

			readiness.wait();
			assert_eq!(
				progressed.load(Ordering::Acquire),
				0,
				"readiness must not release timed work"
			);
			timed_start.wait();
			for worker in workers {
				assert!(worker.join().is_ok(), "barrier contract worker panicked");
			}
		});
		assert_eq!(
			progressed.load(Ordering::Acquire),
			WORKERS as u64,
			"timed release must advance every ready worker"
		);
	}

	/// The fenced TSC path requires both architectural RDTSCP and invariant-TSC
	/// capability bits, as well as the leaf that reports the latter.
	#[cfg(all(windows, target_arch = "x86_64"))]
	#[test]
	fn fenced_tsc_capability_requires_both_cpuid_bits() {
		assert!(supports_fenced_tsc(0x8000_0007, 1 << 27, 1 << 8));
		assert!(!supports_fenced_tsc(0x8000_0006, 1 << 27, 1 << 8));
		assert!(!supports_fenced_tsc(0x8000_0007, 0, 1 << 8));
		assert!(!supports_fenced_tsc(0x8000_0007, 1 << 27, 0));
	}
}
