#[cfg(test)]
use crate::test_assertions::{AssertOption as _, AssertResult as _};
use crate::{
	datastore::{ClassSchema, Datastore, ImportOptions, ImportViolationPolicy},
	gql::{QueryCancellation, QueryExecutionOptions},
	graph::{AtomKind, GraphKind},
	values::{Object, PropertyMap, RecordId, Value},
};

/// Imports store for this module.
fn import_store() -> Datastore {
	let store = Datastore::in_memory();
	let mut transaction = store.begin();
	transaction
		.define_class(ClassSchema::new("person", AtomKind::Node, []).assert_ok())
		.assert_ok();
	transaction
		.define_class(ClassSchema::new("friendship", AtomKind::Edge, []).assert_ok())
		.assert_ok();
	transaction.define_graph("social", GraphKind::Graph).assert_ok();
	transaction.commit().assert_ok();
	store
}

/// Verifies gql import executes all current dml with implicit graph scope.
#[test]
fn gql_import_executes_all_current_dml_with_implicit_graph_scope() {
	let store = import_store();
	let results = store
		.execute(
			"BEGIN IMPORT INTO GRAPH social WITH (SEGMENT SIZE = 2, INDEXES = DEFERRED, \
			 CONSTRAINTS = IMMEDIATE); CREATE person:alice; CREATE person:bob; CREATE \
			 person:remove; UPDATE person:alice SET active = true; UPSERT person:carol SET active \
			 = true; ATTACH person:alice LABEL Person; DETACH person:alice; ATTACH person:alice; \
			 RELATE (person:alice)-[friendship:known]->(person:bob); DELETE person:remove; END \
			 IMPORT WITH VALIDATION;",
			&Object::new(),
		)
		.assert_ok();
	assert_eq!(results.len(), 1);
	assert!(results[0].result.is_ok());
	let report = match results[0].result.as_ref().assert_ok().value() {
		Value::Object(report) => Some(report),
		_ => None,
	}
	.assert_some_with("import must return a structured report");
	assert_eq!(report.get("processed"), Some(&Value::from(10_u64)));
	assert_eq!(report.get("applied"), Some(&Value::from(10_u64)));
	assert_eq!(report.get("segments"), Some(&Value::from(5_u64)));

	let mut read = store.begin();
	for record in ["person:alice", "person:bob", "person:carol", "friendship:known"] {
		assert!(read.atom(&record.parse::<RecordId>().assert_ok()).assert_ok().is_some());
	}
	assert!(read.atom(&"person:remove".parse::<RecordId>().assert_ok()).assert_ok().is_none());
}

/// Verifies gql import abort and skip match callback atomicity.
#[test]
fn gql_import_abort_and_skip_match_callback_atomicity() {
	let aborted = import_store();
	let results = aborted
		.execute(
			"BEGIN IMPORT; CREATE person:alice; CREATE person:alice; END IMPORT;",
			&Object::new(),
		)
		.assert_ok();
	assert!(results[0].result.is_err());
	assert!(
		aborted.begin().atom(&"person:alice".parse::<RecordId>().assert_ok()).assert_ok().is_none()
	);

	let skipped = import_store();
	let results = skipped
		.execute(
			"BEGIN IMPORT WITH (ON VIOLATION = SKIP, REJECT LIMIT = 1); CREATE person:alice; \
			 CREATE person:alice; CREATE person:bob; END IMPORT;",
			&Object::new(),
		)
		.assert_ok();
	let report = match results[0].result.as_ref().assert_ok().value() {
		Value::Object(report) => Some(report),
		_ => None,
	}
	.assert_some_with("import must return a structured report");
	assert_eq!(report.get("processed"), Some(&Value::from(3_u64)));
	assert_eq!(report.get("applied"), Some(&Value::from(2_u64)));
	assert_eq!(report.get("skipped"), Some(&Value::from(1_u64)));

	let callback = import_store();
	let options = ImportOptions {
		on_violation: ImportViolationPolicy::Skip,
		reject_limit: Some(1),
		..ImportOptions::default()
	};
	let report = callback
		.import(options, |session| {
			for record in ["person:alice", "person:alice", "person:bob"] {
				session.apply_item(|transaction| {
					transaction.create_atom(record.parse().assert_ok(), PropertyMap::new())
				})?;
			}
			Ok(())
		})
		.assert_ok();
	assert_eq!(report.processed_items(), 3);
	assert_eq!(report.applied_items(), 2);
	assert_eq!(report.skipped_items(), 1);
}

/// Verifies gql quarantine retains source kind and for binding.
#[test]
fn gql_quarantine_retains_source_kind_and_for_binding() {
	use crate::test_assertions::AssertResult as _;

	let store = import_store();
	let results = store
		.execute(
			"BEGIN IMPORT WITH (ON VIOLATION = QUARANTINE, REJECT LIMIT = 1, SORT_MEM = 4096, \
			 PARALLELISM = 1); FOR $id IN ['alice', 'alice'] { CREATE person:$id; }; END IMPORT;",
			&Object::new(),
		)
		.assert_ok();
	let result = results
		.into_iter()
		.next()
		.ok_or_else(|| {
			crate::gql_status::FerrosGrynnError::invalid_argument(
				"GrynQL quarantine did not return a report",
			)
		})
		.assert_ok();
	let outcome = result.result.assert_ok();
	let Value::Object(report) = outcome.value() else {
		return {
			let test_result: crate::gql_status::FerrosGrynnResult<()> =
				Err(crate::gql_status::FerrosGrynnError::invalid_argument(
					"GrynQL quarantine report is not an object",
				));
			test_result.assert_ok();
		};
	};
	let Value::Uuid(id) = report
		.get("quarantine")
		.ok_or_else(|| {
			crate::gql_status::FerrosGrynnError::invalid_argument(
				"GrynQL quarantine report has no identity",
			)
		})
		.assert_ok()
	else {
		return {
			let test_result: crate::gql_status::FerrosGrynnResult<()> =
				Err(crate::gql_status::FerrosGrynnError::invalid_argument(
					"GrynQL quarantine identity is not a UUID",
				));
			test_result.assert_ok();
		};
	};
	let mut reader =
		store.open_quarantine(crate::datastore::QuarantineId::from_uuid(*id)).assert_ok();
	let item = reader
		.read_next()
		.assert_ok()
		.ok_or_else(|| {
			crate::gql_status::FerrosGrynnError::invalid_argument(
				"GrynQL quarantine has no rejected item",
			)
		})
		.assert_ok();
	let Value::Object(payload) = item.payload() else {
		return {
			let test_result: crate::gql_status::FerrosGrynnResult<()> =
				Err(crate::gql_status::FerrosGrynnError::invalid_argument(
					"GrynQL quarantine payload is not an object",
				));
			test_result.assert_ok();
		};
	};
	assert_eq!(payload.get("statement_kind"), Some(&Value::from("FOR")));
	assert_eq!(payload.get("item"), Some(&Value::from(2_u64)));
	assert!(payload.get("source").is_some_and(|value| {
		matches!(value, Value::String(source) if source.starts_with("FOR $id"))
	}));
	let Some(Value::Object(bindings)) = payload.get("bindings") else {
		return {
			let test_result: crate::gql_status::FerrosGrynnResult<()> =
				Err(crate::gql_status::FerrosGrynnError::invalid_argument(
					"GrynQL quarantine bindings are not an object",
				));
			test_result.assert_ok();
		};
	};
	assert_eq!(bindings.get("id"), Some(&Value::from("alice")));
}

/// Verifies gql and callback import publish the same logical root and report.
#[test]
fn gql_and_callback_import_publish_the_same_logical_root_and_report() {
	let gql = import_store();
	let callback = import_store();
	let results = gql
		.execute(
			"BEGIN IMPORT INTO GRAPH social WITH (SEGMENT SIZE = 2); CREATE person:alice; CREATE \
			 person:bob; ATTACH person:alice; ATTACH person:bob; END IMPORT WITH VALIDATION;",
			&Object::new(),
		)
		.assert_ok();
	let gql_report = match results[0].result.as_ref().assert_ok().value() {
		Value::Object(gql_report) => Some(gql_report),
		_ => None,
	}
	.assert_some_with("import must return a structured report");

	let graph = callback.schema_catalog().named_graph("social").assert_some().reference().id();
	let callback_report = callback
		.import(
			ImportOptions {
				segment_size: 2,
				full_validation: true,
				..ImportOptions::default()
			},
			|session| {
				for name in ["alice", "bob"] {
					session.apply_item(|transaction| {
						transaction.create_atom(
							format!("person:{name}").parse().assert_ok(),
							PropertyMap::new(),
						)
					})?;
				}
				for name in ["alice", "bob"] {
					session.apply_item(|transaction| {
						transaction.attach_node(
							graph,
							&format!("person:{name}").parse().assert_ok(),
							None,
							None,
						)
					})?;
				}
				Ok(())
			},
		)
		.assert_ok();

	assert_eq!(gql_report.get("processed"), Some(&Value::from(callback_report.processed_items())));
	assert_eq!(gql_report.get("applied"), Some(&Value::from(callback_report.applied_items())));
	assert_eq!(gql_report.get("skipped"), Some(&Value::from(callback_report.skipped_items())));
	assert_eq!(gql_report.get("segments"), Some(&Value::from(callback_report.segments())));
	assert_eq!(gql_report.get("indexes"), Some(&Value::from("DEFERRED")));
	assert_eq!(gql_report.get("constraints"), Some(&Value::from("DEFERRED")));
	assert_eq!(gql_report.get("full_validation"), Some(&Value::from(true)));

	let gql_root = gql.inner.current_root();
	let callback_root = callback.inner.current_root();
	assert_eq!(
		gql_root.atoms.atoms().cloned().collect::<Vec<_>>(),
		callback_root.atoms.atoms().cloned().collect::<Vec<_>>()
	);
	assert_eq!(
		gql_root.graph(graph).assert_some().occurrences().cloned().collect::<Vec<_>>(),
		callback_root.graph(graph).assert_some().occurrences().cloned().collect::<Vec<_>>()
	);
	assert_eq!(gql_root.statistics, callback_root.statistics);
}

/// Verifies import preflight rejects results scope mismatch and unsupported
/// options.
#[test]
fn import_preflight_rejects_results_scope_mismatch_and_unsupported_options() {
	let store = import_store();
	for source in [
		"BEGIN IMPORT INTO GRAPH social; CREATE person:alice RETURN VALUE id; END IMPORT;",
		"BEGIN IMPORT INTO GRAPH social; ATTACH person:alice TO GRAPH other; END IMPORT;",
	] {
		assert!(store.execute(source, &Object::new()).is_err(), "source: {source}");
	}
	assert_eq!(store.inner.current_root().commit_ts, 1);
	assert!(
		store.begin().atom(&"person:alice".parse::<RecordId>().assert_ok()).assert_ok().is_none()
	);

	let mut transaction = store.begin();
	let error = transaction
		.execute("BEGIN IMPORT; CREATE person:alice; END IMPORT;", &Object::new())
		.assert_err();
	assert_eq!(error.status().code().to_string(), "FG-50F03");
	assert!(
		transaction.create_atom("person:blocked".parse().assert_ok(), PropertyMap::new()).is_err()
	);
	let mut replacement = store.begin();
	replacement.create_atom("person:after".parse().assert_ok(), PropertyMap::new()).assert_ok();
	replacement.commit().assert_ok();
}

/// Verifies scoped import rejects cross graph delete cascade.
#[test]
fn scoped_import_rejects_cross_graph_delete_cascade() {
	let store = import_store();
	let mut setup = store.begin();
	let social = setup.named_graph("social").assert_ok().assert_some();
	let other = setup.define_graph("other", GraphKind::Graph).assert_ok();
	let alice = "person:alice".parse::<RecordId>().assert_ok();
	setup.create_atom(alice.clone(), PropertyMap::new()).assert_ok();
	setup.attach_node(social.id(), &alice, None, None).assert_ok();
	setup.attach_node(other.id(), &alice, None, None).assert_ok();
	setup.commit().assert_ok();

	let results = store
		.execute(
			"BEGIN IMPORT INTO GRAPH social; DELETE person:alice CASCADE; END IMPORT;",
			&Object::new(),
		)
		.assert_ok();
	assert!(results[0].result.is_err());

	let mut read = store.begin();
	assert!(read.atom(&alice).assert_ok().is_some());
	assert!(read.membership(social.id(), &alice).assert_ok().is_some());
	assert!(read.membership(other.id(), &alice).assert_ok().is_some());
}

/// Verifies deferred graph family failure discards the complete import.
#[test]
fn deferred_graph_family_failure_discards_the_complete_import() {
	let store = Datastore::in_memory();
	let mut schema = store.begin();
	schema.define_class(ClassSchema::new("person", AtomKind::Node, []).assert_ok()).assert_ok();
	schema.define_class(ClassSchema::new("friendship", AtomKind::Edge, []).assert_ok()).assert_ok();
	schema.define_graph("flow", GraphKind::Dag).assert_ok();
	schema.commit().assert_ok();

	let results = store
		.execute(
			"BEGIN IMPORT INTO GRAPH flow WITH (CONSTRAINTS = DEFERRED); CREATE person:alice; \
			 CREATE person:bob; CREATE person:carol; RELATE \
			 (person:alice)-[friendship:ab]->(person:bob); RELATE \
			 (person:bob)-[friendship:bc]->(person:carol); RELATE \
			 (person:carol)-[friendship:ca]->(person:alice); END IMPORT;",
			&Object::new(),
		)
		.assert_ok();
	assert!(results[0].result.is_err());
	assert!(
		store.begin().atom(&"person:alice".parse::<RecordId>().assert_ok()).assert_ok().is_none()
	);
}

/// Import loop locals share the normal typed duration resolver. Invalid
/// clause values remain item failures; elapsed deadlines abort the whole
/// import.
#[test]
fn duration_operands_in_import_loops() {
	use crate::test_assertions::AssertResult as _;

	let store = import_store();
	let duration = "1s"
		.parse::<crate::values::Duration>()
		.map_err(|error| crate::gql_status::FerrosGrynnError::invalid_argument(error.to_string()))
		.assert_ok();
	let inputs = Object::from_iter([("limits", Value::from(vec![Value::from(duration)]))]);
	let results = store
		.execute(
			"BEGIN IMPORT; FOR $limit IN $limits { CREATE person:timed TIMEOUT $limit; }; END \
			 IMPORT;",
			&inputs,
		)
		.assert_ok();
	assert!(results[0].result.is_ok(), "{results:?}");
	assert!(
		store.begin().atom(&"person:timed".parse::<RecordId>().assert_ok()).assert_ok().is_some()
	);
	let zero = "0ms"
		.parse::<crate::values::Duration>()
		.map_err(|error| crate::gql_status::FerrosGrynnError::invalid_argument(error.to_string()))
		.assert_ok();
	let inputs = Object::from_iter([("limits", Value::from(vec![Value::from(zero)]))]);
	let aborted = store
		.execute(
			"BEGIN IMPORT WITH (ON VIOLATION = SKIP, REJECT LIMIT = 1); CREATE \
			 person:rolled_back; FOR $limit IN $limits { CREATE person:timeout TIMEOUT $limit; }; \
			 END IMPORT;",
			&inputs,
		)
		.assert_ok();
	assert!(aborted[0].result.is_err());
	for name in ["person:rolled_back", "person:timeout"] {
		assert!(store.begin().atom(&name.parse::<RecordId>().assert_ok()).assert_ok().is_none());
	}
}

/// A plan cannot acquire a lease after its captured preflight root changes.
#[test]
fn duration_import_preflight_rejects_root_race() {
	use crate::test_assertions::AssertResult as _;

	let store = import_store();
	let source = "CREATE person:rejected TIMEOUT $limit;";
	let program = crate::gql::parse(source, crate::syn::ParserSettings::default()).assert_ok();
	let inputs = Object::from_iter([(
		"limit",
		Value::from("1s".parse::<crate::values::Duration>().assert_ok()),
	)]);
	let control = crate::gql::ExecutionControl::with_runtime(
		&QueryExecutionOptions::default(),
		None,
		None,
		std::sync::Arc::new(crate::gql::closure::LanguageClosureState::default()),
	);
	let mut preflight = store.begin();
	let (_, _, stamp) =
		super::super::analyze_import_preflight(&mut preflight, source, &program, &inputs, &control)
			.assert_ok();
	drop(preflight);
	let created = store.execute("CREATE person:concurrent;", &Object::new()).assert_ok();
	assert!(created[0].result.is_ok());
	let mut entered = false;
	let result = store.import_at_controlled(
		ImportOptions::default(),
		Some(stamp),
		None,
		None,
		&|| Ok(()),
		|_session| {
			entered = true;
			Ok(())
		},
	);
	assert!(result.is_err());
	assert!(!entered);
	let retry =
		store.execute("BEGIN IMPORT; CREATE person:after; END IMPORT;", &Object::new()).assert_ok();
	assert!(retry[0].result.is_ok());
}

/// Import preflight preserves lexical metadata for reactive duration operands.
#[test]
fn duration_import_preflight_uses_live_values() {
	use crate::test_assertions::AssertResult as _;

	let store = import_store();
	let mut session = store.session();
	let inputs = Object::from_iter([
		("second", Value::from("1s".parse::<crate::values::Duration>().assert_ok())),
		("zero", Value::from("0ms".parse::<crate::values::Duration>().assert_ok())),
	]);
	for result in session.execute("LIVE LET $limit = $second;", &inputs).assert_ok() {
		result.result.assert_ok();
	}
	let imported = session
		.execute("BEGIN IMPORT; CREATE person:timed TIMEOUT $limit; END IMPORT;", &Object::new())
		.assert_ok();
	assert!(imported[0].result.is_ok(), "{imported:?}");
	assert!(
		store.begin().atom(&"person:timed".parse::<RecordId>().assert_ok()).assert_ok().is_some()
	);
	let info = session.execute("INFO FOR LIVE LET $limit;", &Object::new()).assert_ok();
	let ordinary = info[0]
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
	assert!(
		session
			.execute(
				"BEGIN IMPORT; CREATE person:rejected TIMEOUT $ordinary; END IMPORT;",
				&Object::from_iter([("ordinary", ordinary.clone())]),
			)
			.is_err()
	);
	for result in session.execute("ALTER LIVE LET $limit VALUE $zero;", &inputs).assert_ok() {
		result.result.assert_ok();
	}
	let shadowed = session
		.execute(
			"BEGIN IMPORT; FOR $limit IN [$second] { CREATE person:shadowed TIMEOUT $limit; }; \
			 END IMPORT;",
			&inputs,
		)
		.assert_ok();
	assert!(shadowed[0].result.is_ok(), "{shadowed:?}");
	assert!(
		store
			.begin()
			.atom(&"person:shadowed".parse::<RecordId>().assert_ok())
			.assert_ok()
			.is_some()
	);
	let aborted = session
		.execute(
			"BEGIN IMPORT WITH (ON VIOLATION = SKIP, REJECT LIMIT = 1); CREATE person:earlier; \
			 CREATE person:rejected TIMEOUT $limit; END IMPORT;",
			&Object::new(),
		)
		.assert_ok();
	assert!(aborted[0].result.is_err());
	for name in ["person:earlier", "person:rejected"] {
		assert!(store.begin().atom(&name.parse::<RecordId>().assert_ok()).assert_ok().is_none());
	}
}

/// Private source changes do not reevaluate preflight deadlines before END.
#[test]
fn duration_import_freezes_source_dependent_binding() {
	use crate::test_assertions::AssertResult as _;

	let store = import_store();
	let mut session = store.session();
	let inputs = Object::from_iter([
		("second", Value::from("1s".parse::<crate::values::Duration>().assert_ok())),
		("zero", Value::from("0ms".parse::<crate::values::Duration>().assert_ok())),
	]);
	for result in session
		.execute(
			"CREATE person:policy; LIVE LET $limit = IF (SELECT VALUE count() FROM person) > 0 \
			 THEN $second ELSE $zero END; LET $fixed = $second;",
			&inputs,
		)
		.assert_ok()
	{
		result.result.assert_ok();
	}
	let imported = session
		.execute(
			"BEGIN IMPORT WITH (SEGMENT SIZE = 1); DELETE person:policy; CREATE person:timed \
			 TIMEOUT $limit; DELETE person:timed TIMEOUT $fixed; END IMPORT;",
			&Object::new(),
		)
		.assert_ok();
	assert!(imported[0].result.is_ok(), "{imported:?}");
	let value = session.execute("RETURN $limit;", &Object::new()).assert_ok();
	assert_eq!(value[0].result.as_ref().ok().map(|result| result.value()), inputs.get("zero"));
	assert!(
		store.begin().atom(&"person:policy".parse::<RecordId>().assert_ok()).assert_ok().is_none()
	);
	assert!(
		store.begin().atom(&"person:timed".parse::<RecordId>().assert_ok()).assert_ok().is_none()
	);
}

/// Invalid captures and metadata admission fail before a writer lease or DML.
#[test]
fn duration_import_preflight_rejects_invalid_inputs() {
	use crate::test_assertions::AssertResult as _;

	for duration in ["P1M", "-PT1S", "PT20000000000S"] {
		let store = import_store();
		let mut session = store.session();
		let inputs = Object::from_iter([(
			"bad",
			Value::from(duration.parse::<crate::values::Duration>().assert_ok()),
		)]);
		for result in session.execute("LIVE LET $limit = $bad;", &inputs).assert_ok() {
			result.result.assert_ok();
		}
		assert!(
			session
				.execute(
					"BEGIN IMPORT WITH (ON VIOLATION = SKIP, REJECT LIMIT = 1); CREATE \
					 person:earlier; CREATE person:rejected TIMEOUT $limit; END IMPORT;",
					&Object::new(),
				)
				.is_err()
		);
		for name in ["person:earlier", "person:rejected"] {
			assert!(
				store.begin().atom(&name.parse::<RecordId>().assert_ok()).assert_ok().is_none()
			);
		}
		let imported = store
			.execute("BEGIN IMPORT; CREATE person:after; END IMPORT;", &Object::new())
			.assert_ok();
		assert!(imported[0].result.is_ok(), "{imported:?}");
	}
	let store = import_store();
	let mut session = store.session();
	let inputs = Object::from_iter([(
		"limit",
		Value::from("1s".parse::<crate::values::Duration>().assert_ok()),
	)]);
	let options = QueryExecutionOptions {
		barrier_memory_bytes: std::num::NonZeroUsize::new(1)
			.ok_or_else(|| {
				crate::gql_status::FerrosGrynnError::invalid_argument("invalid test budget")
			})
			.assert_ok(),
		..QueryExecutionOptions::default()
	};
	let error = session
		.execute_with_options(
			"BEGIN IMPORT; CREATE person:rejected TIMEOUT $limit; END IMPORT;",
			&inputs,
			options,
		)
		.err()
		.ok_or_else(|| {
			crate::gql_status::FerrosGrynnError::invalid_argument(
				"timeout metadata exceeded its budget",
			)
		})
		.assert_ok();
	assert!(error.to_string().contains("import timeout metadata"), "{error}");
	for name in ["person:earlier", "person:rejected"] {
		assert!(store.begin().atom(&name.parse::<RecordId>().assert_ok()).assert_ok().is_none());
	}
	let imported =
		store.execute("BEGIN IMPORT; CREATE person:after; END IMPORT;", &Object::new()).assert_ok();
	assert!(imported[0].result.is_ok(), "{imported:?}");
}

/// Verifies import for uses one item per iteration.
#[test]
fn import_for_uses_one_item_per_iteration() {
	let store = import_store();
	let results = store
		.execute(
			"BEGIN IMPORT WITH (ON VIOLATION = SKIP, REJECT LIMIT = 1, SEGMENT SIZE = 2); FOR $id \
			 IN ['alice', 'alice', 'bob'] { CREATE person:$id; }; END IMPORT;",
			&Object::new(),
		)
		.assert_ok();
	let report = match results[0].result.as_ref().assert_ok().value() {
		Value::Object(report) => Some(report),
		_ => None,
	}
	.assert_some_with("import must return a structured report");
	assert_eq!(report.get("processed"), Some(&Value::from(3_u64)));
	assert_eq!(report.get("applied"), Some(&Value::from(2_u64)));
	assert_eq!(report.get("skipped"), Some(&Value::from(1_u64)));
	assert_eq!(report.get("segments"), Some(&Value::from(1_u64)));
}

/// Verifies import try precedes violation policy and restores the item.
#[test]
fn import_try_precedes_violation_policy_and_restores_the_item() {
	let store = import_store();
	let results = store
		.execute(
			"BEGIN IMPORT WITH (ON VIOLATION = SKIP, REJECT LIMIT = 1); TRY { CREATE \
			 person:temporary; CREATE person:temporary; } CATCH |$error| { CREATE \
			 person:recovered; }; IF true { CREATE person:after; }; END IMPORT;",
			&Object::new(),
		)
		.assert_ok();
	let report = match results[0].result.as_ref().assert_ok().value() {
		Value::Object(report) => Some(report),
		_ => None,
	}
	.assert_some_with("import must return a structured report");
	assert_eq!(report.get("processed"), Some(&Value::from(2_u64)));
	assert_eq!(report.get("applied"), Some(&Value::from(2_u64)));
	assert_eq!(report.get("skipped"), Some(&Value::from(0_u64)));
	let mut read = store.begin();
	assert!(read.atom(&"person:temporary".parse().assert_ok()).assert_ok().is_none());
	assert!(read.atom(&"person:recovered".parse().assert_ok()).assert_ok().is_some());
	assert!(read.atom(&"person:after".parse().assert_ok()).assert_ok().is_some());
}

/// Verifies import unhandled throw and cancellation abort every item.
#[test]
fn import_unhandled_throw_and_cancellation_abort_every_item() {
	let thrown = import_store();
	let results = thrown
		.execute(
			"BEGIN IMPORT WITH (ON VIOLATION = SKIP, REJECT LIMIT = 1); CREATE person:alice; \
			 THROW 'stop'; END IMPORT;",
			&Object::new(),
		)
		.assert_ok();
	assert!(results[0].result.is_err());
	assert!(thrown.begin().atom(&"person:alice".parse().assert_ok()).assert_ok().is_none());

	let cancelled = import_store();
	let cancellation = QueryCancellation::default();
	cancellation.cancel();
	let options = QueryExecutionOptions {
		cancellation: Some(cancellation),
		..QueryExecutionOptions::default()
	};
	let error = cancelled
		.execute_with_options(
			"BEGIN IMPORT; CREATE person:alice; END IMPORT;",
			&Object::new(),
			options,
		)
		.assert_err();
	assert_eq!(error.status().code().to_string(), "FG-50F05");
	assert!(cancelled.begin().atom(&"person:alice".parse().assert_ok()).assert_ok().is_none());
}

/// Verifies import recursive preflight rejects deferred control surface.
#[test]
fn import_recursive_preflight_rejects_deferred_control_surface() {
	let store = import_store();
	for source in [
		"BEGIN IMPORT; IF true { LET $x = 1; }; END IMPORT;",
		"BEGIN IMPORT; TRY { WHILE (false) { BREAK; }; }; END IMPORT;",
		"BEGIN IMPORT; FOR $x IN [1] { ASSERT INVARIANTS; }; END IMPORT;",
	] {
		assert!(store.execute(source, &Object::new()).is_err(), "source: {source}");
	}
	assert_eq!(store.inner.current_root().commit_ts, 1);
}
