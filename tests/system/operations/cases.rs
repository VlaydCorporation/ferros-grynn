use ferros_grynn::{
	graph::{AtomKind, AtomRef, GraphKind, GraphRead},
	values::{PropertyMap, Value},
};

use crate::support::{create_person, people, record};

/// Verifies sdk create read update remove property and delete.
#[test]
fn sdk_create_read_update_remove_property_and_delete() {
	let db = database();
	let (name, age) = people(&db);
	let id = record("person:alice");
	create_person(&db, "person:alice", "Alice", 30);
	let mut write = db.begin();
	let atom = write.atom(&id).unwrap().unwrap();
	assert_eq!(atom.record_id(), &id);
	assert_eq!(atom.kind(), AtomKind::Node);
	assert_eq!(atom.properties().get(name), Some(&Value::from("Alice")));
	write.set_base_property(&id, age, Value::from(31_i64)).unwrap();
	write.remove_base_property(&id, name).unwrap();
	assert_eq!(write.atom(&id).unwrap().unwrap().properties().get(age), Some(&Value::from(31_i64)));
	write.commit().unwrap();
	{
		let mut read = db.begin();
		let atom = read.atom(&id).unwrap().unwrap();
		assert_eq!(atom.properties().get(name), None);
		assert_eq!(atom.properties().get(age), Some(&Value::from(31_i64)));
		read.delete_atom(&id).unwrap();
		read.commit().unwrap();
	}
	db.assert_committed(|store| assert!(store.begin().atom(&id).unwrap().is_none()));
}

/// Verifies dropped sdk writes leave all committed records unchanged.
#[test]
fn dropped_sdk_writes_leave_all_committed_records_unchanged() {
	let db = database();
	let (name, _) = people(&db);
	create_person(&db, "person:alice", "Alice", 30);
	create_person(&db, "person:bob", "Bob", 40);
	{
		let mut write = db.begin();
		write.set_base_property(&record("person:alice"), name, Value::from("Private")).unwrap();
		write.delete_atom(&record("person:bob")).unwrap();
		write.create_atom(record("person:carol"), PropertyMap::new()).unwrap();
		assert!(write.atom(&record("person:carol")).unwrap().is_some());
	}
	db.assert_committed(|store| {
		let mut read = store.begin();
		assert_eq!(
			read.atom(&record("person:alice")).unwrap().unwrap().properties().get(name),
			Some(&Value::from("Alice"))
		);
		assert!(read.atom(&record("person:bob")).unwrap().is_some());
		assert!(read.atom(&record("person:carol")).unwrap().is_none());
	});
}

/// Verifies duplicate create is rejected without overwriting the record.
#[test]
fn duplicate_create_is_rejected_without_overwriting_the_record() {
	let db = database();
	let (name, _) = people(&db);
	create_person(&db, "person:alice", "Alice", 30);
	let mut write = db.begin();
	let mut properties = PropertyMap::new();
	properties.insert(name, "Duplicate");
	assert!(write.create_atom(record("person:alice"), properties).is_err());
	write.create_atom(record("person:bob"), PropertyMap::new()).unwrap();
	write.commit().unwrap();
	db.assert_committed(|store| {
		let mut read = store.begin();
		assert_eq!(
			read.atom(&record("person:alice")).unwrap().unwrap().properties().get(name),
			Some(&Value::from("Alice"))
		);
		assert!(read.atom(&record("person:bob")).unwrap().is_some());
	});
}

/// Verifies detach preserves base atom and cascade delete removes all
/// memberships.
#[test]
fn detach_preserves_base_atom_and_cascade_delete_removes_all_memberships() {
	let db = database();
	people(&db);
	create_person(&db, "person:alice", "Alice", 30);
	let id = record("person:alice");
	let mut write = db.begin();
	let first = write.define_graph("first", GraphKind::Graph).unwrap();
	let second = write.define_graph("second", GraphKind::Graph).unwrap();
	write.attach_node(first.id(), &id, None, None).unwrap();
	write.attach_node(second.id(), &id, None, None).unwrap();
	write.commit().unwrap();
	let mut write = db.begin();
	assert!(write.delete_atom(&id).is_err());
	write.detach_occurrence(first.id(), &id, false).unwrap();
	assert!(write.atom(&id).unwrap().is_some());
	assert!(write.membership(second.id(), &id).unwrap().is_some());
	write.delete_atom_cascade(&id, true).unwrap();
	write.commit().unwrap();
	db.assert_committed(|store| {
		let mut read = store.begin();
		assert!(read.atom(&id).unwrap().is_none());
		for graph in [&first, &second] {
			let view = graph.read(&read).unwrap();
			assert_eq!(view.node_count().unwrap(), 0);
			assert!(view.atom_id(&AtomRef::new(AtomKind::Node, id.clone())).unwrap().is_none());
		}
	});
}
