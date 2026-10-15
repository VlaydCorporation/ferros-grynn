#[cfg(test)]
use crate::test_assertions::AssertOption as _;
#[cfg(test)]
use crate::test_assertions::AssertResult as _;
use crate::{
	datastore::{ClassSchema, Datastore},
	gql::QueryStatementKind,
	graph::{AtomKind, EdgeTopology, GraphKind},
	values::{Number, Object, RecordId, Value},
};

/// Reports whether the atom exists condition holds for this module.
fn atom_exists(store: &Datastore, record: &str) -> bool {
	let mut transaction = store.begin();
	transaction.atom(&record.parse::<RecordId>().assert_ok()).assert_ok().is_some()
}

/// Returns json result for this module.
fn json_result(store: &Datastore, source: &str) -> Object {
	let mut results = store.execute(source, &Object::new()).assert_ok();
	assert_eq!(results.len(), 1);
	assert_eq!(results[0].kind, QueryStatementKind::Explain);
	let outcome = results.remove(0).result.assert_ok();
	match outcome.into_parts().0 {
		Value::Object(value) => Some(value),
		_ => None,
	}
	.assert_some_with("FORMAT JSON must return an object")
}

/// Verifies duration explain uses wrapped timeout.
#[test]
fn duration_explain_uses_wrapped_timeout() {
	use crate::test_assertions::AssertResult as _;

	let store = Datastore::in_memory();
	let mut session = store.session();
	let inputs = Object::from_iter([
		("second", Value::from("1s".parse::<crate::values::Duration>().assert_ok())),
		("zero", Value::from("0ms".parse::<crate::values::Duration>().assert_ok())),
	]);
	for result in session
		.execute(
			"DEFINE CLASS person TYPE NODE; CREATE person:seed; LIVE LET $limit = $zero; LET \
			 $fixed = $zero;",
			&inputs,
		)
		.assert_ok()
	{
		result.result.assert_ok();
	}
	for prefix in ["EXPLAIN", "EXPLAIN FULL", "EXPLAIN ANALYZE"] {
		for timeout in ["$limit", "$fixed", "0ms"] {
			let source = format!("{prefix} SELECT id FROM person TIMEOUT {timeout};");
			let results = session.execute(&source, &Object::new()).assert_ok();
			assert!(results[0].result.is_err(), "{source}: {results:?}");
		}
	}
	for result in session
		.execute("ALTER LIVE LET $limit VALUE $second; ALTER LET $fixed VALUE $second;", &inputs)
		.assert_ok()
	{
		result.result.assert_ok();
	}
	for prefix in ["EXPLAIN", "EXPLAIN FULL", "EXPLAIN ANALYZE"] {
		for timeout in ["$limit", "$fixed", "1s"] {
			let source = format!("{prefix} SELECT id FROM person TIMEOUT {timeout};");
			let results = session.execute(&source, &Object::new()).assert_ok();
			assert!(results[0].result.is_ok(), "{source}: {results:?}");
		}
	}
	let planned = session
		.execute("EXPLAIN FULL CREATE person:planned TIMEOUT 1s;", &Object::new())
		.assert_ok();
	assert!(planned[0].result.is_ok());
	assert!(
		store.begin().atom(&"person:planned".parse::<RecordId>().assert_ok()).assert_ok().is_none()
	);
	let rejected = session
		.execute("EXPLAIN ANALYZE CREATE person:rejected TIMEOUT 0ms;", &Object::new())
		.assert_ok();
	assert!(rejected[0].result.is_err());
	assert!(
		store
			.begin()
			.atom(&"person:rejected".parse::<RecordId>().assert_ok())
			.assert_ok()
			.is_none()
	);
	let overflow = session
		.execute(
			"EXPLAIN ANALYZE CREATE person:rejected TIMEOUT 18446744073709549568s;",
			&Object::new(),
		)
		.assert_ok();
	assert!(overflow[0].result.is_err());
	assert!(
		store
			.begin()
			.atom(&"person:rejected".parse::<RecordId>().assert_ok())
			.assert_ok()
			.is_none()
	);
	let nested = session
		.execute(
			"IF true { LET $limit = $second; EXPLAIN SELECT id FROM person TIMEOUT $limit; };",
			&inputs,
		)
		.assert_ok();
	assert!(nested[0].result.is_ok(), "{nested:?}");
	let info = session.execute("INFO FOR LIVE LET $limit;", &Object::new()).assert_ok();
	let uuid = info[0]
		.result
		.as_ref()
		.map_err(Clone::clone)
		.assert_ok()
		.value()
		.as_object()
		.and_then(|value| value.get("id"))
		.ok_or_else(|| {
			crate::gql_status::FerrosGrynnError::invalid_argument("test binding identity missing")
		})
		.assert_ok();
	let invalid_uuid = session
		.execute(
			"EXPLAIN SELECT id FROM person TIMEOUT $ordinary;",
			&Object::from_iter([("ordinary", uuid.clone())]),
		)
		.assert_ok();
	assert!(invalid_uuid[0].result.is_err());
	for duration in ["P1M", "-PT1S", "PT20000000000S"] {
		let inputs = Object::from_iter([(
			"bad",
			Value::from(duration.parse::<crate::values::Duration>().assert_ok()),
		)]);
		for result in session.execute("ALTER LIVE LET $limit VALUE $bad;", &inputs).assert_ok() {
			result.result.assert_ok();
		}
		let rejected = session
			.execute("EXPLAIN ANALYZE CREATE person:rejected TIMEOUT $limit;", &Object::new())
			.assert_ok();
		assert!(rejected[0].result.is_err());
		assert!(
			store
				.begin()
				.atom(&"person:rejected".parse::<RecordId>().assert_ok())
				.assert_ok()
				.is_none()
		);
	}
}

/// Verifies explain plans without effect and analyze executes once.
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
		.assert_ok();

	let logical = store
		.execute("EXPLAIN SELECT id FROM person WHERE properties.age = 30;", &Object::new())
		.assert_ok()
		.remove(0)
		.result
		.assert_ok()
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
	assert_full_select_plan(&full);
	let rules = json_result(
		&store,
		"EXPLAIN FULL FORMAT JSON SELECT VALUE 1 + 2 FROM person WHERE true AND properties.age = \
		 30;",
	);
	let rules = match rules.get("rules") {
		Some(Value::Array(rules)) => Some(rules),
		_ => None,
	}
	.assert_some_with("optimizer rules are missing");
	assert!(rules.iter().any(|rule| rule == &Value::from("ConstantFolding")));
	assert!(rules.iter().any(|rule| rule == &Value::from("ConjunctionDecomposition")));

	json_result(&store, "EXPLAIN FULL FORMAT JSON CREATE person:planned;");
	assert!(!atom_exists(&store, "person:planned"));

	let analyzed = json_result(&store, "EXPLAIN ANALYZE FORMAT JSON CREATE person:published;");
	let metrics = match analyzed.get("analyze") {
		Some(Value::Object(metrics)) => Some(metrics),
		_ => None,
	}
	.assert_some_with("ANALYZE metrics are missing");
	assert_eq!(metrics.get("publication_state"), Some(&Value::from("published")));
	assert_eq!(metrics.get("affected_rows"), Some(&Value::from(1_u64)));
	assert!(atom_exists(&store, "person:published"));

	let selected = json_result(
		&store,
		"EXPLAIN ANALYZE FORMAT JSON SELECT id FROM person ORDER BY id LIMIT 1;",
	);
	let metrics = match selected.get("analyze") {
		Some(Value::Object(metrics)) => Some(metrics),
		_ => None,
	}
	.assert_some_with("SELECT ANALYZE metrics are missing");
	assert_eq!(metrics.get("actual_rows"), Some(&Value::from(1_u64)));
	assert!(
		matches!(metrics.get("pulls"), Some(Value::Number(value)) if u64::try_from(*value).is_ok_and(|value| value > 0))
	);

	let spilled = json_result(
		&store,
		"EXPLAIN ANALYZE FORMAT JSON SELECT id FROM person ORDER BY id TEMPFILES;",
	);
	let metrics = match spilled.get("analyze") {
		Some(Value::Object(metrics)) => Some(metrics),
		_ => None,
	}
	.assert_some_with("spilled ANALYZE metrics are missing");
	assert!(
		matches!(metrics.get("spill_runs"), Some(Value::Number(value)) if u64::try_from(*value).is_ok_and(|value| value > 0))
	);
	assert!(
		matches!(metrics.get("spill_bytes"), Some(Value::Number(value)) if u64::try_from(*value).is_ok_and(|value| value > 0))
	);

	json_result(&store, "EXPLAIN ANALYZE FORMAT JSON DEFINE CLASS department TYPE NODE;");
	store.execute("CREATE department:published;", &Object::new()).assert_ok();
	assert!(atom_exists(&store, "department:published"));
}

/// Checks the physical SELECT metadata already returned by EXPLAIN FULL.
///
/// # Panics
/// Panics if the original plan metadata or index-node assertion fails.
#[track_caller]
fn assert_full_select_plan(full: &Object) {
	assert!(matches!(full.get("plan_hash"), Some(Value::Number(_))));
	assert!(matches!(full.get("nodes"), Some(Value::Array(nodes)) if !nodes.is_empty()));
	assert_eq!(full.get("provided_ordering"), Some(&Value::from("stable_result_key")));
	assert!(matches!(full.get("statistics"), Some(Value::Object(_))));
	let nodes = match full.get("nodes") {
		Some(Value::Array(nodes)) => Some(nodes),
		_ => None,
	}
	.assert_some_with("physical nodes are missing");
	let index = match nodes.iter().next() {
		Some(Value::Object(index)) => Some(index),
		_ => None,
	}
	.assert_some_with("index node is missing");
	assert_eq!(index.get("index_state"), Some(&Value::from("Ready")));
	assert!(index.contains_key("index_scope"));
	assert!(index.contains_key("children"));
}

/// Verifies explain analyze respects explicit transaction publication.
#[test]
fn explain_analyze_respects_explicit_transaction_publication() {
	let store = Datastore::in_memory();
	store.execute("DEFINE CLASS person TYPE NODE;", &Object::new()).assert_ok();
	let mut session = store.session();
	session.execute("BEGIN;", &Object::new()).assert_ok();
	let result = session
		.execute("EXPLAIN ANALYZE FORMAT JSON CREATE person:private;", &Object::new())
		.assert_ok()
		.remove(0)
		.result
		.assert_ok()
		.into_parts()
		.0;
	let root = match result {
		Value::Object(root) => Some(root),
		_ => None,
	}
	.assert_some_with("FORMAT JSON must return an object");
	let metrics = match root.get("analyze") {
		Some(Value::Object(metrics)) => Some(metrics),
		_ => None,
	}
	.assert_some_with("ANALYZE metrics are missing");
	assert_eq!(metrics.get("publication_state"), Some(&Value::from("deferred")));
	assert!(!atom_exists(&store, "person:private"));
	session.execute("CANCEL;", &Object::new()).assert_ok();
	assert!(!atom_exists(&store, "person:private"));

	session.execute("BEGIN; CREATE person:staged;", &Object::new()).assert_ok();
	let failed = session
		.execute("EXPLAIN ANALYZE FORMAT JSON CREATE person:staged;", &Object::new())
		.assert_ok()
		.remove(0);
	assert!(failed.result.is_err());
	assert!(!session.has_active_transaction());
	assert!(!atom_exists(&store, "person:staged"));

	assert!(store.execute("EXPLAIN EXPLAIN SELECT id FROM person;", &Object::new()).is_err());
	assert!(store.execute("EXPLAIN IF true { RETURN 1; };", &Object::new()).is_err());
}

/// Verifies explain match projects physical algorithms and runtime metrics.
#[test]
fn explain_match_projects_physical_algorithms_and_runtime_metrics() {
	let store = Datastore::in_memory();
	let mut seed = store.begin();
	seed.define_class(ClassSchema::new("person", AtomKind::Node, []).assert_ok()).assert_ok();
	seed.define_class(ClassSchema::new("link", AtomKind::Edge, []).assert_ok()).assert_ok();
	let graph = seed.define_graph("routes", GraphKind::Graph).assert_ok();
	let source = "person:a".parse::<RecordId>().assert_ok();
	let target = "person:b".parse::<RecordId>().assert_ok();
	let edge = "link:ab".parse::<RecordId>().assert_ok();
	for record in [&source, &target, &edge] {
		seed.create_atom(record.clone(), crate::values::PropertyMap::default()).assert_ok();
	}
	let source_id = seed.attach_node(graph.id(), &source, None, None).assert_ok();
	let target_id = seed.attach_node(graph.id(), &target, None, None).assert_ok();
	seed.attach_edge(
		graph.id(),
		&edge,
		None,
		EdgeTopology::directed([source_id], [target_id]),
		Some(Number::Int(2)),
		None,
	)
	.assert_ok();
	seed.commit().assert_ok();

	let node_operators = |root: &Object| {
		let nodes = match root.get("nodes") {
			Some(Value::Array(nodes)) => Some(nodes),
			_ => None,
		}
		.assert_some_with("physical MATCH nodes are missing");
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
	let metrics = match analyzed.get("analyze") {
		Some(Value::Object(metrics)) => Some(metrics),
		_ => None,
	}
	.assert_some_with("MATCH ANALYZE metrics are missing");
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
	let metrics = match without_path_value.get("analyze") {
		Some(Value::Object(metrics)) => Some(metrics),
		_ => None,
	}
	.assert_some_with("MATCH ANALYZE metrics are missing");
	assert_eq!(
		metrics.get("trace_nodes"),
		Some(&Value::from(0_u64)),
		"an unobserved path must not allocate PathTraceArena nodes",
	);

	let mut session = store.session();
	session
		.execute("BEGIN ISOLATION LEVEL SNAPSHOT EXECUTION MODE DETERMINISTIC;", &Object::new())
		.assert_ok();
	let query = "EXPLAIN FULL FORMAT JSON MATCH (person AS p) IN GRAPH routes RETURN p;";
	let first = session.execute(query, &Object::new()).assert_ok();
	let second = session.execute(query, &Object::new()).assert_ok();
	let hash = |results: Vec<crate::gql::QueryResult>| {
		let value = results.into_iter().next().assert_some().result.assert_ok().into_parts().0;
		let root = match value {
			Value::Object(root) => Some(root),
			_ => None,
		}
		.assert_some_with("FORMAT JSON must return an object");
		root.get("plan_hash").cloned().assert_some()
	};
	assert_eq!(hash(first), hash(second));
	session.execute("CANCEL;", &Object::new()).assert_ok();
}

/// Verifies explain traverse projects vm steps and fast effect limit.
#[test]
fn explain_traverse_projects_vm_steps_and_fast_effect_limit() {
	let store = Datastore::in_memory();
	let full = json_result(
		&store,
		"EXPLAIN FULL FORMAT JSON TRAVERSE FROM [1, 2, 2] |> tv::map(|$value| $value + 1) |> \
		 tv::dedup() |> tv::count();",
	);
	assert_eq!(full.get("statement_kind"), Some(&Value::from("TRAVERSE")));
	let nodes = match full.get("nodes") {
		Some(Value::Array(nodes)) => Some(nodes),
		_ => None,
	}
	.assert_some_with("physical traversal nodes are missing");
	let operators = nodes
		.iter()
		.filter_map(Value::as_object)
		.filter_map(|node| node.get("operator"))
		.filter_map(Value::as_str)
		.collect::<Vec<_>>();
	assert_eq!(operators, ["ValueSource", "Map", "Deduplicate", "Count"]);
	assert!(matches!(full.get("plan_hash"), Some(Value::Number(_))));

	let analyzed = json_result(
		&store,
		"EXPLAIN ANALYZE FORMAT JSON TRAVERSE FROM [1, 2, 2] |> tv::dedup() |> tv::count();",
	);
	let metrics = match analyzed.get("analyze") {
		Some(Value::Object(metrics)) => Some(metrics),
		_ => None,
	}
	.assert_some_with("TRAVERSE ANALYZE metrics are missing");
	assert_eq!(metrics.get("actual_rows"), Some(&Value::from(1_u64)));

	let mut session = store.session();
	session.execute("BEGIN EXECUTION MODE FAST;", &Object::new()).assert_ok();
	let explained = session
		.execute(
			"EXPLAIN FULL FORMAT JSON TRAVERSE FROM [1] |> tv::side_effect('seen', |$value, \
			 $previous| $value);",
			&Object::new(),
		)
		.assert_ok()
		.remove(0)
		.result
		.assert_ok()
		.into_parts()
		.0;
	let explained = match explained {
		Value::Object(explained) => Some(explained),
		_ => None,
	}
	.assert_some_with("FORMAT JSON must return an object");
	let warnings = match explained.get("warnings") {
		Some(Value::Array(warnings)) => Some(warnings),
		_ => None,
	}
	.assert_some_with("FAST traversal warnings are missing");
	assert!(warnings.iter().any(|warning| {
		warning.as_str().is_some_and(|warning| warning.starts_with("execution_limited:"))
	}));
	session.execute("CANCEL;", &Object::new()).assert_ok();
}
