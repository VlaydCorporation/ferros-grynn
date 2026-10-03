//! ALL eligibility, exact Number folds and statement-wide resource limits.

use crate::{
	datastore::{ClassSchema, Datastore},
	gql::QueryExecutionOptions,
	graph::{AtomKind, EdgeTopology, GraphKind},
	values::{Number, Object, PropertyMap, RecordId, Value},
};

/// Builds one mixed-numeric route and a shorter connector excluded by labels.
fn store() -> Datastore {
	let store = Datastore::in_memory();
	let mut tx = store.begin();
	tx.define_class(ClassSchema::new("n", AtomKind::Node, []).unwrap()).unwrap();
	tx.define_class(ClassSchema::new("e", AtomKind::Edge, []).unwrap()).unwrap();
	let graph = tx.define_graph("routes", GraphKind::Graph).unwrap();
	let fee = tx.intern_property("fee").unwrap();
	let mut nodes = Vec::new();
	for name in ["a", "b", "c", "d", "z"] {
		let record: RecordId = format!("n:{name}").parse().unwrap();
		tx.create_atom(record.clone(), PropertyMap::new()).unwrap();
		nodes.push(tx.attach_node(graph.id(), &record, None, None).unwrap());
	}
	for (name, from, to, weight, label) in [
		("lead", 0, 1, Number::Int((1_i64 << 60) + 128), "allowed"),
		("middle1", 1, 2, Number::Int(1), "allowed"),
		("middle2", 2, 3, Number::Float(0.0), "allowed"),
		("tail", 3, 4, Number::Int(0), "allowed"),
		("excluded", 1, 3, Number::Int(-1), "blocked"),
	] {
		let record: RecordId = format!("e:{name}").parse().unwrap();
		tx.create_atom(record.clone(), PropertyMap::new()).unwrap();
		tx.set_base_property(&record, fee, Value::Number(weight)).unwrap();
		tx.attach_edge(
			graph.id(),
			&record,
			Some(label.into()),
			EdgeTopology::directed([nodes[from]], [nodes[to]]),
			Some(weight),
			None,
		)
		.unwrap();
	}
	tx.commit().unwrap();
	for outcome in store
		.execute(
			"DEFINE PURE COST FUNCTION fn::fee(edge:any)->number { RETURN <number>$edge.fee; }; \
			 DEFINE PURE COST FUNCTION fn::slow(edge:any)->number { FOR $i IN 0..10000 { $i; }; \
			 RETURN 1; };",
			&Object::new(),
		)
		.unwrap()
	{
		assert!(outcome.result.is_ok(), "{:?}", outcome.result);
	}
	store
}

#[test]
fn tensor_paths_filter_before_minimum_and_preserve_named_number_order() {
	let store = store();
	let query = "MATCH path=(n:b)-[e LABEL allowed*1..2 ALL SHORTEST]->(n:d) IN GRAPH routes \
	             RETURN path::length(path) AS length;";
	let result = store.execute(query, &Object::new()).unwrap();
	let rows = result[0].result.as_ref().unwrap().value().as_array().unwrap();
	assert_eq!(rows.len(), 1);
	assert_eq!(rows.get(0).unwrap().as_object().unwrap().get("length"), Some(&Value::from(2_i64)));
	let weighted = query.replace("ALL SHORTEST", "ALL WEIGHTED SHORTEST");
	assert!(store.execute(&weighted, &Object::new()).unwrap()[0].result.is_ok());
	let named = "MATCH path=(n:a)-[e:lead]->(n:b)-[e LABEL allowed*2 ALL WEIGHTED \
	             SHORTEST]->(n:d)-[e:tail]->(n:z) IN GRAPH routes WITH COST fn::fee RETURN \
	             path::cost(path) AS cost;";
	let result = store.execute(named, &Object::new()).unwrap();
	let rows = result[0].result.as_ref().unwrap().value().as_array().unwrap();
	assert_eq!(rows.len(), 1);
	assert_eq!(
		rows.get(0).unwrap().as_object().unwrap().get("cost"),
		Some(&Value::Number(Number::Float(((1_i64 << 60) + 256) as f64)))
	);
}

#[test]
fn tensor_paths_bound_cost_and_logical_seed_enumeration_before_output() {
	let store = store();
	let options = QueryExecutionOptions {
		graph_algorithm_limits: crate::graph::tensor::GraphAlgorithmLimits {
			max_paths: 1,
			..Default::default()
		},
		..Default::default()
	};
	let query = "TRAVERSE FROM [n:b,n:b] IN GRAPH @routes |> \
	             tv::allShortestPaths(n:d,{labels:['allowed']});";
	assert!(store.execute_with_options(query, &Object::new(), options).unwrap()[0].result.is_err());
	let options = QueryExecutionOptions {
		graph_algorithm_limits: crate::graph::tensor::GraphAlgorithmLimits {
			max_work: 1000,
			..Default::default()
		},
		..Default::default()
	};
	let query = "MATCH path=(n:b)-[e LABEL allowed*1..2 ALL WEIGHTED SHORTEST COST \
	             fn::slow]->(n:d) IN GRAPH routes RETURN path;";
	assert!(store.execute_with_options(query, &Object::new(), options).unwrap()[0].result.is_err());
}
