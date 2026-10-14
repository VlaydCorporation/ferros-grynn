use ferros_grynn::{
	gql::{ParserSettings, parse},
	gql_status::ExecutionPhase,
	values::{Object, Value},
};

use crate::support::{create_person, execute, people, record, value};

#[test]
fn parameterized_dml_and_ordered_pagination_return_owned_record_ids() {
	let db = database();
	people(&db);
	for (id, name, age) in [("person:a", "A", 10), ("person:b", "B", 20), ("person:c", "C", 30)] {
		create_person(&db, id, name, age);
	}
	let parameters = Object::from_iter([("minimum", Value::from(15_i64))]);
	let query = "SELECT VALUE id FROM person WHERE age >= $minimum ORDER BY age DESC LIMIT 1;";
	let expected = Value::Array(vec![Value::from(record("person:c"))].into());
	assert_eq!(value(&db, query, &parameters), expected);
	execute(&db, "UPDATE person:b SET age = $minimum;", &parameters);
	db.assert_committed(|store| {
		assert_eq!(value(store, query, &parameters), expected);
		assert_eq!(
			value(store, "SELECT VALUE age FROM person:b;", &Object::new()),
			Value::Array(vec![Value::from(15_i64)].into())
		);
	});
}

#[test]
fn gql_session_keeps_private_writes_until_commit_and_cancel_rolls_them_back() {
	let db = database();
	people(&db);
	{
		let mut session = db.session();
		for text in ["BEGIN;", "CREATE person:cancelled SET name = 'Private', age = 1;"] {
			for result in session.execute(text, &Object::new()).unwrap() {
				assert!(result.result.is_ok(), "{text}: {:?}", result.result);
			}
		}
		assert!(db.begin().atom(&record("person:cancelled")).unwrap().is_none());
		let private = session.execute("SELECT VALUE id FROM person;", &Object::new()).unwrap();
		assert_eq!(private[0].result.as_ref().unwrap().value().as_array().unwrap().len(), 1);
		assert!(session.execute("CANCEL;", &Object::new()).unwrap()[0].result.is_ok());
		for text in ["BEGIN;", "CREATE person:committed SET name = 'Public', age = 2;", "COMMIT;"] {
			for result in session.execute(text, &Object::new()).unwrap() {
				assert!(result.result.is_ok(), "{text}: {:?}", result.result);
			}
		}
	}
	db.assert_committed(|store| {
		let mut read = store.begin();
		assert!(read.atom(&record("person:cancelled")).unwrap().is_none());
		assert!(read.atom(&record("person:committed")).unwrap().is_some());
	});
}

#[test]
fn sdk_query_results_own_parameters_after_the_database_and_input_are_dropped() {
	let db = database();
	let payload = Value::Object(Object::from_iter([
		("text", Value::from("данные 'quoted' 💾")),
		("items", Value::Array(vec![Value::Null, Value::from(42_i64)].into())),
	]));
	let parameters = Object::from_iter([("payload", payload.clone())]);
	let result = value(&db, "RETURN $payload;", &parameters);
	drop(parameters);
	drop(db);
	assert_eq!(result, payload);
}

#[test]
fn parser_reports_source_span_and_sdk_statement_errors_are_typed() {
	let error = parse("RETURN (", ParserSettings::default()).unwrap_err();
	assert_eq!(error.status().code().to_string(), "FG-42001");
	assert_eq!(error.status().diagnostic().phase(), Some(ExecutionPhase::Parsing));
	assert!(error.status().diagnostic().primary_location().is_some());
	let db = database();
	let results = db.execute("SELECT * FROM missing;", &Object::new()).unwrap();
	let error = results[0].result.as_ref().unwrap_err();
	assert_eq!(error.status().code().to_string(), "FG-42F01");
	assert_eq!(value(&db, "RETURN 7;", &Object::new()), Value::from(7_i64));
}
