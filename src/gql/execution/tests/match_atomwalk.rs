use crate::{
	datastore::{ClassSchema, Datastore, Transaction},
	graph::{AtomKind, EdgeTopology, GraphKind},
	values::{Object, PropertyMap, RecordId},
};

fn row_count(transaction: &mut Transaction, source: &str) -> usize {
	transaction.execute(source, &Object::new()).unwrap()[0]
		.result
		.as_ref()
		.unwrap()
		.value()
		.as_array()
		.unwrap()
		.len()
}

#[test]
fn atomwalk_respects_all_incidence_directions_and_topology_kinds() {
	let store = Datastore::in_memory();
	let mut seed = store.begin();
	seed.define_class(ClassSchema::new("person", AtomKind::Node, []).unwrap()).unwrap();
	seed.define_class(ClassSchema::new("relation", AtomKind::Edge, []).unwrap()).unwrap();
	let graph = seed.define_graph("atomwalk_directions", GraphKind::Graph).unwrap();
	let a = "person:a".parse::<RecordId>().unwrap();
	let b = "person:b".parse::<RecordId>().unwrap();
	let directed = "relation:directed".parse::<RecordId>().unwrap();
	let bidirectional = "relation:bidirectional".parse::<RecordId>().unwrap();
	let undirected = "relation:undirected".parse::<RecordId>().unwrap();
	for record in [&a, &b, &directed, &bidirectional, &undirected] {
		seed.create_atom(record.clone(), PropertyMap::new()).unwrap();
	}
	let a_local = seed.attach_node(graph.id(), &a, None, None).unwrap();
	let b_local = seed.attach_node(graph.id(), &b, None, None).unwrap();
	seed.attach_edge(
		graph.id(),
		&directed,
		None,
		EdgeTopology::directed([a_local], [b_local]),
		None,
		None,
	)
	.unwrap();
	seed.attach_edge(
		graph.id(),
		&bidirectional,
		None,
		EdgeTopology::bidirectional([a_local], [b_local]),
		None,
		None,
	)
	.unwrap();
	seed.attach_edge(
		graph.id(),
		&undirected,
		None,
		EdgeTopology::undirected([a_local, b_local]),
		None,
		None,
	)
	.unwrap();
	seed.commit().unwrap();

	let mut transaction = store.begin();
	for (query, expected) in [
		(
			"MATCH ATOM <(person:a AS a)>~[relation:directed]~><(person:b AS b)> IN GRAPH \
			 atomwalk_directions RETURN b",
			1,
		),
		(
			"MATCH ATOM <(person:b AS b)><~[relation:directed]~<(person:a AS a)> IN GRAPH \
			 atomwalk_directions RETURN a",
			1,
		),
		(
			"MATCH ATOM <(person:a AS a)><~[relation:bidirectional]~><(person:b AS b)> IN GRAPH \
			 atomwalk_directions RETURN b",
			1,
		),
		(
			"MATCH ATOM <(person:b AS b)><~[relation:bidirectional]~><(person:a AS a)> IN GRAPH \
			 atomwalk_directions RETURN a",
			1,
		),
		(
			"MATCH ATOM <(person:a AS a)>~[relation:undirected]~<(person:b AS b)> IN GRAPH \
			 atomwalk_directions RETURN b",
			1,
		),
		(
			"MATCH ATOM <(person:a AS a)>~[relation:directed]~<(person:b AS b)> IN GRAPH \
			 atomwalk_directions RETURN b",
			0,
		),
	] {
		assert_eq!(row_count(&mut transaction, query), expected, "{query}");
	}
}
