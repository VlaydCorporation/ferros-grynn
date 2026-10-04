//! Comparable pre-change/current workloads without changefeed API calls.
//! FG27_ROWS selects the background cardinality; mutation batches remain 64.

use std::{hint::black_box, time::Duration};

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
	store: Datastore,
	records: Vec<RecordId>,
	key: PropertyKeyId,
	value: i64,
}

/// Imports a background dataset through the existing private segment API.
fn fixture(rows: usize) -> FerrosGrynnResult<Fixture> {
	let store = Datastore::in_memory();
	let mut tx = store.begin();
	tx.define_class(ClassSchema::new("item", AtomKind::Node, [ClassField::new(
		"value",
		Type::Int,
	)])?)?;
	tx.commit()?;
	let key = store
		.property_dictionary()
		.get("value")
		.ok_or_else(|| FerrosGrynnError::invalid_argument("benchmark field absent"))?;
	let records = (0..rows)
		.map(|row| format!("item:r{row}").parse::<RecordId>())
		.collect::<FerrosGrynnResult<Vec<_>>>()?;
	let mut options = ImportOptions::default();
	options.segment_size = 2048;
	store.import(options, |session| {
		for record in &records {
			session.apply_item(|tx| {
				let mut properties = PropertyMap::new();
				properties.insert(key, 0);
				tx.create_atom(record.clone(), properties)
			})?;
		}
		Ok(())
	})?;
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
		10
	} else {
		2
	}));
	group.bench_function(BenchmarkId::new("return_literal", rows), |bench| {
		bench.iter(|| require(query(black_box(&fixture.store))))
	});
	group.bench_function(BenchmarkId::new("point_read_16", rows), |bench| {
		bench.iter(|| require(point_read(black_box(&fixture))))
	});
	group.bench_function(BenchmarkId::new("mutate_64", rows), |bench| {
		bench.iter(|| require(mutate(black_box(&mut fixture))))
	});
	group.finish();
	criterion.final_summary();
	Ok(())
}

/// Command failures are explicit and never treated as benchmark samples.
fn main() {
	require(run());
}
