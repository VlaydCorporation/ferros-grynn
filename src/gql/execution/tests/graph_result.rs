use std::num::NonZeroUsize;

use crate::{
	datastore::{ClassSchema, Datastore},
	gql::{QueryCancellation, QueryExecutionOptions, QueryStatementKind},
	graph::{AtomKind, EdgeTopology, GraphKind, GraphRead, NodeRead},
	values::{Object, PropertyKeyId, PropertyMap, RecordId, Value},
};

fn select_graph_fixture() -> (Datastore, Object) {
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
	let mut variables = Object::new();
	variables.insert(
		"edge",
		crate::graph::AtomOccurrenceRef::new(
			crate::graph::AtomRef::new(AtomKind::Edge, edge),
			std::sync::Arc::new(source.clone()),
		),
	);
	variables.insert(
		"root",
		crate::graph::AtomOccurrenceRef::new(
			crate::graph::AtomRef::new(AtomKind::Node, left),
			std::sync::Arc::new(source),
		),
	);
	(store, variables)
}

#[test]
fn select_terminal_graph_consumes_projected_window_group_and_join_rows() {
	let (store, variables) = select_graph_fixture();
	for (query, expected_nodes) in [
		(
			"SELECT id AS picked FROM person ORDER BY id LIMIT 1 RETURN GRAPH {nodes: picked, \
			 edges: []}",
			1,
		),
		(
			"SELECT array::group(id) AS picked FROM person GROUP ALL RETURN GRAPH {nodes: picked, \
			 edges: []}",
			2,
		),
		(
			"SELECT p.id AS left_id, q.id AS right_id FROM person AS p CROSS JOIN person AS q \
			 WHERE p.id = person:left AND q.id = person:right RETURN GRAPH {nodes: [left_id, \
			 right_id], edges: []}",
			2,
		),
		("SELECT id AS picked FROM person LIMIT 0 RETURN GRAPH {nodes: picked, edges: []}", 0),
	] {
		let outcomes = store.execute(query, &variables).unwrap();
		let value = outcomes[0].result.as_ref().unwrap().value();
		let Value::Graph(graph) = value else {
			panic!("expected SELECT graph result");
		};
		assert_eq!(graph.kind(), GraphKind::Subgraph, "{query}");
		assert_eq!(graph.node_ids().unwrap().count(), expected_nodes, "{query}");
		assert_eq!(graph.edge_ids().unwrap().count(), 0, "{query}");
	}
	let outcomes = store
		.execute(
			"SELECT ONLY id AS picked FROM person RETURN GRAPH {nodes: picked, edges: []}",
			&variables,
		)
		.unwrap();
	assert!(outcomes[0].result.is_err());
}

#[test]
fn select_terminal_graph_uses_common_family_validators_and_endpoint_closure() {
	let (store, variables) = select_graph_fixture();
	for (annotation, expected_kind) in [
		("", GraphKind::Subgraph),
		("<GRAPH>", GraphKind::Graph),
		("<FOREST>", GraphKind::Forest),
		("<TREE>", GraphKind::Tree),
		("<ROOTED_TREE> ROOT $root", GraphKind::RootedTree),
		("<DAG>", GraphKind::Dag),
		("<HYPER>", GraphKind::Hypergraph),
		("<META>", GraphKind::Metagraph),
	] {
		let query = format!(
			"SELECT $edge AS picked FROM person LIMIT 1 RETURN GRAPH {annotation} {{nodes: [], \
			 edges: picked}}"
		);
		let outcomes = store.execute(&query, &variables).unwrap();
		let Value::Graph(graph) = outcomes[0].result.as_ref().unwrap().value() else {
			panic!("expected exact graph family");
		};
		assert_eq!(graph.kind(), expected_kind, "{query}");
		assert_eq!(graph.node_ids().unwrap().count(), 2);
		assert_eq!(graph.edge_ids().unwrap().count(), 1);
	}
	let outcomes = store
		.execute(
			"SELECT id AS picked FROM person WHERE id = person:left RETURN GRAPH <ROOTED_TREE> \
			 ROOT person:right {nodes: picked, edges: []}",
			&variables,
		)
		.unwrap();
	assert!(outcomes[0].result.is_err());
}

#[test]
fn terminal_select_graph_is_one_scalar_subquery_value() {
	let (store, variables) = select_graph_fixture();
	let outcomes = store
		.execute(
			"SELECT VALUE (SELECT p.id AS picked FROM person AS p RETURN GRAPH {nodes: picked, \
			 edges: []}) FROM person:left",
			&variables,
		)
		.unwrap();
	let Value::Array(values) = outcomes[0].result.as_ref().unwrap().value() else {
		panic!("expected outer SELECT array");
	};
	assert_eq!(values.len(), 1);
	let Value::Graph(graph) = values.iter().next().unwrap() else {
		panic!("expected one materialized graph value");
	};
	assert_eq!(graph.node_ids().unwrap().count(), 2);
}

#[test]
fn explicit_root_none_or_unselected_record_is_rejected_by_shared_assembly() {
	let (store, variables) = select_graph_fixture();
	for root in ["NONE", "person:right"] {
		let terminal = format!(
			"SELECT id AS picked FROM person WHERE id = person:left RETURN GRAPH <ROOTED_TREE> \
			 ROOT {root} {{nodes: picked, edges: []}}"
		);
		let outcomes = store.execute(&terminal, &variables).unwrap();
		assert!(outcomes[0].result.is_err(), "{terminal}");
		let build = format!(
			"BUILD GRAPH <ROOTED_TREE> FROM (SELECT id AS picked FROM person WHERE id = \
			 person:left) NODES picked ROOT {root}"
		);
		let outcomes = store.execute(&build, &variables).unwrap();
		assert!(outcomes[0].result.is_err(), "{build}");
	}
}

#[test]
fn graph_terminal_derived_and_build_sources_expose_one_graph_value() {
	let (store, variables) = select_graph_fixture();
	let inner = "SELECT id AS picked, $edge AS connector FROM person LIMIT 1 RETURN GRAPH <GRAPH> \
	             {nodes: picked, edges: connector}";
	let outcomes =
		store.execute(&format!("SELECT VALUE d.graph FROM ({inner}) AS d"), &variables).unwrap();
	let Value::Array(values) = outcomes[0].result.as_ref().unwrap().value() else {
		panic!("expected one derived row");
	};
	assert_eq!(values.len(), 1);
	assert!(
		matches!(values.iter().next(), Some(Value::Graph(graph)) if graph.node_count() == 2 && graph.edge_count() == 1)
	);
	let outcomes =
		store.execute(&format!("BUILD GRAPH <GRAPH> FROM ({inner})"), &variables).unwrap();
	assert!(
		matches!(outcomes[0].result.as_ref().unwrap().value(), Value::Graph(graph) if graph.node_count() == 2 && graph.edge_count() == 1)
	);
	let invalid = "SELECT VALUE d.graph FROM (SELECT id AS picked FROM person WHERE id = \
	               person:left RETURN GRAPH <ROOTED_TREE> ROOT person:right {nodes: picked, \
	               edges: []}) AS d";
	assert!(store.execute(invalid, &variables).unwrap()[0].result.is_err());
}

#[test]
fn value_object_keeps_its_single_projected_column_for_graph_selectors() {
	let (store, variables) = select_graph_fixture();
	for query in [
		"SELECT VALUE {node: id} AS picked FROM person RETURN GRAPH {nodes: picked.node, edges: \
		 []}",
		"BUILD GRAPH FROM (SELECT VALUE {node: id} AS picked FROM person) NODES picked.node",
	] {
		let outcomes = store.execute(query, &variables).unwrap();
		assert!(
			matches!(outcomes[0].result.as_ref().unwrap().value(), Value::Graph(graph) if graph.node_count() == 2),
			"{query}"
		);
	}
}

#[test]
fn terminal_graph_pages_use_the_retained_session_cursor() {
	let (store, mut variables) = select_graph_fixture();
	let mut third = store.begin();
	third.create_atom("person:third".parse().unwrap(), PropertyMap::new()).unwrap();
	third.commit().unwrap();
	let mut session = store.session();
	let first = session
		.execute(
			"SELECT id AS picked FROM person ORDER BY id LIMIT 1 RETURN GRAPH {nodes: picked, \
			 edges: []}",
			&variables,
		)
		.unwrap()
		.remove(0);
	let cursor = first.continuation.unwrap();
	variables.insert("cursor", cursor);
	let query = "SELECT id AS picked FROM person ORDER BY id LIMIT 1 START AFTER $cursor RETURN \
	             GRAPH {nodes: picked, edges: []}";
	let second = session.execute(query, &variables).unwrap().remove(0);
	let replay = session.execute(query, &variables).unwrap().remove(0);
	assert!(second.continuation.is_some());
	let Value::Graph(first) = first.result.unwrap().into_parts().0 else {
		panic!("expected first graph page");
	};
	let Value::Graph(second) = second.result.unwrap().into_parts().0 else {
		panic!("expected second graph page");
	};
	let Value::Graph(replay) = replay.result.unwrap().into_parts().0 else {
		panic!("expected replay graph page");
	};
	let records = |graph: &crate::graph::MaterializedGraph| {
		graph
			.node_ids()
			.unwrap()
			.map(|id| graph.node(id.unwrap()).unwrap().record_id().clone())
			.collect::<Vec<_>>()
	};
	assert_ne!(records(&first), records(&second));
	assert_eq!(records(&second), records(&replay));
}

#[test]
fn explain_build_keeps_the_nested_select_graph_assembly() {
	let (store, variables) = select_graph_fixture();
	let outcomes = store
		.execute(
			"EXPLAIN FULL FORMAT JSON BUILD GRAPH <GRAPH> FROM (SELECT id AS picked FROM person \
			 RETURN GRAPH <FOREST> {nodes: picked, edges: []})",
			&variables,
		)
		.unwrap();
	let Value::Object(explain) = outcomes[0].result.as_ref().unwrap().value() else {
		panic!("expected explain object");
	};
	let Some(Value::Array(nodes)) = explain.get("nodes") else {
		panic!("expected physical nodes");
	};
	assert_eq!(nodes.iter().filter(|node| matches!(node, Value::Object(node) if node.get("operator") == Some(&Value::from("GraphAssemble")))).count(), 2);
}

#[test]
fn computed_primary_graph_scope_is_read_once_for_rows_and_assembly() {
	use std::cell::Cell;

	use crate::{
		datastore::ConflictKey,
		gql::{
			ExecutionControl,
			execution::{RootRead, cursor::QueryRead},
			ir::LogicalStatement,
		},
	};
	let (store, variables) = select_graph_fixture();
	assert!(
		store
			.execute(
				"DEFINE CLASS settings TYPE NODE SCHEMALESS; CREATE settings:scope SET name = \
				 'source';",
				&Object::new()
			)
			.unwrap()
			.iter()
			.all(|outcome| outcome.result.is_ok())
	);
	let root = store.inner.current_root();
	let setting = "settings:scope".parse::<RecordId>().unwrap();
	let reads = Cell::new(0);
	let observe = |key| {
		if let ConflictKey::BaseAtom(id) = key
			&& root.atom(id).is_some_and(|atom| atom.record_id() == &setting)
		{
			reads.set(reads.get() + 1);
		}
	};
	let work = |_amount| Ok(());
	let control = ExecutionControl::new(&QueryExecutionOptions::default(), None);
	let read = RootRead::new(&root, &control, &observe, &work, std::env::temp_dir());
	let source = "SELECT id AS picked FROM person IN GRAPH settings:scope.name RETURN GRAPH \
	              {nodes: picked, edges: []}";
	let syntax = crate::gql::parse(source, crate::syn::ParserSettings::default()).unwrap();
	let logical = crate::gql::analysis::analyze(&syntax, source, &read, &variables).unwrap();
	let LogicalStatement::Select(plan) = &logical.statements[0] else {
		panic!("expected terminal SELECT");
	};
	let slots = crate::gql::execution::initialize_slots(&logical, &variables);
	let graph = crate::gql::execution::select::execute_select(
		&read,
		read.snapshot().unwrap(),
		plan,
		&slots,
		source,
		&control,
	)
	.unwrap();
	assert!(matches!(graph, Value::Graph(graph) if graph.node_count() == 2));
	assert_eq!(reads.get(), 1);
}

#[test]
fn graph_terminal_only_distinguishes_empty_window_from_value_none() {
	let (store, variables) = select_graph_fixture();
	let empty = store
		.execute(
			"SELECT ONLY * FROM person WHERE false RETURN GRAPH {nodes: id, edges: []}",
			&variables,
		)
		.unwrap();
	assert!(
		matches!(empty[0].result.as_ref().unwrap().value(), Value::Graph(graph) if graph.node_count() == 0)
	);
	let one = store
		.execute(
			"SELECT ONLY VALUE NONE AS picked FROM person:left RETURN GRAPH {nodes: $root, edges: \
			 []}",
			&variables,
		)
		.unwrap();
	assert!(
		matches!(one[0].result.as_ref().unwrap().value(), Value::Graph(graph) if graph.node_count() == 1)
	);
}

#[test]
fn partial_graph_assembly_retains_qualified_source_mapping() {
	let (store, variables) = select_graph_fixture();
	let outcomes = store
		.execute(
			"SELECT $edge AS picked FROM person LIMIT 1 RETURN GRAPH <GRAPH> {nodes: [], edges: \
			 picked}",
			&variables,
		)
		.unwrap();
	let Value::Graph(graph) = outcomes[0].result.as_ref().unwrap().value() else {
		panic!("expected graph result");
	};
	let crate::graph::MaterializedGraph::Graph(graph) = graph.as_ref() else {
		panic!("expected general family");
	};
	let origin = graph.materialization_origin().unwrap();
	let Value::AtomOccurrence(edge) = variables.get("edge").unwrap() else {
		panic!("expected qualified edge input");
	};
	assert_eq!(origin.graph_ref().unwrap().id(), edge.graph().id());
	for atom in graph.node_ids().unwrap().chain(graph.edge_ids().unwrap()) {
		let atom = atom.unwrap();
		assert_eq!(origin.source_atom(atom), GraphRead::atom_ref(graph, atom).unwrap().as_ref());
	}
	drop(store);
	assert_eq!(graph.node_count(), 2);
}

#[test]
fn embedded_graph_terminals_do_not_publish_an_inner_cursor() {
	let (store, variables) = select_graph_fixture();
	let mut session = store.session();
	for query in [
		"SELECT VALUE d.graph FROM (SELECT id AS picked FROM person ORDER BY id LIMIT 1 RETURN \
		 GRAPH {nodes: picked, edges: []}) AS d",
		"BUILD GRAPH FROM (SELECT id AS picked FROM person ORDER BY id LIMIT 1 RETURN GRAPH \
		 {nodes: picked, edges: []})",
	] {
		let result = session.execute(query, &variables).unwrap().remove(0);
		assert!(result.result.is_ok(), "{query}");
		assert!(result.continuation.is_none(), "{query}");
	}
}

#[test]
fn exact_graph_continuations_use_the_original_root_for_memory_and_index_pages() {
	for (indexed, read_committed) in [(false, false), (true, false), (false, true), (true, true)] {
		let store = Datastore::in_memory();
		assert!(
			store
				.execute(
					"DEFINE CLASS person TYPE NODE SCHEMAFULL; DEFINE FIELD rank ON CLASS person \
					 TYPE int; DEFINE FIELD mark ON CLASS person TYPE int; DEFINE INDEX by_rank \
					 ON CLASS person FIELDS rank UNIQUE SCOPE BASE; CREATE person:a SET rank = 1, \
					 mark = 11; CREATE person:b SET rank = 2, mark = 22; CREATE person:c SET rank \
					 = 3, mark = 33;",
					&Object::new()
				)
				.unwrap()
				.iter()
				.all(|outcome| outcome.result.is_ok())
		);
		let access = if indexed {
			"WITH INDEX by_rank"
		} else {
			"WITH NOINDEX"
		};
		let prefix = format!(
			"SELECT id AS picked FROM person {access} ORDER BY rank ASC NULLS FIRST LIMIT 1"
		);
		let mut session = store.session();
		let first = session
			.execute(&format!("{prefix} RETURN GRAPH {{nodes: picked, edges: []}}"), &Object::new())
			.unwrap()
			.remove(0);
		let token = first.continuation.unwrap();
		assert!(
			store.execute("UPDATE person:b SET mark = 99;", &Object::new()).unwrap()[0]
				.result
				.is_ok()
		);
		if indexed && !read_committed {
			assert!(
				store.execute("REMOVE INDEX by_rank ON CLASS person;", &Object::new()).unwrap()[0]
					.result
					.is_ok()
			);
		}
		let variables = Object::from_iter([("cursor", Value::from(token))]);
		let mode = if read_committed {
			"READ COMMITTED"
		} else {
			""
		};
		let second = session
			.execute(
				&format!(
					"{prefix} START AFTER $cursor {mode} RETURN GRAPH {{nodes: picked, edges: []}}"
				),
				&variables,
			)
			.unwrap()
			.remove(0);
		let Value::Graph(graph) = second.result.unwrap().into_parts().0 else {
			panic!("expected graph continuation");
		};
		let node = graph.node(graph.node_ids().unwrap().next().unwrap().unwrap()).unwrap();
		assert_eq!(node.record_id().to_string(), "person:b");
		let expected = if read_committed {
			99
		} else {
			22
		};
		assert!(node.properties().iter().any(|(_key, value)| value == &Value::from(expected)));
	}
}

#[test]
fn exact_graph_continuation_keeps_the_original_selector_parameters() {
	let (store, mut variables) = select_graph_fixture();
	let mut third = store.begin();
	third.create_atom("person:third".parse().unwrap(), PropertyMap::new()).unwrap();
	third.commit().unwrap();
	variables.insert("selection", variables.get("root").unwrap().clone());
	let mut session = store.session();
	let first = session
		.execute(
			"SELECT id AS picked FROM person ORDER BY id LIMIT 1 RETURN GRAPH {nodes: $selection, \
			 edges: []}",
			&variables,
		)
		.unwrap()
		.remove(0);
	variables.insert("cursor", first.continuation.unwrap());
	let Value::AtomOccurrence(root) = variables.get("root").unwrap() else {
		panic!("expected qualified root");
	};
	let other = crate::graph::AtomOccurrenceRef::new(
		crate::graph::AtomRef::new(AtomKind::Node, "person:right".parse().unwrap()),
		std::sync::Arc::new(root.graph().clone()),
	);
	variables.insert("selection", other);
	let second = session
		.execute(
			"SELECT id AS picked FROM person ORDER BY id LIMIT 1 START AFTER $cursor RETURN GRAPH \
			 {nodes: $selection, edges: []}",
			&variables,
		)
		.unwrap()
		.remove(0);
	let Value::Graph(graph) = second.result.unwrap().into_parts().0 else {
		panic!("expected exact graph page");
	};
	let node = graph.node(graph.node_ids().unwrap().next().unwrap().unwrap()).unwrap();
	assert_eq!(node.record_id().to_string(), "person:left");
}

#[test]
fn graph_cursor_capture_admits_unused_parameter_payload_before_retaining_it() {
	let (store, mut variables) = select_graph_fixture();
	let mut session = store.session();
	let query = "SELECT id AS picked FROM person ORDER BY id LIMIT 1 RETURN GRAPH {nodes: picked, \
	             edges: []}";
	let options = QueryExecutionOptions {
		barrier_memory_bytes: NonZeroUsize::new(64 * 1024).unwrap(),
		..QueryExecutionOptions::default()
	};
	let first = session.execute_with_options(query, &variables, options.clone()).unwrap().remove(0);
	assert!(first.result.is_ok());
	session.close_cursor(&first.continuation.unwrap()).unwrap();
	variables.insert("unused", Value::from("x".repeat(1024 * 1024)));
	let rejected = session.execute_with_options(query, &variables, options).unwrap().remove(0);
	assert!(rejected.result.is_err());
	assert!(rejected.continuation.is_none());
}

#[test]
fn exact_graph_cursor_closures_check_blocks_and_keep_lexical_captures() {
	let (store, mut variables) = select_graph_fixture();
	variables.insert("selection", variables.get("root").unwrap().clone());
	variables.insert("other", variables.get("root").unwrap().clone());
	let selector = "(|$dummy| { 0; RETURN $selection; })(0)";
	let prefix = "SELECT id AS picked FROM person ORDER BY id LIMIT 1";
	for predeclare_cursor in [false, true] {
		let mut frame = variables.clone();
		if predeclare_cursor {
			frame.insert("cursor", Value::None);
		}
		let mut session = store.session();
		let first = session
			.execute(&format!("{prefix} RETURN GRAPH {{nodes: {selector}, edges: []}}"), &frame)
			.unwrap()
			.remove(0);
		assert!(first.result.is_ok());
		frame.insert("cursor", first.continuation.unwrap());
		let changed = session
			.execute(
				&format!(
					"{prefix} START AFTER $cursor RETURN GRAPH {{nodes: {}, edges: []}}",
					selector.replace("$selection", "$other")
				),
				&frame,
			)
			.unwrap()
			.remove(0);
		assert!(changed.result.is_err());
		let same = session
			.execute(
				&format!(
					"{prefix} START AFTER $cursor RETURN GRAPH {{nodes: {selector}, edges: []}}"
				),
				&frame,
			)
			.unwrap()
			.remove(0);
		assert!(same.result.is_ok());
	}
}

fn indexed_graph_cursor_store() -> Datastore {
	let store = Datastore::in_memory();
	let results = store
		.execute(
			"DEFINE CLASS person TYPE NODE SCHEMAFULL; DEFINE FIELD rank ON CLASS person TYPE \
			 int; DEFINE INDEX by_rank ON CLASS person FIELDS rank UNIQUE SCOPE BASE; CREATE \
			 person:a SET rank = 1; CREATE person:b SET rank = 2; CREATE person:c SET rank = 3;",
			&Object::new(),
		)
		.unwrap();
	assert!(results.iter().all(|result| result.result.is_ok()));
	store
}

#[test]
fn exact_graph_cursor_invalid_limit_retires_the_retained_frame() {
	let store = indexed_graph_cursor_store();
	let mut session = store.session();
	let mut variables = Object::from_iter([("page", Value::from(1))]);
	let prefix = "SELECT id AS picked FROM person WITH INDEX by_rank ORDER BY rank ASC NULLS \
	              FIRST LIMIT $page";
	let first = session
		.execute(&format!("{prefix} RETURN GRAPH {{nodes: picked, edges: []}}"), &variables)
		.unwrap()
		.remove(0);
	variables.insert("cursor", first.continuation.unwrap());
	variables.insert("page", Value::from(-1));
	let query = format!("{prefix} START AFTER $cursor RETURN GRAPH {{nodes: picked, edges: []}}");
	assert!(session.execute(&query, &variables).unwrap()[0].result.is_err());
	variables.insert("page", Value::from(1));
	assert!(session.execute(&query, &variables).unwrap()[0].result.is_err());
}

#[test]
fn graph_cursor_only_keeps_cardinality_and_has_no_retained_page() {
	let store = indexed_graph_cursor_store();
	let mut session = store.session();
	let prefix =
		"SELECT ONLY id AS picked FROM person WITH INDEX by_rank ORDER BY rank ASC NULLS FIRST";
	let first = session
		.execute(
			&format!("{prefix} LIMIT 1 RETURN GRAPH {{nodes: picked, edges: []}}"),
			&Object::new(),
		)
		.unwrap()
		.remove(0);
	assert!(first.result.is_ok());
	assert!(first.continuation.is_none());
	let next = session
		.execute(
			&format!("{prefix} LIMIT 2 RETURN GRAPH {{nodes: picked, edges: []}}"),
			&Object::new(),
		)
		.unwrap()
		.remove(0);
	let error = next.result.unwrap_err();
	assert!(matches!(
		error.status().status(),
		crate::gql_status::Status::SelectCardinalityViolation { .. }
	));
	let ordinary = prefix.replace("SELECT ONLY", "SELECT");
	let page = session
		.execute(
			&format!("{ordinary} LIMIT 1 RETURN GRAPH {{nodes: picked, edges: []}}"),
			&Object::new(),
		)
		.unwrap()
		.remove(0);
	let variables = Object::from_iter([("cursor", Value::from(page.continuation.unwrap()))]);
	assert!(
		session
			.execute(
				&format!(
					"{prefix} LIMIT 2 START AFTER $cursor RETURN GRAPH {{nodes: picked, edges: \
					 []}}"
				),
				&variables
			)
			.unwrap()[0]
			.result
			.is_err()
	);
}

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
	let local = children[0].node_ids().unwrap().next().unwrap().unwrap();
	let first_mapping = children[0].materialization_origin().unwrap().source_atom(local).unwrap();
	let second_mapping = children[1].materialization_origin().unwrap().source_atom(local).unwrap();
	assert!(std::ptr::eq(first_mapping, second_mapping));

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
