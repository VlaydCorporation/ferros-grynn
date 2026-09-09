use std::{collections::BTreeSet, error::Error, io};

use super::*;
use crate::{
	datastore::{
		Datastore,
		GraphWriteView,
		IsolationLevel,
		TableField,
		TableKind,
		TableSchema,
		TableSchemaMode,
		TransactionAccess,
		TransactionOptions,
	},
	gql::ir::{Column, Schema},
	gql_status::ExecutionPhase,
	graph::{EdgeTopology, Graph, Mutable},
	values::{PropertyMap, RecordIdPart},
};

struct TestStore {
	schema: Schema,
	rows: Vec<(RecordId, Row)>,
	fail_scan: bool,
}

impl TestStore {
	fn people() -> Self {
		Self {
			schema: Schema {
				columns: vec![
					Column {
						name: "name".into(),
						ty: Type::String,
					},
					Column {
						name: "age".into(),
						ty: Type::Int,
					},
				],
			},
			rows: vec![
				person("alice", "Alice", 30),
				person("bob", "Bob", 20),
				person("carol", "Carol", 17),
			],
			fail_scan: false,
		}
	}
}

impl QueryCatalog for TestStore {
	fn schema(&self, table: &str) -> FerrosGrynnResult<Schema> {
		if table.eq_ignore_ascii_case("person") {
			Ok(self.schema.clone())
		} else {
			Err(FerrosGrynnError::invalid_argument(format!("Unknown table `{table}`.")))
		}
	}

	fn schema_mode(&self, _table: &str) -> FerrosGrynnResult<crate::datastore::TableSchemaMode> {
		Ok(crate::datastore::TableSchemaMode::Schemaless)
	}

	fn graph(&self, name: &str) -> FerrosGrynnResult<GraphRef> {
		Err(FerrosGrynnError::graph_does_not_exist(name))
	}
}

impl QueryRead for TestStore {
	type Graph<'a> = crate::values::MaterializedGraph;

	fn snapshot(&self) -> FerrosGrynnResult<Snapshot> {
		Ok(Snapshot(7))
	}

	fn catalog(&self, snapshot: Snapshot) -> &dyn QueryCatalog {
		assert_eq!(snapshot, Snapshot(7));
		self
	}

	fn scan<'a>(
		&'a self,
		snapshot: Snapshot,
		table: &str,
		_predicate: Option<&TypedExpr>,
		_slots: &[Value],
		_source: &str,
	) -> FerrosGrynnResult<Box<dyn RowCursor + 'a>> {
		assert_eq!(snapshot, Snapshot(7));
		assert_eq!(table, "person");
		if self.fail_scan {
			return Err(FerrosGrynnError::unexpected_external_failure("test-store", "scan")
				.with_technical_source(io::Error::other("secret storage detail")));
		}
		Ok(Box::new(VecCursor::new(self.rows.iter().map(|(_, row)| row.clone()).collect())))
	}

	fn get(&self, snapshot: Snapshot, record: &RecordId) -> FerrosGrynnResult<Option<Row>> {
		assert_eq!(snapshot, Snapshot(7));
		Ok(self.rows.iter().find(|(candidate, _)| candidate == record).map(|(_, row)| row.clone()))
	}

	fn graph<'a>(
		&'a self,
		_snapshot: Snapshot,
		reference: &GraphRef,
	) -> FerrosGrynnResult<Self::Graph<'a>> {
		Err(FerrosGrynnError::graph_does_not_exist(reference.id().to_string()))
	}

	fn property_dictionary(&self, _snapshot: Snapshot) -> Arc<PropertyDictionary> {
		Arc::new(PropertyDictionary::new())
	}

	fn execute_schema(&mut self, _statement: &LogicalSchemaStatement) -> FerrosGrynnResult<Value> {
		Err(FerrosGrynnError::query_feature_not_supported("test schema mutation"))
	}

	fn execute_dml(
		&mut self,
		_statement: &LogicalDmlStatement,
		_slots: &[Value],
		_source: &str,
	) -> FerrosGrynnResult<Value> {
		Err(FerrosGrynnError::query_feature_not_supported("test data mutation"))
	}
}

fn person(id: &str, name: &str, age: i64) -> (RecordId, Row) {
	(
		RecordId::new("person", RecordIdPart::String(id.into())).expect("valid record ID"),
		Row::new(vec![Value::from(name), Value::from(age)]),
	)
}

fn match_store() -> Datastore {
	let store = Datastore::in_memory();
	let name = store.intern_property("name").expect("property key");
	let mut graph = Graph::<Mutable>::new();
	let mut alice_properties = PropertyMap::new();
	alice_properties.insert(name, "Alice");
	let alice = graph
		.add_node_with_id(
			"person:alice".parse().expect("record"),
			Some("Person".into()),
			alice_properties,
		)
		.expect("alice");
	let mut bob_properties = PropertyMap::new();
	bob_properties.insert(name, "Bob");
	let bob = graph
		.add_node_with_id(
			"person:bob".parse().expect("record"),
			Some("Person".into()),
			bob_properties,
		)
		.expect("bob");
	let carol = graph
		.add_node_with_id(
			"person:carol".parse().expect("record"),
			Some("Person".into()),
			PropertyMap::new(),
		)
		.expect("carol");
	let topic = graph
		.add_node_with_id(
			"topic:rust".parse().expect("record"),
			Some("Topic".into()),
			PropertyMap::new(),
		)
		.expect("topic");
	let first = graph
		.add_edge_with_id(
			"knows:first".parse().expect("record"),
			EdgeTopology::directed([alice], [bob]),
			Some("KNOWS".into()),
			PropertyMap::new(),
		)
		.expect("first edge");
	let second = graph
		.add_edge_with_id(
			"knows:second".parse().expect("record"),
			EdgeTopology::directed([bob], [carol]),
			Some("KNOWS".into()),
			PropertyMap::new(),
		)
		.expect("second edge");
	graph
		.add_edge_with_id(
			"annotation:first".parse().expect("record"),
			EdgeTopology::directed([alice], [first]),
			Some("ANNOTATES".into()),
			PropertyMap::new(),
		)
		.expect("V-E edge");
	graph
		.add_edge_with_id(
			"cause:first".parse().expect("record"),
			EdgeTopology::directed([first], [second]),
			Some("CAUSES".into()),
			PropertyMap::new(),
		)
		.expect("E-E edge");
	graph
		.add_edge_with_id(
			"group:team".parse().expect("record"),
			EdgeTopology::directed([alice, bob], [carol, topic]),
			Some("GROUP".into()),
			PropertyMap::new(),
		)
		.expect("hyperedge");
	let mut create = store.begin();
	create
		.create_named_graph(
			"social",
			"graph:internal".parse().expect("graph record"),
			graph.freeze().into(),
		)
		.expect("named graph");
	create.commit().expect("commit");
	store
}

fn dml_store() -> Datastore {
	let store = Datastore::in_memory();
	let mut create = store.begin();
	let reference = create
		.create_named_graph(
			"records",
			"graph:records".parse().expect("graph record"),
			Graph::<Mutable>::new().freeze().into(),
		)
		.expect("named graph");
	create
		.define_table(
			TableSchema::new("person", reference, crate::datastore::TableKind::Normal, [])
				.expect("table schema"),
		)
		.expect("table");
	create.commit().expect("commit");
	store
}

fn run(mut store: TestStore, source: &str) -> FerrosGrynnResult<Vec<QueryResult>> {
	execute_text(&mut store, source, &Object::new(), ParserSettings::default())
}

#[test]
fn executes_return_with_pratt_precedence() {
	let results = run(TestStore::people(), "RETURN 1 + 2 * 3").expect("query succeeds");
	assert_eq!(
		results[0].result.as_ref().expect("statement succeeds").value(),
		&Value::from(7_i64)
	);
}

#[test]
fn executes_graph_backed_dml_with_preimage_expect_and_returns() {
	let store = dml_store();
	let created = store
		.execute(
			"CREATE person:alice CONTENT {name: 'Alice', age: 30} RETURN *; CREATE person:bob \
			 CONTENT {name: 'Bob', age: 17} RETURN VALUE name",
			&Object::new(),
		)
		.expect("CREATE executes");
	assert_eq!(created.len(), 2);
	let Value::Array(alice) = created[0].result.as_ref().expect("Alice created").value() else {
		panic!("expected created rows");
	};
	assert_eq!(
		alice.get(0).and_then(Value::as_object).and_then(|row| row.get("age")),
		Some(&Value::from(30_i64))
	);
	let Value::Array(bob) = created[1].result.as_ref().expect("Bob created").value() else {
		panic!("expected created values");
	};
	assert_eq!(bob.as_slice(), &[Value::from("Bob")]);

	let failed = store
		.execute(
			"UPDATE person:alice, person:bob SET active = true EXPECT age >= 18",
			&Object::new(),
		)
		.expect("statement failure is returned in-band");
	assert_eq!(
		failed[0].result.as_ref().expect_err("Bob fails EXPECT").status().code().to_string(),
		"FG-40F01"
	);
	let unchanged = store
		.execute("UPDATE person:alice RETURN *", &Object::new())
		.expect("record remains readable");
	let Value::Array(rows) = unchanged[0].result.as_ref().expect("readback succeeds").value()
	else {
		panic!("expected readback rows");
	};
	assert!(!rows.get(0).and_then(Value::as_object).expect("object").contains_key("active"));

	let updated = store
		.execute(
			"UPSERT person:alice SET age = 31 RETURN DIFF EXPECT age = 30; UPSERT person:carol \
			 CONTENT {name: 'Carol', age: 22} RETURN AFTER EXPECT NOT EXISTS; DELETE person:bob \
			 RETURN BEFORE EXPECT EXISTS",
			&Object::new(),
		)
		.expect("UPSERT and DELETE execute");
	assert!(updated.iter().all(|result| result.result.is_ok()));
	let Value::Array(diff) = updated[0].result.as_ref().expect("diff").value() else {
		panic!("expected diff rows");
	};
	assert!(diff.get(0).and_then(Value::as_object).expect("diff object").contains_key("age"));
	let Value::Array(deleted) = updated[2].result.as_ref().expect("before image").value() else {
		panic!("expected deleted rows");
	};
	assert_eq!(
		deleted.get(0).and_then(Value::as_object).and_then(|row| row.get("name")),
		Some(&Value::from("Bob"))
	);
}

#[test]
fn rejects_duplicate_dml_targets_before_staging_a_record() {
	let store = dml_store();
	let mut transaction = store.begin();
	let results = transaction
		.execute("CREATE person:duplicate, person:duplicate SET name = 'partial'", &Object::new())
		.expect("statement failure is returned in-band");
	assert!(results[0].result.is_err());
	transaction.commit().expect("failed statement staged no record");

	let readback = store
		.execute("UPDATE person:duplicate RETURN *", &Object::new())
		.expect("missing record query succeeds");
	assert_eq!(
		readback[0].result.as_ref().expect("readback result").value(),
		&Value::Array(Array::new(Vec::new()))
	);
}

#[test]
fn executes_dml_generators_modes_filters_and_return_contracts() {
	let store = dml_store();
	let generated = store
		.execute("CREATE |person:3| SET stage = 'new' RETURN VALUE id", &Object::new())
		.expect("generated CREATE executes");
	let Value::Array(ids) = generated[0].result.as_ref().expect("generated rows").value() else {
		panic!("expected generated record IDs");
	};
	assert_eq!(ids.len(), 3);
	let mut unique = BTreeSet::new();
	for value in ids {
		let Value::Record(record) = value else {
			panic!("generated ID must remain a RecordId");
		};
		assert!(matches!(record.id(), RecordIdPart::Ulid(_)));
		assert!(unique.insert(record.to_string()));
	}

	let results = store
		.execute(
			"CREATE person:item CONTENT {a: 1, b: 2} RETURN NONE; UPDATE person:item MERGE {b: 3, \
			 c: 4} RETURN BEFORE; UPDATE person:item PATCH {a: none, d: 5} RETURN DIFF; UPDATE \
			 person:item REPLACE {only: 9} RETURN AFTER; UPDATE person:item SET only = 10, extra \
			 = 'x' RETURN only, extra; UPDATE person:item UNSET extra RETURN VALUE only; UPDATE \
			 person:item SET filtered = true WHERE false RETURN AFTER; DELETE person:item WHERE \
			 false RETURN BEFORE; DELETE person:item RETURN DIFF",
			&Object::new(),
		)
		.expect("all supported DML modes execute");
	assert!(results.iter().all(|result| result.result.is_ok()));
	assert_eq!(results[0].result.as_ref().expect("RETURN NONE").value(), &Value::None);

	let before = results[1]
		.result
		.as_ref()
		.expect("RETURN BEFORE")
		.value()
		.as_array()
		.and_then(|rows| rows.get(0))
		.and_then(Value::as_object)
		.expect("before object");
	assert_eq!(before.get("b"), Some(&Value::from(2_i64)));
	assert!(!before.contains_key("c"));

	let diff = results[2]
		.result
		.as_ref()
		.expect("RETURN DIFF")
		.value()
		.as_array()
		.and_then(|rows| rows.get(0))
		.and_then(Value::as_object)
		.expect("diff object");
	assert_eq!(
		diff.keys().map(|name| name.as_str()).collect::<BTreeSet<_>>(),
		BTreeSet::from(["a", "d"])
	);

	let replaced = results[3]
		.result
		.as_ref()
		.expect("RETURN AFTER")
		.value()
		.as_array()
		.and_then(|rows| rows.get(0))
		.and_then(Value::as_object)
		.expect("replacement object");
	assert_eq!(replaced.get("only"), Some(&Value::from(9_i64)));
	assert!(!replaced.contains_key("b"));

	let projected = results[4]
		.result
		.as_ref()
		.expect("field projection")
		.value()
		.as_array()
		.and_then(|rows| rows.get(0))
		.and_then(Value::as_object)
		.expect("projected object");
	assert_eq!(projected.get("only"), Some(&Value::from(10_i64)));
	assert_eq!(projected.get("extra"), Some(&Value::from("x")));
	assert_eq!(
		results[5].result.as_ref().expect("RETURN VALUE").value(),
		&Value::Array(Array::new(vec![Value::from(10_i64)]))
	);
	for result in &results[6..8] {
		assert_eq!(
			result.result.as_ref().expect("filtered DML").value(),
			&Value::Array(Array::new(Vec::new()))
		);
	}
	let delete_diff = results[8]
		.result
		.as_ref()
		.expect("delete diff")
		.value()
		.as_array()
		.and_then(|rows| rows.get(0))
		.and_then(Value::as_object)
		.expect("delete diff object");
	assert!(delete_diff.contains_key("id"));
	assert!(delete_diff.contains_key("only"));
}

#[test]
fn schemafull_dml_rejects_unknown_and_mistyped_fields_before_mutation() {
	let store = Datastore::in_memory();
	let mut create = store.begin();
	let graph = create
		.create_named_graph(
			"typed-records",
			"graph:typed-records".parse().expect("graph record"),
			Graph::<Mutable>::new().freeze().into(),
		)
		.expect("named graph");
	create
		.define_table(
			TableSchema::new("person", graph, TableKind::Normal, [TableField::new(
				"name",
				Type::String,
			)])
			.expect("table schema")
			.with_schema_mode(TableSchemaMode::Schemafull),
		)
		.expect("table");
	create.commit().expect("schema commit");

	for source in ["CREATE person:alice SET unknown = 1", "CREATE person:alice SET name = 1"] {
		let error = store.execute(source, &Object::new()).expect_err("analysis must reject DML");
		assert_eq!(error.status().diagnostic().phase(), Some(ExecutionPhase::SemanticAnalysis));
	}
	let readback = store
		.execute("UPDATE person:alice RETURN *", &Object::new())
		.expect("missing record lookup succeeds");
	assert_eq!(
		readback[0].result.as_ref().expect("readback").value(),
		&Value::Array(Array::new(Vec::new()))
	);
}

#[test]
fn binds_let_filters_projects_and_limits() {
	let results = run(
		TestStore::people(),
		"LET $min = 18; SELECT name FROM person WHERE age >= $min LIMIT 2",
	)
	.expect("query succeeds");
	assert_eq!(results[0].result.as_ref().expect("LET succeeds").value(), &Value::None);
	let Value::Array(rows) = results[1].result.as_ref().expect("SELECT succeeds").value() else {
		panic!("expected an array result");
	};
	assert_eq!(rows.len(), 2);
	assert_eq!(
		rows.get(0).and_then(Value::as_object).and_then(|row| row.get("name")),
		Some(&Value::from("Alice"))
	);
	assert_eq!(
		rows.get(1).and_then(Value::as_object).and_then(|row| row.get("name")),
		Some(&Value::from("Bob"))
	);
}

#[test]
fn gets_record_and_materializes_select_value() {
	let results =
		run(TestStore::people(), "SELECT VALUE name FROM person:alice").expect("query succeeds");
	let Value::Array(values) = results[0].result.as_ref().expect("SELECT succeeds").value() else {
		panic!("expected an array result");
	};
	assert_eq!(values.as_slice(), &[Value::from("Alice")]);
}

#[test]
fn rejects_unknown_field_during_analysis() {
	let error = run(TestStore::people(), "SELECT missing FROM person")
		.expect_err("unknown field must fail before execution");
	assert_eq!(error.status().diagnostic().phase(), Some(ExecutionPhase::SemanticAnalysis));
}

#[test]
fn keeps_none_and_null_distinct() {
	let results = run(
		TestStore::people(),
		"RETURN none = null; RETURN [none, null]; SELECT VALUE name FROM person WHERE none = \
		 null; SELECT VALUE name FROM person WHERE none IS none LIMIT 1",
	)
	.expect("query succeeds");
	assert_eq!(
		results[0].result.as_ref().expect("comparison succeeds").value(),
		&Value::Bool(false)
	);
	let Value::Array(values) = results[1].result.as_ref().expect("array succeeds").value() else {
		panic!("expected array");
	};
	assert_eq!(values.as_slice(), &[Value::None, Value::Null]);
	let Value::Array(filtered_out) =
		results[2].result.as_ref().expect("false filter succeeds").value()
	else {
		panic!("expected array");
	};
	assert!(filtered_out.is_empty());
	let Value::Array(filtered_in) =
		results[3].result.as_ref().expect("true filter succeeds").value()
	else {
		panic!("expected array");
	};
	assert_eq!(filtered_in.as_slice(), &[Value::from("Alice")]);
}

#[test]
fn preserves_domain_error_and_redacts_storage_source() {
	let mut store = TestStore::people();
	store.fail_scan = true;
	let results = run(store, "SELECT name FROM person").expect("planning succeeds");
	let error = results[0].result.as_ref().expect_err("scan fails");
	assert_eq!(error.status().code().to_string(), "FG-50F00");
	assert!(error.technical_source().is_some());
	assert!(!format!("{error:?}").contains("secret storage detail"));
	assert!(Error::source(error).is_none());
}

#[test]
fn executes_named_graph_standard_match_with_fields() {
	let store = Datastore::in_memory();
	let name = store.intern_property("name").expect("property key");
	let mut graph = Graph::<Mutable>::new();
	let mut alice_properties = PropertyMap::new();
	alice_properties.insert(name, "Alice");
	let alice = graph
		.add_node_with_id(
			"person:alice".parse().expect("record"),
			Some("Person".into()),
			alice_properties,
		)
		.expect("alice");
	let bob = graph
		.add_node_with_id(
			"person:bob".parse().expect("record"),
			Some("Person".into()),
			PropertyMap::new(),
		)
		.expect("bob");
	graph
		.add_edge_with_id(
			"knows:alice-bob".parse().expect("record"),
			EdgeTopology::directed([alice], [bob]),
			Some("KNOWS".into()),
			PropertyMap::new(),
		)
		.expect("edge");
	let mut create = store.begin();
	create
		.create_named_graph(
			"social",
			"graph:internal".parse().expect("graph record"),
			graph.freeze().into(),
		)
		.expect("named graph");
	create.commit().expect("commit");

	let results = store
		.execute(
			"MATCH (a:Person)-[e:KNOWS]->(b:Person) FROM social WHERE a.name = 'Alice' RETURN \
			 a.id AS source, b.id AS target, e.label AS kind, b.name AS missing",
			&Object::new(),
		)
		.expect("MATCH executes");
	let Value::Array(rows) = results[0].result.as_ref().expect("MATCH succeeds").value() else {
		panic!("expected rows");
	};
	assert_eq!(rows.len(), 1);
	let row = rows.get(0).and_then(Value::as_object).expect("object row");
	assert_eq!(
		row.get("source"),
		Some(&Value::Record(Box::new("person:alice".parse().expect("record"))))
	);
	assert_eq!(row.get("kind"), Some(&Value::from("KNOWS")));
	assert_eq!(row.get("missing"), Some(&Value::None));
}

#[test]
fn executes_atomwalk_hyper_pairs_shared_alias_and_cartesian_patterns() {
	let store = match_store();
	let atomwalk = store
		.execute(
			"MATCH ATOM <(v:Person)>~[link:ANNOTATES]~><[edge:KNOWS]> FROM social RETURN v.id AS \
			 v, edge.id AS edge; MATCH ATOM <[left:KNOWS]>~[cause:CAUSES]~><[right:KNOWS]> FROM \
			 social RETURN left.id AS left, right.id AS right",
			&Object::new(),
		)
		.expect("AtomWalk executes");
	for result in atomwalk {
		let Value::Array(rows) = result.result.expect("MATCH succeeds").into_parts().0 else {
			panic!("expected rows");
		};
		assert_eq!(rows.len(), 1);
	}

	let joined = store
		.execute(
			"MATCH (a:Person)-[:KNOWS]->(b:Person), (b)-[:KNOWS]->(c:Person) FROM social RETURN \
			 a.id AS a, c.id AS c",
			&Object::new(),
		)
		.expect("joined patterns execute");
	let Value::Array(rows) = joined[0].result.as_ref().expect("MATCH succeeds").value() else {
		panic!("expected rows");
	};
	assert_eq!(rows.len(), 1);

	let cartesian = store
		.execute(
			"MATCH (topic:Topic), (person:Person) FROM social RETURN topic.id, person.id",
			&Object::new(),
		)
		.expect("cartesian patterns execute");
	let Value::Array(rows) = cartesian[0].result.as_ref().expect("MATCH succeeds").value() else {
		panic!("expected rows");
	};
	assert_eq!(rows.len(), 3);

	let hyper = store
		.execute(
			"MATCH (source:Person)-[:GROUP]->(target) FROM social RETURN source.id, target.id",
			&Object::new(),
		)
		.expect("hyperedge pair expansion executes");
	let Value::Array(rows) = hyper[0].result.as_ref().expect("MATCH succeeds").value() else {
		panic!("expected rows");
	};
	assert_eq!(rows.len(), 4);

	let distinct_connectors = store
		.execute(
			"MATCH (a:Person)-[:KNOWS]->(b:Person), (a)-[:KNOWS]->(b) FROM social RETURN a.id, \
			 b.id",
			&Object::new(),
		)
		.expect("edge-isomorphic patterns execute");
	let Value::Array(rows) =
		distinct_connectors[0].result.as_ref().expect("MATCH succeeds").value()
	else {
		panic!("expected rows");
	};
	assert!(rows.is_empty());
}

#[test]
fn expands_finite_trail_paths_and_returns_public_path_values() {
	let store = match_store();
	let results = store
		.execute(
			"MATCH path = (a:Person)-[:KNOWS*0..2]->(b:Person) FROM social WHERE a.name = 'Alice' \
			 RETURN path AS path, b.id AS target",
			&Object::new(),
		)
		.expect("path MATCH executes");
	let Value::Array(rows) = results[0].result.as_ref().expect("MATCH succeeds").value() else {
		panic!("expected rows");
	};
	assert_eq!(rows.len(), 3);
	let lengths = rows
		.iter()
		.filter_map(Value::as_object)
		.filter_map(|row| row.get("path"))
		.map(|value| match value {
			Value::Path(path) => path.length(),
			_ => panic!("expected public path"),
		})
		.collect::<BTreeSet<_>>();
	assert_eq!(lengths, BTreeSet::from([0, 1, 2]));

	let backward = store
		.execute(
			"MATCH path = (c:Person)<-[:KNOWS*2]-(a:Person) FROM social RETURN a.id AS start, path",
			&Object::new(),
		)
		.expect("backward path executes");
	let Value::Array(rows) = backward[0].result.as_ref().expect("MATCH succeeds").value() else {
		panic!("expected rows");
	};
	assert_eq!(rows.len(), 1);

	let mut loop_graph = Graph::<Mutable>::new();
	let node = loop_graph
		.add_node_with_id(
			"node:loop".parse().expect("record"),
			Some("Loop".into()),
			PropertyMap::new(),
		)
		.expect("loop node");
	loop_graph
		.add_edge_with_id(
			"edge:loop".parse().expect("record"),
			EdgeTopology::directed([node], [node]),
			Some("LOOP".into()),
			PropertyMap::new(),
		)
		.expect("loop edge");
	let mut create = store.begin();
	create
		.create_named_graph(
			"loops",
			"graph:loops".parse().expect("record"),
			loop_graph.freeze().into(),
		)
		.expect("loop graph");
	create.commit().expect("commit");
	let trail = store
		.execute(
			"MATCH path = (a:Loop)-[:LOOP*0..2]->(b:Loop) FROM loops RETURN path",
			&Object::new(),
		)
		.expect("TRAIL path executes");
	let Value::Array(rows) = trail[0].result.as_ref().expect("MATCH succeeds").value() else {
		panic!("expected rows");
	};
	assert_eq!(rows.len(), 2, "the same connector edge cannot be reused");
}

#[test]
fn rejects_match_semantic_errors_before_execution() {
	let store = match_store();
	for source in [
		"MATCH (a) FROM missing RETURN a",
		"MATCH (a) FROM Social RETURN a",
		"MATCH ATOM <(same)>, <[same]> FROM social RETURN same",
		"MATCH (a) FROM social RETURN unknown",
		"MATCH (a)-[:E*3..2]->(b) FROM social RETURN b",
		"MATCH (a)-[:E*1..31]->(b) FROM social RETURN b",
	] {
		let error = store.execute(source, &Object::new()).expect_err("semantic error expected");
		assert_eq!(
			error.status().diagnostic().phase(),
			Some(ExecutionPhase::SemanticAnalysis),
			"source: {source}"
		);
		assert!(error.status().diagnostic().primary_location().is_some());
	}
}

#[test]
fn match_observes_transaction_private_graphs_and_one_read_committed_snapshot() {
	let store = Datastore::in_memory();
	let mut graph = Graph::<Mutable>::new();
	graph
		.add_node_with_id(
			"person:private".parse().expect("record"),
			Some("Person".into()),
			PropertyMap::new(),
		)
		.expect("node");
	let mut transaction = store.begin();
	let reference = transaction
		.create_named_graph(
			"private",
			"graph:private".parse().expect("record"),
			graph.freeze().into(),
		)
		.expect("private graph");
	let visible = transaction
		.execute("MATCH (n:Person) FROM private RETURN n.id", &Object::new())
		.expect("private graph is visible");
	let Value::Array(rows) = visible[0].result.as_ref().expect("MATCH succeeds").value() else {
		panic!("expected rows");
	};
	assert_eq!(rows.len(), 1);
	transaction.remove_graph(&reference).expect("remove private graph");
	assert!(transaction.execute("MATCH (n) FROM private RETURN n", &Object::new()).is_err());

	let store = match_store();
	let mut read = store.begin_with(TransactionOptions {
		isolation: IsolationLevel::ReadCommitted,
		access: TransactionAccess::ReadOnly,
	});
	let snapshot = read.query_snapshot().expect("statement snapshot");
	let reference = read.query_named_graph(&snapshot, "social").expect("graph ref");
	let mut adapter = TransactionRead {
		transaction: &mut read,
		snapshot,
	};
	let mut concurrent = store.begin();
	if let GraphWriteView::Graph(graph) =
		concurrent.edit_graph_materialized(&reference).expect("edit graph")
	{
		graph
			.add_node_with_id(
				"person:late".parse().expect("record"),
				Some("Person".into()),
				PropertyMap::new(),
			)
			.expect("late node");
	}
	concurrent.commit().expect("concurrent commit");
	let results = execute_text(
		&mut adapter,
		"MATCH (n:Person) FROM social RETURN n.id",
		&Object::new(),
		ParserSettings::default(),
	)
	.expect("captured snapshot executes");
	let Value::Array(rows) = results[0].result.as_ref().expect("MATCH succeeds").value() else {
		panic!("expected rows");
	};
	assert_eq!(rows.len(), 3, "one MATCH must not refresh Read Committed mid-expression");
}

#[test]
fn match_reads_participate_in_serializable_validation() {
	let store = match_store();
	let mut reader = store.begin();
	let snapshot = reader.query_snapshot().expect("snapshot");
	let reference = reader.query_named_graph(&snapshot, "social").expect("graph ref");
	reader
		.execute(
			"MATCH (a:Person)-[:KNOWS]->(b:Person) FROM social RETURN a.id, b.id",
			&Object::new(),
		)
		.expect("MATCH reads graph");
	let mut concurrent = store.begin();
	if let GraphWriteView::Graph(graph) =
		concurrent.edit_graph_materialized(&reference).expect("edit graph")
	{
		graph
			.add_node_with_id(
				"person:phantom".parse().expect("record"),
				Some("Person".into()),
				PropertyMap::new(),
			)
			.expect("phantom node");
	}
	concurrent.commit().expect("concurrent commit");
	assert!(reader.commit().is_err(), "registered MATCH reads must conflict");
}
