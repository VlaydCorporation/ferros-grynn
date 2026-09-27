use crate::{
	datastore::{ClassSchema, Datastore},
	gql::QueryStatementKind,
	graph::{AtomKind, EdgeTopology, GraphKind},
	values::{Number, Object, RecordId, Value},
};

fn atom_exists(store: &Datastore, record: &str) -> bool {
	let mut transaction = store.begin();
	transaction.atom(&record.parse::<RecordId>().unwrap()).unwrap().is_some()
}

fn json_result(store: &Datastore, source: &str) -> Object {
	let mut results = store.execute(source, &Object::new()).unwrap();
	assert_eq!(results.len(), 1);
	assert_eq!(results[0].kind, QueryStatementKind::Explain);
	let outcome = results.remove(0).result.unwrap();
	let Value::Object(value) = outcome.into_parts().0 else {
		panic!("FORMAT JSON must return an object");
	};
	value
}

#[test]
fn explain_plans_without_effect_and_analyze_executes_once() {
	let store = Datastore::in_memory();
	store
		.execute(
			"DEFINE CLASS person TYPE NODE SCHEMAFULL; DEFINE FIELD age ON CLASS person TYPE int; \
			 CREATE person:alice SET age = 30; DEFINE INDEX by_age ON CLASS person FIELDS age \
			 SCOPE BASE;",
			&Object::new(),
		)
		.unwrap();

	let logical = store
		.execute("EXPLAIN SELECT id FROM person WHERE properties.age = 30;", &Object::new())
		.unwrap()
		.remove(0)
		.result
		.unwrap()
		.into_parts()
		.0;
	assert!(
		matches!(logical, Value::String(value) if value.as_str().contains("EXPLAIN LOGICAL SELECT"))
	);

	let full = json_result(
		&store,
		"EXPLAIN FULL FORMAT JSON SELECT id FROM person WITH INDEX by_age WHERE properties.age = \
		 30;",
	);
	assert!(matches!(full.get("plan_hash"), Some(Value::Number(_))));
	assert!(matches!(full.get("nodes"), Some(Value::Array(nodes)) if !nodes.is_empty()));
	assert_eq!(full.get("provided_ordering"), Some(&Value::from("stable_result_key")));
	assert!(matches!(full.get("statistics"), Some(Value::Object(_))));
	let Some(Value::Array(nodes)) = full.get("nodes") else {
		panic!("physical nodes are missing");
	};
	let Some(Value::Object(index)) = nodes.iter().next() else {
		panic!("index node is missing");
	};
	assert_eq!(index.get("index_state"), Some(&Value::from("Ready")));
	assert!(index.contains_key("index_scope"));
	assert!(index.contains_key("children"));
	let rules = json_result(
		&store,
		"EXPLAIN FULL FORMAT JSON SELECT VALUE 1 + 2 FROM person WHERE true AND properties.age = \
		 30;",
	);
	let Some(Value::Array(rules)) = rules.get("rules") else {
		panic!("optimizer rules are missing");
	};
	assert!(rules.iter().any(|rule| rule == &Value::from("ConstantFolding")));
	assert!(rules.iter().any(|rule| rule == &Value::from("ConjunctionDecomposition")));

	json_result(&store, "EXPLAIN FULL FORMAT JSON CREATE person:planned;");
	assert!(!atom_exists(&store, "person:planned"));

	let analyzed = json_result(&store, "EXPLAIN ANALYZE FORMAT JSON CREATE person:published;");
	let Some(Value::Object(metrics)) = analyzed.get("analyze") else {
		panic!("ANALYZE metrics are missing");
	};
	assert_eq!(metrics.get("publication_state"), Some(&Value::from("published")));
	assert_eq!(metrics.get("affected_rows"), Some(&Value::from(1_u64)));
	assert!(atom_exists(&store, "person:published"));

	let selected = json_result(
		&store,
		"EXPLAIN ANALYZE FORMAT JSON SELECT id FROM person ORDER BY id LIMIT 1;",
	);
	let Some(Value::Object(metrics)) = selected.get("analyze") else {
		panic!("SELECT ANALYZE metrics are missing");
	};
	assert_eq!(metrics.get("actual_rows"), Some(&Value::from(1_u64)));
	assert!(
		matches!(metrics.get("pulls"), Some(Value::Number(value)) if u64::try_from(*value).is_ok_and(|value| value > 0))
	);

	let spilled = json_result(
		&store,
		"EXPLAIN ANALYZE FORMAT JSON SELECT id FROM person ORDER BY id TEMPFILES;",
	);
	let Some(Value::Object(metrics)) = spilled.get("analyze") else {
		panic!("spilled ANALYZE metrics are missing");
	};
	assert!(
		matches!(metrics.get("spill_runs"), Some(Value::Number(value)) if u64::try_from(*value).is_ok_and(|value| value > 0))
	);
	assert!(
		matches!(metrics.get("spill_bytes"), Some(Value::Number(value)) if u64::try_from(*value).is_ok_and(|value| value > 0))
	);

	json_result(&store, "EXPLAIN ANALYZE FORMAT JSON DEFINE CLASS department TYPE NODE;");
	store.execute("CREATE department:published;", &Object::new()).unwrap();
	assert!(atom_exists(&store, "department:published"));
}

#[test]
fn explain_analyze_respects_explicit_transaction_publication() {
	let store = Datastore::in_memory();
	store.execute("DEFINE CLASS person TYPE NODE;", &Object::new()).unwrap();
	let mut session = store.session();
	session.execute("BEGIN;", &Object::new()).unwrap();
	let result = session
		.execute("EXPLAIN ANALYZE FORMAT JSON CREATE person:private;", &Object::new())
		.unwrap()
		.remove(0)
		.result
		.unwrap()
		.into_parts()
		.0;
	let Value::Object(root) = result else {
		panic!("FORMAT JSON must return an object");
	};
	let Some(Value::Object(metrics)) = root.get("analyze") else {
		panic!("ANALYZE metrics are missing");
	};
	assert_eq!(metrics.get("publication_state"), Some(&Value::from("deferred")));
	assert!(!atom_exists(&store, "person:private"));
	session.execute("CANCEL;", &Object::new()).unwrap();
	assert!(!atom_exists(&store, "person:private"));

	session.execute("BEGIN; CREATE person:staged;", &Object::new()).unwrap();
	let failed = session
		.execute("EXPLAIN ANALYZE FORMAT JSON CREATE person:staged;", &Object::new())
		.unwrap()
		.remove(0);
	assert!(failed.result.is_err());
	assert!(!session.has_active_transaction());
	assert!(!atom_exists(&store, "person:staged"));

	assert!(store.execute("EXPLAIN EXPLAIN SELECT id FROM person;", &Object::new()).is_err());
	assert!(store.execute("EXPLAIN IF true { RETURN 1; };", &Object::new()).is_err());
}

#[test]
fn explain_match_projects_physical_algorithms_and_runtime_metrics() {
	let store = Datastore::in_memory();
	let mut seed = store.begin();
	seed.define_class(ClassSchema::new("person", AtomKind::Node, []).unwrap()).unwrap();
	seed.define_class(ClassSchema::new("link", AtomKind::Edge, []).unwrap()).unwrap();
	let graph = seed.define_graph("routes", GraphKind::Graph).unwrap();
	let source = "person:a".parse::<RecordId>().unwrap();
	let target = "person:b".parse::<RecordId>().unwrap();
	let edge = "link:ab".parse::<RecordId>().unwrap();
	for record in [&source, &target, &edge] {
		seed.create_atom(record.clone(), Default::default()).unwrap();
	}
	let source_id = seed.attach_node(graph.id(), &source, None, None).unwrap();
	let target_id = seed.attach_node(graph.id(), &target, None, None).unwrap();
	seed.attach_edge(
		graph.id(),
		&edge,
		None,
		EdgeTopology::directed([source_id], [target_id]),
		Some(Number::Int(2)),
		None,
	)
	.unwrap();
	seed.commit().unwrap();

	let node_operators = |root: &Object| {
		let Some(Value::Array(nodes)) = root.get("nodes") else {
			panic!("physical MATCH nodes are missing");
		};
		nodes
			.iter()
			.filter_map(Value::as_object)
			.filter_map(|node| node.get("operator"))
			.filter_map(Value::as_str)
			.map(str::to_owned)
			.collect::<Vec<_>>()
	};
	let full = json_result(
		&store,
		"EXPLAIN FULL FORMAT JSON OPTIONAL MATCH DISTINCT path = (person:a AS a)-[link*1..3 \
		 SHORTEST]->(person:b AS b) IN GRAPH routes RETURN DISTINCT path ORDER BY \
		 path::length(path);",
	);
	let operators = node_operators(&full);
	for expected in [
		"MatchSeed",
		"BreadthFirstShortest",
		"OptionalFallback",
		"BindingDistinct",
		"ProjectionDistinct",
		"MatchOrder",
	] {
		assert!(operators.iter().any(|operator| operator == expected), "{expected}");
	}

	let weighted = json_result(
		&store,
		"EXPLAIN FULL FORMAT JSON MATCH path = (person:a AS a)-[link*1..3 WEIGHTED \
		 SHORTEST]->(person:b AS b) IN GRAPH routes RETURN path;",
	);
	assert!(node_operators(&weighted).iter().any(|operator| operator == "DijkstraShortest"));

	let analyzed = json_result(
		&store,
		"EXPLAIN ANALYZE FORMAT JSON MATCH path = (person:a AS a)-[link*1..3 SHORTEST]->(person:b \
		 AS b) IN GRAPH routes RETURN path;",
	);
	let Some(Value::Object(metrics)) = analyzed.get("analyze") else {
		panic!("MATCH ANALYZE metrics are missing");
	};
	for metric in ["path_expansions", "match_seeds", "frontier_peak", "trace_nodes", "graph_opens"]
	{
		assert!(
			matches!(metrics.get(metric), Some(Value::Number(value)) if u64::try_from(*value).is_ok_and(|value| value > 0)),
			"{metric}",
		);
	}
	let without_path_value = json_result(
		&store,
		"EXPLAIN ANALYZE FORMAT JSON MATCH (person:a AS a)-[link*1..3]->(person:b AS b) IN GRAPH \
		 routes RETURN b;",
	);
	let Some(Value::Object(metrics)) = without_path_value.get("analyze") else {
		panic!("MATCH ANALYZE metrics are missing");
	};
	assert_eq!(
		metrics.get("trace_nodes"),
		Some(&Value::from(0_u64)),
		"an unobserved path must not allocate PathTraceArena nodes",
	);

	let mut session = store.session();
	session
		.execute("BEGIN ISOLATION LEVEL SNAPSHOT EXECUTION MODE DETERMINISTIC;", &Object::new())
		.unwrap();
	let query = "EXPLAIN FULL FORMAT JSON MATCH (person AS p) IN GRAPH routes RETURN p;";
	let first = session.execute(query, &Object::new()).unwrap();
	let second = session.execute(query, &Object::new()).unwrap();
	let hash = |results: Vec<crate::gql::QueryResult>| {
		let value = results.into_iter().next().unwrap().result.unwrap().into_parts().0;
		let Value::Object(root) = value else {
			panic!("FORMAT JSON must return an object")
		};
		root.get("plan_hash").cloned().unwrap()
	};
	assert_eq!(hash(first), hash(second));
	session.execute("CANCEL;", &Object::new()).unwrap();
}
