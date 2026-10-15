//! Comparable pre-change/current workloads without changefeed API calls.
//! FG27_ROWS selects the background cardinality; mutation batches remain 64.

use std::{
	hint::black_box,
	time::{Duration, Instant},
};

use criterion::{BenchmarkId, Criterion};
use ferros_grynn::{
	datastore::{ClassField, ClassSchema, Datastore, ImportOptions},
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	graph::AtomKind,
	values::{Object, PropertyKeyId, PropertyMap, RecordId, Type},
};

/// Stops a benchmark on failed admission or incorrect output.
fn require<T>(result: FerrosGrynnResult<T>) -> T {
	match result {
		Ok(value) => value,
		Err(error) => {
			eprintln!("Stage-27 benchmark failed: {error}");
			std::process::exit(1);
		}
	}
}

/// Owned fixture reused only for bounded mutations and point reads.
struct Fixture {
	/// Datastore retained by the fixture or operation.
	store: Datastore,
	/// Stable record identities seeded for the scenario.
	records: Vec<RecordId>,
	/// Interned property-name identifier used for value lookup.
	key: PropertyKeyId,
	/// Payload retained for the operation or its expected result.
	value: i64,
}

/// Imports a background dataset through the existing private segment API.
fn fixture(rows: usize) -> FerrosGrynnResult<Fixture> {
	fixture_profile(rows, None)
}

/// Accumulates phase wall time only for the explicit diagnostic path.
struct PhaseClock<'a> {
	/// Per-phase durations accumulated by the active measurement clock.
	totals: &'a mut [Duration; 9],
	/// Start of the most recently measured phase.
	last: Instant,
}

impl PhaseClock<'_> {
	/// Records one completed fixture phase without per-item timer overhead.
	fn mark(&mut self, index: usize) {
		let now = Instant::now();
		self.totals[index] += now.duration_since(self.last);
		self.last = now;
	}
}

/// Uses the same public operations with optional phase timing.
fn fixture_profile(rows: usize, totals: Option<&mut [Duration; 9]>) -> FerrosGrynnResult<Fixture> {
	let mut clock = totals.map(|totals| PhaseClock {
		totals,
		last: Instant::now(),
	});
	let store = Datastore::in_memory();
	if let Some(clock) = &mut clock {
		clock.mark(0);
	}
	let mut tx = store.begin();
	tx.define_class(ClassSchema::new("item", AtomKind::Node, [ClassField::new(
		"value",
		Type::Int,
	)])?)?;
	if let Some(clock) = &mut clock {
		clock.mark(1);
	}
	tx.commit()?;
	if let Some(clock) = &mut clock {
		clock.mark(2);
	}
	let key = store
		.property_dictionary()
		.get("value")
		.ok_or_else(|| FerrosGrynnError::invalid_argument("benchmark field absent"))?;
	let records = (0..rows)
		.map(|row| format!("item:r{row}").parse::<RecordId>())
		.collect::<FerrosGrynnResult<Vec<_>>>()?;
	if let Some(clock) = &mut clock {
		clock.mark(3);
	}
	let mut options = ImportOptions::default();
	options.segment_size = 2048;
	let report = store.import(options, |session| {
		for record in &records {
			session.apply_item(|tx| {
				let mut properties = PropertyMap::new();
				properties.insert(key, 0);
				tx.create_atom(record.clone(), properties)
			})?;
		}
		Ok(())
	})?;
	if let Some(clock) = &mut clock {
		clock.mark(4);
		for (index, phase) in ["callback", "validation", "publication"].into_iter().enumerate() {
			clock.totals[6 + index] += *report.phase_timings().get(phase).ok_or_else(|| {
				FerrosGrynnError::invalid_argument("import diagnostic phase absent")
			})?;
		}
	}
	Ok(Fixture {
		store,
		records,
		key,
		value: 0,
	})
}

/// Alternates real values in a fixed-size transaction without history growth.
fn mutate(fixture: &mut Fixture) -> FerrosGrynnResult<()> {
	fixture.value = 1 - fixture.value;
	let mut tx = fixture.store.begin();
	for record in fixture.records.iter().take(64) {
		tx.set_base_property(record, fixture.key, fixture.value.into())?;
	}
	black_box(tx.commit()?);
	Ok(())
}

/// Includes private import, its fresh catalog setup and datastore teardown.
fn import_1000() -> FerrosGrynnResult<()> {
	black_box(fixture(1_000)?);
	Ok(())
}

/// Reads exact existing records with a bounded public snapshot.
fn point_read(fixture: &Fixture) -> FerrosGrynnResult<()> {
	let mut tx = fixture.store.begin();
	for record in fixture.records.iter().take(16) {
		let atom = tx
			.atom(record)?
			.ok_or_else(|| FerrosGrynnError::invalid_argument("benchmark record absent"))?;
		if atom.properties().get(fixture.key) != Some(&fixture.value.into()) {
			return Err(FerrosGrynnError::invalid_argument("benchmark point result differs"));
		}
		black_box(atom);
	}
	Ok(())
}

/// Executes the unchanged query pipeline and propagates statement errors.
fn query(store: &Datastore) -> FerrosGrynnResult<()> {
	let results = store.execute("RETURN 1", &Object::new())?;
	if results.len() != 1 {
		return Err(FerrosGrynnError::invalid_argument("benchmark query cardinality differs"));
	}
	for result in results {
		black_box(result.result?);
	}
	Ok(())
}

/// Runs a maintained Criterion matrix on one controlled cardinality.
fn run() -> FerrosGrynnResult<()> {
	if std::env::var("FG29_IMPORT_PROBE").is_ok_and(|value| value == "1") {
		let background = fixture(100_000)?;
		let mut totals = [Duration::ZERO; 9];
		for _ in 0..1000 {
			let fixture = fixture_profile(1000, Some(&mut totals))?;
			point_read(&fixture)?;
			let started = Instant::now();
			drop(fixture);
			totals[5] += started.elapsed();
		}
		black_box(background);
		for (phase, time) in [
			"store",
			"define",
			"schema_commit",
			"records",
			"import",
			"drop",
			"callback",
			"validation",
			"publication",
		]
		.into_iter()
		.zip(totals)
		{
			println!("FG29_IMPORT_PHASE phase={phase} fixtures=1000 ns={}", time.as_nanos());
		}
		return Ok(());
	}
	let rows = match std::env::var("FG27_ROWS") {
		Ok(value) => value
			.parse::<usize>()
			.map_err(|error| FerrosGrynnError::invalid_argument(error.to_string()))?,
		Err(std::env::VarError::NotPresent) => 10_000,
		Err(error) => return Err(FerrosGrynnError::invalid_argument(error.to_string())),
	};
	if ![10_000, 100_000, 1_000_000].contains(&rows) {
		return Err(FerrosGrynnError::invalid_argument(
			"FG27_ROWS must be 10000, 100000 or 1000000",
		));
	}
	let mut fixture = fixture(rows)?;
	point_read(&fixture)?;
	query(&fixture.store)?;
	let mut criterion = Criterion::default().configure_from_args();
	let mut group = criterion.benchmark_group("gql_stage27_common");
	let extended = std::env::var("FG27_LONG").is_ok_and(|value| value == "1");
	group.sample_size(if extended {
		50
	} else {
		10
	});
	group.warm_up_time(Duration::from_millis(if extended {
		2000
	} else {
		500
	}));
	group.measurement_time(Duration::from_secs(if extended {
		30
	} else {
		2
	}));
	group.bench_function(BenchmarkId::new("return_literal", rows), |bench| {
		bench.iter(|| require(query(black_box(&fixture.store))))
	});
	group.bench_function(BenchmarkId::new("point_read_16", rows), |bench| {
		bench.iter(|| require(point_read(black_box(&fixture))))
	});
	if rows == 1_000_000 {
		group.sample_size(10);
	}
	group.bench_function(BenchmarkId::new("mutate_64", rows), |bench| {
		bench.iter(|| require(mutate(black_box(&mut fixture))))
	});
	group.sample_size(if extended {
		50
	} else {
		10
	});
	group.bench_function(BenchmarkId::new("import_1000", rows), |bench| {
		bench.iter(|| require(import_1000()))
	});
	group.finish();
	criterion.final_summary();
	Ok(())
}

/// Command failures are explicit and never treated as benchmark samples.
fn main() {
	require(run());
}
