use crate::{
	datastore::{ClassField, ClassSchema, ClassSchemaMode, Datastore},
	values::{AtomKind, Object, PropertyMap, RecordId, Type, Value},
};

fn typed_store() -> Datastore {
	let store = Datastore::in_memory();
	let mut schema = store.begin();
	schema
		.define_class(
			ClassSchema::new("person", AtomKind::Node, [ClassField::new("name", Type::String)])
				.unwrap()
				.with_schema_mode(ClassSchemaMode::Schemafull),
		)
		.unwrap();
	schema.commit().unwrap();
	store
}

fn referenced_store(attach_profile: bool) -> Datastore {
	let store = Datastore::in_memory();
	let mut transaction = store.begin();
	transaction
		.define_class(
			ClassSchema::new("profile", AtomKind::Node, [ClassField::new("name", Type::String)])
				.unwrap(),
		)
		.unwrap();
	transaction
		.define_class(
			ClassSchema::new("person", AtomKind::Node, [ClassField::new("profile", Type::Record)])
				.unwrap(),
		)
		.unwrap();
	let profile = "profile:primary".parse::<RecordId>().unwrap();
	let person = "person:alice".parse::<RecordId>().unwrap();
	let name = transaction.intern_property("name").unwrap();
	let profile_field = transaction.intern_property("profile").unwrap();
	let mut profile_values = PropertyMap::new();
	profile_values.insert(name, "Base profile");
	transaction.create_atom(profile.clone(), profile_values).unwrap();
	let mut person_values = PropertyMap::new();
	person_values.insert(profile_field, Value::Record(Box::new(profile.clone())));
	transaction.create_atom(person.clone(), person_values).unwrap();
	let graph = transaction.define_graph("social", crate::graph::GraphKind::Graph).unwrap();
	transaction.attach_node(graph.id(), &person, None, None).unwrap();
	if attach_profile {
		transaction.attach_node(graph.id(), &profile, None, None).unwrap();
	}
	transaction.commit().unwrap();
	store
}

#[test]
fn direct_dml_uses_global_atom_store_without_graph_membership() {
	let store = typed_store();
	let mut transaction = store.begin();
	let results = transaction
		.execute(
			"CREATE person:alice SET name = 'Alice'; UPDATE person:alice SET name = 'Alicia'; \
			 SELECT VALUE name FROM person:alice;",
			&Object::new(),
		)
		.unwrap();
	assert!(results.iter().all(|result| result.result.is_ok()));
	let Value::Array(values) = results[2].result.as_ref().unwrap().value() else {
		panic!("SELECT must return an array");
	};
	assert_eq!(values.as_slice(), &[Value::from("Alicia")]);
	transaction.commit().unwrap();

	let mut read = store.begin();
	let id = "person:alice".parse::<RecordId>().unwrap();
	assert_eq!(read.atom(&id).unwrap().unwrap().kind(), AtomKind::Node);
}

#[test]
fn delete_restricts_attached_global_atom() {
	let store = typed_store();
	let mut seed = store.begin();
	let id = "person:alice".parse::<RecordId>().unwrap();
	seed.create_atom(id.clone(), Default::default()).unwrap();
	let graph = seed.define_graph("social", crate::graph::GraphKind::Graph).unwrap();
	seed.attach_node(graph.id(), &id, None, None).unwrap();
	seed.commit().unwrap();

	let mut transaction = store.begin();
	let results = transaction.execute("DELETE person:alice;", &Object::new()).unwrap();
	assert!(results[0].result.is_err());
}

#[test]
fn graph_scoped_select_and_update_use_effective_properties() {
	let store = typed_store();
	let mut seed = store.begin();
	let attached = "person:alice".parse::<RecordId>().unwrap();
	let unattached = "person:bob".parse::<RecordId>().unwrap();
	let name = seed.intern_property("name").unwrap();
	let mut alice = crate::values::PropertyMap::new();
	alice.insert(name, "Alice");
	seed.create_atom(attached.clone(), alice).unwrap();
	seed.create_atom(unattached, Default::default()).unwrap();
	let graph = seed.define_graph("social", crate::graph::GraphKind::Graph).unwrap();
	seed.attach_node(graph.id(), &attached, None, None).unwrap();
	seed.commit().unwrap();

	let mut transaction = store.begin();
	let results = transaction
		.execute(
			"UPDATE person:alice IN GRAPH social SET name = 'Graph Alice'; SELECT VALUE name FROM \
			 person IN GRAPH social; SELECT VALUE name FROM person:alice; UPDATE person:alice IN \
			 GRAPH social RESET name; SELECT VALUE name FROM person:alice IN GRAPH social;",
			&Object::new(),
		)
		.unwrap();
	assert!(results.iter().all(|result| result.result.is_ok()));
	let values = |index: usize| {
		results[index].result.as_ref().unwrap().value().as_array().unwrap().as_slice()
	};
	assert_eq!(values(1), &[Value::from("Graph Alice")]);
	assert_eq!(values(2), &[Value::from("Alice")]);
	assert_eq!(values(4), &[Value::from("Alice")]);
}

#[test]
fn graph_scoped_update_requires_membership() {
	let store = typed_store();
	let mut seed = store.begin();
	seed.create_atom("person:alice".parse().unwrap(), Default::default()).unwrap();
	seed.define_graph("social", crate::graph::GraphKind::Graph).unwrap();
	seed.commit().unwrap();

	let mut transaction = store.begin();
	let results = transaction
		.execute("UPDATE person:alice IN GRAPH social SET name = 'Alice';", &Object::new())
		.unwrap();
	assert!(results[0].result.is_err());
}

#[test]
fn graph_scoped_record_reference_falls_back_to_base_for_non_member() {
	let store = referenced_store(false);
	let mut transaction = store.begin();
	let results = transaction
		.execute("SELECT VALUE profile.name FROM person:alice IN GRAPH social;", &Object::new())
		.unwrap();
	assert_eq!(results[0].result.as_ref().unwrap().value().as_array().unwrap().as_slice(), &[
		Value::from("Base profile")
	]);
}

#[test]
fn graph_scoped_record_reference_uses_effective_member_properties() {
	let store = referenced_store(true);
	let mut transaction = store.begin();
	let results = transaction
		.execute(
			"UPDATE profile:primary IN GRAPH social SET name = 'Scoped profile'; SELECT VALUE \
			 profile.name FROM person:alice IN GRAPH social;",
			&Object::new(),
		)
		.unwrap();
	assert!(results.iter().all(|result| result.result.is_ok()));
	assert_eq!(results[1].result.as_ref().unwrap().value().as_array().unwrap().as_slice(), &[
		Value::from("Scoped profile")
	]);
}
