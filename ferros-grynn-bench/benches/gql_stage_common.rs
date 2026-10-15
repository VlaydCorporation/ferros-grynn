//! Common Stage-19/current probes used to detect Stage-20/21 regressions.

use std::{hint::black_box, time::Duration};

use criterion::{BatchSize, Criterion, criterion_group, criterion_main};
use ferros_grynn::{
	datastore::{Datastore, ImportOptions},
	values::{Object, PropertyKeyId, PropertyMap, RecordId, Value},
};

/// Default record population shared by staged GQL benchmarks.
const DEFAULT_ROWS: usize = 2_000;

/// Seeded datastore, records and property key used by mutation benchmarks.
struct MutationFixture {
	/// Datastore retained by the fixture or operation.
	store: Datastore,
	/// Stable record identities seeded for the scenario.
	records: Vec<RecordId>,
	/// Interned property-name identifier used for value lookup.
	key: PropertyKeyId,
}

/// Returns rows for this module.
fn rows() -> usize {
	std::env::var("FG_STAGE_GATE_ROWS")
		.ok()
		.and_then(|value| value.parse().ok())
		.unwrap_or(DEFAULT_ROWS)
		.max(1)
}

/// Returns assert query for this module.
fn assert_query(store: &Datastore, query: &str) {
	let results = store.execute(query, &Object::new()).expect("gate query must execute");
	if let Some(error) = results.iter().find_map(|result| result.result.as_ref().err()) {
		panic!("gate query failed: {error}");
	}
	black_box(results);
}

/// Builds the mutation fixture used by nearby scenarios.
fn mutation_fixture(index: bool) -> MutationFixture {
	let store = Datastore::in_memory();
	assert_query(
		&store,
		"DEFINE CLASS person TYPE NODE; DEFINE FIELD score ON CLASS person TYPE int;",
	);
	let key = store.property_dictionary().get("score").expect("score property must exist");
	let records = (0..rows())
		.map(|ordinal| format!("person:r{ordinal}").parse::<RecordId>().expect("valid record id"))
		.collect::<Vec<_>>();
	let mut import_options = ImportOptions::default();
	import_options.segment_size = records.len().max(1);
	store
		.import(import_options, |session| {
			for (ordinal, record) in records.iter().enumerate() {
				session.apply_item(|transaction| {
					let mut properties = PropertyMap::new();
					properties.insert(key, Value::from(ordinal));
					transaction.create_atom(record.clone(), properties)
				})?;
			}
			Ok(())
		})
		.expect("fixture import must succeed");
	if index {
		assert_query(&store, "DEFINE INDEX by_score ON CLASS person FIELDS score SCOPE BASE;");
	}
	MutationFixture {
		store,
		records,
		key,
	}
}

/// Returns mutate all for this module.
fn mutate_all(fixture: &mut MutationFixture) {
	let mut transaction = fixture.store.begin();
	for (ordinal, record) in fixture.records.iter().enumerate() {
		transaction
			.set_base_property(record, fixture.key, Value::from(ordinal.saturating_add(rows())))
			.expect("point update must succeed");
	}
	black_box(transaction.commit().expect("mutation batch must commit"));
}

/// Measures common with the benchmark harness.
fn benchmark_common(criterion: &mut Criterion) {
	let return_store = Datastore::in_memory();
	let mut query = criterion.benchmark_group("gql_stage_common/query");
	query.warm_up_time(Duration::from_secs(3));
	query.measurement_time(Duration::from_secs(10));
	query.sample_size(100);
	query.bench_function("return_literal", |bench| {
		bench.iter(|| assert_query(black_box(&return_store), black_box("RETURN 1;")));
	});
	query.finish();

	let mut mutation = criterion.benchmark_group("gql_stage_common/mutation");
	mutation.warm_up_time(Duration::from_secs(1));
	mutation.measurement_time(Duration::from_secs(4));
	mutation.sample_size(10);
	// Rebuild each fixture outside timing so every mutation sample starts from
	// the same data.
	mutation.bench_function("no_index", |bench| {
		bench.iter_batched_ref(|| mutation_fixture(false), mutate_all, BatchSize::LargeInput);
	});
	mutation.bench_function("immediate_index", |bench| {
		bench.iter_batched_ref(|| mutation_fixture(true), mutate_all, BatchSize::LargeInput);
	});
	mutation.finish();

	let mut build = criterion.benchmark_group("gql_stage_common/index_build");
	build.warm_up_time(Duration::from_secs(1));
	build.measurement_time(Duration::from_secs(4));
	build.sample_size(10);
	build.bench_function("standard", |bench| {
		bench.iter_batched_ref(
			|| mutation_fixture(false),
			|fixture| {
				assert_query(
					&fixture.store,
					"DEFINE INDEX by_score ON CLASS person FIELDS score SCOPE BASE;",
				);
			},
			BatchSize::LargeInput,
		);
	});
	build.finish();
}

criterion_group!(gates, benchmark_common);
criterion_main!(gates);
