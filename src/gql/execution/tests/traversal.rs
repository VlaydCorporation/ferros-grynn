use std::num::NonZeroUsize;

#[cfg(test)]
use crate::test_assertions::AssertOption as _;
#[cfg(test)]
use crate::test_assertions::AssertResult as _;
use crate::{
	datastore::{ClassSchema, Datastore},
	gql::{QueryExecutionOptions, QueryStatementKind},
	graph::{AtomKind, AtomRef, EdgeTopology, GraphKind},
	values::{Number, Object, RecordId, Value},
};

/// Returns traverse for this module.
fn traverse(source: &str) -> Value {
	let store = Datastore::in_memory();
	let result = store.execute(source, &Object::new()).assert_ok();
	let statement = result.last().assert_some();
	assert_eq!(statement.kind, QueryStatementKind::Traverse);
	statement.result.as_ref().assert_ok().value().clone()
}

/// Creates a directed chain used by both estimate bindings and exact traversal.
fn estimate_store() -> Datastore {
	let store = Datastore::in_memory();
	let mut seed = store.begin();
	seed.define_class(ClassSchema::new("person", AtomKind::Node, []).assert_ok()).assert_ok();
	seed.define_class(ClassSchema::new("road", AtomKind::Edge, []).assert_ok()).assert_ok();
	let graph = seed.define_graph("routes", GraphKind::Graph).assert_ok();
	let records = ["person:a", "person:b", "person:c", "road:ab", "road:bc"]
		.map(|value| value.parse::<RecordId>().assert_ok());
	for record in &records {
		seed.create_atom(record.clone(), crate::values::PropertyMap::default()).assert_ok();
	}
	let nodes = records[..3]
		.iter()
		.map(|record| seed.attach_node(graph.id(), record, None, None).assert_ok())
		.collect::<Vec<_>>();
	for (ordinal, pair) in nodes.windows(2).enumerate() {
		seed.attach_edge(
			graph.id(),
			&records[3 + ordinal],
			Some("road".into()),
			EdgeTopology::directed([pair[0]], [pair[1]]),
			None,
			None,
		)
		.assert_ok();
	}
	seed.commit().assert_ok();
	store
}

/// Returns the public query value, failing on parser or statement errors.
fn estimate_value(store: &Datastore, query: &str) -> Value {
	store
		.execute(query, &Object::new())
		.assert_ok()
		.last()
		.assert_some()
		.result
		.as_ref()
		.assert_ok()
		.value()
		.clone()
}

/// Verifies k hop estimate selects only compatible sketches and keeps exact
/// enumeration.
#[test]
fn k_hop_estimate_selects_only_compatible_sketches_and_keeps_exact_enumeration() {
	let store = estimate_store();
	let query = "TRAVERSE FROM person:a IN GRAPH @routes |> tv::kHopEstimate(2);";
	let explain =
		|query: &str| estimate_value(&store, &format!("EXPLAIN ANALYZE FORMAT JSON {query}"));
	let before = explain(query);
	assert_eq!(
		before
			.as_object()
			.assert_some()
			.get("analyze")
			.assert_some()
			.as_object()
			.assert_some()
			.get("neighbourhood_fallbacks"),
		Some(&Value::from(1_u64))
	);
	assert_eq!(estimate_value(&store, query), Value::Array(vec![3_u64].into()));
	for (name, suffix) in [
		("wrong_direction", "DIRECTION IN"),
		("wrong_mode", "MODE ATOM"),
		("wrong_labels", "EDGES road"),
	] {
		estimate_value(
			&store,
			&format!(
				"DEFINE INDEX {name} ON GRAPH routes NEIGHBOURHOOD(4) ALGORITHM SKETCH(12) \
				 {suffix} WITH (MAX_BYTES=1048576); REBUILD INDEX {name} ON GRAPH routes;"
			),
		);
	}
	let incompatible = explain(query);
	assert_eq!(
		incompatible.as_object().assert_some().get("plan_hash"),
		before.as_object().assert_some().get("plan_hash")
	);
	assert_eq!(
		incompatible
			.as_object()
			.assert_some()
			.get("analyze")
			.assert_some()
			.as_object()
			.assert_some()
			.get("neighbourhood_sketch_reads"),
		Some(&Value::from(0_u64))
	);
	for (name, precision) in [("coarse", 4), ("near", 12)] {
		estimate_value(
			&store,
			&format!(
				"DEFINE INDEX {name} ON GRAPH routes NEIGHBOURHOOD(4) ALGORITHM \
				 SKETCH({precision}) WITH (MAX_BYTES=1048576); REBUILD INDEX {name} ON GRAPH \
				 routes;"
			),
		);
	}
	let ready = explain(query);
	let ready = ready.as_object().assert_some();
	assert_ne!(ready.get("plan_hash"), before.as_object().assert_some().get("plan_hash"));
	let node = ready
		.get("nodes")
		.assert_some()
		.as_array()
		.assert_some()
		.iter()
		.filter_map(Value::as_object)
		.find(|node| node.get("operator") == Some(&Value::from("NeighbourhoodSketch")))
		.assert_some();
	assert_eq!(node.get("precision"), Some(&Value::from(12_u64)));
	let metrics = ready.get("analyze").assert_some().as_object().assert_some();
	assert_eq!(metrics.get("neighbourhood_sketch_reads"), Some(&Value::from(1_u64)));
	assert_eq!(metrics.get("neighbourhood_fallbacks"), Some(&Value::from(0_u64)));
	assert_eq!(metrics.get("neighbourhood_exact_work"), Some(&Value::from(0_u64)));
	assert_ne!(metrics.get("graph_artifact_bytes"), Some(&Value::from(0_u64)));
	let exact = "TRAVERSE FROM person:a IN GRAPH @routes |> tv::kHop(2);";
	let rows = estimate_value(&store, exact);
	let members =
		rows.as_array().assert_some().iter().next().assert_some().as_array().assert_some();
	assert_eq!(
		members
			.iter()
			.map(|value| value.as_atom_occurrence().assert_some().atom().id().to_string())
			.collect::<Vec<_>>(),
		["person:a", "person:b", "person:c"]
	);
	assert_eq!(
		explain(exact)
			.as_object()
			.assert_some()
			.get("analyze")
			.assert_some()
			.as_object()
			.assert_some()
			.get("neighbourhood_sketch_reads"),
		Some(&Value::from(0_u64))
	);
	for (radius, count) in [(0, 1_u64), (1, 2), (2, 3), (4, 3), (5, 3)] {
		assert_eq!(
			estimate_value(
				&store,
				&format!("TRAVERSE FROM person:a IN GRAPH @routes |> tv::kHopEstimate({radius});")
			),
			Value::Array(vec![count].into())
		);
	}
	for (source, radius, count) in
		[("person:a", 1, 2_u64), ("person:a", 2, 3), ("road:ab", 2, 1), ("person:a", 5, 3)]
	{
		assert_eq!(
			estimate_value(
				&store,
				&format!(
					"TRAVERSE ATOM FROM {source} IN GRAPH @routes |> tv::kHopEstimate({radius});"
				)
			),
			Value::Array(vec![count].into())
		);
	}
	assert_eq!(
		estimate_value(
			&store,
			"TRAVERSE FROM [person:a, person:a] IN GRAPH @routes |> tv::kHopEstimate(2);"
		),
		Value::Array(vec![3_u64, 3].into())
	);
}

/// Verifies k hop estimate rejects invalid radii and enforces exact resource
/// budgets.
#[test]
fn k_hop_estimate_rejects_invalid_radii_and_enforces_exact_resource_budgets() {
	let store = estimate_store();
	for radius in ["-1", "1.5", "'two'", "4294967296"] {
		let query =
			format!("TRAVERSE FROM person:a IN GRAPH @routes |> tv::kHopEstimate({radius});");
		if let Ok(results) = store.execute(&query, &Object::new()) {
			assert!(results[0].result.is_err(), "{radius}");
		}
	}
	let query = "TRAVERSE FROM person:a IN GRAPH @routes |> tv::kHopEstimate(2);";
	let limited = QueryExecutionOptions {
		max_path_expansions: 1,
		..Default::default()
	};
	assert!(
		store.execute_with_options(query, &Object::new(), limited).assert_ok()[0].result.is_err()
	);
	let tiny = QueryExecutionOptions {
		barrier_memory_bytes: NonZeroUsize::new(2048).assert_some(),
		..Default::default()
	};
	assert!(store.execute_with_options(query, &Object::new(), tiny).assert_ok()[0].result.is_err());
	let cancel = crate::gql::QueryCancellation::default();
	cancel.cancel();
	let cancelled = QueryExecutionOptions {
		cancellation: Some(cancel),
		..Default::default()
	};
	assert!(
		store.execute_with_options(query, &Object::new(), cancelled).assert_ok()[0].result.is_err()
	);
	let full = estimate_value(&store, &format!("EXPLAIN ANALYZE FORMAT JSON {query}"));
	let work = full
		.as_object()
		.assert_some()
		.get("analyze")
		.assert_some()
		.as_object()
		.assert_some()
		.get("neighbourhood_exact_work")
		.assert_some()
		.as_number()
		.and_then(|value| u64::try_from(value).ok())
		.assert_some();
	assert!(work > 1);
	let shared = QueryExecutionOptions {
		max_path_expansions: work,
		..Default::default()
	};
	assert!(
		store
			.execute_with_options(
				"TRAVERSE FROM [person:a, person:b] IN GRAPH @routes |> tv::kHopEstimate(2);",
				&Object::new(),
				shared
			)
			.assert_ok()[0]
			.result
			.is_err()
	);
}

/// Verifies k hop estimate private invalidation old snapshot and ssi.
#[test]
fn k_hop_estimate_private_invalidation_old_snapshot_and_ssi() {
	let store = estimate_store();
	estimate_value(
		&store,
		"DEFINE INDEX near ON GRAPH routes NEIGHBOURHOOD(4) ALGORITHM SKETCH WITH \
		 (MAX_BYTES=1048576); REBUILD INDEX near ON GRAPH routes;",
	);
	let query = "EXPLAIN ANALYZE FORMAT JSON TRAVERSE FROM person:a IN GRAPH @routes |> \
	             tv::kHopEstimate(2);";
	let hits = |value: &Value| {
		value
			.as_object()
			.assert_some()
			.get("analyze")
			.assert_some()
			.as_object()
			.assert_some()
			.get("neighbourhood_sketch_reads")
			.cloned()
	};
	let graph = store.schema_catalog().named_graph("routes").assert_some().id();
	let edge = "road:ab".parse::<RecordId>().assert_ok();
	let mut old = store.begin();
	let mut reader = store.begin();
	assert_eq!(
		hits(
			reader.execute(query, &Object::new()).assert_ok()[0]
				.result
				.as_ref()
				.assert_ok()
				.value()
		),
		Some(Value::from(1_u64))
	);
	let mut private = store.begin();
	private.set_occurrence_label(graph, &edge, Some("private".into())).assert_ok();
	assert_eq!(
		hits(
			private.execute(query, &Object::new()).assert_ok()[0]
				.result
				.as_ref()
				.assert_ok()
				.value()
		),
		Some(Value::from(0_u64))
	);
	drop(private);
	assert_eq!(hits(&estimate_value(&store, query)), Some(Value::from(1_u64)));
	let mut property = store.begin();
	let key = property.intern_property("info").assert_ok();
	property.set_base_property(&edge, key, Value::from(7_i64)).assert_ok();
	property.commit().assert_ok();
	assert_eq!(hits(&estimate_value(&store, query)), Some(Value::from(1_u64)));
	let mut writer = store.begin();
	writer.detach_occurrence(graph, &edge, false).assert_ok();
	writer.commit().assert_ok();
	assert_eq!(hits(&estimate_value(&store, query)), Some(Value::from(0_u64)));
	assert_eq!(
		estimate_value(&store, "TRAVERSE FROM person:a IN GRAPH @routes |> tv::kHopEstimate(2);"),
		Value::Array(vec![1_u64].into())
	);
	assert_eq!(
		hits(old.execute(query, &Object::new()).assert_ok()[0].result.as_ref().assert_ok().value()),
		Some(Value::from(1_u64))
	);
	reader
		.create_atom("person:reader".parse().assert_ok(), crate::values::PropertyMap::default())
		.assert_ok();
	assert_eq!(reader.commit().assert_err().status().code().to_string(), "FG-40001");
}

/// Verifies traverses collection with language closures.
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

/// Verifies named algorithm receives current and flat maps collections.
#[test]
fn named_algorithm_receives_current_and_flat_maps_collections() {
	let store = Datastore::in_memory();
	let defined = store
		.execute(
			"DEFINE PURE FUNCTION fn::offset(value: int, amount: int) -> int { RETURN $value + \
			 $amount; }; DEFINE PURE FUNCTION fn::spread(value: int) -> array { RETURN [$value, \
			 $value + 10]; }; DEFINE IMPURE FUNCTION fn::impure_identity(value: int) -> int { \
			 RETURN $value; };",
			&Object::new(),
		)
		.assert_ok();
	assert!(
		defined.iter().all(|result| result.result.is_ok()),
		"function definition failed: {defined:?}"
	);

	assert_eq!(
		store
			.execute("TRAVERSE FROM [1, 2, 3] |> tv::algo(fn::offset, 2);", &Object::new(),)
			.assert_ok()[0]
			.result
			.as_ref()
			.assert_ok()
			.value(),
		&Value::Array(vec![3_i64, 4, 5].into())
	);
	assert_eq!(
		store.execute("TRAVERSE FROM [1, 2] |> tv::algo(fn::spread);", &Object::new(),).assert_ok()
			[0]
		.result
		.as_ref()
		.assert_ok()
		.value(),
		&Value::Array(vec![1_i64, 11, 2, 12].into())
	);
	let impure = store
		.execute("TRAVERSE FROM [1] |> tv::algo(fn::impure_identity);", &Object::new())
		.assert_ok();
	assert!(impure[0].result.is_err());
}

/// Verifies collection steps cover the public traversal adapters.
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

/// Verifies generic pipeline and block closure use the query runtime.
#[test]
fn generic_pipeline_and_block_closure_use_the_query_runtime() {
	let store = Datastore::in_memory();
	let pipeline = store
		.execute("[1, 2, 3] |> tv::map(|$value| $value * 2) |> tv::fold();", &Object::new())
		.assert_ok();
	assert_eq!(
		pipeline[0].result.as_ref().assert_ok().value(),
		&Value::Array(vec![Value::Array(vec![2_i64, 4, 6].into())].into())
	);
	let closure = store
		.execute(
			"LET $increment = |$value| -> number { LET $next = $value + 1; RETURN $next; }; \
			 $increment(41);",
			&Object::new(),
		)
		.assert_ok();
	assert_eq!(
		closure.last().assert_some().result.as_ref().assert_ok().value(),
		&Value::from(42_i64)
	);
}

/// Verifies persistent session owns closures and transient execute rejects
/// escape.
#[test]
fn persistent_session_owns_closures_and_transient_execute_rejects_escape() {
	let store = Datastore::in_memory();
	let mut session = store.session();
	session
		.execute("LET $factor = 2; LET $twice = |$value| $value * $factor;", &Object::new())
		.assert_ok();
	let result = session.execute("$twice(21);", &Object::new()).assert_ok();
	assert_eq!(result[0].result.as_ref().assert_ok().value(), &Value::from(42_i64));

	let returned = session.execute("$twice;", &Object::new()).assert_ok();
	let closure = returned[0].result.as_ref().assert_ok().value().clone();
	let mut other = store.session();
	let mut variables = Object::new();
	variables.insert("foreign", closure);
	let mismatch = other.execute("$foreign(1);", &variables).assert_ok();
	assert!(mismatch[0].result.is_err());

	let transient = store.execute("[|$value| $value];", &Object::new()).assert_ok();
	assert!(transient[0].result.is_err());
}

/// Verifies persistent session releases statement local closures.
#[test]
fn persistent_session_releases_statement_local_closures() {
	let store = Datastore::in_memory();
	let mut session = store.session();
	for _ in 0..32 {
		let result = session
			.execute("TRAVERSE FROM [1, 2] |> tv::map(|$value| $value + 1);", &Object::new())
			.assert_ok();
		assert!(result[0].result.is_ok());
		assert_eq!(session.retained_closure_bytes(), 0);
	}
}

/// Verifies traversal bulks and splits limit without changing values.
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

/// Verifies sacks receive previous state before current value.
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
			.assert_ok()[0]
			.result
			.is_err()
	);
}

/// Verifies atom source requires snapshot visible graph membership.
#[test]
fn atom_source_requires_snapshot_visible_graph_membership() {
	let store = Datastore::in_memory();
	let mut seed = store.begin();
	seed.define_class(ClassSchema::new("person", AtomKind::Node, []).assert_ok()).assert_ok();
	let graph = seed.define_graph("social", GraphKind::Graph).assert_ok();
	seed.commit().assert_ok();
	let mut variables = Object::new();
	variables.insert("graph", Value::GraphRef(Box::new(graph)));
	variables.insert(
		"missing",
		Value::Atom(Box::new(AtomRef::new(AtomKind::Node, "person:missing".parse().assert_ok()))),
	);
	let result = store
		.execute("TRAVERSE FROM $missing IN GRAPH $graph |> tv::limit(1);", &variables)
		.assert_ok();
	assert!(result[0].result.is_err());
}

/// Verifies fast traversal preserves multiplicity and impure source order.
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
		.assert_ok();
	let traversal =
		results.iter().find(|result| result.kind == QueryStatementKind::Traverse).assert_some();
	assert_eq!(
		traversal.result.as_ref().assert_ok().value(),
		&Value::Array(vec![1_i64, 12, 121].into())
	);
}

/// Verifies traversal return reads named bindings and side effect state.
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

/// Verifies mutating side effect is visible to downstream steps.
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
		.assert_ok();
	let graph = store.schema_catalog().named_graph("social").assert_some().reference().clone();
	let alice = "person:alice".parse::<RecordId>().assert_ok();
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
		.assert_ok();
	let traversal =
		results.iter().find(|result| result.kind == QueryStatementKind::Traverse).assert_some();
	assert_eq!(traversal.result.as_ref().assert_ok().value(), &Value::Array(vec![7_i64].into()));
	assert_eq!(
		store.execute("SELECT VALUE score FROM person:alice;", &Object::new()).assert_ok()[0]
			.result
			.as_ref()
			.assert_ok()
			.value(),
		&Value::Array(vec![7_i64].into())
	);
}

/// Verifies failed language closure restores its transaction savepoint.
#[test]
fn failed_language_closure_restores_its_transaction_savepoint() {
	let store = Datastore::in_memory();
	store
		.execute(
			"DEFINE CLASS person TYPE NODE SCHEMAFULL; DEFINE FIELD score ON CLASS person TYPE \
			 int; CREATE person:alice SET score = 1;",
			&Object::new(),
		)
		.assert_ok();
	let results = store
		.execute(
			"BEGIN; LET $mutate = |$value| { UPDATE person:alice SET score = 9; THROW 'rollback'; \
			 }; TRY { $mutate(0); } CATCH |$error| { none; }; SELECT VALUE score FROM \
			 person:alice; COMMIT;",
			&Object::new(),
		)
		.assert_ok();
	let selected =
		results.iter().find(|result| result.kind == QueryStatementKind::Select).assert_some();
	assert_eq!(selected.result.as_ref().assert_ok().value(), &Value::Array(vec![1_i64].into()));
}

/// Verifies traversal order and dedup spill through shared query runs.
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
		.assert_ok();
	assert_eq!(
		results[0].result.as_ref().assert_ok().value(),
		&Value::Array(vec![1_i64, 2, 3].into())
	);
	let query_root = root.join("tmp").join("query");
	assert!(
		!query_root.exists() || std::fs::read_dir(&query_root).assert_ok().next().is_none(),
		"completed traversal must remove transient query-run sessions"
	);
	drop(std::fs::remove_dir_all(root));
}

/// Verifies traversal frontier obeys the shared memory budget.
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
		.assert_ok();
	assert!(results[0].result.is_err());
}

/// Verifies traversal group accepts a composite field key.
#[test]
fn traversal_group_accepts_a_composite_field_key() {
	let result = traverse(
		"TRAVERSE FROM [{ region: 'eu', tier: 1 }, { region: 'eu', tier: 2 }, { region: 'eu', \
		 tier: 1 }] |> tv::group('region', 'tier');",
	);
	let groups = match result {
		Value::Array(groups) => Some(groups),
		_ => None,
	}
	.assert_some_with("group traversal must return an array");
	assert_eq!(groups.len(), 2);
	let counts = groups
		.iter()
		.map(|group| {
			let group = match group {
				Value::Object(group) => Some(group),
				_ => None,
			}
			.assert_some_with("group traversal must return objects");
			group.get("count").cloned().unwrap_or(Value::None)
		})
		.collect::<Vec<_>>();
	assert_eq!(counts, vec![Value::from(2_i64), Value::from(1_i64)]);
}

/// Verifies repeat propagates until errors.
#[test]
fn repeat_propagates_until_errors() {
	let store = Datastore::in_memory();
	let results = store
		.execute(
			"TRAVERSE FROM [1] |> tv::repeat(|$x| $x + 1).until(|$x| math::sqrt(-1));",
			&Object::new(),
		)
		.assert_ok();
	assert!(results[0].result.is_err());
}

/// Verifies traversal uses the common statement timeout.
#[test]
fn traversal_uses_the_common_statement_timeout() {
	let store = Datastore::in_memory();
	let results = store
		.execute("TRAVERSE FROM [1] |> tv::map(|$value| $value) TIMEOUT 0ms;", &Object::new())
		.assert_ok();
	assert!(results[0].result.is_err());
}

/// Verifies navigates occurrences and resolves fields inside closures.
#[test]
fn navigates_occurrences_and_resolves_fields_inside_closures() {
	let store = Datastore::in_memory();
	let mut seed = store.begin();
	seed.define_class(ClassSchema::new("person", AtomKind::Node, []).assert_ok()).assert_ok();
	seed.define_class(ClassSchema::new("knows", AtomKind::Edge, []).assert_ok()).assert_ok();
	let graph = seed.define_graph("social", GraphKind::Graph).assert_ok();
	let alice = "person:alice".parse::<RecordId>().assert_ok();
	let bob = "person:bob".parse::<RecordId>().assert_ok();
	let knows = "knows:alice-bob".parse::<RecordId>().assert_ok();
	seed.create_atom(alice.clone(), crate::values::PropertyMap::default()).assert_ok();
	seed.create_atom(bob.clone(), crate::values::PropertyMap::default()).assert_ok();
	seed.create_atom(knows.clone(), crate::values::PropertyMap::default()).assert_ok();
	let alice_local =
		seed.attach_node(graph.id(), &alice, Some("Employee".into()), None).assert_ok();
	let bob_local = seed.attach_node(graph.id(), &bob, Some("Employee".into()), None).assert_ok();
	seed.attach_edge(
		graph.id(),
		&knows,
		Some("Friend".into()),
		EdgeTopology::directed([alice_local], [bob_local]),
		None,
		None,
	)
	.assert_ok();
	seed.commit().assert_ok();

	let mut variables = Object::new();
	variables.insert("graph", Value::GraphRef(Box::new(graph)));
	variables.insert("alice", Value::Record(Box::new(alice.clone())));
	let result = store
		.execute(
			"TRAVERSE FROM $graph |> tv::V() |> tv::where(|$x| $x.label = 'Employee') |> \
			 tv::out() |> tv::values('id');",
			&variables,
		)
		.assert_ok();
	assert_eq!(
		result[0].result.as_ref().assert_ok().value(),
		&Value::Array(vec![Value::Record(Box::new(bob.clone()))].into())
	);

	let path = store
		.execute(
			"TRAVERSE FROM $alice IN GRAPH $graph |> tv::outE() |> tv::inV() |> tv::path();",
			&variables,
		)
		.assert_ok();
	let paths = match path[0].result.as_ref().assert_ok().value() {
		Value::Array(paths) => Some(paths),
		_ => None,
	}
	.assert_some_with("path traversal must return an array");
	let path = match paths.iter().next() {
		Some(Value::Path(path)) => Some(path),
		_ => None,
	}
	.assert_some_with("path traversal must return one path");
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
		.assert_ok();
	assert_eq!(
		matched[0].result.as_ref().assert_ok().value(),
		&Value::Array(vec![Value::Record(Box::new(bob))].into())
	);
}

/// Verifies traversal algorithms reuse exact graph core.
#[test]
fn traversal_algorithms_reuse_exact_graph_core() {
	let store = Datastore::in_memory();
	let mut seed = store.begin();
	seed.define_class(ClassSchema::new("person", AtomKind::Node, []).assert_ok()).assert_ok();
	seed.define_class(ClassSchema::new("road", AtomKind::Edge, []).assert_ok()).assert_ok();
	let graph = seed.define_graph("routes", GraphKind::Graph).assert_ok();
	let records =
		["person:a", "person:b", "person:c"].map(|value| value.parse::<RecordId>().assert_ok());
	let edges = ["road:a-b", "road:b-c"].map(|value| value.parse::<RecordId>().assert_ok());
	for record in records.iter().chain(edges.iter()) {
		seed.create_atom(record.clone(), crate::values::PropertyMap::default()).assert_ok();
	}
	let a = seed.attach_node(graph.id(), &records[0], None, None).assert_ok();
	let b = seed.attach_node(graph.id(), &records[1], None, None).assert_ok();
	let c = seed.attach_node(graph.id(), &records[2], None, None).assert_ok();
	seed.attach_edge(
		graph.id(),
		&edges[0],
		None,
		EdgeTopology::directed([a], [b]),
		Some(Number::Int(2)),
		None,
	)
	.assert_ok();
	seed.attach_edge(
		graph.id(),
		&edges[1],
		None,
		EdgeTopology::directed([b], [c]),
		Some(Number::Int(3)),
		None,
	)
	.assert_ok();
	seed.commit().assert_ok();

	let mut variables = Object::new();
	variables.insert("graph", Value::GraphRef(Box::new(graph)));
	variables.insert("source", Value::Record(Box::new(records[0].clone())));
	variables
		.insert("target", Value::Atom(Box::new(AtomRef::new(AtomKind::Node, records[2].clone()))));
	let result = store
		.execute("TRAVERSE FROM $source IN GRAPH $graph |> tv::distanceTo($target);", &variables)
		.assert_ok();
	assert_eq!(
		result[0].result.as_ref().assert_ok().value(),
		&Value::Array(vec![Value::Number(Number::Int(5))].into())
	);

	let result = store
		.execute("TRAVERSE FROM $graph |> tv::V() |> tv::topologicalSort();", &variables)
		.assert_ok();
	let rows = match result[0].result.as_ref().assert_ok().value() {
		Value::Array(rows) => Some(rows),
		_ => None,
	}
	.assert_some_with("topological traversal must return an array");
	let order = match rows.iter().next() {
		Some(Value::Array(order)) => Some(order),
		_ => None,
	}
	.assert_some_with("topological traversal must return one ordered array");
	assert_eq!(order.len(), 3);

	let result = store
		.execute(
			"TRAVERSE FROM $graph |> tv::V() |> tv::stronglyConnectedComponents();",
			&variables,
		)
		.assert_ok();
	let rows = match result[0].result.as_ref().assert_ok().value() {
		Value::Array(rows) => Some(rows),
		_ => None,
	}
	.assert_some_with("SCC traversal must return an array");
	let components = match rows.iter().next() {
		Some(Value::Array(components)) => Some(components),
		_ => None,
	}
	.assert_some_with("SCC traversal must return one component array");
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
		let results = store.execute(query, &variables).assert_ok();
		assert!(results[0].result.is_ok(), "traversal adapter failed: {query}");
	}
}
