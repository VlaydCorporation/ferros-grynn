use std::{
	sync::{Arc, Barrier},
	time::{Duration, Instant},
};

use crate::{
	datastore::Datastore,
	gql::{QueryCancellation, QueryExecutionOptions, QueryStatementKind},
	values::{Object, PropertyMap, RecordId, Value},
};

fn atom_exists(store: &Datastore, id: &str) -> bool {
	let mut transaction = store.begin();
	transaction.atom(&id.parse::<RecordId>().unwrap()).unwrap().is_some()
}

fn selected_value(results: &[crate::gql::QueryResult]) -> Value {
	let Value::Array(values) = results[0].result.as_ref().unwrap().value() else {
		panic!("SELECT must return an array");
	};
	values.as_slice().first().cloned().unwrap_or(Value::None)
}

#[test]
fn datastore_autocommits_each_top_level_statement() {
	let store = Datastore::in_memory();
	let results = store
		.execute(
			"DEFINE CLASS person TYPE NODE; CREATE person:alice; CREATE person:alice;",
			&Object::new(),
		)
		.unwrap();
	assert_eq!(results.len(), 3);
	assert!(results[0].result.is_ok());
	assert!(results[1].result.is_ok());
	assert!(results[2].result.is_err());
	assert!(store.class("person").is_some());
	assert!(atom_exists(&store, "person:alice"));
}

#[test]
fn explicit_transaction_rolls_back_after_statement_failure() {
	let store = Datastore::in_memory();
	let results = store
		.execute(
			"BEGIN; DEFINE CLASS person TYPE NODE; CREATE person:alice; CREATE person:alice; \
			 COMMIT;",
			&Object::new(),
		)
		.unwrap();
	assert_eq!(results.len(), 4);
	assert!(results[..3].iter().all(|result| result.result.is_ok()));
	assert!(results[3].result.is_err());
	assert!(store.class("person").is_none());
}

#[test]
fn persistent_session_spans_calls_and_cancel_discards_private_state() {
	let store = Datastore::in_memory();
	let mut session = store.session();
	assert!(session.execute("BEGIN;", &Object::new()).unwrap()[0].result.is_ok());
	assert!(session.has_active_transaction());
	assert!(
		session
			.execute("DEFINE CLASS person TYPE NODE; CREATE person:alice;", &Object::new())
			.unwrap()
			.iter()
			.all(|result| result.result.is_ok())
	);
	assert!(session.execute("COMMIT WITH VALIDATION;", &Object::new()).unwrap()[0].result.is_ok());
	assert!(!session.has_active_transaction());
	assert!(atom_exists(&store, "person:alice"));

	assert!(session.execute("BEGIN;", &Object::new()).unwrap()[0].result.is_ok());
	assert!(session.execute("CREATE person:bob;", &Object::new()).unwrap()[0].result.is_ok());
	assert!(session.execute("CANCEL;", &Object::new()).unwrap()[0].result.is_ok());
	assert!(!atom_exists(&store, "person:bob"));
}

#[test]
fn datastore_rejects_unclosed_or_unmatched_transaction_before_execution() {
	let store = Datastore::in_memory();
	assert!(store.execute("DEFINE CLASS person TYPE NODE; BEGIN;", &Object::new()).is_err());
	assert!(store.class("person").is_none());
	assert!(store.execute("COMMIT;", &Object::new()).is_err());
	assert!(store.execute("BEGIN; BEGIN; COMMIT;", &Object::new()).is_err());
}

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

#[test]
fn read_only_failure_aborts_the_explicit_transaction() {
	let store = Datastore::in_memory();
	let results = store
		.execute("BEGIN READ ONLY; DEFINE CLASS person TYPE NODE; COMMIT;", &Object::new())
		.unwrap();
	assert_eq!(results.len(), 2);
	assert!(results[0].result.is_ok());
	assert!(results[1].result.is_err());
	assert!(store.class("person").is_none());
}

#[test]
fn session_bindings_follow_session_and_transaction_lifetimes() {
	let store = Datastore::in_memory();
	let mut session = store.session();
	assert!(session.execute("LET $outside = 7;", &Object::new()).unwrap()[0].result.is_ok());
	let value = session.execute("RETURN $outside;", &Object::new()).unwrap();
	assert_eq!(value[0].result.as_ref().unwrap().value(), &Value::from(7_i64));

	assert!(session.execute("BEGIN; LET $inside = 9;", &Object::new()).unwrap()[1].result.is_ok());
	assert!(session.execute("RETURN $inside;", &Object::new()).unwrap()[0].result.is_ok());
	assert!(session.execute("CANCEL;", &Object::new()).unwrap()[0].result.is_ok());
	assert!(session.execute("RETURN $inside;", &Object::new()).unwrap()[0].result.is_err());
	assert!(session.execute("LET $outside = 8;", &Object::new()).unwrap()[0].result.is_err());
}

#[test]
fn cancellation_and_zero_timeout_are_statement_errors() {
	let store = Datastore::in_memory();
	let cancellation = QueryCancellation::default();
	cancellation.cancel();
	let mut options = QueryExecutionOptions::default();
	options.cancellation = Some(cancellation);
	let cancelled = store.execute_with_options("RETURN 1;", &Object::new(), options).unwrap();
	assert_eq!(cancelled[0].kind, QueryStatementKind::Return);
	assert!(cancelled[0].result.is_err());

	let mut options = QueryExecutionOptions::default();
	options.statement_timeout = Some(Duration::ZERO);
	let timed_out = store.execute_with_options("RETURN 1;", &Object::new(), options).unwrap();
	assert!(timed_out[0].result.is_err());

	store.execute("DEFINE CLASS person TYPE NODE; CREATE person:alice;", &Object::new()).unwrap();
	let clause = store.execute("SELECT * FROM person TIMEOUT 0ms;", &Object::new()).unwrap();
	assert_eq!(clause[0].kind, QueryStatementKind::Select);
	assert_eq!(clause[0].result.as_ref().unwrap_err().status().code().to_string(), "FG-50F04");
}

#[test]
fn transaction_execute_rejects_nested_transaction_syntax_before_mutation() {
	let store = Datastore::in_memory();
	store.execute("DEFINE CLASS person TYPE NODE;", &Object::new()).unwrap();
	let mut transaction = store.begin();
	assert!(
		transaction.execute("CREATE person:staged; IF true { BEGIN; };", &Object::new(),).is_err()
	);
	assert!(transaction.create_atom("person:after".parse().unwrap(), PropertyMap::new()).is_err());
	drop(transaction);
	assert!(!atom_exists(&store, "person:staged"));
}

#[test]
fn read_committed_refreshes_between_statements_but_snapshot_does_not() {
	let store = Datastore::in_memory();
	store
		.execute(
			"DEFINE CLASS person TYPE NODE; DEFINE FIELD version ON CLASS person TYPE int; CREATE \
			 person:alice SET version = 1;",
			&Object::new(),
		)
		.unwrap();

	let mut read_committed = store.session();
	read_committed.execute("BEGIN ISOLATION LEVEL READ COMMITTED;", &Object::new()).unwrap();
	assert_eq!(
		selected_value(
			&read_committed
				.execute("SELECT VALUE version FROM person:alice;", &Object::new())
				.unwrap()
		),
		Value::from(1_i64)
	);
	store.execute("UPDATE person:alice SET version = 2;", &Object::new()).unwrap();
	assert_eq!(
		selected_value(
			&read_committed
				.execute("SELECT VALUE version FROM person:alice;", &Object::new())
				.unwrap()
		),
		Value::from(2_i64)
	);
	read_committed.execute("CANCEL;", &Object::new()).unwrap();

	let mut snapshot = store.session();
	snapshot.execute("BEGIN ISOLATION LEVEL SNAPSHOT;", &Object::new()).unwrap();
	assert_eq!(
		selected_value(
			&snapshot.execute("SELECT VALUE version FROM person:alice;", &Object::new()).unwrap()
		),
		Value::from(2_i64)
	);
	store.execute("UPDATE person:alice SET version = 3;", &Object::new()).unwrap();
	assert_eq!(
		selected_value(
			&snapshot.execute("SELECT VALUE version FROM person:alice;", &Object::new()).unwrap()
		),
		Value::from(2_i64)
	);
	snapshot.execute("CANCEL;", &Object::new()).unwrap();
}

#[test]
fn serializable_gql_read_participates_in_conflict_validation() {
	let store = Datastore::in_memory();
	store
		.execute(
			"DEFINE CLASS person TYPE NODE; DEFINE FIELD version ON CLASS person TYPE int; CREATE \
			 person:alice SET version = 1;",
			&Object::new(),
		)
		.unwrap();
	let mut session = store.session();
	session.execute("BEGIN;", &Object::new()).unwrap();
	session.execute("SELECT VALUE version FROM person:alice;", &Object::new()).unwrap();
	store.execute("UPDATE person:alice SET version = 2;", &Object::new()).unwrap();
	session.execute("CREATE person:bob;", &Object::new()).unwrap();
	let commit = session.execute("COMMIT;", &Object::new()).unwrap();
	assert_eq!(commit[0].result.as_ref().unwrap_err().status().code().to_string(), "FG-40001");
	assert!(!session.has_active_transaction());
	assert!(!atom_exists(&store, "person:bob"));
}

#[test]
fn multiple_transaction_blocks_and_session_drop_preserve_only_published_work() {
	let store = Datastore::in_memory();
	store.execute("DEFINE CLASS person TYPE NODE;", &Object::new()).unwrap();
	let results = store
		.execute(
			"BEGIN; CREATE person:alice; COMMIT; BEGIN; CREATE person:bob; CANCEL; CREATE \
			 person:carol;",
			&Object::new(),
		)
		.unwrap();
	assert!(results.iter().all(|result| result.result.is_ok()));
	assert!(atom_exists(&store, "person:alice"));
	assert!(!atom_exists(&store, "person:bob"));
	assert!(atom_exists(&store, "person:carol"));

	{
		let mut session = store.session();
		session.execute("BEGIN;", &Object::new()).unwrap();
		session.execute("CREATE person:dropped;", &Object::new()).unwrap();
	}
	assert!(!atom_exists(&store, "person:dropped"));

	let mut session = store.session();
	assert!(session.execute("BEGIN EXECUTION MODE FAST;", &Object::new()).is_err());
	assert!(!session.has_active_transaction());
}

#[test]
fn persistent_session_rolls_back_after_outer_preflight_failure() {
	let store = Datastore::in_memory();
	store.execute("DEFINE CLASS person TYPE NODE;", &Object::new()).unwrap();
	let mut session = store.session();
	session.execute("BEGIN; CREATE person:staged;", &Object::new()).unwrap();
	assert!(session.has_active_transaction());
	assert!(session.execute("IF true { BEGIN; };", &Object::new()).is_err());
	assert!(!session.has_active_transaction());
	assert!(!atom_exists(&store, "person:staged"));

	session.execute("BEGIN; CREATE person:staged;", &Object::new()).unwrap();
	assert!(session.execute("RETURN (;", &Object::new()).is_err());
	assert!(!session.has_active_transaction());
	assert!(!atom_exists(&store, "person:staged"));
}

#[test]
fn measures_disjoint_serializable_gql_writer_aborts() {
	for workers in [1_usize, 2, 4, 8] {
		let store = Datastore::in_memory();
		store.execute("DEFINE CLASS person TYPE NODE;", &Object::new()).unwrap();
		let barrier = Arc::new(Barrier::new(workers));
		let started = Instant::now();
		let handles = (0..workers)
			.map(|worker| {
				let store = store.clone();
				let barrier = barrier.clone();
				std::thread::spawn(move || {
					barrier.wait();
					(0..16)
						.filter(|iteration| {
							let source = format!("CREATE person:w{workers}_{worker}_{iteration};");
							match store.execute(&source, &Object::new()) {
								Ok(results) => results.iter().any(|result| match &result.result {
									Ok(_) => false,
									Err(error) => {
										assert_eq!(error.status().code().to_string(), "FG-40001");
										true
									}
								}),
								Err(error) => panic!("unexpected outer execute failure: {error}"),
							}
						})
						.count()
				})
			})
			.collect::<Vec<_>>();
		let aborts = handles.into_iter().map(|handle| handle.join().unwrap()).sum::<usize>();
		let elapsed = started.elapsed();
		eprintln!(
			"stage11 disjoint writers: workers={workers}, operations={}, aborts={aborts}, \
			 elapsed={elapsed:?}",
			workers * 16
		);
		assert!(aborts < workers * 16);
	}
}

#[test]
#[ignore = "stage-11 profiling gate"]
fn gql_stage_11_single_statement_probe() {
	const OPERATIONS: usize = 500;
	const SAMPLES: usize = 11;
	let store = Datastore::in_memory();
	let variables = Object::new();

	fn legacy_control(store: &Datastore, variables: &Object) {
		let source = "RETURN 1;";
		let options = QueryExecutionOptions::default();
		let program = crate::gql::parse(source, options.parser_settings).unwrap();
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
		.unwrap();
		assert!(results.iter().all(|result| result.result.is_ok()));
		transaction.commit().unwrap();
		std::hint::black_box(results);
	}

	fn current(store: &Datastore, variables: &Object) {
		let results = store.execute("RETURN 1;", variables).unwrap();
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
