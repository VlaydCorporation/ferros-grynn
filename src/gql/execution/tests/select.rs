use std::{
	fs,
	num::NonZeroUsize,
	path::Path,
	sync::{
		Arc,
		atomic::{AtomicBool, Ordering},
	},
	thread,
	time::{Duration, Instant},
};

use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System};

use crate::{
	datastore::{Datastore, ImportOptions, InMemoryOptions},
	gql::{QueryCancellation, QueryExecutionOptions},
	values::{Object, PropertyMap, RecordId, Value},
};

fn value(store: &Datastore, source: &str) -> Value {
	store.execute(source, &Object::new()).unwrap()[0].result.as_ref().unwrap().value().clone()
}

fn value_with_options(store: &Datastore, source: &str, options: QueryExecutionOptions) -> Value {
	store.execute_with_options(source, &Object::new(), options).unwrap()[0]
		.result
		.as_ref()
		.unwrap()
		.value()
		.clone()
}

fn rows(value: Value) -> Vec<Object> {
	let Value::Array(values) = value else {
		panic!("SELECT must return an array");
	};
	values
		.into_iter()
		.map(|value| {
			let Value::Object(object) = value else {
				panic!("SELECT row must be an object");
			};
			object
		})
		.collect()
}

fn statement_error(store: &Datastore, source: &str) -> crate::gql_status::FerrosGrynnError {
	store.execute(source, &Object::new()).unwrap()[0].result.clone().unwrap_err()
}

#[test]
fn forced_scalar_indexes_and_count_lookup_match_scan_results() {
	let store = Datastore::in_memory();
	store
		.execute(
			"DEFINE CLASS person TYPE NODE SCHEMAFULL; DEFINE FIELD age ON CLASS person TYPE int; \
			 DEFINE INDEX by_age ON CLASS person FIELDS age SCOPE BASE; DEFINE INDEX people_count \
			 ON CLASS person COUNT SCOPE BASE; CREATE person:a SET age = 10; CREATE person:b SET \
			 age = 20; CREATE person:c SET age = 30;",
			&Object::new(),
		)
		.unwrap();

	let indexed = value(
		&store,
		"SELECT VALUE age FROM person WITH INDEX by_age WHERE age IN [20, 30] ORDER BY age;",
	);
	let scanned = value(
		&store,
		"SELECT VALUE age FROM person WITH NOINDEX WHERE age IN [20, 30] ORDER BY age;",
	);
	assert_eq!(indexed, scanned);
	assert_eq!(
		value(&store, "SELECT VALUE count() FROM person WITH INDEX people_count;"),
		Value::Array(vec![Value::from(3_u64)].into()),
	);
}

#[test]
fn composite_index_prefix_and_range_match_scan_results() {
	let store = Datastore::in_memory();
	store
		.execute(
			"DEFINE CLASS person TYPE NODE SCHEMAFULL; DEFINE FIELD country ON CLASS person TYPE \
			 string; DEFINE FIELD age ON CLASS person TYPE int; DEFINE INDEX by_country_age ON \
			 CLASS person FIELDS country, age SCOPE BASE; CREATE person:a SET country = 'us', age \
			 = 10; CREATE person:b SET country = 'us', age = 20; CREATE person:c SET country = \
			 'us', age = 30; CREATE person:d SET country = 'us', age = 40; CREATE person:e SET \
			 country = 'ca', age = 25;",
			&Object::new(),
		)
		.unwrap();

	let indexed = value(
		&store,
		"SELECT VALUE age FROM person WITH INDEX by_country_age WHERE country = 'us' AND age >= \
		 20 AND age < 40 ORDER BY age;",
	);
	let scanned = value(
		&store,
		"SELECT VALUE age FROM person WITH NOINDEX WHERE country = 'us' AND age >= 20 AND age < \
		 40 ORDER BY age;",
	);
	assert_eq!(indexed, scanned);
	assert_eq!(indexed, Value::Array(vec![Value::from(20_i64), Value::from(30_i64)].into()));

	let prefix = value(
		&store,
		"SELECT VALUE age FROM person WITH INDEX by_country_age WHERE country = 'ca';",
	);
	assert_eq!(prefix, Value::Array(vec![Value::from(25_i64)].into()));
}

#[test]
fn statistics_freshness_changes_estimates_without_changing_results() {
	let store = Datastore::in_memory();
	store
		.execute(
			"DEFINE CLASS person TYPE NODE SCHEMAFULL; DEFINE FIELD score ON CLASS person TYPE \
			 int;",
			&Object::new(),
		)
		.unwrap();
	let score = store.property_dictionary().get("score").unwrap();
	store
		.import(ImportOptions::default(), |session| {
			for ordinal in 0..4 {
				session.apply_item(|transaction| {
					let mut properties = PropertyMap::new();
					properties.insert(score, Value::from(ordinal));
					transaction.create_atom(
						format!("person:r{ordinal}").parse::<RecordId>().unwrap(),
						properties,
					)
				})?;
			}
			Ok(())
		})
		.unwrap();
	let root = (*store.inner.current_root()).clone();
	let query = "SELECT VALUE id FROM person WHERE score = 2;";
	let explain = "EXPLAIN FULL FORMAT JSON SELECT VALUE id FROM person WHERE score = 2;";
	let projection = |store: &Datastore| {
		let result = value(store, query);
		let Value::Object(explain) = value(store, explain) else {
			panic!("EXPLAIN JSON must return an object");
		};
		(result, explain)
	};

	let (expected, fresh) = projection(&store);
	let mut approximate_root = root.clone();
	let approximate = std::sync::Arc::make_mut(&mut approximate_root.statistics).base_mut();
	approximate
		.property_histograms
		.values_mut()
		.for_each(|histogram| histogram.method = crate::stats::HistogramMethod::Approximate);
	let approximate_store =
		Datastore::from_root_for_test(InMemoryOptions::default(), approximate_root);
	let (approximate_result, approximate) = projection(&approximate_store);

	let mut stale_root = root.clone();
	let stale = std::sync::Arc::make_mut(&mut stale_root.statistics).base_mut();
	stale.revision = stale.analyzed_revision.saturating_add(1);
	stale.dirty_rows = 1_025;
	let stale_store = Datastore::from_root_for_test(InMemoryOptions::default(), stale_root);
	let (stale_result, stale) = projection(&stale_store);

	let mut missing_root = root;
	std::sync::Arc::make_mut(&mut missing_root.statistics).base_mut().property_histograms.clear();
	let missing_store = Datastore::from_root_for_test(InMemoryOptions::default(), missing_root);
	let (missing_result, missing) = projection(&missing_store);

	assert_eq!(approximate_result, expected);
	assert_eq!(stale_result, expected);
	assert_eq!(missing_result, expected);
	assert_eq!(fresh.get("plan_hash"), approximate.get("plan_hash"));
	assert_eq!(fresh.get("plan_hash"), stale.get("plan_hash"));
	assert_eq!(fresh.get("plan_hash"), missing.get("plan_hash"));
	assert!(
		matches!(stale.get("warnings"), Some(Value::Array(values)) if values.iter().any(|value| value.to_string().contains("statistics_stale")))
	);
	assert!(
		matches!(missing.get("warnings"), Some(Value::Array(values)) if values.iter().any(|value| value.to_string().contains("statistics_missing")))
	);
	assert!(
		matches!(approximate.get("statistics"), Some(Value::Object(statistics)) if matches!(statistics.get("property_histograms"), Some(Value::Array(values)) if values.iter().any(|value| value.to_string().contains("Approximate"))))
	);
}

#[test]
fn unique_index_supplies_forward_and_reverse_order_without_changing_results() {
	let store = Datastore::in_memory();
	store
		.execute(
			"DEFINE CLASS person TYPE NODE SCHEMAFULL; DEFINE FIELD rank ON CLASS person TYPE \
			 int; DEFINE INDEX by_rank ON CLASS person FIELDS rank UNIQUE SCOPE BASE; CREATE \
			 person:c SET rank = 3; CREATE person:a SET rank = 1; CREATE person:b SET rank = 2;",
			&Object::new(),
		)
		.unwrap();

	assert_eq!(
		value(&store, "SELECT VALUE rank FROM person ORDER BY rank ASC NULLS FIRST LIMIT 2;",),
		Value::Array(vec![Value::from(1_i64), Value::from(2_i64)].into())
	);
	assert_eq!(
		value(&store, "SELECT VALUE rank FROM person ORDER BY rank DESC LIMIT 2;"),
		Value::Array(vec![Value::from(3_i64), Value::from(2_i64)].into())
	);
}

#[test]
fn index_ordered_cursor_seeks_inside_its_exact_snapshot_without_a_spool() {
	let store = Datastore::in_memory_with(InMemoryOptions {
		max_snapshot_age: Duration::from_secs(60),
		..InMemoryOptions::default()
	});
	store
		.execute(
			"DEFINE CLASS person TYPE NODE SCHEMAFULL; DEFINE FIELD rank ON CLASS person TYPE \
			 int; DEFINE INDEX by_rank ON CLASS person FIELDS rank UNIQUE SCOPE BASE; CREATE \
			 person:a SET rank = 1; CREATE person:c SET rank = 3; CREATE person:e SET rank = 5;",
			&Object::new(),
		)
		.unwrap();
	let scratch =
		std::env::temp_dir().join(format!("fg-index-seek-{}", crate::values::Uuid::now_v7()));
	let options = QueryExecutionOptions {
		barrier_memory_bytes: NonZeroUsize::MIN,
		temporary_directory: Some(scratch.clone()),
		..QueryExecutionOptions::default()
	};
	let mut session = store.session();
	let first = session
		.execute_with_options(
			"SELECT VALUE rank FROM person WITH INDEX by_rank ORDER BY rank ASC NULLS FIRST LIMIT \
			 1;",
			&Object::new(),
			options.clone(),
		)
		.unwrap()
		.remove(0);
	assert_eq!(first.result.unwrap().into_parts().0, Value::Array(vec![Value::from(1_i64)].into()));
	assert_eq!(directory_bytes(&scratch), 0, "index seek must not materialize a spool");
	let first_cursor = first.continuation.unwrap();
	let mut read_committed_session = store.session();
	let read_committed_first = read_committed_session
		.execute_with_options(
			"SELECT VALUE rank FROM person WITH INDEX by_rank ORDER BY rank ASC NULLS FIRST LIMIT \
			 1;",
			&Object::new(),
			options.clone(),
		)
		.unwrap()
		.remove(0);
	let read_committed_cursor = read_committed_first.continuation.unwrap();

	store.execute("CREATE person:b SET rank = 2;", &Object::new()).unwrap();
	let mut variables = Object::new();
	variables.insert("cursor", Value::from(first_cursor));
	let continuation = "SELECT VALUE rank FROM person WITH INDEX by_rank ORDER BY rank ASC NULLS \
	                    FIRST LIMIT 1 START AFTER $cursor;";
	let second =
		session.execute_with_options(continuation, &variables, options.clone()).unwrap().remove(0);
	let replay =
		session.execute_with_options(continuation, &variables, options.clone()).unwrap().remove(0);
	assert_eq!(
		second.result.as_ref().unwrap().value(),
		&Value::Array(vec![Value::from(3_i64)].into())
	);
	assert_eq!(second.result.as_ref().unwrap().value(), replay.result.as_ref().unwrap().value());
	assert_eq!(second.continuation, replay.continuation);
	assert_eq!(directory_bytes(&scratch), 0, "replayed seek must not materialize a spool");
	let mut read_committed_variables = Object::new();
	read_committed_variables.insert("cursor", Value::from(read_committed_cursor));
	let read_committed = read_committed_session
		.execute_with_options(
			"SELECT VALUE rank FROM person WITH INDEX by_rank ORDER BY rank ASC NULLS FIRST LIMIT \
			 1 START AFTER $cursor READ COMMITTED;",
			&read_committed_variables,
			options.clone(),
		)
		.unwrap()
		.remove(0);
	assert_eq!(
		read_committed.result.unwrap().into_parts().0,
		Value::Array(vec![Value::from(2_i64)].into())
	);
	if let Some(cursor) = read_committed.continuation {
		read_committed_session.close_cursor(&cursor).unwrap();
	}
	assert_eq!(directory_bytes(&scratch), 0, "READ COMMITTED seek must not build a spool");

	variables.insert("cursor", Value::from(second.continuation.unwrap()));
	let last = session.execute_with_options(continuation, &variables, options).unwrap().remove(0);
	assert_eq!(last.result.unwrap().into_parts().0, Value::Array(vec![Value::from(5_i64)].into()));
	assert!(last.continuation.is_none());
	assert_eq!(directory_bytes(&scratch), 0);
	drop(fs::remove_dir_all(scratch));
}

#[test]
fn standard_index_orders_duplicate_keys_by_public_record_id() {
	let store = Datastore::in_memory();
	store
		.execute(
			"DEFINE CLASS person TYPE NODE SCHEMAFULL; DEFINE FIELD rank ON CLASS person TYPE \
			 int; DEFINE INDEX by_rank ON CLASS person FIELDS rank SCOPE BASE; CREATE person:z \
			 SET rank = 1; CREATE person:a SET rank = 1; CREATE person:m SET rank = 2;",
			&Object::new(),
		)
		.unwrap();

	for direction in ["ASC NULLS FIRST", "DESC"] {
		let indexed = value(
			&store,
			&format!("SELECT VALUE id FROM person WITH INDEX by_rank ORDER BY rank {direction};"),
		);
		let scanned = value(
			&store,
			&format!("SELECT VALUE id FROM person WITH NOINDEX ORDER BY rank {direction};"),
		);
		assert_eq!(indexed, scanned);
	}
}

#[test]
fn forced_index_must_be_ready_and_match_its_source() {
	let store = Datastore::in_memory();
	store
		.execute(
			"DEFINE CLASS person TYPE NODE SCHEMAFULL; DEFINE FIELD age ON CLASS person TYPE int; \
			 DEFINE INDEX by_age ON CLASS person FIELDS age SCOPE BASE; ALTER INDEX by_age ON \
			 CLASS person PREPARE REMOVE;",
			&Object::new(),
		)
		.unwrap();
	let error = statement_error(
		&store,
		"SELECT VALUE age FROM person WITH INDEX person.by_age WHERE age = 10;",
	);
	assert!(error.to_string().contains("not Ready"));
	let fallback =
		value(&store, "EXPLAIN FULL FORMAT JSON SELECT VALUE age FROM person WHERE age = 10;");
	let Value::Object(fallback) = fallback else {
		panic!("EXPLAIN fallback must be an object");
	};
	assert!(
		matches!(fallback.get("warnings"), Some(Value::Array(warnings)) if !warnings.is_empty())
	);
}

#[test]
fn index_hints_select_one_candidate_per_qualified_source() {
	let store = Datastore::in_memory();
	store
		.execute(
			"DEFINE CLASS person TYPE NODE SCHEMAFULL; DEFINE FIELD code ON CLASS person TYPE \
			 string; DEFINE FIELD age ON CLASS person TYPE int; DEFINE INDEX by_code ON CLASS \
			 person FIELDS code SCOPE BASE; DEFINE INDEX by_age ON CLASS person FIELDS age SCOPE \
			 BASE; DEFINE CLASS department TYPE NODE SCHEMAFULL; DEFINE FIELD code ON CLASS \
			 department TYPE string; DEFINE INDEX by_code ON CLASS department FIELDS code SCOPE \
			 BASE; CREATE person:a SET code = 'd', age = 30; CREATE department:d SET code = 'd';",
			&Object::new(),
		)
		.unwrap();

	let ambiguous = statement_error(
		&store,
		"SELECT p.id FROM person AS p JOIN department AS d ON p.code = d.code WITH INDEX by_code;",
	);
	assert!(ambiguous.to_string().contains("ambiguous"));
	let result = value(
		&store,
		"SELECT VALUE p.id FROM person AS p JOIN department AS d ON p.code = d.code WITH INDEX \
		 p.by_age, p.by_code, d.by_code WHERE p.age = 30;",
	);
	assert_eq!(
		result,
		Value::Array(vec![Value::from("person:a".parse::<RecordId>().unwrap())].into())
	);
}

#[test]
fn effective_index_matches_graph_scan_and_preserves_ties() {
	let store = Datastore::in_memory();
	store
		.execute(
			"DEFINE GRAPH social; DEFINE CLASS person TYPE NODE SCHEMAFULL; DEFINE FIELD score ON \
			 CLASS person TYPE int; DEFINE INDEX by_score ON CLASS person FIELDS score SCOPE \
			 EFFECTIVE IN GRAPH social; CREATE person:z SET score = 1; CREATE person:a SET score \
			 = 1; CREATE person:m SET score = 2; ATTACH person:z TO GRAPH social; ATTACH person:a \
			 TO GRAPH social; ATTACH person:m TO GRAPH social; UPDATE person:m IN GRAPH social \
			 SET score = 0;",
			&Object::new(),
		)
		.unwrap();

	let indexed = value(
		&store,
		"SELECT VALUE id FROM person IN GRAPH @social WITH INDEX by_score ORDER BY score ASC \
		 NULLS FIRST;",
	);
	let scanned = value(
		&store,
		"SELECT VALUE id FROM person IN GRAPH @social WITH NOINDEX ORDER BY score ASC NULLS FIRST;",
	);
	assert_eq!(indexed, scanned);
}

#[test]
fn selects_graph_catalog_rows_and_durable_properties() {
	let store = Datastore::in_memory();
	store
		.execute(
			"DEFINE GRAPH sales TYPE DAG CONTENT {owner: 'analytics'}; DEFINE GRAPH fraud CONTENT \
			 {owner: 'risk'};",
			&Object::new(),
		)
		.unwrap();

	let result = rows(value(
		&store,
		"SELECT g.name AS graph, g.properties.owner AS owner FROM GRAPHS [@fraud, @sales] AS g;",
	));
	assert_eq!(result.len(), 2);
	assert_eq!(result[0].get("graph"), Some(&Value::from("fraud")));
	assert_eq!(result[0].get("owner"), Some(&Value::from("risk")));
	assert_eq!(result[1].get("graph"), Some(&Value::from("sales")));
	assert_eq!(result[1].get("owner"), Some(&Value::from("analytics")));
}

#[test]
fn executes_join_group_order_and_derived_source() {
	let store = Datastore::in_memory();
	let setup = store
		.execute(
			"DEFINE CLASS department TYPE NODE SCHEMAFULL; DEFINE FIELD name ON CLASS department \
			 TYPE string; DEFINE FIELD code ON CLASS department TYPE string; DEFINE INDEX by_code \
			 ON CLASS department FIELDS code SCOPE BASE; DEFINE CLASS person TYPE NODE \
			 SCHEMAFULL; DEFINE FIELD name ON CLASS person TYPE string; DEFINE FIELD \
			 department_id ON CLASS person TYPE string; CREATE department:eng SET name = \
			 'Engineering', code = 'eng'; CREATE department:risk SET name = 'Risk', code = \
			 'risk'; CREATE person:alice SET name = 'Alice', department_id = 'eng'; CREATE \
			 person:bob SET name = 'Bob', department_id = 'risk'; CREATE person:carol SET name = \
			 'Carol';",
			&Object::new(),
		)
		.unwrap();
	assert!(setup.iter().all(|result| result.result.is_ok()));

	let joined = rows(value(
		&store,
		"SELECT p.name AS person, d.name AS department FROM person AS p JOIN department AS d ON \
		 p.department_id = d.code ORDER BY p.name;",
	));
	assert_eq!(joined.len(), 2);
	assert_eq!(joined[0].get("person"), Some(&Value::from("Alice")));
	assert_eq!(joined[1].get("department"), Some(&Value::from("Risk")));

	let left = rows(value(
		&store,
		"SELECT p.name AS person, d.name AS department FROM person AS p LEFT JOIN department AS d \
		 ON p.department_id = d.code ORDER BY p.name;",
	));
	assert_eq!(left.len(), 3);
	assert_eq!(left[2].get("person"), Some(&Value::from("Carol")));
	assert_eq!(left[2].get("department"), Some(&Value::None));

	let derived = rows(value(
		&store,
		"SELECT s.department, s.people FROM (SELECT department_id AS department, count() AS \
		 people FROM person GROUP BY department_id) AS s ORDER BY s.people DESC;",
	));
	assert_eq!(derived.len(), 3);
	assert_eq!(derived[0].get("people"), Some(&Value::from(1_i64)));
}

#[test]
fn dp_and_greedy_join_orders_match_fixed_left_deep_results() {
	let store = Datastore::in_memory();
	store
		.execute(
			"DEFINE CLASS sample TYPE NODE SCHEMAFULL; DEFINE FIELD code ON CLASS sample TYPE \
			 int; CREATE sample:a SET code = 1; CREATE sample:b SET code = 1;",
			&Object::new(),
		)
		.unwrap();

	for sources in [8_usize, 10] {
		let query = |kind: &str| {
			let mut query = "SELECT VALUE count() FROM sample AS s0".to_owned();
			for index in 1..sources {
				query.push_str(&format!(
					" {kind} JOIN sample AS s{index} ON s{}.code = s{index}.code",
					index - 1
				));
			}
			query.push_str(" GROUP ALL;");
			query
		};
		let reordered = value(&store, &query("INNER"));
		let fixed = value(&store, &query("LEFT"));
		assert_eq!(reordered, fixed);
		assert_eq!(reordered, Value::Array(vec![Value::from(1_u64 << sources)].into()));
	}
}

#[test]
fn executes_split_omit_only_and_scalar_subquery() {
	let store = Datastore::in_memory();
	let setup = store
		.execute(
			"DEFINE GRAPH social; DEFINE CLASS person TYPE NODE SCHEMALESS; CREATE person:alice \
			 SET name = 'Alice', tags = ['rust', 'graph']; ATTACH person:alice TO GRAPH social;",
			&Object::new(),
		)
		.unwrap();
	assert!(setup.iter().all(|result| result.result.is_ok()));

	let split = rows(value(
		&store,
		"SELECT properties.name AS name, properties.tags AS tags FROM person SPLIT \
		 properties.tags ORDER BY tags;",
	));
	assert_eq!(split.len(), 2);
	assert_eq!(split[0].get("tags"), Some(&Value::from("graph")));
	assert_eq!(split[1].get("tags"), Some(&Value::from("rust")));

	let omitted = rows(value(&store, "SELECT * FROM person OMIT properties;"));
	assert_eq!(omitted.len(), 1);
	assert!(!omitted[0].contains_key("properties"));

	assert_eq!(
		value(&store, "SELECT ONLY VALUE properties.name FROM person:alice;"),
		Value::from("Alice")
	);
	let graphs = rows(value(
		&store,
		"SELECT g.name, (SELECT VALUE count() FROM person IN GRAPH g.id) AS people FROM GRAPHS \
		 @social AS g;",
	));
	assert_eq!(graphs[0].get("people"), Some(&Value::from(1_i64)));
}

#[test]
fn persistent_session_replays_exact_ordered_pages() {
	let store = Datastore::in_memory();
	store
		.execute(
			"DEFINE CLASS person TYPE NODE SCHEMALESS; CREATE person:a SET rank = 1; CREATE \
			 person:b SET rank = 2; CREATE person:c SET rank = 3; CREATE person:d SET rank = 4; \
			 CREATE person:e SET rank = 5;",
			&Object::new(),
		)
		.unwrap();
	let query = "SELECT VALUE properties.rank FROM person ORDER BY properties.rank LIMIT 2;";
	assert!(store.execute(query, &Object::new()).unwrap()[0].continuation.is_none());

	let mut session = store.session();
	let first = session.execute(query, &Object::new()).unwrap().remove(0);
	let cursor = first.continuation.unwrap();
	assert_eq!(
		first.result.unwrap().into_parts().0,
		Value::Array(vec![Value::from(1_i64), Value::from(2_i64)].into())
	);

	let mut variables = Object::new();
	variables.insert("cursor", Value::from(cursor));
	let continuation = "SELECT VALUE properties.rank FROM person ORDER BY properties.rank LIMIT 2 \
	                    START AFTER $cursor;";
	let second = session.execute(continuation, &variables).unwrap().remove(0);
	let replay = session.execute(continuation, &variables).unwrap().remove(0);
	assert_eq!(second.result.as_ref().unwrap().value(), replay.result.as_ref().unwrap().value());
	assert_eq!(second.continuation, replay.continuation);

	let mut other = store.session();
	let rejected = other.execute(continuation, &variables).unwrap().remove(0);
	assert!(rejected.result.is_err());

	variables.insert("cursor", Value::from(second.continuation.unwrap()));
	let last = session.execute(continuation, &variables).unwrap().remove(0);
	assert!(last.continuation.is_none());
	assert_eq!(last.result.unwrap().into_parts().0, Value::Array(vec![Value::from(5_i64)].into()));
}

#[test]
fn cancellation_closes_the_supplied_cursor_spool() {
	let store = Datastore::in_memory();
	store
		.execute(
			"DEFINE CLASS person TYPE NODE SCHEMALESS; CREATE person:a SET rank = 1; CREATE \
			 person:b SET rank = 2;",
			&Object::new(),
		)
		.unwrap();
	let mut session = store.session();
	let first = session
		.execute(
			"SELECT VALUE properties.rank FROM person ORDER BY properties.rank LIMIT 1;",
			&Object::new(),
		)
		.unwrap()
		.remove(0);
	let cursor = first.continuation.unwrap();
	let mut variables = Object::new();
	variables.insert("cursor", Value::from(cursor));
	let cancellation = QueryCancellation::default();
	cancellation.cancel();
	let cancelled = session
		.execute_with_options(
			"SELECT VALUE properties.rank FROM person ORDER BY properties.rank LIMIT 1 START \
			 AFTER $cursor;",
			&variables,
			QueryExecutionOptions {
				cancellation: Some(cancellation),
				..QueryExecutionOptions::default()
			},
		)
		.unwrap()
		.remove(0)
		.result
		.unwrap_err();
	assert_eq!(cancelled.status().code().to_string(), "FG-50F05");
	let closed = session
		.execute(
			"SELECT VALUE properties.rank FROM person ORDER BY properties.rank LIMIT 1 START \
			 AFTER $cursor;",
			&variables,
		)
		.unwrap()
		.remove(0)
		.result
		.unwrap_err();
	assert_eq!(closed.status().code().to_string(), "FG-22F20");
}

#[test]
fn spills_ordered_cursor_and_removes_its_run_on_close() {
	let store = Datastore::in_memory();
	store
		.execute(
			"DEFINE CLASS person TYPE NODE SCHEMALESS; CREATE person:a SET rank = 3; CREATE \
			 person:b SET rank = 1; CREATE person:c SET rank = 2;",
			&Object::new(),
		)
		.unwrap();
	let scratch =
		std::env::temp_dir().join(format!("fg-select-spill-{}", crate::values::Uuid::now_v7()));
	let options = QueryExecutionOptions {
		barrier_memory_bytes: NonZeroUsize::new(64).unwrap(),
		temporary_directory: Some(scratch.clone()),
		..QueryExecutionOptions::default()
	};
	let mut session = store.session();
	let result = session
		.execute_with_options(
			"SELECT VALUE properties.rank FROM person ORDER BY properties.rank LIMIT 1 TEMPFILES;",
			&Object::new(),
			options,
		)
		.unwrap()
		.remove(0);
	assert_eq!(
		result.result.as_ref().unwrap().value(),
		&Value::Array(vec![Value::from(1_i64)].into())
	);
	let cursor = result.continuation.expect("ordered spill must retain a cursor");
	let run_root = scratch.join("tmp").join("query");
	assert_eq!(std::fs::read_dir(&run_root).unwrap().count(), 1);
	session.close_cursor(&cursor).unwrap();
	assert_eq!(std::fs::read_dir(&run_root).unwrap().count(), 0);
	drop(std::fs::remove_dir_all(scratch));
}

#[test]
fn bounded_streaming_select_stops_without_creating_query_runs() {
	let store = Datastore::in_memory();
	store
		.execute(
			"DEFINE CLASS person TYPE NODE SCHEMALESS; CREATE person:a SET rank = 1; CREATE \
			 person:b SET rank = 2; CREATE person:c SET rank = 3;",
			&Object::new(),
		)
		.unwrap();
	let scratch =
		std::env::temp_dir().join(format!("fg-select-stream-{}", crate::values::Uuid::now_v7()));
	let options = QueryExecutionOptions {
		barrier_memory_bytes: NonZeroUsize::new(64).unwrap(),
		temporary_directory: Some(scratch.clone()),
		..QueryExecutionOptions::default()
	};
	assert_eq!(
		value_with_options(
			&store,
			"SELECT VALUE properties.rank FROM person LIMIT 1;",
			options.clone(),
		),
		Value::Array(vec![Value::from(1_i64)].into())
	);
	assert_eq!(
		value_with_options(&store, "SELECT VALUE properties.rank FROM person LIMIT 0;", options,),
		Value::Array(Vec::<Value>::new().into())
	);
	assert!(!scratch.join("tmp").join("query").exists());
	drop(std::fs::remove_dir_all(scratch));
}

#[test]
fn partitioned_join_and_group_match_the_in_memory_result() {
	let store = Datastore::in_memory();
	store
		.execute(
			"DEFINE CLASS department TYPE NODE SCHEMAFULL; DEFINE FIELD code ON CLASS department \
			 TYPE string; DEFINE FIELD name ON CLASS department TYPE string; DEFINE CLASS person \
			 TYPE NODE SCHEMAFULL; DEFINE FIELD department ON CLASS person TYPE string; DEFINE \
			 FIELD score ON CLASS person TYPE int; CREATE department:a SET code = 'a', name = \
			 'A'; CREATE department:b SET code = 'b', name = 'B'; CREATE person:a SET department \
			 = 'b', score = 1; CREATE person:b SET department = 'a', score = 2; CREATE person:c \
			 SET department = 'b', score = 2; CREATE person:d SET score = 3;",
			&Object::new(),
		)
		.unwrap();
	let join = "SELECT p.id AS person, d.name AS department FROM person AS p LEFT JOIN department \
	            AS d ON p.department = d.code;";
	let expected = value(&store, join);
	let scratch = std::env::temp_dir()
		.join(format!("fg-select-partitioned-{}", crate::values::Uuid::now_v7()));
	let options = QueryExecutionOptions {
		barrier_memory_bytes: NonZeroUsize::new(64).unwrap(),
		temporary_directory: Some(scratch.clone()),
		..QueryExecutionOptions::default()
	};
	assert_eq!(value_with_options(&store, join, options.clone()), expected);
	let aggregate = "SELECT count() AS count, count(DISTINCT score) AS distinct_scores, \
	                 math::variance_pop(score) AS variance FROM person GROUP ALL TEMPFILES;";
	let grouped = rows(value_with_options(&store, aggregate, options));
	assert_eq!(grouped[0].get("count"), Some(&Value::from(4_i64)));
	assert_eq!(grouped[0].get("distinct_scores"), Some(&Value::from(3_i64)));
	assert_eq!(grouped[0].get("variance"), Some(&Value::from(0.5_f64)));
	drop(std::fs::remove_dir_all(scratch));
}

#[test]
fn resolves_graph_sources_wildcards_effective_properties_and_ambiguity() {
	let store = Datastore::in_memory();
	let setup = store
		.execute(
			"DEFINE GRAPH sales CONTENT {owner: 'analytics'}; DEFINE GRAPH fraud; DEFINE GRAPH \
			 social; DEFINE CLASS strict TYPE NODE SCHEMAFULL; DEFINE FIELD name ON CLASS strict \
			 TYPE string; DEFINE CLASS loose TYPE NODE SCHEMALESS; CREATE strict:a SET name = \
			 'A'; CREATE loose:x SET z = 3, a = 1; ATTACH loose:x TO GRAPH social; UPDATE loose:x \
			 IN GRAPH social SET a = 9, extra = 'scoped';",
			&Object::new(),
		)
		.unwrap();
	assert!(setup.iter().all(|result| result.result.is_ok()));

	let all_graphs = rows(value(&store, "SELECT name FROM GRAPHS;"));
	assert_eq!(
		all_graphs.iter().map(|row| row.get("name").cloned().unwrap()).collect::<Vec<_>>(),
		vec![Value::from("sales"), Value::from("fraud"), Value::from("social")]
	);
	let one_graph = rows(value(&store, "SELECT name, properties.owner FROM GRAPH @sales;"));
	assert_eq!(one_graph[0].get("name"), Some(&Value::from("sales")));
	assert_eq!(one_graph[0].get("owner"), Some(&Value::from("analytics")));

	let strict = rows(value(&store, "SELECT * FROM strict;"));
	assert!(strict[0].contains_key("id"));
	assert_eq!(strict[0].get("name"), Some(&Value::from("A")));
	assert!(!strict[0].contains_key("properties"));
	let loose = rows(value(&store, "SELECT * FROM loose IN GRAPH @social;"));
	assert!(loose[0].contains_key("id"));
	assert_eq!(loose[0].get("a"), Some(&Value::from(9_i64)));
	assert_eq!(loose[0].get("z"), Some(&Value::from(3_i64)));
	assert_eq!(loose[0].get("extra"), Some(&Value::from("scoped")));

	let error =
		statement_error(&store, "SELECT id FROM strict AS left CROSS JOIN strict AS right;");
	assert_eq!(error.status().code().to_string(), "FG-22N11");
}

#[test]
fn preserves_join_bags_and_subquery_cardinality() {
	let store = Datastore::in_memory();
	let setup = store
		.execute(
			"DEFINE CLASS department TYPE NODE SCHEMALESS; DEFINE CLASS person TYPE NODE \
			 SCHEMALESS; CREATE department:a SET code = 'eng', name = 'A'; CREATE department:b \
			 SET code = 'eng', name = 'B'; CREATE department:c SET code = null, name = 'C'; \
			 CREATE person:alice SET department = 'eng', rank = 1; CREATE person:bob SET \
			 department = null, rank = 2; CREATE person:carol SET rank = 3;",
			&Object::new(),
		)
		.unwrap();
	assert!(setup.iter().all(|result| result.result.is_ok()));

	let joined = rows(value(
		&store,
		"SELECT p.id AS person, d.id AS department FROM person AS p LEFT JOIN department AS d ON \
		 p.properties.department = d.properties.code ORDER BY p.id, d.id;",
	));
	assert_eq!(joined.len(), 4);
	assert_eq!(joined.iter().filter(|row| row.get("department") == Some(&Value::None)).count(), 2);

	assert_eq!(
		value(
			&store,
			"SELECT VALUE (SELECT VALUE q.properties.rank FROM person AS q WHERE \
			 q.properties.rank = 99) FROM person:alice AS p;",
		),
		Value::Array(vec![Value::None].into())
	);
	let cardinality = statement_error(
		&store,
		"SELECT VALUE (SELECT VALUE q.properties.rank FROM person AS q) FROM person:alice AS p;",
	);
	assert_eq!(cardinality.status().code().to_string(), "FG-22F19");
	assert_eq!(
		value(
			&store,
			"SELECT VALUE p.properties.rank IN (SELECT VALUE q.properties.rank FROM person AS q \
			 WHERE q.properties.rank = 1) FROM person:alice AS p;",
		),
		Value::Array(vec![Value::from(true)].into())
	);
	assert!(
		store
			.execute(
				"SELECT VALUE p.properties.rank IN (SELECT q.properties.rank FROM person AS q) \
				 FROM person:alice AS p;",
				&Object::new(),
			)
			.unwrap()[0]
			.result
			.is_err()
	);
}

#[test]
fn fetches_records_and_graphs_without_exposing_storage_ids() {
	let store = Datastore::in_memory();
	store
		.execute(
			"DEFINE GRAPH social CONTENT {owner: 'team'}; DEFINE CLASS person TYPE NODE \
			 SCHEMALESS; CREATE person:bob SET name = 'Bob'; CREATE person:alice SET name = \
			 'Alice';",
			&Object::new(),
		)
		.unwrap();
	let mut record_variables = Object::new();
	record_variables
		.insert("friend", Value::from("person:bob".parse::<crate::values::RecordId>().unwrap()));
	let update =
		store.execute("UPDATE person:alice SET friend = $friend;", &record_variables).unwrap();
	assert!(update[0].result.is_ok());
	let fetched =
		rows(value(&store, "SELECT properties.friend AS friend FROM person:alice FETCH friend;"));
	let Some(Value::Object(friend)) = fetched[0].get("friend") else {
		panic!("FETCH must materialize a record object");
	};
	let Some(Value::Object(properties)) = friend.get("properties") else {
		panic!("fetched record must retain its property object");
	};
	assert_eq!(properties.get("name"), Some(&Value::from("Bob")));
	assert!(!friend.contains_key("atom_id"));

	let graph = value(&store, "SELECT ONLY VALUE id FROM GRAPH @social;");
	let mut variables = Object::new();
	variables.insert("graph", graph);
	let update =
		store.execute("UPDATE GRAPH social SET properties.related = $graph;", &variables).unwrap();
	assert!(update[0].result.is_ok());
	let graph = rows(value(
		&store,
		"SELECT properties.related AS related FROM GRAPH @social FETCH related;",
	));
	let Some(Value::Object(related)) = graph[0].get("related") else {
		panic!("FETCH must materialize graph metadata");
	};
	assert_eq!(related.get("name"), Some(&Value::from("social")));
	assert_eq!(related.get("node_count"), Some(&Value::from(0_i64)));
}

#[test]
fn implements_empty_and_numeric_aggregate_contracts() {
	let store = Datastore::in_memory();
	store
		.execute(
			"DEFINE CLASS sample TYPE NODE SCHEMALESS; DEFINE CLASS empty TYPE NODE SCHEMALESS; \
			 CREATE sample:a SET score = 1; CREATE sample:b SET score = 2; CREATE sample:c SET \
			 score = 3; CREATE sample:d SET score = null; CREATE sample:e SET label = 'missing';",
			&Object::new(),
		)
		.unwrap();
	let aggregate = rows(value(
		&store,
		"SELECT count() AS rows, count(properties.score) AS present, sum(properties.score) AS \
		 total, avg(properties.score) AS average, min(properties.score) AS minimum, \
		 max(properties.score) AS maximum, math::variance_pop(properties.score) AS variance_pop, \
		 math::variance_samp(properties.score) AS variance_samp, \
		 math::stddev_pop(properties.score) AS stddev_pop, math::stddev_samp(properties.score) AS \
		 stddev_samp FROM sample GROUP ALL;",
	));
	let row = &aggregate[0];
	assert_eq!(row.get("rows"), Some(&Value::from(5_i64)));
	assert_eq!(row.get("present"), Some(&Value::from(3_i64)));
	assert_eq!(row.get("total"), Some(&Value::from(6_i64)));
	assert_eq!(row.get("average"), Some(&Value::from(2.0_f64)));
	assert_eq!(row.get("minimum"), Some(&Value::from(1_i64)));
	assert_eq!(row.get("maximum"), Some(&Value::from(3_i64)));
	assert_eq!(row.get("variance_pop"), Some(&Value::from(2.0_f64 / 3.0)));
	assert_eq!(row.get("variance_samp"), Some(&Value::from(1.0_f64)));
	assert_eq!(row.get("stddev_pop"), Some(&Value::from((2.0_f64 / 3.0).sqrt())));
	assert_eq!(row.get("stddev_samp"), Some(&Value::from(1.0_f64)));

	let empty = rows(value(
		&store,
		"SELECT count() AS rows, sum(properties.score) AS total, avg(properties.score) AS average \
		 FROM empty GROUP ALL;",
	));
	assert_eq!(empty[0].get("rows"), Some(&Value::from(0_i64)));
	assert_eq!(empty[0].get("total"), Some(&Value::from(0_i64)));
	assert_eq!(empty[0].get("average"), Some(&Value::None));
}

#[test]
fn groups_collections_and_rejects_illegal_aggregate_shapes() {
	let store = Datastore::in_memory();
	store
		.execute(
			"DEFINE CLASS sample TYPE NODE SCHEMALESS; CREATE sample:a SET item = [1, 2]; CREATE \
			 sample:b SET item = null; CREATE sample:c SET item = [3]; CREATE sample:d SET label \
			 = 'missing';",
			&Object::new(),
		)
		.unwrap();
	let grouped =
		rows(value(&store, "SELECT array::group(properties.item) AS items FROM sample GROUP ALL;"));
	assert_eq!(
		grouped[0].get("items"),
		Some(&Value::Array(
			vec![Value::from(1_i64), Value::from(2_i64), Value::Null, Value::from(3_i64),].into()
		))
	);
	assert_eq!(
		rows(value(
			&store,
			"SELECT properties.item AS item, count() AS count FROM sample GROUP BY \
			 properties.item ORDER BY count() DESC;",
		))
		.len(),
		4
	);

	assert!(
		store
			.execute(
				"SELECT properties.label, count() FROM sample GROUP BY properties.item;",
				&Object::new(),
			)
			.unwrap()[0]
			.result
			.is_err()
	);
	assert!(
		store
			.execute("SELECT sum(count(properties.item)) FROM sample GROUP ALL;", &Object::new(),)
			.unwrap()[0]
			.result
			.is_err()
	);
	assert!(
		store
			.execute(
				"SELECT count() AS count FROM sample ORDER BY properties.item;",
				&Object::new(),
			)
			.unwrap()[0]
			.result
			.is_err()
	);
}

#[test]
fn applies_inner_limits_and_only_to_derived_sources() {
	let store = Datastore::in_memory();
	store
		.execute(
			"DEFINE CLASS sample TYPE NODE SCHEMALESS; CREATE sample:a SET rank = 1; CREATE \
			 sample:b SET rank = 2; CREATE sample:c SET rank = 3;",
			&Object::new(),
		)
		.unwrap();
	let rows = rows(value(
		&store,
		"SELECT s.rank FROM (SELECT properties.rank AS rank FROM sample ORDER BY properties.rank \
		 DESC LIMIT 2) AS s ORDER BY s.rank;",
	));
	assert_eq!(rows.len(), 2);
	assert_eq!(rows[0].get("rank"), Some(&Value::from(2_i64)));
	assert_eq!(rows[1].get("rank"), Some(&Value::from(3_i64)));

	let error = statement_error(
		&store,
		"SELECT s.rank FROM (SELECT ONLY properties.rank AS rank FROM sample) AS s;",
	);
	assert_eq!(error.status().code().to_string(), "FG-22F22");
}

#[test]
fn constant_folding_preserves_empty_source_error_semantics() {
	let store = Datastore::in_memory();
	store.execute("DEFINE CLASS sample TYPE NODE;", &Object::new()).unwrap();
	assert_eq!(
		value(&store, "SELECT VALUE 1 + 2 FROM sample;"),
		Value::Array(Vec::<Value>::new().into())
	);
	assert_eq!(
		value(&store, "SELECT VALUE 1 / 0 FROM sample;"),
		Value::Array(Vec::<Value>::new().into())
	);
	store.execute("CREATE sample:one;", &Object::new()).unwrap();
	assert_eq!(
		value(&store, "SELECT VALUE 1 + 2 FROM sample;"),
		Value::Array(vec![Value::from(3_i64)].into())
	);
}

#[test]
fn expires_exact_cursors_and_reexecutes_read_committed_after_anchor() {
	let store = Datastore::in_memory_with(InMemoryOptions {
		max_snapshot_age: Duration::from_millis(100),
		..InMemoryOptions::default()
	});
	store
		.execute(
			"DEFINE CLASS person TYPE NODE SCHEMALESS; CREATE person:a SET rank = 1; CREATE \
			 person:c SET rank = 3; CREATE person:e SET rank = 5;",
			&Object::new(),
		)
		.unwrap();
	let query = "SELECT VALUE properties.rank FROM person ORDER BY properties.rank LIMIT 1;";
	let mut session = store.session();
	let first = session.execute(query, &Object::new()).unwrap().remove(0);
	let cursor = first.continuation.unwrap();
	store.execute("CREATE person:b SET rank = 2;", &Object::new()).unwrap();
	let mut variables = Object::new();
	variables.insert("cursor", Value::from(cursor));
	let rc = session
		.execute(
			"SELECT VALUE properties.rank FROM person ORDER BY properties.rank LIMIT 2 START \
			 AFTER $cursor READ COMMITTED;",
			&variables,
		)
		.unwrap()
		.remove(0);
	assert_eq!(
		rc.result.unwrap().into_parts().0,
		Value::Array(vec![Value::from(2_i64), Value::from(3_i64)].into())
	);

	let first = session.execute(query, &Object::new()).unwrap().remove(0);
	let cursor = first.continuation.unwrap();
	thread::sleep(Duration::from_millis(150));
	variables.insert("cursor", Value::from(cursor));
	let expired = session
		.execute(
			"SELECT VALUE properties.rank FROM person ORDER BY properties.rank LIMIT 1 START \
			 AFTER $cursor;",
			&variables,
		)
		.unwrap()
		.remove(0)
		.result
		.unwrap_err();
	assert_eq!(expired.status().code().to_string(), "FG-25F07");

	let first = session.execute(query, &Object::new()).unwrap().remove(0);
	let cursor = first.continuation.unwrap();
	thread::sleep(Duration::from_millis(150));
	variables.insert("cursor", Value::from(cursor));
	let refreshed = session
		.execute(
			"SELECT VALUE properties.rank FROM person ORDER BY properties.rank LIMIT 1 START \
			 AFTER $cursor READ COMMITTED;",
			&variables,
		)
		.unwrap()
		.remove(0)
		.result
		.unwrap()
		.into_parts()
		.0;
	assert_eq!(refreshed, Value::Array(vec![Value::from(2_i64)].into()));
}

#[test]
fn enforces_cursor_limit_and_order_offset_contract() {
	let store = Datastore::in_memory();
	store
		.execute(
			"DEFINE CLASS person TYPE NODE SCHEMALESS; CREATE person:a SET rank = 2; CREATE \
			 person:b SET rank = null; CREATE person:c SET rank = 1; CREATE person:d SET name = \
			 'missing'; CREATE person:e SET rank = 2;",
			&Object::new(),
		)
		.unwrap();
	let page = rows(value(
		&store,
		"SELECT id, properties.rank AS rank FROM person ORDER BY rank ASC NULLS FIRST START AT 1 \
		 LIMIT 3;",
	));
	assert_eq!(page.len(), 3);
	assert!(matches!(page[0].get("rank"), Some(Value::Null | Value::None)));
	assert_eq!(page[1].get("rank"), Some(&Value::from(1_i64)));
	assert_eq!(page[2].get("rank"), Some(&Value::from(2_i64)));

	let only = statement_error(&store, "SELECT ONLY VALUE id FROM person;");
	assert_eq!(only.status().code().to_string(), "FG-22F22");
	assert_eq!(value(&store, "SELECT ONLY VALUE id FROM person WHERE false;"), Value::None);
	let Value::Array(random) =
		value(&store, "SELECT VALUE id FROM person ORDER BY RAND() LIMIT 1;")
	else {
		panic!("RAND ordering must return a SELECT array");
	};
	assert_eq!(random.len(), 1);

	let mut session = store.session();
	let query = "SELECT VALUE id FROM person ORDER BY id LIMIT 1;";
	for _index in 0..64 {
		let result = session.execute(query, &Object::new()).unwrap().remove(0);
		assert!(result.result.is_ok());
		assert!(result.continuation.is_some());
	}
	let limit = session.execute(query, &Object::new()).unwrap().remove(0).result.unwrap_err();
	assert_eq!(limit.status().code().to_string(), "FG-54F10");
}

#[test]
fn splits_cartesian_fields_and_keeps_null_and_none_group_keys_distinct() {
	let store = Datastore::in_memory();
	store
		.execute(
			"DEFINE CLASS item TYPE NODE SCHEMALESS; CREATE item:a SET left = [1, 2], right = \
			 ['x', 'y'], group = null; CREATE item:b SET group = 1; CREATE item:c SET group = 1; \
			 CREATE item:d SET label = 'missing';",
			&Object::new(),
		)
		.unwrap();
	let split = rows(value(
		&store,
		"SELECT properties.left AS left, properties.right AS right FROM item:a SPLIT \
		 properties.left, properties.right ORDER BY left, right;",
	));
	assert_eq!(split.len(), 4);
	assert_eq!(split[0].get("left"), Some(&Value::from(1_i64)));
	assert_eq!(split[0].get("right"), Some(&Value::from("x")));
	assert_eq!(split[3].get("left"), Some(&Value::from(2_i64)));
	assert_eq!(split[3].get("right"), Some(&Value::from("y")));

	let groups = rows(value(
		&store,
		"SELECT properties.group AS key, count() AS count FROM item GROUP BY properties.group \
		 ORDER BY key NULLS FIRST;",
	));
	assert_eq!(groups.len(), 3);
	assert!(groups.iter().any(|row| {
		row.get("key") == Some(&Value::Null) && row.get("count") == Some(&Value::from(1_i64))
	}));
	assert!(groups.iter().any(|row| {
		row.get("key") == Some(&Value::None) && row.get("count") == Some(&Value::from(1_i64))
	}));
	assert!(groups.iter().any(|row| {
		row.get("key") == Some(&Value::from(1_i64)) && row.get("count") == Some(&Value::from(2_i64))
	}));
}

#[test]
#[ignore = "stage-13 profiling gate"]
fn gql_stage_13_simple_select_probe() {
	let sample_count = std::env::var("FG_SELECT_SAMPLES")
		.ok()
		.and_then(|value| value.parse::<usize>().ok())
		.unwrap_or(15)
		.max(1);
	let row_count = std::env::var("FG_SELECT_ROWS")
		.ok()
		.and_then(|value| value.parse::<usize>().ok())
		.unwrap_or(10_000);
	let limit = std::env::var("FG_SELECT_LIMIT")
		.ok()
		.and_then(|value| value.parse::<usize>().ok())
		.unwrap_or(row_count);
	let store = Datastore::in_memory();
	store
		.execute(
			"DEFINE CLASS person TYPE NODE; DEFINE FIELD score ON CLASS person TYPE int; CREATE \
			 person:seed SET score = 0;",
			&Object::new(),
		)
		.unwrap();
	let key = store.property_dictionary().get("score").unwrap();
	store
		.import(
			ImportOptions {
				segment_size: 20_000,
				..ImportOptions::default()
			},
			|session| {
				for ordinal in 1..row_count {
					session.apply_item(|transaction| {
						let mut properties = PropertyMap::new();
						properties.insert(key, Value::from(ordinal));
						transaction.create_atom(
							format!("person:r{ordinal}").parse::<RecordId>().unwrap(),
							properties,
						)
					})?;
				}
				Ok(())
			},
		)
		.unwrap();
	let query = format!("SELECT VALUE score FROM person LIMIT {limit};");
	for _iteration in 0..3 {
		std::hint::black_box(value(&store, &query));
	}
	let mut samples = Vec::with_capacity(sample_count);
	for _sample in 0..sample_count {
		let started = Instant::now();
		let selected = value(&store, &query);
		let elapsed = started.elapsed().as_nanos() as f64;
		let Value::Array(values) = selected else {
			panic!("SELECT probe must return an array");
		};
		assert_eq!(values.len(), limit.min(row_count));
		std::hint::black_box(values);
		samples.push(elapsed);
	}
	samples.sort_by(f64::total_cmp);
	let median_ns = samples[sample_count / 2];
	eprintln!(
		"stage13 simple select: rows={row_count}, limit={limit}, median_ns={median_ns:.2}, \
		 ns_per_returned_row={:.2}",
		median_ns / limit.min(row_count).max(1) as f64,
	);
}

#[test]
#[ignore = "stage-13 resource and barrier profiling gate"]
fn gql_stage_13_resource_probe() {
	let row_count = profile_usize("FG_SELECT_ROWS", 100_000);
	let limit = profile_usize("FG_SELECT_LIMIT", 100).min(row_count);
	let budget = profile_usize("FG_SELECT_BARRIER_BYTES", 4 * 1024 * 1024).max(1);
	let payload_bytes = profile_usize("FG_SELECT_PAYLOAD_BYTES", 64);
	let mode = std::env::var("FG_SELECT_MODE").unwrap_or_else(|_| "order".to_owned());
	let force_tempfiles = std::env::var("FG_SELECT_TEMPFILES").is_ok_and(|value| value == "1");
	let scratch = std::env::temp_dir()
		.join(format!("fg-select-stage13-profile-{}", crate::values::Uuid::now_v7()));
	let setup_started = Instant::now();
	let store = stage13_resource_store(row_count, payload_bytes);
	let setup_elapsed = setup_started.elapsed();
	let query = stage13_resource_query(&mode, limit, force_tempfiles);
	let options = QueryExecutionOptions {
		barrier_memory_bytes: NonZeroUsize::new(budget).unwrap(),
		temporary_directory: Some(scratch.clone()),
		..QueryExecutionOptions::default()
	};
	let rss_before = profile_rss();
	let running = Arc::new(AtomicBool::new(true));
	let sampler = {
		let running = running.clone();
		let scratch = scratch.clone();
		thread::spawn(move || profile_resources(&running, &scratch, rss_before))
	};
	let started = Instant::now();
	let mut session = store.session();
	let result = session.execute_with_options(&query, &Object::new(), options).unwrap().remove(0);
	let elapsed = started.elapsed();
	let value = result.result.as_ref().unwrap().value();
	std::hint::black_box(value);
	let retained_temp_bytes = directory_bytes(&scratch);
	if let Some(cursor) = result.continuation {
		session.close_cursor(&cursor).unwrap();
	}
	running.store(false, Ordering::Release);
	let (rss_peak, temp_peak) = sampler.join().unwrap();
	drop(session);
	assert_eq!(directory_bytes(&scratch), 0, "query-run files must be released");
	drop(fs::remove_dir_all(&scratch));
	eprintln!(
		"stage13 resource: mode={mode}, rows={row_count}, limit={limit}, budget={budget}, \
		 payload_bytes={payload_bytes}, tempfiles={force_tempfiles}, setup_ns={}, elapsed_ns={}, \
		 rss_before={rss_before}, rss_peak={rss_peak}, rss_delta={}, temp_peak={}, \
		 retained_temp_bytes={retained_temp_bytes}",
		setup_elapsed.as_nanos(),
		elapsed.as_nanos(),
		rss_peak.saturating_sub(rss_before),
		temp_peak.max(retained_temp_bytes),
	);
}

pub(super) fn stage13_resource_store(row_count: usize, payload_bytes: usize) -> Datastore {
	let store = Datastore::in_memory_with(InMemoryOptions {
		max_snapshot_age: Duration::from_secs(3_600),
		..InMemoryOptions::default()
	});
	store
		.execute(
			"DEFINE CLASS person TYPE NODE; DEFINE FIELD score ON CLASS person TYPE int; DEFINE \
			 FIELD bucket ON CLASS person TYPE int; DEFINE FIELD department ON CLASS person TYPE \
			 string; DEFINE FIELD payload ON CLASS person TYPE string; DEFINE CLASS department \
			 TYPE NODE; DEFINE FIELD code ON CLASS department TYPE string; DEFINE FIELD name ON \
			 CLASS department TYPE string;",
			&Object::new(),
		)
		.unwrap();
	let dictionary = store.property_dictionary();
	let score = dictionary.get("score").unwrap();
	let bucket = dictionary.get("bucket").unwrap();
	let department = dictionary.get("department").unwrap();
	let payload = dictionary.get("payload").unwrap();
	let code = dictionary.get("code").unwrap();
	let name = dictionary.get("name").unwrap();
	let payload_value = "x".repeat(payload_bytes);
	store
		.import(
			ImportOptions {
				segment_size: 20_000,
				..ImportOptions::default()
			},
			|session| {
				for ordinal in 0..row_count {
					session.apply_item(|transaction| {
						let mut properties = PropertyMap::with_capacity(4);
						properties.insert(score, Value::from(ordinal));
						properties.insert(bucket, Value::from(ordinal % 1_024));
						properties.insert(department, format!("d{:04}", ordinal % 1_024));
						properties.insert(payload, payload_value.clone());
						transaction.create_atom(
							format!("person:r{ordinal}").parse::<RecordId>().unwrap(),
							properties,
						)
					})?;
				}
				for ordinal in 0..1_024.min(row_count) {
					session.apply_item(|transaction| {
						let mut properties = PropertyMap::with_capacity(2);
						let identity = format!("d{ordinal:04}");
						properties.insert(code, identity.clone());
						properties.insert(name, format!("Department {ordinal}"));
						transaction.create_atom(
							format!("department:{identity}").parse::<RecordId>().unwrap(),
							properties,
						)
					})?;
				}
				Ok(())
			},
		)
		.unwrap();
	store
}

fn stage13_resource_query(mode: &str, limit: usize, force_tempfiles: bool) -> String {
	let tempfiles = if force_tempfiles {
		" TEMPFILES"
	} else {
		""
	};
	match mode {
		"simple" => format!("SELECT VALUE score FROM person LIMIT {limit};"),
		"order" => {
			format!("SELECT VALUE score FROM person ORDER BY score DESC LIMIT {limit}{tempfiles};")
		}
		"group" => format!(
			"SELECT bucket, count() AS total, sum(score) AS score_sum FROM person GROUP BY bucket \
			 ORDER BY bucket{tempfiles};"
		),
		"join" => format!(
			"SELECT p.id AS person, d.name AS department FROM person AS p LEFT JOIN department AS \
			 d ON p.department = d.code LIMIT {limit}{tempfiles};"
		),
		other => panic!("unsupported FG_SELECT_MODE '{other}'"),
	}
}

pub(super) fn profile_usize(name: &str, default: usize) -> usize {
	std::env::var(name).ok().and_then(|value| value.parse().ok()).unwrap_or(default)
}

fn profile_resources(running: &AtomicBool, scratch: &Path, initial_rss: u64) -> (u64, u64) {
	let pid = Pid::from(usize::try_from(std::process::id()).unwrap());
	let mut system = System::new();
	let mut rss_peak = initial_rss;
	let mut temp_peak = 0;
	while running.load(Ordering::Acquire) {
		system.refresh_processes_specifics(
			ProcessesToUpdate::Some(&[pid]),
			true,
			ProcessRefreshKind::nothing().with_memory(),
		);
		if let Some(process) = system.process(pid) {
			rss_peak = rss_peak.max(process.memory());
		}
		temp_peak = temp_peak.max(directory_bytes(scratch));
		thread::sleep(Duration::from_millis(50));
	}
	(rss_peak, temp_peak)
}

fn profile_rss() -> u64 {
	let pid = Pid::from(usize::try_from(std::process::id()).unwrap());
	let mut system = System::new();
	system.refresh_processes_specifics(
		ProcessesToUpdate::Some(&[pid]),
		true,
		ProcessRefreshKind::nothing().with_memory(),
	);
	system.process(pid).map_or(0, sysinfo::Process::memory)
}

fn directory_bytes(root: &Path) -> u64 {
	let mut total = 0_u64;
	let mut directories = vec![root.to_path_buf()];
	while let Some(directory) = directories.pop() {
		let Ok(entries) = fs::read_dir(directory) else {
			continue;
		};
		for entry in entries.flatten() {
			let path = entry.path();
			if path.is_dir() {
				directories.push(path);
			} else if let Ok(metadata) = entry.metadata() {
				total = total.saturating_add(metadata.len());
			}
		}
	}
	total
}
