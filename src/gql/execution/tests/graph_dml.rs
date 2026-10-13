use std::sync::Arc;

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
fn relate_atomwalk_reference_endpoints_cover_all_incidence_directions() -> FerrosGrynnResult<()> {
	let store = graph_store(GraphKind::Graph, &["scope"]);
	for result in store.execute(
		"RELATE (person:alice)-[friendship:base]->(person:bob) IN GRAPH scope; RELATE \
		 (person:bob)-[friendship:other]->(person:carol) IN GRAPH scope;",
		&Object::new(),
	)? {
		result.result?;
	}
	let inputs = Object::from_iter([
		("node", Value::Record(Box::new("person:alice".parse()?))),
		("edge", Value::Atom(Box::new(AtomRef::new(AtomKind::Edge, "friendship:base".parse()?)))),
		("other", Value::Record(Box::new("friendship:other".parse()?))),
	]);
	for query in [
		"RELATE ATOM <($node)>~[friendship:vv]~><(person:carol)> IN GRAPH scope;",
		"RELATE ATOM <($node)><~[friendship:ve]~<[$edge]> IN GRAPH scope;",
		"RELATE ATOM <[$edge]>~[friendship:ev]~<($node)> IN GRAPH scope;",
		"RELATE ATOM <[$edge]><~[friendship:ee]~><[$other]> IN GRAPH scope;",
	] {
		for result in store.execute(query, &inputs)? {
			result.result?;
		}
	}
	let mut read = store.begin();
	let graph = read
		.named_graph("scope")?
		.ok_or_else(|| crate::gql_status::FerrosGrynnError::invalid_argument("missing scope"))?;
	let view = graph.read(&read)?;
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
		let id =
			view.atom_id(&AtomRef::new(AtomKind::Edge, record.parse()?))?.ok_or_else(|| {
				crate::gql_status::FerrosGrynnError::invalid_argument("missing connector")
			})?;
		let edge = view.edge(id)?.ok_or_else(|| {
			crate::gql_status::FerrosGrynnError::invalid_argument("missing connector edge")
		})?;
		let topology = edge.topology()?;
		assert_eq!(topology.direction(), direction);
		assert_eq!(
			topology.endpoints().map(|endpoint| endpoint.kind()).collect::<Vec<_>>(),
			kinds.map(Some)
		);
	}
	Ok(())
}

#[test]
fn relate_reference_idioms_and_bare_numeric_records_preserve_key_semantics() -> FerrosGrynnResult<()>
{
	let store = graph_store(GraphKind::Graph, &["scope"]);
	for result in store.execute("CREATE person:1; CREATE person:2;", &Object::new())? {
		result.result?;
	}
	let inputs = Object::from_iter([(
		"source",
		Value::Object(Object::from_iter([(
			"endpoint",
			Value::Record(Box::new("person:alice".parse()?)),
		)])),
	)]);
	for result in store.execute(
		"RELATE person:1-[friendship:numeric]->person:2 IN GRAPH scope; RELATE \
		 ($source.endpoint)-[friendship:idiom]->(person:bob) IN GRAPH scope;",
		&inputs,
	)? {
		result.result?;
	}
	assert!(store.begin().atom(&"friendship:numeric".parse()?)?.is_some());
	assert!(store.begin().atom(&"friendship:idiom".parse()?)?.is_some());
	Ok(())
}

#[test]
fn relate_qualified_references_reject_wrong_routes_kinds_and_unattached_edges()
-> FerrosGrynnResult<()> {
	let store = graph_store(GraphKind::Metagraph, &["parent", "child", "other"]);
	let mut seed = store.begin();
	let parent = seed
		.named_graph("parent")?
		.ok_or_else(|| crate::gql_status::FerrosGrynnError::invalid_argument("missing parent"))?;
	let child = seed
		.named_graph("child")?
		.ok_or_else(|| crate::gql_status::FerrosGrynnError::invalid_argument("missing child"))?;
	seed.attach_node(parent.id(), &"person:bob".parse()?, None, Some(child.id()))?;
	seed.attach_node(parent.id(), &"person:carol".parse()?, None, None)?;
	seed.attach_node(child.id(), &"person:alice".parse()?, None, None)?;
	seed.create_atom("friendship:unattached".parse()?, Default::default())?;
	seed.commit()?;
	let occurrence = |record: &str, graph: &crate::values::GraphRef| -> FerrosGrynnResult<Value> {
		Ok(Value::AtomOccurrence(Box::new(AtomOccurrenceRef::new(
			AtomRef::new(AtomKind::Node, record.parse()?),
			Arc::new(graph.clone()),
		))))
	};
	let terminal = occurrence("person:alice", &child)?;
	let owner = occurrence("person:bob", &parent)?;
	let descriptor = |owners: Vec<Value>| {
		Value::Object(Object::from_iter([
			("occurrence", terminal.clone()),
			("owners", Value::from(owners)),
		]))
	};
	for result in store.execute(
		"RELATE ATOM <($endpoint)>~[friendship:valid]~><(person:carol)> IN GRAPH parent;",
		&Object::from_iter([("endpoint", descriptor(vec![owner.clone()]))]),
	)? {
		result.result?;
	}
	let malformed = [
		terminal.clone(),
		descriptor(vec![]),
		descriptor(vec![occurrence("person:carol", &parent)?]),
		descriptor(vec![owner.clone(); 65]),
		Value::Record(Box::new("friendship:unattached".parse()?)),
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
		assert!(store.begin().atom(&"friendship:bad".parse()?)?.is_none());
		assert!(store.begin().membership(parent.id(), &"person:alice".parse()?)?.is_none());
	}
	let failed = match store.execute(
		"RELATE ATOM <[$endpoint]>~[friendship:bad]~><(person:carol)> IN GRAPH parent;",
		&Object::from_iter([("endpoint", descriptor(vec![owner]))]),
	) {
		Err(_error) => true,
		Ok(results) => results.iter().any(|result| result.result.is_err()),
	};
	assert!(failed);
	assert!(store.begin().atom(&"friendship:bad".parse()?)?.is_none());
	Ok(())
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

#[test]
fn levelpath_is_read_only_until_graph_dml_persists_it() -> FerrosGrynnResult<()> {
	let store = Datastore::in_memory();
	let mut seed = store.begin();
	seed.define_class(ClassSchema::new("module", AtomKind::Node, [])?)?;
	seed.define_class(ClassSchema::new("document", AtomKind::Node, [])?)?;
	let child = seed.define_graph("level_child", GraphKind::Graph)?;
	let root = seed.define_graph("level_root", GraphKind::Metagraph)?;
	let owner = "module:owner".parse::<RecordId>()?;
	let leaf = "document:leaf".parse::<RecordId>()?;
	seed.create_atom(owner.clone(), Default::default())?;
	seed.create_atom(leaf.clone(), Default::default())?;
	seed.attach_node(child.id(), &leaf, None, None)?;
	seed.attach_node(root.id(), &owner, None, Some(child.id()))?;
	seed.commit()?;

	let mut transaction = store.begin();
	let snapshot = transaction.query_snapshot()?;
	let occurrence =
		AtomOccurrenceRef::new(AtomRef::new(AtomKind::Node, leaf.clone()), Arc::new(child.clone()));
	let computed = transaction.query_level_path(&snapshot, &occurrence)?;
	assert!(transaction.query_property_dictionary().get("levelpath").is_none());

	let result = transaction.execute(
		"UPDATE document:leaf IN GRAPH level_child KEEP LEVELPATH RETURN AFTER",
		&Object::new(),
	)?;
	assert!(result[0].result.is_ok());
	let key = transaction.query_property_dictionary().get("levelpath").ok_or_else(|| {
		crate::gql_status::FerrosGrynnError::invalid_argument("missing levelpath")
	})?;
	assert_eq!(transaction.effective_property(child.id(), &leaf, key)?, Some(computed),);

	let direct = store
		.begin()
		.execute("UPDATE document:leaf IN GRAPH level_child SET levelpath = {}", &Object::new())?;
	assert!(direct[0].result.is_err());
	Ok(())
}

#[test]
fn levelpath_covers_nested_binary_edges_and_rejects_hyperedges() -> FerrosGrynnResult<()> {
	let store = Datastore::in_memory();
	let mut transaction = store.begin();
	transaction.define_class(ClassSchema::new("module", AtomKind::Node, [])?)?;
	transaction.define_class(ClassSchema::new("document", AtomKind::Node, [])?)?;
	transaction.define_class(ClassSchema::new("bridge", AtomKind::Edge, [])?)?;
	let child = transaction.define_graph("level_nested_child", GraphKind::Graph)?;
	let meta = transaction.define_graph("level_nested_meta", GraphKind::Metagraph)?;
	let owner = "module:owner".parse::<RecordId>()?;
	let leaf = "document:leaf".parse::<RecordId>()?;
	let bridge = "bridge:nested".parse::<RecordId>()?;
	let hyper = "bridge:hyper".parse::<RecordId>()?;
	for record in [&owner, &leaf, &bridge, &hyper] {
		transaction.create_atom(record.clone(), Default::default())?;
	}
	let leaf_local = transaction.attach_node(child.id(), &leaf, None, None)?;
	let owner_local = transaction.attach_node(meta.id(), &owner, None, Some(child.id()))?;
	transaction.attach_edge(
		meta.id(),
		&bridge,
		None,
		EdgeTopology::directed([EdgeEndpoint::Local(owner_local)], [EdgeEndpoint::Nested(
			QualifiedAtomId::new([owner_local], leaf_local)?,
		)]),
		None,
		None,
	)?;
	let stored = transaction.store_level_path(meta.id(), &bridge)?;
	let edge = stored.as_object().ok_or_else(|| {
		crate::gql_status::FerrosGrynnError::invalid_argument("LevelPath edge must be an object")
	})?;
	assert_eq!(edge.get("kind"), Some(&Value::from("binary_edge")));
	let target = edge.get("target").and_then(Value::as_object).ok_or_else(|| {
		crate::gql_status::FerrosGrynnError::invalid_argument("LevelPath target must be an object")
	})?;
	let chains = target.get("chains").and_then(Value::as_array).ok_or_else(|| {
		crate::gql_status::FerrosGrynnError::invalid_argument("LevelPath chains must be an array")
	})?;
	assert_eq!(chains.len(), 1);
	assert_eq!(chains.get(0).and_then(Value::as_array).map(|chain| chain.len()), Some(1));

	transaction.attach_edge(
		meta.id(),
		&hyper,
		None,
		EdgeTopology::directed([EdgeEndpoint::Local(owner_local)], [
			EdgeEndpoint::Local(owner_local),
			EdgeEndpoint::Nested(QualifiedAtomId::new([owner_local], leaf_local)?),
		]),
		None,
		None,
	)?;
	assert!(transaction.store_level_path(meta.id(), &hyper).is_err());
	Ok(())
}

#[test]
fn detach_drops_stored_levelpath_with_the_occurrence() -> FerrosGrynnResult<()> {
	let store = Datastore::in_memory();
	let mut transaction = store.begin();
	transaction.define_class(ClassSchema::new("document", AtomKind::Node, [])?)?;
	let graph = transaction.define_graph("level_detach", GraphKind::Graph)?;
	let record = "document:leaf".parse::<RecordId>()?;
	transaction.create_atom(record.clone(), Default::default())?;
	transaction.attach_node(graph.id(), &record, None, None)?;
	transaction.store_level_path(graph.id(), &record)?;
	let key = transaction.query_property_dictionary().get("levelpath").ok_or_else(|| {
		crate::gql_status::FerrosGrynnError::invalid_argument("missing levelpath")
	})?;
	assert!(transaction.effective_property(graph.id(), &record, key)?.is_some());
	transaction.detach_occurrence(graph.id(), &record, false)?;
	transaction.attach_node(graph.id(), &record, None, None)?;
	assert_eq!(transaction.effective_property(graph.id(), &record, key)?, None);
	Ok(())
}
