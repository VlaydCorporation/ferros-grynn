use crate::{
	datastore::{ClassSchema, Datastore, ImportOptions, ImportViolationPolicy},
	graph::{AtomKind, GraphKind},
	values::{Object, PropertyMap, RecordId, Value},
};

fn import_store() -> Datastore {
	let store = Datastore::in_memory();
	let mut transaction = store.begin();
	transaction.define_class(ClassSchema::new("person", AtomKind::Node, []).unwrap()).unwrap();
	transaction.define_class(ClassSchema::new("friendship", AtomKind::Edge, []).unwrap()).unwrap();
	transaction.define_graph("social", GraphKind::Graph).unwrap();
	transaction.commit().unwrap();
	store
}

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
		.unwrap();
	assert_eq!(results.len(), 1);
	assert!(results[0].result.is_ok());
	let Value::Object(report) = results[0].result.as_ref().unwrap().value() else {
		panic!("import must return a structured report");
	};
	assert_eq!(report.get("processed"), Some(&Value::from(10_u64)));
	assert_eq!(report.get("applied"), Some(&Value::from(10_u64)));
	assert_eq!(report.get("segments"), Some(&Value::from(5_u64)));

	let mut read = store.begin();
	for record in ["person:alice", "person:bob", "person:carol", "friendship:known"] {
		assert!(read.atom(&record.parse::<RecordId>().unwrap()).unwrap().is_some());
	}
	assert!(read.atom(&"person:remove".parse::<RecordId>().unwrap()).unwrap().is_none());
}

#[test]
fn gql_import_abort_and_skip_match_callback_atomicity() {
	let aborted = import_store();
	let results = aborted
		.execute(
			"BEGIN IMPORT; CREATE person:alice; CREATE person:alice; END IMPORT;",
			&Object::new(),
		)
		.unwrap();
	assert!(results[0].result.is_err());
	assert!(aborted.begin().atom(&"person:alice".parse::<RecordId>().unwrap()).unwrap().is_none());

	let skipped = import_store();
	let results = skipped
		.execute(
			"BEGIN IMPORT WITH (ON VIOLATION = SKIP, REJECT LIMIT = 1); CREATE person:alice; \
			 CREATE person:alice; CREATE person:bob; END IMPORT;",
			&Object::new(),
		)
		.unwrap();
	let Value::Object(report) = results[0].result.as_ref().unwrap().value() else {
		panic!("import must return a structured report");
	};
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
					transaction.create_atom(record.parse().unwrap(), PropertyMap::new())
				})?;
			}
			Ok(())
		})
		.unwrap();
	assert_eq!(report.processed_items(), 3);
	assert_eq!(report.applied_items(), 2);
	assert_eq!(report.skipped_items(), 1);
}

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
		.unwrap();
	let Value::Object(gql_report) = results[0].result.as_ref().unwrap().value() else {
		panic!("import must return a structured report");
	};

	let graph = callback.schema_catalog().named_graph("social").unwrap().reference().id();
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
							format!("person:{name}").parse().unwrap(),
							PropertyMap::new(),
						)
					})?;
				}
				for name in ["alice", "bob"] {
					session.apply_item(|transaction| {
						transaction.attach_node(
							graph,
							&format!("person:{name}").parse().unwrap(),
							None,
							None,
						)
					})?;
				}
				Ok(())
			},
		)
		.unwrap();

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
		gql_root.graph(graph).unwrap().occurrences().cloned().collect::<Vec<_>>(),
		callback_root.graph(graph).unwrap().occurrences().cloned().collect::<Vec<_>>()
	);
	assert_eq!(gql_root.statistics, callback_root.statistics);
}

#[test]
fn import_preflight_rejects_results_scope_mismatch_and_unsupported_options() {
	let store = import_store();
	for source in [
		"BEGIN IMPORT WITH (EVENTS = ON); CREATE person:alice; END IMPORT;",
		"BEGIN IMPORT INTO GRAPH social; CREATE person:alice RETURN VALUE id; END IMPORT;",
		"BEGIN IMPORT INTO GRAPH social; ATTACH person:alice TO GRAPH other; END IMPORT;",
	] {
		assert!(store.execute(source, &Object::new()).is_err(), "source: {source}");
	}
	assert_eq!(store.inner.current_root().commit_ts, 1);
	assert!(store.begin().atom(&"person:alice".parse::<RecordId>().unwrap()).unwrap().is_none());

	let mut transaction = store.begin();
	let error = transaction
		.execute("BEGIN IMPORT; CREATE person:alice; END IMPORT;", &Object::new())
		.unwrap_err();
	assert_eq!(error.status().code().to_string(), "FG-50F03");
	transaction.create_atom("person:after".parse().unwrap(), PropertyMap::new()).unwrap();
	transaction.commit().unwrap();
}

#[test]
fn scoped_import_rejects_cross_graph_delete_cascade() {
	let store = import_store();
	let mut setup = store.begin();
	let social = setup.named_graph("social").unwrap().unwrap();
	let other = setup.define_graph("other", GraphKind::Graph).unwrap();
	let alice = "person:alice".parse::<RecordId>().unwrap();
	setup.create_atom(alice.clone(), PropertyMap::new()).unwrap();
	setup.attach_node(social.id(), &alice, None, None).unwrap();
	setup.attach_node(other.id(), &alice, None, None).unwrap();
	setup.commit().unwrap();

	let results = store
		.execute(
			"BEGIN IMPORT INTO GRAPH social; DELETE person:alice CASCADE; END IMPORT;",
			&Object::new(),
		)
		.unwrap();
	assert!(results[0].result.is_err());

	let mut read = store.begin();
	assert!(read.atom(&alice).unwrap().is_some());
	assert!(read.membership(social.id(), &alice).unwrap().is_some());
	assert!(read.membership(other.id(), &alice).unwrap().is_some());
}

#[test]
fn deferred_graph_family_failure_discards_the_complete_import() {
	let store = Datastore::in_memory();
	let mut schema = store.begin();
	schema.define_class(ClassSchema::new("person", AtomKind::Node, []).unwrap()).unwrap();
	schema.define_class(ClassSchema::new("friendship", AtomKind::Edge, []).unwrap()).unwrap();
	schema.define_graph("flow", GraphKind::Dag).unwrap();
	schema.commit().unwrap();

	let results = store
		.execute(
			"BEGIN IMPORT INTO GRAPH flow WITH (CONSTRAINTS = DEFERRED); CREATE person:alice; \
			 CREATE person:bob; CREATE person:carol; RELATE \
			 (person:alice)-[friendship:ab]->(person:bob); RELATE \
			 (person:bob)-[friendship:bc]->(person:carol); RELATE \
			 (person:carol)-[friendship:ca]->(person:alice); END IMPORT;",
			&Object::new(),
		)
		.unwrap();
	assert!(results[0].result.is_err());
	assert!(store.begin().atom(&"person:alice".parse::<RecordId>().unwrap()).unwrap().is_none());
}
