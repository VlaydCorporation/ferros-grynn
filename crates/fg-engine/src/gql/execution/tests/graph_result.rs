use std::num::NonZeroUsize;

#[cfg(test)]
use crate::test_assertions::{AssertOption as _, AssertResult as _};
use crate::{
	datastore::{ClassSchema, Datastore},
	gql::{QueryCancellation, QueryExecutionOptions, QueryStatementKind},
	graph::{AtomKind, EdgeTopology, GraphKind, GraphRead, NodeRead},
	values::{Object, PropertyKeyId, PropertyMap, RecordId, Value},
};

/// Builds the select graph fixture used by nearby scenarios.
fn select_graph_fixture() -> (Datastore, Object) {
	let store = Datastore::in_memory();
	let mut seed = store.begin();
	seed.define_class(ClassSchema::new("person", AtomKind::Node, []).assert_ok()).assert_ok();
	seed.define_class(ClassSchema::new("link", AtomKind::Edge, []).assert_ok()).assert_ok();
	let source = seed.define_graph("source", GraphKind::Graph).assert_ok();
	let left = "person:left".parse::<RecordId>().assert_ok();
	let right = "person:right".parse::<RecordId>().assert_ok();
	let edge = "link:edge".parse::<RecordId>().assert_ok();
	for record in [&left, &right, &edge] {
		seed.create_atom(record.clone(), PropertyMap::new()).assert_ok();
	}
	let left_id = seed.attach_node(source.id(), &left, None, None).assert_ok();
	let right_id = seed.attach_node(source.id(), &right, None, None).assert_ok();
	seed.attach_edge(
		source.id(),
		&edge,
		None,
		EdgeTopology::directed([left_id], [right_id]),
		None,
		None,
	)
	.assert_ok();
	seed.commit().assert_ok();
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

/// Verifies select terminal graph consumes projected window group and join
/// rows.
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
		let outcomes = store.execute(query, &variables).assert_ok();
		let value = outcomes[0].result.as_ref().assert_ok().value();
		let graph = match value {
			Value::Graph(graph) => Some(graph),
			_ => None,
		}
		.assert_some_with("expected SELECT graph result");
		assert_eq!(graph.kind(), GraphKind::Subgraph, "{query}");
		assert_eq!(graph.node_ids().assert_ok().count(), expected_nodes, "{query}");
		assert_eq!(graph.edge_ids().assert_ok().count(), 0, "{query}");
	}
	let outcomes = store
		.execute(
			"SELECT ONLY id AS picked FROM person RETURN GRAPH {nodes: picked, edges: []}",
			&variables,
		)
		.assert_ok();
	assert!(outcomes[0].result.is_err());
}

/// Verifies select terminal graph uses common family validators and endpoint
/// closure.
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
		let outcomes = store.execute(&query, &variables).assert_ok();
		let graph = match outcomes[0].result.as_ref().assert_ok().value() {
			Value::Graph(graph) => Some(graph),
			_ => None,
		}
		.assert_some_with("expected exact graph family");
		assert_eq!(graph.kind(), expected_kind, "{query}");
		assert_eq!(graph.node_ids().assert_ok().count(), 2);
		assert_eq!(graph.edge_ids().assert_ok().count(), 1);
	}
	let outcomes = store
		.execute(
			"SELECT id AS picked FROM person WHERE id = person:left RETURN GRAPH <ROOTED_TREE> \
			 ROOT person:right {nodes: picked, edges: []}",
			&variables,
		)
		.assert_ok();
	assert!(outcomes[0].result.is_err());
}

/// Verifies terminal select graph is one scalar subquery value.
#[test]
fn terminal_select_graph_is_one_scalar_subquery_value() {
	let (store, variables) = select_graph_fixture();
	let outcomes = store
		.execute(
			"SELECT VALUE (SELECT p.id AS picked FROM person AS p RETURN GRAPH {nodes: picked, \
			 edges: []}) FROM person:left",
			&variables,
		)
		.assert_ok();
	let values = match outcomes[0].result.as_ref().assert_ok().value() {
		Value::Array(values) => Some(values),
		_ => None,
	}
	.assert_some_with("expected outer SELECT array");
	assert_eq!(values.len(), 1);
	let graph = match values.iter().next().assert_some() {
		Value::Graph(graph) => Some(graph),
		_ => None,
	}
	.assert_some_with("expected one materialized graph value");
	assert_eq!(graph.node_ids().assert_ok().count(), 2);
}

/// Verifies explicit root none or unselected record is rejected by shared
/// assembly.
#[test]
fn explicit_root_none_or_unselected_record_is_rejected_by_shared_assembly() {
	let (store, variables) = select_graph_fixture();
	for root in ["NONE", "person:right"] {
		let terminal = format!(
			"SELECT id AS picked FROM person WHERE id = person:left RETURN GRAPH <ROOTED_TREE> \
			 ROOT {root} {{nodes: picked, edges: []}}"
		);
		let outcomes = store.execute(&terminal, &variables).assert_ok();
		assert!(outcomes[0].result.is_err(), "{terminal}");
		let build = format!(
			"BUILD GRAPH <ROOTED_TREE> FROM (SELECT id AS picked FROM person WHERE id = \
			 person:left) NODES picked ROOT {root}"
		);
		let outcomes = store.execute(&build, &variables).assert_ok();
		assert!(outcomes[0].result.is_err(), "{build}");
	}
}

/// Verifies graph terminal derived and build sources expose one graph value.
#[test]
fn graph_terminal_derived_and_build_sources_expose_one_graph_value() {
	let (store, variables) = select_graph_fixture();
	let inner = "SELECT id AS picked, $edge AS connector FROM person LIMIT 1 RETURN GRAPH <GRAPH> \
	             {nodes: picked, edges: connector}";
	let outcomes =
		store.execute(&format!("SELECT VALUE d.graph FROM ({inner}) AS d"), &variables).assert_ok();
	let values = match outcomes[0].result.as_ref().assert_ok().value() {
		Value::Array(values) => Some(values),
		_ => None,
	}
	.assert_some_with("expected one derived row");
	assert_eq!(values.len(), 1);
	assert!(
		matches!(values.iter().next(), Some(Value::Graph(graph)) if graph.node_count() == 2 && graph.edge_count() == 1)
	);
	let outcomes =
		store.execute(&format!("BUILD GRAPH <GRAPH> FROM ({inner})"), &variables).assert_ok();
	assert!(
		matches!(outcomes[0].result.as_ref().assert_ok().value(), Value::Graph(graph) if graph.node_count() == 2 && graph.edge_count() == 1)
	);
	let invalid = "SELECT VALUE d.graph FROM (SELECT id AS picked FROM person WHERE id = \
	               person:left RETURN GRAPH <ROOTED_TREE> ROOT person:right {nodes: picked, \
	               edges: []}) AS d";
	assert!(store.execute(invalid, &variables).assert_ok()[0].result.is_err());
}

/// Verifies value object keeps its single projected column for graph selectors.
#[test]
fn value_object_keeps_its_single_projected_column_for_graph_selectors() {
	let (store, variables) = select_graph_fixture();
	for query in [
		"SELECT VALUE {node: id} AS picked FROM person RETURN GRAPH {nodes: picked.node, edges: \
		 []}",
		"BUILD GRAPH FROM (SELECT VALUE {node: id} AS picked FROM person) NODES picked.node",
	] {
		let outcomes = store.execute(query, &variables).assert_ok();
		assert!(
			matches!(outcomes[0].result.as_ref().assert_ok().value(), Value::Graph(graph) if graph.node_count() == 2),
			"{query}"
		);
	}
}

/// Verifies terminal graph pages use the retained session cursor.
#[test]
fn terminal_graph_pages_use_the_retained_session_cursor() {
	let (store, mut variables) = select_graph_fixture();
	let mut third = store.begin();
	third.create_atom("person:third".parse().assert_ok(), PropertyMap::new()).assert_ok();
	third.commit().assert_ok();
	let mut session = store.session();
	let first = session
		.execute(
			"SELECT id AS picked FROM person ORDER BY id LIMIT 1 RETURN GRAPH {nodes: picked, \
			 edges: []}",
			&variables,
		)
		.assert_ok()
		.remove(0);
	let cursor = first.continuation.assert_some();
	variables.insert("cursor", cursor);
	let query = "SELECT id AS picked FROM person ORDER BY id LIMIT 1 START AFTER $cursor RETURN \
	             GRAPH {nodes: picked, edges: []}";
	let second = session.execute(query, &variables).assert_ok().remove(0);
	let replay = session.execute(query, &variables).assert_ok().remove(0);
	assert!(second.continuation.is_some());
	let first = match first.result.assert_ok().into_parts().0 {
		Value::Graph(first) => Some(first),
		_ => None,
	}
	.assert_some_with("expected first graph page");
	let second = match second.result.assert_ok().into_parts().0 {
		Value::Graph(second) => Some(second),
		_ => None,
	}
	.assert_some_with("expected second graph page");
	let replay = match replay.result.assert_ok().into_parts().0 {
		Value::Graph(replay) => Some(replay),
		_ => None,
	}
	.assert_some_with("expected replay graph page");
	let records = |graph: &crate::graph::MaterializedGraph| {
		graph
			.node_ids()
			.assert_ok()
			.map(|id| graph.node(id.assert_ok()).assert_some().record_id().clone())
			.collect::<Vec<_>>()
	};
	assert_ne!(records(&first), records(&second));
	assert_eq!(records(&second), records(&replay));
}

/// Verifies explain build keeps the nested select graph assembly.
#[test]
fn explain_build_keeps_the_nested_select_graph_assembly() {
	let (store, variables) = select_graph_fixture();
	let outcomes = store
		.execute(
			"EXPLAIN FULL FORMAT JSON BUILD GRAPH <GRAPH> FROM (SELECT id AS picked FROM person \
			 RETURN GRAPH <FOREST> {nodes: picked, edges: []})",
			&variables,
		)
		.assert_ok();
	let explain = match outcomes[0].result.as_ref().assert_ok().value() {
		Value::Object(explain) => Some(explain),
		_ => None,
	}
	.assert_some_with("expected explain object");
	let nodes = match explain.get("nodes") {
		Some(Value::Array(nodes)) => Some(nodes),
		_ => None,
	}
	.assert_some_with("expected physical nodes");
	assert_eq!(nodes.iter().filter(|node| matches!(node, Value::Object(node) if node.get("operator") == Some(&Value::from("GraphAssemble")))).count(), 2);
}

/// Verifies computed primary graph scope is read once for rows and assembly.
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
			.assert_ok()
			.iter()
			.all(|outcome| outcome.result.is_ok())
	);
	let root = store.inner.current_root();
	let setting = "settings:scope".parse::<RecordId>().assert_ok();
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
	let syntax = crate::gql::parse(source, crate::syn::ParserSettings::default()).assert_ok();
	let logical = crate::gql::analysis::analyze(&syntax, source, &read, &variables).assert_ok();
	let plan = match &logical.statements[0] {
		LogicalStatement::Select(plan) => Some(plan),
		_ => None,
	}
	.assert_some_with("expected terminal SELECT");
	let slots = crate::gql::execution::initialize_slots(&logical, &variables);
	let graph = crate::gql::execution::select::execute_select(
		&read,
		read.snapshot().assert_ok(),
		plan,
		&slots,
		source,
		&control,
	)
	.assert_ok();
	assert!(matches!(graph, Value::Graph(graph) if graph.node_count() == 2));
	assert_eq!(reads.get(), 1);
}

/// Verifies graph terminal only distinguishes empty window from value none.
#[test]
fn graph_terminal_only_distinguishes_empty_window_from_value_none() {
	let (store, variables) = select_graph_fixture();
	let empty = store
		.execute(
			"SELECT ONLY * FROM person WHERE false RETURN GRAPH {nodes: id, edges: []}",
			&variables,
		)
		.assert_ok();
	assert!(
		matches!(empty[0].result.as_ref().assert_ok().value(), Value::Graph(graph) if graph.node_count() == 0)
	);
	let one = store
		.execute(
			"SELECT ONLY VALUE NONE AS picked FROM person:left RETURN GRAPH {nodes: $root, edges: \
			 []}",
			&variables,
		)
		.assert_ok();
	assert!(
		matches!(one[0].result.as_ref().assert_ok().value(), Value::Graph(graph) if graph.node_count() == 1)
	);
}

/// Verifies partial graph assembly retains qualified source mapping.
#[test]
fn partial_graph_assembly_retains_qualified_source_mapping() {
	let (store, variables) = select_graph_fixture();
	let outcomes = store
		.execute(
			"SELECT $edge AS picked FROM person LIMIT 1 RETURN GRAPH <GRAPH> {nodes: [], edges: \
			 picked}",
			&variables,
		)
		.assert_ok();
	let graph = match outcomes[0].result.as_ref().assert_ok().value() {
		Value::Graph(graph) => Some(graph),
		_ => None,
	}
	.assert_some_with("expected graph result");
	let graph = match graph.as_ref() {
		crate::graph::MaterializedGraph::Graph(graph) => Some(graph),
		_ => None,
	}
	.assert_some_with("expected general family");
	let origin = graph.materialization_origin().assert_some();
	let edge = match variables.get("edge").assert_some() {
		Value::AtomOccurrence(edge) => Some(edge),
		_ => None,
	}
	.assert_some_with("expected qualified edge input");
	assert_eq!(origin.graph_ref().assert_some().id(), edge.graph().id());
	for atom in graph.node_ids().assert_ok().chain(graph.edge_ids().assert_ok()) {
		let atom = atom.assert_ok();
		assert_eq!(origin.source_atom(atom), GraphRead::atom_ref(graph, atom).assert_ok().as_ref());
	}
	drop(store);
	assert_eq!(graph.node_count(), 2);
}

/// Verifies embedded graph terminals do not publish an inner cursor.
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
		let result = session.execute(query, &variables).assert_ok().remove(0);
		assert!(result.result.is_ok(), "{query}");
		assert!(result.continuation.is_none(), "{query}");
	}
}

/// Verifies exact graph continuations use the original root for memory and
/// index pages.
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
				.assert_ok()
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
			.assert_ok()
			.remove(0);
		let token = first.continuation.assert_some();
		assert!(
			store.execute("UPDATE person:b SET mark = 99;", &Object::new()).assert_ok()[0]
				.result
				.is_ok()
		);
		if indexed && !read_committed {
			assert!(
				store.execute("REMOVE INDEX by_rank ON CLASS person;", &Object::new()).assert_ok()
					[0]
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
			.assert_ok()
			.remove(0);
		let graph = match second.result.assert_ok().into_parts().0 {
			Value::Graph(graph) => Some(graph),
			_ => None,
		}
		.assert_some_with("expected graph continuation");
		let node =
			graph.node(graph.node_ids().assert_ok().next().assert_some().assert_ok()).assert_some();
		assert_eq!(node.record_id().to_string(), "person:b");
		let expected = if read_committed {
			99
		} else {
			22
		};
		assert!(node.properties().iter().any(|(_key, value)| value == &Value::from(expected)));
	}
}

/// Verifies exact graph continuation keeps the original selector parameters.
#[test]
fn exact_graph_continuation_keeps_the_original_selector_parameters() {
	let (store, mut variables) = select_graph_fixture();
	let mut third = store.begin();
	third.create_atom("person:third".parse().assert_ok(), PropertyMap::new()).assert_ok();
	third.commit().assert_ok();
	variables.insert("selection", variables.get("root").assert_some().clone());
	let mut session = store.session();
	let first = session
		.execute(
			"SELECT id AS picked FROM person ORDER BY id LIMIT 1 RETURN GRAPH {nodes: $selection, \
			 edges: []}",
			&variables,
		)
		.assert_ok()
		.remove(0);
	variables.insert("cursor", first.continuation.assert_some());
	let root = match variables.get("root").assert_some() {
		Value::AtomOccurrence(root) => Some(root),
		_ => None,
	}
	.assert_some_with("expected qualified root");
	let other = crate::graph::AtomOccurrenceRef::new(
		crate::graph::AtomRef::new(AtomKind::Node, "person:right".parse().assert_ok()),
		std::sync::Arc::new(root.graph().clone()),
	);
	variables.insert("selection", other);
	let second = session
		.execute(
			"SELECT id AS picked FROM person ORDER BY id LIMIT 1 START AFTER $cursor RETURN GRAPH \
			 {nodes: $selection, edges: []}",
			&variables,
		)
		.assert_ok()
		.remove(0);
	let graph = match second.result.assert_ok().into_parts().0 {
		Value::Graph(graph) => Some(graph),
		_ => None,
	}
	.assert_some_with("expected exact graph page");
	let node =
		graph.node(graph.node_ids().assert_ok().next().assert_some().assert_ok()).assert_some();
	assert_eq!(node.record_id().to_string(), "person:left");
}

/// Verifies graph cursor capture admits unused parameter payload before
/// retaining it.
#[test]
fn graph_cursor_capture_admits_unused_parameter_payload_before_retaining_it() {
	let (store, mut variables) = select_graph_fixture();
	let mut session = store.session();
	let query = "SELECT id AS picked FROM person ORDER BY id LIMIT 1 RETURN GRAPH {nodes: picked, \
	             edges: []}";
	let options = QueryExecutionOptions {
		barrier_memory_bytes: NonZeroUsize::new(64 * 1024).assert_some(),
		..QueryExecutionOptions::default()
	};
	let first =
		session.execute_with_options(query, &variables, options.clone()).assert_ok().remove(0);
	assert!(first.result.is_ok());
	session.close_cursor(&first.continuation.assert_some()).assert_ok();
	variables.insert("unused", Value::from("x".repeat(1024 * 1024)));
	let rejected = session.execute_with_options(query, &variables, options).assert_ok().remove(0);
	assert!(rejected.result.is_err());
	assert!(rejected.continuation.is_none());
}

/// Verifies exact graph cursor closures check blocks and keep lexical captures.
#[test]
fn exact_graph_cursor_closures_check_blocks_and_keep_lexical_captures() {
	let (store, mut variables) = select_graph_fixture();
	variables.insert("selection", variables.get("root").assert_some().clone());
	variables.insert("other", variables.get("root").assert_some().clone());
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
			.assert_ok()
			.remove(0);
		assert!(first.result.is_ok());
		frame.insert("cursor", first.continuation.assert_some());
		let changed = session
			.execute(
				&format!(
					"{prefix} START AFTER $cursor RETURN GRAPH {{nodes: {}, edges: []}}",
					selector.replace("$selection", "$other")
				),
				&frame,
			)
			.assert_ok()
			.remove(0);
		assert!(changed.result.is_err());
		let same = session
			.execute(
				&format!(
					"{prefix} START AFTER $cursor RETURN GRAPH {{nodes: {selector}, edges: []}}"
				),
				&frame,
			)
			.assert_ok()
			.remove(0);
		assert!(same.result.is_ok());
	}
}

/// Returns indexed graph cursor store for this module.
fn indexed_graph_cursor_store() -> Datastore {
	let store = Datastore::in_memory();
	let results = store
		.execute(
			"DEFINE CLASS person TYPE NODE SCHEMAFULL; DEFINE FIELD rank ON CLASS person TYPE \
			 int; DEFINE INDEX by_rank ON CLASS person FIELDS rank UNIQUE SCOPE BASE; CREATE \
			 person:a SET rank = 1; CREATE person:b SET rank = 2; CREATE person:c SET rank = 3;",
			&Object::new(),
		)
		.assert_ok();
	assert!(results.iter().all(|result| result.result.is_ok()));
	store
}

/// Verifies exact graph cursor invalid limit retires the retained frame.
#[test]
fn exact_graph_cursor_invalid_limit_retires_the_retained_frame() {
	let store = indexed_graph_cursor_store();
	let mut session = store.session();
	let mut variables = Object::from_iter([("page", Value::from(1))]);
	let prefix = "SELECT id AS picked FROM person WITH INDEX by_rank ORDER BY rank ASC NULLS \
	              FIRST LIMIT $page";
	let first = session
		.execute(&format!("{prefix} RETURN GRAPH {{nodes: picked, edges: []}}"), &variables)
		.assert_ok()
		.remove(0);
	variables.insert("cursor", first.continuation.assert_some());
	variables.insert("page", Value::from(-1));
	let query = format!("{prefix} START AFTER $cursor RETURN GRAPH {{nodes: picked, edges: []}}");
	assert!(session.execute(&query, &variables).assert_ok()[0].result.is_err());
	variables.insert("page", Value::from(1));
	assert!(session.execute(&query, &variables).assert_ok()[0].result.is_err());
}

/// Verifies graph cursor only keeps cardinality and has no retained page.
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
		.assert_ok()
		.remove(0);
	assert!(first.result.is_ok());
	assert!(first.continuation.is_none());
	let next = session
		.execute(
			&format!("{prefix} LIMIT 2 RETURN GRAPH {{nodes: picked, edges: []}}"),
			&Object::new(),
		)
		.assert_ok()
		.remove(0);
	let error = next.result.assert_err();
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
		.assert_ok()
		.remove(0);
	let variables = Object::from_iter([("cursor", Value::from(page.continuation.assert_some()))]);
	assert!(
		session
			.execute(
				&format!(
					"{prefix} LIMIT 2 START AFTER $cursor RETURN GRAPH {{nodes: picked, edges: \
					 []}}"
				),
				&variables
			)
			.assert_ok()[0]
			.result
			.is_err()
	);
}

/// Verifies return graph is owned and closes selected edge endpoints.
#[test]
fn return_graph_is_owned_and_closes_selected_edge_endpoints() {
	let store = Datastore::in_memory();
	let mut seed = store.begin();
	seed.define_class(ClassSchema::new("person", AtomKind::Node, []).assert_ok()).assert_ok();
	seed.define_class(ClassSchema::new("link", AtomKind::Edge, []).assert_ok()).assert_ok();
	let source = seed.define_graph("source", GraphKind::Graph).assert_ok();
	let left = "person:left".parse::<RecordId>().assert_ok();
	let right = "person:right".parse::<RecordId>().assert_ok();
	let edge = "link:edge".parse::<RecordId>().assert_ok();
	for record in [&left, &right, &edge] {
		seed.create_atom(record.clone(), PropertyMap::new()).assert_ok();
	}
	let left_id = seed.attach_node(source.id(), &left, None, None).assert_ok();
	let right_id = seed.attach_node(source.id(), &right, None, None).assert_ok();
	seed.attach_edge(
		source.id(),
		&edge,
		None,
		EdgeTopology::directed([left_id], [right_id]),
		None,
		None,
	)
	.assert_ok();
	seed.commit().assert_ok();

	let value = store
		.execute(
			"MATCH (person:left AS a)-[link:edge AS e]->(person:right AS b) IN GRAPH source \
			 RETURN GRAPH { nodes: [], edges: e }",
			&Object::new(),
		)
		.assert_ok()
		.remove(0)
		.result
		.assert_ok()
		.into_parts()
		.0;
	let bounded = store
		.execute(
			"BUILD GRAPH too_small FROM (MATCH (person:left AS a)-[link:edge AS e]->(person:right \
			 AS b) IN GRAPH source RETURN e) EDGES e LIMIT NODES 1",
			&Object::new(),
		)
		.assert_ok();
	assert!(bounded[0].result.is_err());
	assert!(store.begin().named_graph("too_small").assert_ok().is_none());
	let bounded = store
		.execute(
			"BUILD GRAPH no_edges FROM (MATCH (person:left AS a)-[link:edge AS e]->(person:right \
			 AS b) IN GRAPH source RETURN e) EDGES e LIMIT EDGES 0",
			&Object::new(),
		)
		.assert_ok();
	assert!(bounded[0].result.is_err());
	assert!(store.begin().named_graph("no_edges").assert_ok().is_none());
	drop(store);
	let graph = match value {
		Value::Graph(graph) => Some(graph),
		_ => None,
	}
	.assert_some_with("expected an owned graph value");
	assert_eq!(graph.node_ids().assert_ok().count(), 2);
	assert_eq!(graph.edge_ids().assert_ok().count(), 1);
}

/// Verifies return graph preserves one shared child for multiple owners.
#[test]
fn return_graph_preserves_one_shared_child_for_multiple_owners() {
	let store = Datastore::in_memory();
	let mut seed = store.begin();
	seed.define_class(ClassSchema::new("module", AtomKind::Node, []).assert_ok()).assert_ok();
	seed.define_class(ClassSchema::new("document", AtomKind::Node, []).assert_ok()).assert_ok();
	let child = seed.define_graph("child", GraphKind::Graph).assert_ok();
	let root = seed.define_graph("root", GraphKind::Metagraph).assert_ok();
	let document = "document:spec".parse::<RecordId>().assert_ok();
	let first = "module:first".parse::<RecordId>().assert_ok();
	let second = "module:second".parse::<RecordId>().assert_ok();
	for record in [&document, &first, &second] {
		seed.create_atom(record.clone(), PropertyMap::new()).assert_ok();
	}
	seed.attach_node(child.id(), &document, None, None).assert_ok();
	seed.attach_node(root.id(), &first, None, Some(child.id())).assert_ok();
	seed.attach_node(root.id(), &second, None, Some(child.id())).assert_ok();
	seed.commit().assert_ok();

	let value = store
		.execute(
			"MATCH (document AS d) WITHIN <(module AS owner)> IN GRAPH root RETURN GRAPH { nodes: \
			 d, edges: [] }",
			&Object::new(),
		)
		.assert_ok()
		.remove(0)
		.result
		.assert_ok()
		.into_parts()
		.0;
	let graph = match value {
		Value::Graph(graph) => Some(graph),
		_ => None,
	}
	.assert_some_with("expected graph value");
	let mut children = Vec::new();
	for id in graph.node_ids().assert_ok() {
		let node = graph.node(id.assert_ok()).assert_some();
		if let Some(child) = node.inner_graph() {
			children.push(child.clone());
		}
	}
	assert_eq!(children.len(), 2);
	assert_eq!(children[0].identity(), children[1].identity());
	assert_eq!(children[0].node_ids().assert_ok().count(), 1);
	let local = children[0].node_ids().assert_ok().next().assert_some().assert_ok();
	let first_mapping =
		children[0].materialization_origin().assert_some().source_atom(local).assert_some();
	let second_mapping =
		children[1].materialization_origin().assert_some().source_atom(local).assert_some();
	assert!(std::ptr::eq(first_mapping, second_mapping));

	let mut variables = Object::new();
	variables.insert("graph", Value::GraphRef(Box::new(root)));
	let value = store
		.execute("BUILD GRAPH <META> FROM $graph", &variables)
		.assert_ok()
		.remove(0)
		.result
		.assert_ok()
		.into_parts()
		.0;
	let graph = match value {
		Value::Graph(graph) => Some(graph),
		_ => None,
	}
	.assert_some_with("expected full copied graph value");
	let child_ids = graph
		.node_ids()
		.assert_ok()
		.filter_map(|id| graph.node(id.assert_ok()).assert_some().inner_graph())
		.map(crate::graph::Graph::identity)
		.collect::<Vec<_>>();
	assert_eq!(child_ids.len(), 2);
	assert_eq!(child_ids[0], child_ids[1]);
}

/// Verifies anonymous build accepts one database global node.
#[test]
fn anonymous_build_accepts_one_database_global_node() {
	let store = Datastore::in_memory();
	let mut seed = store.begin();
	seed.define_class(ClassSchema::new("person", AtomKind::Node, []).assert_ok()).assert_ok();
	seed.create_atom("person:solo".parse().assert_ok(), PropertyMap::new()).assert_ok();
	seed.commit().assert_ok();

	let value = store
		.execute("BUILD GRAPH <GRAPH> FROM person:solo", &Object::new())
		.assert_ok()
		.remove(0)
		.result
		.assert_ok()
		.into_parts()
		.0;
	let graph = match value {
		Value::Graph(graph) => Some(graph),
		_ => None,
	}
	.assert_some_with("expected graph value");
	assert_eq!(graph.node_ids().assert_ok().count(), 1);
	assert_eq!(graph.edge_ids().assert_ok().count(), 0);
}

/// Verifies anonymous build handles empty input and rejects invalid or
/// ambiguous scope.
#[test]
fn anonymous_build_handles_empty_input_and_rejects_invalid_or_ambiguous_scope() {
	let store = Datastore::in_memory();
	let mut seed = store.begin();
	let left = seed.define_graph("left", GraphKind::Graph).assert_ok();
	let right = seed.define_graph("right", GraphKind::Graph).assert_ok();
	seed.commit().assert_ok();

	let empty = store
		.execute("BUILD GRAPH FROM []", &Object::new())
		.assert_ok()
		.remove(0)
		.result
		.assert_ok()
		.into_parts()
		.0;
	assert!(
		matches!(empty, Value::Graph(graph) if graph.node_count() == 0 && graph.edge_count() == 0)
	);
	assert!(store.execute("BUILD GRAPH FROM null", &Object::new()).assert_ok()[0].result.is_err());

	let mut variables = Object::new();
	variables.insert(
		"graphs",
		Value::Array(
			vec![Value::GraphRef(Box::new(left)), Value::GraphRef(Box::new(right))].into(),
		),
	);
	let result = store.execute("BUILD GRAPH ambiguous FROM $graphs", &variables).assert_ok();
	assert!(result[0].result.is_err());
	assert!(store.begin().named_graph("ambiguous").assert_ok().is_none());
}

/// Verifies let accepts anonymous build as its entire rhs.
#[test]
fn let_accepts_anonymous_build_as_its_entire_rhs() {
	let store = Datastore::in_memory();
	let mut seed = store.begin();
	seed.define_class(ClassSchema::new("person", AtomKind::Node, []).assert_ok()).assert_ok();
	seed.create_atom("person:solo".parse().assert_ok(), PropertyMap::new()).assert_ok();
	seed.commit().assert_ok();

	let results = store
		.execute("LET $built = BUILD GRAPH <GRAPH> FROM person:solo; RETURN $built", &Object::new())
		.assert_ok();
	let graph = match results[1].result.as_ref().assert_ok().value() {
		Value::Graph(graph) => Some(graph),
		_ => None,
	}
	.assert_some_with("expected graph value bound by LET");
	assert_eq!(graph.node_ids().assert_ok().count(), 1);
}

/// Verifies named build installs existing atoms and topology atomically.
#[test]
fn named_build_installs_existing_atoms_and_topology_atomically() {
	let store = Datastore::in_memory();
	let mut seed = store.begin();
	seed.define_class(ClassSchema::new("person", AtomKind::Node, []).assert_ok()).assert_ok();
	seed.define_class(ClassSchema::new("link", AtomKind::Edge, []).assert_ok()).assert_ok();
	let source = seed.define_graph("source", GraphKind::Graph).assert_ok();
	let left = "person:left".parse::<RecordId>().assert_ok();
	let right = "person:right".parse::<RecordId>().assert_ok();
	let edge = "link:edge".parse::<RecordId>().assert_ok();
	for record in [&left, &right, &edge] {
		seed.create_atom(record.clone(), PropertyMap::new()).assert_ok();
	}
	let left_id = seed.attach_node(source.id(), &left, None, None).assert_ok();
	let right_id = seed.attach_node(source.id(), &right, None, None).assert_ok();
	seed.attach_edge(
		source.id(),
		&edge,
		None,
		EdgeTopology::directed([left_id], [right_id]),
		None,
		None,
	)
	.assert_ok();
	seed.commit().assert_ok();

	let value = store
		.execute(
			"BUILD GRAPH copied FROM (MATCH (person:left AS a)-[link:edge AS e]->(person:right AS \
			 b) IN GRAPH source RETURN a, e, b) NODES [a, b] EDGES e",
			&Object::new(),
		)
		.assert_ok()
		.remove(0)
		.result
		.assert_ok()
		.into_parts()
		.0;
	let reference = match value {
		Value::GraphRef(reference) => Some(reference),
		_ => None,
	}
	.assert_some_with("expected graph reference");
	assert_eq!(reference.name().as_str(), "copied");
	let rows = store
		.execute(
			"MATCH (person:left AS a)-[link:edge]->(person:right AS b) IN GRAPH copied RETURN b",
			&Object::new(),
		)
		.assert_ok();
	assert!(matches!(
		rows[0].result.as_ref().assert_ok().value(),
		Value::Array(values) if values.len() == 1
	));
}

/// Verifies named build reproduces effective overlays without creating base
/// atoms.
#[test]
fn named_build_reproduces_effective_overlays_without_creating_base_atoms() {
	let store = Datastore::in_memory();
	let mut seed = store.begin();
	seed.define_class(ClassSchema::new("person", AtomKind::Node, []).assert_ok()).assert_ok();
	let source = seed.define_graph("source", GraphKind::Graph).assert_ok();
	let person = "person:alice".parse::<RecordId>().assert_ok();
	let inherited = PropertyKeyId::new(101);
	let changed = PropertyKeyId::new(102);
	let mut properties = PropertyMap::new();
	properties.insert(inherited, "base");
	properties.insert(changed, "base");
	seed.create_atom(person.clone(), properties).assert_ok();
	seed.attach_node(source.id(), &person, None, None).assert_ok();
	seed.hide_property(source.id(), &person, inherited).assert_ok();
	seed.set_overlay(source.id(), &person, changed, Value::from("overlay")).assert_ok();
	seed.commit().assert_ok();

	store
		.execute(
			"BUILD GRAPH copied FROM (MATCH (person:alice AS p) IN GRAPH source RETURN p) NODES p",
			&Object::new(),
		)
		.assert_ok();
	let mut read = store.begin();
	let copied = read.named_graph("copied").assert_ok().assert_some();
	assert_eq!(read.effective_property(copied.id(), &person, inherited).assert_ok(), None);
	assert_eq!(
		read.effective_property(copied.id(), &person, changed).assert_ok(),
		Some(Value::from("overlay"))
	);
}

/// Verifies named build keeps one hidden child for multiple owners.
#[test]
fn named_build_keeps_one_hidden_child_for_multiple_owners() {
	let store = Datastore::in_memory();
	let mut seed = store.begin();
	seed.define_class(ClassSchema::new("module", AtomKind::Node, []).assert_ok()).assert_ok();
	seed.define_class(ClassSchema::new("document", AtomKind::Node, []).assert_ok()).assert_ok();
	let child = seed.define_graph("child", GraphKind::Graph).assert_ok();
	let root = seed.define_graph("root", GraphKind::Metagraph).assert_ok();
	let document = "document:spec".parse::<RecordId>().assert_ok();
	let first = "module:first".parse::<RecordId>().assert_ok();
	let second = "module:second".parse::<RecordId>().assert_ok();
	for record in [&document, &first, &second] {
		seed.create_atom(record.clone(), PropertyMap::new()).assert_ok();
	}
	seed.attach_node(child.id(), &document, None, None).assert_ok();
	seed.attach_node(root.id(), &first, None, Some(child.id())).assert_ok();
	seed.attach_node(root.id(), &second, None, Some(child.id())).assert_ok();
	seed.commit().assert_ok();

	store
		.execute(
			"BUILD GRAPH <META> copied FROM (MATCH (document AS d) WITHIN <(module AS owner)> IN \
			 GRAPH root RETURN d, owner) NODES [d, owner]",
			&Object::new(),
		)
		.assert_ok();
	let mut transaction = store.begin();
	let copied = transaction.named_graph("copied").assert_ok().assert_some();
	assert_eq!(transaction.graphs().assert_ok().len(), 3);
	let view = copied.read(&transaction).assert_ok();
	let mut children = Vec::new();
	for id in view.node_ids().assert_ok() {
		let node = view.node(id.assert_ok()).assert_ok().assert_some();
		if let Some(child) = node.inner_graph_ref().assert_ok() {
			children.push(child.into_owned());
		}
	}
	assert_eq!(children.len(), 2);
	assert_eq!(children[0].id(), children[1].id());
	assert!(children[0].name().as_str().starts_with("$fg$build_"));
	assert!(transaction.named_graph(children[0].name()).assert_ok().is_none());
}

/// Verifies overwrite preserves graph identity properties and system indexes.
#[test]
fn overwrite_preserves_graph_identity_properties_and_system_indexes() {
	let store = Datastore::in_memory();
	let mut seed = store.begin();
	seed.define_class(ClassSchema::new("person", AtomKind::Node, []).assert_ok()).assert_ok();
	let source = seed.define_graph("source", GraphKind::Graph).assert_ok();
	let person = "person:alice".parse::<RecordId>().assert_ok();
	seed.create_atom(person.clone(), PropertyMap::new()).assert_ok();
	seed.attach_node(source.id(), &person, None, None).assert_ok();
	seed.commit().assert_ok();
	store
		.execute("DEFINE GRAPH target TYPE GRAPH CONTENT { owner: 'preserved' }", &Object::new())
		.assert_ok();
	let before = store.schema_catalog().named_graph("target").assert_some();
	let index_ids = before.system_indexes().map(|index| index.id()).collect::<Vec<_>>();

	store
		.execute(
			"BUILD GRAPH OVERWRITE <GRAPH> target FROM (MATCH (person:alice AS p) IN GRAPH source \
			 RETURN p) NODES p",
			&Object::new(),
		)
		.assert_ok();
	let after = store.schema_catalog().named_graph("target").assert_some();
	assert_eq!(after.id(), before.id());
	assert_eq!(after.reference().kind(), before.reference().kind());
	assert_eq!(after.properties(), before.properties());
	assert_eq!(after.system_indexes().map(|index| index.id()).collect::<Vec<_>>(), index_ids);
}

/// Verifies if not exists does not evaluate the build source.
#[test]
fn if_not_exists_does_not_evaluate_the_build_source() {
	let store = Datastore::in_memory();
	let mut seed = store.begin();
	seed.define_graph("kept", GraphKind::Graph).assert_ok();
	seed.commit().assert_ok();
	let value = store
		.execute("BUILD GRAPH IF NOT EXISTS kept FROM 1 / 0", &Object::new())
		.assert_ok()
		.remove(0)
		.result
		.assert_ok()
		.into_parts()
		.0;
	assert!(matches!(value, Value::GraphRef(reference) if reference.name().as_str() == "kept"));
}

/// Verifies named build does not create a missing base atom.
#[test]
fn named_build_does_not_create_a_missing_base_atom() {
	let store = Datastore::in_memory();
	let mut seed = store.begin();
	seed.define_class(ClassSchema::new("person", AtomKind::Node, []).assert_ok()).assert_ok();
	seed.commit().assert_ok();
	assert!(
		store.execute("BUILD GRAPH missing FROM person:absent", &Object::new()).assert_ok()[0]
			.result
			.is_err()
	);
	let mut read = store.begin();
	assert!(read.named_graph("missing").assert_ok().is_none());
}

/// Verifies graph scoped select resolves record kind before edge closure.
#[test]
fn graph_scoped_select_resolves_record_kind_before_edge_closure() {
	let store = Datastore::in_memory();
	let mut seed = store.begin();
	seed.define_class(ClassSchema::new("person", AtomKind::Node, []).assert_ok()).assert_ok();
	seed.define_class(ClassSchema::new("link", AtomKind::Edge, []).assert_ok()).assert_ok();
	let source = seed.define_graph("source", GraphKind::Graph).assert_ok();
	let left = "person:left".parse::<RecordId>().assert_ok();
	let right = "person:right".parse::<RecordId>().assert_ok();
	let edge = "link:edge".parse::<RecordId>().assert_ok();
	for record in [&left, &right, &edge] {
		seed.create_atom(record.clone(), PropertyMap::new()).assert_ok();
	}
	let left_id = seed.attach_node(source.id(), &left, None, None).assert_ok();
	let right_id = seed.attach_node(source.id(), &right, None, None).assert_ok();
	seed.attach_edge(
		source.id(),
		&edge,
		None,
		EdgeTopology::directed([left_id], [right_id]),
		None,
		None,
	)
	.assert_ok();
	seed.commit().assert_ok();

	let value = store
		.execute("BUILD GRAPH FROM (SELECT VALUE id FROM link IN GRAPH source)", &Object::new())
		.assert_ok()
		.remove(0)
		.result
		.assert_ok()
		.into_parts()
		.0;
	let graph = match value {
		Value::Graph(graph) => Some(graph),
		_ => None,
	}
	.assert_some_with("expected graph value");
	assert_eq!(graph.node_count(), 2);
	assert_eq!(graph.edge_count(), 1);
}

/// Verifies graph result memory limit fails without publishing named target.
#[test]
fn graph_result_memory_limit_fails_without_publishing_named_target() {
	let store = Datastore::in_memory();
	let mut seed = store.begin();
	seed.define_class(ClassSchema::new("person", AtomKind::Node, []).assert_ok()).assert_ok();
	seed.create_atom("person:solo".parse().assert_ok(), PropertyMap::new()).assert_ok();
	seed.commit().assert_ok();
	let options = QueryExecutionOptions {
		barrier_memory_bytes: NonZeroUsize::MIN,
		..QueryExecutionOptions::default()
	};
	let result = store
		.execute_with_options("BUILD GRAPH bounded FROM person:solo", &Object::new(), options)
		.assert_ok();
	assert!(result[0].result.is_err());
	assert!(store.begin().named_graph("bounded").assert_ok().is_none());

	let limited = store
		.execute("BUILD GRAPH limited FROM person:solo LIMIT NODES 0", &Object::new())
		.assert_ok();
	assert!(limited[0].result.is_err());
	assert!(store.begin().named_graph("limited").assert_ok().is_none());

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
		.assert_ok();
	assert!(cancelled[0].result.is_err());
	assert!(store.begin().named_graph("cancelled").assert_ok().is_none());
}

/// Verifies read only allows anonymous but rejects named build.
#[test]
fn read_only_allows_anonymous_but_rejects_named_build() {
	let store = Datastore::in_memory();
	let mut seed = store.begin();
	seed.define_class(ClassSchema::new("person", AtomKind::Node, []).assert_ok()).assert_ok();
	seed.create_atom("person:solo".parse().assert_ok(), PropertyMap::new()).assert_ok();
	seed.commit().assert_ok();
	let anonymous = store
		.execute("BEGIN READ ONLY; BUILD GRAPH FROM person:solo; COMMIT;", &Object::new())
		.assert_ok();
	assert!(anonymous.iter().all(|result| result.result.is_ok()));
	let named = store
		.execute("BEGIN READ ONLY; BUILD GRAPH forbidden FROM person:solo; COMMIT;", &Object::new())
		.assert_ok();
	assert!(named.iter().any(|result| result.result.is_err()));
	assert!(store.begin().named_graph("forbidden").assert_ok().is_none());
}

/// Verifies atomic import rejects build graph before opening staging.
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

/// Verifies rooted tree contract uses the explicit root and rejects ignored
/// root clauses.
#[test]
fn rooted_tree_contract_uses_the_explicit_root_and_rejects_ignored_root_clauses() {
	let store = Datastore::in_memory();
	let mut seed = store.begin();
	seed.define_class(ClassSchema::new("person", AtomKind::Node, []).assert_ok()).assert_ok();
	seed.define_class(ClassSchema::new("link", AtomKind::Edge, []).assert_ok()).assert_ok();
	let source = seed.define_graph("source", GraphKind::Graph).assert_ok();
	let root = "person:root".parse::<RecordId>().assert_ok();
	let child = "person:child".parse::<RecordId>().assert_ok();
	let edge = "link:edge".parse::<RecordId>().assert_ok();
	for record in [&root, &child, &edge] {
		seed.create_atom(record.clone(), PropertyMap::new()).assert_ok();
	}
	let root_id = seed.attach_node(source.id(), &root, None, None).assert_ok();
	let child_id = seed.attach_node(source.id(), &child, None, None).assert_ok();
	seed.attach_edge(
		source.id(),
		&edge,
		None,
		EdgeTopology::directed([root_id], [child_id]),
		None,
		None,
	)
	.assert_ok();
	seed.commit().assert_ok();
	let source = "(MATCH (person:root AS a)-[link:edge AS e]->(person:child AS b) IN GRAPH source \
	              RETURN a, e, b) ROOT a NODES [a, b] EDGES e";
	let result = store
		.execute(
			&format!("BUILD GRAPH <ROOTED_TREE SIMPLE BIPARTITE COMPLETE> FROM {source}"),
			&Object::new(),
		)
		.assert_ok()
		.remove(0)
		.result
		.assert_ok()
		.into_parts()
		.0;
	assert!(matches!(result, Value::Graph(graph) if graph.kind() == GraphKind::RootedTree));
	assert!(
		store
			.execute(
				&format!("BUILD GRAPH <ROOTED_TREE> FROM {source}").replace("ROOT a", "ROOT b"),
				&Object::new()
			)
			.assert_ok()[0]
			.result
			.is_err()
	);
	assert!(
		store.execute("BUILD GRAPH FROM person:root ROOT person:root", &Object::new()).assert_ok()
			[0]
		.result
		.is_err()
	);
}

/// Verifies explain build graph is effect free until analyze.
#[test]
fn explain_build_graph_is_effect_free_until_analyze() {
	let store = Datastore::in_memory();
	let mut seed = store.begin();
	seed.define_class(ClassSchema::new("person", AtomKind::Node, []).assert_ok()).assert_ok();
	seed.create_atom("person:solo".parse().assert_ok(), PropertyMap::new()).assert_ok();
	seed.commit().assert_ok();

	let planned = store
		.execute("EXPLAIN FULL FORMAT JSON BUILD GRAPH planned FROM person:solo", &Object::new())
		.assert_ok()
		.remove(0);
	assert_eq!(planned.kind, QueryStatementKind::Explain);
	let explain = match planned.result.assert_ok().into_parts().0 {
		Value::Object(explain) => Some(explain),
		_ => None,
	}
	.assert_some_with("expected JSON explain object");
	let nodes = match explain.get("nodes") {
		Some(Value::Array(nodes)) => Some(nodes),
		_ => None,
	}
	.assert_some_with("expected graph-result physical nodes");
	assert!(nodes.iter().any(|node| {
		matches!(node, Value::Object(node) if node.get("operator") == Some(&Value::from("BuildGraph")))
	}));
	assert!(store.begin().named_graph("planned").assert_ok().is_none());

	let analyzed = store
		.execute(
			"EXPLAIN ANALYZE FORMAT JSON BUILD GRAPH published FROM person:solo",
			&Object::new(),
		)
		.assert_ok();
	assert!(analyzed[0].result.is_ok());
	let explain = match analyzed[0].result.as_ref().assert_ok().value() {
		Value::Object(explain) => Some(explain),
		_ => None,
	}
	.assert_some_with("expected JSON explain object");
	let metrics = match explain.get("analyze") {
		Some(Value::Object(metrics)) => Some(metrics),
		_ => None,
	}
	.assert_some_with("expected graph assembly metrics");
	assert_eq!(metrics.get("graph_source_rows"), Some(&Value::from(1_u64)));
	assert_eq!(metrics.get("graph_selected_nodes"), Some(&Value::from(1_u64)));
	assert_eq!(metrics.get("graph_final_nodes"), Some(&Value::from(1_u64)));
	assert_eq!(metrics.get("graph_final_edges"), Some(&Value::from(0_u64)));
	assert!(store.begin().named_graph("published").assert_ok().is_some());
}
