#![cfg(test)]
#![cfg(windows)]
#![allow(
	clippy::unwrap_used,
	reason = "Public backend contract test fails on the first unexpected error"
)]
/// Assertions shared with the root unit-test harness.
#[path = "common/assertions.rs"]
pub mod test_assertions;

use std::{num::NonZeroUsize, path::PathBuf};

use ferros_grynn::{
	datastore::{CheckpointMode, Datastore, LocalOptions},
	gql::{QueryCancellation, QueryExecutionOptions},
	values::{Geometry, Object, RecordId, Uuid, Value},
};

/// Removes the unique integration database after handles have been dropped.
struct Directory(
	/// Removes the unique integration database after handles have been dropped.
	PathBuf,
);

impl Drop for Directory {
	/// Runs cleanup for this `Directory` before its fields are released.
	fn drop(&mut self) {
		drop(std::fs::remove_dir_all(&self.0));
	}
}

/// Verifies geometry sync effective delta private writes and recovery.
#[test]
fn geometry_sync_effective_delta_private_writes_and_recovery() {
	let directory = Directory(
		std::env::temp_dir()
			.join(format!("fgdb-{:016x}", Uuid::now_v7().as_inner().as_u128() as u64)),
	);
	let mut options = LocalOptions::default();
	options.buffer_pool_pages = 16;
	let store = Datastore::create_local(&directory.0, options).unwrap();
	let setup = store
		.execute(
			"DEFINE GRAPH places; DEFINE CLASS place TYPE NODE SCHEMAFULL;
		 DEFINE FIELD shape ON CLASS place TYPE geometry;
		 CREATE place:a SET shape = <geometry>{type:'Point', coordinates:[8,8]};
		 CREATE place:b SET shape = <geometry>{type:'Point', coordinates:[2,2]};
		 ATTACH place:a TO GRAPH places; ATTACH place:b TO GRAPH places;
		 DEFINE INDEX base_shape ON CLASS place FIELDS shape GEOMETRY TYPE Point SCOPE BASE;
		 DEFINE INDEX graph_shape ON CLASS place FIELDS shape GEOMETRY TYPE Point SCOPE EFFECTIVE IN \
			 GRAPH places;
		 UPDATE place:a IN GRAPH places SET shape = <geometry>{type:'Point', coordinates:[1,1]};
		 ANALYZE CLASS place BASE; ANALYZE CLASS place EFFECTIVE IN GRAPH places;",
			&Object::new(),
		)
		.unwrap();
	for result in setup {
		assert!(result.result.is_ok(), "{:?}", result.result);
	}
	let parameters =
		Object::from_iter([
			(
				"area",
				Value::Geometry(
					r#"{"type":"Polygon","coordinates":[[[0,0],[5,0],[5,5],[0,5],[0,0]]]}"#
						.parse::<Geometry>()
						.unwrap(),
				),
			),
		]);
	let indexed = "SELECT VALUE id FROM place IN GRAPH @places WITH INDEX graph_shape WHERE shape \
	               INSIDE $area ORDER BY id;";
	let scan = indexed.replace("WITH INDEX graph_shape", "WITH NOINDEX");
	let value = |store: &Datastore, text: &str| {
		store.execute(text, &parameters).unwrap().remove(0).result.unwrap().into_parts().0
	};
	let both = Value::Array(
		["place:a", "place:b"]
			.map(|record| Value::from(record.parse::<RecordId>().unwrap()))
			.to_vec()
			.into(),
	);
	assert_eq!(value(&store, indexed), both);
	assert_eq!(value(&store, indexed), value(&store, &scan));
	let mut old = store.begin();
	let update =
		store.execute("UPDATE place:a IN GRAPH places RESET shape;", &Object::new()).unwrap();
	assert!(update[0].result.is_ok(), "{:?}", update[0].result);
	let current = value(&store, &scan);
	assert_ne!(current, both);
	assert_eq!(value(&store, indexed), current);
	assert_eq!(
		old.execute(indexed, &parameters).unwrap().remove(0).result.unwrap().into_parts().0,
		both
	);
	drop(old);
	let incompatible = indexed.replace("WITH INDEX graph_shape", "WITH INDEX base_shape");
	assert!(store.execute(&incompatible, &parameters).unwrap()[0].result.is_err());
	let mut private = store.begin();
	private
		.execute(
			"UPDATE place:a IN GRAPH places SET shape = <geometry>{type:'Point', \
			 coordinates:[1,1]};",
			&Object::new(),
		)
		.unwrap()
		.remove(0)
		.result
		.unwrap();
	let automatic = scan.replace(" WITH NOINDEX", "");
	assert_eq!(
		private.execute(&automatic, &parameters).unwrap().remove(0).result.unwrap().into_parts().0,
		both
	);
	drop(private);
	assert_eq!(value(&store, indexed), current);
	let mut limited = QueryExecutionOptions::default();
	limited.barrier_memory_bytes = NonZeroUsize::new(1024).unwrap();
	assert!(store.execute_with_options(indexed, &parameters, limited).unwrap()[0].result.is_err());
	let cancellation = QueryCancellation::default();
	cancellation.cancel();
	let mut cancelled = QueryExecutionOptions::default();
	cancelled.cancellation = Some(cancellation);
	assert!(
		store.execute_with_options(indexed, &parameters, cancelled).unwrap()[0].result.is_err()
	);
	assert!(store.checkpoint(CheckpointMode::Restart).unwrap());
	drop(store);
	let reopened = Datastore::open_local(&directory.0, options).unwrap();
	assert_eq!(value(&reopened, indexed), current);
	assert_eq!(value(&reopened, &scan), current);
}
