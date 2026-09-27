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
fn hyper_match_distinguishes_arity_sides_and_undirected_endpoints() {
	let store = Datastore::in_memory();
	let mut seed = store.begin();
	seed.define_class(ClassSchema::new("person", AtomKind::Node, []).unwrap()).unwrap();
	seed.define_class(ClassSchema::new("meeting", AtomKind::Edge, []).unwrap()).unwrap();
	let graph = seed.define_graph("hyper_contract", GraphKind::Hypergraph).unwrap();
	let people =
		["person:a", "person:b", "person:c"].map(|value| value.parse::<RecordId>().unwrap());
	let edges =
		["meeting:binary", "meeting:directed", "meeting:bidirectional", "meeting:undirected"]
			.map(|value| value.parse::<RecordId>().unwrap());
	for record in people.iter().chain(&edges) {
		seed.create_atom(record.clone(), PropertyMap::new()).unwrap();
	}
	let local = people
		.iter()
		.map(|record| seed.attach_node(graph.id(), record, None, None).unwrap())
		.collect::<Vec<_>>();
	for (edge, topology) in [
		(&edges[0], EdgeTopology::directed([local[0]], [local[1]])),
		(&edges[1], EdgeTopology::directed([local[0], local[1]], [local[2]])),
		(&edges[2], EdgeTopology::bidirectional([local[0], local[1]], [local[2]])),
		(&edges[3], EdgeTopology::undirected([local[0], local[1], local[2]])),
	] {
		seed.attach_edge(graph.id(), edge, None, topology, None, None).unwrap();
	}
	seed.commit().unwrap();

	let mut transaction = store.begin();
	assert_eq!(
		row_count(
			&mut transaction,
			"MATCH HYPER [meeting AS e] CONTAINS HEAD [(person AS h)], TAIL [(person AS t)] IN \
			 GRAPH hyper_contract RETURN e, h, t",
		),
		4,
		"directed and bidirectional hyperedges each contribute two HEAD-to-TAIL assignments",
	);
	assert_eq!(
		row_count(
			&mut transaction,
			"MATCH HYPER [meeting:undirected AS e] CONTAINS HEAD [(person AS h)] IN GRAPH \
			 hyper_contract RETURN h",
		),
		0,
	);
	assert_eq!(
		row_count(
			&mut transaction,
			"MATCH HYPER [meeting:undirected AS e] CONTAINS [(person AS p)] IN GRAPH \
			 hyper_contract RETURN p",
		),
		3,
	);
	assert_eq!(
		row_count(
			&mut transaction,
			"MATCH HYPER [meeting:binary AS e] CONTAINS [(person AS p)] IN GRAPH hyper_contract \
			 RETURN p",
		),
		0,
		"HYPER excludes binary edges",
	);
	assert_eq!(
		row_count(
			&mut transaction,
			"MATCH DIFFERENT ATOMS HYPER [meeting:directed AS e] CONTAINS HEAD [(person AS a), \
			 (person AS b)] IN GRAPH hyper_contract RETURN a, b",
		),
		2,
		"DIFFERENT ATOMS enforces one-to-one endpoint assignments",
	);
}
