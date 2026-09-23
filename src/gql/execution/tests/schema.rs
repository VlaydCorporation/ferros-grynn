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
