#![cfg(test)]
#![allow(
	clippy::unwrap_used,
	reason = "Ignored paired probe stops immediately on invalid fixture or output"
)]
/// Assertions shared with the root unit-test harness.
#[path = "common/assertions.rs"]
pub mod test_assertions;

use std::{
	hint::black_box,
	path::{Path, PathBuf},
	time::{Duration, Instant},
};

use ferros_grynn::{
	datastore::{ClassField, ClassSchema, Datastore, ImportOptions, InMemoryOptions, LocalOptions},
	graph::AtomKind,
	values::{Geometry, Object, Position, PropertyMap, RecordId, RecordIdPart, Type, Uuid, Value},
};

/// Removes the isolated probe database after native handles have been dropped.
struct Directory(
	/// Removes the isolated probe database after native handles have been
	/// dropped.
	PathBuf,
);

impl Drop for Directory {
	/// Runs cleanup for this `Directory` before its fields are released.
	fn drop(&mut self) {
		drop(std::fs::remove_dir_all(&self.0));
	}
}

/// Runs public queries and DML on identical populations before/after adding the
/// index. Cold local observations reopen the database outside the query timer;
/// RAM is warm only.
#[test]
#[ignore = "stage-24 paired runtime/resource/performance matrix"]
fn geometry_stage_24_paired_probe() {
	let sizes = std::env::var("FG_GEOMETRY_ROWS").unwrap_or_else(|_| "10000,100000,1000000".into());
	let backend = std::env::var("FG_GEOMETRY_BACKEND").unwrap_or_else(|_| "ram".into());
	let samples = std::env::var("FG_GEOMETRY_SAMPLES")
		.ok()
		.map(|value| value.parse::<usize>().unwrap())
		.unwrap_or(7)
		.max(5);
	let point_samples = std::env::var("FG_GEOMETRY_POINT_SAMPLES")
		.ok()
		.map(|value| value.parse::<usize>().unwrap())
		.unwrap_or(samples);
	for rows in sizes.split(',').map(|value| value.parse::<usize>().unwrap()) {
		let directory = Directory(
			std::env::temp_dir()
				.join(format!("fgdb-{:016x}", Uuid::now_v7().as_inner().as_u128() as u64)),
		);
		let mut local = LocalOptions::default();
		local.buffer_pool_pages = 256;
		local.max_snapshot_age = Duration::from_secs(3600);
		local.max_write_set_bytes = 1024 * 1024 * 1024;
		#[cfg(feature = "native-async-io")]
		if backend == "native" {
			local.io_mode = ferros_grynn::datastore::IoMode::NativeAsync;
		}
		let store = if backend == "ram" {
			let mut options = InMemoryOptions::default();
			options.max_snapshot_age = local.max_snapshot_age;
			options.max_write_set_bytes = local.max_write_set_bytes;
			Datastore::in_memory_with(options)
		} else {
			assert!(backend == "sync" || cfg!(feature = "native-async-io") && backend == "native");
			Datastore::create_local(&directory.0, local).unwrap()
		};
		let mut define = store.begin();
		define
			.define_class(
				ClassSchema::new("place", AtomKind::Node, [
					ClassField::new("shape", Type::Geometry),
					ClassField::new("tag", Type::Int),
				])
				.unwrap(),
			)
			.unwrap();
		define.commit().unwrap();
		let dictionary = store.property_dictionary();
		let shape = dictionary.get("shape").unwrap();
		let tag = dictionary.get("tag").unwrap();
		let started = Instant::now();
		store
			.import(ImportOptions::default(), |session| {
				for ordinal in 0..rows {
					session.apply_item(|transaction| {
						transaction.create_atom(
							RecordId::new("place", RecordIdPart::Int(ordinal as i64))?,
							PropertyMap::from_iter([
								(
									shape,
									Value::Geometry(Geometry::Point(Position::new([
										-170.0 + (ordinal % 1000) as f64 * 0.34,
										-80.0 + (ordinal / 1000) as f64 * 0.16,
									])?)),
								),
								(tag, Value::from(0_i64)),
							]),
						)
					})?;
				}
				Ok(())
			})
			.unwrap();
		println!(
			"geometry_probe backend={backend} rows={rows} phase=import ns={}",
			started.elapsed().as_nanos()
		);
		let unrelated = "SELECT VALUE tag FROM place:0;";
		let baseline_query = measure(&store, unrelated, point_samples);
		let baseline_dml = measure_dml(&store, tag, samples);
		let started = Instant::now();
		execute(&store, "DEFINE INDEX by_shape ON CLASS place FIELDS shape GEOMETRY SCOPE BASE;");
		let build = started.elapsed().as_nanos();
		let indexed_query = measure(&store, unrelated, point_samples);
		let indexed_dml = measure_dml(&store, tag, samples);
		if std::env::var_os("FG_GEOMETRY_NONSPATIAL_ONLY").is_some() {
			println!(
				"geometry_probe backend={backend} rows={rows} phase=nonspatial \
				 query_samples={point_samples} unrelated_before_ns={baseline_query} \
				 unrelated_after_ns={indexed_query} dml_before_ns={baseline_dml} \
				 dml_after_ns={indexed_dml}"
			);
			continue;
		}
		let indexed = "SELECT VALUE id FROM place WITH INDEX by_shape WHERE shape INSIDE \
		               <geometry>{type:'Polygon',coordinates:[[[-170.1,-80.1],[-169.9,-80.1],\
		               [-169.9,-79.9],[-170.1,-79.9],[-170.1,-80.1]]]} ORDER BY id;";
		let scan = indexed.replace("WITH INDEX by_shape", "WITH NOINDEX");
		let expected = execute(&store, &scan);
		assert_eq!(expected.as_array().unwrap().len(), 1);
		let mut latency = [Vec::new(), Vec::new()];
		for round in 0..samples {
			for slot in [(round % 2), 1 - (round % 2)] {
				let started = Instant::now();
				let actual = execute(
					&store,
					if slot == 0 {
						&scan
					} else {
						indexed
					},
				);
				latency[slot].push(started.elapsed().as_nanos());
				assert_eq!(actual, expected);
				black_box(actual);
			}
		}
		println!(
			"geometry_probe backend={backend} rows={rows} phase=warm samples={samples} scan_ns={} \
			 index_ns={} build_ns={build} unrelated_before_ns={baseline_query} \
			 unrelated_after_ns={indexed_query} dml_before_ns={baseline_dml} \
			 dml_after_ns={indexed_dml}",
			median(&mut latency[0]),
			median(&mut latency[1])
		);
		println!(
			"geometry_probe backend={backend} rows={rows} phase=metrics {:?}",
			execute(&store, &format!("EXPLAIN ANALYZE FORMAT JSON {indexed}"))
		);
		let wal_before = wal_bytes(&directory.0);
		let started = Instant::now();
		let mut mutate = store.begin();
		mutate
			.set_base_property(
				&RecordId::new("place", RecordIdPart::Int(0)).unwrap(),
				shape,
				Value::Geometry(Geometry::Point(Position::new([-169.0, -79.0]).unwrap())),
			)
			.unwrap();
		mutate.commit().unwrap();
		println!(
			"geometry_probe backend={backend} rows={rows} phase=mutation ns={} wal_bytes={}",
			started.elapsed().as_nanos(),
			wal_bytes(&directory.0).saturating_sub(wal_before)
		);
		assert_eq!(execute(&store, indexed), execute(&store, &scan));
		let started = Instant::now();
		execute(&store, "REBUILD INDEX by_shape ON CLASS place;");
		println!(
			"geometry_probe backend={backend} rows={rows} phase=rebuild ns={}",
			started.elapsed().as_nanos()
		);
		assert_eq!(execute(&store, indexed), execute(&store, &scan));
		drop(store);
		if backend != "ram" {
			for query in [&scan, indexed] {
				let reopened = Datastore::open_local(&directory.0, local).unwrap();
				let started = Instant::now();
				let actual = execute(&reopened, query);
				println!(
					"geometry_probe backend={backend} rows={rows} phase=cold indexed={} ns={} \
					 result={actual:?}",
					query == indexed,
					started.elapsed().as_nanos()
				);
			}
		}
	}
}

/// Runs one public statement and retains its exact output as the oracle.
fn execute(store: &Datastore, query: &str) -> Value {
	store.execute(query, &Object::new()).unwrap().remove(0).result.unwrap().into_parts().0
}

/// Warms a nonspatial point query before retaining repeated latencies.
fn measure(store: &Datastore, query: &str, samples: usize) -> u128 {
	for _ in 0..3 {
		black_box(execute(store, query));
	}
	median(
		&mut (0..samples)
			.map(|_| {
				let started = Instant::now();
				black_box(execute(store, query));
				started.elapsed().as_nanos()
			})
			.collect::<Vec<_>>(),
	)
}

/// Changes an unrelated property through the public transaction/commit path.
fn measure_dml(
	store: &Datastore,
	key: ferros_grynn::values::PropertyKeyId,
	samples: usize,
) -> u128 {
	let record = RecordId::new("place", RecordIdPart::Int(0)).unwrap();
	let mut observations = Vec::new();
	for ordinal in 0..samples + 3 {
		let started = Instant::now();
		let mut transaction = store.begin();
		transaction.set_base_property(&record, key, Value::from(ordinal as i64)).unwrap();
		transaction.commit().unwrap();
		if ordinal >= 3 {
			observations.push(started.elapsed().as_nanos());
		}
	}
	median(&mut observations)
}

/// Selects the central observed latency without averaging tail outliers.
fn median(values: &mut [u128]) -> u128 {
	values.sort_unstable();
	values[values.len() / 2]
}

/// Observes durable WAL segment bytes without including COW base files.
fn wal_bytes(root: &Path) -> u64 {
	if !root.exists() {
		return 0;
	}
	std::fs::read_dir(root.join("wal"))
		.unwrap()
		.map(|entry| {
			let entry = entry.unwrap();
			if entry.path().extension().is_some_and(|extension| extension == "wal") {
				entry.metadata().unwrap().len()
			} else {
				0
			}
		})
		.sum()
}
