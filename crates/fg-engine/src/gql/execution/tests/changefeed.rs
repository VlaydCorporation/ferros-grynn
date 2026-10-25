//! End-to-end language/SDK parity, historical lookup and whole-commit results.

use std::num::NonZeroUsize;

use fg_runtime::__internal::ErrorValueInternal as _;

use crate::{
	changefeed::{ChangefeedConfig, ChangefeedId, ChangefeedSelector, ChangefeedSource},
	datastore::Datastore,
	gql::QueryExecutionOptions,
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	values::{Object, Value},
};

/// Uses ordinary query outcomes, retaining execution errors as typed errors.
fn execute(store: &Datastore, query: &str, variables: &Object) -> FerrosGrynnResult<Value> {
	let results = store.execute(query, variables)?;
	if let Some(error) = results.iter().find_map(|result| result.result.as_ref().err()) {
		return Err(error.clone());
	}
	let result = results
		.last()
		.ok_or_else(|| FerrosGrynnError::invalid_argument("test query returned no outcome"))?;
	result.result.as_ref().map(|outcome| outcome.value().clone()).map_err(Clone::clone)
}

/// Checks read-plan identity, deferred effects and payload-free actual
/// counters.
#[test]
fn changefeed_explain_bindings_hash_modes_resume_and_actual_counters() {
	use crate::test_assertions::AssertResult as _;

	let store = Datastore::in_memory();
	execute(
		&store,
		"DEFINE CLASS item TYPE NODE CHANGEFEED 7d FULL; CREATE item:a SET \
		 secret='payload-must-stay-private'; UPDATE item:a SET secret='payload-must-stay-private';",
		&Object::new(),
	)
	.assert_ok();
	let query = "EXPLAIN FULL FORMAT JSON SHOW CHANGES FOR CLASS item SINCE $since LIMIT 1;";
	let mut variables = Object::new();
	variables.insert("since", 0_u64);
	let net = execute(&store, query, &variables).assert_ok();
	let net_object = net
		.as_object()
		.ok_or_else(|| FerrosGrynnError::invalid_argument("plan missing"))
		.assert_ok();
	assert_eq!(net_object.get("statement_kind"), Some(&Value::from("SHOW CHANGES")));
	let physical = net_object
		.get("nodes")
		.ok_or_else(|| FerrosGrynnError::invalid_argument("physical plan missing"))
		.assert_ok();
	let description = format!("{physical:?}");
	assert!(description.contains("ChangefeedHistoryRead"));
	assert!(description.contains("capture_revision"));
	assert!(description.contains("capture_full"));
	assert!(description.contains("snapshot_through"));
	assert!(!description.contains("payload-must-stay-private"));
	assert!(!description.contains("Publication"));
	let repeat = execute(&store, query, &variables).assert_ok();
	assert_eq!(
		net_object.get("plan_hash"),
		repeat.as_object().and_then(|plan| plan.get("plan_hash"))
	);
	variables.insert("since", 1_u64);
	let shifted = execute(&store, query, &variables).assert_ok();
	assert_ne!(
		net_object.get("plan_hash"),
		shifted.as_object().and_then(|plan| plan.get("plan_hash"))
	);
	variables.insert("since", 0_u64);
	let full = execute(
		&store,
		"EXPLAIN FULL FORMAT JSON SHOW CHANGES FOR CLASS item SINCE $since FULL LIMIT 1;",
		&variables,
	)
	.assert_ok();
	assert_ne!(
		net_object.get("plan_hash"),
		full.as_object().and_then(|plan| plan.get("plan_hash"))
	);
	let analyzed = execute(
		&store,
		"EXPLAIN ANALYZE FORMAT JSON SHOW CHANGES FOR CLASS item SINCE 0 FULL;",
		&Object::new(),
	)
	.assert_ok();
	let analysis = analyzed
		.as_object()
		.and_then(|plan| plan.get("analyze"))
		.and_then(Value::as_object)
		.ok_or_else(|| FerrosGrynnError::invalid_argument("observation missing"))
		.assert_ok();
	assert_eq!(analysis.get("changefeed_entries"), Some(&Value::from(2_u64)));
	assert_eq!(analysis.get("changefeed_page_reads"), Some(&Value::from(0_u64)));
	assert_eq!(analysis.get("publication_state"), Some(&Value::from("not_applicable")));
	assert_eq!(analysis.get("affected_rows"), Some(&Value::from(0_u64)));
	let batches =
		execute(&store, "SHOW CHANGES FOR CLASS item SINCE 0 LIMIT 1;", &Object::new()).assert_ok();
	let cursor = row(&batches, 0)
		.assert_ok()
		.get("cursor")
		.ok_or_else(|| FerrosGrynnError::invalid_argument("cursor missing"))
		.assert_ok()
		.clone();
	variables.insert("since", cursor);
	let resumed = execute(&store, query, &variables).assert_ok();
	assert!(format!("{resumed:?}").contains("resume"));
	assert!(
		execute(
			&store,
			"EXPLAIN FULL FORMAT JSON SHOW CHANGES FOR CLASS item SINCE $since FULL;",
			&variables
		)
		.is_err()
	);
	execute(
		&store,
		"DEFINE IMPURE FUNCTION fn::deferred() -> int { THROW 'effect must wait'; RETURN 0; };",
		&Object::new(),
	)
	.assert_ok();
	assert!(
		execute(
			&store,
			"EXPLAIN FULL FORMAT JSON SHOW CHANGES FOR CLASS item SINCE fn::deferred();",
			&Object::new()
		)
		.is_ok()
	);
	assert!(
		execute(
			&store,
			"EXPLAIN ANALYZE FORMAT JSON SHOW CHANGES FOR CLASS item SINCE fn::deferred();",
			&Object::new()
		)
		.is_err()
	);
	for query in [
		"SHOW CHANGES FOR CLASS item SINCE fn::deferred() LIMIT -1;",
		"SHOW CHANGES FOR CLASS missing SINCE fn::deferred() LIMIT 1;",
		"SHOW CHANGES FOR CHANGEFEED fn::deferred() SINCE -1 LIMIT 1;",
	] {
		let ordinary = execute(&store, query, &Object::new())
			.err()
			.ok_or_else(|| FerrosGrynnError::invalid_argument("ordinary read should throw"))
			.assert_ok();
		let analyzed =
			execute(&store, &format!("EXPLAIN ANALYZE FORMAT JSON {query}"), &Object::new())
				.err()
				.ok_or_else(|| FerrosGrynnError::invalid_argument("analyzed read should throw"))
				.assert_ok();
		assert_eq!(ordinary.status().condition(), analyzed.status().condition());
		assert!(format!("{ordinary}").contains("effect must wait"));
		assert!(format!("{analyzed}").contains("effect must wait"));
	}
	let first_closure = execute(
		&store,
		"EXPLAIN FULL FORMAT JSON SHOW CHANGES FOR CLASS item SINCE (|$x| -> int { RETURN 1; \
		 })(0);",
		&Object::new(),
	)
	.assert_ok();
	let second_closure = execute(
		&store,
		"EXPLAIN FULL FORMAT JSON SHOW CHANGES FOR CLASS item SINCE (|$x| -> int { RETURN 2; \
		 })(0);",
		&Object::new(),
	)
	.assert_ok();
	assert_ne!(
		first_closure.as_object().and_then(|plan| plan.get("plan_hash")),
		second_closure.as_object().and_then(|plan| plan.get("plan_hash"))
	);
	let text = execute(
		&store,
		"EXPLAIN ANALYZE SHOW CHANGES FOR CLASS item SINCE 0 FULL;",
		&Object::new(),
	)
	.assert_ok();
	assert!(format!("{text:?}").contains("changefeed_entries=2"));
}

/// Fully retired identities retain independent cursor-origin/snapshot
/// validation.
#[test]
fn changefeed_explain_retired_cursor_validation_uses_committed_context() {
	use crate::{
		changefeed::{ChangefeedCursor, ChangefeedReadMode, Versionstamp},
		test_assertions::AssertResult as _,
	};
	let store = Datastore::in_memory();
	execute(&store, "DEFINE CLASS item TYPE NODE CHANGEFEED 7d;", &Object::new()).assert_ok();
	let info =
		store.begin().changefeed_info(ChangefeedSelector::ClassName("item".into())).assert_ok();
	let retired = ChangefeedId::allocate();
	let mut variables = Object::new();
	variables.insert("id", retired.as_uuid());
	for (origin, stamp) in [
		(crate::values::Uuid::now_v7(), info.snapshot_versionstamp()),
		(info.database_id(), Versionstamp::new(u64::MAX)),
	] {
		let cursor =
			ChangefeedCursor::new(origin, retired, ChangefeedReadMode::Net, stamp).assert_ok();
		variables.insert("cursor", Value::Bytes(cursor.encode()));
		assert!(
			execute(&store, "SHOW CHANGES FOR CHANGEFEED $id SINCE $cursor;", &variables).is_err()
		);
		assert!(
			execute(
				&store,
				"EXPLAIN FULL FORMAT JSON SHOW CHANGES FOR CHANGEFEED $id SINCE $cursor;",
				&variables
			)
			.is_err()
		);
	}
	let cursor = ChangefeedCursor::new(
		info.database_id(),
		retired,
		ChangefeedReadMode::Net,
		info.snapshot_versionstamp(),
	)
	.assert_ok();
	variables.insert("cursor", Value::Bytes(cursor.encode()));
	let plan = execute(
		&store,
		"EXPLAIN FULL FORMAT JSON SHOW CHANGES FOR CHANGEFEED $id SINCE $cursor;",
		&variables,
	)
	.assert_ok();
	assert!(format!("{plan:?}").contains("retired_or_unpublished"));
}

/// Successive analyzed statements own scratch peaks rather than inheriting
/// maxima.
#[test]
fn changefeed_explain_scratch_peak_is_scoped_to_each_statement() {
	use crate::{
		datastore::ClassSchema,
		graph::AtomKind,
		test_assertions::AssertResult as _,
		values::PropertyMap,
	};
	let store = Datastore::in_memory();
	let mut tx = store.begin();
	for (name, count) in [("big", 100), ("small", 1)] {
		let class =
			tx.define_class(ClassSchema::new(name, AtomKind::Node, []).assert_ok()).assert_ok();
		tx.configure_changefeed(
			ChangefeedSource::Class(class),
			ChangefeedConfig::from_nanoseconds(86_400_000_000_000).assert_ok(),
		)
		.assert_ok();
		for ordinal in 0..count {
			tx.create_atom(format!("{name}:{ordinal}").parse().assert_ok(), PropertyMap::new())
				.assert_ok();
		}
	}
	tx.commit().assert_ok();
	let results = store
		.execute(
			"EXPLAIN ANALYZE FORMAT JSON SHOW CHANGES FOR CLASS big SINCE 0; EXPLAIN ANALYZE \
			 FORMAT JSON SHOW CHANGES FOR CLASS small SINCE 0;",
			&Object::new(),
		)
		.assert_ok();
	let mut peaks = Vec::new();
	for result in results {
		let value = result.result.assert_ok().into_parts().0;
		let peak = value
			.as_object()
			.and_then(|plan| plan.get("analyze"))
			.and_then(Value::as_object)
			.and_then(|analysis| analysis.get("changefeed_peak_scratch_bytes"))
			.ok_or_else(|| FerrosGrynnError::invalid_argument("scratch peak absent"))
			.assert_ok();
		peaks.push(peak.clone());
	}
	assert_eq!(peaks.len(), 2);
	assert!(peaks[0] > peaks[1]);
}

/// Extracts a hash from a transaction-private EXPLAIN without publishing DDL.
fn private_plan_hash(tx: &mut crate::datastore::Transaction) -> FerrosGrynnResult<Value> {
	let results = tx.execute(
		"EXPLAIN FULL FORMAT JSON SHOW CHANGES FOR CLASS item SINCE fn::stamp();",
		&Object::new(),
	)?;
	let result =
		results.last().ok_or_else(|| FerrosGrynnError::invalid_argument("private plan absent"))?;
	result
		.result
		.as_ref()
		.map_err(Clone::clone)?
		.value()
		.as_object()
		.and_then(|plan| plan.get("plan_hash"))
		.cloned()
		.ok_or_else(|| FerrosGrynnError::invalid_argument("private hash absent"))
}

/// Private definitions and rollback/redefine cannot reuse an executable hash.
#[test]
fn changefeed_explain_hashes_private_function_versions_and_bodies() {
	use crate::test_assertions::AssertResult as _;

	let store = Datastore::in_memory();
	execute(
		&store,
		"DEFINE CLASS item TYPE NODE CHANGEFEED 7d; DEFINE PURE FUNCTION fn::stamp() -> int { \
		 RETURN 0; };",
		&Object::new(),
	)
	.assert_ok();
	let mut tx = store.begin();
	let before = private_plan_hash(&mut tx).assert_ok();
	let savepoint = tx.gql_savepoint();
	for result in
		tx.execute("ALTER FUNCTION fn::stamp() { RETURN 1; };", &Object::new()).assert_ok()
	{
		let _outcome = result.result.assert_ok();
	}
	let first = private_plan_hash(&mut tx).assert_ok();
	assert_ne!(before, first);
	tx.restore_gql_savepoint(savepoint);
	assert_eq!(private_plan_hash(&mut tx).assert_ok(), before);
	for result in
		tx.execute("ALTER FUNCTION fn::stamp() { RETURN 2; };", &Object::new()).assert_ok()
	{
		let _outcome = result.result.assert_ok();
	}
	assert_ne!(private_plan_hash(&mut tx).assert_ok(), first);
}

/// Compiled UDF and session closure operands retain their defining source.
#[test]
fn changefeed_explain_inside_saved_functions_owns_operand_provenance() {
	use crate::test_assertions::AssertResult as _;

	let store = Datastore::in_memory();
	execute(
		&store,
		"DEFINE CLASS item TYPE NODE CHANGEFEED 7d; DEFINE STABLE FUNCTION fn::inspect() -> int { \
		 EXPLAIN FULL FORMAT JSON SHOW CHANGES FOR CLASS item SINCE 0; RETURN 0; };",
		&Object::new(),
	)
	.assert_ok();
	assert_eq!(
		execute(&store, "RETURN fn::inspect();", &Object::new()).assert_ok(),
		Value::from(0)
	);
	let mut session = store.session();
	for result in session
		.execute(
			"LET $inspect = | | -> int { EXPLAIN FULL FORMAT JSON SHOW CHANGES FOR CLASS item \
			 SINCE 0; RETURN 0; };",
			&Object::new(),
		)
		.assert_ok()
	{
		let _outcome = result.result.assert_ok();
	}
	let results = session.execute("RETURN $inspect();", &Object::new()).assert_ok();
	let result = results
		.last()
		.ok_or_else(|| FerrosGrynnError::invalid_argument("closure result absent"))
		.assert_ok();
	assert_eq!(result.result.as_ref().map_err(Clone::clone).assert_ok().value(), &Value::from(0));
}

/// Recursive runtime values and byte-heavy descriptors are rejected before
/// Hash.
#[test]
fn changefeed_explain_admits_deep_path_geometry_error_and_hash_work() {
	use std::sync::Arc;

	use crate::{
		graph::{
			AtomKind,
			AtomOccurrenceRef,
			AtomRef,
			EndpointRole,
			Path,
			PathDirection,
			PathStep,
			SnapshotId,
		},
		test_assertions::AssertResult as _,
		values::{Array, ErrorValue, Geometry, RecordId, RecordIdPart, Uuid},
	};
	let store = Datastore::in_memory();
	execute(
		&store,
		"DEFINE CLASS item TYPE NODE CHANGEFEED 7d; DEFINE GRAPH scope TYPE GRAPH;",
		&Object::new(),
	)
	.assert_ok();
	let graph = store
		.begin()
		.named_graph("scope")
		.assert_ok()
		.ok_or_else(|| FerrosGrynnError::invalid_argument("graph absent"))
		.assert_ok();
	let mut nested = Value::from(0);
	for _ in 0..62 {
		nested = Value::Array(Array::new(vec![nested]));
	}
	let record = RecordId::new("item", RecordIdPart::Array(Array::new(vec![nested]))).assert_ok();
	let occurrence = AtomOccurrenceRef::new(AtomRef::new(AtomKind::Node, record), Arc::new(graph));
	let path = Path::new(
		Uuid::now_v7(),
		SnapshotId::new(0, 0),
		vec![PathStep::new(occurrence, None, PathDirection::Forward, EndpointRole::Source)],
		None,
	);
	let mut geometry = Geometry::Collection(Vec::new());
	let mut error = FerrosGrynnError::invalid_argument("leaf");
	for _ in 0..65 {
		geometry = Geometry::Collection(vec![geometry]);
		error = FerrosGrynnError::invalid_argument("cause").with_cause(error);
	}
	for value in [
		Value::Path(Box::new(path)),
		Value::Geometry(geometry),
		Value::Error(Box::new(ErrorValue::from_error(&error))),
		Value::from("x".repeat(1_000_001)),
	] {
		let mut variables = Object::new();
		variables.insert("unused", value);
		let error = execute(
			&store,
			"EXPLAIN FULL FORMAT JSON SHOW CHANGES FOR CLASS item SINCE 0;",
			&variables,
		)
		.err()
		.ok_or_else(|| FerrosGrynnError::invalid_argument("hash work should be bounded"))
		.assert_ok();
		assert_eq!(error.status().condition(), crate::gql_status::Condition::ProgramLimitExceeded);
	}
}

/// Extracts one complete result object without indexing an unchecked ordinal.
fn row(value: &Value, ordinal: usize) -> FerrosGrynnResult<&Object> {
	value
		.as_array()
		.and_then(|rows| rows.get(ordinal))
		.and_then(Value::as_object)
		.ok_or_else(|| FerrosGrynnError::invalid_argument("test batch row absent"))
}

/// Reads named fields in captured images rather than looking up live records.
fn image_property(entry: &Value, side: &str, property: &str) -> Option<Value> {
	entry
		.as_object()?
		.get(side)?
		.as_object()?
		.get("properties")?
		.as_object()?
		.get(property)
		.cloned()
}

/// Verifies changefeed gql final policy full net images cursor and info.
#[test]
fn changefeed_gql_final_policy_full_net_images_cursor_and_info() {
	use crate::test_assertions::AssertResult as _;

	let store = Datastore::in_memory();
	execute(&store, "DEFINE CLASS item TYPE NODE;", &Object::new()).assert_ok();
	let mut tx = store.begin();
	tx.execute(
		"CREATE item:a SET value=1; UPDATE item:a SET value=2; ALTER CLASS item CHANGEFEED 7d \
		 INCLUDE ORIGINAL FULL;",
		&Object::new(),
	)
	.assert_ok();
	let stamp = tx.commit().assert_ok().versionstamp();
	let full = execute(&store, "SHOW CHANGES FOR CLASS item SINCE 0 FULL LIMIT 1;", &Object::new())
		.assert_ok();
	let batch = row(&full, 0).assert_ok();
	assert_eq!(batch.get("versionstamp"), Some(&Value::from(stamp.get())));
	let changes = batch
		.get("changes")
		.and_then(Value::as_array)
		.ok_or_else(|| FerrosGrynnError::invalid_argument("changes absent"))
		.assert_ok()
		.as_slice();
	assert_eq!(changes.len(), 2);
	assert_eq!(image_property(&changes[0], "after", "value"), Some(1.into()));
	assert_eq!(image_property(&changes[1], "before", "value"), Some(1.into()));
	assert_eq!(image_property(&changes[1], "after", "value"), Some(2.into()));
	let net =
		execute(&store, "SHOW CHANGES FOR CLASS item SINCE 0 LIMIT 1;", &Object::new()).assert_ok();
	assert_eq!(
		row(&net, 0)
			.assert_ok()
			.get("changes")
			.and_then(Value::as_array)
			.map(|changes| changes.len()),
		Some(1)
	);
	let mut parameters = Object::new();
	parameters.insert(
		"id",
		batch
			.get("source")
			.cloned()
			.ok_or_else(|| FerrosGrynnError::invalid_argument("source absent"))
			.assert_ok(),
	);
	parameters.insert(
		"since",
		batch
			.get("cursor")
			.cloned()
			.ok_or_else(|| FerrosGrynnError::invalid_argument("cursor absent"))
			.assert_ok(),
	);
	let info = execute(&store, "INFO FOR CHANGEFEED $id;", &parameters).assert_ok();
	assert_eq!(info.as_object().and_then(|info| info.get("full")), Some(&Value::Bool(true)));
	assert!(execute(&store, "SHOW CHANGES FOR CHANGEFEED $id SINCE $since;", &parameters).is_err());
	let resumed =
		execute(&store, "SHOW CHANGES FOR CHANGEFEED $id SINCE $since FULL;", &parameters)
			.assert_ok();
	assert!(resumed.as_array().is_some_and(|rows| rows.is_empty()));
	execute(&store, "UPDATE item:a SET value=3 RETURN NONE;", &Object::new()).assert_ok();
	let resumed =
		execute(&store, "SHOW CHANGES FOR CHANGEFEED $id SINCE $since FULL LIMIT 1;", &parameters)
			.assert_ok();
	assert_eq!(
		row(&resumed, 0)
			.assert_ok()
			.get("changes")
			.and_then(Value::as_array)
			.map(|changes| changes.len()),
		Some(1)
	);
	drop(store);
	assert_eq!(image_property(&changes[1], "before", "value"), Some(1.into()));
}

/// Verifies changefeed gql historical full coverage drop and recreated name.
#[test]
fn changefeed_gql_historical_full_coverage_drop_and_recreated_name() {
	use crate::test_assertions::AssertResult as _;

	let store = Datastore::in_memory();
	execute(
		&store,
		"DEFINE CLASS item TYPE NODE CHANGEFEED 7d; CREATE item:a SET value=1; ALTER CLASS item \
		 CHANGEFEED 7d FULL; UPDATE item:a SET value=2;",
		&Object::new(),
	)
	.assert_ok();
	assert!(
		execute(&store, "SHOW CHANGES FOR CLASS item SINCE 0 FULL LIMIT 1;", &Object::new())
			.is_err()
	);
	let info =
		store.begin().changefeed_info(ChangefeedSelector::ClassName("item".into())).assert_ok();
	let mut variables = Object::new();
	variables.insert("id", info.id().as_uuid());
	let mut tx = store.begin();
	tx.execute("UPDATE item:a SET value=3; ALTER CLASS item DROP CHANGEFEED;", &Object::new())
		.assert_ok();
	tx.commit().assert_ok();
	let gap = execute(&store, "SHOW CHANGES FOR CLASS item SINCE 0;", &Object::new()).assert_ok();
	assert_eq!(row(&gap, 2).assert_ok().get("reason"), Some(&Value::from("SourceDropped")));
	execute(&store, "DELETE item:a RETURN NONE; REMOVE CLASS item;", &Object::new()).assert_ok();
	let history =
		execute(&store, "SHOW CHANGES FOR CLASS item SINCE 0;", &Object::new()).assert_ok();
	assert_eq!(history, gap);
	execute(
		&store,
		"DEFINE CLASS item TYPE NODE CHANGEFEED 7d FULL; CREATE item:new;",
		&Object::new(),
	)
	.assert_ok();
	let active =
		execute(&store, "SHOW CHANGES FOR CLASS item SINCE 0 FULL;", &Object::new()).assert_ok();
	assert_eq!(active.as_array().map(|rows| rows.len()), Some(1));
	let old = execute(&store, "SHOW CHANGES FOR CHANGEFEED $id SINCE 0;", &variables).assert_ok();
	assert_eq!(old, history);
}

/// Verifies changefeed gql graph images use qualified occurrences and overlay
/// decisions.
#[test]
fn changefeed_gql_graph_images_use_qualified_occurrences_and_overlay_decisions() {
	use crate::test_assertions::AssertResult as _;

	let store = Datastore::in_memory();
	execute(
		&store,
		"DEFINE CLASS item TYPE NODE; DEFINE CLASS link TYPE EDGE; DEFINE GRAPH network TYPE \
		 GRAPH CHANGEFEED 7d FULL; CREATE item:a SET value=1; CREATE item:b;",
		&Object::new(),
	)
	.assert_ok();
	let mut tx = store.begin();
	tx.execute(
		"ATTACH item:a TO GRAPH network; ATTACH item:b TO GRAPH network; RELATE \
		 (item:a)-[link:ab]->(item:b) IN GRAPH network; UPDATE item:a IN GRAPH network SET \
		 value=2;",
		&Object::new(),
	)
	.assert_ok();
	tx.commit().assert_ok();
	let mut variables = Object::new();
	variables.insert(
		"graph",
		store
			.begin()
			.named_graph("network")
			.assert_ok()
			.ok_or_else(|| FerrosGrynnError::invalid_argument("graph absent"))
			.assert_ok(),
	);
	let history =
		execute(&store, "SHOW CHANGES FOR GRAPH $graph SINCE 0 FULL LIMIT 1;", &variables)
			.assert_ok();
	let entries = row(&history, 0)
		.assert_ok()
		.get("changes")
		.and_then(Value::as_array)
		.ok_or_else(|| FerrosGrynnError::invalid_argument("graph changes absent"))
		.assert_ok()
		.as_slice();
	assert_eq!(entries.len(), 4);
	for entry in entries {
		let after = entry
			.as_object()
			.and_then(|entry| entry.get("after"))
			.and_then(Value::as_object)
			.ok_or_else(|| FerrosGrynnError::invalid_argument("graph image absent"))
			.assert_ok();
		assert!(matches!(after.get("identity"), Some(Value::AtomOccurrence(_))));
	}
	let final_image = entries[3]
		.as_object()
		.and_then(|entry| entry.get("after"))
		.and_then(Value::as_object)
		.ok_or_else(|| FerrosGrynnError::invalid_argument("overlay image absent"))
		.assert_ok();
	assert_eq!(
		final_image
			.get("properties")
			.and_then(Value::as_object)
			.and_then(|properties| properties.get("value")),
		Some(&Value::from(2))
	);
	assert_eq!(
		final_image
			.get("overlays")
			.and_then(Value::as_object)
			.and_then(|overlays| overlays.get("value"))
			.and_then(Value::as_object)
			.and_then(|decision| decision.get("kind")),
		Some(&Value::from("value"))
	);
}

/// Verifies changefeed gql operand admission precedes large cursor or selector
/// copy.
#[test]
fn changefeed_gql_operand_admission_precedes_large_cursor_or_selector_copy() {
	use crate::test_assertions::AssertResult as _;

	let store = Datastore::in_memory();
	execute(&store, "DEFINE CLASS item TYPE NODE CHANGEFEED 7d;", &Object::new()).assert_ok();
	let mut variables = Object::new();
	variables.insert("cursor", Value::Bytes(vec![0; 1024 * 1024].into()));
	variables.insert("selector", "x".repeat(1024 * 1024));
	for query in [
		"SHOW CHANGES FOR CLASS item SINCE $cursor;",
		"SHOW CHANGES FOR CHANGEFEED $selector SINCE 0;",
		"SHOW CHANGES FOR GRAPH $selector SINCE 0;",
		"INFO FOR CHANGEFEED $selector;",
	] {
		let options = QueryExecutionOptions {
			barrier_memory_bytes: NonZeroUsize::new(128 * 1024)
				.ok_or_else(|| FerrosGrynnError::invalid_argument("test budget missing"))
				.assert_ok(),
			..QueryExecutionOptions::default()
		};
		let results = store.execute_with_options(query, &variables, options).assert_ok();
		let error = results[0]
			.result
			.as_ref()
			.err()
			.ok_or_else(|| {
				FerrosGrynnError::invalid_argument("operand exceeded budget without error")
			})
			.assert_ok();
		assert!(error.to_string().contains("changefeed operand scratch"), "{error}");
	}
	let mut nested = Value::from(0);
	for _ in 0..65 {
		nested = Value::Array(crate::values::Array::new(vec![nested]));
	}
	let range =
		crate::values::Range::new(std::ops::Bound::Included(nested), std::ops::Bound::Unbounded)
			.assert_ok();
	let mut variables = Object::new();
	variables.insert("since", range);
	let results = store.execute("INFO FOR CHANGEFEED $since;", &variables).assert_ok();
	let error = results[0]
		.result
		.as_ref()
		.err()
		.ok_or_else(|| FerrosGrynnError::invalid_argument("deep operand did not fail"))
		.assert_ok();
	assert!(
		matches!(error.status().status(), crate::gql_status::Status::StorageDecodeLimitExceeded { limit, actual: 65, maximum: 64 } if limit == "SHOW CHANGES depth")
	);
}

/// Verifies changefeed gql import on keeps all segments and off returns gap.
#[test]
fn changefeed_gql_import_on_keeps_all_segments_and_off_returns_gap() {
	use crate::test_assertions::AssertResult as _;

	let store = Datastore::in_memory();
	execute(&store, "DEFINE CLASS item TYPE NODE CHANGEFEED 7d FULL;", &Object::new()).assert_ok();
	execute(
		&store,
		"BEGIN IMPORT WITH (CHANGEFEED=ON, SEGMENT SIZE=1); CREATE item:a SET value=1; UPDATE \
		 item:a SET value=1; UPDATE item:a SET value=2; END IMPORT;",
		&Object::new(),
	)
	.assert_ok();
	let history =
		execute(&store, "SHOW CHANGES FOR CLASS item SINCE 0 FULL;", &Object::new()).assert_ok();
	assert_eq!(history.as_array().map(|rows| rows.len()), Some(1));
	assert_eq!(
		row(&history, 0)
			.assert_ok()
			.get("changes")
			.and_then(Value::as_array)
			.map(|changes| changes.len()),
		Some(3)
	);
	execute(
		&store,
		"BEGIN IMPORT WITH (CHANGEFEED=OFF); UPDATE item:a SET value=3; END IMPORT;",
		&Object::new(),
	)
	.assert_ok();
	let history =
		execute(&store, "SHOW CHANGES FOR CLASS item SINCE 0 FULL;", &Object::new()).assert_ok();
	assert_eq!(row(&history, 1).assert_ok().get("reason"), Some(&Value::from("ImportSuppressed")));
}

/// Verifies changefeed gql rejects invalid configuration types and result
/// resources.
#[test]
fn changefeed_gql_rejects_invalid_configuration_types_and_result_resources() {
	use crate::test_assertions::AssertResult as _;

	let store = Datastore::in_memory();
	for query in [
		"DEFINE CLASS item TYPE NODE CHANGEFEED 0d;",
		"DEFINE CLASS item TYPE NODE CHANGEFEED 1mo;",
		"DEFINE CLASS item TYPE NODE CHANGEFEED 1y;",
		"DEFINE CLASS item TYPE NODE CHANGEFEED 7 d;",
	] {
		assert!(execute(&store, query, &Object::new()).is_err(), "{query}");
		assert!(store.class("item").is_none());
	}
	execute(
		&store,
		"DEFINE CLASS item TYPE NODE CHANGEFEED 7d FULL; CREATE item:a SET value=1;",
		&Object::new(),
	)
	.assert_ok();
	for query in [
		"SHOW CHANGES FOR CLASS item SINCE 0.5;",
		"SHOW CHANGES FOR CLASS item SINCE -1;",
		"SHOW CHANGES FOR CLASS item SINCE true;",
		"SHOW CHANGES FOR CLASS item SINCE 0 LIMIT -1;",
		"SHOW CHANGES FOR CLASS item SINCE 0 LIMIT 0.5;",
	] {
		assert!(execute(&store, query, &Object::new()).is_err(), "{query}");
	}
	let options = QueryExecutionOptions {
		barrier_memory_bytes: NonZeroUsize::new(8192)
			.ok_or_else(|| FerrosGrynnError::invalid_argument("test budget missing"))
			.assert_ok(),
		..QueryExecutionOptions::default()
	};
	let results = store
		.execute_with_options("SHOW CHANGES FOR CLASS item SINCE 0 FULL;", &Object::new(), options)
		.assert_ok();
	assert!(results[0].result.is_err());
	let info =
		store.begin().changefeed_info(ChangefeedSelector::ClassName("item".into())).assert_ok();
	let id: ChangefeedId = info.id().as_uuid().to_string().parse().assert_ok();
	assert_eq!(id, info.id());
}

/// Verifies changefeed gql admits long labels and historical metadata before
/// cloning.
#[test]
fn changefeed_gql_admits_long_labels_and_historical_metadata_before_cloning() {
	use crate::test_assertions::AssertResult as _;

	let store = Datastore::in_memory();
	execute(
		&store,
		"DEFINE CLASS item TYPE NODE; DEFINE GRAPH network TYPE GRAPH CHANGEFEED 7d FULL; CREATE \
		 item:a;",
		&Object::new(),
	)
	.assert_ok();
	let mut tx = store.begin();
	let graph = tx
		.named_graph("network")
		.assert_ok()
		.ok_or_else(|| FerrosGrynnError::invalid_argument("graph absent"))
		.assert_ok();
	tx.attach_node(
		graph.id(),
		&"item:a".parse().assert_ok(),
		Some("x".repeat(1024 * 1024).into()),
		None,
	)
	.assert_ok();
	tx.commit().assert_ok();
	let options = QueryExecutionOptions {
		barrier_memory_bytes: NonZeroUsize::new(128 * 1024)
			.ok_or_else(|| FerrosGrynnError::invalid_argument("test budget missing"))
			.assert_ok(),
		..QueryExecutionOptions::default()
	};
	let results = store
		.execute_with_options(
			"SHOW CHANGES FOR GRAPH network SINCE 0 FULL;",
			&Object::new(),
			options,
		)
		.assert_ok();
	assert!(results[0].result.is_err());
	let source = ChangefeedSource::Graph(graph.id());
	let configuration =
		ChangefeedConfig::from_nanoseconds(7 * 24 * 3600 * 1_000_000_000).assert_ok();
	for epoch in 0..1024 {
		let mut tx = store.begin();
		tx.configure_changefeed(source, configuration.with_full(epoch % 2 == 0)).assert_ok();
		tx.commit().assert_ok();
	}
	let info = store.begin().changefeed_info(source).assert_ok();
	assert!(info.capture_epochs().len() >= 1024);
	let mut variables = Object::new();
	variables.insert("id", info.id().as_uuid());
	let options = QueryExecutionOptions {
		barrier_memory_bytes: NonZeroUsize::new(64 * 1024)
			.ok_or_else(|| FerrosGrynnError::invalid_argument("test budget missing"))
			.assert_ok(),
		..QueryExecutionOptions::default()
	};
	let results =
		store.execute_with_options("INFO FOR CHANGEFEED $id;", &variables, options).assert_ok();
	assert!(results[0].result.is_err());
	assert!(execute(&store, "INFO FOR CHANGEFEED $id;", &variables).is_ok());
}

/// Verifies changefeed gql show operand effects remain transitive.
#[test]
fn changefeed_gql_show_operand_effects_remain_transitive() {
	use crate::test_assertions::AssertResult as _;

	let store = Datastore::in_memory();
	execute(
		&store,
		"DEFINE CLASS item TYPE NODE CHANGEFEED 7d; DEFINE IMPURE FUNCTION fn::stamp() -> int { \
		 RETURN 0; };",
		&Object::new(),
	)
	.assert_ok();
	for body in [
		"SHOW CHANGES FOR CLASS item SINCE fn::stamp();",
		"SHOW CHANGES FOR CLASS item SINCE 0 LIMIT fn::stamp();",
	] {
		let query = format!("DEFINE STABLE FUNCTION fn::invalid() -> int {{ {body} RETURN 0; }};");
		assert!(execute(&store, &query, &Object::new()).is_err());
	}
}
