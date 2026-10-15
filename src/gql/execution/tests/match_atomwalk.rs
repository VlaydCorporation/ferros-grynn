#[cfg(test)]
use crate::test_assertions::AssertOption as _;
#[cfg(test)]
use crate::test_assertions::AssertResult as _;
use crate::{
	datastore::{ClassSchema, Datastore, Transaction},
	graph::{AtomKind, EdgeTopology, GraphKind},
	values::{Object, PropertyMap, RecordId},
};

/// Returns row count for this module.
fn row_count(transaction: &mut Transaction, source: &str) -> usize {
	transaction.execute(source, &Object::new()).assert_ok()[0]
		.result
		.as_ref()
		.assert_ok()
		.value()
		.as_array()
		.assert_some()
		.len()
}

/// Verifies atomwalk respects all incidence directions and topology kinds.
#[test]
fn atomwalk_respects_all_incidence_directions_and_topology_kinds() {
	let store = Datastore::in_memory();
	let mut seed = store.begin();
	seed.define_class(ClassSchema::new("person", AtomKind::Node, []).assert_ok()).assert_ok();
	seed.define_class(ClassSchema::new("relation", AtomKind::Edge, []).assert_ok()).assert_ok();
	let graph = seed.define_graph("atomwalk_directions", GraphKind::Graph).assert_ok();
	let a = "person:a".parse::<RecordId>().assert_ok();
	let b = "person:b".parse::<RecordId>().assert_ok();
	let directed = "relation:directed".parse::<RecordId>().assert_ok();
	let bidirectional = "relation:bidirectional".parse::<RecordId>().assert_ok();
	let undirected = "relation:undirected".parse::<RecordId>().assert_ok();
	for record in [&a, &b, &directed, &bidirectional, &undirected] {
		seed.create_atom(record.clone(), PropertyMap::new()).assert_ok();
	}
	let a_local = seed.attach_node(graph.id(), &a, None, None).assert_ok();
	let b_local = seed.attach_node(graph.id(), &b, None, None).assert_ok();
	seed.attach_edge(
		graph.id(),
		&directed,
		None,
		EdgeTopology::directed([a_local], [b_local]),
		None,
		None,
	)
	.assert_ok();
	seed.attach_edge(
		graph.id(),
		&bidirectional,
		None,
		EdgeTopology::bidirectional([a_local], [b_local]),
		None,
		None,
	)
	.assert_ok();
	seed.attach_edge(
		graph.id(),
		&undirected,
		None,
		EdgeTopology::undirected([a_local, b_local]),
		None,
		None,
	)
	.assert_ok();
	seed.commit().assert_ok();

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
