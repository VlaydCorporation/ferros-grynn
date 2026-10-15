use std::time::Duration;

use super::*;
#[cfg(test)]
use crate::test_assertions::AssertOption as _;
#[cfg(test)]
use crate::test_assertions::AssertResult as _;
use crate::{
	datastore::{
		InMemoryOptions,
		OccurrenceStateRef,
		TransactionAccess,
		consts::{GRAPH_INCIDENCE_WRITE_BYTES, GRAPH_MEMBERSHIP_WRITE_BYTES},
	},
	graph::{AtomKind, EdgeTopology, GraphRead, GraphTopologyConstraint, GraphTopologyConstraints},
	values::{Object, RecordIdPart},
};

/// Property identifier shared by transaction mutation and index fixtures.
const KEY: PropertyKeyId = PropertyKeyId::new(7);

/// Verifies edge endpoint postings publish abort replace and retire with
/// topology.
#[test]
fn edge_endpoint_postings_publish_abort_replace_and_retire_with_topology() {
	use crate::graph::{EdgeEndpoint, TraversalIndex, TraversalSemantics};
	let store = Datastore::in_memory();
	let mut seed = store.begin();
	seed.define_class(ClassSchema::new("person", AtomKind::Node, []).assert_ok()).assert_ok();
	seed.define_class(ClassSchema::new("relation", AtomKind::Edge, []).assert_ok()).assert_ok();
	let graph = seed.define_graph("endpoints", GraphKind::Graph).assert_ok();
	let records = ["person:a", "person:b", "relation:base", "relation:connector"]
		.map(|value| value.parse::<RecordId>().assert_ok());
	for record in &records {
		seed.create_atom(record.clone(), PropertyMap::new()).assert_ok();
	}
	let a = seed.attach_node(graph.id(), &records[0], None, None).assert_ok();
	let b = seed.attach_node(graph.id(), &records[1], None, None).assert_ok();
	let endpoint = seed
		.attach_edge(graph.id(), &records[2], None, EdgeTopology::directed([a], [b]), None, None)
		.assert_ok();
	seed.commit().assert_ok();
	let old_without = store.begin();
	assert!(
		old_without.snapshot.schema.graph(graph.id()).assert_some().edge_endpoint_index().is_none()
	);
	let mut writer = store.begin();
	let connector = writer
		.attach_edge(
			graph.id(),
			&records[3],
			None,
			EdgeTopology::directed([endpoint, a], [endpoint, b]),
			None,
			None,
		)
		.assert_ok();
	let key = EdgeEndpoint::Local(endpoint);
	assert_eq!(
		writer
			.working
			.graph(graph.id())
			.assert_some()
			.edge_endpoint_postings(&key)
			.copied()
			.collect::<Vec<_>>(),
		vec![(connector, false), (connector, true)]
	);
	writer.commit().assert_ok();
	assert!(
		old_without.snapshot.schema.graph(graph.id()).assert_some().edge_endpoint_index().is_none()
	);
	let mut old = store.begin();
	let capability =
		old.snapshot.schema.graph(graph.id()).assert_some().edge_endpoint_index().assert_some();
	let view = graph.read(&old).assert_ok();
	assert_eq!(view.edge_endpoint_connectors(&key).assert_ok(), Some(vec![connector]));
	let exact = TraversalIndex::new(&view, TraversalSemantics::Atom).assert_ok();
	let indexed = TraversalIndex::for_edge_endpoint(&view, &key).assert_ok().assert_some();
	for direction in [
		crate::graph::TraversalDirection::Forward,
		crate::graph::TraversalDirection::Backward,
		crate::graph::TraversalDirection::Both,
	] {
		assert_eq!(
			indexed.steps(&key, direction).collect::<Vec<_>>(),
			exact.steps(&key, direction).collect::<Vec<_>>()
		);
	}
	drop(view);
	let mut private = store.begin();
	private
		.replace_occurrence_state(
			graph.id(),
			&records[3],
			OccurrenceState::edge(EdgeTopology::directed([a], [a, b]), None, None),
		)
		.assert_ok();
	assert_eq!(
		private.working.graph(graph.id()).assert_some().edge_endpoint_postings(&key).count(),
		0
	);
	let rebuilt = crate::stats::analyze_database(&private.working).assert_ok();
	assert_eq!(
		private.working.statistics.graph(graph.id()).assert_some().total_in_degree,
		rebuilt.graph(graph.id()).assert_some().total_in_degree
	);
	drop(private);
	assert_eq!(
		old.snapshot.graph(graph.id()).assert_some().edge_endpoint_postings(&key).count(),
		2
	);
	let mut remove = store.begin();
	remove.detach_occurrence(graph.id(), &records[3], false).assert_ok();
	remove.commit().assert_ok();
	assert_eq!(
		store.begin().snapshot.graph(graph.id()).assert_some().edge_endpoint_postings(&key).count(),
		0
	);
	assert_eq!(
		store.begin().snapshot.schema.graph(graph.id()).assert_some().edge_endpoint_index(),
		Some(capability)
	);
	assert_eq!(old.graphs().assert_ok()[0].edge_endpoint_index(), Some(capability));
}

/// Verifies edge endpoint postings preserve two paths to the same child graph.
#[test]
fn edge_endpoint_postings_preserve_two_paths_to_the_same_child_graph() {
	use crate::graph::{EdgeEndpoint, QualifiedAtomId};
	let store = Datastore::in_memory();
	let mut tx = store.begin();
	for (class, kind) in [("person", AtomKind::Node), ("relation", AtomKind::Edge)] {
		tx.define_class(ClassSchema::new(class, kind, []).assert_ok()).assert_ok();
	}
	let parent = tx.define_graph("parent", GraphKind::Metagraph).assert_ok();
	let child = tx.define_graph("child", GraphKind::Graph).assert_ok();
	let records = [
		"person:a",
		"person:b",
		"person:c1",
		"person:c2",
		"relation:base",
		"relation:x",
		"relation:y",
	]
	.map(|value| value.parse::<RecordId>().assert_ok());
	for record in &records {
		tx.create_atom(record.clone(), PropertyMap::new()).assert_ok();
	}
	let a = tx.attach_node(child.id(), &records[0], None, None).assert_ok();
	let b = tx.attach_node(child.id(), &records[1], None, None).assert_ok();
	let endpoint = tx
		.attach_edge(child.id(), &records[4], None, EdgeTopology::directed([a], [b]), None, None)
		.assert_ok();
	let first = tx.attach_node(parent.id(), &records[2], None, Some(child.id())).assert_ok();
	let second = tx.attach_node(parent.id(), &records[3], None, Some(child.id())).assert_ok();
	let q1 = EdgeEndpoint::Nested(QualifiedAtomId::new(vec![first], endpoint).assert_ok());
	let q2 = EdgeEndpoint::Nested(QualifiedAtomId::new(vec![second], endpoint).assert_ok());
	let x = tx
		.attach_edge(
			parent.id(),
			&records[5],
			None,
			EdgeTopology::directed([q1.clone()], [EdgeEndpoint::Local(second)]),
			None,
			None,
		)
		.assert_ok();
	let y = tx
		.attach_edge(
			parent.id(),
			&records[6],
			None,
			EdgeTopology::directed([q2.clone()], [EdgeEndpoint::Local(first)]),
			None,
			None,
		)
		.assert_ok();
	tx.commit().assert_ok();
	let read = store.begin();
	let view = parent.read(&read).assert_ok();
	assert_eq!(view.edge_endpoint_connectors(&q1).assert_ok(), Some(vec![x]));
	assert_eq!(view.edge_endpoint_connectors(&q2).assert_ok(), Some(vec![y]));
}

/// Returns record for this module.
fn record(name: &str) -> RecordId {
	RecordId::new("person", RecordIdPart::String(name.into())).assert_ok()
}

/// Builds the setup fixture used by nearby scenarios.
fn setup(memberships: usize, empty_graphs: usize) -> (Datastore, RecordId, Vec<GraphRef>) {
	let store = Datastore::in_memory();
	let mut transaction = store.begin();
	transaction
		.define_class(ClassSchema::new("person", AtomKind::Node, []).assert_ok())
		.assert_ok();
	let identity = record("shared");
	let mut properties = PropertyMap::new();
	properties.insert(KEY, "base");
	transaction.create_atom(identity.clone(), properties).assert_ok();
	let mut graphs = Vec::new();
	for index in 0..memberships + empty_graphs {
		let graph = transaction.define_graph(format!("g{index}"), GraphKind::Graph).assert_ok();
		if index < memberships {
			transaction.attach_node(graph.id(), &identity, None, None).assert_ok();
		}
		graphs.push(graph);
	}
	transaction.commit().assert_ok();
	(store, identity, graphs)
}

/// Verifies commit rejects a direct rust mutation that breaks simple.
#[test]
fn commit_rejects_a_direct_rust_mutation_that_breaks_simple() {
	let store = Datastore::in_memory();
	let mut transaction = store.begin();
	transaction
		.define_class(ClassSchema::new("person", AtomKind::Node, []).assert_ok())
		.assert_ok();
	transaction
		.define_class(ClassSchema::new("relation", AtomKind::Edge, []).assert_ok())
		.assert_ok();
	let node = record("loop");
	let edge = RecordId::new("relation", RecordIdPart::String("self".into())).assert_ok();
	transaction.create_atom(node.clone(), PropertyMap::new()).assert_ok();
	transaction.create_atom(edge.clone(), PropertyMap::new()).assert_ok();
	let mut constraints = GraphTopologyConstraints::empty();
	constraints.insert(GraphTopologyConstraint::Simple);
	let graph = transaction
		.define_graph_with_contract(
			"simple",
			GraphKind::Graph,
			PropertyMap::new(),
			constraints,
			None,
			None,
		)
		.assert_ok();
	let local = transaction.attach_node(graph.id(), &node, None, None).assert_ok();
	transaction
		.attach_edge(graph.id(), &edge, None, EdgeTopology::directed([local], [local]), None, None)
		.assert_ok();

	assert!(transaction.commit().is_err());
}

/// Verifies commit rejects direct rust family and root violations.
#[test]
fn commit_rejects_direct_rust_family_and_root_violations() {
	let store = Datastore::in_memory();
	let mut transaction = store.begin();
	transaction
		.define_class(ClassSchema::new("person", AtomKind::Node, []).assert_ok())
		.assert_ok();
	transaction
		.define_class(ClassSchema::new("relation", AtomKind::Edge, []).assert_ok())
		.assert_ok();
	let left = record("left");
	let right = record("right");
	let forward = RecordId::new("relation", RecordIdPart::String("forward".into())).assert_ok();
	let reverse = RecordId::new("relation", RecordIdPart::String("reverse".into())).assert_ok();
	for atom in [&left, &right, &forward, &reverse] {
		transaction.create_atom(atom.clone(), PropertyMap::new()).assert_ok();
	}
	let dag = transaction.define_graph("dag", GraphKind::Dag).assert_ok();
	let local_left = transaction.attach_node(dag.id(), &left, None, None).assert_ok();
	let local_right = transaction.attach_node(dag.id(), &right, None, None).assert_ok();
	transaction
		.attach_edge(
			dag.id(),
			&forward,
			None,
			EdgeTopology::directed([local_left], [local_right]),
			None,
			None,
		)
		.assert_ok();
	transaction
		.attach_edge(
			dag.id(),
			&reverse,
			None,
			EdgeTopology::directed([local_right], [local_left]),
			None,
			None,
		)
		.assert_ok();
	assert!(transaction.commit().is_err());

	let store = Datastore::in_memory();
	let mut transaction = store.begin();
	transaction
		.define_class(ClassSchema::new("person", AtomKind::Node, []).assert_ok())
		.assert_ok();
	transaction
		.define_class(ClassSchema::new("relation", AtomKind::Edge, []).assert_ok())
		.assert_ok();
	let left = record("left");
	let right = record("right");
	let edge = RecordId::new("relation", RecordIdPart::String("edge".into())).assert_ok();
	for atom in [&left, &right, &edge] {
		transaction.create_atom(atom.clone(), PropertyMap::new()).assert_ok();
	}
	let rooted = transaction
		.define_graph_with_contract(
			"rooted",
			GraphKind::RootedTree,
			PropertyMap::new(),
			GraphTopologyConstraints::empty(),
			Some(right.clone()),
			None,
		)
		.assert_ok();
	let local_left = transaction.attach_node(rooted.id(), &left, None, None).assert_ok();
	let local_right = transaction.attach_node(rooted.id(), &right, None, None).assert_ok();
	transaction
		.attach_edge(
			rooted.id(),
			&edge,
			None,
			EdgeTopology::directed([local_left], [local_right]),
			None,
			None,
		)
		.assert_ok();
	assert!(transaction.commit().is_err());
}

/// Verifies first committer wins for the same base atom.
#[test]
fn first_committer_wins_for_the_same_base_atom() {
	let (store, identity, _) = setup(0, 0);
	let options = TransactionOptions::new(IsolationLevel::Snapshot, TransactionAccess::ReadWrite);
	let mut first = store.begin_with(options);
	let mut second = store.begin_with(options);
	first.set_base_property(&identity, KEY, Value::from("first")).assert_ok();
	second.set_base_property(&identity, KEY, Value::from("second")).assert_ok();

	first.commit().assert_ok();
	let error = second.commit().assert_err();
	assert_eq!(error.status().code().to_string(), "FG-40001");
}

/// Verifies concurrent create of the same record still conflicts.
#[test]
fn concurrent_create_of_the_same_record_still_conflicts() {
	let store = Datastore::in_memory();
	let mut schema = store.begin();
	schema.define_class(ClassSchema::new("person", AtomKind::Node, []).assert_ok()).assert_ok();
	schema.commit().assert_ok();
	let identity = record("same");
	let mut first = store.begin();
	let mut second = store.begin();
	first.create_atom(identity.clone(), PropertyMap::new()).assert_ok();
	second.create_atom(identity, PropertyMap::new()).assert_ok();

	first.commit().assert_ok();
	assert_eq!(second.commit().assert_err().status().code().to_string(), "FG-40001");
}

/// Verifies atom snapshot keeps its immutable payload after a private update.
#[test]
fn atom_snapshot_keeps_its_immutable_payload_after_a_private_update() {
	let (store, identity, _) = setup(0, 0);
	let mut transaction = store.begin();
	let before = transaction.atom(&identity).assert_ok().assert_some();
	transaction.set_base_property(&identity, KEY, Value::from("updated")).assert_ok();
	let after = transaction.atom(&identity).assert_ok().assert_some();

	assert_eq!(before.properties().get(KEY), Some(&Value::from("base")));
	assert_eq!(after.properties().get(KEY), Some(&Value::from("updated")));
}

/// Verifies base and overlay writes are independent conflict targets.
#[test]
fn base_and_overlay_writes_are_independent_conflict_targets() {
	let (store, identity, graphs) = setup(2, 0);
	let options = TransactionOptions::new(IsolationLevel::Snapshot, TransactionAccess::ReadWrite);
	let mut base = store.begin_with(options);
	let mut overlay = store.begin_with(options);
	base.set_base_property(&identity, KEY, Value::from("updated")).assert_ok();
	overlay.set_overlay(graphs[0].id(), &identity, KEY, Value::from("private")).assert_ok();

	base.commit().assert_ok();
	overlay.commit().assert_ok();
	let mut read = store.begin();
	assert_eq!(
		read.effective_property(graphs[0].id(), &identity, KEY).assert_ok(),
		Some(Value::from("private"))
	);
	assert_eq!(
		read.effective_property(graphs[1].id(), &identity, KEY).assert_ok(),
		Some(Value::from("updated"))
	);
}

/// Verifies same overlay obeys first committer wins.
#[test]
fn same_overlay_obeys_first_committer_wins() {
	let (store, identity, graphs) = setup(1, 0);
	let options = TransactionOptions::new(IsolationLevel::Snapshot, TransactionAccess::ReadWrite);
	let mut first = store.begin_with(options);
	let mut second = store.begin_with(options);
	first.set_overlay(graphs[0].id(), &identity, KEY, Value::from("first")).assert_ok();
	second.set_overlay(graphs[0].id(), &identity, KEY, Value::from("second")).assert_ok();

	first.commit().assert_ok();
	assert_eq!(second.commit().assert_err().status().code().to_string(), "FG-40001");
}

/// Verifies serializable effective read detects a base antidependency.
#[test]
fn serializable_effective_read_detects_a_base_antidependency() {
	let (store, identity, graphs) = setup(1, 0);
	let mut reader = store.begin();
	assert_eq!(
		reader.effective_property(graphs[0].id(), &identity, KEY).assert_ok(),
		Some(Value::from("base"))
	);
	let mut writer = store.begin();
	writer.set_base_property(&identity, KEY, Value::from("new")).assert_ok();
	writer.commit().assert_ok();
	reader.hide_property(graphs[0].id(), &identity, KEY).assert_ok();

	assert_eq!(reader.commit().assert_err().status().code().to_string(), "FG-40001");
}

/// Verifies serializable graph view tracks effective point reads.
#[test]
fn serializable_graph_view_tracks_effective_point_reads() {
	let (store, identity, graphs) = setup(1, 0);
	let mut reader = store.begin();
	let global = reader.working.resolve_atom(&identity).assert_some();
	let local = reader.working.membership(graphs[0].id(), global).assert_some();
	let view = graphs[0].read(&reader).assert_ok();
	assert!(view.node(local).assert_ok().is_some());
	drop(view);

	let mut writer = store.begin();
	writer.set_base_property(&identity, KEY, Value::from("new")).assert_ok();
	writer.commit().assert_ok();
	reader.set_overlay(graphs[0].id(), &identity, KEY, Value::from("reader")).assert_ok();

	assert_eq!(reader.commit().assert_err().status().code().to_string(), "FG-40001");
}

/// Verifies serializable class scan detects a created atom phantom.
#[test]
fn serializable_class_scan_detects_a_created_atom_phantom() {
	let (store, identity, _) = setup(0, 0);
	let mut reader = store.begin();
	reader.execute("SELECT VALUE id FROM person;", &Object::new()).assert_ok();

	let mut writer = store.begin();
	writer.create_atom(record("phantom"), PropertyMap::new()).assert_ok();
	writer.commit().assert_ok();
	reader.set_base_property(&identity, KEY, Value::from("reader")).assert_ok();

	assert_eq!(reader.commit().assert_err().status().code().to_string(), "FG-40001");
}

/// Verifies serializable point read detects a created record.
#[test]
fn serializable_point_read_detects_a_created_record() {
	let (store, identity, _) = setup(0, 0);
	let mut reader = store.begin();
	reader.execute("SELECT VALUE id FROM person:missing;", &Object::new()).assert_ok();

	let mut writer = store.begin();
	writer.create_atom(record("missing"), PropertyMap::new()).assert_ok();
	writer.commit().assert_ok();
	reader.set_base_property(&identity, KEY, Value::from("reader")).assert_ok();

	assert_eq!(reader.commit().assert_err().status().code().to_string(), "FG-40001");
}

/// Verifies serializable direct point read detects a created record.
#[test]
fn serializable_direct_point_read_detects_a_created_record() {
	let (store, identity, _) = setup(0, 0);
	let missing = record("missing-direct");
	let mut reader = store.begin();
	assert!(reader.atom(&missing).assert_ok().is_none());

	let mut writer = store.begin();
	writer.create_atom(missing, PropertyMap::new()).assert_ok();
	writer.commit().assert_ok();
	reader.set_base_property(&identity, KEY, Value::from("reader")).assert_ok();

	assert_eq!(reader.commit().assert_err().status().code().to_string(), "FG-40001");
}

/// Verifies serializable graph scan detects an overlay phantom.
#[test]
fn serializable_graph_scan_detects_an_overlay_phantom() {
	let (store, identity, graphs) = setup(1, 0);
	let mut reader = store.begin();
	reader.execute("SELECT VALUE id FROM person IN GRAPH g0;", &Object::new()).assert_ok();

	let mut writer = store.begin();
	writer.set_overlay(graphs[0].id(), &identity, KEY, Value::from("private")).assert_ok();
	writer.commit().assert_ok();
	reader.set_base_property(&identity, KEY, Value::from("reader")).assert_ok();

	assert_eq!(reader.commit().assert_err().status().code().to_string(), "FG-40001");
}

/// Verifies serializable graph scan detects a membership phantom.
#[test]
fn serializable_graph_scan_detects_a_membership_phantom() {
	let (store, identity, graphs) = setup(1, 0);
	let candidate = record("candidate");
	let mut create = store.begin();
	create.create_atom(candidate.clone(), PropertyMap::new()).assert_ok();
	create.commit().assert_ok();

	let mut reader = store.begin();
	reader.execute("SELECT VALUE id FROM person IN GRAPH g0;", &Object::new()).assert_ok();
	let mut writer = store.begin();
	writer.attach_node(graphs[0].id(), &candidate, None, None).assert_ok();
	writer.commit().assert_ok();
	reader.set_base_property(&identity, KEY, Value::from("reader")).assert_ok();

	assert_eq!(reader.commit().assert_err().status().code().to_string(), "FG-40001");
}

/// Verifies serializable graph catalog scan detects a property change.
#[test]
fn serializable_graph_catalog_scan_detects_a_property_change() {
	let (store, identity, _) = setup(0, 1);
	let mut reader = store.begin();
	reader.execute("SELECT name, properties FROM GRAPHS;", &Object::new()).assert_ok();

	let mut writer = store.begin();
	let result = writer
		.execute("UPDATE GRAPH g0 SET properties.owner = 'writer';", &Object::new())
		.assert_ok();
	assert!(result[0].result.is_ok());
	writer.commit().assert_ok();
	reader.set_base_property(&identity, KEY, Value::from("reader")).assert_ok();

	assert_eq!(reader.commit().assert_err().status().code().to_string(), "FG-40001");
}

/// Verifies fanout limit fails before private state changes.
#[test]
fn fanout_limit_fails_before_private_state_changes() {
	let (store, identity, graphs) = setup(2, 0);
	let root = (*store.inner.current_root()).clone();
	let options_defaults = InMemoryOptions::default();
	let options = InMemoryOptions {
		max_snapshot_age: Duration::from_secs(300),
		max_write_set_bytes: BASE_ATOM_WRITE_BYTES + EFFECTIVE_FANOUT_WRITE_BYTES,
		..options_defaults
	};
	let limited = Datastore::from_root_for_test(options, root);
	let mut transaction = limited.begin();
	let error = transaction.set_base_property(&identity, KEY, Value::from("rejected")).assert_err();

	assert_eq!(error.status().code().to_string(), "FG-54F07");
	assert_eq!(transaction.write_set_bytes(), 0);
	assert_eq!(
		transaction.effective_property(graphs[0].id(), &identity, KEY).assert_ok(),
		Some(Value::from("base"))
	);
}

/// Verifies fanout uses reverse memberships not all graphs.
#[test]
fn fanout_uses_reverse_memberships_not_all_graphs() {
	let (store, identity, graphs) = setup(1, 32);
	let root = (*store.inner.current_root()).clone();
	let options_defaults = InMemoryOptions::default();
	let options = InMemoryOptions {
		max_write_set_bytes: BASE_ATOM_WRITE_BYTES
			+ EFFECTIVE_FANOUT_WRITE_BYTES
			+ std::mem::size_of::<changefeed_journal::CaptureOperation>()
			+ std::mem::size_of::<changefeed_journal::CaptureIntent>(),
		..options_defaults
	};
	let limited = Datastore::from_root_for_test(options, root);
	let mut transaction = limited.begin();

	transaction.set_base_property(&identity, KEY, Value::from("allowed")).assert_ok();
	transaction.commit().assert_ok();
	let mut read = limited.begin();
	assert_eq!(
		read.effective_property(graphs[0].id(), &identity, KEY).assert_ok(),
		Some(Value::from("allowed"))
	);
}

/// Verifies commit rechecks memberships added after base preflight.
#[test]
fn commit_rechecks_memberships_added_after_base_preflight() {
	let (store, identity, graphs) = setup(0, 2);
	let root = (*store.inner.current_root()).clone();
	let options_defaults = InMemoryOptions::default();
	let options = InMemoryOptions {
		max_write_set_bytes: GRAPH_MEMBERSHIP_WRITE_BYTES
			+ GRAPH_INCIDENCE_WRITE_BYTES
			+ std::mem::size_of::<changefeed_journal::CaptureOperation>()
			+ std::mem::size_of::<changefeed_journal::CaptureIntent>(),
		..options_defaults
	};
	let limited = Datastore::from_root_for_test(options, root);
	let mut base = limited.begin_with(TransactionOptions::new(
		IsolationLevel::Snapshot,
		TransactionAccess::ReadWrite,
	));
	base.set_base_property(&identity, KEY, Value::from("pending")).assert_ok();

	for graph in &graphs {
		let mut attach = limited.begin();
		attach.attach_node(graph.id(), &identity, None, None).assert_ok();
		attach.commit().assert_ok();
	}
	assert_eq!(base.commit().assert_err().status().code().to_string(), "FG-54F07");
}

/// Verifies disjoint graph memberships rebase snapshot local slots.
#[test]
fn disjoint_graph_memberships_rebase_snapshot_local_slots() {
	let (store, first_record, graphs) = setup(0, 1);
	let second_record = record("second");
	let mut create = store.begin();
	create.create_atom(second_record.clone(), PropertyMap::new()).assert_ok();
	create.commit().assert_ok();
	let options = TransactionOptions::new(IsolationLevel::Snapshot, TransactionAccess::ReadWrite);
	let mut first = store.begin_with(options);
	let mut second = store.begin_with(options);
	let first_local = first.attach_node(graphs[0].id(), &first_record, None, None).assert_ok();
	let second_local = second.attach_node(graphs[0].id(), &second_record, None, None).assert_ok();
	assert_eq!(first_local, second_local);

	first.commit().assert_ok();
	second.commit().assert_ok();
	let mut read = store.begin();
	let first_published = read.membership(graphs[0].id(), &first_record).assert_ok().assert_some();
	let second_published =
		read.membership(graphs[0].id(), &second_record).assert_ok().assert_some();
	assert_ne!(first_published, second_published);
}

/// Verifies rebased edge topology uses published endpoint slots.
#[test]
fn rebased_edge_topology_uses_published_endpoint_slots() {
	let store = Datastore::in_memory();
	let mut seed = store.begin();
	seed.define_class(ClassSchema::new("person", AtomKind::Node, []).assert_ok()).assert_ok();
	seed.define_class(ClassSchema::new("relation", AtomKind::Edge, []).assert_ok()).assert_ok();
	let graph = seed.define_graph("social", GraphKind::Graph).assert_ok();
	let nodes = [record("a1"), record("a2"), record("b1"), record("b2")];
	let edges = [
		RecordId::new("relation", RecordIdPart::String("a".into())).assert_ok(),
		RecordId::new("relation", RecordIdPart::String("b".into())).assert_ok(),
	];
	for identity in nodes.iter().chain(edges.iter()) {
		seed.create_atom(identity.clone(), PropertyMap::new()).assert_ok();
	}
	seed.commit().assert_ok();

	let mut first = store.begin();
	let first_source = first.attach_node(graph.id(), &nodes[0], None, None).assert_ok();
	let first_target = first.attach_node(graph.id(), &nodes[1], None, None).assert_ok();
	first
		.attach_edge(
			graph.id(),
			&edges[0],
			None,
			EdgeTopology::directed([first_source], [first_target]),
			None,
			None,
		)
		.assert_ok();
	let mut second = store.begin();
	let private_source = second.attach_node(graph.id(), &nodes[2], None, None).assert_ok();
	let private_target = second.attach_node(graph.id(), &nodes[3], None, None).assert_ok();
	second
		.attach_edge(
			graph.id(),
			&edges[1],
			None,
			EdgeTopology::directed([private_source], [private_target]),
			None,
			None,
		)
		.assert_ok();

	first.commit().assert_ok();
	second.commit().assert_ok();
	let root = store.inner.current_root();
	let source_global = root.resolve_atom(&nodes[2]).assert_some();
	let target_global = root.resolve_atom(&nodes[3]).assert_some();
	let edge_global = root.resolve_atom(&edges[1]).assert_some();
	let source = root.membership(graph.id(), source_global).assert_some();
	let target = root.membership(graph.id(), target_global).assert_some();
	let edge = root.membership(graph.id(), edge_global).assert_some();
	let topology = match root.occurrence(graph.id(), edge).assert_some().state().view() {
		OccurrenceStateRef::Edge {
			topology,
			..
		} => Some(topology),
		OccurrenceStateRef::Node {
			..
		} => None,
	}
	.assert_some_with("relation occurrence must remain an edge");
	assert_eq!(
		topology.local_binary().map(|(left, right, _direction)| (left, right)),
		Some((source, target))
	);
}

/// Verifies read committed refreshes while snapshot keeps its root.
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
		read_committed.atom(&identity).assert_ok().assert_some().properties().get(KEY),
		Some(&Value::from("base"))
	);

	let mut writer = store.begin();
	writer.set_base_property(&identity, KEY, Value::from("new")).assert_ok();
	writer.commit().assert_ok();

	assert_eq!(
		read_committed.atom(&identity).assert_ok().assert_some().properties().get(KEY),
		Some(&Value::from("new"))
	);
	assert_eq!(
		snapshot.atom(&identity).assert_ok().assert_some().properties().get(KEY),
		Some(&Value::from("base"))
	);
}
