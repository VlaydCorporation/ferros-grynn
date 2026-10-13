//! Stage29 RAM measurements with checked outputs. FG29_CASE selects fanout,
//! async, graph or retained; FG29_ROWS defaults to1000. Retained mode keeps32
//! old guards alive between stdout markers for external process RSS sampling.

use std::{
	hint::black_box,
	thread,
	time::{Duration, Instant},
};

use criterion::{BenchmarkId, Criterion};
use ferros_grynn::{
	datastore::{
		ClassField,
		ClassId,
		ClassSchema,
		Datastore,
		EventAsyncOptions,
		EventDefinition,
		EventExecution,
		EventTarget,
		ImportOptions,
		MaintenanceJobKind,
		MaintenanceJobState,
	},
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	graph::{AtomKind, Frozen, Graph, GraphKind, GraphRef},
	live::{LiveExpr, LiveGraphNext, LiveGraphResult, LiveOptions, LiveQuery, LiveSource},
	values::{Object, PropertyKeyId, PropertyMap, RecordId, Type, Value},
};

/// Propagates invalid observations instead of sampling failed operations.
fn require<T>(result: FerrosGrynnResult<T>) -> T {
	match result {
		Ok(value) => value,
		Err(error) => {
			eprintln!("Stage29 measurement failed: {error}");
			std::process::exit(1);
		}
	}
}

/// Owns a bounded fixture whose ordinary mutations remain observable.
struct Fixture {
	/// The production RAM backend, without changefeed capture.
	store: Datastore,
	/// Stable event owner shared by all fanout handlers.
	class: ClassId,
	/// Whole-graph selectors retain this source identity.
	graph: GraphRef,
	/// Source rows and their independently writable properties.
	records: Vec<RecordId>,
	/// The shared field handle resolved by schema registration.
	key: PropertyKeyId,
	/// Alternates real mutations to exclude equality no-ops.
	value: i64,
}

/// Private import seeds data before registering handlers or LIVE capture.
fn fixture(rows: usize) -> FerrosGrynnResult<Fixture> {
	if !(2..=100_000).contains(&rows) {
		return Err(FerrosGrynnError::invalid_argument("FG29_ROWS must be2..100000"));
	}
	let store = Datastore::in_memory();
	let mut schema = store.begin();
	let class =
		schema.define_class(ClassSchema::new("item", AtomKind::Node, [ClassField::new(
			"value",
			Type::Int,
		)])?)?;
	let graph = schema.define_graph("source", GraphKind::Graph)?;
	schema.commit()?;
	let key = store
		.property_dictionary()
		.get("value")
		.ok_or_else(|| FerrosGrynnError::invalid_argument("benchmark field handle absent"))?;
	let records = (0..rows)
		.map(|row| format!("item:r{row}").parse())
		.collect::<FerrosGrynnResult<Vec<RecordId>>>()?;
	store.import(ImportOptions::default(), |session| {
		for record in &records {
			session.apply_item(|tx| {
				tx.create_atom(record.clone(), PropertyMap::from_iter([(key, 0.into())]))?;
				tx.attach_node(graph.id(), record, None, None)?;
				Ok(())
			})?;
		}
		Ok(())
	})?;
	Ok(Fixture {
		store,
		class,
		graph,
		records,
		key,
		value: 0,
	})
}

/// Changes one stable source record through the ordinary transaction path.
fn update(fixture: &mut Fixture) -> FerrosGrynnResult<ferros_grynn::changefeed::Versionstamp> {
	fixture.value = 1 - fixture.value;
	let mut tx = fixture.store.begin();
	tx.set_base_property(&fixture.records[0], fixture.key, fixture.value.into())?;
	Ok(tx.commit()?.versionstamp())
}

/// One projected row selects every node/edge through the common graph
/// assembler.
fn recipe(fixture: &Fixture) -> LiveQuery {
	LiveQuery::new(LiveSource::GraphRecord {
		graph: fixture.graph.clone(),
		record: fixture.records[0].clone(),
	})
	.with_graph_result(
		LiveGraphResult::new(
			LiveExpr::Literal(fixture.graph.clone().into()),
			LiveExpr::Literal(fixture.graph.clone().into()),
		)
		.with_family(GraphKind::Graph),
	)
}

/// The initial checkpoint counts graph metadata as well as selected atoms.
fn graph_options(rows: usize) -> FerrosGrynnResult<LiveOptions> {
	let options = LiveOptions::default();
	let entries = options.queue_entries().max(rows + 2);
	let bytes = options.queue_bytes();
	println!("FG29_GRAPH_QUEUE entries={entries} bytes={bytes}");
	options.with_queue(entries, bytes)
}

/// Every output uses the normal GQL terminal and has the expected cardinality.
fn assemble(fixture: &Fixture) -> FerrosGrynnResult<()> {
	for result in fixture.store.execute(
		"RETURN GRAPH <GRAPH> {nodes:$graph, edges:$graph};",
		&Object::from_iter([("graph", Value::from(fixture.graph.clone()))]),
	)? {
		let Value::Graph(graph) = result.result?.into_parts().0 else {
			return Err(FerrosGrynnError::invalid_argument("graph result absent"));
		};
		if graph.node_count() != fixture.records.len() || graph.edge_count() != 0 {
			return Err(FerrosGrynnError::invalid_argument("graph oracle cardinality differs"));
		}
		black_box(graph);
	}
	Ok(())
}

/// Captures source acceptance-to-completion lag for64 durable invocations.
fn async_lag() -> FerrosGrynnResult<()> {
	let fixture = fixture(65)?;
	let mut tx = fixture.store.begin();
	tx.define_event(
		EventDefinition::new(
			"queued",
			EventTarget::Class(fixture.class),
			"$event = 'UPDATE' AND $after.value = 1",
			"{ UPDATE item:r64 SET value = 42; }",
		)?
		.with_execution(EventExecution::Async(EventAsyncOptions::default())),
	)?;
	tx.commit()?;
	let mut tx = fixture.store.begin();
	for record in fixture.records.iter().take(64) {
		tx.set_base_property(record, fixture.key, 1.into())?;
	}
	tx.commit()?;
	let started = Instant::now();
	loop {
		let jobs = fixture.store.jobs()?;
		let jobs = jobs
			.iter()
			.filter(|job| job.kind() == MaintenanceJobKind::EventInvocation)
			.collect::<Vec<_>>();
		if jobs.iter().any(|job| {
			matches!(job.state(), MaintenanceJobState::Failed | MaintenanceJobState::Cancelled)
		}) {
			return Err(FerrosGrynnError::invalid_argument("ASYNC invocation did not complete"));
		}
		if jobs.len() == 64 && jobs.iter().all(|job| job.state() == MaintenanceJobState::Completed)
		{
			let value = fixture
				.store
				.begin()
				.atom(&fixture.records[64])?
				.and_then(|atom| atom.properties().get(fixture.key).cloned());
			if value != Some(42.into()) {
				return Err(FerrosGrynnError::invalid_argument("ASYNC effect oracle differs"));
			}
			println!(
				"FG29_ASYNC_COMPLETED invocations=64 lag_us={}",
				started.elapsed().as_micros()
			);
			return Ok(());
		}
		if started.elapsed() > Duration::from_secs(60) {
			return Err(FerrosGrynnError::invalid_argument("ASYNC completion deadline exceeded"));
		}
		thread::sleep(Duration::from_millis(10));
	}
}

/// Old immutable revisions remain live at the second RSS observation marker.
fn retained(mut fixture: Fixture) -> FerrosGrynnResult<()> {
	let mut live = fixture.store.subscribe_live_graph::<Graph<Frozen>>(
		recipe(&fixture),
		graph_options(fixture.records.len())?,
	)?;
	println!("FG29_RSS_BASE pid={} rows={}", std::process::id(), fixture.records.len());
	thread::sleep(Duration::from_secs(2));
	let mut guards = Vec::new();
	for _ in 0..32 {
		guards.push(live.materialization()?.read()?);
		let revision = update(&mut fixture)?;
		if !matches!(live.next()?, LiveGraphNext::Updated { revision: accepted } if accepted == revision)
		{
			return Err(FerrosGrynnError::invalid_argument("retained revision oracle differs"));
		}
	}
	if guards.iter().any(|guard| guard.node_count() != fixture.records.len()) {
		return Err(FerrosGrynnError::invalid_argument("old guard cardinality changed"));
	}
	println!(
		"FG29_RSS_RETAINED pid={} rows={} guards={}",
		std::process::id(),
		fixture.records.len(),
		guards.len()
	);
	thread::sleep(Duration::from_secs(5));
	black_box(guards);
	live.close()
}

/// Isolates work units; graph setup and handler registration are outside
/// timing.
fn run() -> FerrosGrynnResult<()> {
	let case = std::env::var("FG29_CASE").map_err(|error| {
		FerrosGrynnError::invalid_argument("FG29_CASE is required").with_technical_source(error)
	})?;
	let rows = match std::env::var("FG29_ROWS") {
		Ok(value) => value.parse::<usize>().map_err(|error| {
			FerrosGrynnError::invalid_argument("FG29_ROWS must be an integer")
				.with_technical_source(error)
		})?,
		Err(std::env::VarError::NotPresent) => 1000,
		Err(error) => {
			return Err(FerrosGrynnError::invalid_argument("FG29_ROWS is not Unicode")
				.with_technical_source(error));
		}
	};
	if case == "async" {
		return async_lag();
	}
	if case == "retained" {
		return retained(fixture(rows)?);
	}
	let mut criterion = Criterion::default().configure_from_args();
	let mut group = criterion.benchmark_group("gql_stage29");
	group.sample_size(10);
	group.warm_up_time(Duration::from_millis(500));
	group.measurement_time(Duration::from_secs(2));
	match case.as_str() {
		"fanout" => {
			for fanout in [1, 8, 64] {
				let mut fixture = fixture(2)?;
				let mut tx = fixture.store.begin();
				for id in 0..fanout {
					tx.define_event(EventDefinition::new(
						format!("sync_{id}"),
						EventTarget::Class(fixture.class),
						"$event = 'UPDATE'",
						"1",
					)?)?;
				}
				tx.commit()?;
				group.bench_function(BenchmarkId::new("sync_fanout", fanout), |bench| {
					bench.iter(|| require(update(black_box(&mut fixture))))
				});
			}
		}
		"graph" => {
			let mut fixture = fixture(rows)?;
			assemble(&fixture)?;
			group.bench_function(BenchmarkId::new("assembly", rows), |bench| {
				bench.iter(|| require(assemble(black_box(&fixture))))
			});
			let mut live = fixture
				.store
				.subscribe_live_graph::<Graph<Frozen>>(recipe(&fixture), graph_options(rows)?)?;
			group.bench_function(BenchmarkId::new("commit_delta_apply_ack", rows), |bench| {
				bench.iter(|| {
					let revision = require(update(&mut fixture));
					let event = require(live.next());
					if !matches!(event, LiveGraphNext::Updated { revision: accepted } if accepted == revision)
					{
						require::<()>(Err(FerrosGrynnError::invalid_argument(
							"graph delivery oracle differs",
						)));
					}
					black_box(require(live.materialization()));
				})
			});
			live.close()?;
		}
		_ => return Err(FerrosGrynnError::invalid_argument("unknown FG29_CASE")),
	}
	group.finish();
	criterion.final_summary();
	Ok(())
}

/// Measurement errors always stop the process with an explicit diagnostic.
fn main() {
	require(run());
}
