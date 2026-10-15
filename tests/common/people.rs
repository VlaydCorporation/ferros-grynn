//! Public graph setup shared by datastore and graph-index integration targets.

use ferros_grynn::{
	datastore::{ClassField, ClassSchema, Datastore},
	graph::{AtomKind, AtomRef, EdgeTopology, GraphKind, GraphRef},
	values::{PropertyKeyId, PropertyMap, RecordId, Type},
};

/// Commits a small graph through the public transaction API.
///
/// # Panics
/// Panics if public schema, record, graph or transaction fixture setup fails.
pub fn commit_people_graph(store: &Datastore) -> (GraphRef, AtomRef, PropertyKeyId) {
	let mut schema = store.begin();
	schema
		.define_class(
			ClassSchema::new("person", AtomKind::Node, [ClassField::new("name", Type::String)])
				.unwrap(),
		)
		.unwrap();
	schema.define_class(ClassSchema::new("knows", AtomKind::Edge, []).unwrap()).unwrap();
	schema.commit().unwrap();
	let name = store.property_dictionary().get("name").unwrap();
	let alice_record: RecordId = "person:alice".parse().unwrap();
	let mut alice_properties = PropertyMap::new();
	alice_properties.insert(name, "Alice");
	let mut transaction = store.begin();
	let reference = transaction.define_graph("people", GraphKind::Graph).unwrap();
	transaction.create_atom(alice_record.clone(), alice_properties).unwrap();
	let bob_record: RecordId = "person:bob".parse().unwrap();
	transaction.create_atom(bob_record.clone(), PropertyMap::new()).unwrap();
	let alice = transaction
		.attach_node(reference.id(), &alice_record, Some("Person".into()), None)
		.unwrap();
	let bob =
		transaction.attach_node(reference.id(), &bob_record, Some("Person".into()), None).unwrap();
	let edge_record: RecordId = "knows:alice-bob".parse().unwrap();
	transaction.create_atom(edge_record.clone(), PropertyMap::new()).unwrap();
	transaction
		.attach_edge(
			reference.id(),
			&edge_record,
			Some("KNOWS".into()),
			EdgeTopology::directed([alice], [bob]),
			None,
			None,
		)
		.unwrap();
	transaction.commit().unwrap();

	(reference, AtomRef::new(AtomKind::Node, alice_record), name)
}
