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
fn stage21_string_and_random_builtins_follow_runtime_contract()
-> crate::gql_status::FerrosGrynnResult<()> {
	let store = Datastore::in_memory();
	assert_eq!(value(&store, "RETURN string::matches('graph', '^gr(a|y)ph$');"), Value::Bool(true));
	assert_eq!(value(&store, "RETURN string::slice('aЖ🙂z', 1, 3);"), Value::from("Ж🙂"));
	assert_eq!(value(&store, "RETURN meta::type(uuid());"), Value::from("uuid"));
	assert_eq!(value(&store, "RETURN meta::type(ulid());"), Value::from("ulid"));
	let random = value(&store, "RETURN rand::id(64);");
	let Value::String(random) = random else {
		return Err(crate::gql_status::FerrosGrynnError::invalid_argument(
			"rand::id must return a string",
		));
	};
	assert_eq!(random.len(), 64);
	assert!(random.bytes().all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit()));
	for source in ["RETURN rand::id(0);", "RETURN string::slice('x', 0, 2);"] {
		let result = store.execute(source, &Object::new())?;
		assert!(result[0].result.is_err(), "{source}");
	}

	let mut session = store.session();
	let begun = session
		.execute("BEGIN ISOLATION LEVEL SNAPSHOT EXECUTION MODE DETERMINISTIC;", &Object::new())?;
	assert!(begun[0].result.is_ok());
	assert!(session.execute("RETURN uuid();", &Object::new())?[0].result.is_err());
	Ok(())
}

#[test]
fn matcher_shares_state_and_reports_unicode_scalar_offsets()
-> crate::gql_status::FerrosGrynnResult<()> {
	let store = Datastore::in_memory();
	let results = store.execute(
		"LET $matcher = regex::matcher('Жа Алла Анна', 'А.+?а'); RETURN [$matcher.start, \
		 $matcher.end]; RETURN regex::next($matcher); RETURN [$matcher.start, $matcher.end]; \
		 RETURN regex::next($matcher); RETURN [$matcher.start, $matcher.end]; RETURN \
		 regex::next($matcher); RETURN [$matcher.start, $matcher.end];",
		&Object::new(),
	)?;
	let values = results
		.iter()
		.skip(1)
		.map(|result| {
			result.result.as_ref().map(|outcome| outcome.value().clone()).map_err(Clone::clone)
		})
		.collect::<crate::gql_status::FerrosGrynnResult<Vec<_>>>()?;
	assert_eq!(values, vec![
		Value::Array(vec![Value::None, Value::None].into()),
		Value::Bool(true),
		Value::Array(vec![3_i64, 7].into()),
		Value::Bool(true),
		Value::Array(vec![8_i64, 12].into()),
		Value::Bool(false),
		Value::Array(vec![8_i64, 12].into()),
	],);
	let mut session = store.session();
	session
		.execute("BEGIN ISOLATION LEVEL SNAPSHOT EXECUTION MODE DETERMINISTIC;", &Object::new())?;
	assert!(
		session.execute("RETURN regex::matcher('x', 'x');", &Object::new())?[0].result.is_err()
	);
	Ok(())
}

#[test]
fn mutable_collection_helpers_require_and_update_lexical_bindings()
-> crate::gql_status::FerrosGrynnResult<()> {
	let store = Datastore::in_memory();
	let mut session = store.session();
	let declared_array = session.execute("LET $a = [1];", &Object::new()).map_err(|error| {
		crate::gql_status::FerrosGrynnError::invalid_argument(format!(
			"array binding declaration failed: {error}"
		))
	})?;
	let declared_set = session.execute("LET $s = SET[1];", &Object::new()).map_err(|error| {
		crate::gql_status::FerrosGrynnError::invalid_argument(format!(
			"set binding declaration failed: {error}"
		))
	})?;
	assert!(declared_array.iter().all(|result| result.result.is_ok()));
	assert!(declared_set.iter().all(|result| result.result.is_ok()));

	let results = session
		.execute(
			"RETURN array::push($a, 2); RETURN array::pop($a); RETURN array::push($a, 3); RETURN \
			 $a; RETURN set::add($s, 2); RETURN set::add($s, 2); RETURN $s; RETURN \
			 array::clear($a); RETURN $a; RETURN set::clear($s); RETURN $s;",
			&Object::new(),
		)
		.map_err(|error| {
			crate::gql_status::FerrosGrynnError::invalid_argument(format!(
				"collection mutation program failed: {error}"
			))
		})?;
	let values = results
		.iter()
		.map(|result| {
			result.result.as_ref().map(|outcome| outcome.value().clone()).map_err(Clone::clone)
		})
		.collect::<crate::gql_status::FerrosGrynnResult<Vec<_>>>()?;
	assert_eq!(values, vec![
		Value::from(2_i64),
		Value::from(2_i64),
		Value::from(2_i64),
		Value::Array(vec![1_i64, 3].into()),
		Value::Bool(true),
		Value::Bool(false),
		Value::Set(vec![1_i64, 2].into()),
		Value::None,
		Value::Array(Vec::<Value>::new().into()),
		Value::None,
		Value::Set(Vec::<Value>::new().into()),
	],);

	let mut parameters = Object::new();
	parameters.insert("a", Value::Array(vec![1_i64].into()));
	let immutable_parameter = store.execute("RETURN array::push($a, 2);", &parameters);
	assert!(immutable_parameter.is_err() || immutable_parameter.unwrap()[0].result.is_err());
	let expression_target = store.execute("RETURN array::push([1], 2);", &Object::new());
	assert!(expression_target.is_err() || expression_target.unwrap()[0].result.is_err());
	let durable = store
		.execute(
			"DEFINE PARAM $durable VALUE [1]; RETURN array::push($durable, 2);",
			&Object::new(),
		)
		.map_err(|error| {
			crate::gql_status::FerrosGrynnError::invalid_argument(format!(
				"durable mutable-binding probe did not parse: {error}"
			))
		})?;
	assert!(durable[0].result.is_ok());
	assert!(durable[1].result.is_err());

	let function = store
		.execute(
			"DEFINE IMPURE FUNCTION fn::append_local(values: array) -> array { LET $_ = \
			 array::push($values, 2); RETURN $values; }; RETURN fn::append_local([1]);",
			&Object::new(),
		)
		.map_err(|error| {
			crate::gql_status::FerrosGrynnError::invalid_argument(format!(
				"mutable UDF probe did not parse: {error}"
			))
		})?;
	assert!(function[0].result.is_ok());
	assert_eq!(
		function[1].result.as_ref().map_err(Clone::clone)?.value(),
		&Value::Array(vec![1_i64, 2].into())
	);
	Ok(())
}

#[test]
fn object_conversion_merges_collections_without_silent_overwrite()
-> crate::gql_status::FerrosGrynnResult<()> {
	let store = Datastore::in_memory();
	assert_eq!(
		value(&store, "RETURN type::to<object>([{b: 2}, {a: 1}]);"),
		Value::Object([("a", 1_i64), ("b", 2_i64)].into_iter().collect())
	);
	assert_eq!(value(&store, "RETURN <object>[];"), Value::Object(Object::new()));
	assert_eq!(
		value(&store, "RETURN <object>SET[{b: 2}, {a: 1}];"),
		Value::Object([("a", 1_i64), ("b", 2_i64)].into_iter().collect())
	);
	for source in
		["RETURN type::to<object>([{a: 1}, {a: 2}]);", "RETURN type::to<object>([{a: 1}, 2]);"]
	{
		let result = store.execute(source, &Object::new())?;
		assert!(result[0].result.is_err(), "{source}");
	}
	Ok(())
}

#[test]
fn sleep_is_cooperative_and_rejected_in_deterministic_mode()
-> crate::gql_status::FerrosGrynnResult<()> {
	let store = Datastore::in_memory();
	let zero = store.execute("SLEEP 0ms;", &Object::new())?;
	assert_eq!(zero[0].kind, crate::gql::QueryStatementKind::Sleep);
	assert_eq!(zero[0].result.as_ref().map_err(Clone::clone)?.value(), &Value::None);

	let mut bindings = Object::new();
	let delay = "1ms".parse::<crate::values::Duration>().map_err(|error| {
		crate::gql_status::FerrosGrynnError::invalid_argument(error.to_string())
	})?;
	bindings.insert("delay", Value::from(delay));
	let parameter = store.execute("SLEEP $delay;", &bindings)?;
	assert!(parameter[0].result.is_ok());

	let mut options = QueryExecutionOptions::default();
	options.statement_timeout = Some(std::time::Duration::from_millis(5));
	let timed_out = store.execute_with_options("SLEEP 100ms;", &Object::new(), options)?;
	assert!(timed_out[0].result.is_err());

	let mut session = store.session();
	let begun = session
		.execute("BEGIN ISOLATION LEVEL SNAPSHOT EXECUTION MODE DETERMINISTIC;", &Object::new())?;
	assert!(begun[0].result.is_ok());
	let deterministic = session.execute("SLEEP 0ms;", &Object::new())?;
	assert!(deterministic[0].result.is_err());
	Ok(())
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
