use crate::{
	datastore::{ClassSchema, Datastore},
	graph::{
		AtomKind,
		EdgeRead,
		Graph,
		GraphKind,
		GraphMut,
		GraphRead,
		GraphStorageMode,
		Mutable,
		NodeRead,
		query_to_memory,
	},
	values::{Object, RecordId, Value},
};

fn graph_store(kind: GraphKind, names: &[&str]) -> Datastore {
	let store = Datastore::in_memory();
	let mut transaction = store.begin();
	transaction.define_class(ClassSchema::new("person", AtomKind::Node, []).unwrap()).unwrap();
	transaction.define_class(ClassSchema::new("friendship", AtomKind::Edge, []).unwrap()).unwrap();
	for name in names {
		transaction.define_graph(*name, kind).unwrap();
	}
	for name in ["alice", "bob", "carol"] {
		transaction
			.create_atom(format!("person:{name}").parse().unwrap(), Default::default())
			.unwrap();
	}
	transaction.commit().unwrap();
	store
}

#[test]
fn relate_reuses_one_edge_with_graph_local_topology_label_and_weight() {
	let store = graph_store(GraphKind::Graph, &["first", "second"]);
	let mut transaction = store.begin();
	let results = transaction
		.execute(
			"RELATE (person:alice)-[friendship:shared LABEL First]->(person:bob) IN GRAPH first \
			 WEIGHTED 1; RELATE (person:bob)-[friendship:shared LABEL Second]-(person:carol) IN \
			 GRAPH second WEIGHTED 2;",
			&Object::new(),
		)
		.unwrap();
	assert!(results.iter().all(|result| result.result.is_ok()));
	assert!(transaction.atom(&"friendship:shared".parse::<RecordId>().unwrap()).unwrap().is_some());

	let snapshot = transaction.query_snapshot().unwrap();
	let first = transaction.query_named_graph(&snapshot, "first").unwrap();
	let second = transaction.query_named_graph(&snapshot, "second").unwrap();
	let first = transaction.query_graph(&snapshot, &first).unwrap();
	let second = transaction.query_graph(&snapshot, &second).unwrap();
	let first_edge =
		first.edge(first.edge_ids().unwrap().next().unwrap().unwrap()).unwrap().unwrap();
	let second_edge =
		second.edge(second.edge_ids().unwrap().next().unwrap().unwrap()).unwrap().unwrap();
	assert_eq!(first_edge.label().unwrap().as_deref(), Some("First"));
	assert_eq!(second_edge.label().unwrap().as_deref(), Some("Second"));
	assert_ne!(first_edge.topology().unwrap(), second_edge.topology().unwrap());
	assert_eq!(first_edge.weight().unwrap().to_string(), "1");
	assert_eq!(second_edge.weight().unwrap().to_string(), "2");
}

#[test]
fn detach_restrict_rolls_back_and_cascade_keeps_global_edge() {
	let store = graph_store(GraphKind::Graph, &["social"]);
	let edge = "friendship:shared".parse::<RecordId>().unwrap();
	let alice = "person:alice".parse::<RecordId>().unwrap();
	let mut transaction = store.begin();
	let related = transaction
		.execute(
			"RELATE (person:alice)-[friendship:shared]->(person:bob) IN GRAPH social;",
			&Object::new(),
		)
		.unwrap();
	assert!(related[0].result.is_ok());
	transaction.commit().unwrap();

	let mut transaction = store.begin();
	let restricted =
		transaction.execute("DETACH person:alice FROM GRAPH social;", &Object::new()).unwrap();
	assert!(restricted[0].result.is_err());
	assert!(transaction.query_snapshot().is_err());

	let mut transaction = store.begin();
	let snapshot = transaction.query_snapshot().unwrap();
	let graph = transaction.query_named_graph(&snapshot, "social").unwrap();
	assert!(transaction.membership(graph.id(), &alice).unwrap().is_some());

	let detached = transaction
		.execute("DETACH person:alice FROM GRAPH social CASCADE;", &Object::new())
		.unwrap();
	assert!(detached[0].result.is_ok());
	assert!(transaction.membership(graph.id(), &alice).unwrap().is_none());
	assert!(transaction.membership(graph.id(), &edge).unwrap().is_none());
	assert!(transaction.atom(&edge).unwrap().is_some());
	transaction.commit().unwrap();
}

#[test]
fn dag_cycle_failure_removes_the_statement_private_edge() {
	let store = graph_store(GraphKind::Dag, &["flow"]);
	let mut transaction = store.begin();
	let initial = transaction
		.execute(
			"RELATE (person:alice)-[friendship:ab]->(person:bob) IN GRAPH flow; RELATE \
			 (person:bob)-[friendship:bc]->(person:carol) IN GRAPH flow;",
			&Object::new(),
		)
		.unwrap();
	assert!(initial.iter().all(|result| result.result.is_ok()));
	transaction.commit().unwrap();

	let mut transaction = store.begin();
	let cycle = transaction
		.execute(
			"RELATE (person:carol)-[friendship:cycle]->(person:alice) IN GRAPH flow;",
			&Object::new(),
		)
		.unwrap();
	assert!(cycle[0].result.is_err());
	assert!(transaction.atom(&"friendship:cycle".parse::<RecordId>().unwrap()).is_err());
	assert!(
		store.begin().atom(&"friendship:cycle".parse::<RecordId>().unwrap()).unwrap().is_none()
	);
}

#[test]
fn query_to_memory_owns_effective_graph_after_source_transaction_is_dropped() {
	let store = graph_store(GraphKind::Graph, &["social"]);
	let alice = "person:alice".parse::<RecordId>().unwrap();
	let mut transaction = store.begin();
	let name = transaction.intern_property("name").unwrap();
	let owner = transaction.intern_property("owner").unwrap();
	let mut graph_properties = crate::values::PropertyMap::new();
	graph_properties.insert(owner, "team");
	transaction.replace_graph_properties("social", graph_properties).unwrap();
	transaction.set_base_property(&alice, name, Value::from("Base")).unwrap();
	let snapshot = transaction.query_snapshot().unwrap();
	let graph = transaction.query_named_graph(&snapshot, "social").unwrap();
	transaction.attach_node(graph.id(), &alice, None, None).unwrap();
	transaction.set_overlay(graph.id(), &alice, name, Value::from("Effective")).unwrap();
	let view = graph.read(&transaction).unwrap();
	let materialized = query_to_memory::<Graph<Mutable>, _>(&view).unwrap();
	drop(view);
	drop(transaction);

	assert_eq!(materialized.storage_mode(), GraphStorageMode::Hybrid);
	assert_eq!(materialized.graph_ref(), Some(&graph));
	assert_eq!(materialized.properties().get(owner), Some(&Value::from("team")));
	let node = materialized
		.node_ids()
		.unwrap()
		.find_map(|id| {
			let id = id.unwrap();
			let node = materialized.node(id).unwrap();
			(node.record_id() == &alice).then_some(node)
		})
		.unwrap();
	assert_eq!(node.properties().get(name), Some(&Value::from("Effective")));
}

#[test]
fn mutable_query_to_memory_flushes_only_graph_local_overlay() {
	let store = graph_store(GraphKind::Graph, &["social"]);
	let alice = "person:alice".parse::<RecordId>().unwrap();
	let mut transaction = store.begin();
	let name = transaction.intern_property("name").unwrap();
	transaction.set_base_property(&alice, name, Value::from("Base")).unwrap();
	let snapshot = transaction.query_snapshot().unwrap();
	let graph = transaction.query_named_graph(&snapshot, "social").unwrap();
	transaction.attach_node(graph.id(), &alice, None, None).unwrap();
	let view = graph.read(&transaction).unwrap();
	let mut materialized = query_to_memory::<Graph<Mutable>, _>(&view).unwrap();
	drop(view);
	let local = materialized.node_ids().unwrap().next().unwrap().unwrap();
	materialized.node_properties_mut(local).unwrap().insert(name, "Graph local");
	materialized.set_node_label(local, Some("Selected".into())).unwrap();
	materialized.flush(&mut transaction).unwrap();

	assert_eq!(
		transaction.atom(&alice).unwrap().unwrap().properties().get(name),
		Some(&Value::from("Base"))
	);
	assert_eq!(
		transaction.effective_property(graph.id(), &alice, name).unwrap(),
		Some(Value::from("Graph local"))
	);
	let refreshed = graph.read(&transaction).unwrap();
	let node =
		refreshed.node(refreshed.node_ids().unwrap().next().unwrap().unwrap()).unwrap().unwrap();
	assert_eq!(node.label().unwrap().as_deref(), Some("Selected"));
}

#[test]
fn mutable_query_to_memory_flushes_graph_local_edge_topology() {
	let store = graph_store(GraphKind::Graph, &["social"]);
	let mut transaction = store.begin();
	let results = transaction
		.execute(
			"RELATE (person:alice)-[friendship:shared]->(person:bob) IN GRAPH social;",
			&Object::new(),
		)
		.unwrap();
	assert!(results[0].result.is_ok());
	let snapshot = transaction.query_snapshot().unwrap();
	let graph = transaction.query_named_graph(&snapshot, "social").unwrap();
	let view = graph.read(&transaction).unwrap();
	let mut materialized = query_to_memory::<Graph<Mutable>, _>(&view).unwrap();
	drop(view);
	let mut nodes = materialized
		.node_ids()
		.unwrap()
		.map(Result::unwrap)
		.map(|id| (materialized.node(id).unwrap().record_id().clone(), id))
		.collect::<std::collections::BTreeMap<_, _>>();
	let alice = nodes.remove(&"person:alice".parse().unwrap()).unwrap();
	let carol = materialized
		.add_node_with_id("person:carol".parse().unwrap(), None, Default::default())
		.unwrap();
	let edge = materialized.edge_ids().unwrap().next().unwrap().unwrap();
	materialized
		.replace_edge_topology(edge, crate::graph::EdgeTopology::directed([carol], [alice]))
		.unwrap();
	materialized.flush(&mut transaction).unwrap();

	let refreshed = graph.read(&transaction).unwrap();
	let edge = refreshed.edge_ids().unwrap().next().unwrap().unwrap();
	let edge = refreshed.edge(edge).unwrap().unwrap();
	let topology = edge.topology().unwrap();
	let endpoints = topology
		.endpoints()
		.filter_map(crate::graph::EdgeEndpoint::local_id)
		.map(|id| refreshed.node(id).unwrap().unwrap().record_id().unwrap().into_owned())
		.collect::<Vec<_>>();
	assert_eq!(endpoints, ["person:carol".parse().unwrap(), "person:alice".parse().unwrap()]);
}
