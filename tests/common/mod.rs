use ferros_grynn::{
	datastore::{ClassField, ClassSchema, Datastore},
	graph::{
		AtomKind,
		AtomRef,
		EdgeRead,
		EdgeTopology,
		Frozen,
		Graph,
		GraphKind,
		GraphRead,
		GraphRef,
		GraphStorageMode,
		NodeRead,
		query_to_memory,
	},
	values::{Number, PropertyKeyId, PropertyMap, RecordId, Type, Value},
};

/// Commits a small graph through the public transaction API.
pub(crate) fn commit_people_graph(store: &Datastore) -> (GraphRef, AtomRef, PropertyKeyId) {
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
	let reference = transaction.define_graph("transient", GraphKind::Graph).unwrap();
	assert!(reference.read(&transaction).is_ok());
	reference
}

/// Exercises the normalized global-atom and graph-occurrence model.
pub(crate) fn exercise_normalized_atom_model(
	store: &Datastore,
) -> (GraphRef, AtomRef, PropertyKeyId) {
	let alice = "shared_person:alice".parse::<RecordId>().unwrap();
	let bob = "shared_person:bob".parse::<RecordId>().unwrap();
	let edge = "shared_link:shared".parse::<RecordId>().unwrap();
	let mut schema = store.begin();
	schema
		.define_class(
			ClassSchema::new("shared_person", AtomKind::Node, [ClassField::new(
				"shared_name",
				Type::String,
			)])
			.unwrap(),
		)
		.unwrap();
	schema.define_class(ClassSchema::new("shared_link", AtomKind::Edge, []).unwrap()).unwrap();
	schema.commit().unwrap();
	let key = store.property_dictionary().get("shared_name").unwrap();
	let mut seed = store.begin();
	let mut properties = PropertyMap::new();
	properties.insert(key, "Base");
	seed.create_atom(alice.clone(), properties).unwrap();
	seed.create_atom(bob.clone(), PropertyMap::new()).unwrap();
	seed.create_atom(edge.clone(), PropertyMap::new()).unwrap();
	let first = seed.define_graph("first", GraphKind::Graph).unwrap();
	let second = seed.define_graph("second", GraphKind::Graph).unwrap();
	let first_alice = seed.attach_node(first.id(), &alice, Some("First".into()), None).unwrap();
	let first_bob = seed.attach_node(first.id(), &bob, None, None).unwrap();
	let second_alice = seed.attach_node(second.id(), &alice, Some("Second".into()), None).unwrap();
	let second_bob = seed.attach_node(second.id(), &bob, None, None).unwrap();
	seed.attach_edge(
		first.id(),
		&edge,
		Some("Forward".into()),
		EdgeTopology::directed([first_alice], [first_bob]),
		Some(Number::from(1_i64)),
		None,
	)
	.unwrap();
	seed.attach_edge(
		second.id(),
		&edge,
		Some("Reverse".into()),
		EdgeTopology::directed([second_bob], [second_alice]),
		Some(Number::from(2_i64)),
		None,
	)
	.unwrap();
	seed.commit().unwrap();

	let mut update = store.begin();
	update.set_base_property(&alice, key, Value::from("Updated")).unwrap();
	update.set_overlay(first.id(), &alice, key, Value::from("First view")).unwrap();
	update.commit().unwrap();

	let mut read = store.begin();
	assert_eq!(
		read.effective_property(first.id(), &alice, key).unwrap(),
		Some(Value::from("First view"))
	);
	assert_eq!(
		read.effective_property(second.id(), &alice, key).unwrap(),
		Some(Value::from("Updated"))
	);
	let first_view = first.read(&read).unwrap();
	let materialized = query_to_memory::<Graph<Frozen>, _>(&first_view).unwrap();
	assert_eq!(materialized.storage_mode(), GraphStorageMode::Hybrid);
	let local =
		materialized.atom_id(&AtomRef::new(AtomKind::Node, alice.clone())).unwrap().unwrap();
	assert_eq!(
		materialized.node(local).unwrap().properties().get(key),
		Some(&Value::from("First view"))
	);
	drop(first_view);
	drop(read);

	let mut reset = store.begin();
	reset.reset_property(first.id(), &alice, key).unwrap();
	reset.commit().unwrap();
	let mut read = store.begin();
	assert_eq!(
		read.effective_property(first.id(), &alice, key).unwrap(),
		Some(Value::from("Updated"))
	);
	let first_view = first.read(&read).unwrap();
	let second_view = second.read(&read).unwrap();
	let alice_ref = AtomRef::new(AtomKind::Node, alice.clone());
	let first_alice = first_view.atom_id(&alice_ref).unwrap().unwrap();
	let second_alice = second_view.atom_id(&alice_ref).unwrap().unwrap();
	assert_eq!(
		first_view.node(first_alice).unwrap().unwrap().label().unwrap().as_deref(),
		Some("First")
	);
	assert_eq!(
		second_view.node(second_alice).unwrap().unwrap().label().unwrap().as_deref(),
		Some("Second")
	);
	let first_edge =
		first_view.edge(first_view.edge_ids().unwrap().next().unwrap().unwrap()).unwrap().unwrap();
	let second_edge = second_view
		.edge(second_view.edge_ids().unwrap().next().unwrap().unwrap())
		.unwrap()
		.unwrap();
	assert_ne!(first_edge.topology().unwrap(), second_edge.topology().unwrap());
	assert_eq!(first_edge.weight().unwrap().to_string(), "1");
	assert_eq!(second_edge.weight().unwrap().to_string(), "2");
	drop(first_view);
	drop(second_view);
	drop(read);

	let mut remove = store.begin();
	remove.remove_graph("first").unwrap();
	remove.commit().unwrap();
	let mut read = store.begin();
	assert!(first.read(&read).is_err());
	assert!(second.read(&read).is_ok());
	assert!(read.atom(&alice).unwrap().is_some());

	(second, alice_ref, key)
}
