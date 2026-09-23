use std::time::Duration;

use super::*;
use crate::{
	datastore::{
		InMemoryOptions,
		TransactionAccess,
		consts::{GRAPH_INCIDENCE_WRITE_BYTES, GRAPH_MEMBERSHIP_WRITE_BYTES},
	},
	graph::{AtomKind, GraphRead},
	values::{Object, RecordIdPart},
};

const KEY: PropertyKeyId = PropertyKeyId::new(7);

fn record(name: &str) -> RecordId {
	RecordId::new("person", RecordIdPart::String(name.into())).unwrap()
}

fn setup(memberships: usize, empty_graphs: usize) -> (Datastore, RecordId, Vec<GraphRef>) {
	let store = Datastore::in_memory();
	let mut transaction = store.begin();
	transaction.define_class(ClassSchema::new("person", AtomKind::Node, []).unwrap()).unwrap();
	let identity = record("shared");
	let mut properties = PropertyMap::new();
	properties.insert(KEY, "base");
	transaction.create_atom(identity.clone(), properties).unwrap();
	let mut graphs = Vec::new();
	for index in 0..memberships + empty_graphs {
		let graph = transaction.define_graph(format!("g{index}"), GraphKind::Graph).unwrap();
		if index < memberships {
			transaction.attach_node(graph.id(), &identity, None, None).unwrap();
		}
		graphs.push(graph);
	}
	transaction.commit().unwrap();
	(store, identity, graphs)
}

#[test]
fn first_committer_wins_for_the_same_base_atom() {
	let (store, identity, _) = setup(0, 0);
	let options = TransactionOptions::new(IsolationLevel::Snapshot, TransactionAccess::ReadWrite);
	let mut first = store.begin_with(options);
	let mut second = store.begin_with(options);
	first.set_base_property(&identity, KEY, Value::from("first")).unwrap();
	second.set_base_property(&identity, KEY, Value::from("second")).unwrap();

	first.commit().unwrap();
	let error = second.commit().unwrap_err();
	assert_eq!(error.status().code().to_string(), "FG-40001");
}

#[test]
fn atom_snapshot_keeps_its_immutable_payload_after_a_private_update() {
	let (store, identity, _) = setup(0, 0);
	let mut transaction = store.begin();
	let before = transaction.atom(&identity).unwrap().unwrap();
	transaction.set_base_property(&identity, KEY, Value::from("updated")).unwrap();
	let after = transaction.atom(&identity).unwrap().unwrap();

	assert_eq!(before.properties().get(KEY), Some(&Value::from("base")));
	assert_eq!(after.properties().get(KEY), Some(&Value::from("updated")));
}

#[test]
fn base_and_overlay_writes_are_independent_conflict_targets() {
	let (store, identity, graphs) = setup(2, 0);
	let options = TransactionOptions::new(IsolationLevel::Snapshot, TransactionAccess::ReadWrite);
	let mut base = store.begin_with(options);
	let mut overlay = store.begin_with(options);
	base.set_base_property(&identity, KEY, Value::from("updated")).unwrap();
	overlay.set_overlay(graphs[0].id(), &identity, KEY, Value::from("private")).unwrap();

	base.commit().unwrap();
	overlay.commit().unwrap();
	let mut read = store.begin();
	assert_eq!(
		read.effective_property(graphs[0].id(), &identity, KEY).unwrap(),
		Some(Value::from("private"))
	);
	assert_eq!(
		read.effective_property(graphs[1].id(), &identity, KEY).unwrap(),
		Some(Value::from("updated"))
	);
}

#[test]
fn same_overlay_obeys_first_committer_wins() {
	let (store, identity, graphs) = setup(1, 0);
	let options = TransactionOptions::new(IsolationLevel::Snapshot, TransactionAccess::ReadWrite);
	let mut first = store.begin_with(options);
	let mut second = store.begin_with(options);
	first.set_overlay(graphs[0].id(), &identity, KEY, Value::from("first")).unwrap();
	second.set_overlay(graphs[0].id(), &identity, KEY, Value::from("second")).unwrap();

	first.commit().unwrap();
	assert_eq!(second.commit().unwrap_err().status().code().to_string(), "FG-40001");
}

#[test]
fn serializable_effective_read_detects_a_base_antidependency() {
	let (store, identity, graphs) = setup(1, 0);
	let mut reader = store.begin();
	assert_eq!(
		reader.effective_property(graphs[0].id(), &identity, KEY).unwrap(),
		Some(Value::from("base"))
	);
	let mut writer = store.begin();
	writer.set_base_property(&identity, KEY, Value::from("new")).unwrap();
	writer.commit().unwrap();
	reader.hide_property(graphs[0].id(), &identity, KEY).unwrap();

	assert_eq!(reader.commit().unwrap_err().status().code().to_string(), "FG-40001");
}

#[test]
fn serializable_graph_view_tracks_effective_point_reads() {
	let (store, identity, graphs) = setup(1, 0);
	let mut reader = store.begin();
	let global = reader.working.resolve_atom(&identity).unwrap();
	let local = reader.working.membership(graphs[0].id(), global).unwrap();
	let view = graphs[0].read(&reader).unwrap();
	assert!(view.node(local).unwrap().is_some());
	drop(view);

	let mut writer = store.begin();
	writer.set_base_property(&identity, KEY, Value::from("new")).unwrap();
	writer.commit().unwrap();
	reader.set_overlay(graphs[0].id(), &identity, KEY, Value::from("reader")).unwrap();

	assert_eq!(reader.commit().unwrap_err().status().code().to_string(), "FG-40001");
}

#[test]
fn serializable_class_scan_detects_a_created_atom_phantom() {
	let (store, identity, _) = setup(0, 0);
	let mut reader = store.begin();
	reader.execute("SELECT VALUE id FROM person;", &Object::new()).unwrap();

	let mut writer = store.begin();
	writer.create_atom(record("phantom"), PropertyMap::new()).unwrap();
	writer.commit().unwrap();
	reader.set_base_property(&identity, KEY, Value::from("reader")).unwrap();

	assert_eq!(reader.commit().unwrap_err().status().code().to_string(), "FG-40001");
}

#[test]
fn serializable_point_read_detects_a_created_record() {
	let (store, identity, _) = setup(0, 0);
	let mut reader = store.begin();
	reader.execute("SELECT VALUE id FROM person:missing;", &Object::new()).unwrap();

	let mut writer = store.begin();
	writer.create_atom(record("missing"), PropertyMap::new()).unwrap();
	writer.commit().unwrap();
	reader.set_base_property(&identity, KEY, Value::from("reader")).unwrap();

	assert_eq!(reader.commit().unwrap_err().status().code().to_string(), "FG-40001");
}

#[test]
fn serializable_direct_point_read_detects_a_created_record() {
	let (store, identity, _) = setup(0, 0);
	let missing = record("missing-direct");
	let mut reader = store.begin();
	assert!(reader.atom(&missing).unwrap().is_none());

	let mut writer = store.begin();
	writer.create_atom(missing, PropertyMap::new()).unwrap();
	writer.commit().unwrap();
	reader.set_base_property(&identity, KEY, Value::from("reader")).unwrap();

	assert_eq!(reader.commit().unwrap_err().status().code().to_string(), "FG-40001");
}

#[test]
fn serializable_graph_scan_detects_an_overlay_phantom() {
	let (store, identity, graphs) = setup(1, 0);
	let mut reader = store.begin();
	reader.execute("SELECT VALUE id FROM person IN GRAPH g0;", &Object::new()).unwrap();

	let mut writer = store.begin();
	writer.set_overlay(graphs[0].id(), &identity, KEY, Value::from("private")).unwrap();
	writer.commit().unwrap();
	reader.set_base_property(&identity, KEY, Value::from("reader")).unwrap();

	assert_eq!(reader.commit().unwrap_err().status().code().to_string(), "FG-40001");
}

#[test]
fn serializable_graph_scan_detects_a_membership_phantom() {
	let (store, identity, graphs) = setup(1, 0);
	let candidate = record("candidate");
	let mut create = store.begin();
	create.create_atom(candidate.clone(), PropertyMap::new()).unwrap();
	create.commit().unwrap();

	let mut reader = store.begin();
	reader.execute("SELECT VALUE id FROM person IN GRAPH g0;", &Object::new()).unwrap();
	let mut writer = store.begin();
	writer.attach_node(graphs[0].id(), &candidate, None, None).unwrap();
	writer.commit().unwrap();
	reader.set_base_property(&identity, KEY, Value::from("reader")).unwrap();

	assert_eq!(reader.commit().unwrap_err().status().code().to_string(), "FG-40001");
}

#[test]
fn fanout_limit_fails_before_private_state_changes() {
	let (store, identity, graphs) = setup(2, 0);
	let root = (*store.inner.current_root()).clone();
	let mut options = InMemoryOptions::default();
	options.max_snapshot_age = Duration::from_secs(300);
	options.max_write_set_bytes = BASE_ATOM_WRITE_BYTES + EFFECTIVE_FANOUT_WRITE_BYTES;
	let limited = Datastore::from_root_for_test(options, root);
	let mut transaction = limited.begin();
	let error = transaction.set_base_property(&identity, KEY, Value::from("rejected")).unwrap_err();

	assert_eq!(error.status().code().to_string(), "FG-54F07");
	assert_eq!(transaction.write_set_bytes(), 0);
	assert_eq!(
		transaction.effective_property(graphs[0].id(), &identity, KEY).unwrap(),
		Some(Value::from("base"))
	);
}

#[test]
fn fanout_uses_reverse_memberships_not_all_graphs() {
	let (store, identity, graphs) = setup(1, 32);
	let root = (*store.inner.current_root()).clone();
	let mut options = InMemoryOptions::default();
	options.max_write_set_bytes = BASE_ATOM_WRITE_BYTES + EFFECTIVE_FANOUT_WRITE_BYTES;
	let limited = Datastore::from_root_for_test(options, root);
	let mut transaction = limited.begin();

	transaction.set_base_property(&identity, KEY, Value::from("allowed")).unwrap();
	transaction.commit().unwrap();
	let mut read = limited.begin();
	assert_eq!(
		read.effective_property(graphs[0].id(), &identity, KEY).unwrap(),
		Some(Value::from("allowed"))
	);
}

#[test]
fn commit_rechecks_memberships_added_after_base_preflight() {
	let (store, identity, graphs) = setup(0, 2);
	let root = (*store.inner.current_root()).clone();
	let mut options = InMemoryOptions::default();
	options.max_write_set_bytes = GRAPH_MEMBERSHIP_WRITE_BYTES + GRAPH_INCIDENCE_WRITE_BYTES;
	let limited = Datastore::from_root_for_test(options, root);
	let mut base = limited.begin_with(TransactionOptions::new(
		IsolationLevel::Snapshot,
		TransactionAccess::ReadWrite,
	));
	base.set_base_property(&identity, KEY, Value::from("pending")).unwrap();

	for graph in &graphs {
		let mut attach = limited.begin();
		attach.attach_node(graph.id(), &identity, None, None).unwrap();
		attach.commit().unwrap();
	}
	assert_eq!(base.commit().unwrap_err().status().code().to_string(), "FG-54F07");
}

#[test]
fn graph_incidence_prevents_reusing_a_concurrently_claimed_local_slot() {
	let (store, first_record, graphs) = setup(0, 1);
	let second_record = record("second");
	let mut create = store.begin();
	create.create_atom(second_record.clone(), PropertyMap::new()).unwrap();
	create.commit().unwrap();
	let options = TransactionOptions::new(IsolationLevel::Snapshot, TransactionAccess::ReadWrite);
	let mut first = store.begin_with(options);
	let mut second = store.begin_with(options);
	let first_local = first.attach_node(graphs[0].id(), &first_record, None, None).unwrap();
	let second_local = second.attach_node(graphs[0].id(), &second_record, None, None).unwrap();
	assert_eq!(first_local, second_local);

	first.commit().unwrap();
	assert_eq!(second.commit().unwrap_err().status().code().to_string(), "FG-40001");
}

#[test]
fn read_committed_refreshes_while_snapshot_keeps_its_root() {
	let (store, identity, _) = setup(0, 0);
	let mut read_committed = store.begin_with(TransactionOptions::new(
		IsolationLevel::ReadCommitted,
		TransactionAccess::ReadOnly,
	));
	let mut snapshot = store
		.begin_with(TransactionOptions::new(IsolationLevel::Snapshot, TransactionAccess::ReadOnly));
	assert_eq!(
		read_committed.atom(&identity).unwrap().unwrap().properties().get(KEY),
		Some(&Value::from("base"))
	);

	let mut writer = store.begin();
	writer.set_base_property(&identity, KEY, Value::from("new")).unwrap();
	writer.commit().unwrap();

	assert_eq!(
		read_committed.atom(&identity).unwrap().unwrap().properties().get(KEY),
		Some(&Value::from("new"))
	);
	assert_eq!(
		snapshot.atom(&identity).unwrap().unwrap().properties().get(KEY),
		Some(&Value::from("base"))
	);
}
