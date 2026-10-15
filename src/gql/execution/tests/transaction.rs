use std::{
	sync::{Arc, Barrier},
	time::{Duration, Instant},
};

#[cfg(test)]
use crate::test_assertions::AssertOption as _;
#[cfg(test)]
use crate::test_assertions::AssertResult as _;
use crate::{
	datastore::Datastore,
	gql::{QueryCancellation, QueryExecutionOptions, QueryStatementKind},
	values::{Object, PropertyMap, RecordId, Value},
};

/// Reports whether the atom exists condition holds for this module.
fn atom_exists(store: &Datastore, id: &str) -> bool {
	let mut transaction = store.begin();
	transaction.atom(&id.parse::<RecordId>().assert_ok()).assert_ok().is_some()
}

/// Returns selected value for this module.
fn selected_value(results: &[crate::gql::QueryResult]) -> Value {
	let values = match results[0].result.as_ref().assert_ok().value() {
		Value::Array(values) => Some(values),
		_ => None,
	}
	.assert_some_with("SELECT must return an array");
	values.as_slice().first().cloned().unwrap_or(Value::None)
}

/// Verifies datastore autocommits each top level statement.
#[test]
fn datastore_autocommits_each_top_level_statement() {
	let store = Datastore::in_memory();
	let results = store
		.execute(
			"DEFINE CLASS person TYPE NODE; CREATE person:alice; CREATE person:alice;",
			&Object::new(),
		)
		.assert_ok();
	assert_eq!(results.len(), 3);
	assert!(results[0].result.is_ok());
	assert!(results[1].result.is_ok());
	assert!(results[2].result.is_err());
	assert!(store.class("person").is_some());
	assert!(atom_exists(&store, "person:alice"));
}

/// Verifies explicit transaction rolls back after statement failure.
#[test]
fn explicit_transaction_rolls_back_after_statement_failure() {
	let store = Datastore::in_memory();
	let results = store
		.execute(
			"BEGIN; DEFINE CLASS person TYPE NODE; CREATE person:alice; CREATE person:alice; \
			 COMMIT;",
			&Object::new(),
		)
		.assert_ok();
	assert_eq!(results.len(), 4);
	assert!(results[..3].iter().all(|result| result.result.is_ok()));
	assert!(results[3].result.is_err());
	assert!(store.class("person").is_none());
}

/// Verifies persistent session spans calls and cancel discards private state.
#[test]
fn persistent_session_spans_calls_and_cancel_discards_private_state() {
	let store = Datastore::in_memory();
	let mut session = store.session();
	assert!(session.execute("BEGIN;", &Object::new()).assert_ok()[0].result.is_ok());
	assert!(session.has_active_transaction());
	assert!(
		session
			.execute("DEFINE CLASS person TYPE NODE; CREATE person:alice;", &Object::new())
			.assert_ok()
			.iter()
			.all(|result| result.result.is_ok())
	);
	assert!(
		session.execute("COMMIT WITH VALIDATION;", &Object::new()).assert_ok()[0].result.is_ok()
	);
	assert!(!session.has_active_transaction());
	assert!(atom_exists(&store, "person:alice"));

	assert!(session.execute("BEGIN;", &Object::new()).assert_ok()[0].result.is_ok());
	assert!(session.execute("CREATE person:bob;", &Object::new()).assert_ok()[0].result.is_ok());
	assert!(session.execute("CANCEL;", &Object::new()).assert_ok()[0].result.is_ok());
	assert!(!atom_exists(&store, "person:bob"));
}

/// Verifies datastore rejects unclosed or unmatched transaction before
/// execution.
#[test]
fn datastore_rejects_unclosed_or_unmatched_transaction_before_execution() {
	let store = Datastore::in_memory();
	assert!(store.execute("DEFINE CLASS person TYPE NODE; BEGIN;", &Object::new()).is_err());
	assert!(store.class("person").is_none());
	assert!(store.execute("COMMIT;", &Object::new()).is_err());
	assert!(store.execute("BEGIN; BEGIN; COMMIT;", &Object::new()).is_err());
}

/// Verifies structural errors prevent every statement in the call.
#[test]
fn structural_errors_prevent_every_statement_in_the_call() {
	let store = Datastore::in_memory();
	assert!(store.execute("DEFINE CLASS person TYPE NODE; BREAK;", &Object::new()).is_err());
	assert!(store.class("person").is_none());
	assert!(store.execute("IF true { BEGIN; };", &Object::new()).is_err());
	assert!(
		store
			.execute(
				"DEFINE CLASS person TYPE NODE; BEGIN IMPORT { CREATE person:alice; } END IMPORT;",
				&Object::new(),
			)
			.is_err()
	);
	assert!(store.class("person").is_none());
}

/// Verifies read only failure aborts the explicit transaction.
#[test]
fn read_only_failure_aborts_the_explicit_transaction() {
	let store = Datastore::in_memory();
	let results = store
		.execute("BEGIN READ ONLY; DEFINE CLASS person TYPE NODE; COMMIT;", &Object::new())
		.assert_ok();
	assert_eq!(results.len(), 2);
	assert!(results[0].result.is_ok());
	assert!(results[1].result.is_err());
	assert!(store.class("person").is_none());
}

/// Verifies session bindings follow session and transaction lifetimes.
#[test]
fn session_bindings_follow_session_and_transaction_lifetimes() {
	let store = Datastore::in_memory();
	let mut session = store.session();
	assert!(session.execute("LET $outside = 7;", &Object::new()).assert_ok()[0].result.is_ok());
	let value = session.execute("RETURN $outside;", &Object::new()).assert_ok();
	assert_eq!(value[0].result.as_ref().assert_ok().value(), &Value::from(7_i64));

	assert!(
		session.execute("BEGIN; LET $inside = 9;", &Object::new()).assert_ok()[1].result.is_ok()
	);
	assert!(session.execute("RETURN $inside;", &Object::new()).assert_ok()[0].result.is_ok());
	assert!(session.execute("CANCEL;", &Object::new()).assert_ok()[0].result.is_ok());
	assert!(session.execute("RETURN $inside;", &Object::new()).assert_ok()[0].result.is_err());
	assert!(session.execute("LET $outside = 8;", &Object::new()).assert_ok()[0].result.is_err());
}

/// Verifies cancellation and zero timeout are statement errors.
#[test]
fn cancellation_and_zero_timeout_are_statement_errors() {
	let store = Datastore::in_memory();
	let cancellation = QueryCancellation::default();
	cancellation.cancel();
	let options = QueryExecutionOptions {
		cancellation: Some(cancellation),
		..Default::default()
	};
	let cancelled = store.execute_with_options("RETURN 1;", &Object::new(), options).assert_ok();
	assert_eq!(cancelled[0].kind, QueryStatementKind::Return);
	assert!(cancelled[0].result.is_err());

	let options = QueryExecutionOptions {
		statement_timeout: Some(Duration::ZERO),
		..Default::default()
	};
	let timed_out = store.execute_with_options("RETURN 1;", &Object::new(), options).assert_ok();
	assert!(timed_out[0].result.is_err());

	store
		.execute("DEFINE CLASS person TYPE NODE; CREATE person:alice;", &Object::new())
		.assert_ok();
	let clause = store.execute("SELECT * FROM person TIMEOUT 0ms;", &Object::new()).assert_ok();
	assert_eq!(clause[0].kind, QueryStatementKind::Select);
	assert_eq!(clause[0].result.as_ref().assert_err().status().code().to_string(), "FG-50F04");
}

/// Verifies transaction execute rejects nested transaction syntax before
/// mutation.
#[test]
fn transaction_execute_rejects_nested_transaction_syntax_before_mutation() {
	let store = Datastore::in_memory();
	store.execute("DEFINE CLASS person TYPE NODE;", &Object::new()).assert_ok();
	let mut transaction = store.begin();
	assert!(
		transaction.execute("CREATE person:staged; IF true { BEGIN; };", &Object::new(),).is_err()
	);
	assert!(
		transaction.create_atom("person:after".parse().assert_ok(), PropertyMap::new()).is_err()
	);
	drop(transaction);
	assert!(!atom_exists(&store, "person:staged"));
}

/// Verifies read committed refreshes between statements but snapshot does not.
#[test]
fn read_committed_refreshes_between_statements_but_snapshot_does_not() {
	let store = Datastore::in_memory();
	store
		.execute(
			"DEFINE CLASS person TYPE NODE; DEFINE FIELD version ON CLASS person TYPE int; CREATE \
			 person:alice SET version = 1;",
			&Object::new(),
		)
		.assert_ok();

	let mut read_committed = store.session();
	read_committed.execute("BEGIN ISOLATION LEVEL READ COMMITTED;", &Object::new()).assert_ok();
	assert_eq!(
		selected_value(
			&read_committed
				.execute("SELECT VALUE version FROM person:alice;", &Object::new())
				.assert_ok()
		),
		Value::from(1_i64)
	);
	store.execute("UPDATE person:alice SET version = 2;", &Object::new()).assert_ok();
	assert_eq!(
		selected_value(
			&read_committed
				.execute("SELECT VALUE version FROM person:alice;", &Object::new())
				.assert_ok()
		),
		Value::from(2_i64)
	);
	read_committed.execute("CANCEL;", &Object::new()).assert_ok();

	let mut snapshot = store.session();
	snapshot.execute("BEGIN ISOLATION LEVEL SNAPSHOT;", &Object::new()).assert_ok();
	assert_eq!(
		selected_value(
			&snapshot
				.execute("SELECT VALUE version FROM person:alice;", &Object::new())
				.assert_ok()
		),
		Value::from(2_i64)
	);
	store.execute("UPDATE person:alice SET version = 3;", &Object::new()).assert_ok();
	assert_eq!(
		selected_value(
			&snapshot
				.execute("SELECT VALUE version FROM person:alice;", &Object::new())
				.assert_ok()
		),
		Value::from(2_i64)
	);
	snapshot.execute("CANCEL;", &Object::new()).assert_ok();
}

/// Verifies serializable gql read participates in conflict validation.
#[test]
fn serializable_gql_read_participates_in_conflict_validation() {
	let store = Datastore::in_memory();
	store
		.execute(
			"DEFINE CLASS person TYPE NODE; DEFINE FIELD version ON CLASS person TYPE int; CREATE \
			 person:alice SET version = 1;",
			&Object::new(),
		)
		.assert_ok();
	let mut session = store.session();
	session.execute("BEGIN;", &Object::new()).assert_ok();
	session.execute("SELECT VALUE version FROM person:alice;", &Object::new()).assert_ok();
	store.execute("UPDATE person:alice SET version = 2;", &Object::new()).assert_ok();
	session.execute("CREATE person:bob;", &Object::new()).assert_ok();
	let commit = session.execute("COMMIT;", &Object::new()).assert_ok();
	assert_eq!(commit[0].result.as_ref().assert_err().status().code().to_string(), "FG-40001");
	assert!(!session.has_active_transaction());
	assert!(!atom_exists(&store, "person:bob"));
}

/// Verifies serializable index scan tracks the same class range as a scan.
#[test]
fn serializable_index_scan_tracks_the_same_class_range_as_a_scan() {
	let store = Datastore::in_memory();
	store
		.execute(
			"DEFINE CLASS person TYPE NODE SCHEMAFULL; DEFINE FIELD age ON CLASS person TYPE int; \
			 DEFINE INDEX by_age ON CLASS person FIELDS age SCOPE BASE; CREATE person:seed SET \
			 age = 10;",
			&Object::new(),
		)
		.assert_ok();
	let mut reader = store.begin();
	reader
		.execute("SELECT VALUE id FROM person WITH INDEX by_age WHERE age = 10;", &Object::new())
		.assert_ok();
	store.execute("CREATE person:phantom SET age = 10;", &Object::new()).assert_ok();
	reader.execute("CREATE person:reader SET age = 99;", &Object::new()).assert_ok();
	assert_eq!(reader.commit().assert_err().status().code().to_string(), "FG-40001");
	assert!(!atom_exists(&store, "person:reader"));
}

/// Verifies multiple transaction blocks and session drop preserve only
/// published work.
#[test]
fn multiple_transaction_blocks_and_session_drop_preserve_only_published_work() {
	let store = Datastore::in_memory();
	store.execute("DEFINE CLASS person TYPE NODE;", &Object::new()).assert_ok();
	let results = store
		.execute(
			"BEGIN; CREATE person:alice; COMMIT; BEGIN; CREATE person:bob; CANCEL; CREATE \
			 person:carol;",
			&Object::new(),
		)
		.assert_ok();
	assert!(results.iter().all(|result| result.result.is_ok()));
	assert!(atom_exists(&store, "person:alice"));
	assert!(!atom_exists(&store, "person:bob"));
	assert!(atom_exists(&store, "person:carol"));

	{
		let mut session = store.session();
		session.execute("BEGIN;", &Object::new()).assert_ok();
		session.execute("CREATE person:dropped;", &Object::new()).assert_ok();
	}
	assert!(!atom_exists(&store, "person:dropped"));

	let mut session = store.session();
	let fast = session.execute("BEGIN EXECUTION MODE FAST;", &Object::new()).assert_ok();
	assert!(fast[0].result.is_ok());
	assert!(session.has_active_transaction());
	session.execute("CANCEL;", &Object::new()).assert_ok();
	assert!(!session.has_active_transaction());
}

/// Verifies deterministic mode replays random order and rejects impure time.
#[test]
fn deterministic_mode_replays_random_order_and_rejects_impure_time() {
	let store = Datastore::in_memory();
	store
		.execute(
			"DEFINE CLASS person TYPE NODE; CREATE person:a; CREATE person:b; CREATE person:c; \
			 CREATE person:d; CREATE person:e;",
			&Object::new(),
		)
		.assert_ok();
	let options = QueryExecutionOptions {
		deterministic_seed: 42,
		..QueryExecutionOptions::default()
	};
	let mut session = store.session();
	session
		.execute_with_options(
			"BEGIN ISOLATION LEVEL SNAPSHOT EXECUTION MODE DETERMINISTIC;",
			&Object::new(),
			options.clone(),
		)
		.assert_ok();
	let query = "SELECT VALUE id FROM person ORDER BY RAND();";
	let first = session
		.execute_with_options(query, &Object::new(), options.clone())
		.assert_ok()
		.remove(0)
		.result
		.assert_ok()
		.into_parts()
		.0;
	let second = session
		.execute_with_options(query, &Object::new(), options.clone())
		.assert_ok()
		.remove(0)
		.result
		.assert_ok()
		.into_parts()
		.0;
	assert_eq!(first, second);
	for _run in 0..18 {
		let replay = session
			.execute_with_options(query, &Object::new(), options.clone())
			.assert_ok()
			.remove(0)
			.result
			.assert_ok()
			.into_parts()
			.0;
		assert_eq!(first, replay);
	}
	let mut expected_plan_hash = None;
	for _run in 0..20 {
		let explained = session
			.execute_with_options(
				"EXPLAIN FULL FORMAT JSON SELECT VALUE id FROM person ORDER BY RAND();",
				&Object::new(),
				options.clone(),
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
		.assert_some_with("deterministic EXPLAIN must return an object");
		let hash = explained.get("plan_hash").cloned();
		assert!(hash.is_some());
		match &expected_plan_hash {
			None => expected_plan_hash = hash,
			Some(expected) => assert_eq!(Some(expected), hash.as_ref()),
		}
	}

	let impure = session
		.execute_with_options("RETURN time::now();", &Object::new(), options)
		.assert_ok()
		.remove(0)
		.result
		.assert_err();
	assert_eq!(impure.status().code().to_string(), "FG-50F03");
	assert!(!session.has_active_transaction());

	let error = store
		.session()
		.execute(
			"BEGIN ISOLATION LEVEL READ COMMITTED EXECUTION MODE DETERMINISTIC;",
			&Object::new(),
		)
		.assert_err();
	assert_eq!(error.status().code().to_string(), "FG-22N11");
}

/// Verifies persistent session rolls back after outer preflight failure.
#[test]
fn persistent_session_rolls_back_after_outer_preflight_failure() {
	let store = Datastore::in_memory();
	store.execute("DEFINE CLASS person TYPE NODE;", &Object::new()).assert_ok();
	let mut session = store.session();
	session.execute("BEGIN; CREATE person:staged;", &Object::new()).assert_ok();
	assert!(session.has_active_transaction());
	assert!(session.execute("IF true { BEGIN; };", &Object::new()).is_err());
	assert!(!session.has_active_transaction());
	assert!(!atom_exists(&store, "person:staged"));

	session.execute("BEGIN; CREATE person:staged;", &Object::new()).assert_ok();
	assert!(session.execute("RETURN (;", &Object::new()).is_err());
	assert!(!session.has_active_transaction());
	assert!(!atom_exists(&store, "person:staged"));
}

/// Verifies disjoint serializable gql writers commit without allocator
/// conflicts.
#[test]
fn disjoint_serializable_gql_writers_commit_without_allocator_conflicts() {
	for workers in [1_usize, 2, 4, 8] {
		let store = Datastore::in_memory();
		store.execute("DEFINE CLASS person TYPE NODE;", &Object::new()).assert_ok();
		let barrier = Arc::new(Barrier::new(workers));
		let started = Instant::now();
		// Start every worker before joining any of them: all wait at this
		// barrier.
		let mut handles = (0..workers)
			.map(|worker| {
				let store = store.clone();
				let barrier = barrier.clone();
				std::thread::spawn(move || {
					barrier.wait();
					(0..16)
						.filter(|iteration| {
							let source = format!("CREATE person:w{workers}_{worker}_{iteration};");
							store.execute(&source, &Object::new()).assert_ok().iter().any(
								|result| match &result.result {
									Ok(_) => false,
									Err(error) => {
										assert_eq!(error.status().code().to_string(), "FG-40001");
										true
									}
								},
							)
						})
						.count()
				})
			})
			.collect::<Vec<_>>();
		let aborts = handles.drain(..).map(|handle| handle.join().assert_ok()).sum::<usize>();
		let elapsed = started.elapsed();
		eprintln!(
			"stage11 disjoint writers: workers={workers}, operations={}, aborts={aborts}, \
			 elapsed={elapsed:?}",
			workers * 16
		);
		assert_eq!(aborts, 0);
	}
}

/// Verifies gql stage 11 single statement probe.
#[test]
#[ignore = "stage-11 profiling gate"]
fn gql_stage_11_single_statement_probe() {
	/// Statement executions grouped into each stage-11 timing sample.
	const OPERATIONS: usize = 500;
	/// Timing samples collected for the stage-11 single-statement comparison.
	const SAMPLES: usize = 11;
	let store = Datastore::in_memory();
	let variables = Object::new();

	/// Returns legacy control for this module.
	fn legacy_control(store: &Datastore, variables: &Object) {
		let source = "RETURN 1;";
		let options = QueryExecutionOptions::default();
		let program = crate::gql::parse(source, options.parser_settings).assert_ok();
		let control = crate::gql::ExecutionControl::new(&options, None);
		let mut transaction = store.begin_with(crate::datastore::TransactionOptions::new(
			crate::datastore::IsolationLevel::Serializable,
			crate::datastore::TransactionAccess::ReadOnly,
		));
		let mut bindings = variables.clone();
		let results = crate::gql::execute_parsed_transaction_with_bindings(
			&mut transaction,
			source,
			&program,
			&mut bindings,
			&control,
		)
		.assert_ok();
		assert!(results.iter().all(|result| result.result.is_ok()));
		transaction.commit().assert_ok();
		std::hint::black_box(results);
	}

	/// Returns current for this module.
	fn current(store: &Datastore, variables: &Object) {
		let results = store.execute("RETURN 1;", variables).assert_ok();
		assert!(results.iter().all(|result| result.result.is_ok()));
		std::hint::black_box(results);
	}

	for _ in 0..100 {
		legacy_control(&store, &variables);
		current(&store, &variables);
	}
	let mut legacy = Vec::with_capacity(SAMPLES);
	let mut staged = Vec::with_capacity(SAMPLES);
	for sample in 0..SAMPLES {
		let measure = |function: fn(&Datastore, &Object)| {
			let started = Instant::now();
			for _ in 0..OPERATIONS {
				function(&store, &variables);
			}
			started.elapsed().as_nanos() as f64 / OPERATIONS as f64
		};
		if sample % 2 == 0 {
			legacy.push(measure(legacy_control));
			staged.push(measure(current));
		} else {
			staged.push(measure(current));
			legacy.push(measure(legacy_control));
		}
	}
	legacy.sort_by(f64::total_cmp);
	staged.sort_by(f64::total_cmp);
	let legacy_median = legacy[SAMPLES / 2];
	let staged_median = staged[SAMPLES / 2];
	let ratio = staged_median / legacy_median;
	eprintln!(
		"stage11 single statement: legacy_control_ns={legacy_median:.2}, \
		 current_ns={staged_median:.2}, ratio={ratio:.4}"
	);
	assert!(ratio <= 1.05, "temporary-session overhead ratio {ratio:.4} exceeds 1.05");
}
