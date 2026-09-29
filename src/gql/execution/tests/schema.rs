use crate::{
	datastore::{ClassIndexKind, ClassSchemaMode, Datastore, FunctionEffect, IndexScope},
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	values::{AtomKind, Object, Type, Value},
};

#[test]
fn durable_named_function_supports_ddl_overloads_and_calls() -> FerrosGrynnResult<()> {
	let store = Datastore::in_memory();
	let defined = store.execute(
		"DEFINE PURE FUNCTION fn::increment(value: int) -> int { RETURN $value + 1; } COMMENT \
		 'integer';",
		&Object::new(),
	)?;
	if let Some(error) = defined.iter().find_map(|result| result.result.as_ref().err()) {
		return Err(FerrosGrynnError::invalid_argument(format!(
			"named function definition returned a statement error: {error}"
		)));
	}
	let results = store.execute(
		"RETURN fn::increment(41); INFO FOR FUNCTION fn::increment(int);",
		&Object::new(),
	)?;
	if let Some(error) = results.iter().find_map(|result| result.result.as_ref().err()) {
		return Err(FerrosGrynnError::invalid_argument(format!(
			"named function invocation returned a statement error: {error}"
		)));
	}
	if results[0].result.as_ref().map_err(Clone::clone)?.value() != &Value::from(42_i64) {
		return Err(FerrosGrynnError::invalid_argument(
			"named function returned an unexpected value",
		));
	}
	let info =
		results[1].result.as_ref().map_err(Clone::clone)?.value().as_object().ok_or_else(|| {
			FerrosGrynnError::invalid_argument("INFO FUNCTION returned no object")
		})?;
	if info.get("effect") != Some(&Value::from("pure"))
		|| info.get("comment") != Some(&Value::from("integer"))
	{
		return Err(FerrosGrynnError::invalid_argument("function metadata was not retained"));
	}

	let catalog = store.schema_catalog();
	let before = catalog.functions().next().cloned().ok_or_else(|| {
		FerrosGrynnError::invalid_argument("function was not published in the schema catalog")
	})?;
	let altered = store.execute(
		"ALTER FUNCTION fn::increment(int) { RETURN $value + 2; }; RETURN fn::increment(40);",
		&Object::new(),
	)?;
	if altered.iter().any(|result| result.result.is_err())
		|| altered[1].result.as_ref().map_err(Clone::clone)?.value() != &Value::from(42_i64)
	{
		return Err(FerrosGrynnError::invalid_argument("ALTER FUNCTION did not replace its body"));
	}
	let after = store
		.schema_catalog()
		.functions()
		.next()
		.cloned()
		.ok_or_else(|| FerrosGrynnError::invalid_argument("altered function disappeared"))?;
	if before.id() != after.id() || before.version() >= after.version() {
		return Err(FerrosGrynnError::invalid_argument(
			"ALTER FUNCTION did not preserve identity and advance version",
		));
	}
	if after.effect() != FunctionEffect::Pure {
		return Err(FerrosGrynnError::invalid_argument("ALTER FUNCTION changed its effect"));
	}
	let invalid_alter = store.execute(
		"ALTER FUNCTION fn::increment(int) { LET $_ = time::now(); RETURN $value; };",
		&Object::new(),
	)?;
	assert!(invalid_alter[0].result.is_err());
	let unchanged = store.execute("RETURN fn::increment(40);", &Object::new())?;
	assert_eq!(unchanged[0].result.as_ref().map_err(Clone::clone)?.value(), &Value::from(42_i64));

	let removed = store.execute(
		"REMOVE FUNCTION fn::increment(int); INFO FOR FUNCTION fn::increment(int);",
		&Object::new(),
	)?;
	if removed[0].result.is_err() || removed[1].result.is_ok() {
		return Err(FerrosGrynnError::invalid_argument("REMOVE FUNCTION did not remove overload"));
	}
	Ok(())
}

#[test]
fn named_function_arguments_bind_by_declared_name() -> FerrosGrynnResult<()> {
	let store = Datastore::in_memory();
	let defined = store.execute(
		"DEFINE PURE FUNCTION fn::subtract(left: int, right: int) -> int { RETURN $left - $right; \
		 };",
		&Object::new(),
	)?;
	assert!(defined[0].result.is_ok());

	let called = store.execute(
		"RETURN fn::subtract(right = 2, left = 5); RETURN fn::subtract(8, right = 3);",
		&Object::new(),
	)?;
	assert_eq!(called[0].result.as_ref().map_err(Clone::clone)?.value(), &Value::from(3_i64));
	assert_eq!(called[1].result.as_ref().map_err(Clone::clone)?.value(), &Value::from(5_i64));

	let unknown = store.execute("RETURN fn::subtract(other = 2, left = 5);", &Object::new())?;
	assert!(unknown[0].result.is_err());
	assert!(store.execute("RETURN fn::subtract(left = 5, left = 2);", &Object::new()).is_err());
	assert!(store.execute("RETURN fn::subtract(left = 5, 2);", &Object::new()).is_err());
	Ok(())
}

#[test]
fn named_function_effects_are_transitive_and_deterministic_requires_pure() -> FerrosGrynnResult<()>
{
	let store = Datastore::in_memory();
	let defined = store.execute(
		"DEFINE PURE FUNCTION fn::increment(value: int) -> int { RETURN $value + 1; }; DEFINE \
		 PURE FUNCTION fn::twice(value: int) -> int { RETURN \
		 fn::increment(fn::increment($value)); }; DEFINE STABLE FUNCTION fn::clock() -> datetime \
		 { RETURN time::now(); };",
		&Object::new(),
	)?;
	if let Some(error) = defined.iter().find_map(|result| result.result.as_ref().err()) {
		return Err(FerrosGrynnError::invalid_argument(format!(
			"transitive named-function definition failed: {error}"
		)));
	}
	let called = store.execute("RETURN fn::twice(40);", &Object::new())?;
	assert_eq!(called[0].result.as_ref().map_err(Clone::clone)?.value(), &Value::from(42_i64));

	let invalid = store.execute(
		"DEFINE PURE FUNCTION fn::invalid_clock() -> datetime { RETURN time::now(); };",
		&Object::new(),
	)?;
	assert!(invalid[0].result.is_err());

	let mut session = store.session();
	let begun = session
		.execute("BEGIN ISOLATION LEVEL SNAPSHOT EXECUTION MODE DETERMINISTIC;", &Object::new())?;
	assert!(begun[0].result.is_ok());
	let stable = session.execute("RETURN fn::clock();", &Object::new())?;
	assert!(stable[0].result.is_err());
	Ok(())
}

#[test]
fn named_function_recursion_requires_and_enforces_max_depth() -> FerrosGrynnResult<()> {
	let store = Datastore::in_memory();
	let missing_bound = store.execute(
		"DEFINE PURE FUNCTION fn::unbounded(value: int) -> int { RETURN fn::unbounded($value); };",
		&Object::new(),
	)?;
	assert!(missing_bound[0].result.is_err());

	let defined = store.execute(
		"DEFINE PURE FUNCTION fn::sum_down(value: int) -> int MAXDEPTH 8 { IF $value <= 0 { \
		 RETURN 0; }; RETURN $value + fn::sum_down($value - 1); };",
		&Object::new(),
	)?;
	if let Some(error) = defined.iter().find_map(|result| result.result.as_ref().err()) {
		return Err(FerrosGrynnError::invalid_argument(format!(
			"recursive function definition failed: {error}"
		)));
	}
	let called = store.execute("RETURN fn::sum_down(5);", &Object::new())?;
	assert_eq!(called[0].result.as_ref().map_err(Clone::clone)?.value(), &Value::from(15_i64));

	let exceeded = store.execute("RETURN fn::sum_down(9);", &Object::new())?;
	assert!(exceeded[0].result.is_err());

	let mutual = store.execute(
		"DEFINE PURE FUNCTION fn::is_even(value: int) -> bool MAXDEPTH 16 { RETURN true; }; \
		 DEFINE PURE FUNCTION fn::is_odd(value: int) -> bool MAXDEPTH 16 { IF $value <= 0 { \
		 RETURN false; }; RETURN fn::is_even($value - 1); }; ALTER FUNCTION fn::is_even(int) { IF \
		 $value <= 0 { RETURN true; }; RETURN fn::is_odd($value - 1); }; RETURN fn::is_even(6); \
		 RETURN fn::is_odd(6);",
		&Object::new(),
	)?;
	if let Some(error) = mutual.iter().find_map(|result| result.result.as_ref().err()) {
		return Err(FerrosGrynnError::invalid_argument(format!(
			"mutually recursive function failed: {error}"
		)));
	}
	assert_eq!(mutual[3].result.as_ref().map_err(Clone::clone)?.value(), &Value::Bool(true));
	assert_eq!(mutual[4].result.as_ref().map_err(Clone::clone)?.value(), &Value::Bool(false));
	Ok(())
}

#[test]
fn durable_parameters_support_ddl_precedence_and_transaction_rollback() -> FerrosGrynnResult<()> {
	let store = Datastore::in_memory();
	let results = store.execute(
		"DEFINE PARAM $answer VALUE 40 COMMENT 'base'; RETURN $answer + 2; INFO FOR PARAM \
		 $answer; DEFINE PARAM IF NOT EXISTS $answer VALUE 1 / 0; ALTER PARAM $answer VALUE 41 \
		 COMMENT 'changed'; RETURN $answer + 1;",
		&Object::new(),
	)?;
	if let Some(error) = results.iter().find_map(|result| result.result.as_ref().err()) {
		return Err(FerrosGrynnError::invalid_argument(format!(
			"durable parameter program returned a statement error: {error}"
		)));
	}
	if results[1].result.as_ref().map_err(Clone::clone)?.value() != &Value::from(42_i64)
		|| results[5].result.as_ref().map_err(Clone::clone)?.value() != &Value::from(42_i64)
	{
		return Err(FerrosGrynnError::invalid_argument(
			"durable parameter was not visible to the following statement",
		));
	}
	let info =
		results[2].result.as_ref().map_err(Clone::clone)?.value().as_object().ok_or_else(|| {
			FerrosGrynnError::invalid_argument("INFO FOR PARAM returned no object")
		})?;
	if info.get("comment") != Some(&Value::from("base")) {
		return Err(FerrosGrynnError::invalid_argument("parameter comment was not retained"));
	}

	let mut execute = Object::new();
	execute.insert("answer", 7_i64);
	let shadowed = store.execute("RETURN $answer;", &execute)?;
	if shadowed[0].result.as_ref().map_err(Clone::clone)?.value() != &Value::from(7_i64) {
		return Err(FerrosGrynnError::invalid_argument(
			"execute parameter did not shadow the durable parameter",
		));
	}

	let mut transaction = store.begin();
	let changed = transaction.execute("ALTER PARAM $answer VALUE 99;", &Object::new())?;
	if changed[0].result.is_err() {
		return Err(FerrosGrynnError::invalid_argument("transactional ALTER PARAM failed"));
	}
	drop(transaction);
	let after = store.execute("RETURN $answer;", &Object::new())?;
	if after[0].result.as_ref().map_err(Clone::clone)?.value() != &Value::from(41_i64) {
		return Err(FerrosGrynnError::invalid_argument(
			"rolled-back parameter change became visible",
		));
	}
	Ok(())
}

#[test]
fn durable_parameter_overwrite_preserves_identity_and_rejects_runtime_values() {
	let store = Datastore::in_memory();
	let first = store.execute("DEFINE PARAM $value VALUE 1;", &Object::new()).unwrap();
	assert!(first[0].result.is_ok());
	let before = store.schema_catalog().parameter("value").unwrap();
	let overwritten =
		store.execute("DEFINE PARAM OVERWRITE $value VALUE 2;", &Object::new()).unwrap();
	assert!(overwritten[0].result.is_ok());
	let after = store.schema_catalog().parameter("value").unwrap();
	assert_eq!(before.id(), after.id());
	assert!(after.version() > before.version());
	assert_eq!(after.value(), &Value::from(2_i64));

	let runtime = store.execute("DEFINE PARAM $callable VALUE |$x| $x;", &Object::new()).unwrap();
	assert!(runtime[0].result.is_err());
}

#[test]
fn deferred_index_is_stale_inside_mutation_and_never_serves_old_rows() -> FerrosGrynnResult<()> {
	let store = Datastore::in_memory();
	let definitions = store.execute(
		"DEFINE CLASS person TYPE NODE SCHEMAFULL; DEFINE FIELD age ON CLASS person TYPE int; \
		 DEFINE INDEX by_age ON CLASS person FIELDS age SCOPE BASE DEFER;",
		&Object::new(),
	)?;
	if definitions.iter().any(|result| result.result.is_err()) {
		return Err(FerrosGrynnError::invalid_argument(
			"deferred index setup returned a statement error",
		));
	}

	let mut transaction = store.begin();
	let create = transaction.execute("CREATE person:alice SET age = 42;", &Object::new())?;
	if create.first().is_none_or(|result| result.result.is_err()) {
		return Err(FerrosGrynnError::invalid_argument("deferred-index mutation failed"));
	}
	let scan =
		transaction.execute("SELECT VALUE id FROM person WHERE age = 42;", &Object::new())?;
	let value = scan
		.first()
		.and_then(|result| result.result.as_ref().ok())
		.map(crate::gql_status::Outcome::value)
		.ok_or_else(|| FerrosGrynnError::invalid_argument("scan fallback failed"))?;
	if !matches!(value, Value::Array(values) if values.len() == 1) {
		return Err(FerrosGrynnError::invalid_argument(
			"scan fallback did not return the uncommitted row",
		));
	}
	let forced = transaction
		.execute("SELECT VALUE id FROM person WITH INDEX by_age WHERE age = 42;", &Object::new())?;
	let forced_error = forced
		.first()
		.and_then(|result| result.result.as_ref().err())
		.ok_or_else(|| FerrosGrynnError::invalid_argument("a stale forced index was accepted"))?;
	if !forced_error.to_string().contains("not Ready") {
		return Err(FerrosGrynnError::invalid_argument(
			"forced stale index returned an unexpected diagnostic",
		));
	}
	Ok(())
}

#[test]
fn info_for_index_exposes_deferred_query_visibility() -> FerrosGrynnResult<()> {
	let store = Datastore::in_memory();
	let results = store.execute(
		"DEFINE CLASS person TYPE NODE SCHEMAFULL; DEFINE FIELD age ON CLASS person TYPE int; \
		 DEFINE INDEX by_age ON CLASS person FIELDS age SCOPE BASE DEFER; INFO FOR INDEX by_age \
		 ON CLASS person;",
		&Object::new(),
	)?;
	let value = results
		.last()
		.ok_or_else(|| FerrosGrynnError::invalid_argument("INFO FOR INDEX returned no result"))?
		.result
		.as_ref()
		.map_err(Clone::clone)?
		.value();
	let Value::Object(info) = value else {
		return Err(FerrosGrynnError::invalid_argument("INFO FOR INDEX did not return an object"));
	};
	if info.get("maintenance") != Some(&Value::from("deferred"))
		|| info.get("query_visible") != Some(&Value::Bool(true))
		|| info.get("fallback_reason") != Some(&Value::None)
		|| !matches!(info.get("build_revision"), Some(Value::Number(_)))
	{
		return Err(FerrosGrynnError::invalid_argument(
			"INFO FOR INDEX omitted deferred maintenance fields",
		));
	}
	Ok(())
}

#[test]
fn class_field_index_and_info_share_one_transaction_visible_catalog() {
	let store = Datastore::in_memory();
	let mut transaction = store.begin();
	let definitions = transaction
		.execute("DEFINE GRAPH social; DEFINE CLASS person TYPE NODE SCHEMAFULL;", &Object::new())
		.unwrap();
	assert!(definitions.iter().all(|result| result.result.is_ok()));
	let results = transaction
		.execute(
			"DEFINE FIELD email ON CLASS person TYPE string; DEFINE INDEX by_email ON CLASS \
			 person FIELDS email UNIQUE SCOPE EFFECTIVE IN GRAPH social; INFO FOR CLASS person;",
			&Object::new(),
		)
		.unwrap();
	assert_eq!(results.len(), 3);
	assert!(results.iter().all(|result| result.result.is_ok()));
	let info = results[2].result.as_ref().unwrap().value().as_object().unwrap();
	assert!(info.get("definition").is_some());
	assert!(info.get("indexes").and_then(Value::as_object).unwrap().contains_key("by_email"));
	transaction.commit().unwrap();

	let class = store.class("person").unwrap();
	assert_eq!(class.kind(), AtomKind::Node);
	assert_eq!(class.schema_mode(), ClassSchemaMode::Schemafull);
	assert_eq!(class.fields().next().unwrap().value_type(), &Type::String);
	let index = class.indexes().next().unwrap();
	assert_eq!(index.kind(), ClassIndexKind::Unique);
	assert!(matches!(index.scope(), IndexScope::Effective { .. }));
}

#[test]
fn overwrite_cannot_change_class_kind() {
	let store = Datastore::in_memory();
	let mut transaction = store.begin();
	transaction.execute("DEFINE CLASS person TYPE NODE;", &Object::new()).unwrap();
	let results =
		transaction.execute("DEFINE CLASS OVERWRITE person TYPE EDGE;", &Object::new()).unwrap();
	assert!(results[0].result.is_err());
}

#[test]
fn graph_properties_are_transactional_and_use_nested_paths() {
	let store = Datastore::in_memory();
	let defined = store
		.execute(
			"DEFINE GRAPH sales TYPE DAG CONTENT {owner: 'analytics', region: 'eu', address: \
			 {city: 'Paris'}};",
			&Object::new(),
		)
		.unwrap();
	assert!(defined[0].result.is_ok());
	let updated = store
		.execute(
			"UPDATE GRAPH sales SET properties.owner = 'risk', properties.address.city = 'Berlin' \
			 RETURN AFTER; UPDATE GRAPH sales UNSET properties.region RETURN DIFF;",
			&Object::new(),
		)
		.unwrap();
	assert!(updated.iter().all(|result| result.result.is_ok()));
	let after = updated[0].result.as_ref().unwrap().value().as_object().unwrap();
	let properties = after.get("properties").and_then(Value::as_object).unwrap();
	assert_eq!(properties.get("owner"), Some(&Value::from("risk")));
	assert_eq!(
		properties
			.get("address")
			.and_then(Value::as_object)
			.and_then(|address| address.get("city")),
		Some(&Value::from("Berlin")),
	);
	let schema = store.schema_catalog().named_graph("sales").unwrap();
	let dictionary = store.property_dictionary();
	assert_eq!(
		schema.properties().get(dictionary.get("owner").unwrap()),
		Some(&Value::from("risk")),
	);
	assert!(!schema.properties().contains_key(dictionary.get("region").unwrap()));

	let mut rolled_back = store.begin();
	let result = rolled_back
		.execute("UPDATE GRAPH sales SET properties.owner = 'temporary';", &Object::new())
		.unwrap();
	assert!(result[0].result.is_ok());
	drop(rolled_back);
	let schema = store.schema_catalog().named_graph("sales").unwrap();
	assert_eq!(
		schema.properties().get(dictionary.get("owner").unwrap()),
		Some(&Value::from("risk")),
	);
}

#[test]
fn concurrent_graph_property_writers_conflict() {
	let store = Datastore::in_memory();
	store.execute("DEFINE GRAPH sales CONTENT {owner: 'first'};", &Object::new()).unwrap();
	let mut first = store.begin();
	let mut second = store.begin();
	assert!(
		first.execute("UPDATE GRAPH sales SET properties.owner = 'one';", &Object::new()).unwrap()
			[0]
		.result
		.is_ok()
	);
	assert!(
		second.execute("UPDATE GRAPH sales SET properties.owner = 'two';", &Object::new()).unwrap()
			[0]
		.result
		.is_ok()
	);
	first.commit().unwrap();
	assert!(second.commit().is_err());
}
