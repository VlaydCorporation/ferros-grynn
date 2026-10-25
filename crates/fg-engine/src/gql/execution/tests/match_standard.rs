use std::num::NonZeroUsize;

#[cfg(test)]
use crate::test_assertions::AssertOption as _;
#[cfg(test)]
use crate::test_assertions::AssertResult as _;
use crate::{
	datastore::{ClassField, ClassSchema, Datastore, Transaction},
	gql::QueryExecutionOptions,
	graph::{AtomKind, EdgeTopology, GraphKind},
	values::{Number, Object, PropertyMap, RecordId, Type, Value},
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

/// Verifies standard match respects topology directions bounds and inline
/// predicates.
#[test]
fn standard_match_respects_topology_directions_bounds_and_inline_predicates() {
	let store = Datastore::in_memory();
	let mut schema = store.begin();
	schema
		.define_class(
			ClassSchema::new("person", AtomKind::Node, [
				ClassField::new("active", Type::Bool),
				ClassField::new("nickname", Type::String),
			])
			.assert_ok(),
		)
		.assert_ok();
	schema.define_class(ClassSchema::new("link", AtomKind::Edge, []).assert_ok()).assert_ok();
	schema.commit().assert_ok();
	let active = store.property_dictionary().get("active").assert_some();
	let nickname = store.property_dictionary().get("nickname").assert_some();

	let mut seed = store.begin();
	let graph = seed.define_graph("directions", GraphKind::Graph).assert_ok();
	let a = "person:a".parse::<RecordId>().assert_ok();
	let b = "person:b".parse::<RecordId>().assert_ok();
	let directed = "link:directed".parse::<RecordId>().assert_ok();
	let bidirectional = "link:bidirectional".parse::<RecordId>().assert_ok();
	let undirected = "link:undirected".parse::<RecordId>().assert_ok();
	let loop_edge = "link:loop".parse::<RecordId>().assert_ok();
	let mut a_properties = PropertyMap::new();
	a_properties.insert(active, Value::Bool(true));
	a_properties.insert(nickname, Value::Null);
	seed.create_atom(a.clone(), a_properties).assert_ok();
	seed.create_atom(b.clone(), PropertyMap::new()).assert_ok();
	for edge in [&directed, &bidirectional, &undirected, &loop_edge] {
		seed.create_atom(edge.clone(), PropertyMap::new()).assert_ok();
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
	seed.attach_edge(
		graph.id(),
		&loop_edge,
		None,
		EdgeTopology::directed([a_local], [a_local]),
		None,
		None,
	)
	.assert_ok();
	seed.commit().assert_ok();

	let mut transaction = store.begin();
	for (query, expected) in [
		("MATCH (person:a AS a)-[link:directed]->(person:b AS b) IN GRAPH directions RETURN b", 1),
		("MATCH (person:b AS b)<-[link:directed]-(person:a AS a) IN GRAPH directions RETURN a", 1),
		(
			"MATCH (person:a AS a)<-[link:bidirectional]->(person:b AS b) IN GRAPH directions \
			 RETURN b",
			1,
		),
		(
			"MATCH (person:b AS b)<-[link:bidirectional]->(person:a AS a) IN GRAPH directions \
			 RETURN a",
			1,
		),
		("MATCH (person:a AS a)-[link:undirected]-(person:b AS b) IN GRAPH directions RETURN b", 1),
		("MATCH (person:a AS a)-[link:directed]-(person:b AS b) IN GRAPH directions RETURN b", 0),
		("MATCH (person:a AS a)<-[link:directed]->(person:b AS b) IN GRAPH directions RETURN b", 0),
	] {
		assert_eq!(row_count(&mut transaction, query), expected, "{query}");
	}

	assert_eq!(
		row_count(
			&mut transaction,
			"MATCH (person AS p { active: true, nickname: null } WHERE p.active) IN GRAPH \
			 directions RETURN p",
		),
		1,
	);
	assert_eq!(
		row_count(
			&mut transaction,
			"MATCH (person AS p { nickname: null }) IN GRAPH directions RETURN p",
		),
		1,
		"a missing property must not equal stored null",
	);
	assert_eq!(
		row_count(
			&mut transaction,
			"MATCH path = (person:a AS a)-[link*0..0]->(person:a AS b) IN GRAPH directions RETURN \
			 path::length(path) AS length",
		),
		1,
	);
	assert_eq!(
		row_count(
			&mut transaction,
			"MATCH path = (person:a AS a)-[link*1..]->(person:b AS b) IN GRAPH directions \
			 MAXDEPTH 1 RETURN path",
		),
		2,
		"directed and bidirectional connectors both satisfy forward traversal",
	);
	assert_eq!(
		row_count(
			&mut transaction,
			"MATCH REPEATABLE ELEMENTS CONSTRAINTS <TRAIL> path = (person:a AS a)-[link:loop*2 \
			 WALK]->(person:a AS b) IN GRAPH directions RETURN path",
		),
		1,
		"a local path mode replaces the global mode on its own axis",
	);
	assert_eq!(
		row_count(
			&mut transaction,
			"MATCH REPEATABLE ELEMENTS CONSTRAINTS <TRAIL> path = (person:a AS \
			 a)-[link:loop*2]->(person:a AS b) IN GRAPH directions RETURN path",
		),
		0,
		"the inherited TRAIL mode rejects reuse of the connector",
	);
	let late_alias = transaction
		.execute(
			"MATCH (person:a AS a WHERE b.active)-[link]->(person:b AS b) IN GRAPH directions \
			 RETURN a",
			&Object::new(),
		)
		.assert_ok();
	assert!(late_alias[0].result.is_err());
	let too_deep = transaction
		.execute(
			"MATCH (person:a AS a)-[link*]->(person:b AS b) IN GRAPH directions MAXDEPTH 31 \
			 RETURN b",
			&Object::new(),
		)
		.assert_ok();
	assert!(too_deep[0].result.is_err());
	let inverted = transaction
		.execute(
			"MATCH (person:a AS a)-[link*3..2]->(person:b AS b) IN GRAPH directions RETURN b",
			&Object::new(),
		)
		.assert_ok();
	assert!(inverted[0].result.is_err());
}

/// Verifies weighted shortest validates weights and is canonical.
#[test]
fn weighted_shortest_validates_weights_and_is_canonical() {
	let store = Datastore::in_memory();
	let mut seed = store.begin();
	seed.define_class(ClassSchema::new("person", AtomKind::Node, []).assert_ok()).assert_ok();
	seed.define_class(ClassSchema::new("link", AtomKind::Edge, []).assert_ok()).assert_ok();
	seed.define_class(ClassSchema::new("bad_link", AtomKind::Edge, []).assert_ok()).assert_ok();
	let graph = seed.define_graph("weights", GraphKind::Graph).assert_ok();
	let nodes = ["person:a", "person:b", "person:c", "person:d"]
		.map(|value| value.parse::<RecordId>().assert_ok());
	let edges =
		["link:ab", "link:bd", "link:ac", "link:cd", "bad_link:negative", "bad_link:infinite"]
			.map(|value| value.parse::<RecordId>().assert_ok());
	for record in nodes.iter().chain(&edges) {
		seed.create_atom(record.clone(), PropertyMap::new()).assert_ok();
	}
	let local = nodes
		.iter()
		.map(|record| seed.attach_node(graph.id(), record, None, None).assert_ok())
		.collect::<Vec<_>>();
	for (edge, source, target, weight) in [
		(&edges[0], local[0], local[1], None),
		(&edges[1], local[1], local[3], Some(Number::Int(1))),
		(&edges[2], local[0], local[2], Some(Number::Int(1))),
		(&edges[3], local[2], local[3], Some(Number::Int(1))),
		(&edges[4], local[0], local[3], Some(Number::Int(-1))),
		(&edges[5], local[0], local[3], Some(Number::Float(f64::INFINITY))),
	] {
		seed.attach_edge(
			graph.id(),
			edge,
			None,
			EdgeTopology::directed([source], [target]),
			weight,
			None,
		)
		.assert_ok();
	}
	seed.commit().assert_ok();

	let mut transaction = store.begin();
	let canonical = "MATCH path = (person:a AS a)-[link*2 WEIGHTED SHORTEST]->(person:d AS d) IN \
	                 GRAPH weights RETURN path::atoms(path) AS atoms, path::cost(path) AS cost";
	for _ in 0..20 {
		let results = transaction.execute(canonical, &Object::new()).assert_ok();
		let row = results[0]
			.result
			.as_ref()
			.assert_ok()
			.value()
			.as_array()
			.assert_some()
			.get(0)
			.assert_some()
			.as_object()
			.assert_some();
		assert_eq!(row.get("cost"), Some(&Value::from(2_i64)));
		assert_eq!(
			row.get("atoms")
				.and_then(Value::as_array)
				.assert_some()
				.get(2)
				.assert_some()
				.as_atom_occurrence()
				.assert_some()
				.atom()
				.id(),
			&nodes[1],
		);
	}
	for edge in ["negative", "infinite"] {
		let query = format!(
			"MATCH path = (person:a AS a)-[bad_link:{edge}*1 WEIGHTED SHORTEST]->(person:d AS d) \
			 IN GRAPH weights RETURN path"
		);
		let result = transaction.execute(&query, &Object::new()).assert_ok();
		assert!(result[0].result.is_err(), "{edge}");
	}

	let options = QueryExecutionOptions {
		barrier_memory_bytes: NonZeroUsize::MIN,
		..QueryExecutionOptions::default()
	};
	let result = transaction.execute_with_options(canonical, &Object::new(), options).assert_ok();
	assert!(result[0].result.is_err(), "named path trace must obey the shared memory budget");
}

/// Verifies serializable match scan tracks graph membership phantoms.
#[test]
fn serializable_match_scan_tracks_graph_membership_phantoms() {
	let store = Datastore::in_memory();
	let mut seed = store.begin();
	seed.define_class(ClassSchema::new("person", AtomKind::Node, []).assert_ok()).assert_ok();
	let graph = seed.define_graph("match_ssi", GraphKind::Graph).assert_ok();
	let first = "person:first".parse::<RecordId>().assert_ok();
	seed.create_atom(first.clone(), PropertyMap::new()).assert_ok();
	seed.attach_node(graph.id(), &first, None, None).assert_ok();
	seed.commit().assert_ok();

	let mut reader = store.begin();
	assert_eq!(row_count(&mut reader, "MATCH (person AS p) IN GRAPH match_ssi RETURN p"), 1,);
	let mut writer = store.begin();
	let phantom = "person:phantom".parse::<RecordId>().assert_ok();
	writer.create_atom(phantom.clone(), PropertyMap::new()).assert_ok();
	writer.attach_node(graph.id(), &phantom, None, None).assert_ok();
	writer.commit().assert_ok();

	reader.create_atom("person:reader".parse().assert_ok(), PropertyMap::new()).assert_ok();
	assert_eq!(reader.commit().assert_err().status().code().to_string(), "FG-40001");
	assert!(store.begin().atom(&"person:reader".parse().assert_ok()).assert_ok().is_none());
}
