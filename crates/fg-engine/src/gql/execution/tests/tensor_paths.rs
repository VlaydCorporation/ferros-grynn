//! ALL eligibility, exact Number folds and statement-wide resource limits.

#[cfg(test)]
use crate::test_assertions::AssertOption as _;
#[cfg(test)]
use crate::test_assertions::AssertResult as _;
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
	tx.define_class(ClassSchema::new("n", AtomKind::Node, []).assert_ok()).assert_ok();
	tx.define_class(ClassSchema::new("e", AtomKind::Edge, []).assert_ok()).assert_ok();
	let graph = tx.define_graph("routes", GraphKind::Graph).assert_ok();
	let fee = tx.intern_property("fee").assert_ok();
	let mut nodes = Vec::new();
	for name in ["a", "b", "c", "d", "z"] {
		let record: RecordId = format!("n:{name}").parse().assert_ok();
		tx.create_atom(record.clone(), PropertyMap::new()).assert_ok();
		nodes.push(tx.attach_node(graph.id(), &record, None, None).assert_ok());
	}
	for (name, from, to, weight, label) in [
		("lead", 0, 1, Number::Int((1_i64 << 60) + 128), "allowed"),
		("middle1", 1, 2, Number::Int(1), "allowed"),
		("middle2", 2, 3, Number::Float(0.0), "allowed"),
		("tail", 3, 4, Number::Int(0), "allowed"),
		("excluded", 1, 3, Number::Int(-1), "blocked"),
	] {
		let record: RecordId = format!("e:{name}").parse().assert_ok();
		tx.create_atom(record.clone(), PropertyMap::new()).assert_ok();
		tx.set_base_property(&record, fee, Value::Number(weight)).assert_ok();
		tx.attach_edge(
			graph.id(),
			&record,
			Some(label.into()),
			EdgeTopology::directed([nodes[from]], [nodes[to]]),
			Some(weight),
			None,
		)
		.assert_ok();
	}
	tx.commit().assert_ok();
	for outcome in store
		.execute(
			"DEFINE PURE COST FUNCTION fn::fee(edge:any)->number { RETURN <number>$edge.fee; }; \
			 DEFINE PURE COST FUNCTION fn::slow(edge:any)->number { FOR $i IN 0..10000 { $i; }; \
			 RETURN 1; };",
			&Object::new(),
		)
		.assert_ok()
	{
		assert!(outcome.result.is_ok(), "{:?}", outcome.result);
	}
	store
}

/// Verifies tensor paths filter before minimum and preserve named number order.
#[test]
fn tensor_paths_filter_before_minimum_and_preserve_named_number_order() {
	let store = store();
	let query = "MATCH path=(n:b)-[e LABEL allowed*1..2 ALL SHORTEST]->(n:d) IN GRAPH routes \
	             RETURN path::length(path) AS length;";
	let result = store.execute(query, &Object::new()).assert_ok();
	let rows = result[0].result.as_ref().assert_ok().value().as_array().assert_some();
	assert_eq!(rows.len(), 1);
	assert_eq!(
		rows.get(0).assert_some().as_object().assert_some().get("length"),
		Some(&Value::from(2_i64))
	);
	let weighted = query.replace("ALL SHORTEST", "ALL WEIGHTED SHORTEST");
	assert!(store.execute(&weighted, &Object::new()).assert_ok()[0].result.is_ok());
	let named = "MATCH path=(n:a)-[e:lead]->(n:b)-[e LABEL allowed*2 ALL WEIGHTED \
	             SHORTEST]->(n:d)-[e:tail]->(n:z) IN GRAPH routes WITH COST fn::fee RETURN \
	             path::cost(path) AS cost;";
	let result = store.execute(named, &Object::new()).assert_ok();
	let rows = result[0].result.as_ref().assert_ok().value().as_array().assert_some();
	assert_eq!(rows.len(), 1);
	assert_eq!(
		rows.get(0).assert_some().as_object().assert_some().get("cost"),
		Some(&Value::Number(Number::Float(((1_i64 << 60) + 256) as f64)))
	);
}

/// Verifies tensor paths bound cost and logical seed enumeration before output.
#[test]
fn tensor_paths_bound_cost_and_logical_seed_enumeration_before_output() {
	let store = store();
	let options = QueryExecutionOptions {
		graph_algorithm_limits: {
			let split_field_0 = 1;
			let mut split_options: crate::graph::tensor::GraphAlgorithmLimits =
				crate::graph::tensor::GraphAlgorithmLimits::default();
			split_options.max_paths = split_field_0;
			split_options
		},
		..Default::default()
	};
	let query = "TRAVERSE FROM [n:b,n:b] IN GRAPH @routes |> \
	             tv::allShortestPaths(n:d,{labels:['allowed']});";
	assert!(
		store.execute_with_options(query, &Object::new(), options).assert_ok()[0].result.is_err()
	);
	let options = QueryExecutionOptions {
		graph_algorithm_limits: {
			let split_field_0 = 1000;
			let mut split_options: crate::graph::tensor::GraphAlgorithmLimits =
				crate::graph::tensor::GraphAlgorithmLimits::default();
			split_options.max_work = split_field_0;
			split_options
		},
		..Default::default()
	};
	let query = "MATCH path=(n:b)-[e LABEL allowed*1..2 ALL WEIGHTED SHORTEST COST \
	             fn::slow]->(n:d) IN GRAPH routes RETURN path;";
	assert!(
		store.execute_with_options(query, &Object::new(), options).assert_ok()[0].result.is_err()
	);
}
