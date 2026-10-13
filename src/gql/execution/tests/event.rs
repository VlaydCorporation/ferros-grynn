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

#[test]
fn event_common_dml_udf_keeps_transaction_reader() -> FerrosGrynnResult<()> {
	let store = Datastore::in_memory();
	execute(
		&store,
		"DEFINE CLASS item TYPE NODE; DEFINE CLASS audit TYPE NODE; CREATE item:a CONTENT { \
		 value: 7 }; DEFINE STABLE FUNCTION fn::read() -> int { RETURN <int>item:a.value; };",
	)?;
	execute(&store, "CREATE audit:a CONTENT { value: fn::read() };")?;
	let key = store
		.property_dictionary()
		.get("value")
		.ok_or_else(|| FerrosGrynnError::invalid_argument("value key"))?;
	assert_eq!(
		store
			.begin()
			.atom(&"audit:a".parse()?)?
			.and_then(|atom| atom.properties().get(key).cloned()),
		Some(Value::from(7))
	);
	Ok(())
}

#[test]
fn event_common_dml_mutable_builtin_is_rejected_consistently() -> FerrosGrynnResult<()> {
	for sibling in ["", ", sibling: fn::seven()"] {
		let store = Datastore::in_memory();
		execute(
			&store,
			"DEFINE CLASS audit TYPE NODE; DEFINE PURE FUNCTION fn::seven() -> int { RETURN 7; };",
		)?;
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
		})?;
		assert!(error.to_string().contains("Mutable collection helpers are not allowed"));
		assert!(store.begin().atom(&"audit:a".parse()?)?.is_none());
	}
	Ok(())
}

#[test]
fn event_common_dml_where_expect_and_return_keep_reader() -> FerrosGrynnResult<()> {
	let store = Datastore::in_memory();
	execute(
		&store,
		"DEFINE CLASS item TYPE NODE; DEFINE CLASS audit TYPE NODE; CREATE item:a CONTENT { \
		 value: 7 }; CREATE audit:a CONTENT { value: 1 }; DEFINE STABLE FUNCTION fn::read() -> \
		 int { RETURN <int>item:a.value; };",
	)?;
	let values = execute(
		&store,
		"UPDATE audit:a SET value = 2 WHERE fn::read() = 7 RETURN VALUE fn::read() EXPECT \
		 fn::read() = 7;",
	)?;
	assert_eq!(
		values.last().and_then(Value::as_array).map(crate::values::Array::as_slice),
		Some([Value::from(7)].as_slice())
	);
	Ok(())
}

#[test]
fn event_ddl_lifecycle_uses_stable_identity_and_common_info() -> FerrosGrynnResult<()> {
	let store = Datastore::in_memory();
	execute(
		&store,
		"DEFINE CLASS item TYPE NODE; DEFINE GRAPH scope; DEFINE EVENT audit ON CLASS item WHEN \
		 $event = 'CREATE' THEN { RETURN $after; }; DEFINE EVENT audit ON GRAPH scope ASYNC RETRY \
		 255 MAXDEPTH 0 WHEN true THEN 1;",
	)?;
	let catalog = store.schema_catalog();
	let class = catalog
		.class("item")
		.and_then(|class| class.id())
		.ok_or_else(|| FerrosGrynnError::invalid_argument("fixture class"))?;
	let graph = catalog
		.named_graph("scope")
		.ok_or_else(|| FerrosGrynnError::invalid_argument("fixture graph"))?;
	let event = catalog
		.named_event(EventTarget::Class(class), "audit")
		.ok_or_else(|| FerrosGrynnError::invalid_argument("fixture event"))?;
	let id = event.id();
	execute(
		&store,
		"DEFINE EVENT IF NOT EXISTS audit ON CLASS item WHEN true THEN 2; DEFINE EVENT OVERWRITE \
		 audit ON CLASS item WHEN true THEN 3;",
	)?;
	let current = store
		.schema_catalog()
		.named_event(EventTarget::Class(class), "audit")
		.ok_or_else(|| FerrosGrynnError::invalid_argument("fixture event"))?;
	assert_eq!(current.id(), id);
	assert_eq!(current.version().get(), 2);
	execute(
		&store,
		"ALTER EVENT audit ON CLASS item ASYNC RETRY 2 MAXDEPTH 16 WHEN false THEN { RETURN 4; } \
		 COMMENT 'audit';",
	)?;
	let current = store
		.schema_catalog()
		.named_event(EventTarget::Class(class), "audit")
		.ok_or_else(|| FerrosGrynnError::invalid_argument("fixture event"))?;
	assert_eq!(current.id(), id);
	assert_eq!(current.version().get(), 3);
	assert!(
		matches!(current.execution(), EventExecution::Async(options) if options.retries() == 2 && options.max_depth() == 16)
	);
	let infos = execute(&store, "INFO FOR CLASS item; INFO FOR GRAPH scope;")?;
	for info in infos {
		let events = info
			.as_object()
			.and_then(|info| info.get("events"))
			.and_then(Value::as_object)
			.ok_or_else(|| FerrosGrynnError::invalid_argument("EVENT INFO missing"))?;
		assert!(events.contains_key("audit"));
	}
	execute(
		&store,
		"ALTER EVENT audit ON CLASS item THEN 1, 2; ALTER EVENT audit ON CLASS item DROP ASYNC \
		 DROP WHEN DROP THEN DROP COMMENT; REMOVE EVENT audit ON CLASS item; REMOVE EVENT IF \
		 EXISTS audit ON CLASS item; REMOVE EVENT IF EXISTS missing ON CLASS absent;",
	)?;
	assert!(store.schema_catalog().named_event(EventTarget::Class(class), "audit").is_none());
	assert!(store.schema_catalog().named_event(EventTarget::Graph(graph.id()), "audit").is_some());
	Ok(())
}

#[test]
fn event_ddl_validation_does_not_execute_source() -> FerrosGrynnResult<()> {
	let store = Datastore::in_memory();
	execute(
		&store,
		"DEFINE CLASS item TYPE NODE; DEFINE CLASS audit TYPE NODE; DEFINE EVENT safe ON CLASS \
		 item WHEN true THEN { CREATE audit:unexpected; };",
	)?;
	assert!(store.begin().atom(&"audit:unexpected".parse()?)?.is_none());
	let results = store.execute(
		"DEFINE EVENT bad ON CLASS item WHEN { CREATE audit:forbidden; RETURN true; } THEN 1;",
		&Object::new(),
	);
	assert!(
		results.is_err()
			|| results.is_ok_and(|results| results.iter().any(|result| result.result.is_err()))
	);
	assert!(store.begin().atom(&"audit:forbidden".parse()?)?.is_none());
	Ok(())
}
