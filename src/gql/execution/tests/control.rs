use crate::{
	datastore::{DatabaseRoot, Datastore, GraphRoot, InMemoryOptions},
	gql::QueryExecutionOptions,
	gql_status::formatting::DiagnosticValue,
	graph::{GraphId, GraphKind, GraphRef, GraphStorageMode},
	values::{Object, RecordId, Value},
};

fn value(store: &Datastore, source: &str) -> Value {
	store.execute(source, &Object::new()).unwrap()[0].result.as_ref().unwrap().value().clone()
}

#[test]
fn evaluates_collections_ranges_idioms_and_builtins() {
	let store = Datastore::in_memory();
	assert_eq!(value(&store, "RETURN [1, 2, 3] CONTAINS 2;"), Value::Bool(true));
	for expression in ["false", "0", "''", "[]", "SET[]", "{}", "null", "none"] {
		assert_eq!(
			value(&store, &format!("RETURN !!({expression});")),
			Value::Bool(false),
			"{expression}"
		);
	}
	assert_eq!(value(&store, "RETURN !!([1]);"), Value::Bool(true));
	assert_eq!(value(&store, "RETURN SET[3, 1, 3];"), Value::Set(vec![1_i64, 3].into()));
	assert_eq!(
		value(&store, "RETURN [{v: 1}, {v: 2}, {v: 3}][WHERE v >= 2];"),
		Value::Array(
			vec![
				Value::Object([("v", 2_i64)].into_iter().collect()),
				Value::Object([("v", 3_i64)].into_iter().collect()),
			]
			.into()
		)
	);
	assert_eq!(value(&store, "RETURN string::upper('Grynn');"), Value::from("GRYNN"));
	assert_eq!(
		value(&store, "RETURN array::distinct([2, 1, 2]);"),
		Value::Array(vec![2_i64, 1].into())
	);
	assert_eq!(value(&store, "RETURN type::is<int>(1);"), Value::Bool(true));
	assert_eq!(
		value(&store, "RETURN {address: {city: 'Moscow', zip: 1}}.address.{city};"),
		Value::Object([("city", "Moscow")].into_iter().collect())
	);
	assert_eq!(value(&store, "RETURN none?.city;"), Value::None);
	let unsafe_null_access = store.execute("RETURN none.city;", &Object::new()).unwrap();
	assert!(unsafe_null_access[0].result.is_err());
	assert!(store.execute("RETURN none?.{city};", &Object::new()).is_err());
	let invalid = store.execute("RETURN string::upper(1);", &Object::new()).unwrap();
	assert!(invalid[0].result.is_err());

	let lossy = store.execute("RETURN math::sqrt(0.1dec);", &Object::new()).unwrap();
	let outcome = lossy[0].result.as_ref().unwrap();
	assert!(outcome.statuses().iter().any(|status| status.code().to_string() == "FG-01F02"));
	assert_eq!(
		value(&store, "RETURN time::diff(time::now(), time::now());"),
		Value::Duration(crate::values::Duration::default())
	);
}

#[test]
fn enforces_numeric_coercion_and_power_contract() {
	let store = Datastore::in_memory();
	assert_eq!(value(&store, "RETURN 2 + 3.5;"), Value::from(5.5_f64));
	assert_eq!(
		value(&store, "RETURN 2 + 3.5dec;"),
		Value::from("5.5dec".parse::<crate::values::Number>().unwrap())
	);
	assert_eq!(
		value(&store, "RETURN math::pow(2dec, -2);"),
		Value::from("0.25dec".parse::<crate::values::Number>().unwrap())
	);

	for source in [
		"RETURN 1.0 + 1dec;",
		"RETURN math::pow(-1.0, 0.5);",
		"RETURN math::pow(2dec, 0.5dec);",
		"RETURN 9223372036854775807 + 1;",
	] {
		let result = store.execute(source, &Object::new()).unwrap();
		assert!(result[0].result.is_err(), "{source}");
	}
}

#[test]
fn evaluates_collection_operators_and_range_boundaries() {
	let store = Datastore::in_memory();
	for expression in [
		"[1, 2] ?= 2",
		"[2, 2] *= 2",
		"2 IN [1, 2]",
		"3 NOT IN [1, 2]",
		"[1, 2] ALLIN SET[1, 2, 3]",
		"[4, 2] ANYIN SET[1, 2, 3]",
		"[4, 5] NONEIN SET[1, 2, 3]",
		"'ferros-grynn' CONTAINS 'grynn'",
		"{a: 1} CONTAINS 'a'",
		"[1, 2, 3] CONTAINSANY [0, 2]",
		"[1, 2, 3] CONTAINSALL [1, 3]",
		"[1, 2, 3] CONTAINSNOT 4",
		"3 IN 1..=3",
	] {
		assert_eq!(
			value(&store, &format!("RETURN {expression};")),
			Value::Bool(true),
			"{expression}"
		);
	}
	assert_eq!(value(&store, "RETURN [7, 8][1];"), Value::from(8_i64));
	assert_eq!(value(&store, "RETURN {a: 7}['missing'];"), Value::None);
	assert_eq!(value(&store, "FOR $x IN 1..3 { $x; };"), Value::from(2_i64));
	assert_eq!(value(&store, "FOR $x IN SET[3, 1, 2] { $x; };"), Value::from(3_i64));
	for source in ["FOR $x IN ..3 { $x; };", "FOR $x IN 1.. { $x; };"] {
		let result = store.execute(source, &Object::new()).unwrap();
		assert!(result[0].result.is_err(), "{source}");
	}
}

#[test]
fn executes_the_static_builtin_catalog() {
	let store = Datastore::in_memory();
	assert_eq!(value(&store, "RETURN math::abs(-3);"), Value::from(3_i64));
	assert_eq!(value(&store, "RETURN math::sqrt(9);"), Value::from(3.0_f64));
	assert_eq!(value(&store, "RETURN math::floor(1.9);"), Value::from(1.0_f64));
	assert_eq!(value(&store, "RETURN math::ceil(1.1);"), Value::from(2.0_f64));
	assert_eq!(value(&store, "RETURN string::lower('GRYNN');"), Value::from("grynn"));
	assert_eq!(value(&store, "RETURN string::len('жы');"), Value::from(2_i64));
	assert_eq!(value(&store, "RETURN string::contains('ferros', 'err');"), Value::Bool(true));
	assert_eq!(
		value(&store, "RETURN string::split('a,b', ',');"),
		Value::Array(vec!["a", "b"].into())
	);
	assert_eq!(
		value(&store, "RETURN string::concat('ferros', '-', 'grynn');"),
		Value::from("ferros-grynn")
	);
	assert_eq!(value(&store, "RETURN array::len([1, 2]);"), Value::from(2_i64));
	assert_eq!(
		value(&store, "RETURN array::concat([1], [2, 3]);"),
		Value::Array(vec![1_i64, 2, 3].into())
	);
	assert_eq!(value(&store, "RETURN array::first([]);"), Value::None);
	assert_eq!(
		value(&store, "RETURN array::flatten([[1, 2], 3, [4]]);"),
		Value::Array(vec![1_i64, 2, 3, 4].into())
	);
	assert_eq!(value(&store, "RETURN set::len(SET[2, 1, 2]);"), Value::from(2_i64));
	assert_eq!(
		value(&store, "RETURN set::union(SET[2, 1], SET[2, 3]);"),
		Value::Set(vec![1_i64, 2, 3].into())
	);
	assert_eq!(
		value(&store, "RETURN set::intersect(SET[1, 2], SET[2, 3]);"),
		Value::Set(vec![2_i64].into())
	);
	assert_eq!(
		value(&store, "RETURN set::diff(SET[1, 2], SET[2, 3]);"),
		Value::Set(vec![1_i64].into())
	);
	assert_eq!(value(&store, "RETURN set::has(SET[1, 2], 2);"), Value::Bool(true));
	assert_eq!(value(&store, "RETURN type::to<int>('3');"), Value::from(3_i64));
	assert_eq!(value(&store, "RETURN option::is_some(0);"), Value::Bool(true));
	assert_eq!(value(&store, "RETURN option::is_none(null);"), Value::Bool(true));
	assert_eq!(value(&store, "RETURN meta::type(time::now());"), Value::from("datetime"));
	assert_eq!(
		value(
			&store,
			"RETURN meta::type(time::add(time::now(), time::diff(time::now(), time::now())));",
		),
		Value::from("datetime")
	);
	assert!(
		value(&store, "RETURN time::format(time::now(), '%Y');")
			.as_str()
			.is_some_and(|year| year.len() == 4)
	);

	store.execute("DEFINE CLASS person TYPE NODE; CREATE person:alice;", &Object::new()).unwrap();
	assert_eq!(
		value(&store, "SELECT VALUE meta::class(id) FROM person:alice;"),
		Value::Array(vec!["person"].into())
	);
	assert_eq!(
		value(&store, "SELECT VALUE meta::id(id) FROM person:alice;"),
		Value::Array(vec!["alice"].into())
	);

	for source in [
		"RETURN math::sqrt(-1);",
		"RETURN math::pow(2, 63);",
		"RETURN math::abs(-9223372036854775807 - 1);",
		"RETURN string::len();",
		"RETURN set::union(SET[1], [2]);",
	] {
		let result = store.execute(source, &Object::new()).unwrap();
		assert!(result[0].result.is_err(), "{source}");
	}
}

#[test]
fn executes_if_case_for_and_bounded_while() {
	let store = Datastore::in_memory();
	assert_eq!(value(&store, "IF true { 7; } ELSE { 9; };"), Value::from(7_i64));
	assert_eq!(
		value(&store, "RETURN CASE 2 WHEN 1 THEN 'a' WHEN 2 THEN 'b' ELSE 'c' END;"),
		Value::from("b")
	);
	assert_eq!(
		value(&store, "FOR $x IN 1..=3 { IF $x = 2 { CONTINUE; }; $x * 2; };"),
		Value::from(6_i64)
	);
	assert_eq!(value(&store, "WHILE (true) { BREAK; };"), Value::None);

	let mut options = QueryExecutionOptions::default();
	options.max_loop_iterations = 2;
	let result =
		store.execute_with_options("FOR $x IN 1..=3 { $x; };", &Object::new(), options).unwrap();
	assert!(result[0].result.is_err());

	let mut options = QueryExecutionOptions::default();
	options.max_loop_iterations = 1;
	let result = store
		.execute_with_options("FOR $x IN 1..=9223372036854775807 { $x; };", &Object::new(), options)
		.unwrap();
	assert!(result[0].result.is_err());
}

#[test]
fn try_restores_all_mutations_and_exposes_error_value() {
	let store = Datastore::in_memory();
	store.execute("DEFINE CLASS person TYPE NODE; CREATE person:alice;", &Object::new()).unwrap();
	let result =
		value(&store, "TRY { CREATE person:bob; CREATE person:alice; } CATCH |$e| { $e.code; };");
	assert!(result.as_str().is_some_and(|code| code.starts_with("FG-")));
	let mut transaction = store.begin();
	assert!(transaction.atom(&"person:bob".parse::<RecordId>().unwrap()).unwrap().is_none());
	assert_eq!(
		value(&store, "TRY { THROW 'boom'; } CATCH |$e| { $e.message; };"),
		Value::from("boom")
	);
	assert_eq!(
		value(
			&store,
			"TRY { TRY { THROW 'first'; } CATCH |$inner| { THROW 'second'; }; } CATCH |$outer| { \
			 $outer.message; };",
		),
		Value::from("second")
	);
	let mut options = QueryExecutionOptions::default();
	options.max_loop_iterations = 1;
	let result = store
		.execute_with_options(
			"TRY { FOR $x IN 1..=2 { $x; }; } CATCH |$error| { 'caught'; };",
			&Object::new(),
			options,
		)
		.unwrap();
	assert_eq!(result[0].result.as_ref().unwrap_err().status().code().to_string(), "FG-54F09");
}

#[test]
fn assert_invariants_reads_private_root() {
	let store = Datastore::in_memory();
	let result = store
		.execute(
			"BEGIN; DEFINE GRAPH social TYPE DAG; ASSERT INVARIANTS REPORT 10; COMMIT;",
			&Object::new(),
		)
		.unwrap();
	assert_eq!(result.len(), 4, "{result:#?}");
	let report = result[2].result.as_ref().unwrap().value().as_object().unwrap();
	assert_eq!(report.get("checked_graphs"), Some(&Value::from(1_i64)));
	assert_eq!(report.get("violation_total"), Some(&Value::from(0_i64)));
	assert_eq!(report.get("violation_totals"), Some(&Value::Object(Object::new())));
	assert_eq!(report.get("violation_details"), Some(&Value::Array(Vec::<Value>::new().into())));
}

#[test]
fn assert_invariants_reports_complete_totals_with_bounded_details() {
	let mut root = DatabaseRoot::default();
	let reference = GraphRef::new(
		GraphId::new(1).unwrap(),
		"orphan",
		GraphKind::Graph,
		GraphStorageMode::InMemory,
	)
	.unwrap();
	root.graphs.insert(reference.id(), std::sync::Arc::new(GraphRoot::new(reference)));
	let store = Datastore::from_root_for_test(InMemoryOptions::default(), root);
	let results = store.execute("ASSERT INVARIANTS REPORT 0;", &Object::new()).unwrap();
	let error = results[0].result.as_ref().unwrap_err();
	let extensions = error.status().diagnostic().extensions();
	assert_eq!(extensions.get("violation_total"), Some(&DiagnosticValue::Unsigned(1)));
	assert_eq!(extensions.get("violation_details"), Some(&DiagnosticValue::List(Vec::new())));
	assert_eq!(
		value(&store, "TRY { ASSERT INVARIANTS REPORT 0; } CATCH |$error| { $error.code; };",),
		Value::from("FG-G2F05")
	);
}

#[cfg(feature = "allocation-tracking")]
#[test]
fn loop_does_not_retain_results_per_iteration() {
	fn retained_after(iterations: u64) -> usize {
		let store = Datastore::in_memory();
		let mut options = QueryExecutionOptions::default();
		options.max_loop_iterations = iterations;
		crate::memory::ALLOC.flush_local_allocations();
		let before = crate::memory::ALLOC.memory_allocated();
		let source = format!("FOR $x IN 0..{iterations} {{ $x; }};");
		let results = store.execute_with_options(&source, &Object::new(), options).unwrap();
		assert_eq!(
			results[0].result.as_ref().unwrap().value(),
			&Value::from(iterations as i64 - 1)
		);
		drop(results);
		drop(store);
		crate::memory::ALLOC.flush_local_allocations();
		crate::memory::ALLOC.memory_allocated().saturating_sub(before)
	}

	let small = retained_after(10);
	let large = retained_after(100_000);
	assert!(large <= small.saturating_add(64 * 1024), "small={small}, large={large}");
}
