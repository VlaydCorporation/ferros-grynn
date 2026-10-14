use ferros_grynn::{
	datastore::{IsolationLevel, TransactionAccess, TransactionOptions},
	values::Value,
};

use crate::support::{create_person, people, record};

#[test]
fn snapshot_is_stable_while_read_committed_refreshes_between_sdk_calls() {
	let db = database();
	let (name, _) = people(&db);
	create_person(&db, "person:alice", "Before", 30);
	let id = record("person:alice");
	let mut snapshot = db
		.begin_with(TransactionOptions::new(IsolationLevel::Snapshot, TransactionAccess::ReadOnly));
	let mut refreshed = db.begin_with(TransactionOptions::new(
		IsolationLevel::ReadCommitted,
		TransactionAccess::ReadOnly,
	));
	assert_eq!(
		refreshed.atom(&id).unwrap().unwrap().properties().get(name),
		Some(&Value::from("Before"))
	);
	let mut write = db.begin();
	write.set_base_property(&id, name, Value::from("After")).unwrap();
	write.commit().unwrap();
	assert_eq!(
		snapshot.atom(&id).unwrap().unwrap().properties().get(name),
		Some(&Value::from("Before"))
	);
	assert_eq!(
		refreshed.atom(&id).unwrap().unwrap().properties().get(name),
		Some(&Value::from("After"))
	);
	drop(snapshot);
	drop(refreshed);
	db.assert_committed(|store| {
		assert_eq!(
			store.begin().atom(&id).unwrap().unwrap().properties().get(name),
			Some(&Value::from("After"))
		);
	});
}

#[test]
fn first_committer_wins_and_rejected_commit_publishes_no_partial_writes() {
	let db = database();
	let (name, _) = people(&db);
	create_person(&db, "person:alice", "Before", 30);
	create_person(&db, "person:bob", "Bob", 40);
	let options = TransactionOptions::new(IsolationLevel::Snapshot, TransactionAccess::ReadWrite);
	let mut first = db.begin_with(options);
	let mut second = db.begin_with(options);
	first.set_base_property(&record("person:alice"), name, Value::from("Winner")).unwrap();
	second.set_base_property(&record("person:alice"), name, Value::from("Loser")).unwrap();
	second.set_base_property(&record("person:bob"), name, Value::from("Must not leak")).unwrap();
	first.commit().unwrap();
	let error = second.commit().unwrap_err();
	assert_eq!(error.status().code().to_string(), "FG-40001");
	db.assert_committed(|store| {
		let mut read = store.begin();
		assert_eq!(
			read.atom(&record("person:alice")).unwrap().unwrap().properties().get(name),
			Some(&Value::from("Winner"))
		);
		assert_eq!(
			read.atom(&record("person:bob")).unwrap().unwrap().properties().get(name),
			Some(&Value::from("Bob"))
		);
	});
}

#[test]
fn disjoint_snapshot_writes_merge_instead_of_losing_a_commit() {
	let db = database();
	let (name, _) = people(&db);
	create_person(&db, "person:alice", "Alice", 30);
	create_person(&db, "person:bob", "Bob", 40);
	let options = TransactionOptions::new(IsolationLevel::Snapshot, TransactionAccess::ReadWrite);
	let mut first = db.begin_with(options);
	let mut second = db.begin_with(options);
	first.set_base_property(&record("person:alice"), name, Value::from("A")).unwrap();
	second.set_base_property(&record("person:bob"), name, Value::from("B")).unwrap();
	first.commit().unwrap();
	second.commit().unwrap();
	db.assert_committed(|store| {
		let mut read = store.begin();
		for (id, expected) in [("person:alice", "A"), ("person:bob", "B")] {
			assert_eq!(
				read.atom(&record(id)).unwrap().unwrap().properties().get(name),
				Some(&Value::from(expected))
			);
		}
	});
}

#[test]
fn serializable_rejects_a_write_based_on_a_changed_read() {
	let db = database();
	let (_, age) = people(&db);
	create_person(&db, "person:alice", "Alice", 30);
	create_person(&db, "person:bob", "Bob", 40);
	let mut dependent = db.begin();
	assert_eq!(
		dependent.atom(&record("person:alice")).unwrap().unwrap().properties().get(age),
		Some(&Value::from(30_i64))
	);
	let mut writer = db.begin();
	writer.set_base_property(&record("person:alice"), age, Value::from(31_i64)).unwrap();
	writer.commit().unwrap();
	dependent.set_base_property(&record("person:bob"), age, Value::from(99_i64)).unwrap();
	assert_eq!(dependent.commit().unwrap_err().status().code().to_string(), "FG-40001");
	db.assert_committed(|store| {
		assert_eq!(
			store.begin().atom(&record("person:bob")).unwrap().unwrap().properties().get(age),
			Some(&Value::from(40_i64))
		);
	});
}

#[test]
fn read_only_sdk_transaction_rejects_mutation() {
	let db = database();
	let (name, _) = people(&db);
	create_person(&db, "person:alice", "Alice", 30);
	let mut read = db
		.begin_with(TransactionOptions::new(IsolationLevel::Snapshot, TransactionAccess::ReadOnly));
	assert!(
		read.set_base_property(&record("person:alice"), name, Value::from("Forbidden")).is_err()
	);
	assert!(read.delete_atom(&record("person:alice")).is_err());
	drop(read);
	db.assert_committed(|store| {
		assert_eq!(
			store.begin().atom(&record("person:alice")).unwrap().unwrap().properties().get(name),
			Some(&Value::from("Alice"))
		);
	});
}
