use crate::{
	datastore::{ClassSchema, Datastore, Transaction},
	graph::{AtomKind, EdgeTopology, GraphKind},
	values::{Object, RecordId, Text},
};

fn matching_ids(transaction: &mut Transaction, source: &str, column: &str) -> Vec<RecordId> {
	let results = transaction.execute(source, &Object::new()).unwrap();
	let rows = results[0].result.as_ref().unwrap().value().as_array().unwrap();
	let mut ids = rows
		.iter()
		.map(|row| row.as_object().unwrap().get(column).unwrap().as_atom().unwrap().id().clone())
		.collect::<Vec<_>>();
	ids.sort();
	ids
}

#[test]
fn match_routes_exact_class_label_and_intersection_without_legacy_syntax() {
	let store = Datastore::in_memory();
	let mut seed = store.begin();
	seed.define_class(ClassSchema::new("person", AtomKind::Node, []).unwrap()).unwrap();
	seed.define_class(ClassSchema::new("device", AtomKind::Node, []).unwrap()).unwrap();
	seed.define_class(ClassSchema::new("friendship", AtomKind::Edge, []).unwrap()).unwrap();
	let graph = seed.define_graph("social", GraphKind::Graph).unwrap();
	let alice = "person:alice".parse::<RecordId>().unwrap();
	let bob = "person:bob".parse::<RecordId>().unwrap();
	let router = "device:router".parse::<RecordId>().unwrap();
	let friendship = "friendship:shared".parse::<RecordId>().unwrap();
	for record in [&alice, &bob, &router, &friendship] {
		seed.create_atom(record.clone(), Default::default()).unwrap();
	}
	let alice_local =
		seed.attach_node(graph.id(), &alice, Some(Text::from("Employee")), None).unwrap();
	let bob_local = seed.attach_node(graph.id(), &bob, Some(Text::from("Visitor")), None).unwrap();
	seed.attach_node(graph.id(), &router, Some(Text::from("Employee")), None).unwrap();
	seed.attach_edge(
		graph.id(),
		&friendship,
		Some(Text::from("Close")),
		EdgeTopology::directed([alice_local], [bob_local]),
		None,
		None,
	)
	.unwrap();
	seed.commit().unwrap();

	let mut transaction = store.begin();
	assert_eq!(
		matching_ids(&mut transaction, "MATCH (person:alice AS n) IN GRAPH social RETURN n", "n",),
		[alice.clone()]
	);
	assert!(
		matching_ids(
			&mut transaction,
			"MATCH (person:alice AS n LABEL Visitor) IN GRAPH social RETURN n",
			"n",
		)
		.is_empty()
	);
	assert_eq!(
		matching_ids(&mut transaction, "MATCH (person AS n) IN GRAPH social RETURN n", "n"),
		[alice.clone(), bob.clone()]
	);
	assert_eq!(
		matching_ids(
			&mut transaction,
			"MATCH (* AS n LABEL Employee) IN GRAPH social RETURN n",
			"n",
		),
		[router, alice.clone()]
	);
	assert_eq!(
		matching_ids(
			&mut transaction,
			"MATCH (person AS n LABEL Employee) IN GRAPH social RETURN n",
			"n",
		),
		[alice.clone()]
	);
	assert!(
		matching_ids(
			&mut transaction,
			"MATCH (person:alice AS n), (person AS n LABEL Visitor) IN GRAPH social RETURN n",
			"n",
		)
		.is_empty()
	);
	assert_eq!(
		matching_ids(
			&mut transaction,
			"MATCH (person:alice AS a)-[friendship:shared AS e LABEL Close]->(person:bob AS b) IN \
			 GRAPH social RETURN e",
			"e",
		),
		[friendship]
	);

	let legacy = transaction
		.execute("MATCH (person AS n) FROM social RETURN n", &Object::new())
		.unwrap_err();
	assert_eq!(legacy.status().code().to_string(), "FG-42001");
}
