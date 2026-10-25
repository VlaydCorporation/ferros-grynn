#[cfg(test)]
use crate::test_assertions::AssertOption as _;
#[cfg(test)]
use crate::test_assertions::AssertResult as _;
use crate::{
	datastore::{ClassSchema, Datastore, Transaction},
	gql::QueryExecutionOptions,
	graph::{
		AtomKind,
		EdgeEndpoint,
		EdgeTopology,
		EndpointRole,
		GraphKind,
		PathDirection,
		QualifiedAtomId,
	},
	values::{Number, Object, RecordId, Text, Value},
};

/// Returns matching ids for this module.
fn matching_ids(transaction: &mut Transaction, source: &str, column: &str) -> Vec<RecordId> {
	let results = transaction.execute(source, &Object::new()).assert_ok();
	let rows = results[0].result.as_ref().assert_ok().value().as_array().assert_some();
	let mut ids = rows
		.iter()
		.map(|row| {
			row.as_object()
				.assert_some()
				.get(column)
				.assert_some()
				.as_atom_occurrence()
				.assert_some()
				.atom()
				.id()
				.clone()
		})
		.collect::<Vec<_>>();
	ids.sort();
	ids
}

/// Verifies match routes exact class label and intersection without legacy
/// syntax.
#[test]
fn match_routes_exact_class_label_and_intersection_without_legacy_syntax() {
	let store = Datastore::in_memory();
	let mut seed = store.begin();
	seed.define_class(ClassSchema::new("person", AtomKind::Node, []).assert_ok()).assert_ok();
	seed.define_class(ClassSchema::new("device", AtomKind::Node, []).assert_ok()).assert_ok();
	seed.define_class(ClassSchema::new("friendship", AtomKind::Edge, []).assert_ok()).assert_ok();
	let graph = seed.define_graph("social", GraphKind::Graph).assert_ok();
	let alice = "person:alice".parse::<RecordId>().assert_ok();
	let bob = "person:bob".parse::<RecordId>().assert_ok();
	let router = "device:router".parse::<RecordId>().assert_ok();
	let friendship = "friendship:shared".parse::<RecordId>().assert_ok();
	for record in [&alice, &bob, &router, &friendship] {
		seed.create_atom(record.clone(), crate::values::PropertyMap::default()).assert_ok();
	}
	let alice_local =
		seed.attach_node(graph.id(), &alice, Some(Text::from("Employee")), None).assert_ok();
	let bob_local =
		seed.attach_node(graph.id(), &bob, Some(Text::from("Visitor")), None).assert_ok();
	seed.attach_node(graph.id(), &router, Some(Text::from("Employee")), None).assert_ok();
	seed.attach_edge(
		graph.id(),
		&friendship,
		Some(Text::from("Close")),
		EdgeTopology::directed([alice_local], [bob_local]),
		None,
		None,
	)
	.assert_ok();
	seed.commit().assert_ok();

	let mut transaction = store.begin();
	assert_eq!(
		matching_ids(&mut transaction, "MATCH (person:alice AS n) IN GRAPH social RETURN n", "n",),
		std::slice::from_ref(&alice)
	);
	assert!(
		matching_ids(
			&mut transaction,
			"MATCH (person:alice AS n LABEL Visitor) IN GRAPH social RETURN n",
			"n",
		)
		.is_empty()
	);
	assert_eq!(
		matching_ids(&mut transaction, "MATCH (person AS n) IN GRAPH social RETURN n", "n"),
		[alice.clone(), bob.clone()]
	);
	assert_eq!(
		matching_ids(
			&mut transaction,
			"MATCH (* AS n LABEL Employee) IN GRAPH social RETURN n",
			"n",
		),
		[router, alice.clone()]
	);
	assert_eq!(
		matching_ids(
			&mut transaction,
			"MATCH (person AS n LABEL Employee) IN GRAPH social RETURN n",
			"n",
		),
		std::slice::from_ref(&alice)
	);
	assert!(
		matching_ids(
			&mut transaction,
			"MATCH (person:alice AS n), (person AS n LABEL Visitor) IN GRAPH social RETURN n",
			"n",
		)
		.is_empty()
	);
	assert_eq!(
		matching_ids(
			&mut transaction,
			"MATCH (person:alice AS a)-[friendship:shared AS e LABEL Close]->(person:bob AS b) IN \
			 GRAPH social RETURN e",
			"e",
		),
		[friendship]
	);

	let legacy = transaction
		.execute("MATCH (person AS n) FROM social RETURN n", &Object::new())
		.assert_err();
	assert_eq!(legacy.status().code().to_string(), "FG-42001");
}

/// Verifies match shortest uses bfs dijkstra and path functions.
#[test]
fn match_shortest_uses_bfs_dijkstra_and_path_functions() {
	let store = Datastore::in_memory();
	let mut seed = store.begin();
	seed.define_class(ClassSchema::new("person", AtomKind::Node, []).assert_ok()).assert_ok();
	seed.define_class(ClassSchema::new("link", AtomKind::Edge, []).assert_ok()).assert_ok();
	let graph = seed.define_graph("routes", GraphKind::Graph).assert_ok();
	let nodes = ["person:a", "person:b", "person:c", "person:d"]
		.map(|value| value.parse::<RecordId>().assert_ok());
	let edges = ["link:direct", "link:ab", "link:bd", "link:ac", "link:cd"]
		.map(|value| value.parse::<RecordId>().assert_ok());
	for record in nodes.iter().chain(&edges) {
		seed.create_atom(record.clone(), crate::values::PropertyMap::default()).assert_ok();
	}
	let local = nodes
		.iter()
		.map(|record| seed.attach_node(graph.id(), record, None, None).assert_ok())
		.collect::<Vec<_>>();
	for (record, source, target, weight) in [
		(&edges[0], local[0], local[3], 10_i64),
		(&edges[1], local[0], local[1], 1_i64),
		(&edges[2], local[1], local[3], 1_i64),
		(&edges[3], local[0], local[2], 1_i64),
		(&edges[4], local[2], local[3], 1_i64),
	] {
		seed.attach_edge(
			graph.id(),
			record,
			None,
			EdgeTopology::directed([source], [target]),
			Some(Number::Int(weight)),
			None,
		)
		.assert_ok();
	}
	seed.commit().assert_ok();

	let mut transaction = store.begin();
	let query = |transaction: &mut Transaction, objective: &str| {
		let source = format!(
			"MATCH path = (person:a AS a)-[link*1..3 {objective}]->(person:d AS d) IN GRAPH \
			 routes RETURN path::length(path) AS length, path::cost(path) AS cost, \
			 path::atoms(path) AS atoms"
		);
		transaction
			.execute(&source, &Object::new())
			.assert_ok()
			.remove(0)
			.result
			.assert_ok()
			.value()
			.clone()
	};
	let unweighted = query(&mut transaction, "SHORTEST");
	let unweighted =
		unweighted.as_array().assert_some().get(0).assert_some().as_object().assert_some();
	assert_eq!(unweighted.get("length"), Some(&Value::from(1_i64)));
	assert_eq!(unweighted.get("cost"), Some(&Value::from(1_i64)));
	assert_eq!(unweighted.get("atoms").and_then(Value::as_array).assert_some().len(), 3);

	let weighted = query(&mut transaction, "WEIGHTED SHORTEST");
	let weighted = weighted.as_array().assert_some().get(0).assert_some().as_object().assert_some();
	assert_eq!(weighted.get("length"), Some(&Value::from(2_i64)));
	assert_eq!(weighted.get("cost"), Some(&Value::from(2_i64)));
	let atoms = weighted.get("atoms").and_then(Value::as_array).assert_some();
	assert_eq!(atoms.len(), 5);
	assert_eq!(
		atoms.get(2).assert_some().as_atom_occurrence().assert_some().atom().id(),
		&nodes[1],
		"equal-cost paths use public occurrence identity as the canonical tie-break"
	);
	let all = query(&mut transaction, "ALL WEIGHTED SHORTEST");
	let all = all.as_array().assert_some();
	assert_eq!(all.len(), 2);
	for row in all.iter() {
		let row = row.as_object().assert_some();
		assert_eq!(row.get("length"), Some(&Value::from(2_i64)));
		assert_eq!(row.get("cost"), Some(&Value::from(2_i64)));
	}
	let all_hop = query(&mut transaction, "ALL SHORTEST");
	assert_eq!(all_hop.as_array().assert_some().len(), 1);
	let traversed = transaction
		.execute(
			"TRAVERSE FROM [person:a,person:a] IN GRAPH @routes |> \
			 tv::allWeightedShortestPaths(person:d);",
			&Object::new(),
		)
		.assert_ok();
	let traversed = traversed[0].result.as_ref().assert_ok().value().as_array().assert_some();
	assert_eq!(traversed.len(), 4);
	assert!(traversed.iter().all(|value| matches!(value, Value::Path(_))));
}

/// Verifies graph first match executes hyper and within relations.
#[test]
fn graph_first_match_executes_hyper_and_within_relations() {
	let store = Datastore::in_memory();
	let mut seed = store.begin();
	for (class, kind) in [
		("person", AtomKind::Node),
		("document", AtomKind::Node),
		("module", AtomKind::Node),
		("meeting", AtomKind::Edge),
	] {
		seed.define_class(ClassSchema::new(class, kind, []).assert_ok()).assert_ok();
	}
	let knowledge = seed.define_graph("knowledge", GraphKind::Hypergraph).assert_ok();
	let child = seed.define_graph("child", GraphKind::Graph).assert_ok();
	let meta = seed.define_graph("meta", GraphKind::Metagraph).assert_ok();
	let alice = "person:alice".parse::<RecordId>().assert_ok();
	let bob = "person:bob".parse::<RecordId>().assert_ok();
	let doc = "document:spec".parse::<RecordId>().assert_ok();
	let module = "module:root".parse::<RecordId>().assert_ok();
	let meeting = "meeting:review".parse::<RecordId>().assert_ok();
	for record in [&alice, &bob, &doc, &module, &meeting] {
		seed.create_atom(record.clone(), crate::values::PropertyMap::default()).assert_ok();
	}
	let alice_local = seed.attach_node(knowledge.id(), &alice, None, None).assert_ok();
	let bob_local = seed.attach_node(knowledge.id(), &bob, None, None).assert_ok();
	let doc_local = seed.attach_node(knowledge.id(), &doc, None, None).assert_ok();
	seed.attach_edge(
		knowledge.id(),
		&meeting,
		None,
		EdgeTopology::directed([alice_local, bob_local], [doc_local]),
		None,
		None,
	)
	.assert_ok();
	seed.attach_node(child.id(), &doc, None, None).assert_ok();
	seed.attach_node(meta.id(), &module, None, Some(child.id())).assert_ok();
	seed.commit().assert_ok();

	let mut transaction = store.begin();
	let hyper = transaction
		.execute(
			"MATCH HYPER [meeting AS e] CONTAINS HEAD [(person AS p)], TAIL [(document AS d)] IN \
			 GRAPH knowledge RETURN p, d",
			&Object::new(),
		)
		.assert_ok();
	let rows = hyper[0].result.as_ref().assert_ok().value().as_array().assert_some();
	assert_eq!(rows.len(), 2);

	let within = transaction
		.execute(
			"MATCH (document AS d) WITHIN <(module AS m)> IN GRAPH meta RETURN d, m",
			&Object::new(),
		)
		.assert_ok();
	let rows = within[0].result.as_ref().assert_ok().value().as_array().assert_some();
	assert_eq!(rows.len(), 1);
	let row = rows.get(0).assert_some().as_object().assert_some();
	assert_eq!(
		row.get("d").assert_some().as_atom_occurrence().assert_some().graph().id(),
		child.id()
	);
	assert_eq!(
		row.get("m").assert_some().as_atom_occurrence().assert_some().graph().id(),
		meta.id()
	);
}

/// Verifies atomwalk supports vertex edge and edge edge incidence.
#[test]
fn atomwalk_supports_vertex_edge_and_edge_edge_incidence() {
	let store = Datastore::in_memory();
	let mut seed = store.begin();
	seed.define_class(ClassSchema::new("person", AtomKind::Node, []).assert_ok()).assert_ok();
	seed.define_class(ClassSchema::new("relation", AtomKind::Edge, []).assert_ok()).assert_ok();
	let graph = seed.define_graph("atomwalk", GraphKind::Graph).assert_ok();
	let a = "person:a".parse::<RecordId>().assert_ok();
	let b = "person:b".parse::<RecordId>().assert_ok();
	let target = "relation:target".parse::<RecordId>().assert_ok();
	let vertex_edge = "relation:ve".parse::<RecordId>().assert_ok();
	let edge_vertex = "relation:ev".parse::<RecordId>().assert_ok();
	let edge_edge = "relation:ee".parse::<RecordId>().assert_ok();
	for record in [&a, &b, &target, &vertex_edge, &edge_vertex, &edge_edge] {
		seed.create_atom(record.clone(), crate::values::PropertyMap::default()).assert_ok();
	}
	let a_local = seed.attach_node(graph.id(), &a, None, None).assert_ok();
	let b_local = seed.attach_node(graph.id(), &b, None, None).assert_ok();
	let target_local = seed
		.attach_edge(
			graph.id(),
			&target,
			None,
			EdgeTopology::directed([a_local], [b_local]),
			None,
			None,
		)
		.assert_ok();
	let vertex_edge_local = seed
		.attach_edge(
			graph.id(),
			&vertex_edge,
			None,
			EdgeTopology::directed([a_local], [target_local]),
			None,
			None,
		)
		.assert_ok();
	seed.attach_edge(
		graph.id(),
		&edge_vertex,
		None,
		EdgeTopology::directed([target_local], [b_local]),
		None,
		None,
	)
	.assert_ok();
	seed.attach_edge(
		graph.id(),
		&edge_edge,
		None,
		EdgeTopology::directed([target_local], [vertex_edge_local]),
		None,
		None,
	)
	.assert_ok();
	seed.commit().assert_ok();

	let mut transaction = store.begin();
	for query in [
		"MATCH ATOM <(person:a AS p)>~[relation:ve AS c]~><[relation:target AS e]> IN GRAPH \
		 atomwalk RETURN p, c, e",
		"MATCH ATOM <[relation:target AS e]>~[relation:ev AS c]~><(person:b AS p)> IN GRAPH \
		 atomwalk RETURN e, c, p",
		"MATCH ATOM <[relation:target AS a]>~[relation:ee AS c]~><[relation:ve AS b]> IN GRAPH \
		 atomwalk RETURN a, c, b",
		"MATCH ATOM path=<(person:a)>~[relation*1..2 ALL SHORTEST]~><[relation:target]> IN GRAPH \
		 atomwalk RETURN path",
	] {
		let result = transaction.execute(query, &Object::new()).assert_ok();
		let rows = result[0].result.as_ref().assert_ok().value().as_array().assert_some();
		assert_eq!(rows.len(), 1, "{query}");
	}
}

/// Verifies nested match preserves scopes for fixed variable and shortest
/// paths.
#[test]
fn nested_match_preserves_scopes_for_fixed_variable_and_shortest_paths() {
	let store = Datastore::in_memory();
	let mut seed = store.begin();
	for (class, kind) in [
		("module", AtomKind::Node),
		("document", AtomKind::Node),
		("bridge", AtomKind::Edge),
		("meeting", AtomKind::Edge),
	] {
		seed.define_class(ClassSchema::new(class, kind, []).assert_ok()).assert_ok();
	}
	let child = seed.define_graph("nested_docs", GraphKind::Graph).assert_ok();
	let meta = seed.define_graph("nested_meta", GraphKind::Metagraph).assert_ok();
	let module = "module:root".parse::<RecordId>().assert_ok();
	let document = "document:spec".parse::<RecordId>().assert_ok();
	let appendix = "document:appendix".parse::<RecordId>().assert_ok();
	let bridge = "bridge:contains".parse::<RecordId>().assert_ok();
	let meeting = "meeting:review".parse::<RecordId>().assert_ok();
	for record in [&module, &document, &appendix, &bridge, &meeting] {
		seed.create_atom(record.clone(), crate::values::PropertyMap::default()).assert_ok();
	}
	let child_document = seed.attach_node(child.id(), &document, None, None).assert_ok();
	let child_appendix = seed.attach_node(child.id(), &appendix, None, None).assert_ok();
	let owner = seed.attach_node(meta.id(), &module, None, Some(child.id())).assert_ok();
	let nested = QualifiedAtomId::new([owner], child_document).assert_ok();
	seed.attach_edge(
		meta.id(),
		&bridge,
		None,
		EdgeTopology::bidirectional([EdgeEndpoint::Local(owner)], [EdgeEndpoint::Nested(nested)]),
		Some(Number::Int(3)),
		None,
	)
	.assert_ok();
	let nested_document = QualifiedAtomId::new([owner], child_document).assert_ok();
	let nested_appendix = QualifiedAtomId::new([owner], child_appendix).assert_ok();
	seed.attach_edge(
		meta.id(),
		&meeting,
		None,
		EdgeTopology::directed([EdgeEndpoint::Local(owner)], [
			EdgeEndpoint::Nested(nested_document),
			EdgeEndpoint::Nested(nested_appendix),
		]),
		None,
		None,
	)
	.assert_ok();
	seed.commit().assert_ok();

	let mut transaction = store.begin();
	let fixed = transaction
		.execute(
			"MATCH (module:root AS m)-[bridge:contains AS e]->(document:spec AS d) IN GRAPH \
			 nested_meta RETURN m, e, d, graph::inner(m) AS inner",
			&Object::new(),
		)
		.assert_ok();
	let fixed = fixed[0].result.as_ref().assert_ok().value().as_array().assert_some();
	assert_eq!(fixed.len(), 1);
	let fixed = fixed.get(0).assert_some().as_object().assert_some();
	assert_eq!(
		fixed.get("d").assert_some().as_atom_occurrence().assert_some().graph().id(),
		child.id()
	);
	assert_eq!(fixed.get("inner"), Some(&Value::GraphRef(Box::new(child.clone()))));

	let reverse = transaction
		.execute(
			"MATCH REPEATABLE ELEMENTS (module:root AS m)-[bridge:contains AS \
			 first]->(document:spec AS d)<-[bridge:contains AS second]-(module:root AS back) IN \
			 GRAPH nested_meta RETURN back",
			&Object::new(),
		)
		.assert_ok();
	assert_eq!(reverse[0].result.as_ref().assert_ok().value().as_array().assert_some().len(), 1);

	let variable = transaction
		.execute(
			"MATCH REPEATABLE ELEMENTS path = (module:root AS m)-[bridge*2 SIMPLE]->(module:root \
			 AS back) IN GRAPH nested_meta RETURN path, path::length(path) AS length, \
			 path::atoms(path) AS atoms",
			&Object::new(),
		)
		.assert_ok();
	let variable = variable[0].result.as_ref().assert_ok().value().as_array().assert_some();
	assert_eq!(variable.len(), 1);
	let variable = variable.get(0).assert_some().as_object().assert_some();
	assert_eq!(variable.get("length"), Some(&Value::from(2_i64)));
	let atoms = variable.get("atoms").and_then(Value::as_array).assert_some();
	assert_eq!(
		atoms.get(2).assert_some().as_atom_occurrence().assert_some().graph().id(),
		child.id()
	);
	let path = variable.get("path").and_then(Value::as_path).assert_some();
	assert_eq!(path.steps()[1].direction(), PathDirection::Forward);
	assert_eq!(path.steps()[1].role(), EndpointRole::Target);
	assert_eq!(path.steps()[2].direction(), PathDirection::Reverse);
	assert_eq!(path.steps()[2].role(), EndpointRole::Source);

	let shortest = transaction
		.execute(
			"MATCH path = (module:root AS m)-[bridge*1..2 WEIGHTED SHORTEST]->(document:spec AS \
			 d) IN GRAPH nested_meta RETURN path::cost(path) AS cost",
			&Object::new(),
		)
		.assert_ok();
	let shortest = shortest[0].result.as_ref().assert_ok().value().as_array().assert_some();
	assert_eq!(shortest.len(), 1);
	assert_eq!(
		shortest.get(0).assert_some().as_object().assert_some().get("cost"),
		Some(&Value::from(3_i64))
	);
	let all = transaction
		.execute(
			"MATCH path=(module:root)-[bridge*1..2 ALL WEIGHTED SHORTEST]->(document:spec) IN \
			 GRAPH nested_meta RETURN path::cost(path) AS cost",
			&Object::new(),
		)
		.assert_ok();
	assert_eq!(all[0].result.as_ref().assert_ok().value().as_array().assert_some(), shortest);

	let hyper = transaction
		.execute(
			"MATCH HYPER [meeting AS e] CONTAINS HEAD [(module AS m)], TAIL [(document AS d)] IN \
			 GRAPH nested_meta RETURN d",
			&Object::new(),
		)
		.assert_ok();
	let hyper = hyper[0].result.as_ref().assert_ok().value().as_array().assert_some();
	assert_eq!(hyper.len(), 2);
	assert!(hyper.iter().all(|row| {
		row.as_object()
			.and_then(|row| row.get("d"))
			.and_then(Value::as_atom_occurrence)
			.is_some_and(|occurrence| occurrence.graph().id() == child.id())
	}));
}

/// Verifies match optional distinct modes and expansion budget follow the
/// clause contract.
#[test]
fn match_optional_distinct_modes_and_expansion_budget_follow_the_clause_contract() {
	let store = Datastore::in_memory();
	let mut seed = store.begin();
	seed.define_class(ClassSchema::new("person", AtomKind::Node, []).assert_ok()).assert_ok();
	seed.define_class(ClassSchema::new("link", AtomKind::Edge, []).assert_ok()).assert_ok();
	let modes = seed.define_graph("modes", GraphKind::Graph).assert_ok();
	let parallel = seed.define_graph("parallel", GraphKind::Graph).assert_ok();
	let a = "person:a".parse::<RecordId>().assert_ok();
	let b = "person:b".parse::<RecordId>().assert_ok();
	let loop_edge = "link:loop".parse::<RecordId>().assert_ok();
	let first = "link:first".parse::<RecordId>().assert_ok();
	let second = "link:second".parse::<RecordId>().assert_ok();
	for record in [&a, &b, &loop_edge, &first, &second] {
		seed.create_atom(record.clone(), crate::values::PropertyMap::default()).assert_ok();
	}
	let modes_a = seed.attach_node(modes.id(), &a, None, None).assert_ok();
	let modes_b = seed.attach_node(modes.id(), &b, None, None).assert_ok();
	seed.attach_edge(
		modes.id(),
		&loop_edge,
		None,
		EdgeTopology::bidirectional([modes_a], [modes_b]),
		None,
		None,
	)
	.assert_ok();
	let parallel_a = seed.attach_node(parallel.id(), &a, None, None).assert_ok();
	let parallel_b = seed.attach_node(parallel.id(), &b, None, None).assert_ok();
	for edge in [&first, &second] {
		seed.attach_edge(
			parallel.id(),
			edge,
			None,
			EdgeTopology::directed([parallel_a], [parallel_b]),
			None,
			None,
		)
		.assert_ok();
	}
	seed.commit().assert_ok();

	let mut transaction = store.begin();
	let optional = transaction
		.execute("OPTIONAL MATCH (person:missing AS p) IN GRAPH modes RETURN p", &Object::new())
		.assert_ok();
	let optional = optional[0].result.as_ref().assert_ok().value().as_array().assert_some();
	assert_eq!(optional.len(), 1);
	assert_eq!(
		optional.get(0).assert_some().as_object().assert_some().get("p"),
		Some(&Value::None),
	);

	let count = |transaction: &mut Transaction, query: &str| {
		transaction.execute(query, &Object::new()).assert_ok()[0]
			.result
			.as_ref()
			.assert_ok()
			.value()
			.as_array()
			.assert_some()
			.len()
	};
	for (mode, expected) in [("WALK", 1), ("TRAIL", 0), ("ACYCLIC", 0), ("SIMPLE", 1)] {
		assert_eq!(
			count(
				&mut transaction,
				&format!(
					"MATCH REPEATABLE ELEMENTS path = (person:a AS a)<-[link:loop*2 \
					 {mode}]->(person:a AS b) IN GRAPH modes RETURN path"
				),
			),
			expected,
			"{mode}",
		);
	}

	let plain = count(
		&mut transaction,
		"MATCH (person:a AS a)-[link AS e]->(person:b AS b) IN GRAPH parallel RETURN b",
	);
	let binding_distinct = count(
		&mut transaction,
		"MATCH DISTINCT (person:a AS a)-[link AS e]->(person:b AS b) IN GRAPH parallel RETURN b",
	);
	let projection_distinct = count(
		&mut transaction,
		"MATCH (person:a AS a)-[link AS e]->(person:b AS b) IN GRAPH parallel RETURN DISTINCT b",
	);
	assert_eq!((plain, binding_distinct, projection_distinct), (2, 2, 1));
	drop(transaction);

	let options = QueryExecutionOptions {
		max_path_expansions: 1,
		..QueryExecutionOptions::default()
	};
	let mut limited = store.begin();
	let results = limited
		.execute_with_options(
			"MATCH REPEATABLE ELEMENTS path = (person:a AS a)<-[link:loop*2 WALK]->(person:a AS \
			 b) IN GRAPH modes RETURN path",
			&Object::new(),
			options,
		)
		.assert_ok();
	assert_eq!(results[0].result.as_ref().assert_err().status().code().to_string(), "FG-54F11",);
}
