use std::num::NonZeroUsize;

use crate::{
	datastore::{ClassSchema, Datastore},
	gql::{QueryExecutionOptions, QueryStatementKind},
	graph::{AtomKind, AtomRef, EdgeTopology, GraphKind},
	values::{Number, Object, RecordId, Value},
};

fn traverse(source: &str) -> Value {
	let store = Datastore::in_memory();
	let result = store.execute(source, &Object::new()).unwrap();
	let statement = result.last().unwrap();
	assert_eq!(statement.kind, QueryStatementKind::Traverse);
	statement.result.as_ref().unwrap().value().clone()
}

#[test]
fn traverses_collection_with_language_closures() {
	assert_eq!(
		traverse("TRAVERSE FROM [1, 2, 3] |> tv::map(|$x| $x * 2);"),
		Value::Array(vec![2_i64, 4, 6].into())
	);
	assert_eq!(
		traverse(
			"LET $factor = 3; TRAVERSE FROM [1, 2, 3] |> tv::where(|$x| $x > 1) |> tv::map(|$x| \
			 $x * $factor);"
		),
		Value::Array(vec![6_i64, 9].into())
	);
}

#[test]
fn collection_steps_cover_the_public_traversal_adapters() {
	assert_eq!(
		traverse(
			"TRAVERSE FROM [{ k: 1 }, { k: 2 }, { x: 3 }] |> tv::hasattr('k') |> tv::has('k', 2) \
			 |> tv::values('k');"
		),
		Value::Array(vec![2_i64].into())
	);
	assert_eq!(
		traverse("TRAVERSE FROM [[1, 2], [3]] |> tv::unfold();"),
		Value::Array(vec![1_i64, 2, 3].into())
	);
	assert_eq!(
		traverse(
			"TRAVERSE FROM [1] |> tv::as('before') |> tv::map(|$x| $x + 1) |> tv::as('after') |> \
			 tv::project('before', 'after');"
		),
		Value::Array(
			vec![Value::Object(Object::from_iter([
				("before", Value::from(1_i64)),
				("after", Value::from(2_i64)),
			]))]
			.into()
		)
	);
	assert_eq!(
		traverse(
			"TRAVERSE FROM [{ k: 1, v: 'first' }, { k: 1, v: 'second' }] |> tv::dedup_by(|$x| \
			 $x.k) |> tv::values('v');"
		),
		Value::Array(vec![Value::from("first")].into())
	);
	assert_eq!(
		traverse("TRAVERSE FROM [1, 2] |> tv::choose(|$x| $x > 1, |$x| $x * 10, |$x| -$x);"),
		Value::Array(vec![-1_i64, 20].into())
	);
	assert_eq!(
		traverse("TRAVERSE FROM [1] |> tv::coalesce(|$x| none, |$x| $x + 1);"),
		Value::Array(vec![2_i64].into())
	);
	assert_eq!(
		traverse("TRAVERSE FROM [1] |> tv::repeat(|$x| $x + 1).times(2);"),
		Value::Array(vec![3_i64].into())
	);
	assert_eq!(
		traverse("TRAVERSE FROM [1, 1, 1, 2] |> tv::skip(2);"),
		Value::Array(vec![1_i64, 2].into())
	);
	for (step, expected) in [
		("sum", Value::from(6_i64)),
		("min", Value::from(1_i64)),
		("max", Value::from(3_i64)),
		("avg", Value::from(2_i64)),
	] {
		assert_eq!(
			traverse(&format!("TRAVERSE FROM [1, 2, 3] |> tv::{step}();")),
			Value::Array(vec![expected].into())
		);
	}
}

#[test]
fn generic_pipeline_and_block_closure_use_the_query_runtime() {
	let store = Datastore::in_memory();
	let pipeline = store
		.execute("[1, 2, 3] |> tv::map(|$value| $value * 2) |> tv::fold();", &Object::new())
		.unwrap();
	assert_eq!(
		pipeline[0].result.as_ref().unwrap().value(),
		&Value::Array(vec![Value::Array(vec![2_i64, 4, 6].into())].into())
	);
	let closure = store
		.execute(
			"LET $increment = |$value| -> number { LET $next = $value + 1; RETURN $next; }; \
			 $increment(41);",
			&Object::new(),
		)
		.unwrap();
	assert_eq!(closure.last().unwrap().result.as_ref().unwrap().value(), &Value::from(42_i64));
}

#[test]
fn persistent_session_owns_closures_and_transient_execute_rejects_escape() {
	let store = Datastore::in_memory();
	let mut session = store.session();
	session
		.execute("LET $factor = 2; LET $twice = |$value| $value * $factor;", &Object::new())
		.unwrap();
	let result = session.execute("$twice(21);", &Object::new()).unwrap();
	assert_eq!(result[0].result.as_ref().unwrap().value(), &Value::from(42_i64));

	let returned = session.execute("$twice;", &Object::new()).unwrap();
	let closure = returned[0].result.as_ref().unwrap().value().clone();
	let mut other = store.session();
	let mut variables = Object::new();
	variables.insert("foreign", closure);
	let mismatch = other.execute("$foreign(1);", &variables).unwrap();
	assert!(mismatch[0].result.is_err());

	let transient = store.execute("[|$value| $value];", &Object::new()).unwrap();
	assert!(transient[0].result.is_err());
}

#[test]
fn persistent_session_releases_statement_local_closures() {
	let store = Datastore::in_memory();
	let mut session = store.session();
	for _ in 0..32 {
		let result = session
			.execute("TRAVERSE FROM [1, 2] |> tv::map(|$value| $value + 1);", &Object::new())
			.unwrap();
		assert!(result[0].result.is_ok());
		assert_eq!(session.retained_closure_bytes(), 0);
	}
}

#[test]
fn traversal_bulks_and_splits_limit_without_changing_values() {
	assert_eq!(
		traverse("TRAVERSE FROM [1, 1, 1, 2] |> tv::limit(2) |> tv::count();"),
		Value::Array(vec![2_i64].into())
	);
	assert_eq!(
		traverse("TRAVERSE FROM [1] |> tv::map(|$x| math::sqrt(-1)) |> tv::limit(0);"),
		Value::Array(Vec::<Value>::new().into())
	);
}

#[test]
fn sacks_receive_previous_state_before_current_value() {
	assert_eq!(
		traverse(
			"TRAVERSE FROM [2] |> tv::init_sack(10) |> tv::sack(|$sack, $current| $sack - \
			 $current) |> tv::get_sack();"
		),
		Value::Array(vec![8_i64].into())
	);
	assert_eq!(
		traverse(
			"TRAVERSE FROM [2] |> tv::init_named_sack('total', 10) |> tv::named_sack('total', \
			 |$sack, $current| $sack - $current) |> tv::get_named_sack('total');"
		),
		Value::Array(vec![8_i64].into())
	);
	assert!(
		Datastore::in_memory()
			.execute("TRAVERSE FROM [2] |> tv::get_named_sack('missing');", &Object::new())
			.unwrap()[0]
			.result
			.is_err()
	);
}

#[test]
fn atom_source_requires_snapshot_visible_graph_membership() {
	let store = Datastore::in_memory();
	let mut seed = store.begin();
	seed.define_class(ClassSchema::new("person", AtomKind::Node, []).unwrap()).unwrap();
	let graph = seed.define_graph("social", GraphKind::Graph).unwrap();
	seed.commit().unwrap();
	let mut variables = Object::new();
	variables.insert("graph", Value::GraphRef(Box::new(graph)));
	variables.insert(
		"missing",
		Value::Atom(Box::new(AtomRef::new(AtomKind::Node, "person:missing".parse().unwrap()))),
	);
	let result = store
		.execute("TRAVERSE FROM $missing IN GRAPH $graph |> tv::limit(1);", &variables)
		.unwrap();
	assert!(result[0].result.is_err());
}

#[test]
fn fast_traversal_preserves_multiplicity_and_impure_source_order() {
	let store = Datastore::in_memory();
	let results = store
		.execute(
			"BEGIN EXECUTION MODE FAST; TRAVERSE FROM [0] |> tv::branch(|$x| [1, 2], |$x| [1]) |> \
			 tv::side_effect('seen', |$x, $acc| { IF option::is_none($acc) { RETURN $x; }; RETURN \
			 $acc * 10 + $x; }) |> tv::select('seen'); COMMIT;",
			&Object::new(),
		)
		.unwrap();
	let traversal =
		results.iter().find(|result| result.kind == QueryStatementKind::Traverse).unwrap();
	assert_eq!(
		traversal.result.as_ref().unwrap().value(),
		&Value::Array(vec![1_i64, 12, 121].into())
	);
}

#[test]
fn traversal_return_reads_named_bindings_and_side_effect_state() {
	assert_eq!(
		traverse(
			"TRAVERSE FROM [1, 2] |> tv::as('original') |> tv::map(|$value| $value * 10) RETURN \
			 [original, current];"
		),
		Value::Array(
			vec![Value::Array(vec![1_i64, 10].into()), Value::Array(vec![2_i64, 20].into()),]
				.into()
		)
	);
	assert_eq!(
		traverse(
			"TRAVERSE FROM [1, 2] |> tv::side_effect('total', |$value, $total| { IF \
			 option::is_none($total) { RETURN $value; }; RETURN $total + $value; }) RETURN \
			 [current, total];"
		),
		Value::Array(
			vec![Value::Array(vec![1_i64, 1].into()), Value::Array(vec![2_i64, 3].into()),].into()
		)
	);
}

#[test]
fn mutating_side_effect_is_visible_to_downstream_steps() {
	let store = Datastore::in_memory();
	store
		.execute(
			"DEFINE GRAPH social; DEFINE CLASS person TYPE NODE SCHEMAFULL; DEFINE FIELD score ON \
			 CLASS person TYPE int; CREATE person:alice SET score = 1; ATTACH person:alice TO \
			 GRAPH social;",
			&Object::new(),
		)
		.unwrap();
	let graph = store.schema_catalog().named_graph("social").unwrap().reference().clone();
	let alice = "person:alice".parse::<RecordId>().unwrap();
	let mut variables = Object::new();
	variables.insert("graph", Value::GraphRef(Box::new(graph)));
	variables.insert("alice", Value::Record(Box::new(alice)));

	let results = store
		.execute(
			"BEGIN; TRAVERSE FROM $alice IN GRAPH $graph |> tv::side_effect('updated', |$current, \
			 $previous| { UPDATE person:alice SET score = 7; RETURN true; }) |> \
			 tv::values('score'); COMMIT;",
			&variables,
		)
		.unwrap();
	let traversal =
		results.iter().find(|result| result.kind == QueryStatementKind::Traverse).unwrap();
	assert_eq!(traversal.result.as_ref().unwrap().value(), &Value::Array(vec![7_i64].into()));
	assert_eq!(
		store.execute("SELECT VALUE score FROM person:alice;", &Object::new()).unwrap()[0]
			.result
			.as_ref()
			.unwrap()
			.value(),
		&Value::Array(vec![7_i64].into())
	);
}

#[test]
fn failed_language_closure_restores_its_transaction_savepoint() {
	let store = Datastore::in_memory();
	store
		.execute(
			"DEFINE CLASS person TYPE NODE SCHEMAFULL; DEFINE FIELD score ON CLASS person TYPE \
			 int; CREATE person:alice SET score = 1;",
			&Object::new(),
		)
		.unwrap();
	let results = store
		.execute(
			"BEGIN; LET $mutate = |$value| { UPDATE person:alice SET score = 9; THROW 'rollback'; \
			 }; TRY { $mutate(0); } CATCH |$error| { none; }; SELECT VALUE score FROM \
			 person:alice; COMMIT;",
			&Object::new(),
		)
		.unwrap();
	let selected = results.iter().find(|result| result.kind == QueryStatementKind::Select).unwrap();
	assert_eq!(selected.result.as_ref().unwrap().value(), &Value::Array(vec![1_i64].into()));
}

#[test]
fn traversal_order_and_dedup_spill_through_shared_query_runs() {
	let store = Datastore::in_memory();
	let root =
		std::env::temp_dir().join(format!("fg-traversal-spill-{}", crate::values::Uuid::now_v7()));
	let results = store
		.execute_with_options(
			"TRAVERSE FROM [3, 1, 2, 1] |> tv::order() |> tv::dedup();",
			&Object::new(),
			QueryExecutionOptions {
				barrier_memory_bytes: NonZeroUsize::MIN,
				temporary_directory: Some(root.clone()),
				..QueryExecutionOptions::default()
			},
		)
		.unwrap();
	assert_eq!(
		results[0].result.as_ref().unwrap().value(),
		&Value::Array(vec![1_i64, 2, 3].into())
	);
	let query_root = root.join("tmp").join("query");
	assert!(
		!query_root.exists() || std::fs::read_dir(&query_root).unwrap().next().is_none(),
		"completed traversal must remove transient query-run sessions"
	);
	drop(std::fs::remove_dir_all(root));
}

#[test]
fn traversal_frontier_obeys_the_shared_memory_budget() {
	let results = Datastore::in_memory()
		.execute_with_options(
			"TRAVERSE FROM [1] |> tv::flatMap(|$value| [$value, $value + 1]);",
			&Object::new(),
			QueryExecutionOptions {
				barrier_memory_bytes: NonZeroUsize::MIN,
				..QueryExecutionOptions::default()
			},
		)
		.unwrap();
	assert!(results[0].result.is_err());
}

#[test]
fn traversal_group_accepts_a_composite_field_key() {
	let result = traverse(
		"TRAVERSE FROM [{ region: 'eu', tier: 1 }, { region: 'eu', tier: 2 }, { region: 'eu', \
		 tier: 1 }] |> tv::group('region', 'tier');",
	);
	let Value::Array(groups) = result else {
		panic!("group traversal must return an array")
	};
	assert_eq!(groups.len(), 2);
	let counts = groups
		.iter()
		.map(|group| {
			let Value::Object(group) = group else {
				panic!("group traversal must return objects")
			};
			group.get("count").cloned().unwrap_or(Value::None)
		})
		.collect::<Vec<_>>();
	assert_eq!(counts, vec![Value::from(2_i64), Value::from(1_i64)]);
}

#[test]
fn repeat_propagates_until_errors() {
	let store = Datastore::in_memory();
	let results = store
		.execute(
			"TRAVERSE FROM [1] |> tv::repeat(|$x| $x + 1).until(|$x| math::sqrt(-1));",
			&Object::new(),
		)
		.unwrap();
	assert!(results[0].result.is_err());
}

#[test]
fn traversal_uses_the_common_statement_timeout() {
	let store = Datastore::in_memory();
	let results = store
		.execute("TRAVERSE FROM [1] |> tv::map(|$value| $value) TIMEOUT 0ms;", &Object::new())
		.unwrap();
	assert!(results[0].result.is_err());
}

#[test]
fn navigates_occurrences_and_resolves_fields_inside_closures() {
	let store = Datastore::in_memory();
	let mut seed = store.begin();
	seed.define_class(ClassSchema::new("person", AtomKind::Node, []).unwrap()).unwrap();
	seed.define_class(ClassSchema::new("knows", AtomKind::Edge, []).unwrap()).unwrap();
	let graph = seed.define_graph("social", GraphKind::Graph).unwrap();
	let alice = "person:alice".parse::<RecordId>().unwrap();
	let bob = "person:bob".parse::<RecordId>().unwrap();
	let knows = "knows:alice-bob".parse::<RecordId>().unwrap();
	seed.create_atom(alice.clone(), Default::default()).unwrap();
	seed.create_atom(bob.clone(), Default::default()).unwrap();
	seed.create_atom(knows.clone(), Default::default()).unwrap();
	let alice_local = seed.attach_node(graph.id(), &alice, Some("Employee".into()), None).unwrap();
	let bob_local = seed.attach_node(graph.id(), &bob, Some("Employee".into()), None).unwrap();
	seed.attach_edge(
		graph.id(),
		&knows,
		Some("Friend".into()),
		EdgeTopology::directed([alice_local], [bob_local]),
		None,
		None,
	)
	.unwrap();
	seed.commit().unwrap();

	let mut variables = Object::new();
	variables.insert("graph", Value::GraphRef(Box::new(graph)));
	variables.insert("alice", Value::Record(Box::new(alice.clone())));
	let result = store
		.execute(
			"TRAVERSE FROM $graph |> tv::V() |> tv::where(|$x| $x.label = 'Employee') |> \
			 tv::out() |> tv::values('id');",
			&variables,
		)
		.unwrap();
	assert_eq!(
		result[0].result.as_ref().unwrap().value(),
		&Value::Array(vec![Value::Record(Box::new(bob.clone()))].into())
	);

	let path = store
		.execute(
			"TRAVERSE FROM $alice IN GRAPH $graph |> tv::outE() |> tv::inV() |> tv::path();",
			&variables,
		)
		.unwrap();
	let Value::Array(paths) = path[0].result.as_ref().unwrap().value() else {
		panic!("path traversal must return an array")
	};
	let Some(Value::Path(path)) = paths.iter().next() else {
		panic!("path traversal must return one path")
	};
	assert_eq!(path.steps().len(), 2);
	assert_eq!(path.steps()[0].atom().atom().id(), &alice);
	assert_eq!(path.steps()[1].atom().atom().id(), &bob);

	let matched = store
		.execute(
			"TRAVERSE FROM $graph |> tv::V() |> tv::as('p') |> tv::match(MATCH (person AS \
			 p)-[knows]->(person AS q) IN GRAPH $graph RETURN p, q) |> tv::select('q') |> \
			 tv::values('id');",
			&variables,
		)
		.unwrap();
	assert_eq!(
		matched[0].result.as_ref().unwrap().value(),
		&Value::Array(vec![Value::Record(Box::new(bob))].into())
	);
}

#[test]
fn traversal_algorithms_reuse_exact_graph_core() {
	let store = Datastore::in_memory();
	let mut seed = store.begin();
	seed.define_class(ClassSchema::new("person", AtomKind::Node, []).unwrap()).unwrap();
	seed.define_class(ClassSchema::new("road", AtomKind::Edge, []).unwrap()).unwrap();
	let graph = seed.define_graph("routes", GraphKind::Graph).unwrap();
	let records =
		["person:a", "person:b", "person:c"].map(|value| value.parse::<RecordId>().unwrap());
	let edges = ["road:a-b", "road:b-c"].map(|value| value.parse::<RecordId>().unwrap());
	for record in records.iter().chain(edges.iter()) {
		seed.create_atom(record.clone(), Default::default()).unwrap();
	}
	let a = seed.attach_node(graph.id(), &records[0], None, None).unwrap();
	let b = seed.attach_node(graph.id(), &records[1], None, None).unwrap();
	let c = seed.attach_node(graph.id(), &records[2], None, None).unwrap();
	seed.attach_edge(
		graph.id(),
		&edges[0],
		None,
		EdgeTopology::directed([a], [b]),
		Some(Number::Int(2)),
		None,
	)
	.unwrap();
	seed.attach_edge(
		graph.id(),
		&edges[1],
		None,
		EdgeTopology::directed([b], [c]),
		Some(Number::Int(3)),
		None,
	)
	.unwrap();
	seed.commit().unwrap();

	let mut variables = Object::new();
	variables.insert("graph", Value::GraphRef(Box::new(graph)));
	variables.insert("source", Value::Record(Box::new(records[0].clone())));
	variables
		.insert("target", Value::Atom(Box::new(AtomRef::new(AtomKind::Node, records[2].clone()))));
	let result = store
		.execute("TRAVERSE FROM $source IN GRAPH $graph |> tv::distanceTo($target);", &variables)
		.unwrap();
	assert_eq!(
		result[0].result.as_ref().unwrap().value(),
		&Value::Array(vec![Value::Number(Number::Int(5))].into())
	);

	let result = store
		.execute("TRAVERSE FROM $graph |> tv::V() |> tv::topologicalSort();", &variables)
		.unwrap();
	let Value::Array(rows) = result[0].result.as_ref().unwrap().value() else {
		panic!("topological traversal must return an array")
	};
	let Some(Value::Array(order)) = rows.iter().next() else {
		panic!("topological traversal must return one ordered array")
	};
	assert_eq!(order.len(), 3);

	let result = store
		.execute(
			"TRAVERSE FROM $graph |> tv::V() |> tv::stronglyConnectedComponents();",
			&variables,
		)
		.unwrap();
	let Value::Array(rows) = result[0].result.as_ref().unwrap().value() else {
		panic!("SCC traversal must return an array")
	};
	let Some(Value::Array(components)) = rows.iter().next() else {
		panic!("SCC traversal must return one component array")
	};
	assert_eq!(components.len(), 3);

	for query in [
		"TRAVERSE FROM $source IN GRAPH $graph |> tv::kHop(2);",
		"TRAVERSE FROM $source IN GRAPH $graph |> tv::reachable($target);",
		"TRAVERSE FROM $source IN GRAPH $graph |> tv::shortestPath($target);",
		"TRAVERSE FROM $source IN GRAPH $graph |> tv::weightedShortestPath($target);",
		"TRAVERSE FROM $source IN GRAPH $graph |> tv::bellmanFord($target);",
		"TRAVERSE FROM $graph |> tv::V() |> tv::weaklyConnectedComponents();",
		"TRAVERSE FROM $graph |> tv::V() |> tv::condensation();",
		"TRAVERSE FROM $graph |> tv::V() |> tv::minimumSpanningForest();",
		"TRAVERSE FROM $graph |> tv::V() |> tv::findCuts();",
		"TRAVERSE FROM $graph |> tv::V() |> tv::hasCycle();",
	] {
		let results = store.execute(query, &variables).unwrap();
		assert!(results[0].result.is_ok(), "traversal adapter failed: {query}");
	}
}
