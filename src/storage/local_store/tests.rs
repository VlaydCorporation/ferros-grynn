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
	graph::{AtomKind, EdgeTopology, GraphKind},
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
fn local_traversal_uses_exact_physical_snapshot_and_falls_back_after_mutation() {
	let directory = TestDirectory::new();
	let options = LocalOptions::default();
	let store = Datastore::create_local(&directory.0, options).unwrap();
	let mut seed = store.begin();
	seed.define_class(ClassSchema::new("person", AtomKind::Node, []).unwrap()).unwrap();
	seed.define_class(ClassSchema::new("knows", AtomKind::Edge, []).unwrap()).unwrap();
	let graph = seed.define_graph("physical_routes", GraphKind::Graph).unwrap();
	let alice = "person:alice".parse::<RecordId>().unwrap();
	let bob = "person:bob".parse::<RecordId>().unwrap();
	let carol = "person:carol".parse::<RecordId>().unwrap();
	let alice_bob = "knows:alice-bob".parse::<RecordId>().unwrap();
	let bob_carol = "knows:bob-carol".parse::<RecordId>().unwrap();
	for record in [&alice, &bob, &carol, &alice_bob, &bob_carol] {
		seed.create_atom(record.clone(), PropertyMap::new()).unwrap();
	}
	let alice_id = seed.attach_node(graph.id(), &alice, None, None).unwrap();
	let bob_id = seed.attach_node(graph.id(), &bob, None, None).unwrap();
	let carol_id = seed.attach_node(graph.id(), &carol, None, None).unwrap();
	seed.attach_edge(
		graph.id(),
		&alice_bob,
		None,
		EdgeTopology::directed([alice_id], [bob_id]),
		None,
		None,
	)
	.unwrap();
	seed.commit().unwrap();

	let mut variables = Object::new();
	variables.insert("graph", Value::GraphRef(Box::new(graph.clone())));
	variables.insert("source", Value::Record(Box::new(alice.clone())));
	let traversal = "TRAVERSE FROM $source IN GRAPH $graph |> tv::out() |> tv::values('id');";
	let value =
		store.execute(traversal, &variables).unwrap().remove(0).result.unwrap().into_parts().0;
	assert_eq!(value, Value::Array(vec![Value::Record(Box::new(bob.clone()))].into()));

	let explained = store
		.execute(
			"EXPLAIN ANALYZE FORMAT JSON TRAVERSE FROM $source IN GRAPH $graph |> tv::out();",
			&variables,
		)
		.unwrap()
		.remove(0)
		.result
		.unwrap()
		.into_parts()
		.0;
	let Value::Object(explained) = explained else {
		panic!("FORMAT JSON must return an object");
	};
	let Some(Value::Object(metrics)) = explained.get("analyze") else {
		panic!("ANALYZE metrics are missing");
	};
	assert_eq!(metrics.get("physical_windows"), Some(&Value::from(1_u64)));
	let physical_pages = [
		"physical_resident_pages",
		"physical_wal_pages",
		"physical_sync_pages",
		"physical_ioring_pages",
	]
	.into_iter()
	.map(|field| {
		metrics.get(field).and_then(Value::as_number).and_then(|value| value.as_i64()).unwrap_or(0)
	})
	.sum::<i64>();
	assert!(physical_pages > 0, "the physical route must read or probe exact pages");

	let mut transaction = store.begin();
	transaction
		.attach_edge(
			graph.id(),
			&bob_carol,
			None,
			EdgeTopology::directed([bob_id], [carol_id]),
			None,
			None,
		)
		.unwrap();
	let explained = transaction
		.execute(
			"EXPLAIN ANALYZE FORMAT JSON TRAVERSE FROM $source IN GRAPH $graph |> tv::out() |> \
			 tv::out();",
			&variables,
		)
		.unwrap()
		.remove(0)
		.result
		.unwrap()
		.into_parts()
		.0;
	let Value::Object(explained) = explained else {
		panic!("FORMAT JSON must return an object");
	};
	let Some(Value::Object(metrics)) = explained.get("analyze") else {
		panic!("ANALYZE metrics are missing");
	};
	assert_eq!(metrics.get("actual_rows"), Some(&Value::from(1_u64)));
	assert_eq!(metrics.get("physical_windows"), Some(&Value::from(0_u64)));
}

#[test]
fn local_physical_traversal_reads_adjacency_overflow() {
	let directory = TestDirectory::new();
	let store = Datastore::create_local(&directory.0, LocalOptions::default()).unwrap();
	let mut seed = store.begin();
	seed.define_class(ClassSchema::new("person", AtomKind::Node, []).unwrap()).unwrap();
	seed.define_class(ClassSchema::new("knows", AtomKind::Edge, []).unwrap()).unwrap();
	let graph = seed.define_graph("overflow_routes", GraphKind::Graph).unwrap();
	let source = "person:source".parse::<RecordId>().unwrap();
	seed.create_atom(source.clone(), PropertyMap::new()).unwrap();
	let source_id = seed.attach_node(graph.id(), &source, None, None).unwrap();
	for index in 0..6 {
		let target = format!("person:target-{index}").parse::<RecordId>().unwrap();
		let edge = format!("knows:edge-{index}").parse::<RecordId>().unwrap();
		seed.create_atom(target.clone(), PropertyMap::new()).unwrap();
		seed.create_atom(edge.clone(), PropertyMap::new()).unwrap();
		let target_id = seed.attach_node(graph.id(), &target, None, None).unwrap();
		seed.attach_edge(
			graph.id(),
			&edge,
			None,
			EdgeTopology::directed([source_id], [target_id]),
			None,
			None,
		)
		.unwrap();
	}
	seed.commit().unwrap();

	let mut variables = Object::new();
	variables.insert("graph", Value::GraphRef(Box::new(graph)));
	variables.insert("source", Value::Record(Box::new(source)));
	let explained = store
		.execute(
			"EXPLAIN ANALYZE FORMAT JSON TRAVERSE FROM $source IN GRAPH $graph |> tv::out();",
			&variables,
		)
		.unwrap()
		.remove(0)
		.result
		.unwrap()
		.into_parts()
		.0;
	let Value::Object(explained) = explained else {
		panic!("FORMAT JSON must return an object");
	};
	let Some(Value::Object(metrics)) = explained.get("analyze") else {
		panic!("ANALYZE metrics are missing");
	};
	assert_eq!(metrics.get("actual_rows"), Some(&Value::from(6_u64)));
	assert_eq!(metrics.get("physical_windows"), Some(&Value::from(1_u64)));
}

#[test]
fn local_physical_traversal_resolves_node_and_edge_inner_graphs() {
	let directory = TestDirectory::new();
	let store = Datastore::create_local(&directory.0, LocalOptions::default()).unwrap();
	let mut seed = store.begin();
	seed.define_class(ClassSchema::new("module", AtomKind::Node, []).unwrap()).unwrap();
	seed.define_class(ClassSchema::new("portal", AtomKind::Edge, []).unwrap()).unwrap();
	let node_child = seed.define_graph("physical_node_child", GraphKind::Graph).unwrap();
	let edge_child = seed.define_graph("physical_edge_child", GraphKind::Graph).unwrap();
	let meta = seed.define_graph("physical_meta", GraphKind::Metagraph).unwrap();
	let owner = "module:owner".parse::<RecordId>().unwrap();
	let target = "module:target".parse::<RecordId>().unwrap();
	let portal = "portal:owner-target".parse::<RecordId>().unwrap();
	for record in [&owner, &target, &portal] {
		seed.create_atom(record.clone(), PropertyMap::new()).unwrap();
	}
	let owner_id = seed.attach_node(meta.id(), &owner, None, Some(node_child.id())).unwrap();
	let target_id = seed.attach_node(meta.id(), &target, None, None).unwrap();
	seed.attach_edge(
		meta.id(),
		&portal,
		None,
		EdgeTopology::directed([owner_id], [target_id]),
		None,
		Some(edge_child.id()),
	)
	.unwrap();
	seed.commit().unwrap();

	let mut variables = Object::new();
	variables.insert("graph", Value::GraphRef(Box::new(meta)));
	variables.insert("owner", Value::Record(Box::new(owner)));
	variables.insert("portal", Value::Record(Box::new(portal)));
	for (source, child) in [("owner", node_child.clone()), ("portal", edge_child)] {
		let query = format!("TRAVERSE FROM ${source} IN GRAPH $graph |> tv::meta_inner();");
		let value =
			store.execute(&query, &variables).unwrap().remove(0).result.unwrap().into_parts().0;
		assert_eq!(value, Value::Array(vec![Value::GraphRef(Box::new(child.clone()))].into()));
	}

	let explained = store
		.execute(
			"EXPLAIN ANALYZE FORMAT JSON TRAVERSE FROM $owner IN GRAPH $graph |> tv::meta_inner();",
			&variables,
		)
		.unwrap()
		.remove(0)
		.result
		.unwrap()
		.into_parts()
		.0;
	let Value::Object(explained) = explained else {
		panic!("FORMAT JSON must return an object");
	};
	let Some(Value::Object(metrics)) = explained.get("analyze") else {
		panic!("ANALYZE metrics are missing");
	};
	assert_eq!(metrics.get("physical_windows"), Some(&Value::from(1_u64)));
}

#[test]
fn local_physical_traversal_resolves_effective_properties() {
	let directory = TestDirectory::new();
	let options = LocalOptions::default();
	let store = Datastore::create_local(&directory.0, options).unwrap();
	let mut seed = store.begin();
	seed.define_class(ClassSchema::new("person", AtomKind::Node, []).unwrap()).unwrap();
	let graph = seed.define_graph("physical_fields", GraphKind::Graph).unwrap();
	let name = seed.intern_property("name").unwrap();
	let hidden = seed.intern_property("hidden").unwrap();
	let source = "person:source".parse::<RecordId>().unwrap();
	let mut properties = PropertyMap::new();
	properties.insert(name, Value::from("base"));
	properties.insert(hidden, Value::from("remove"));
	seed.create_atom(source.clone(), properties).unwrap();
	seed.attach_node(graph.id(), &source, None, None).unwrap();
	seed.set_overlay(graph.id(), &source, name, Value::from("effective")).unwrap();
	seed.hide_property(graph.id(), &source, hidden).unwrap();
	seed.commit().unwrap();

	let mut variables = Object::new();
	variables.insert("graph", Value::GraphRef(Box::new(graph.clone())));
	variables.insert("source", Value::Record(Box::new(source.clone())));
	let query = "TRAVERSE FROM $source IN GRAPH $graph |> tv::values('name');";
	let value = store.execute(query, &variables).unwrap().remove(0).result.unwrap().into_parts().0;
	assert_eq!(value, Value::Array(vec![Value::from("effective")].into()));

	let explained = store
		.execute(
			"EXPLAIN ANALYZE FORMAT JSON TRAVERSE FROM $source IN GRAPH $graph |> \
			 tv::values('properties');",
			&variables,
		)
		.unwrap()
		.remove(0)
		.result
		.unwrap()
		.into_parts()
		.0;
	let Value::Object(explained) = explained else {
		panic!("FORMAT JSON must return an object");
	};
	let Some(Value::Object(metrics)) = explained.get("analyze") else {
		panic!("ANALYZE metrics are missing");
	};
	assert_eq!(metrics.get("physical_windows"), Some(&Value::from(1_u64)));
	assert!(store.checkpoint(CheckpointMode::Restart).unwrap());
	drop(store);

	let reopened = Datastore::open_local(&directory.0, options).unwrap();
	let value =
		reopened.execute(query, &variables).unwrap().remove(0).result.unwrap().into_parts().0;
	assert_eq!(value, Value::Array(vec![Value::from("effective")].into()));
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
