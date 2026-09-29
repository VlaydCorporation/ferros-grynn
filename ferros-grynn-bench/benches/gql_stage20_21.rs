//! Stage-20/21 option, UDF and collation performance probes.

use std::{
	hint::black_box,
	num::NonZeroUsize,
	path::PathBuf,
	sync::atomic::{AtomicU64, Ordering},
	thread,
	time::{Duration, Instant},
};

use criterion::{BatchSize, Criterion, criterion_group, criterion_main};
use ferros_grynn::{
	datastore::{Datastore, ImportOptions, LocalOptions, MaintenanceJobState},
	gql::QueryExecutionOptions,
	values::{Object, PropertyKeyId, PropertyMap, RecordId, Value},
};

const DEFAULT_ROWS: usize = 10_000;
static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

struct MutationFixture {
	store: Datastore,
	records: Vec<RecordId>,
	key: PropertyKeyId,
}

struct LocalMaintenanceFixture {
	_store_directory: PathBuf,
	fixture: MutationFixture,
}

impl Drop for LocalMaintenanceFixture {
	fn drop(&mut self) {
		drop(std::fs::remove_dir_all(&self._store_directory));
	}
}

fn rows() -> usize {
	std::env::var("FG_STAGE_GATE_ROWS")
		.ok()
		.and_then(|value| value.parse().ok())
		.unwrap_or(DEFAULT_ROWS)
		.max(1)
}

fn assert_query(store: &Datastore, query: &str) -> Value {
	let results = store.execute(query, &Object::new()).expect("gate query must execute");
	if let Some(error) = results.iter().find_map(|result| result.result.as_ref().err()) {
		panic!("gate query failed: {error}");
	}
	results
		.last()
		.and_then(|result| result.result.as_ref().ok())
		.map(|outcome| outcome.value().clone())
		.unwrap_or(Value::None)
}

fn mutation_fixture(mode: &str) -> MutationFixture {
	prepare_mutation_fixture(Datastore::in_memory(), mode)
}

fn prepare_mutation_fixture(store: Datastore, mode: &str) -> MutationFixture {
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
	let suffix = if mode == "deferred" {
		" DEFER"
	} else {
		""
	};
	assert_query(
		&store,
		&format!("DEFINE INDEX by_score ON CLASS person FIELDS score SCOPE BASE{suffix};"),
	);
	MutationFixture {
		store,
		records,
		key,
	}
}

fn local_maintenance_fixture() -> LocalMaintenanceFixture {
	let sequence = TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
	let directory =
		std::env::temp_dir().join(format!("fgs20-{:x}-{sequence:x}", std::process::id()));
	let mut options = LocalOptions::default();
	options.buffer_pool_pages = 256;
	let store = Datastore::create_local(&directory, options)
		.expect("local maintenance fixture must be created");
	LocalMaintenanceFixture {
		_store_directory: directory,
		fixture: prepare_mutation_fixture(store, "immediate"),
	}
}

fn mutate_all(fixture: &mut MutationFixture) {
	let mut transaction = fixture.store.begin();
	for (ordinal, record) in fixture.records.iter().enumerate() {
		transaction
			.set_base_property(record, fixture.key, Value::from(ordinal.saturating_add(rows())))
			.expect("point update must succeed");
	}
	black_box(transaction.commit().expect("mutation batch must commit"));
}

fn wait_for_jobs(store: &Datastore) {
	let deadline = Instant::now() + Duration::from_secs(60);
	loop {
		if store.jobs().expect("jobs must remain readable").iter().all(|job| {
			matches!(
				job.state(),
				MaintenanceJobState::Completed
					| MaintenanceJobState::Cancelled
					| MaintenanceJobState::Failed
			)
		}) {
			return;
		}
		assert!(Instant::now() < deadline, "maintenance job did not reach a terminal state");
		thread::sleep(Duration::from_millis(1));
	}
}

fn benchmark_stage20(criterion: &mut Criterion) {
	let mut deferred = criterion.benchmark_group("gql_stage20/deferred_mutation");
	deferred.warm_up_time(Duration::from_secs(1));
	deferred.measurement_time(Duration::from_secs(4));
	deferred.sample_size(10);
	deferred.bench_function("immediate", |bench| {
		bench.iter_batched_ref(|| mutation_fixture("immediate"), mutate_all, BatchSize::LargeInput);
	});
	deferred.bench_function("deferred", |bench| {
		bench.iter_batched_ref(|| mutation_fixture("deferred"), mutate_all, BatchSize::LargeInput);
	});
	deferred.finish();

	let parallelism = thread::available_parallelism().map_or(1, NonZeroUsize::get).min(8);
	let mut rebuild = criterion.benchmark_group("gql_stage20/rebuild");
	rebuild.warm_up_time(Duration::from_secs(1));
	rebuild.measurement_time(Duration::from_secs(4));
	rebuild.sample_size(10);
	for workers in [1, parallelism] {
		rebuild.bench_function(format!("parallelism_{workers}"), |bench| {
			bench.iter_batched_ref(
				|| mutation_fixture("immediate"),
				|fixture| {
					assert_query(
						&fixture.store,
						&format!(
							"REBUILD INDEX by_score ON CLASS person WITH (PARALLELISM = \
							 {workers});"
						),
					);
				},
				BatchSize::LargeInput,
			);
		});
	}
	rebuild.finish();

	let clean = Datastore::in_memory();
	let completed = Datastore::in_memory();
	assert_query(&completed, "ALTER DATABASE COMPACT CONCURRENTLY;");
	wait_for_jobs(&completed);
	let mut idle = criterion.benchmark_group("gql_stage20/idle_ledger");
	idle.warm_up_time(Duration::from_secs(1));
	idle.measurement_time(Duration::from_secs(3));
	idle.sample_size(30);
	idle.bench_function("empty", |bench| {
		bench.iter(|| black_box(assert_query(&clean, "RETURN 1;")));
	});
	idle.bench_function("one_terminal_job", |bench| {
		bench.iter(|| black_box(assert_query(&completed, "RETURN 1;")));
	});
	idle.finish();

	let foreground = local_maintenance_fixture();
	let concurrent = local_maintenance_fixture();
	let mut ledger = criterion.benchmark_group("gql_stage20/local_job_ledger");
	ledger.warm_up_time(Duration::from_secs(1));
	ledger.measurement_time(Duration::from_secs(5));
	ledger.sample_size(10);
	ledger.bench_function("foreground", |bench| {
		bench.iter(|| {
			black_box(assert_query(
				&foreground.fixture.store,
				"REBUILD INDEX by_score ON CLASS person WITH (PARALLELISM = 1);",
			));
		});
	});
	ledger.bench_function("concurrent", |bench| {
		bench.iter(|| {
			black_box(assert_query(
				&concurrent.fixture.store,
				"REBUILD INDEX by_score ON CLASS person CONCURRENTLY WITH (PARALLELISM = 1);",
			));
			wait_for_jobs(&concurrent.fixture.store);
		});
	});
	ledger.finish();
}

fn benchmark_stage21(criterion: &mut Criterion) {
	let store = Datastore::in_memory();
	assert_query(
		&store,
		"DEFINE PARAM $answer VALUE 41; DEFINE PURE FUNCTION fn::increment(value: int) -> int { \
		 RETURN $value + 1; };",
	);
	let mut execute_parameter = Object::new();
	execute_parameter.insert("answer", 41_i64);
	let empty_parameters = Object::new();
	let mut parameter = criterion.benchmark_group("gql_stage21/parameter");
	parameter.warm_up_time(Duration::from_secs(3));
	parameter.measurement_time(Duration::from_secs(10));
	parameter.sample_size(100);
	parameter.bench_function("execute", |bench| {
		bench.iter(|| {
			black_box(
				store
					.execute("RETURN $answer + 1;", black_box(&execute_parameter))
					.expect("execute parameter query must run"),
			);
		});
	});
	parameter.bench_function("durable", |bench| {
		bench.iter(|| {
			black_box(
				store
					.execute("RETURN $answer + 1;", &empty_parameters)
					.expect("durable parameter query must run"),
			);
		});
	});
	parameter.finish();

	let mut session = store.session();
	let closure = session
		.execute("LET $increment = |$value| $value + 1;", &Object::new())
		.expect("session closure must be retained");
	assert!(closure.iter().all(|result| result.result.is_ok()));
	let mut function = criterion.benchmark_group("gql_stage21/call");
	function.warm_up_time(Duration::from_secs(3));
	function.measurement_time(Duration::from_secs(10));
	function.sample_size(100);
	function.bench_function("session_closure", |bench| {
		bench.iter(|| {
			black_box(
				session
					.execute("FOR $value IN 0..64 { $increment($value); };", &empty_parameters)
					.expect("closure call must run"),
			);
		});
	});
	function.bench_function("cached_udf", |bench| {
		bench.iter(|| {
			black_box(
				session
					.execute("FOR $value IN 0..64 { fn::increment($value); };", &empty_parameters)
					.expect("UDF call must run"),
			);
		});
	});
	function.finish();

	let sort_store = Datastore::in_memory();
	assert_query(
		&sort_store,
		"DEFINE CLASS item TYPE NODE; DEFINE FIELD text ON CLASS item TYPE string; CREATE item:a \
		 SET text = 'a10'; CREATE item:b SET text = 'a2'; CREATE item:c SET text = 'É'; CREATE \
		 item:d SET text = 'é';",
	);
	let scratch = PathBuf::from("target/gql-stage21/query-temp");
	let mut options = QueryExecutionOptions::default();
	options.barrier_memory_bytes = NonZeroUsize::new(4 * 1024 * 1024).expect("positive budget");
	options.temporary_directory = Some(scratch.clone());
	let mut sort_session = sort_store.session();
	let checked = sort_session
		.execute_with_options(
			"SELECT VALUE text FROM item ORDER BY text COLLATE NUMERIC;",
			&Object::new(),
			options,
		)
		.expect("collated sort must run");
	assert!(checked.iter().all(|result| result.result.is_ok()));
	assert!(
		!scratch.exists()
			|| std::fs::read_dir(&scratch).is_ok_and(|mut entries| entries.next().is_none()),
		"small ICU sort must not retain query-run files"
	);
	let mut collation = criterion.benchmark_group("gql_stage21/collation");
	collation.warm_up_time(Duration::from_secs(3));
	collation.measurement_time(Duration::from_secs(10));
	collation.sample_size(100);
	collation.bench_function("binary", |bench| {
		bench.iter(|| {
			black_box(
				sort_store
					.execute("SELECT VALUE text FROM item ORDER BY text;", &Object::new())
					.expect("binary sort must run"),
			);
		});
	});
	collation.bench_function("icu_numeric", |bench| {
		bench.iter(|| {
			black_box(
				sort_store
					.execute(
						"SELECT VALUE text FROM item ORDER BY text COLLATE NUMERIC;",
						&Object::new(),
					)
					.expect("collated sort must run"),
			);
		});
	});
	collation.finish();
}

criterion_group!(gates, benchmark_stage20, benchmark_stage21);
criterion_main!(gates);
