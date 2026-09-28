//! End-to-end sparse-frontier probe for the Stage-17 local traversal cursor.

use std::{
	error::Error,
	fs::OpenOptions,
	hint::black_box,
	io::Write,
	path::{Path, PathBuf},
	time::Duration,
};

use ferros_grynn::{
	datastore::{CheckpointMode, ClassSchema, Datastore, IoMode, LocalOptions},
	graph::{AtomKind, EdgeTopology, GraphKind, GraphRef},
	storage_engine_prototype_api::clear_resident_pages,
	values::{Object, PropertyMap, RecordId, Uuid, Value},
};

use crate::harness::{MeasurementTimer, process_memory_snapshot};

const QUERY: &str = "TRAVERSE FROM $source IN GRAPH $graph |> tv::out() |> tv::values('id');";
const EXPLAIN: &str =
	"EXPLAIN ANALYZE FORMAT JSON TRAVERSE FROM $source IN GRAPH $graph |> tv::out();";
const FRONTIER_WIDTH: usize = 4;
const SLOT_GAP: usize = 102;
const DEFAULT_QUERIES_PER_SAMPLE: usize = 64;

struct TestDirectory(PathBuf);

impl TestDirectory {
	fn new() -> Self {
		Self(
			std::env::temp_dir()
				.join(format!("fgdb-{:016x}", Uuid::now_v7().as_inner().as_u128() as u64)),
		)
	}
}

impl Drop for TestDirectory {
	fn drop(&mut self) {
		drop(std::fs::remove_dir_all(&self.0));
	}
}

struct Fixture {
	directory: TestDirectory,
	graph: GraphRef,
	source: RecordId,
	expected: Value,
}

#[derive(Clone, Copy)]
struct Sample {
	wall_time: Duration,
	cpu_time: Duration,
	rss_delta_bytes: i64,
	page_fault_delta: u32,
}

#[derive(Clone, Copy)]
enum Candidate {
	Sync,
	IoRing,
}

impl Candidate {
	const fn name(self) -> &'static str {
		match self {
			Self::Sync => "sync_direct",
			Self::IoRing => "frame_ioring_qd32",
		}
	}

	const fn mode(self) -> IoMode {
		match self {
			Self::Sync => IoMode::Sync,
			Self::IoRing => IoMode::NativeAsync,
		}
	}
}

/// Runs the real local traversal cursor against identical cold R8 state.
///
/// The four selected connector and target pages are deliberately separated by
/// one complete hot-page slot range. Each timed sample reopens the datastore,
/// so resident state never leaks between paired candidates.
#[test]
#[ignore = "paired Direct/IoRing benchmark; writes target/gql-stage17 artifacts"]
fn storage_engine_prototype_traversal_frontier_probe() -> Result<(), Box<dyn Error>> {
	let fixture = build_fixture()?;
	let repetitions = std::env::var("FG_STAGE17_Q10_REPETITIONS")
		.ok()
		.and_then(|value| value.parse::<usize>().ok())
		.unwrap_or(7)
		.max(1);
	let queries_per_sample = std::env::var("FG_STAGE17_Q10_QUERIES_PER_SAMPLE")
		.ok()
		.and_then(|value| value.parse::<usize>().ok())
		.unwrap_or(DEFAULT_QUERIES_PER_SAMPLE)
		.max(1);
	if queries_per_sample > 64 {
		return Err("FG_STAGE17_Q10_QUERIES_PER_SAMPLE must be at most 64".into());
	}
	let artifact = artifact_file()?;
	let mut csv = OpenOptions::new().create(true).truncate(true).write(true).open(&artifact)?;
	writeln!(
		csv,
		"repetition,order,candidate,queries_per_sample,wall_time_ns,cpu_time_ns,cpu_wall_ratio,\
		 rss_delta_bytes,page_fault_delta"
	)?;
	let mut sync_samples = Vec::with_capacity(repetitions);
	let mut ioring_samples = Vec::with_capacity(repetitions);

	for repetition in 0..repetitions {
		let order = if repetition % 2 == 0 {
			[Candidate::Sync, Candidate::IoRing]
		} else {
			[Candidate::IoRing, Candidate::Sync]
		};
		for (position, candidate) in order.into_iter().enumerate() {
			let sample = measure(&fixture, candidate, queries_per_sample)?;
			let cpu_wall_ratio = if sample.wall_time.is_zero() {
				0.0
			} else {
				sample.cpu_time.as_secs_f64() / sample.wall_time.as_secs_f64()
			};
			writeln!(
				csv,
				"{repetition},{position},{},{queries_per_sample},{},{},{cpu_wall_ratio:.6},{},{}",
				candidate.name(),
				sample.wall_time.as_nanos(),
				sample.cpu_time.as_nanos(),
				sample.rss_delta_bytes,
				sample.page_fault_delta,
			)?;
			match candidate {
				Candidate::Sync => sync_samples.push(sample),
				Candidate::IoRing => ioring_samples.push(sample),
			}
		}
	}
	csv.flush()?;
	let sync = summarize(&sync_samples);
	let ioring = summarize(&ioring_samples);
	let decision = gate_decision(sync, ioring);

	let metrics = transport_metrics(&fixture, Candidate::IoRing)?;
	if metrics.ioring_pages == 0 {
		return Err(format!(
			"NativeAsync traversal did not submit a frame-IoRing sparse window: windows={}, \
			 resident={}, wal={}, sync={}, ioring={}",
			metrics.windows,
			metrics.resident_pages,
			metrics.wal_pages,
			metrics.sync_pages,
			metrics.ioring_pages,
		)
		.into());
	}
	println!(
		"artifact={} decision={} sync_median_ns={} ioring_median_ns={} sync_p99_ns={} \
		 ioring_p99_ns={} sync_cpu_total_ns={} ioring_cpu_total_ns={} physical_windows={} \
		 resident_pages={} wal_pages={} sync_pages={} ioring_pages={}",
		artifact.display(),
		decision,
		sync.wall_median_ns,
		ioring.wall_median_ns,
		sync.wall_p99_ns,
		ioring.wall_p99_ns,
		sync.cpu_total_ns,
		ioring.cpu_total_ns,
		metrics.windows,
		metrics.resident_pages,
		metrics.wal_pages,
		metrics.sync_pages,
		metrics.ioring_pages,
	);
	Ok(())
}

fn build_fixture() -> Result<Fixture, Box<dyn Error>> {
	let directory = TestDirectory::new();
	let store = Datastore::create_local(&directory.0, local_options(IoMode::Sync))?;
	let mut transaction = store.begin();
	transaction.define_class(ClassSchema::new("person", AtomKind::Node, [])?)?;
	transaction.define_class(ClassSchema::new("link", AtomKind::Edge, [])?)?;
	let graph = transaction.define_graph("q10_frontier", GraphKind::Graph)?;
	let source = "person:source".parse::<RecordId>()?;
	transaction.create_atom(source.clone(), PropertyMap::new())?;
	let source_id = transaction.attach_node(graph.id(), &source, None, None)?;

	let mut targets = Vec::with_capacity(FRONTIER_WIDTH);
	let mut filler_nodes = Vec::with_capacity((FRONTIER_WIDTH - 1) * SLOT_GAP);
	for target_index in 0..FRONTIER_WIDTH {
		let target = format!("person:target-{target_index}").parse::<RecordId>()?;
		transaction.create_atom(target.clone(), PropertyMap::new())?;
		let target_id = transaction.attach_node(graph.id(), &target, None, None)?;
		targets.push((target, target_id));
		if target_index + 1 < FRONTIER_WIDTH {
			for offset in 0..SLOT_GAP {
				let record =
					format!("person:filler-node-{target_index}-{offset}").parse::<RecordId>()?;
				transaction.create_atom(record.clone(), PropertyMap::new())?;
				filler_nodes.push(transaction.attach_node(graph.id(), &record, None, None)?);
			}
		}
	}

	let filler_source = filler_nodes[0];
	let filler_target = filler_nodes[1];
	for (target_index, (_target, target_id)) in targets.iter().enumerate() {
		let edge = format!("link:frontier-{target_index}").parse::<RecordId>()?;
		transaction.create_atom(edge.clone(), PropertyMap::new())?;
		transaction.attach_edge(
			graph.id(),
			&edge,
			None,
			EdgeTopology::directed([source_id], [*target_id]),
			None,
			None,
		)?;
		if target_index + 1 < FRONTIER_WIDTH {
			for offset in 0..SLOT_GAP {
				let edge =
					format!("link:filler-edge-{target_index}-{offset}").parse::<RecordId>()?;
				transaction.create_atom(edge.clone(), PropertyMap::new())?;
				transaction.attach_edge(
					graph.id(),
					&edge,
					None,
					EdgeTopology::directed([filler_source], [filler_target]),
					None,
					None,
				)?;
			}
		}
	}
	transaction.commit()?;
	let _checkpointed = store.checkpoint(CheckpointMode::Restart)?;
	drop(store);

	let expected = Value::Array(
		targets
			.into_iter()
			.map(|(record, _id)| Value::Record(Box::new(record)))
			.collect::<Vec<_>>()
			.into(),
	);
	Ok(Fixture {
		directory,
		graph,
		source,
		expected,
	})
}

fn measure(
	fixture: &Fixture,
	candidate: Candidate,
	queries_per_sample: usize,
) -> Result<Sample, Box<dyn Error>> {
	let store = Datastore::open_local(&fixture.directory.0, local_options(candidate.mode()))?;
	let variables = variables(fixture);
	let memory_before = process_memory_snapshot()?;
	let mut wall_time = Duration::ZERO;
	let mut cpu_time = Duration::ZERO;
	for _ in 0..queries_per_sample {
		let _cleared = clear_resident_pages(&store)?;
		let timer = MeasurementTimer::start()?;
		let value = store.execute(QUERY, &variables)?.remove(0).result?.into_parts().0;
		let timing = timer.finish()?;
		wall_time += timing.elapsed;
		cpu_time += timing.cpu_time;
		if value != fixture.expected {
			return Err(
				format!("{} traversal result differs from the fixture", candidate.name()).into()
			);
		}
		black_box(value);
	}
	let memory_after = process_memory_snapshot()?;
	drop(store);
	let divisor = u32::try_from(queries_per_sample)?;
	Ok(Sample {
		wall_time: wall_time / divisor,
		cpu_time: cpu_time / divisor,
		rss_delta_bytes: signed_delta(
			memory_after.working_set_bytes,
			memory_before.working_set_bytes,
		),
		page_fault_delta: memory_after
			.page_fault_count
			.saturating_sub(memory_before.page_fault_count),
	})
}

#[derive(Clone, Copy)]
struct Summary {
	wall_median_ns: u128,
	wall_p99_ns: u128,
	cpu_total_ns: u128,
}

fn summarize(samples: &[Sample]) -> Summary {
	let mut wall = samples.iter().map(|sample| sample.wall_time.as_nanos()).collect::<Vec<_>>();
	wall.sort_unstable();
	Summary {
		wall_median_ns: wall[wall.len() / 2],
		wall_p99_ns: wall[wall.len().saturating_sub(1)],
		cpu_total_ns: samples.iter().map(|sample| sample.cpu_time.as_nanos()).sum(),
	}
}

fn gate_decision(sync: Summary, ioring: Summary) -> &'static str {
	let throughput_gate =
		ioring.wall_median_ns.saturating_mul(100) <= sync.wall_median_ns.saturating_mul(80);
	let p99_gate = ioring.wall_p99_ns.saturating_mul(100) <= sync.wall_p99_ns.saturating_mul(105);
	let cpu_gate = ioring.cpu_total_ns.saturating_mul(100) <= sync.cpu_total_ns.saturating_mul(110);
	if throughput_gate && p99_gate && cpu_gate {
		"enable_ioring"
	} else {
		"keep_sync"
	}
}

fn signed_delta(after: u64, before: u64) -> i64 {
	if after >= before {
		i64::try_from(after - before).unwrap_or(i64::MAX)
	} else {
		i64::try_from(before - after).map_or(i64::MIN, |delta| -delta)
	}
}

struct TransportMetrics {
	windows: i64,
	resident_pages: i64,
	wal_pages: i64,
	sync_pages: i64,
	ioring_pages: i64,
}

fn transport_metrics(
	fixture: &Fixture,
	candidate: Candidate,
) -> Result<TransportMetrics, Box<dyn Error>> {
	let store = Datastore::open_local(&fixture.directory.0, local_options(candidate.mode()))?;
	let _cleared = clear_resident_pages(&store)?;
	let value = store.execute(EXPLAIN, &variables(fixture))?.remove(0).result?.into_parts().0;
	let Value::Object(root) = value else {
		return Err("EXPLAIN FORMAT JSON did not return an object".into());
	};
	let Some(Value::Object(metrics)) = root.get("analyze") else {
		return Err("EXPLAIN ANALYZE did not return metrics".into());
	};
	Ok(TransportMetrics {
		windows: integer(metrics, "physical_windows")?,
		resident_pages: integer(metrics, "physical_resident_pages")?,
		wal_pages: integer(metrics, "physical_wal_pages")?,
		sync_pages: integer(metrics, "physical_sync_pages")?,
		ioring_pages: integer(metrics, "physical_ioring_pages")?,
	})
}

fn integer(values: &Object, field: &str) -> Result<i64, Box<dyn Error>> {
	values
		.get(field)
		.and_then(Value::as_number)
		.and_then(|value| value.as_i64())
		.ok_or_else(|| format!("EXPLAIN metric {field} is not an integer").into())
}

fn variables(fixture: &Fixture) -> Object {
	let mut variables = Object::new();
	variables.insert("graph", Value::GraphRef(Box::new(fixture.graph.clone())));
	variables.insert("source", Value::Record(Box::new(fixture.source.clone())));
	variables
}

fn local_options(mode: IoMode) -> LocalOptions {
	let mut options = LocalOptions::default();
	options.io_mode = mode;
	options.buffer_pool_pages = 1_024;
	options
}

fn artifact_file() -> Result<PathBuf, Box<dyn Error>> {
	let root = Path::new(env!("CARGO_MANIFEST_DIR"))
		.join("..")
		.join("..")
		.join("target")
		.join("gql-stage17");
	std::fs::create_dir_all(&root)?;
	Ok(root.join(format!("q10-traversal-frontier-{}.csv", Uuid::now_v7())))
}
