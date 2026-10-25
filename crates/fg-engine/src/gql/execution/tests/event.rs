//! Event DDL follows the same SDK definition and source-validation boundary.

use crate::{
	datastore::{Datastore, EventExecution, EventTarget},
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	values::{Object, Value},
};

/// Runs a complete command list and propagates any statement diagnostic.
fn execute(store: &Datastore, source: &str) -> FerrosGrynnResult<Vec<Value>> {
	store
		.execute(source, &Object::new())?
		.into_iter()
		.map(|result| result.result.map(|outcome| outcome.into_parts().0))
		.collect()
}

/// Verifies event common dml udf keeps transaction reader.
#[test]
fn event_common_dml_udf_keeps_transaction_reader() {
	use crate::test_assertions::AssertResult as _;

	let store = Datastore::in_memory();
	execute(
		&store,
		"DEFINE CLASS item TYPE NODE; DEFINE CLASS audit TYPE NODE; CREATE item:a CONTENT { \
		 value: 7 }; DEFINE STABLE FUNCTION fn::read() -> int { RETURN <int>item:a.value; };",
	)
	.assert_ok();
	execute(&store, "CREATE audit:a CONTENT { value: fn::read() };").assert_ok();
	let key = store
		.property_dictionary()
		.get("value")
		.ok_or_else(|| FerrosGrynnError::invalid_argument("value key"))
		.assert_ok();
	assert_eq!(
		store
			.begin()
			.atom(&"audit:a".parse().assert_ok())
			.assert_ok()
			.and_then(|atom| atom.properties().get(key).cloned()),
		Some(Value::from(7))
	);
}

/// Verifies event common dml mutable builtin is rejected consistently.
#[test]
fn event_common_dml_mutable_builtin_is_rejected_consistently() {
	use crate::test_assertions::AssertResult as _;

	for sibling in ["", ", sibling: fn::seven()"] {
		let store = Datastore::in_memory();
		execute(
			&store,
			"DEFINE CLASS audit TYPE NODE; DEFINE PURE FUNCTION fn::seven() -> int { RETURN 7; };",
		)
		.assert_ok();
		let error = execute(
			&store,
			&format!(
				"LET $items = []; CREATE audit:a CONTENT {{ size: array::push($items, 7){sibling} \
				 }}; RETURN $items;"
			),
		)
		.err()
		.ok_or_else(|| {
			FerrosGrynnError::invalid_argument(
				"row-producing DML accepted a mutable collection helper",
			)
		})
		.assert_ok();
		assert!(error.to_string().contains("Mutable collection helpers are not allowed"));
		assert!(store.begin().atom(&"audit:a".parse().assert_ok()).assert_ok().is_none());
	}
}

/// Verifies event common dml where expect and return keep reader.
#[test]
fn event_common_dml_where_expect_and_return_keep_reader() {
	use crate::test_assertions::AssertResult as _;

	let store = Datastore::in_memory();
	execute(
		&store,
		"DEFINE CLASS item TYPE NODE; DEFINE CLASS audit TYPE NODE; CREATE item:a CONTENT { \
		 value: 7 }; CREATE audit:a CONTENT { value: 1 }; DEFINE STABLE FUNCTION fn::read() -> \
		 int { RETURN <int>item:a.value; };",
	)
	.assert_ok();
	let values = execute(
		&store,
		"UPDATE audit:a SET value = 2 WHERE fn::read() = 7 RETURN VALUE fn::read() EXPECT \
		 fn::read() = 7;",
	)
	.assert_ok();
	assert_eq!(
		values.last().and_then(Value::as_array).map(crate::values::Array::as_slice),
		Some([Value::from(7)].as_slice())
	);
}

/// Verifies event ddl lifecycle uses stable identity and common info.
#[test]
fn event_ddl_lifecycle_uses_stable_identity_and_common_info() {
	use crate::test_assertions::AssertResult as _;

	let store = Datastore::in_memory();
	execute(
		&store,
		"DEFINE CLASS item TYPE NODE; DEFINE GRAPH scope; DEFINE EVENT audit ON CLASS item WHEN \
		 $event = 'CREATE' THEN { RETURN $after; }; DEFINE EVENT audit ON GRAPH scope ASYNC RETRY \
		 255 MAXDEPTH 0 WHEN true THEN 1;",
	)
	.assert_ok();
	let catalog = store.schema_catalog();
	let class = catalog
		.class("item")
		.and_then(|class| class.id())
		.ok_or_else(|| FerrosGrynnError::invalid_argument("fixture class"))
		.assert_ok();
	let graph = catalog
		.named_graph("scope")
		.ok_or_else(|| FerrosGrynnError::invalid_argument("fixture graph"))
		.assert_ok();
	let event = catalog
		.named_event(EventTarget::Class(class), "audit")
		.ok_or_else(|| FerrosGrynnError::invalid_argument("fixture event"))
		.assert_ok();
	let id = event.id();
	execute(
		&store,
		"DEFINE EVENT IF NOT EXISTS audit ON CLASS item WHEN true THEN 2; DEFINE EVENT OVERWRITE \
		 audit ON CLASS item WHEN true THEN 3;",
	)
	.assert_ok();
	let current = store
		.schema_catalog()
		.named_event(EventTarget::Class(class), "audit")
		.ok_or_else(|| FerrosGrynnError::invalid_argument("fixture event"))
		.assert_ok();
	assert_eq!(current.id(), id);
	assert_eq!(current.version().get(), 2);
	execute(
		&store,
		"ALTER EVENT audit ON CLASS item ASYNC RETRY 2 MAXDEPTH 16 WHEN false THEN { RETURN 4; } \
		 COMMENT 'audit';",
	)
	.assert_ok();
	let current = store
		.schema_catalog()
		.named_event(EventTarget::Class(class), "audit")
		.ok_or_else(|| FerrosGrynnError::invalid_argument("fixture event"))
		.assert_ok();
	assert_eq!(current.id(), id);
	assert_eq!(current.version().get(), 3);
	assert!(
		matches!(current.execution(), EventExecution::Async(options) if options.retries() == 2 && options.max_depth() == 16)
	);
	let infos = execute(&store, "INFO FOR CLASS item; INFO FOR GRAPH scope;").assert_ok();
	for info in infos {
		let events = info
			.as_object()
			.and_then(|info| info.get("events"))
			.and_then(Value::as_object)
			.ok_or_else(|| FerrosGrynnError::invalid_argument("EVENT INFO missing"))
			.assert_ok();
		assert!(events.contains_key("audit"));
	}
	execute(
		&store,
		"ALTER EVENT audit ON CLASS item THEN 1, 2; ALTER EVENT audit ON CLASS item DROP ASYNC \
		 DROP WHEN DROP THEN DROP COMMENT; REMOVE EVENT audit ON CLASS item; REMOVE EVENT IF \
		 EXISTS audit ON CLASS item; REMOVE EVENT IF EXISTS missing ON CLASS absent;",
	)
	.assert_ok();
	assert!(store.schema_catalog().named_event(EventTarget::Class(class), "audit").is_none());
	assert!(store.schema_catalog().named_event(EventTarget::Graph(graph.id()), "audit").is_some());
}

/// Verifies event ddl validation does not execute source.
#[test]
fn event_ddl_validation_does_not_execute_source() {
	use crate::test_assertions::AssertResult as _;

	let store = Datastore::in_memory();
	execute(
		&store,
		"DEFINE CLASS item TYPE NODE; DEFINE CLASS audit TYPE NODE; DEFINE EVENT safe ON CLASS \
		 item WHEN true THEN { CREATE audit:unexpected; };",
	)
	.assert_ok();
	assert!(store.begin().atom(&"audit:unexpected".parse().assert_ok()).assert_ok().is_none());
	let results = store.execute(
		"DEFINE EVENT bad ON CLASS item WHEN { CREATE audit:forbidden; RETURN true; } THEN 1;",
		&Object::new(),
	);
	assert!(
		results.is_err()
			|| results.is_ok_and(|results| results.iter().any(|result| result.result.is_err()))
	);
	assert!(store.begin().atom(&"audit:forbidden".parse().assert_ok()).assert_ok().is_none());
}
