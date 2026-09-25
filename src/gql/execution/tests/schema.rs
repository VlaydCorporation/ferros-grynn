use crate::{
	datastore::{ClassIndexKind, ClassSchemaMode, Datastore, IndexScope},
	values::{AtomKind, Object, Type, Value},
};

#[test]
fn class_field_index_and_info_share_one_transaction_visible_catalog() {
	let store = Datastore::in_memory();
	let mut transaction = store.begin();
	let definitions = transaction
		.execute("DEFINE GRAPH social; DEFINE CLASS person TYPE NODE SCHEMAFULL;", &Object::new())
		.unwrap();
	assert!(definitions.iter().all(|result| result.result.is_ok()));
	let results = transaction
		.execute(
			"DEFINE FIELD email ON CLASS person TYPE string; DEFINE INDEX by_email ON CLASS \
			 person FIELDS email UNIQUE SCOPE EFFECTIVE IN GRAPH social; INFO FOR CLASS person;",
			&Object::new(),
		)
		.unwrap();
	assert_eq!(results.len(), 3);
	assert!(results.iter().all(|result| result.result.is_ok()));
	let info = results[2].result.as_ref().unwrap().value().as_object().unwrap();
	assert!(info.get("definition").is_some());
	assert!(info.get("indexes").and_then(Value::as_object).unwrap().contains_key("by_email"));
	transaction.commit().unwrap();

	let class = store.class("person").unwrap();
	assert_eq!(class.kind(), AtomKind::Node);
	assert_eq!(class.schema_mode(), ClassSchemaMode::Schemafull);
	assert_eq!(class.fields().next().unwrap().value_type(), &Type::String);
	let index = class.indexes().next().unwrap();
	assert_eq!(index.kind(), ClassIndexKind::Unique);
	assert!(matches!(index.scope(), IndexScope::Effective { .. }));
}

#[test]
fn overwrite_cannot_change_class_kind() {
	let store = Datastore::in_memory();
	let mut transaction = store.begin();
	transaction.execute("DEFINE CLASS person TYPE NODE;", &Object::new()).unwrap();
	let results =
		transaction.execute("DEFINE CLASS OVERWRITE person TYPE EDGE;", &Object::new()).unwrap();
	assert!(results[0].result.is_err());
}

#[test]
fn graph_properties_are_transactional_and_use_nested_paths() {
	let store = Datastore::in_memory();
	let defined = store
		.execute(
			"DEFINE GRAPH sales TYPE DAG CONTENT {owner: 'analytics', region: 'eu', address: \
			 {city: 'Paris'}};",
			&Object::new(),
		)
		.unwrap();
	assert!(defined[0].result.is_ok());
	let updated = store
		.execute(
			"UPDATE GRAPH sales SET properties.owner = 'risk', properties.address.city = 'Berlin' \
			 RETURN AFTER; UPDATE GRAPH sales UNSET properties.region RETURN DIFF;",
			&Object::new(),
		)
		.unwrap();
	assert!(updated.iter().all(|result| result.result.is_ok()));
	let after = updated[0].result.as_ref().unwrap().value().as_object().unwrap();
	let properties = after.get("properties").and_then(Value::as_object).unwrap();
	assert_eq!(properties.get("owner"), Some(&Value::from("risk")));
	assert_eq!(
		properties
			.get("address")
			.and_then(Value::as_object)
			.and_then(|address| address.get("city")),
		Some(&Value::from("Berlin")),
	);
	let schema = store.schema_catalog().named_graph("sales").unwrap();
	let dictionary = store.property_dictionary();
	assert_eq!(
		schema.properties().get(dictionary.get("owner").unwrap()),
		Some(&Value::from("risk")),
	);
	assert!(!schema.properties().contains_key(dictionary.get("region").unwrap()));

	let mut rolled_back = store.begin();
	let result = rolled_back
		.execute("UPDATE GRAPH sales SET properties.owner = 'temporary';", &Object::new())
		.unwrap();
	assert!(result[0].result.is_ok());
	drop(rolled_back);
	let schema = store.schema_catalog().named_graph("sales").unwrap();
	assert_eq!(
		schema.properties().get(dictionary.get("owner").unwrap()),
		Some(&Value::from("risk")),
	);
}

#[test]
fn concurrent_graph_property_writers_conflict() {
	let store = Datastore::in_memory();
	store.execute("DEFINE GRAPH sales CONTENT {owner: 'first'};", &Object::new()).unwrap();
	let mut first = store.begin();
	let mut second = store.begin();
	assert!(
		first.execute("UPDATE GRAPH sales SET properties.owner = 'one';", &Object::new()).unwrap()
			[0]
		.result
		.is_ok()
	);
	assert!(
		second.execute("UPDATE GRAPH sales SET properties.owner = 'two';", &Object::new()).unwrap()
			[0]
		.result
		.is_ok()
	);
	first.commit().unwrap();
	assert!(second.commit().is_err());
}
