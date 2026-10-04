use std::time::Duration;

use super::*;
use crate::{
	datastore::{
		InMemoryOptions,
		TransactionAccess,
		consts::{GRAPH_INCIDENCE_WRITE_BYTES, GRAPH_MEMBERSHIP_WRITE_BYTES},
	},
	graph::{AtomKind, EdgeTopology, GraphRead, GraphTopologyConstraint, GraphTopologyConstraints},
	values::{Object, RecordIdPart},
};

const KEY: PropertyKeyId = PropertyKeyId::new(7);

#[test]
fn edge_endpoint_postings_publish_abort_replace_and_retire_with_topology() {
	use crate::graph::{EdgeEndpoint, TraversalIndex, TraversalSemantics};
	let store = Datastore::in_memory();
	let mut seed = store.begin();
	seed.define_class(ClassSchema::new("person", AtomKind::Node, []).unwrap()).unwrap();
	seed.define_class(ClassSchema::new("relation", AtomKind::Edge, []).unwrap()).unwrap();
	let graph = seed.define_graph("endpoints", GraphKind::Graph).unwrap();
	let records = ["person:a", "person:b", "relation:base", "relation:connector"].map(|value| value.parse::<RecordId>().unwrap());
	for record in &records { seed.create_atom(record.clone(), PropertyMap::new()).unwrap(); }
	let a = seed.attach_node(graph.id(), &records[0], None, None).unwrap();
	let b = seed.attach_node(graph.id(), &records[1], None, None).unwrap();
	let endpoint = seed.attach_edge(graph.id(), &records[2], None, EdgeTopology::directed([a], [b]), None, None).unwrap();
	seed.commit().unwrap();
	let old_without = store.begin();
	assert!(old_without.snapshot.schema.graph(graph.id()).unwrap().edge_endpoint_index().is_none());
	let mut writer = store.begin();
	let connector = writer.attach_edge(graph.id(), &records[3], None, EdgeTopology::directed([endpoint, a], [endpoint, b]), None, None).unwrap();
	let key = EdgeEndpoint::Local(endpoint);
	assert_eq!(writer.working.graph(graph.id()).unwrap().edge_endpoint_postings(&key).copied().collect::<Vec<_>>(), vec![(connector, false), (connector, true)]);
	writer.commit().unwrap();
	assert!(old_without.snapshot.schema.graph(graph.id()).unwrap().edge_endpoint_index().is_none());
	let mut old = store.begin();
	let capability = old.snapshot.schema.graph(graph.id()).unwrap().edge_endpoint_index().unwrap();
	let view = graph.read(&old).unwrap();
	assert_eq!(view.edge_endpoint_connectors(&key).unwrap(), Some(vec![connector]));
	let exact = TraversalIndex::new(&view, TraversalSemantics::Atom).unwrap();
	let indexed = TraversalIndex::for_edge_endpoint(&view, &key).unwrap().unwrap();
	for direction in [crate::graph::TraversalDirection::Forward, crate::graph::TraversalDirection::Backward, crate::graph::TraversalDirection::Both] {
		assert_eq!(indexed.steps(&key, direction).collect::<Vec<_>>(), exact.steps(&key, direction).collect::<Vec<_>>());
	}
	drop(view);
	let mut private = store.begin();
	private.replace_occurrence_state(graph.id(), &records[3], OccurrenceState::Edge { topology: EdgeTopology::directed([a], [a, b]), weight: None, transition_graph: None }).unwrap();
	assert_eq!(private.working.graph(graph.id()).unwrap().edge_endpoint_postings(&key).count(), 0);
	let rebuilt = crate::stats::analyze_database(&private.working).unwrap();
	assert_eq!(private.working.statistics.graph(graph.id()).unwrap().total_in_degree, rebuilt.graph(graph.id()).unwrap().total_in_degree);
	drop(private);
	assert_eq!(old.snapshot.graph(graph.id()).unwrap().edge_endpoint_postings(&key).count(), 2);
	let mut remove = store.begin();
	remove.detach_occurrence(graph.id(), &records[3], false).unwrap();
	remove.commit().unwrap();
	assert_eq!(store.begin().snapshot.graph(graph.id()).unwrap().edge_endpoint_postings(&key).count(), 0);
	assert_eq!(store.begin().snapshot.schema.graph(graph.id()).unwrap().edge_endpoint_index(), Some(capability));
	assert_eq!(old.graphs().unwrap()[0].edge_endpoint_index(), Some(capability));
}

#[test]
fn edge_endpoint_postings_preserve_two_paths_to_the_same_child_graph() {
	use crate::graph::{EdgeEndpoint, QualifiedAtomId};
	let store = Datastore::in_memory();
	let mut tx = store.begin();
	for (class, kind) in [("person", AtomKind::Node), ("relation", AtomKind::Edge)] { tx.define_class(ClassSchema::new(class, kind, []).unwrap()).unwrap(); }
	let parent = tx.define_graph("parent", GraphKind::Metagraph).unwrap();
	let child = tx.define_graph("child", GraphKind::Graph).unwrap();
	let records = ["person:a", "person:b", "person:c1", "person:c2", "relation:base", "relation:x", "relation:y"].map(|value| value.parse::<RecordId>().unwrap());
	for record in &records { tx.create_atom(record.clone(), PropertyMap::new()).unwrap(); }
	let a = tx.attach_node(child.id(), &records[0], None, None).unwrap();
	let b = tx.attach_node(child.id(), &records[1], None, None).unwrap();
	let endpoint = tx.attach_edge(child.id(), &records[4], None, EdgeTopology::directed([a], [b]), None, None).unwrap();
	let first = tx.attach_node(parent.id(), &records[2], None, Some(child.id())).unwrap();
	let second = tx.attach_node(parent.id(), &records[3], None, Some(child.id())).unwrap();
	let q1 = EdgeEndpoint::Nested(QualifiedAtomId::new(vec![first], endpoint).unwrap());
	let q2 = EdgeEndpoint::Nested(QualifiedAtomId::new(vec![second], endpoint).unwrap());
	let x = tx.attach_edge(parent.id(), &records[5], None, EdgeTopology::directed([q1.clone()], [EdgeEndpoint::Local(second)]), None, None).unwrap();
	let y = tx.attach_edge(parent.id(), &records[6], None, EdgeTopology::directed([q2.clone()], [EdgeEndpoint::Local(first)]), None, None).unwrap();
	tx.commit().unwrap();
	let read = store.begin();
	let view = parent.read(&read).unwrap();
	assert_eq!(view.edge_endpoint_connectors(&q1).unwrap(), Some(vec![x]));
	assert_eq!(view.edge_endpoint_connectors(&q2).unwrap(), Some(vec![y]));
}

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
fn commit_rejects_a_direct_rust_mutation_that_breaks_simple() {
	let store = Datastore::in_memory();
	let mut transaction = store.begin();
	transaction.define_class(ClassSchema::new("person", AtomKind::Node, []).unwrap()).unwrap();
	transaction.define_class(ClassSchema::new("relation", AtomKind::Edge, []).unwrap()).unwrap();
	let node = record("loop");
	let edge = RecordId::new("relation", RecordIdPart::String("self".into())).unwrap();
	transaction.create_atom(node.clone(), PropertyMap::new()).unwrap();
	transaction.create_atom(edge.clone(), PropertyMap::new()).unwrap();
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
		.unwrap();
	let local = transaction.attach_node(graph.id(), &node, None, None).unwrap();
	transaction
		.attach_edge(graph.id(), &edge, None, EdgeTopology::directed([local], [local]), None, None)
		.unwrap();

	assert!(transaction.commit().is_err());
}

#[test]
fn commit_rejects_direct_rust_family_and_root_violations() {
	let store = Datastore::in_memory();
	let mut transaction = store.begin();
	transaction.define_class(ClassSchema::new("person", AtomKind::Node, []).unwrap()).unwrap();
	transaction.define_class(ClassSchema::new("relation", AtomKind::Edge, []).unwrap()).unwrap();
	let left = record("left");
	let right = record("right");
	let forward = RecordId::new("relation", RecordIdPart::String("forward".into())).unwrap();
	let reverse = RecordId::new("relation", RecordIdPart::String("reverse".into())).unwrap();
	for atom in [&left, &right, &forward, &reverse] {
		transaction.create_atom(atom.clone(), PropertyMap::new()).unwrap();
	}
	let dag = transaction.define_graph("dag", GraphKind::Dag).unwrap();
	let local_left = transaction.attach_node(dag.id(), &left, None, None).unwrap();
	let local_right = transaction.attach_node(dag.id(), &right, None, None).unwrap();
	transaction
		.attach_edge(
			dag.id(),
			&forward,
			None,
			EdgeTopology::directed([local_left], [local_right]),
			None,
			None,
		)
		.unwrap();
	transaction
		.attach_edge(
			dag.id(),
			&reverse,
			None,
			EdgeTopology::directed([local_right], [local_left]),
			None,
			None,
		)
		.unwrap();
	assert!(transaction.commit().is_err());

	let store = Datastore::in_memory();
	let mut transaction = store.begin();
	transaction.define_class(ClassSchema::new("person", AtomKind::Node, []).unwrap()).unwrap();
	transaction.define_class(ClassSchema::new("relation", AtomKind::Edge, []).unwrap()).unwrap();
	let left = record("left");
	let right = record("right");
	let edge = RecordId::new("relation", RecordIdPart::String("edge".into())).unwrap();
	for atom in [&left, &right, &edge] {
		transaction.create_atom(atom.clone(), PropertyMap::new()).unwrap();
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
		.unwrap();
	let local_left = transaction.attach_node(rooted.id(), &left, None, None).unwrap();
	let local_right = transaction.attach_node(rooted.id(), &right, None, None).unwrap();
	transaction
		.attach_edge(
			rooted.id(),
			&edge,
			None,
			EdgeTopology::directed([local_left], [local_right]),
			None,
			None,
		)
		.unwrap();
	assert!(transaction.commit().is_err());
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
fn concurrent_create_of_the_same_record_still_conflicts() {
	let store = Datastore::in_memory();
	let mut schema = store.begin();
	schema.define_class(ClassSchema::new("person", AtomKind::Node, []).unwrap()).unwrap();
	schema.commit().unwrap();
	let identity = record("same");
	let mut first = store.begin();
	let mut second = store.begin();
	first.create_atom(identity.clone(), PropertyMap::new()).unwrap();
	second.create_atom(identity, PropertyMap::new()).unwrap();

	first.commit().unwrap();
	assert_eq!(second.commit().unwrap_err().status().code().to_string(), "FG-40001");
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
fn serializable_graph_catalog_scan_detects_a_property_change() {
	let (store, identity, _) = setup(0, 1);
	let mut reader = store.begin();
	reader.execute("SELECT name, properties FROM GRAPHS;", &Object::new()).unwrap();

	let mut writer = store.begin();
	let result =
		writer.execute("UPDATE GRAPH g0 SET properties.owner = 'writer';", &Object::new()).unwrap();
	assert!(result[0].result.is_ok());
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
	options.max_write_set_bytes = BASE_ATOM_WRITE_BYTES + EFFECTIVE_FANOUT_WRITE_BYTES
		+ std::mem::size_of::<changefeed_journal::CaptureOperation>()
		+ std::mem::size_of::<changefeed_journal::CaptureIntent>();
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
	options.max_write_set_bytes = GRAPH_MEMBERSHIP_WRITE_BYTES + GRAPH_INCIDENCE_WRITE_BYTES
		+ std::mem::size_of::<changefeed_journal::CaptureOperation>()
		+ std::mem::size_of::<changefeed_journal::CaptureIntent>();
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
fn disjoint_graph_memberships_rebase_snapshot_local_slots() {
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
	second.commit().unwrap();
	let mut read = store.begin();
	let first_published = read.membership(graphs[0].id(), &first_record).unwrap().unwrap();
	let second_published = read.membership(graphs[0].id(), &second_record).unwrap().unwrap();
	assert_ne!(first_published, second_published);
}

#[test]
fn rebased_edge_topology_uses_published_endpoint_slots() {
	let store = Datastore::in_memory();
	let mut seed = store.begin();
	seed.define_class(ClassSchema::new("person", AtomKind::Node, []).unwrap()).unwrap();
	seed.define_class(ClassSchema::new("relation", AtomKind::Edge, []).unwrap()).unwrap();
	let graph = seed.define_graph("social", GraphKind::Graph).unwrap();
	let nodes = [record("a1"), record("a2"), record("b1"), record("b2")];
	let edges = [
		RecordId::new("relation", RecordIdPart::String("a".into())).unwrap(),
		RecordId::new("relation", RecordIdPart::String("b".into())).unwrap(),
	];
	for identity in nodes.iter().chain(edges.iter()) {
		seed.create_atom(identity.clone(), PropertyMap::new()).unwrap();
	}
	seed.commit().unwrap();

	let mut first = store.begin();
	let first_source = first.attach_node(graph.id(), &nodes[0], None, None).unwrap();
	let first_target = first.attach_node(graph.id(), &nodes[1], None, None).unwrap();
	first
		.attach_edge(
			graph.id(),
			&edges[0],
			None,
			EdgeTopology::directed([first_source], [first_target]),
			None,
			None,
		)
		.unwrap();
	let mut second = store.begin();
	let private_source = second.attach_node(graph.id(), &nodes[2], None, None).unwrap();
	let private_target = second.attach_node(graph.id(), &nodes[3], None, None).unwrap();
	second
		.attach_edge(
			graph.id(),
			&edges[1],
			None,
			EdgeTopology::directed([private_source], [private_target]),
			None,
			None,
		)
		.unwrap();

	first.commit().unwrap();
	second.commit().unwrap();
	let root = store.inner.current_root();
	let source_global = root.resolve_atom(&nodes[2]).unwrap();
	let target_global = root.resolve_atom(&nodes[3]).unwrap();
	let edge_global = root.resolve_atom(&edges[1]).unwrap();
	let source = root.membership(graph.id(), source_global).unwrap();
	let target = root.membership(graph.id(), target_global).unwrap();
	let edge = root.membership(graph.id(), edge_global).unwrap();
	let OccurrenceState::Edge {
		topology,
		..
	} = root.occurrence(graph.id(), edge).unwrap().state()
	else {
		panic!("relation occurrence must remain an edge");
	};
	assert_eq!(
		topology.local_binary().map(|(left, right, _direction)| (left, right)),
		Some((source, target))
	);
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
