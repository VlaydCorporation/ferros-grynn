use ferros_grynn::{
	datastore::{ClassIndex, ClassIndexKind, ClassIndexState, ClassSchema, Datastore, IndexScope},
	graph::{AtomKind, GraphKind},
	values::{Object, PropertyMap, RecordId, Value},
};

use crate::support::{create_person, execute, people, record, value};

/// Reads label-only MATCH through the downstream query API.
fn occurrence_label_ids(store: &Datastore, source: &str) -> Vec<RecordId> {
	let rows = value(store, source, &Object::new());
	let mut ids = rows
		.as_array()
		.unwrap()
		.iter()
		.map(|row| {
			row.as_object()
				.unwrap()
				.get("n")
				.unwrap()
				.as_atom_occurrence()
				.unwrap()
				.atom()
				.id()
				.clone()
		})
		.collect::<Vec<_>>();
	ids.sort();
	ids
}

/// Label replacement, reset and abort preserve exact MATCH after recovery.
#[test]
fn occurrence_label_lookup_matches_query_after_replace_reset_and_reopen() {
	const LABEL_A: &str = "MATCH (* AS n LABEL A) IN GRAPH selected RETURN n";
	const LABEL_B: &str = "MATCH (* AS n LABEL B) IN GRAPH selected RETURN n";
	let db = database();
	let mut seed = db.begin();
	seed.define_class(ClassSchema::new("person", AtomKind::Node, []).unwrap()).unwrap();
	let graph = seed.define_graph("selected", GraphKind::Graph).unwrap();
	let other = seed.define_graph("other", GraphKind::Graph).unwrap();
	let records = ["person:a", "person:b", "person:c"].map(record);
	for (record, label) in records.iter().zip(["A", "B", "A"]) {
		seed.create_atom(record.clone(), PropertyMap::new()).unwrap();
		seed.attach_node(graph.id(), record, Some(label.into()), None).unwrap();
	}
	seed.attach_node(other.id(), &records[0], Some("A".into()), None).unwrap();
	seed.commit().unwrap();
	{
		let mut aborted = db.begin();
		aborted.set_occurrence_label(graph.id(), &records[0], Some("C".into())).unwrap();
	}
	assert_eq!(occurrence_label_ids(&db, LABEL_A), [records[0].clone(), records[2].clone()]);
	let mut write = db.begin();
	write.set_occurrence_label(graph.id(), &records[0], Some("B".into())).unwrap();
	write.commit().unwrap();
	assert_eq!(occurrence_label_ids(&db, LABEL_B), records[..2]);
	let mut reset = db.begin();
	reset.set_occurrence_label(graph.id(), &records[2], None).unwrap();
	reset.commit().unwrap();
	db.assert_committed(|store| {
		assert!(occurrence_label_ids(store, LABEL_A).is_empty());
		assert_eq!(occurrence_label_ids(store, LABEL_B), records[..2]);
		assert_eq!(
			occurrence_label_ids(store, "MATCH (* AS n LABEL A) IN GRAPH other RETURN n"),
			std::slice::from_ref(&records[0]),
		);
	});
}

/// Verifies sdk scalar index matches scan after update delete and rollback.
#[test]
fn sdk_scalar_index_matches_scan_after_update_delete_and_rollback() {
	let db = database();
	let (_, age) = people(&db);
	let mut schema = db.begin();
	schema
		.define_index(
			"person",
			ClassIndex::new("by_age", ClassIndexKind::Standard, IndexScope::Base, ["age"]).unwrap(),
		)
		.unwrap();
	schema.commit().unwrap();
	for (id, age) in [("person:a", 10), ("person:b", 20), ("person:c", 30)] {
		create_person(&db, id, id, age);
	}
	let forced = "SELECT VALUE id FROM person WITH INDEX by_age WHERE age >= 20 ORDER BY id;";
	let scan = "SELECT VALUE id FROM person WITH NOINDEX WHERE age >= 20 ORDER BY id;";
	let before = value(&db, forced, &Object::new());
	{
		let mut aborted = db.begin();
		aborted.set_base_property(&record("person:a"), age, Value::from(100_i64)).unwrap();
	}
	assert_eq!(value(&db, forced, &Object::new()), before);
	let mut write = db.begin();
	write.set_base_property(&record("person:a"), age, Value::from(25_i64)).unwrap();
	write.delete_atom(&record("person:c")).unwrap();
	write.commit().unwrap();
	let expected =
		Value::Array(vec![Value::from(record("person:a")), Value::from(record("person:b"))].into());
	db.assert_committed(|store| {
		assert_eq!(value(store, forced, &Object::new()), expected);
		assert_eq!(value(store, scan, &Object::new()), expected);
	});
}

/// Verifies sdk unique index rejects duplicate values without losing valid
/// writes.
#[test]
fn sdk_unique_index_rejects_duplicate_values_without_losing_valid_writes() {
	let db = database();
	let (_, age) = people(&db);
	let mut schema = db.begin();
	schema
		.define_index(
			"person",
			ClassIndex::new("unique_age", ClassIndexKind::Unique, IndexScope::Base, ["age"])
				.unwrap(),
		)
		.unwrap();
	schema.commit().unwrap();
	create_person(&db, "person:a", "A", 10);
	create_person(&db, "person:b", "B", 20);
	let mut write = db.begin();
	assert!(write.set_base_property(&record("person:b"), age, Value::from(10_i64)).is_err());
	write.set_base_property(&record("person:b"), age, Value::from(30_i64)).unwrap();
	write.commit().unwrap();
	db.assert_committed(|store| {
		assert_eq!(
			value(store, "SELECT VALUE age FROM person ORDER BY age;", &Object::new()),
			Value::Array(vec![Value::from(10_i64), Value::from(30_i64)].into())
		);
	});
}

/// Verifies effective scalar index tracks overlay reset and detach.
#[test]
fn effective_scalar_index_tracks_overlay_reset_and_detach() {
	let db = database();
	people(&db);
	create_person(&db, "person:a", "A", 10);
	create_person(&db, "person:b", "B", 40);
	execute(
		&db,
		"DEFINE GRAPH selected; ATTACH person:a TO GRAPH selected; ATTACH person:b TO GRAPH \
		 selected; DEFINE INDEX effective_age ON CLASS person FIELDS age SCOPE EFFECTIVE IN GRAPH \
		 selected; UPDATE person:a IN GRAPH selected SET age = 50;",
		&Object::new(),
	);
	let forced = "SELECT VALUE id FROM person IN GRAPH @selected WITH INDEX effective_age WHERE \
	              age >= 30 ORDER BY id;";
	assert_eq!(
		value(&db, forced, &Object::new()),
		Value::Array(vec![Value::from(record("person:a")), Value::from(record("person:b"))].into())
	);
	let mut write = db.begin();
	let graph = write.named_graph("selected").unwrap().unwrap();
	let age = db.property_dictionary().get("age").unwrap();
	write.reset_property(graph.id(), &record("person:a"), age).unwrap();
	write.detach_occurrence(graph.id(), &record("person:b"), false).unwrap();
	write.commit().unwrap();
	db.assert_committed(|store| {
		assert!(value(store, forced, &Object::new()).as_array().unwrap().is_empty());
		assert_eq!(
			value(store, "SELECT VALUE id FROM person ORDER BY id;", &Object::new())
				.as_array()
				.unwrap()
				.len(),
			2
		);
	});
}

/// Verifies deferred index visibility follows its snapshot and count remains exact.
#[test]
fn deferred_index_requires_rebuild_and_count_remains_exact() {
	let db = database();
	people(&db);
	execute(
		&db,
		"DEFINE INDEX by_age ON CLASS person FIELDS age SCOPE BASE DEFER; DEFINE INDEX total ON \
		 CLASS person COUNT SCOPE BASE; CREATE person:a SET name = 'A', age = 10;",
		&Object::new(),
	);
	// Background catch-up may publish Ready between independent datastore reads.
	let mut reader = db.begin();
	let class = reader.class("person").unwrap().unwrap();
	let index = class.indexes().find(|index| index.name().as_str() == "by_age").unwrap();
	assert!(matches!(index.state(), ClassIndexState::Stale | ClassIndexState::Ready));
	let forced = "SELECT VALUE id FROM person WITH INDEX by_age WHERE age = 10;";
	let rows = reader.execute(forced, &Object::new()).unwrap();
	assert_eq!(rows.len(), 1);
	if index.state() == ClassIndexState::Stale {
		assert_eq!(rows[0].result.as_ref().unwrap_err().status().code().to_string(), "FG-42F03");
	} else {
		assert_eq!(
			rows[0].result.as_ref().unwrap().value(),
			&Value::Array(vec![Value::from(record("person:a"))].into())
		);
	}
	drop(reader);
	assert_eq!(
		value(&db, "SELECT VALUE id FROM person WHERE age = 10;", &Object::new())
			.as_array()
			.unwrap()
			.len(),
		1
	);
	let rebuild = db.execute("REBUILD INDEX by_age ON CLASS person;", &Object::new()).unwrap();
	assert_eq!(rebuild.len(), 1);
	// A winning background publication must leave the same exact Ready index below.
	if let Err(error) = &rebuild[0].result {
		assert_eq!(error.status().code().to_string(), "FG-40F02");
	}
	db.assert_committed(|store| {
		assert_eq!(
			store
				.class("person")
				.unwrap()
				.indexes()
				.find(|index| index.name().as_str() == "by_age")
				.unwrap()
				.state(),
			ClassIndexState::Ready
		);
		assert_eq!(
			value(store, forced, &Object::new()),
			Value::Array(vec![Value::from(record("person:a"))].into())
		);
		assert_eq!(
			value(store, "SELECT VALUE count() FROM person WITH INDEX total;", &Object::new()),
			Value::Array(vec![Value::from(1_i64)].into())
		);
	});
}
