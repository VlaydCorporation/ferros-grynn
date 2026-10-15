//! Stage-22/23 Fulltext and vector-index end-to-end performance gates.

use std::{
	collections::BTreeMap,
	fs,
	hint::black_box,
	path::PathBuf,
	sync::atomic::{AtomicU64, Ordering},
	time::{Duration, Instant},
};

use criterion::{Criterion, criterion_group, criterion_main};
use ferros_grynn::{
	datastore::{Datastore, ImportOptions, LocalOptions},
	values::{Object, PropertyMap, RecordId, Value, Vector},
};

/// Default record population for text and vector search measurements.
const DEFAULT_ROWS: usize = 10_000;
/// Default number of scalar components in each benchmark vector.
const DEFAULT_DIMENSION: usize = 32;
/// Number of nearest neighbours requested and used to calculate recall.
const K: usize = 10;
/// Number of query vectors used by the recall comparison.
const RECALL_QUERIES: usize = 20;
/// Process-local suffix counter that separates temporary benchmark directories.
static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// Seeded local datastore and query parameters used by search benchmarks.
struct SearchFixture {
	/// Datastore retained by the fixture or operation.
	store: Datastore,
	/// Directory retaining the local files used by the scenario.
	directory: PathBuf,
	/// Declared parameters or bound values supplied to this operation.
	parameters: Object,
	/// Configured row count of the fixture or measurement.
	rows: usize,
	/// Number of scalar coordinates in each indexed vector.
	dimension: usize,
	/// Page count used when configuring the benchmark buffer pool.
	buffer_pool_pages: usize,
}

impl SearchFixture {
	/// Constructs `SearchFixture` from the supplied initial state.
	fn new() -> Self {
		let rows = environment_usize("FG_STAGE_GATE_ROWS", DEFAULT_ROWS).max(K);
		let dimension = environment_usize("FG_STAGE_GATE_DIMENSION", DEFAULT_DIMENSION).max(1);
		let sequence = TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
		let directory =
			std::env::temp_dir().join(format!("fgs23-{:x}-{sequence:x}", std::process::id()));
		let mut options = LocalOptions::default();
		options.buffer_pool_pages =
			environment_usize("FG_STAGE_GATE_BUFFER_POOL_PAGES", options.buffer_pool_pages).max(1);
		let buffer_pool_pages = options.buffer_pool_pages;
		let store = Datastore::create_local(&directory, options).expect("search fixture must open");
		assert_query(
			&store,
			"DEFINE ANALYZER words TOKENIZERS blank FILTERS lowercase; DEFINE CLASS document TYPE \
			 NODE SCHEMAFULL; DEFINE FIELD title ON CLASS document TYPE string; DEFINE FIELD \
			 embedding ON CLASS document TYPE vector<f32>;",
			&Object::new(),
		);
		let dictionary = store.property_dictionary();
		let title = dictionary.get("title").expect("title property must exist");
		let embedding = dictionary.get("embedding").expect("embedding property must exist");
		let imported_at = Instant::now();
		let mut import_options = ImportOptions::default();
		import_options.segment_size = rows.min(10_000);
		store
			.import(import_options, |session| {
				for ordinal in 0..rows {
					let record = format!("document:r{ordinal}")
						.parse::<RecordId>()
						.expect("generated RecordId must parse");
					let mut properties = PropertyMap::new();
					properties.insert(
						title,
						Value::from(if ordinal % 100 == 0 {
							format!("target document {ordinal}")
						} else {
							format!("ordinary document {}", ordinal % 1_024)
						}),
					);
					properties.insert(
						embedding,
						Value::Vector(Vector::F32(deterministic_vector(ordinal, dimension))),
					);
					session
						.apply_item(|transaction| transaction.create_atom(record, properties))?;
				}
				Ok(())
			})
			.expect("search fixture import must succeed");
		let import_elapsed = imported_at.elapsed();

		let fulltext_at = Instant::now();
		assert_query(
			&store,
			"DEFINE INDEX title_search ON CLASS document FIELDS title FULLTEXT ANALYZER words \
			 BM25(1.2, 0.75) HIGHLIGHTS SCOPE BASE;",
			&Object::new(),
		);
		let fulltext_elapsed = fulltext_at.elapsed();
		let mut parameters = Object::new();
		parameters.insert("query", Value::Vector(Vector::F32(deterministic_vector(7, dimension))));

		let hnsw_at = Instant::now();
		assert_query(
			&store,
			&format!(
				"DEFINE INDEX embedding_hnsw ON CLASS document FIELDS embedding HNSW DIMENSION \
				 {dimension} TYPE F32 DIST EUCLIDEAN EFC 150 M 12 SCOPE BASE;"
			),
			&Object::new(),
		);
		let hnsw_elapsed = hnsw_at.elapsed();
		let hnsw_recall_after_hnsw =
			measure_index_recall(&store, "embedding_hnsw", rows, dimension);

		let diskann_at = Instant::now();
		assert_query(
			&store,
			&format!(
				"DEFINE INDEX embedding_disk ON CLASS document FIELDS embedding DISKANN DIMENSION \
				 {dimension} TYPE F32 DIST EUCLIDEAN DEGREE 64 L_BUILD 100 ALPHA 1.2 SCOPE BASE;"
			),
			&Object::new(),
		);
		let diskann_elapsed = diskann_at.elapsed();
		let hnsw_recall_after_diskann =
			measure_index_recall(&store, "embedding_hnsw", rows, dimension);

		let hashed_at = Instant::now();
		assert_query(
			&store,
			&format!(
				"DEFINE INDEX embedding_disk_hashed ON CLASS document FIELDS embedding DISKANN \
				 DIMENSION {dimension} TYPE F32 DIST EUCLIDEAN DEGREE 64 L_BUILD 100 ALPHA 1.2 \
				 HASHED_VECTOR SCOPE BASE;"
			),
			&Object::new(),
		);
		let hashed_elapsed = hashed_at.elapsed();
		let hnsw_recall_after_hashed_diskann =
			measure_index_recall(&store, "embedding_hnsw", rows, dimension);
		let fixture = Self {
			store,
			directory,
			parameters,
			rows,
			dimension,
			buffer_pool_pages,
		};
		fixture.verify_and_record(
			import_elapsed,
			fulltext_elapsed,
			hnsw_elapsed,
			diskann_elapsed,
			hashed_elapsed,
			hnsw_recall_after_hnsw,
			hnsw_recall_after_diskann,
			hnsw_recall_after_hashed_diskann,
		);
		fixture
	}

	/// Returns verify and record for `SearchFixture`.
	fn verify_and_record(
		&self,
		import_elapsed: Duration,
		fulltext_elapsed: Duration,
		hnsw_elapsed: Duration,
		diskann_elapsed: Duration,
		hashed_elapsed: Duration,
		hnsw_recall_after_hnsw: f64,
		hnsw_recall_after_diskann: f64,
		hnsw_recall_after_hashed_diskann: f64,
	) {
		let fulltext_scan = self.query(
			"SELECT VALUE id FROM document WITH NOINDEX WHERE title @@ 'target' ORDER BY id;",
		);
		let fulltext_index = self.query(
			"SELECT VALUE id FROM document WITH INDEX title_search WHERE title @@ 'target' ORDER \
			 BY id;",
		);
		assert_eq!(fulltext_index, fulltext_scan, "Fulltext index must match its scan oracle");

		let diskann = self.query(
			"SELECT VALUE id FROM document WITH INDEX embedding_disk WHERE embedding <| 10, \
			 EUCLIDEAN |> $query;",
		);
		let hashed = self.query(
			"SELECT VALUE id FROM document WITH INDEX embedding_disk_hashed WHERE embedding <| \
			 10, EUCLIDEAN |> $query;",
		);
		let hnsw_recall = hnsw_recall_after_hashed_diskann;
		let diskann_recall =
			measure_index_recall(&self.store, "embedding_disk", self.rows, self.dimension);
		write_metrics(artifact_path("gql-stage22"), &[
			("rows", self.rows as f64),
			("import_ms", millis(import_elapsed)),
			("fulltext_build_ms", millis(fulltext_elapsed)),
		]);
		write_metrics(artifact_path("gql-stage23"), &[
			("rows", self.rows as f64),
			("dimension", self.dimension as f64),
			("buffer_pool_pages", self.buffer_pool_pages as f64),
			("hnsw_build_ms", millis(hnsw_elapsed)),
			("diskann_build_ms", millis(diskann_elapsed)),
			("hashed_diskann_build_ms", millis(hashed_elapsed)),
			("hnsw_recall_after_hnsw", hnsw_recall_after_hnsw),
			("hnsw_recall_after_diskann", hnsw_recall_after_diskann),
			("hnsw_recall_after_hashed_diskann", hnsw_recall_after_hashed_diskann),
			("hnsw_recall_at_10", hnsw_recall),
			("diskann_recall_at_10", diskann_recall),
		]);

		assert!(hnsw_recall >= 0.95, "HNSW recall@{K} was {hnsw_recall}");
		assert!(diskann_recall >= 0.95, "DiskANN recall@{K} was {diskann_recall}");
		assert_eq!(hashed, diskann, "HASHED_VECTOR must not change the result");
	}

	/// Returns query for `SearchFixture`.
	fn query(&self, query: &str) -> Value {
		assert_query(&self.store, query, &self.parameters)
	}
}

/// Measures index recall for this module.
fn measure_index_recall(store: &Datastore, index: &str, rows: usize, dimension: usize) -> f64 {
	let query = format!(
		"SELECT VALUE embedding FROM document WITH INDEX {index} WHERE embedding <| 10, EUCLIDEAN \
		 |> $query;"
	);
	let mut total = 0.0;
	for sample in 0..RECALL_QUERIES {
		let ordinal = (7 + sample * 7_919) % rows;
		let mut parameters = Object::new();
		parameters
			.insert("query", Value::Vector(Vector::F32(deterministic_vector(ordinal, dimension))));
		let exact = assert_query(
			store,
			"SELECT VALUE embedding FROM document WITH NOINDEX WHERE embedding <| 10, EUCLIDEAN \
			 |> $query;",
			&parameters,
		);
		let approximate = assert_query(store, &query, &parameters);
		total += recall_at_k(&exact, &approximate, K);
	}
	total / RECALL_QUERIES as f64
}

impl Drop for SearchFixture {
	/// Runs cleanup for this `SearchFixture` before its fields are released.
	fn drop(&mut self) {
		drop(fs::remove_dir_all(&self.directory));
	}
}

/// Returns environment usize for this module.
fn environment_usize(name: &str, default: usize) -> usize {
	std::env::var(name).ok().and_then(|value| value.parse().ok()).unwrap_or(default)
}

/// Returns artifact path for this module.
fn artifact_path(stage: &str) -> PathBuf {
	PathBuf::from(env!("CARGO_MANIFEST_DIR"))
		.parent()
		.expect("benchmark crate must be inside the workspace")
		.join("target")
		.join(stage)
		.join("setup.csv")
}

/// Returns deterministic vector for this module.
fn deterministic_vector(ordinal: usize, dimension: usize) -> Vec<f32> {
	let source = if ordinal.is_multiple_of(5) {
		ordinal % 100
	} else {
		ordinal
	};
	(0..dimension)
		.map(|axis| {
			let mut value = (source as u64)
				.wrapping_add(0x9e37_79b9_7f4a_7c15_u64.wrapping_mul(axis as u64 + 1));
			value ^= value >> 30;
			value = value.wrapping_mul(0xbf58_476d_1ce4_e5b9);
			value ^= value >> 27;
			value = value.wrapping_mul(0x94d0_49bb_1331_11eb);
			value ^= value >> 31;
			(value as u32) as f32 / u32::MAX as f32
		})
		.collect()
}

/// Returns assert query for this module.
fn assert_query(store: &Datastore, query: &str, parameters: &Object) -> Value {
	let results = store.execute(query, parameters).expect("gate query must execute");
	if let Some(error) = results.iter().find_map(|result| result.result.as_ref().err()) {
		panic!("gate query failed: {error}");
	}
	results
		.last()
		.and_then(|result| result.result.as_ref().ok())
		.map(|outcome| outcome.value().clone())
		.unwrap_or(Value::None)
}

/// Returns recall at k for this module.
fn recall_at_k(exact: &Value, approximate: &Value, k: usize) -> f64 {
	let exact = exact.as_array().expect("exact KNN must return an array");
	let approximate = approximate.as_array().expect("ANN must return an array");
	let mut expected = BTreeMap::<Value, usize>::new();
	let mut actual = BTreeMap::<Value, usize>::new();
	for value in exact.iter().take(k) {
		*expected.entry(value.clone()).or_default() += 1;
	}
	for value in approximate.iter().take(k) {
		*actual.entry(value.clone()).or_default() += 1;
	}
	let matched = expected
		.iter()
		.map(|(value, count)| (*count).min(actual.get(value).copied().unwrap_or(0)))
		.sum::<usize>();
	matched as f64 / exact.len().min(k).max(1) as f64
}

/// Returns millis for this module.
fn millis(duration: Duration) -> f64 {
	duration.as_secs_f64() * 1_000.0
}

/// Writes metrics for this module.
fn write_metrics(path: PathBuf, metrics: &[(&str, f64)]) {
	if let Some(parent) = path.parent() {
		fs::create_dir_all(parent).expect("stage artifact directory must be created");
	}
	let mut output = String::from("metric,value\n");
	for (name, value) in metrics {
		output.push_str(&format!("{name},{value}\n"));
	}
	fs::write(path, output).expect("stage setup metrics must be written");
}

/// Measures search with the benchmark harness.
fn benchmark_search(criterion: &mut Criterion) {
	let fixture = SearchFixture::new();

	let mut fulltext = criterion.benchmark_group("gql_stage22/fulltext_query");
	fulltext.warm_up_time(Duration::from_secs(1));
	fulltext.measurement_time(Duration::from_secs(5));
	fulltext.sample_size(20);
	for (name, query) in [
		(
			"exact_scan",
			"SELECT VALUE id FROM document WITH NOINDEX WHERE title @@ 'target' ORDER BY id;",
		),
		(
			"fulltext_index",
			"SELECT VALUE id FROM document WITH INDEX title_search WHERE title @@ 'target' ORDER \
			 BY id;",
		),
	] {
		fulltext.bench_function(name, |bench| {
			bench.iter(|| black_box(fixture.query(black_box(query))));
		});
	}
	fulltext.finish();

	let mut vector = criterion.benchmark_group("gql_stage23/knn_query");
	vector.warm_up_time(Duration::from_secs(1));
	vector.measurement_time(Duration::from_secs(5));
	vector.sample_size(20);
	for (name, query) in [
		(
			"exact_scan",
			"SELECT VALUE id FROM document WITH NOINDEX WHERE embedding <| 10, EUCLIDEAN |> \
			 $query;",
		),
		(
			"hnsw",
			"SELECT VALUE id FROM document WITH INDEX embedding_hnsw WHERE embedding <| 10, \
			 EUCLIDEAN |> $query;",
		),
		(
			"diskann",
			"SELECT VALUE id FROM document WITH INDEX embedding_disk WHERE embedding <| 10, \
			 EUCLIDEAN |> $query;",
		),
		(
			"diskann_hashed",
			"SELECT VALUE id FROM document WITH INDEX embedding_disk_hashed WHERE embedding <| \
			 10, EUCLIDEAN |> $query;",
		),
	] {
		vector.bench_function(name, |bench| {
			bench.iter(|| black_box(fixture.query(black_box(query))));
		});
	}
	vector.finish();
}

criterion_group!(gates, benchmark_search);
criterion_main!(gates);
