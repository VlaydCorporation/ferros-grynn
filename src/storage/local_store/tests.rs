use std::{
	fs,
	num::NonZeroUsize,
	path::{Path, PathBuf},
	sync::atomic::{AtomicU64, Ordering},
};

use super::{IMPORT_FAILURE_POINT, ImportFailurePoint};
use crate::{
	datastore::{CheckpointMode, ClassSchema, Datastore, ImportOptions, LocalOptions},
	gql::QueryExecutionOptions,
	graph::AtomKind,
	values::{Object, PropertyMap, RecordId, Value},
};

static NEXT_DIRECTORY: AtomicU64 = AtomicU64::new(0);

struct TestDirectory(PathBuf);

impl TestDirectory {
	fn new() -> Self {
		Self(
			std::env::temp_dir()
				.join(format!("fgdb-{:016x}", NEXT_DIRECTORY.fetch_add(1, Ordering::Relaxed))),
		)
	}
}

impl Drop for TestDirectory {
	fn drop(&mut self) {
		drop(fs::remove_dir_all(&self.0));
	}
}

#[test]
fn every_prepublication_failure_discards_root_and_unreachable_generation() {
	let directory = TestDirectory::new();
	let store = Datastore::create_local(&directory.0, LocalOptions::default()).unwrap();
	let mut schema = store.begin();
	schema.define_class(ClassSchema::new("person", AtomKind::Node, []).unwrap()).unwrap();
	schema.commit().unwrap();
	let generation_count = generation_directories(&directory.0);

	for point in [
		ImportFailurePoint::AfterBuild,
		ImportFailurePoint::AfterFileSync,
		ImportFailurePoint::AfterCatalogStage,
		ImportFailurePoint::BeforeWalCommit,
	] {
		IMPORT_FAILURE_POINT.set(point);
		let result = store.import(ImportOptions::default(), |session| {
			session.apply_item(|transaction| {
				transaction.create_atom("person:alice".parse()?, PropertyMap::new())
			})?;
			Ok(())
		});
		IMPORT_FAILURE_POINT.set(ImportFailurePoint::None);

		assert!(result.is_err());
		assert!(
			store.begin().atom(&"person:alice".parse::<RecordId>().unwrap()).unwrap().is_none()
		);
		assert_eq!(generation_directories(&directory.0), generation_count);
	}
}

#[test]
fn graph_property_catalog_update_reuses_graph_generation_and_reopens() {
	let directory = TestDirectory::new();
	let options = LocalOptions::default();
	let store = Datastore::create_local(&directory.0, options).unwrap();
	let define = store
		.execute("DEFINE GRAPH sales TYPE DAG CONTENT {owner: 'analytics'};", &Object::new())
		.unwrap();
	assert!(define[0].result.is_ok());
	let before = generation_directories(&directory.0);

	let update = store
		.execute(
			"UPDATE GRAPH sales SET properties.owner = 'risk', properties.address.city = 'Berlin';",
			&Object::new(),
		)
		.unwrap();
	assert!(update[0].result.is_ok());
	assert_eq!(generation_directories(&directory.0), before);
	assert!(store.checkpoint(CheckpointMode::Restart).unwrap());
	drop(store);

	let reopened = Datastore::open_local(&directory.0, options).unwrap();
	let schema = reopened.schema_catalog().named_graph("sales").unwrap();
	let dictionary = reopened.property_dictionary();
	let owner = dictionary.get("owner").unwrap();
	let address = dictionary.get("address").unwrap();
	assert_eq!(schema.properties().get(owner), Some(&Value::from("risk")));
	let Some(Value::Object(address)) = schema.properties().get(address) else {
		panic!("nested graph property must reopen as an object");
	};
	assert_eq!(address.get("city"), Some(&Value::from("Berlin")));
	let selected = reopened
		.execute("SELECT ONLY VALUE properties.owner FROM GRAPH @sales;", &Object::new())
		.unwrap();
	assert_eq!(selected[0].result.as_ref().unwrap().value(), &Value::from("risk"));
}

#[test]
fn relational_select_spill_matches_after_local_reopen() {
	let directory = TestDirectory::new();
	let options = LocalOptions::default();
	let store = Datastore::create_local(&directory.0, options).unwrap();
	let setup = store
		.execute(
			"DEFINE CLASS department TYPE NODE SCHEMALESS; DEFINE CLASS person TYPE NODE \
			 SCHEMALESS; CREATE department:eng SET code = 'eng', name = 'Engineering'; CREATE \
			 department:risk SET code = 'risk', name = 'Risk'; CREATE person:alice SET department \
			 = 'eng', score = 3; CREATE person:bob SET department = 'risk', score = 2; CREATE \
			 person:carol SET department = 'eng', score = 1;",
			&Object::new(),
		)
		.unwrap();
	assert!(setup.iter().all(|result| result.result.is_ok()));

	let query = "SELECT d.properties.name AS department, count() AS people, \
	             sum(p.properties.score) AS total FROM person AS p LEFT JOIN department AS d ON \
	             p.properties.department = d.properties.code GROUP BY d.properties.name ORDER BY \
	             total DESC TEMPFILES;";
	let query_options = QueryExecutionOptions {
		barrier_memory_bytes: NonZeroUsize::new(64).unwrap_or(NonZeroUsize::MIN),
		..QueryExecutionOptions::default()
	};
	let before = store
		.execute_with_options(query, &Object::new(), query_options.clone())
		.unwrap()
		.remove(0)
		.result
		.unwrap()
		.into_parts()
		.0;
	assert!(store.checkpoint(CheckpointMode::Restart).unwrap());
	drop(store);

	let reopened = Datastore::open_local(&directory.0, options).unwrap();
	let after = reopened
		.execute_with_options(query, &Object::new(), query_options)
		.unwrap()
		.remove(0)
		.result
		.unwrap()
		.into_parts()
		.0;
	assert_eq!(after, before);
}

fn generation_directories(root: &Path) -> usize {
	let mut count = 0;
	for family in ["atoms", "graphs"] {
		let family = root.join(family);
		let Ok(entries) = fs::read_dir(&family) else {
			continue;
		};
		for entry in entries.flatten() {
			let path = entry.path();
			if !path.is_dir() {
				continue;
			}
			if family.ends_with("atoms") {
				count += 1;
			} else if let Ok(generations) = fs::read_dir(path) {
				count += generations.flatten().filter(|entry| entry.path().is_dir()).count();
			}
		}
	}
	count
}
