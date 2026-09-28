use std::num::NonZeroUsize;

use crate::{
	datastore::{ClassSchema, Datastore},
	gql::{QueryCancellation, QueryExecutionOptions, QueryStatementKind},
	graph::{AtomKind, EdgeTopology, GraphKind, GraphRead, NodeRead},
	values::{Object, PropertyKeyId, PropertyMap, RecordId, Value},
};

#[test]
fn return_graph_is_owned_and_closes_selected_edge_endpoints() {
	let store = Datastore::in_memory();
	let mut seed = store.begin();
	seed.define_class(ClassSchema::new("person", AtomKind::Node, []).unwrap()).unwrap();
	seed.define_class(ClassSchema::new("link", AtomKind::Edge, []).unwrap()).unwrap();
	let source = seed.define_graph("source", GraphKind::Graph).unwrap();
	let left = "person:left".parse::<RecordId>().unwrap();
	let right = "person:right".parse::<RecordId>().unwrap();
	let edge = "link:edge".parse::<RecordId>().unwrap();
	for record in [&left, &right, &edge] {
		seed.create_atom(record.clone(), PropertyMap::new()).unwrap();
	}
	let left_id = seed.attach_node(source.id(), &left, None, None).unwrap();
	let right_id = seed.attach_node(source.id(), &right, None, None).unwrap();
	seed.attach_edge(
		source.id(),
		&edge,
		None,
		EdgeTopology::directed([left_id], [right_id]),
		None,
		None,
	)
	.unwrap();
	seed.commit().unwrap();

	let value = store
		.execute(
			"MATCH (person:left AS a)-[link:edge AS e]->(person:right AS b) IN GRAPH source \
			 RETURN GRAPH { nodes: [], edges: e }",
			&Object::new(),
		)
		.unwrap()
		.remove(0)
		.result
		.unwrap()
		.into_parts()
		.0;
	let bounded = store
		.execute(
			"BUILD GRAPH too_small FROM (MATCH (person:left AS a)-[link:edge AS e]->(person:right \
			 AS b) IN GRAPH source RETURN e) EDGES e LIMIT NODES 1",
			&Object::new(),
		)
		.unwrap();
	assert!(bounded[0].result.is_err());
	assert!(store.begin().named_graph("too_small").unwrap().is_none());
	let bounded = store
		.execute(
			"BUILD GRAPH no_edges FROM (MATCH (person:left AS a)-[link:edge AS e]->(person:right \
			 AS b) IN GRAPH source RETURN e) EDGES e LIMIT EDGES 0",
			&Object::new(),
		)
		.unwrap();
	assert!(bounded[0].result.is_err());
	assert!(store.begin().named_graph("no_edges").unwrap().is_none());
	drop(store);
	let Value::Graph(graph) = value else {
		panic!("expected an owned graph value");
	};
	assert_eq!(graph.node_ids().unwrap().count(), 2);
	assert_eq!(graph.edge_ids().unwrap().count(), 1);
}

#[test]
fn return_graph_preserves_one_shared_child_for_multiple_owners() {
	let store = Datastore::in_memory();
	let mut seed = store.begin();
	seed.define_class(ClassSchema::new("module", AtomKind::Node, []).unwrap()).unwrap();
	seed.define_class(ClassSchema::new("document", AtomKind::Node, []).unwrap()).unwrap();
	let child = seed.define_graph("child", GraphKind::Graph).unwrap();
	let root = seed.define_graph("root", GraphKind::Metagraph).unwrap();
	let document = "document:spec".parse::<RecordId>().unwrap();
	let first = "module:first".parse::<RecordId>().unwrap();
	let second = "module:second".parse::<RecordId>().unwrap();
	for record in [&document, &first, &second] {
		seed.create_atom(record.clone(), PropertyMap::new()).unwrap();
	}
	seed.attach_node(child.id(), &document, None, None).unwrap();
	seed.attach_node(root.id(), &first, None, Some(child.id())).unwrap();
	seed.attach_node(root.id(), &second, None, Some(child.id())).unwrap();
	seed.commit().unwrap();

	let value = store
		.execute(
			"MATCH (document AS d) WITHIN <(module AS owner)> IN GRAPH root RETURN GRAPH { nodes: \
			 d, edges: [] }",
			&Object::new(),
		)
		.unwrap()
		.remove(0)
		.result
		.unwrap()
		.into_parts()
		.0;
	let Value::Graph(graph) = value else {
		panic!("expected graph value");
	};
	let mut children = Vec::new();
	for id in graph.node_ids().unwrap() {
		let node = graph.node(id.unwrap()).unwrap();
		if let Some(child) = node.inner_graph() {
			children.push(child.clone());
		}
	}
	assert_eq!(children.len(), 2);
	assert_eq!(children[0].identity(), children[1].identity());
	assert_eq!(children[0].node_ids().unwrap().count(), 1);

	let mut variables = Object::new();
	variables.insert("graph", Value::GraphRef(Box::new(root)));
	let value = store
		.execute("BUILD GRAPH <META> FROM $graph", &variables)
		.unwrap()
		.remove(0)
		.result
		.unwrap()
		.into_parts()
		.0;
	let Value::Graph(graph) = value else {
		panic!("expected full copied graph value");
	};
	let child_ids = graph
		.node_ids()
		.unwrap()
		.filter_map(|id| graph.node(id.unwrap()).unwrap().inner_graph())
		.map(crate::graph::Graph::identity)
		.collect::<Vec<_>>();
	assert_eq!(child_ids.len(), 2);
	assert_eq!(child_ids[0], child_ids[1]);
}

#[test]
fn anonymous_build_accepts_one_database_global_node() {
	let store = Datastore::in_memory();
	let mut seed = store.begin();
	seed.define_class(ClassSchema::new("person", AtomKind::Node, []).unwrap()).unwrap();
	seed.create_atom("person:solo".parse().unwrap(), PropertyMap::new()).unwrap();
	seed.commit().unwrap();

	let value = store
		.execute("BUILD GRAPH <GRAPH> FROM person:solo", &Object::new())
		.unwrap()
		.remove(0)
		.result
		.unwrap()
		.into_parts()
		.0;
	let Value::Graph(graph) = value else {
		panic!("expected graph value");
	};
	assert_eq!(graph.node_ids().unwrap().count(), 1);
	assert_eq!(graph.edge_ids().unwrap().count(), 0);
}

#[test]
fn anonymous_build_handles_empty_input_and_rejects_invalid_or_ambiguous_scope() {
	let store = Datastore::in_memory();
	let mut seed = store.begin();
	let left = seed.define_graph("left", GraphKind::Graph).unwrap();
	let right = seed.define_graph("right", GraphKind::Graph).unwrap();
	seed.commit().unwrap();

	let empty = store
		.execute("BUILD GRAPH FROM []", &Object::new())
		.unwrap()
		.remove(0)
		.result
		.unwrap()
		.into_parts()
		.0;
	assert!(
		matches!(empty, Value::Graph(graph) if graph.node_count() == 0 && graph.edge_count() == 0)
	);
	assert!(store.execute("BUILD GRAPH FROM null", &Object::new()).unwrap()[0].result.is_err());

	let mut variables = Object::new();
	variables.insert(
		"graphs",
		Value::Array(
			vec![Value::GraphRef(Box::new(left)), Value::GraphRef(Box::new(right))].into(),
		),
	);
	let result = store.execute("BUILD GRAPH ambiguous FROM $graphs", &variables).unwrap();
	assert!(result[0].result.is_err());
	assert!(store.begin().named_graph("ambiguous").unwrap().is_none());
}

#[test]
fn let_accepts_anonymous_build_as_its_entire_rhs() {
	let store = Datastore::in_memory();
	let mut seed = store.begin();
	seed.define_class(ClassSchema::new("person", AtomKind::Node, []).unwrap()).unwrap();
	seed.create_atom("person:solo".parse().unwrap(), PropertyMap::new()).unwrap();
	seed.commit().unwrap();

	let results = store
		.execute("LET $built = BUILD GRAPH <GRAPH> FROM person:solo; RETURN $built", &Object::new())
		.unwrap();
	let Value::Graph(graph) = results[1].result.as_ref().unwrap().value() else {
		panic!("expected graph value bound by LET");
	};
	assert_eq!(graph.node_ids().unwrap().count(), 1);
}

#[test]
fn named_build_installs_existing_atoms_and_topology_atomically() {
	let store = Datastore::in_memory();
	let mut seed = store.begin();
	seed.define_class(ClassSchema::new("person", AtomKind::Node, []).unwrap()).unwrap();
	seed.define_class(ClassSchema::new("link", AtomKind::Edge, []).unwrap()).unwrap();
	let source = seed.define_graph("source", GraphKind::Graph).unwrap();
	let left = "person:left".parse::<RecordId>().unwrap();
	let right = "person:right".parse::<RecordId>().unwrap();
	let edge = "link:edge".parse::<RecordId>().unwrap();
	for record in [&left, &right, &edge] {
		seed.create_atom(record.clone(), PropertyMap::new()).unwrap();
	}
	let left_id = seed.attach_node(source.id(), &left, None, None).unwrap();
	let right_id = seed.attach_node(source.id(), &right, None, None).unwrap();
	seed.attach_edge(
		source.id(),
		&edge,
		None,
		EdgeTopology::directed([left_id], [right_id]),
		None,
		None,
	)
	.unwrap();
	seed.commit().unwrap();

	let value = store
		.execute(
			"BUILD GRAPH copied FROM (MATCH (person:left AS a)-[link:edge AS e]->(person:right AS \
			 b) IN GRAPH source RETURN a, e, b) NODES [a, b] EDGES e",
			&Object::new(),
		)
		.unwrap()
		.remove(0)
		.result
		.unwrap()
		.into_parts()
		.0;
	let Value::GraphRef(reference) = value else {
		panic!("expected graph reference");
	};
	assert_eq!(reference.name().as_str(), "copied");
	let rows = store
		.execute(
			"MATCH (person:left AS a)-[link:edge]->(person:right AS b) IN GRAPH copied RETURN b",
			&Object::new(),
		)
		.unwrap();
	assert!(matches!(
		rows[0].result.as_ref().unwrap().value(),
		Value::Array(values) if values.len() == 1
	));
}

#[test]
fn named_build_reproduces_effective_overlays_without_creating_base_atoms() {
	let store = Datastore::in_memory();
	let mut seed = store.begin();
	seed.define_class(ClassSchema::new("person", AtomKind::Node, []).unwrap()).unwrap();
	let source = seed.define_graph("source", GraphKind::Graph).unwrap();
	let person = "person:alice".parse::<RecordId>().unwrap();
	let inherited = PropertyKeyId::new(101);
	let changed = PropertyKeyId::new(102);
	let mut properties = PropertyMap::new();
	properties.insert(inherited, "base");
	properties.insert(changed, "base");
	seed.create_atom(person.clone(), properties).unwrap();
	seed.attach_node(source.id(), &person, None, None).unwrap();
	seed.hide_property(source.id(), &person, inherited).unwrap();
	seed.set_overlay(source.id(), &person, changed, Value::from("overlay")).unwrap();
	seed.commit().unwrap();

	store
		.execute(
			"BUILD GRAPH copied FROM (MATCH (person:alice AS p) IN GRAPH source RETURN p) NODES p",
			&Object::new(),
		)
		.unwrap();
	let mut read = store.begin();
	let copied = read.named_graph("copied").unwrap().unwrap();
	assert_eq!(read.effective_property(copied.id(), &person, inherited).unwrap(), None);
	assert_eq!(
		read.effective_property(copied.id(), &person, changed).unwrap(),
		Some(Value::from("overlay"))
	);
}

#[test]
fn named_build_keeps_one_hidden_child_for_multiple_owners() {
	let store = Datastore::in_memory();
	let mut seed = store.begin();
	seed.define_class(ClassSchema::new("module", AtomKind::Node, []).unwrap()).unwrap();
	seed.define_class(ClassSchema::new("document", AtomKind::Node, []).unwrap()).unwrap();
	let child = seed.define_graph("child", GraphKind::Graph).unwrap();
	let root = seed.define_graph("root", GraphKind::Metagraph).unwrap();
	let document = "document:spec".parse::<RecordId>().unwrap();
	let first = "module:first".parse::<RecordId>().unwrap();
	let second = "module:second".parse::<RecordId>().unwrap();
	for record in [&document, &first, &second] {
		seed.create_atom(record.clone(), PropertyMap::new()).unwrap();
	}
	seed.attach_node(child.id(), &document, None, None).unwrap();
	seed.attach_node(root.id(), &first, None, Some(child.id())).unwrap();
	seed.attach_node(root.id(), &second, None, Some(child.id())).unwrap();
	seed.commit().unwrap();

	store
		.execute(
			"BUILD GRAPH <META> copied FROM (MATCH (document AS d) WITHIN <(module AS owner)> IN \
			 GRAPH root RETURN d, owner) NODES [d, owner]",
			&Object::new(),
		)
		.unwrap();
	let mut transaction = store.begin();
	let copied = transaction.named_graph("copied").unwrap().unwrap();
	assert_eq!(transaction.graphs().unwrap().len(), 3);
	let view = copied.read(&transaction).unwrap();
	let mut children = Vec::new();
	for id in view.node_ids().unwrap() {
		let node = view.node(id.unwrap()).unwrap().unwrap();
		if let Some(child) = node.inner_graph_ref().unwrap() {
			children.push(child.into_owned());
		}
	}
	assert_eq!(children.len(), 2);
	assert_eq!(children[0].id(), children[1].id());
	assert!(children[0].name().as_str().starts_with("$fg$build_"));
	assert!(transaction.named_graph(children[0].name()).unwrap().is_none());
}

#[test]
fn overwrite_preserves_graph_identity_properties_and_system_indexes() {
	let store = Datastore::in_memory();
	let mut seed = store.begin();
	seed.define_class(ClassSchema::new("person", AtomKind::Node, []).unwrap()).unwrap();
	let source = seed.define_graph("source", GraphKind::Graph).unwrap();
	let person = "person:alice".parse::<RecordId>().unwrap();
	seed.create_atom(person.clone(), PropertyMap::new()).unwrap();
	seed.attach_node(source.id(), &person, None, None).unwrap();
	seed.commit().unwrap();
	store
		.execute("DEFINE GRAPH target TYPE GRAPH CONTENT { owner: 'preserved' }", &Object::new())
		.unwrap();
	let before = store.schema_catalog().named_graph("target").unwrap();
	let index_ids = before.system_indexes().map(|index| index.id()).collect::<Vec<_>>();

	store
		.execute(
			"BUILD GRAPH OVERWRITE <GRAPH> target FROM (MATCH (person:alice AS p) IN GRAPH source \
			 RETURN p) NODES p",
			&Object::new(),
		)
		.unwrap();
	let after = store.schema_catalog().named_graph("target").unwrap();
	assert_eq!(after.id(), before.id());
	assert_eq!(after.reference().kind(), before.reference().kind());
	assert_eq!(after.properties(), before.properties());
	assert_eq!(after.system_indexes().map(|index| index.id()).collect::<Vec<_>>(), index_ids);
}

#[test]
fn if_not_exists_does_not_evaluate_the_build_source() {
	let store = Datastore::in_memory();
	let mut seed = store.begin();
	seed.define_graph("kept", GraphKind::Graph).unwrap();
	seed.commit().unwrap();
	let value = store
		.execute("BUILD GRAPH IF NOT EXISTS kept FROM 1 / 0", &Object::new())
		.unwrap()
		.remove(0)
		.result
		.unwrap()
		.into_parts()
		.0;
	assert!(matches!(value, Value::GraphRef(reference) if reference.name().as_str() == "kept"));
}

#[test]
fn named_build_does_not_create_a_missing_base_atom() {
	let store = Datastore::in_memory();
	let mut seed = store.begin();
	seed.define_class(ClassSchema::new("person", AtomKind::Node, []).unwrap()).unwrap();
	seed.commit().unwrap();
	assert!(
		store.execute("BUILD GRAPH missing FROM person:absent", &Object::new()).unwrap()[0]
			.result
			.is_err()
	);
	let mut read = store.begin();
	assert!(read.named_graph("missing").unwrap().is_none());
}

#[test]
fn graph_scoped_select_resolves_record_kind_before_edge_closure() {
	let store = Datastore::in_memory();
	let mut seed = store.begin();
	seed.define_class(ClassSchema::new("person", AtomKind::Node, []).unwrap()).unwrap();
	seed.define_class(ClassSchema::new("link", AtomKind::Edge, []).unwrap()).unwrap();
	let source = seed.define_graph("source", GraphKind::Graph).unwrap();
	let left = "person:left".parse::<RecordId>().unwrap();
	let right = "person:right".parse::<RecordId>().unwrap();
	let edge = "link:edge".parse::<RecordId>().unwrap();
	for record in [&left, &right, &edge] {
		seed.create_atom(record.clone(), PropertyMap::new()).unwrap();
	}
	let left_id = seed.attach_node(source.id(), &left, None, None).unwrap();
	let right_id = seed.attach_node(source.id(), &right, None, None).unwrap();
	seed.attach_edge(
		source.id(),
		&edge,
		None,
		EdgeTopology::directed([left_id], [right_id]),
		None,
		None,
	)
	.unwrap();
	seed.commit().unwrap();

	let value = store
		.execute("BUILD GRAPH FROM (SELECT VALUE id FROM link IN GRAPH source)", &Object::new())
		.unwrap()
		.remove(0)
		.result
		.unwrap()
		.into_parts()
		.0;
	let Value::Graph(graph) = value else {
		panic!("expected graph value");
	};
	assert_eq!(graph.node_count(), 2);
	assert_eq!(graph.edge_count(), 1);
}

#[test]
fn graph_result_memory_limit_fails_without_publishing_named_target() {
	let store = Datastore::in_memory();
	let mut seed = store.begin();
	seed.define_class(ClassSchema::new("person", AtomKind::Node, []).unwrap()).unwrap();
	seed.create_atom("person:solo".parse().unwrap(), PropertyMap::new()).unwrap();
	seed.commit().unwrap();
	let options = QueryExecutionOptions {
		barrier_memory_bytes: NonZeroUsize::MIN,
		..QueryExecutionOptions::default()
	};
	let result = store
		.execute_with_options("BUILD GRAPH bounded FROM person:solo", &Object::new(), options)
		.unwrap();
	assert!(result[0].result.is_err());
	assert!(store.begin().named_graph("bounded").unwrap().is_none());

	let limited = store
		.execute("BUILD GRAPH limited FROM person:solo LIMIT NODES 0", &Object::new())
		.unwrap();
	assert!(limited[0].result.is_err());
	assert!(store.begin().named_graph("limited").unwrap().is_none());

	let cancellation = QueryCancellation::default();
	cancellation.cancel();
	let cancelled = store
		.execute_with_options(
			"BUILD GRAPH cancelled FROM person:solo",
			&Object::new(),
			QueryExecutionOptions {
				cancellation: Some(cancellation),
				..QueryExecutionOptions::default()
			},
		)
		.unwrap();
	assert!(cancelled[0].result.is_err());
	assert!(store.begin().named_graph("cancelled").unwrap().is_none());
}

#[test]
fn read_only_allows_anonymous_but_rejects_named_build() {
	let store = Datastore::in_memory();
	let mut seed = store.begin();
	seed.define_class(ClassSchema::new("person", AtomKind::Node, []).unwrap()).unwrap();
	seed.create_atom("person:solo".parse().unwrap(), PropertyMap::new()).unwrap();
	seed.commit().unwrap();
	let anonymous = store
		.execute("BEGIN READ ONLY; BUILD GRAPH FROM person:solo; COMMIT;", &Object::new())
		.unwrap();
	assert!(anonymous.iter().all(|result| result.result.is_ok()));
	let named = store
		.execute("BEGIN READ ONLY; BUILD GRAPH forbidden FROM person:solo; COMMIT;", &Object::new())
		.unwrap();
	assert!(named.iter().any(|result| result.result.is_err()));
	assert!(store.begin().named_graph("forbidden").unwrap().is_none());
}

#[test]
fn atomic_import_rejects_build_graph_before_opening_staging() {
	let store = Datastore::in_memory();
	assert!(
		store
			.execute(
				"BEGIN IMPORT; BUILD GRAPH forbidden FROM person:solo; END IMPORT;",
				&Object::new(),
			)
			.is_err()
	);
	assert_eq!(store.inner.current_root().commit_ts, 0);
}

#[test]
fn rooted_tree_contract_uses_the_explicit_root_and_rejects_ignored_root_clauses() {
	let store = Datastore::in_memory();
	let mut seed = store.begin();
	seed.define_class(ClassSchema::new("person", AtomKind::Node, []).unwrap()).unwrap();
	seed.define_class(ClassSchema::new("link", AtomKind::Edge, []).unwrap()).unwrap();
	let source = seed.define_graph("source", GraphKind::Graph).unwrap();
	let root = "person:root".parse::<RecordId>().unwrap();
	let child = "person:child".parse::<RecordId>().unwrap();
	let edge = "link:edge".parse::<RecordId>().unwrap();
	for record in [&root, &child, &edge] {
		seed.create_atom(record.clone(), PropertyMap::new()).unwrap();
	}
	let root_id = seed.attach_node(source.id(), &root, None, None).unwrap();
	let child_id = seed.attach_node(source.id(), &child, None, None).unwrap();
	seed.attach_edge(
		source.id(),
		&edge,
		None,
		EdgeTopology::directed([root_id], [child_id]),
		None,
		None,
	)
	.unwrap();
	seed.commit().unwrap();
	let source = "(MATCH (person:root AS a)-[link:edge AS e]->(person:child AS b) IN GRAPH source \
	              RETURN a, e, b) ROOT a NODES [a, b] EDGES e";
	let result = store
		.execute(
			&format!("BUILD GRAPH <ROOTED_TREE SIMPLE BIPARTITE COMPLETE> FROM {source}"),
			&Object::new(),
		)
		.unwrap()
		.remove(0)
		.result
		.unwrap()
		.into_parts()
		.0;
	assert!(matches!(result, Value::Graph(graph) if graph.kind() == GraphKind::RootedTree));
	assert!(
		store
			.execute(
				&format!("BUILD GRAPH <ROOTED_TREE> FROM {source}").replace("ROOT a", "ROOT b"),
				&Object::new()
			)
			.unwrap()[0]
			.result
			.is_err()
	);
	assert!(
		store.execute("BUILD GRAPH FROM person:root ROOT person:root", &Object::new()).unwrap()[0]
			.result
			.is_err()
	);
}

#[test]
fn explain_build_graph_is_effect_free_until_analyze() {
	let store = Datastore::in_memory();
	let mut seed = store.begin();
	seed.define_class(ClassSchema::new("person", AtomKind::Node, []).unwrap()).unwrap();
	seed.create_atom("person:solo".parse().unwrap(), PropertyMap::new()).unwrap();
	seed.commit().unwrap();

	let planned = store
		.execute("EXPLAIN FULL FORMAT JSON BUILD GRAPH planned FROM person:solo", &Object::new())
		.unwrap()
		.remove(0);
	assert_eq!(planned.kind, QueryStatementKind::Explain);
	let Value::Object(explain) = planned.result.unwrap().into_parts().0 else {
		panic!("expected JSON explain object");
	};
	let Some(Value::Array(nodes)) = explain.get("nodes") else {
		panic!("expected graph-result physical nodes");
	};
	assert!(nodes.iter().any(|node| {
		matches!(node, Value::Object(node) if node.get("operator") == Some(&Value::from("BuildGraph")))
	}));
	assert!(store.begin().named_graph("planned").unwrap().is_none());

	let analyzed = store
		.execute(
			"EXPLAIN ANALYZE FORMAT JSON BUILD GRAPH published FROM person:solo",
			&Object::new(),
		)
		.unwrap();
	assert!(analyzed[0].result.is_ok());
	let Value::Object(explain) = analyzed[0].result.as_ref().unwrap().value() else {
		panic!("expected JSON explain object");
	};
	let Some(Value::Object(metrics)) = explain.get("analyze") else {
		panic!("expected graph assembly metrics");
	};
	assert_eq!(metrics.get("graph_source_rows"), Some(&Value::from(1_u64)));
	assert_eq!(metrics.get("graph_selected_nodes"), Some(&Value::from(1_u64)));
	assert_eq!(metrics.get("graph_final_nodes"), Some(&Value::from(1_u64)));
	assert_eq!(metrics.get("graph_final_edges"), Some(&Value::from(0_u64)));
	assert!(store.begin().named_graph("published").unwrap().is_some());
}
