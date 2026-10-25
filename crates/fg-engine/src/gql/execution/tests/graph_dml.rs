use std::sync::Arc;

use fg_runtime::__internal::{EdgeEndpointInternal as _, GraphMutableInternal as _};

#[cfg(test)]
use crate::test_assertions::AssertOption as _;
#[cfg(test)]
use crate::test_assertions::AssertResult as _;
use crate::{
	datastore::{ClassSchema, Datastore},
	gql_status::FerrosGrynnResult,
	graph::{
		AtomKind,
		AtomOccurrenceRef,
		AtomRef,
		EdgeEndpoint,
		EdgeRead,
		EdgeTopology,
		Graph,
		GraphKind,
		GraphMut,
		GraphRead,
		GraphStorageMode,
		Mutable,
		NodeRead,
		QualifiedAtomId,
		query_to_memory,
	},
	values::{Object, RecordId, Value},
};

/// Returns graph store for this module.
fn graph_store(kind: GraphKind, names: &[&str]) -> Datastore {
	let store = Datastore::in_memory();
	let mut transaction = store.begin();
	transaction
		.define_class(ClassSchema::new("person", AtomKind::Node, []).assert_ok())
		.assert_ok();
	transaction
		.define_class(ClassSchema::new("friendship", AtomKind::Edge, []).assert_ok())
		.assert_ok();
	for name in names {
		transaction.define_graph(*name, kind).assert_ok();
	}
	for name in ["alice", "bob", "carol"] {
		transaction
			.create_atom(
				format!("person:{name}").parse().assert_ok(),
				crate::values::PropertyMap::default(),
			)
			.assert_ok();
	}
	transaction.commit().assert_ok();
	store
}

/// Verifies relate atomwalk reference endpoints cover all incidence directions.
#[test]
fn relate_atomwalk_reference_endpoints_cover_all_incidence_directions() {
	use crate::test_assertions::AssertResult as _;

	let store = graph_store(GraphKind::Graph, &["scope"]);
	for result in store
		.execute(
			"RELATE (person:alice)-[friendship:base]->(person:bob) IN GRAPH scope; RELATE \
			 (person:bob)-[friendship:other]->(person:carol) IN GRAPH scope;",
			&Object::new(),
		)
		.assert_ok()
	{
		result.result.assert_ok();
	}
	let inputs = Object::from_iter([
		("node", Value::Record(Box::new("person:alice".parse().assert_ok()))),
		(
			"edge",
			Value::Atom(Box::new(AtomRef::new(
				AtomKind::Edge,
				"friendship:base".parse().assert_ok(),
			))),
		),
		("other", Value::Record(Box::new("friendship:other".parse().assert_ok()))),
	]);
	for query in [
		"RELATE ATOM <($node)>~[friendship:vv]~><(person:carol)> IN GRAPH scope;",
		"RELATE ATOM <($node)><~[friendship:ve]~<[$edge]> IN GRAPH scope;",
		"RELATE ATOM <[$edge]>~[friendship:ev]~<($node)> IN GRAPH scope;",
		"RELATE ATOM <[$edge]><~[friendship:ee]~><[$other]> IN GRAPH scope;",
	] {
		for result in store.execute(query, &inputs).assert_ok() {
			result.result.assert_ok();
		}
	}
	let mut read = store.begin();
	let graph = read
		.named_graph("scope")
		.assert_ok()
		.ok_or_else(|| crate::gql_status::FerrosGrynnError::invalid_argument("missing scope"))
		.assert_ok();
	let view = graph.read(&read).assert_ok();
	for (record, direction, kinds) in [
		("friendship:vv", crate::graph::EdgeDirection::Directed, [AtomKind::Node, AtomKind::Node]),
		("friendship:ve", crate::graph::EdgeDirection::Directed, [AtomKind::Edge, AtomKind::Node]),
		("friendship:ev", crate::graph::EdgeDirection::Undirected, [
			AtomKind::Edge,
			AtomKind::Node,
		]),
		("friendship:ee", crate::graph::EdgeDirection::Bidirectional, [
			AtomKind::Edge,
			AtomKind::Edge,
		]),
	] {
		let id = view
			.atom_id(&AtomRef::new(AtomKind::Edge, record.parse().assert_ok()))
			.assert_ok()
			.ok_or_else(|| {
				crate::gql_status::FerrosGrynnError::invalid_argument("missing connector")
			})
			.assert_ok();
		let edge = view
			.edge(id)
			.assert_ok()
			.ok_or_else(|| {
				crate::gql_status::FerrosGrynnError::invalid_argument("missing connector edge")
			})
			.assert_ok();
		let topology = edge.topology().assert_ok();
		assert_eq!(topology.direction(), direction);
		assert_eq!(
			topology.endpoints().map(|endpoint| endpoint.kind()).collect::<Vec<_>>(),
			kinds.map(Some)
		);
	}
}

/// Verifies relate reference idioms and bare numeric records preserve key
/// semantics.
#[test]
fn relate_reference_idioms_and_bare_numeric_records_preserve_key_semantics() {
	use crate::test_assertions::AssertResult as _;

	let store = graph_store(GraphKind::Graph, &["scope"]);
	for result in store.execute("CREATE person:1; CREATE person:2;", &Object::new()).assert_ok() {
		result.result.assert_ok();
	}
	let inputs = Object::from_iter([(
		"source",
		Value::Object(Object::from_iter([(
			"endpoint",
			Value::Record(Box::new("person:alice".parse().assert_ok())),
		)])),
	)]);
	for result in store
		.execute(
			"RELATE person:1-[friendship:numeric]->person:2 IN GRAPH scope; RELATE \
			 ($source.endpoint)-[friendship:idiom]->(person:bob) IN GRAPH scope;",
			&inputs,
		)
		.assert_ok()
	{
		result.result.assert_ok();
	}
	assert!(store.begin().atom(&"friendship:numeric".parse().assert_ok()).assert_ok().is_some());
	assert!(store.begin().atom(&"friendship:idiom".parse().assert_ok()).assert_ok().is_some());
}

/// Verifies relate qualified references reject wrong routes kinds and
/// unattached edges.
#[test]
fn relate_qualified_references_reject_wrong_routes_kinds_and_unattached_edges() {
	use crate::test_assertions::AssertResult as _;

	let store = graph_store(GraphKind::Metagraph, &["parent", "child", "other"]);
	let mut seed = store.begin();
	let parent = seed
		.named_graph("parent")
		.assert_ok()
		.ok_or_else(|| crate::gql_status::FerrosGrynnError::invalid_argument("missing parent"))
		.assert_ok();
	let child = seed
		.named_graph("child")
		.assert_ok()
		.ok_or_else(|| crate::gql_status::FerrosGrynnError::invalid_argument("missing child"))
		.assert_ok();
	seed.attach_node(parent.id(), &"person:bob".parse().assert_ok(), None, Some(child.id()))
		.assert_ok();
	seed.attach_node(parent.id(), &"person:carol".parse().assert_ok(), None, None).assert_ok();
	seed.attach_node(child.id(), &"person:alice".parse().assert_ok(), None, None).assert_ok();
	seed.create_atom(
		"friendship:unattached".parse().assert_ok(),
		crate::values::PropertyMap::default(),
	)
	.assert_ok();
	seed.commit().assert_ok();
	let occurrence = |record: &str, graph: &crate::values::GraphRef| -> FerrosGrynnResult<Value> {
		Ok(Value::AtomOccurrence(Box::new(AtomOccurrenceRef::new(
			AtomRef::new(AtomKind::Node, record.parse()?),
			Arc::new(graph.clone()),
		))))
	};
	let terminal = occurrence("person:alice", &child).assert_ok();
	let owner = occurrence("person:bob", &parent).assert_ok();
	let descriptor = |owners: Vec<Value>| {
		Value::Object(Object::from_iter([
			("occurrence", terminal.clone()),
			("owners", Value::from(owners)),
		]))
	};
	for result in store
		.execute(
			"RELATE ATOM <($endpoint)>~[friendship:valid]~><(person:carol)> IN GRAPH parent;",
			&Object::from_iter([("endpoint", descriptor(vec![owner.clone()]))]),
		)
		.assert_ok()
	{
		result.result.assert_ok();
	}
	let malformed = [
		terminal.clone(),
		descriptor(vec![]),
		descriptor(vec![occurrence("person:carol", &parent).assert_ok()]),
		descriptor(vec![owner.clone(); 65]),
		Value::Record(Box::new("friendship:unattached".parse().assert_ok())),
	];
	for endpoint in malformed {
		let failed = match store.execute(
			"RELATE ATOM <($endpoint)>~[friendship:bad]~><(person:carol)> IN GRAPH parent;",
			&Object::from_iter([("endpoint", endpoint)]),
		) {
			Err(_error) => true,
			Ok(results) => results.iter().any(|result| result.result.is_err()),
		};
		assert!(failed);
		assert!(store.begin().atom(&"friendship:bad".parse().assert_ok()).assert_ok().is_none());
		assert!(
			store
				.begin()
				.membership(parent.id(), &"person:alice".parse().assert_ok())
				.assert_ok()
				.is_none()
		);
	}
	let failed = match store.execute(
		"RELATE ATOM <[$endpoint]>~[friendship:bad]~><(person:carol)> IN GRAPH parent;",
		&Object::from_iter([("endpoint", descriptor(vec![owner]))]),
	) {
		Err(_error) => true,
		Ok(results) => results.iter().any(|result| result.result.is_err()),
	};
	assert!(failed);
	assert!(store.begin().atom(&"friendship:bad".parse().assert_ok()).assert_ok().is_none());
}

/// Verifies relate reuses one edge with graph local topology label and weight.
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
		.assert_ok();
	assert!(results.iter().all(|result| result.result.is_ok()));
	assert!(
		transaction
			.atom(&"friendship:shared".parse::<RecordId>().assert_ok())
			.assert_ok()
			.is_some()
	);

	let snapshot = transaction.query_snapshot().assert_ok();
	let first = transaction.query_named_graph(&snapshot, "first").assert_ok();
	let second = transaction.query_named_graph(&snapshot, "second").assert_ok();
	let first = transaction.query_graph(&snapshot, &first).assert_ok();
	let second = transaction.query_graph(&snapshot, &second).assert_ok();
	let first_edge = first
		.edge(first.edge_ids().assert_ok().next().assert_some().assert_ok())
		.assert_ok()
		.assert_some();
	let second_edge = second
		.edge(second.edge_ids().assert_ok().next().assert_some().assert_ok())
		.assert_ok()
		.assert_some();
	assert_eq!(first_edge.label().assert_ok().as_deref(), Some("First"));
	assert_eq!(second_edge.label().assert_ok().as_deref(), Some("Second"));
	assert_ne!(first_edge.topology().assert_ok(), second_edge.topology().assert_ok());
	assert_eq!(first_edge.weight().assert_some().to_string(), "1");
	assert_eq!(second_edge.weight().assert_some().to_string(), "2");
}

/// Verifies detach restrict rolls back and cascade keeps global edge.
#[test]
fn detach_restrict_rolls_back_and_cascade_keeps_global_edge() {
	let store = graph_store(GraphKind::Graph, &["social"]);
	let edge = "friendship:shared".parse::<RecordId>().assert_ok();
	let alice = "person:alice".parse::<RecordId>().assert_ok();
	let mut transaction = store.begin();
	let related = transaction
		.execute(
			"RELATE (person:alice)-[friendship:shared]->(person:bob) IN GRAPH social;",
			&Object::new(),
		)
		.assert_ok();
	assert!(related[0].result.is_ok());
	transaction.commit().assert_ok();

	let mut transaction = store.begin();
	let restricted =
		transaction.execute("DETACH person:alice FROM GRAPH social;", &Object::new()).assert_ok();
	assert!(restricted[0].result.is_err());
	assert!(transaction.query_snapshot().is_err());

	let mut transaction = store.begin();
	let snapshot = transaction.query_snapshot().assert_ok();
	let graph = transaction.query_named_graph(&snapshot, "social").assert_ok();
	assert!(transaction.membership(graph.id(), &alice).assert_ok().is_some());

	let detached = transaction
		.execute("DETACH person:alice FROM GRAPH social CASCADE;", &Object::new())
		.assert_ok();
	assert!(detached[0].result.is_ok());
	assert!(transaction.membership(graph.id(), &alice).assert_ok().is_none());
	assert!(transaction.membership(graph.id(), &edge).assert_ok().is_none());
	assert!(transaction.atom(&edge).assert_ok().is_some());
	transaction.commit().assert_ok();
}

/// Verifies dag cycle failure removes the statement private edge.
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
		.assert_ok();
	assert!(initial.iter().all(|result| result.result.is_ok()));
	transaction.commit().assert_ok();

	let mut transaction = store.begin();
	let cycle = transaction
		.execute(
			"RELATE (person:carol)-[friendship:cycle]->(person:alice) IN GRAPH flow;",
			&Object::new(),
		)
		.assert_ok();
	assert!(cycle[0].result.is_err());
	assert!(transaction.atom(&"friendship:cycle".parse::<RecordId>().assert_ok()).is_err());
	assert!(
		store
			.begin()
			.atom(&"friendship:cycle".parse::<RecordId>().assert_ok())
			.assert_ok()
			.is_none()
	);
}

/// Verifies query to memory owns effective graph after source transaction is
/// dropped.
#[test]
fn query_to_memory_owns_effective_graph_after_source_transaction_is_dropped() {
	let store = graph_store(GraphKind::Graph, &["social"]);
	let alice = "person:alice".parse::<RecordId>().assert_ok();
	let mut transaction = store.begin();
	let name = transaction.intern_property("name").assert_ok();
	let owner = transaction.intern_property("owner").assert_ok();
	let mut graph_properties = crate::values::PropertyMap::new();
	graph_properties.insert(owner, "team");
	transaction.replace_graph_properties("social", graph_properties).assert_ok();
	transaction.set_base_property(&alice, name, Value::from("Base")).assert_ok();
	let snapshot = transaction.query_snapshot().assert_ok();
	let graph = transaction.query_named_graph(&snapshot, "social").assert_ok();
	transaction.attach_node(graph.id(), &alice, None, None).assert_ok();
	transaction.set_overlay(graph.id(), &alice, name, Value::from("Effective")).assert_ok();
	let view = graph.read(&transaction).assert_ok();
	let materialized = query_to_memory::<Graph<Mutable>, _>(&view).assert_ok();
	drop(view);
	drop(transaction);

	assert_eq!(materialized.storage_mode(), GraphStorageMode::Hybrid);
	assert_eq!(materialized.graph_ref(), Some(&graph));
	assert_eq!(materialized.properties().get(owner), Some(&Value::from("team")));
	let node = materialized
		.node_ids()
		.assert_ok()
		.find_map(|id| {
			let id = id.assert_ok();
			let node = materialized.node(id).assert_some();
			(node.record_id() == &alice).then_some(node)
		})
		.assert_some();
	assert_eq!(node.properties().get(name), Some(&Value::from("Effective")));
}

/// Verifies mutable query to memory flushes only graph local overlay.
#[test]
fn mutable_query_to_memory_flushes_only_graph_local_overlay() {
	let store = graph_store(GraphKind::Graph, &["social"]);
	let alice = "person:alice".parse::<RecordId>().assert_ok();
	let mut transaction = store.begin();
	let name = transaction.intern_property("name").assert_ok();
	transaction.set_base_property(&alice, name, Value::from("Base")).assert_ok();
	let snapshot = transaction.query_snapshot().assert_ok();
	let graph = transaction.query_named_graph(&snapshot, "social").assert_ok();
	transaction.attach_node(graph.id(), &alice, None, None).assert_ok();
	let view = graph.read(&transaction).assert_ok();
	let mut materialized = query_to_memory::<Graph<Mutable>, _>(&view).assert_ok();
	drop(view);
	let local = materialized.node_ids().assert_ok().next().assert_some().assert_ok();
	materialized.node_properties_mut(local).assert_ok().insert(name, "Graph local");
	materialized.set_node_label(local, Some("Selected".into())).assert_ok();
	materialized.flush(&mut transaction).assert_ok();

	assert_eq!(
		transaction.atom(&alice).assert_ok().assert_some().properties().get(name),
		Some(&Value::from("Base"))
	);
	assert_eq!(
		transaction.effective_property(graph.id(), &alice, name).assert_ok(),
		Some(Value::from("Graph local"))
	);
	let refreshed = graph.read(&transaction).assert_ok();
	let node = refreshed
		.node(refreshed.node_ids().assert_ok().next().assert_some().assert_ok())
		.assert_ok()
		.assert_some();
	assert_eq!(node.label().assert_ok().as_deref(), Some("Selected"));
}

/// Verifies mutable query to memory flushes graph local edge topology.
#[test]
#[allow(
	clippy::mutable_key_type,
	reason = "Runtime value and reference keys retain stable logical Eq, Ord and Hash identities"
)]
fn mutable_query_to_memory_flushes_graph_local_edge_topology() {
	let store = graph_store(GraphKind::Graph, &["social"]);
	let mut transaction = store.begin();
	let results = transaction
		.execute(
			"RELATE (person:alice)-[friendship:shared]->(person:bob) IN GRAPH social;",
			&Object::new(),
		)
		.assert_ok();
	assert!(results[0].result.is_ok());
	let snapshot = transaction.query_snapshot().assert_ok();
	let graph = transaction.query_named_graph(&snapshot, "social").assert_ok();
	let view = graph.read(&transaction).assert_ok();
	let mut materialized = query_to_memory::<Graph<Mutable>, _>(&view).assert_ok();
	drop(view);
	let mut nodes = materialized
		.node_ids()
		.assert_ok()
		.map(Result::unwrap)
		.map(|id| (materialized.node(id).assert_some().record_id().clone(), id))
		.collect::<std::collections::BTreeMap<_, _>>();
	let alice = nodes.remove(&"person:alice".parse().assert_ok()).assert_some();
	let carol = materialized
		.add_node_with_id(
			"person:carol".parse().assert_ok(),
			None,
			crate::values::PropertyMap::default(),
		)
		.assert_ok();
	let edge = materialized.edge_ids().assert_ok().next().assert_some().assert_ok();
	materialized
		.replace_edge_topology(edge, crate::graph::EdgeTopology::directed([carol], [alice]))
		.assert_ok();
	materialized.flush(&mut transaction).assert_ok();

	let refreshed = graph.read(&transaction).assert_ok();
	let edge = refreshed.edge_ids().assert_ok().next().assert_some().assert_ok();
	let edge = refreshed.edge(edge).assert_ok().assert_some();
	let topology = edge.topology().assert_ok();
	let endpoints = topology
		.endpoints()
		.filter_map(crate::graph::EdgeEndpoint::local_id)
		.map(|id| refreshed.node(id).assert_ok().assert_some().record_id().assert_ok().into_owned())
		.collect::<Vec<_>>();
	assert_eq!(endpoints, ["person:carol".parse().assert_ok(), "person:alice".parse().assert_ok()]);
}

/// Verifies levelpath is read only until graph dml persists it.
#[test]
fn levelpath_is_read_only_until_graph_dml_persists_it() {
	use crate::test_assertions::AssertResult as _;

	let store = Datastore::in_memory();
	let mut seed = store.begin();
	seed.define_class(ClassSchema::new("module", AtomKind::Node, []).assert_ok()).assert_ok();
	seed.define_class(ClassSchema::new("document", AtomKind::Node, []).assert_ok()).assert_ok();
	let child = seed.define_graph("level_child", GraphKind::Graph).assert_ok();
	let root = seed.define_graph("level_root", GraphKind::Metagraph).assert_ok();
	let owner = "module:owner".parse::<RecordId>().assert_ok();
	let leaf = "document:leaf".parse::<RecordId>().assert_ok();
	seed.create_atom(owner.clone(), crate::values::PropertyMap::default()).assert_ok();
	seed.create_atom(leaf.clone(), crate::values::PropertyMap::default()).assert_ok();
	seed.attach_node(child.id(), &leaf, None, None).assert_ok();
	seed.attach_node(root.id(), &owner, None, Some(child.id())).assert_ok();
	seed.commit().assert_ok();

	let mut transaction = store.begin();
	let snapshot = transaction.query_snapshot().assert_ok();
	let occurrence =
		AtomOccurrenceRef::new(AtomRef::new(AtomKind::Node, leaf.clone()), Arc::new(child.clone()));
	let computed = transaction.query_level_path(&snapshot, &occurrence).assert_ok();
	assert!(transaction.query_property_dictionary().get("levelpath").is_none());

	let result = transaction
		.execute(
			"UPDATE document:leaf IN GRAPH level_child KEEP LEVELPATH RETURN AFTER",
			&Object::new(),
		)
		.assert_ok();
	assert!(result[0].result.is_ok());
	let key = transaction
		.query_property_dictionary()
		.get("levelpath")
		.ok_or_else(|| crate::gql_status::FerrosGrynnError::invalid_argument("missing levelpath"))
		.assert_ok();
	assert_eq!(transaction.effective_property(child.id(), &leaf, key).assert_ok(), Some(computed),);

	let direct = store
		.begin()
		.execute("UPDATE document:leaf IN GRAPH level_child SET levelpath = {}", &Object::new())
		.assert_ok();
	assert!(direct[0].result.is_err());
}

/// Verifies levelpath covers nested binary edges and rejects hyperedges.
#[test]
fn levelpath_covers_nested_binary_edges_and_rejects_hyperedges() {
	use crate::test_assertions::AssertResult as _;

	let store = Datastore::in_memory();
	let mut transaction = store.begin();
	transaction
		.define_class(ClassSchema::new("module", AtomKind::Node, []).assert_ok())
		.assert_ok();
	transaction
		.define_class(ClassSchema::new("document", AtomKind::Node, []).assert_ok())
		.assert_ok();
	transaction
		.define_class(ClassSchema::new("bridge", AtomKind::Edge, []).assert_ok())
		.assert_ok();
	let child = transaction.define_graph("level_nested_child", GraphKind::Graph).assert_ok();
	let meta = transaction.define_graph("level_nested_meta", GraphKind::Metagraph).assert_ok();
	let owner = "module:owner".parse::<RecordId>().assert_ok();
	let leaf = "document:leaf".parse::<RecordId>().assert_ok();
	let bridge = "bridge:nested".parse::<RecordId>().assert_ok();
	let hyper = "bridge:hyper".parse::<RecordId>().assert_ok();
	for record in [&owner, &leaf, &bridge, &hyper] {
		transaction.create_atom(record.clone(), crate::values::PropertyMap::default()).assert_ok();
	}
	let leaf_local = transaction.attach_node(child.id(), &leaf, None, None).assert_ok();
	let owner_local =
		transaction.attach_node(meta.id(), &owner, None, Some(child.id())).assert_ok();
	transaction
		.attach_edge(
			meta.id(),
			&bridge,
			None,
			EdgeTopology::directed([EdgeEndpoint::Local(owner_local)], [EdgeEndpoint::Nested(
				QualifiedAtomId::new([owner_local], leaf_local).assert_ok(),
			)]),
			None,
			None,
		)
		.assert_ok();
	let stored = transaction.store_level_path(meta.id(), &bridge).assert_ok();
	let edge = stored
		.as_object()
		.ok_or_else(|| {
			crate::gql_status::FerrosGrynnError::invalid_argument(
				"LevelPath edge must be an object",
			)
		})
		.assert_ok();
	assert_eq!(edge.get("kind"), Some(&Value::from("binary_edge")));
	let target = edge
		.get("target")
		.and_then(Value::as_object)
		.ok_or_else(|| {
			crate::gql_status::FerrosGrynnError::invalid_argument(
				"LevelPath target must be an object",
			)
		})
		.assert_ok();
	let chains = target
		.get("chains")
		.and_then(Value::as_array)
		.ok_or_else(|| {
			crate::gql_status::FerrosGrynnError::invalid_argument(
				"LevelPath chains must be an array",
			)
		})
		.assert_ok();
	assert_eq!(chains.len(), 1);
	assert_eq!(chains.get(0).and_then(Value::as_array).map(|chain| chain.len()), Some(1));

	transaction
		.attach_edge(
			meta.id(),
			&hyper,
			None,
			EdgeTopology::directed([EdgeEndpoint::Local(owner_local)], [
				EdgeEndpoint::Local(owner_local),
				EdgeEndpoint::Nested(QualifiedAtomId::new([owner_local], leaf_local).assert_ok()),
			]),
			None,
			None,
		)
		.assert_ok();
	assert!(transaction.store_level_path(meta.id(), &hyper).is_err());
}

/// Verifies detach drops stored levelpath with the occurrence.
#[test]
fn detach_drops_stored_levelpath_with_the_occurrence() {
	use crate::test_assertions::AssertResult as _;

	let store = Datastore::in_memory();
	let mut transaction = store.begin();
	transaction
		.define_class(ClassSchema::new("document", AtomKind::Node, []).assert_ok())
		.assert_ok();
	let graph = transaction.define_graph("level_detach", GraphKind::Graph).assert_ok();
	let record = "document:leaf".parse::<RecordId>().assert_ok();
	transaction.create_atom(record.clone(), crate::values::PropertyMap::default()).assert_ok();
	transaction.attach_node(graph.id(), &record, None, None).assert_ok();
	transaction.store_level_path(graph.id(), &record).assert_ok();
	let key = transaction
		.query_property_dictionary()
		.get("levelpath")
		.ok_or_else(|| crate::gql_status::FerrosGrynnError::invalid_argument("missing levelpath"))
		.assert_ok();
	assert!(transaction.effective_property(graph.id(), &record, key).assert_ok().is_some());
	transaction.detach_occurrence(graph.id(), &record, false).assert_ok();
	transaction.attach_node(graph.id(), &record, None, None).assert_ok();
	assert_eq!(transaction.effective_property(graph.id(), &record, key).assert_ok(), None);
}
