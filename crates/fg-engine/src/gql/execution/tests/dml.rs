#[cfg(test)]
use crate::test_assertions::{AssertOption as _, AssertResult as _};
use crate::{
	datastore::{ClassField, ClassSchema, ClassSchemaMode, Datastore},
	values::{AtomKind, Object, PropertyMap, RecordId, Type, Value},
};

/// Returns typed store for this module.
fn typed_store() -> Datastore {
	let store = Datastore::in_memory();
	let mut schema = store.begin();
	schema
		.define_class(
			ClassSchema::new("person", AtomKind::Node, [ClassField::new("name", Type::String)])
				.assert_ok()
				.with_schema_mode(ClassSchemaMode::Schemafull),
		)
		.assert_ok();
	schema.commit().assert_ok();
	store
}

/// Returns referenced store for this module.
fn referenced_store(attach_profile: bool) -> Datastore {
	let store = Datastore::in_memory();
	let mut transaction = store.begin();
	transaction
		.define_class(
			ClassSchema::new("profile", AtomKind::Node, [ClassField::new("name", Type::String)])
				.assert_ok(),
		)
		.assert_ok();
	transaction
		.define_class(
			ClassSchema::new("person", AtomKind::Node, [ClassField::new("profile", Type::Record)])
				.assert_ok(),
		)
		.assert_ok();
	let profile = "profile:primary".parse::<RecordId>().assert_ok();
	let person = "person:alice".parse::<RecordId>().assert_ok();
	let name = transaction.intern_property("name").assert_ok();
	let profile_field = transaction.intern_property("profile").assert_ok();
	let mut profile_values = PropertyMap::new();
	profile_values.insert(name, "Base profile");
	transaction.create_atom(profile.clone(), profile_values).assert_ok();
	let mut person_values = PropertyMap::new();
	person_values.insert(profile_field, Value::Record(Box::new(profile.clone())));
	transaction.create_atom(person.clone(), person_values).assert_ok();
	let graph = transaction.define_graph("social", crate::graph::GraphKind::Graph).assert_ok();
	transaction.attach_node(graph.id(), &person, None, None).assert_ok();
	if attach_profile {
		transaction.attach_node(graph.id(), &profile, None, None).assert_ok();
	}
	transaction.commit().assert_ok();
	store
}

/// Verifies direct dml uses global atom store without graph membership.
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
		.assert_ok();
	assert!(results.iter().all(|result| result.result.is_ok()));
	let values = match results[2].result.as_ref().assert_ok().value() {
		Value::Array(values) => Some(values),
		_ => None,
	}
	.assert_some_with("SELECT must return an array");
	assert_eq!(values.as_slice(), &[Value::from("Alicia")]);
	transaction.commit().assert_ok();

	let mut read = store.begin();
	let id = "person:alice".parse::<RecordId>().assert_ok();
	assert_eq!(read.atom(&id).assert_ok().assert_some().kind(), AtomKind::Node);
}

/// Verifies delete restricts attached global atom.
#[test]
fn delete_restricts_attached_global_atom() {
	let store = typed_store();
	let mut seed = store.begin();
	let id = "person:alice".parse::<RecordId>().assert_ok();
	seed.create_atom(id.clone(), PropertyMap::default()).assert_ok();
	let graph = seed.define_graph("social", crate::graph::GraphKind::Graph).assert_ok();
	seed.attach_node(graph.id(), &id, None, None).assert_ok();
	seed.commit().assert_ok();

	let mut transaction = store.begin();
	let results = transaction.execute("DELETE person:alice;", &Object::new()).assert_ok();
	assert!(results[0].result.is_err());
}

/// Verifies graph scoped select and update use effective properties.
#[test]
fn graph_scoped_select_and_update_use_effective_properties() {
	let store = typed_store();
	let mut seed = store.begin();
	let attached = "person:alice".parse::<RecordId>().assert_ok();
	let unattached = "person:bob".parse::<RecordId>().assert_ok();
	let name = seed.intern_property("name").assert_ok();
	let mut alice = crate::values::PropertyMap::new();
	alice.insert(name, "Alice");
	seed.create_atom(attached.clone(), alice).assert_ok();
	seed.create_atom(unattached, PropertyMap::default()).assert_ok();
	let graph = seed.define_graph("social", crate::graph::GraphKind::Graph).assert_ok();
	seed.attach_node(graph.id(), &attached, None, None).assert_ok();
	seed.commit().assert_ok();

	let mut transaction = store.begin();
	let results = transaction
		.execute(
			"UPDATE person:alice IN GRAPH social SET name = 'Graph Alice'; SELECT VALUE name FROM \
			 person IN GRAPH social; SELECT VALUE name FROM person:alice; UPDATE person:alice IN \
			 GRAPH social RESET name; SELECT VALUE name FROM person:alice IN GRAPH social;",
			&Object::new(),
		)
		.assert_ok();
	assert!(results.iter().all(|result| result.result.is_ok()));
	let values = |index: usize| {
		results[index].result.as_ref().assert_ok().value().as_array().assert_some().as_slice()
	};
	assert_eq!(values(1), &[Value::from("Graph Alice")]);
	assert_eq!(values(2), &[Value::from("Alice")]);
	assert_eq!(values(4), &[Value::from("Alice")]);
}

/// Verifies graph scoped update requires membership.
#[test]
fn graph_scoped_update_requires_membership() {
	let store = typed_store();
	let mut seed = store.begin();
	seed.create_atom("person:alice".parse().assert_ok(), PropertyMap::default()).assert_ok();
	seed.define_graph("social", crate::graph::GraphKind::Graph).assert_ok();
	seed.commit().assert_ok();

	let mut transaction = store.begin();
	let results = transaction
		.execute("UPDATE person:alice IN GRAPH social SET name = 'Alice';", &Object::new())
		.assert_ok();
	assert!(results[0].result.is_err());
}

/// Verifies graph scoped record reference falls back to base for non member.
#[test]
fn graph_scoped_record_reference_falls_back_to_base_for_non_member() {
	let store = referenced_store(false);
	let mut transaction = store.begin();
	let results = transaction
		.execute("SELECT VALUE profile.name FROM person:alice IN GRAPH social;", &Object::new())
		.assert_ok();
	assert_eq!(
		results[0].result.as_ref().assert_ok().value().as_array().assert_some().as_slice(),
		&[Value::from("Base profile")]
	);
}

/// Verifies graph scoped record reference uses effective member properties.
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
		.assert_ok();
	assert!(results.iter().all(|result| result.result.is_ok()));
	assert_eq!(
		results[1].result.as_ref().assert_ok().value().as_array().assert_some().as_slice(),
		&[Value::from("Scoped profile")]
	);
}

/// Verifies graph scoped dml record operand preserves member and base fallback.
#[test]
fn graph_scoped_dml_record_operand_preserves_member_and_base_fallback() {
	use crate::test_assertions::AssertResult as _;

	for member in [false, true] {
		let store = referenced_store(member);
		let mut transaction = store.begin();
		if member {
			for result in transaction
				.execute(
					"UPDATE profile:primary IN GRAPH social SET name = 'Scoped profile';",
					&Object::new(),
				)
				.assert_ok()
			{
				let _value = result.result.assert_ok();
			}
		}
		let results = transaction
			.execute(
				"UPDATE person:alice IN GRAPH social SET name = profile.name RETURN VALUE name;",
				&Object::new(),
			)
			.assert_ok();
		let expected = if member {
			"Scoped profile"
		} else {
			"Base profile"
		};
		for result in results {
			assert_eq!(
				result.result.assert_ok().value().as_array().map(crate::values::Array::as_slice),
				Some([Value::from(expected)].as_slice())
			);
		}
	}
}
