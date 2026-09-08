use ferros_grynn::{
	datastore::Datastore,
	graph::{AtomKind, AtomRef, EdgeTopology, Graph, GraphRead, GraphRef, Mutable, NodeRead},
	values::{PropertyKeyId, PropertyMap, RecordId, Value},
};

/// Commits a small graph through the public transaction API.
pub(crate) fn commit_people_graph(store: &Datastore) -> (GraphRef, AtomRef, PropertyKeyId) {
	let name = store.intern_property("name").unwrap();
	let alice_record: RecordId = "person:alice".parse().unwrap();
	let mut graph = Graph::<Mutable>::new();
	let mut alice_properties = PropertyMap::new();
	alice_properties.insert(name, "Alice");
	let alice = graph
		.add_node_with_id(alice_record.clone(), Some("Person".into()), alice_properties)
		.unwrap();
	let bob = graph
		.add_node_with_id("person:bob".parse().unwrap(), Some("Person".into()), PropertyMap::new())
		.unwrap();
	graph
		.add_edge_with_id(
			"knows:alice-bob".parse().unwrap(),
			EdgeTopology::directed([alice], [bob]),
			Some("KNOWS".into()),
			PropertyMap::new(),
		)
		.unwrap();

	let mut transaction = store.begin();
	let reference =
		transaction.create_graph("graph:people".parse().unwrap(), graph.freeze().into()).unwrap();
	transaction.commit().unwrap();

	(reference, AtomRef::new(AtomKind::Node, alice_record), name)
}

/// Verifies a committed graph through stable public references.
pub(crate) fn assert_people_graph(
	store: &Datastore,
	reference: &GraphRef,
	alice: &AtomRef,
	name: PropertyKeyId,
) {
	let transaction = store.begin();
	let graph = reference.read(&transaction).unwrap();
	assert_eq!(graph.node_count().unwrap(), 2);
	assert_eq!(graph.edge_count().unwrap(), 1);
	let alice = graph.atom_id(alice).unwrap().unwrap();
	let alice = graph.node(alice).unwrap().unwrap();
	assert_eq!(alice.label().unwrap().as_deref(), Some("Person"));
	assert_eq!(alice.properties().unwrap().get(name), Some(&Value::from("Alice")));
}

/// Creates a graph that is visible only to its uncommitted transaction.
pub(crate) fn create_uncommitted_graph(store: &Datastore) -> GraphRef {
	let mut transaction = store.begin();
	let reference = transaction
		.create_graph("graph:transient".parse().unwrap(), Graph::<Mutable>::new().freeze().into())
		.unwrap();
	assert!(reference.read(&transaction).is_ok());
	reference
}
