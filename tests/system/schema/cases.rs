use ferros_grynn::{
	datastore::{ClassField, ClassSchema},
	graph::{AtomKind, GraphKind},
	values::{Object, PropertyMap, Type},
};

use crate::support::{execute, record};

/// Verifies sdk catalog definitions are private until commit and keep stable
/// identities.
#[test]
fn sdk_catalog_definitions_are_private_until_commit_and_keep_stable_identities() {
	let db = database();
	let mut write = db.begin();
	let class = write
		.define_class(
			ClassSchema::new("person", AtomKind::Node, [ClassField::new("name", Type::String)])
				.unwrap(),
		)
		.unwrap();
	let graph = write.define_graph("people", GraphKind::Dag).unwrap();
	assert_eq!(write.class("person").unwrap().unwrap().id(), Some(class));
	assert_eq!(write.named_graph("people").unwrap().unwrap().id(), graph.id());
	assert!(db.class("person").is_none());
	assert!(db.begin().named_graph("people").unwrap().is_none());
	write.commit().unwrap();
	db.assert_committed(|store| {
		assert_eq!(store.class("person").unwrap().id(), Some(class));
		assert_eq!(store.class_by_id(class).unwrap().name().as_str(), "person");
		assert_eq!(store.graph(graph.id()).unwrap().reference().kind(), GraphKind::Dag);
		let mut read = store.begin();
		assert!(read.classes().unwrap().iter().any(|definition| definition.id() == Some(class)));
		assert!(read.graphs().unwrap().iter().any(|definition| definition.id() == graph.id()));
		assert_eq!(read.named_graph("people").unwrap().unwrap().id(), graph.id());
	});
}

/// Verifies schemafull rejects wrong types and unknown fields without partial
/// data.
#[test]
fn schemafull_rejects_wrong_types_and_unknown_fields_without_partial_data() {
	let db = database();
	execute(
		&db,
		"DEFINE CLASS person TYPE NODE SCHEMAFULL; DEFINE FIELD age ON CLASS person TYPE int;",
		&Object::new(),
	);
	for query in
		["CREATE person:wrong SET age = 'thirty';", "CREATE person:unknown SET extra = true;"]
	{
		let result = db.execute(query, &Object::new()).unwrap();
		assert!(result[0].result.is_err(), "{query}");
	}
	execute(&db, "CREATE person:valid SET age = 30;", &Object::new());
	db.assert_committed(|store| {
		let mut read = store.begin();
		assert!(read.atom(&record("person:wrong")).unwrap().is_none());
		assert!(read.atom(&record("person:unknown")).unwrap().is_none());
		assert!(read.atom(&record("person:valid")).unwrap().is_some());
	});
}

/// Verifies dropping uncommitted schema leaves no class or graph.
#[test]
fn dropping_uncommitted_schema_leaves_no_class_or_graph() {
	let db = database();
	{
		let mut write = db.begin();
		write.define_class(ClassSchema::new("private", AtomKind::Node, []).unwrap()).unwrap();
		write.define_graph("private_graph", GraphKind::Graph).unwrap();
		write.create_atom(record("private:a"), PropertyMap::new()).unwrap();
	}
	db.assert_committed(|store| {
		let mut read = store.begin();
		assert!(read.class("private").unwrap().is_none());
		assert!(read.named_graph("private_graph").unwrap().is_none());
		assert!(store.class("private").is_none());
	});
}
