//! Differential bounded path and immutable landmark binding evidence.

#[cfg(test)]
use crate::test_assertions::AssertOption as _;
#[cfg(test)]
use crate::test_assertions::AssertResult as _;
use crate::{
	datastore::{
		ClassSchema,
		Datastore,
		GraphIndex,
		GraphIndexCost,
		GraphIndexDefinition,
		PathIndexAlgorithm,
		Transaction,
	},
	graph::{
		AtomKind,
		EdgeEndpoint,
		EdgeTopology,
		GraphKind,
		QualifiedAtomId,
		TraversalDirection,
		TraversalSemantics,
	},
	values::{Number, Object, PropertyMap, RecordId, Value},
};

/// Independent path cost, connector choices and visited vertices.
type OraclePath = (Number, Vec<(usize, usize)>, Vec<usize>);

/// Builds equal canonical routes, a cycle and a longer detour to the target.
fn routes() -> Datastore {
	routes_kind(GraphKind::Graph)
}

/// Keeps the same relation for local and scoped execution evidence.
fn routes_kind(kind: GraphKind) -> Datastore {
	let store = Datastore::in_memory();
	let mut tx = store.begin();
	tx.define_class(ClassSchema::new("person", AtomKind::Node, []).assert_ok()).assert_ok();
	tx.define_class(ClassSchema::new("link", AtomKind::Edge, []).assert_ok()).assert_ok();
	let graph = tx.define_graph("routes", kind).assert_ok();
	let mut local = Vec::new();
	for ordinal in 0..18 {
		let record = format!("person:n{ordinal}").parse::<RecordId>().assert_ok();
		tx.create_atom(record.clone(), PropertyMap::new()).assert_ok();
		local.push(tx.attach_node(graph.id(), &record, None, None).assert_ok());
	}
	let mut edges = vec![(0, 1), (1, 3), (0, 2), (2, 3), (1, 0), (0, 0), (0, 4), (17, 3)];
	edges.extend((4..17).map(|from| (from, from + 1)));
	let mut connectors = Vec::new();
	for (ordinal, (from, to)) in edges.into_iter().enumerate() {
		let record = format!("link:e{ordinal}").parse::<RecordId>().assert_ok();
		tx.create_atom(record.clone(), PropertyMap::new()).assert_ok();
		connectors.push(
			tx.attach_edge(
				graph.id(),
				&record,
				None,
				EdgeTopology::directed([local[from]], [local[to]]),
				None,
				None,
			)
			.assert_ok(),
		);
	}
	for (name, from, to) in
		[("to_edge", local[0], connectors[0]), ("from_edge", connectors[0], local[3])]
	{
		let record = format!("link:{name}").parse::<RecordId>().assert_ok();
		tx.create_atom(record.clone(), PropertyMap::new()).assert_ok();
		tx.attach_edge(graph.id(), &record, None, EdgeTopology::directed([from], [to]), None, None)
			.assert_ok();
	}
	for (name, topology) in [
		("bidirectional", EdgeTopology::bidirectional([local[1]], [local[2]])),
		("undirected", EdgeTopology::undirected([local[2], local[3]])),
	] {
		let record = format!("link:{name}").parse::<RecordId>().assert_ok();
		tx.create_atom(record.clone(), PropertyMap::new()).assert_ok();
		tx.attach_edge(graph.id(), &record, None, topology, None, None).assert_ok();
	}
	for mode in [TraversalSemantics::Node, TraversalSemantics::Atom] {
		for direction in
			[TraversalDirection::Forward, TraversalDirection::Backward, TraversalDirection::Both]
		{
			for (prefix, cost) in
				[("path", GraphIndexCost::Hop), ("weighted", GraphIndexCost::Weighted)]
			{
				let index = GraphIndex::new(
					format!("{prefix}_{mode:?}_{direction:?}"),
					GraphIndexDefinition::Path {
						algorithm: PathIndexAlgorithm::LandmarkSssp(256),
						cost,
						patterns: Vec::new(),
					},
					Some(1 << 20),
				)
				.assert_ok()
				.with_mode(mode)
				.with_direction(direction);
				tx.define_graph_index("routes", index).assert_ok();
			}
		}
	}
	tx.commit().assert_ok();
	store
}

/// Returns owned public rows, preserving bag multiplicity and canonical order.
fn result(tx: &mut Transaction, query: &str) -> Value {
	let mut rows = tx.execute(query, &Object::new()).assert_ok();
	let outcome = rows.remove(0).result;
	assert!(outcome.is_ok(), "{query}: {outcome:?}");
	outcome.assert_ok().value().clone()
}

/// Maintenance uses the production Datastore entrypoint, outside a transaction.
fn rebuild_named(store: &Datastore, name: &str) {
	let rows = store
		.execute(&format!("REBUILD INDEX {name} ON GRAPH routes;"), &Object::new())
		.assert_ok();
	assert!(rows[0].result.is_ok(), "{:?}", rows[0].result);
}

/// Publishes every definition through the production maintenance pipeline.
fn rebuild(store: &Datastore) {
	for mode in [TraversalSemantics::Node, TraversalSemantics::Atom] {
		for direction in
			[TraversalDirection::Forward, TraversalDirection::Backward, TraversalDirection::Both]
		{
			for prefix in ["path", "weighted"] {
				let rows = store
					.execute(
						&format!("REBUILD INDEX {prefix}_{mode:?}_{direction:?} ON GRAPH routes;"),
						&Object::new(),
					)
					.assert_ok();
				assert!(rows[0].result.is_ok(), "{:?}", rows[0].result);
			}
		}
	}
}

/// Uses cycles, equal alternatives and an unreachable vertex for weighted
/// traversal, adding a true hyperedge for the Hypergraph family.
fn weighted_traversal_routes(kind: GraphKind, weights: [Number; 8]) -> Datastore {
	let store = Datastore::in_memory();
	let mut tx = store.begin();
	tx.define_class(ClassSchema::new("person", AtomKind::Node, []).assert_ok()).assert_ok();
	tx.define_class(ClassSchema::new("link", AtomKind::Edge, []).assert_ok()).assert_ok();
	let graph = tx.define_graph("routes", kind).assert_ok();
	let nodes = (0..5)
		.map(|ordinal| {
			let record = format!("person:n{ordinal}").parse::<RecordId>().assert_ok();
			tx.create_atom(record.clone(), PropertyMap::new()).assert_ok();
			tx.attach_node(graph.id(), &record, None, None).assert_ok()
		})
		.collect::<Vec<_>>();
	for (ordinal, ((from, to), weight)) in
		[(0, 1), (1, 3), (0, 2), (2, 3), (1, 2), (2, 1), (3, 3), (3, 0)]
			.into_iter()
			.zip(weights)
			.enumerate()
	{
		let record = format!("link:e{ordinal}").parse::<RecordId>().assert_ok();
		tx.create_atom(record.clone(), PropertyMap::new()).assert_ok();
		tx.attach_edge(
			graph.id(),
			&record,
			None,
			EdgeTopology::directed([nodes[from]], [nodes[to]]),
			Some(weight),
			None,
		)
		.assert_ok();
	}
	if kind == GraphKind::Hypergraph {
		let record = "link:hyper".parse::<RecordId>().assert_ok();
		tx.create_atom(record.clone(), PropertyMap::new()).assert_ok();
		tx.attach_edge(
			graph.id(),
			&record,
			None,
			EdgeTopology::directed([nodes[1], nodes[2]], [nodes[3]]),
			Some(Number::Int(0)),
			None,
		)
		.assert_ok();
	}
	for (name, mode) in
		[("weighted", TraversalSemantics::Node), ("atom_weighted", TraversalSemantics::Atom)]
	{
		tx.define_graph_index(
			"routes",
			GraphIndex::new(
				name,
				GraphIndexDefinition::Path {
					algorithm: PathIndexAlgorithm::LandmarkSssp(256),
					cost: GraphIndexCost::Weighted,
					patterns: Vec::new(),
				},
				Some(1 << 20),
			)
			.assert_ok()
			.with_mode(mode),
		)
		.assert_ok();
	}
	tx.commit().assert_ok();
	store
}

/// Snapshot ids differ after publication; the public path and exact numeric
/// result must retain the same steps, predecessor order, type and value.
fn assert_traversal_results(actual: &Value, expected: &Value) {
	let actual = actual.as_array().assert_some();
	let expected = expected.as_array().assert_some();
	assert_eq!(actual.len(), expected.len());
	for (actual, expected) in actual.iter().zip(expected) {
		if let (Some(actual), Some(expected)) = (actual.as_path(), expected.as_path()) {
			assert_eq!(actual.steps(), expected.steps());
			assert_eq!(actual.cost(), expected.cost());
			assert_eq!(
				actual.cost().map(|value| std::mem::discriminant(&value)),
				expected.cost().map(|value| std::mem::discriminant(&value))
			);
		} else {
			assert_eq!(actual, expected);
			assert_eq!(actual.kind(), expected.kind());
			assert_eq!(
				actual.as_number().map(|value| std::mem::discriminant(&value)),
				expected.as_number().map(|value| std::mem::discriminant(&value))
			);
		}
	}
}

/// Verifies landmark traversal weighted all pairs preserves dijkstra paths and
/// numeric types.
#[test]
fn landmark_traversal_weighted_all_pairs_preserves_dijkstra_paths_and_numeric_types() {
	let decimal = |value: &str| Number::Decimal(value.parse().assert_ok());
	for (weights, pruning_safe) in [
		(
			[
				Number::Int(0),
				Number::Int(1),
				Number::Int(0),
				Number::Int(1),
				Number::Int(0),
				Number::Int(0),
				Number::Int(0),
				Number::Int(2),
			],
			true,
		),
		(
			[
				Number::Float(0.1),
				Number::Float(0.2),
				Number::Float(0.3),
				Number::Float(0.0),
				Number::Float(0.0),
				Number::Float(f64::from_bits(1)),
				Number::Float(0.0),
				Number::Float(0.5),
			],
			true,
		),
		(
			[
				decimal("0.0000000000000000000000000001"),
				decimal("9007199254740993.123456789"),
				Number::Int(9_007_199_254_740_993),
				Number::Int(0),
				Number::Int(0),
				Number::Int(0),
				Number::Int(0),
				Number::Int(0),
			],
			true,
		),
		(
			[
				Number::Int(9_007_199_254_740_993),
				Number::Float(0.0),
				Number::Int(9_007_199_254_740_993),
				Number::Int(0),
				Number::Int(0),
				Number::Int(0),
				Number::Float(f64::from_bits(1)),
				Number::Int(0),
			],
			false,
		),
		(
			[
				Number::Int(9_007_199_254_740_993),
				Number::Int(0),
				Number::Int(9_007_199_254_740_993),
				Number::Int(0),
				Number::Int(0),
				Number::Int(0),
				Number::Int(0),
				Number::Int(0),
			],
			true,
		),
	] {
		for kind in [GraphKind::Graph, GraphKind::Metagraph, GraphKind::Hypergraph] {
			let store = weighted_traversal_routes(kind, weights);
			let queries = ["weightedShortestPath", "distanceTo"]
				.into_iter()
				.flat_map(|method| {
					[false, true].into_iter().flat_map(move |atom| {
						(0..5).flat_map(move |source| {
							(0..5).map(move |target| {
								format!(
									"TRAVERSE {} FROM person:n{source} IN GRAPH @routes |> \
									 tv::{method}(person:n{target});",
									if atom {
										"ATOM"
									} else {
										""
									}
								)
							})
						})
					})
				})
				.collect::<Vec<_>>();
			let mut old = store.begin();
			let baseline = queries.iter().map(|query| result(&mut old, query)).collect::<Vec<_>>();
			let explain = "EXPLAIN ANALYZE FORMAT JSON TRAVERSE FROM person:n0 IN GRAPH @routes \
			               |> tv::weightedShortestPath(person:n3);";
			rebuild_named(&store, "atom_weighted");
			let incompatible = result(&mut store.begin(), explain);
			assert_eq!(
				incompatible
					.as_object()
					.assert_some()
					.get("analyze")
					.assert_some()
					.as_object()
					.assert_some()
					.get("landmark_lookups"),
				Some(&Value::from(0_u64))
			);
			rebuild_named(&store, "weighted");
			let mut ready = store.begin();
			for (query, expected) in queries.iter().zip(&baseline) {
				assert_traversal_results(&result(&mut ready, query), expected);
			}
			let indexed = result(&mut ready, explain);
			let indexed = indexed.as_object().assert_some();
			let node = indexed
				.get("nodes")
				.assert_some()
				.as_array()
				.assert_some()
				.iter()
				.filter_map(Value::as_object)
				.find(|node| node.get("operator") == Some(&Value::from("LandmarkWeighted")))
				.assert_some();
			assert_eq!(node.get("cost"), Some(&Value::from("WEIGHT")));
			assert_eq!(node.get("mode"), Some(&Value::from("NODE")));
			let metrics = indexed.get("analyze").assert_some().as_object().assert_some();
			assert_eq!(
				metrics.get("landmark_lookups").assert_some() > &Value::from(0_u64),
				pruning_safe
			);
			assert_eq!(
				metrics.get("landmark_fallbacks"),
				Some(&Value::from(u64::from(!pruning_safe)))
			);
			let old_plan = result(&mut old, explain);
			assert_ne!(
				indexed.get("plan_hash"),
				old_plan.as_object().assert_some().get("plan_hash")
			);
			let graph = ready
				.graphs()
				.assert_ok()
				.iter()
				.find(|graph| graph.name() == "routes")
				.assert_some()
				.id();
			ready.detach_occurrence(graph, &"link:e0".parse().assert_ok(), true).assert_ok();
			let private = result(&mut ready, explain);
			assert_eq!(
				private
					.as_object()
					.assert_some()
					.get("analyze")
					.assert_some()
					.as_object()
					.assert_some()
					.get("landmark_lookups"),
				Some(&Value::from(0_u64))
			);
			drop(ready);
			assert_traversal_results(&result(&mut store.begin(), &queries[3]), &baseline[3]);
		}
	}
}

/// Verifies landmark traversal weighted preserves errors and enforces query
/// limits.
#[test]
fn landmark_traversal_weighted_preserves_errors_and_enforces_query_limits() {
	for weights in [
		[
			Number::Int(1),
			Number::Int(1),
			Number::Int(1),
			Number::Int(1),
			Number::Int(0),
			Number::Int(0),
			Number::Int(0),
			Number::Int(i64::MAX),
		],
		[
			Number::Float(f64::MAX),
			Number::Float(f64::MAX),
			Number::Float(f64::MAX),
			Number::Float(f64::MAX),
			Number::Float(0.0),
			Number::Float(0.0),
			Number::Float(0.0),
			Number::Float(0.0),
		],
		[
			Number::Float(1.0),
			Number::Decimal("1".parse().assert_ok()),
			Number::Float(1.0),
			Number::Decimal("1".parse().assert_ok()),
			Number::Int(0),
			Number::Int(0),
			Number::Int(0),
			Number::Int(0),
		],
	] {
		let store = weighted_traversal_routes(GraphKind::Graph, weights);
		for method in ["weightedShortestPath", "distanceTo"] {
			// Even a zero-length target must preserve errors in other reachable
			// arithmetic, as the pre-index all-target API does.
			let query =
				format!("TRAVERSE FROM person:n0 IN GRAPH @routes |> tv::{method}(person:n0);");
			let before =
				store.execute(&query, &Object::new()).assert_ok().remove(0).result.assert_err();
			rebuild_named(&store, "weighted");
			let after =
				store.execute(&query, &Object::new()).assert_ok().remove(0).result.assert_err();
			assert_eq!(after.status().code(), before.status().code());
			assert_eq!(after.to_string(), before.to_string());
		}
	}
	let store = weighted_traversal_routes(GraphKind::Graph, [Number::Int(1); 8]);
	for ready in [false, true] {
		if ready {
			rebuild_named(&store, "weighted");
		}
		for method in ["weightedShortestPath", "distanceTo"] {
			let query =
				format!("TRAVERSE FROM person:n0 IN GRAPH @routes |> tv::{method}(person:n3);");
			for options in [
				crate::gql::QueryExecutionOptions {
					max_path_expansions: 1,
					..Default::default()
				},
				crate::gql::QueryExecutionOptions {
					barrier_memory_bytes: std::num::NonZeroUsize::new(2048).assert_some(),
					..Default::default()
				},
			] {
				let rows = store.execute_with_options(&query, &Object::new(), options).assert_ok();
				assert!(rows[0].result.is_err(), "{query} ready={ready}");
			}
		}
	}
}

/// Verifies landmark traversal weighted bound reduces expansions without
/// changing ties.
#[test]
fn landmark_traversal_weighted_bound_reduces_expansions_without_changing_ties() {
	let store = routes();
	let mut old = store.begin();
	let query = "TRAVERSE FROM person:n0 IN GRAPH @routes |> tv::weightedShortestPath(person:n3);";
	let expected = result(&mut old, query);
	let explain = format!("EXPLAIN ANALYZE FORMAT JSON {query}");
	let before = result(&mut old, &explain);
	rebuild_named(&store, "weighted_Node_Forward");
	let mut ready = store.begin();
	assert_traversal_results(&result(&mut ready, query), &expected);
	let after = result(&mut ready, &explain);
	let expansions = |value: &Value| {
		value
			.as_object()
			.assert_some()
			.get("analyze")
			.assert_some()
			.as_object()
			.assert_some()
			.get("path_expansions")
			.assert_some()
			.clone()
	};
	assert!(expansions(&after) < expansions(&before));
	let distance = result(&mut ready, &query.replace("weightedShortestPath", "distanceTo"));
	assert_eq!(distance.as_array().assert_some().get(0), Some(&Value::from(2_i64)));
}

/// Verifies landmark traversal hop all pairs preserves fifo paths and node
/// domain.
#[test]
fn landmark_traversal_hop_all_pairs_preserves_fifo_paths_and_node_domain() {
	// Builds publish a new SnapshotId; public steps and exact cost must remain
	// identical while each Path retains its own captured snapshot.
	let comparable = |value: Value| {
		value
			.as_array()
			.assert_some()
			.iter()
			.map(|value| {
				if matches!(value, Value::None) {
					None
				} else {
					let path = value.as_path().assert_some();
					Some((path.steps().to_vec(), path.cost()))
				}
			})
			.collect::<Vec<_>>()
	};
	for kind in [GraphKind::Graph, GraphKind::Metagraph] {
		let store = routes_kind(kind);
		let queries = [false, true]
			.into_iter()
			.flat_map(|atom| {
				(0..18).flat_map(move |source| {
					(0..18).map(move |target| {
						format!(
							"TRAVERSE {} FROM person:n{source} IN GRAPH @routes |> \
							 tv::shortestPath(person:n{target});",
							if atom {
								"ATOM"
							} else {
								""
							}
						)
					})
				})
			})
			.collect::<Vec<_>>();
		let mut before = store.begin();
		let baseline =
			queries.iter().map(|query| comparable(result(&mut before, query))).collect::<Vec<_>>();
		rebuild(&store);
		let mut ready = store.begin();
		for (query, expected) in queries.iter().zip(&baseline) {
			assert_eq!(comparable(result(&mut ready, query)), *expected, "{kind:?}: {query}");
		}
		let query = "TRAVERSE FROM person:n0 IN GRAPH @routes |> tv::shortestPath(person:n3);";
		let explain = format!("EXPLAIN ANALYZE FORMAT JSON {query}");
		let indexed = result(&mut ready, &explain);
		let indexed = indexed.as_object().assert_some();
		let node = indexed
			.get("nodes")
			.assert_some()
			.as_array()
			.assert_some()
			.iter()
			.filter_map(Value::as_object)
			.find(|node| node.get("operator") == Some(&Value::from("LandmarkShortest")))
			.assert_some();
		assert_eq!(node.get("mode"), Some(&Value::from("NODE")));
		assert_eq!(node.get("cost"), Some(&Value::from("HOP")));
		let metrics = indexed.get("analyze").assert_some().as_object().assert_some();
		assert!(metrics.get("landmark_lookups").assert_some() > &Value::from(0_u64));
		assert_eq!(metrics.get("landmark_fallbacks"), Some(&Value::from(0_u64)));
		let old = result(&mut before, &explain);
		assert_ne!(indexed.get("plan_hash"), old.as_object().assert_some().get("plan_hash"));
		assert_eq!(
			old.as_object()
				.assert_some()
				.get("analyze")
				.assert_some()
				.as_object()
				.assert_some()
				.get("landmark_lookups"),
			Some(&Value::from(0_u64))
		);
		let graph = ready
			.graphs()
			.assert_ok()
			.iter()
			.find(|graph| graph.name() == "routes")
			.assert_some()
			.id();
		let edge = "link:e0".parse::<RecordId>().assert_ok();
		ready.detach_occurrence(graph, &edge, true).assert_ok();
		let private = result(&mut ready, &explain);
		assert_eq!(
			private
				.as_object()
				.assert_some()
				.get("analyze")
				.assert_some()
				.as_object()
				.assert_some()
				.get("landmark_lookups"),
			Some(&Value::from(0_u64))
		);
		drop(ready);
		let mut restored = store.begin();
		assert_eq!(comparable(result(&mut restored, query)), baseline[3]);
		for options in [
			crate::gql::QueryExecutionOptions {
				max_path_expansions: 1,
				..Default::default()
			},
			crate::gql::QueryExecutionOptions {
				barrier_memory_bytes: std::num::NonZeroUsize::new(2048).assert_some(),
				..Default::default()
			},
		] {
			let rows = store.execute_with_options(query, &Object::new(), options).assert_ok();
			assert!(rows[0].result.is_err());
		}
	}
}

/// Verifies landmark traversal keeps seed multiplicity and bounds retained path
/// results.
#[test]
fn landmark_traversal_keeps_seed_multiplicity_and_bounds_retained_path_results() {
	let store = routes();
	let duplicates =
		"TRAVERSE FROM [person:n0, person:n0] IN GRAPH @routes |> tv::shortestPath(person:n3);";
	let seeds =
		(0..64).map(|index| format!("person:n{}", 4 + index % 4)).collect::<Vec<_>>().join(",");
	let large = format!("TRAVERSE FROM [{seeds}] IN GRAPH @routes |> tv::shortestPath(person:n3);");
	for ready in [false, true] {
		if ready {
			rebuild(&store);
		}
		let mut read = store.begin();
		assert_eq!(result(&mut read, duplicates).as_array().assert_some().len(), 2);
		let rows = result(&mut read, &large);
		assert_eq!(rows.as_array().assert_some().len(), 64);
		assert!(rows.estimated_heap_bytes() > 96 * 1024);
		let limited = crate::gql::QueryExecutionOptions {
			barrier_memory_bytes: std::num::NonZeroUsize::new(96 * 1024).assert_some(),
			..Default::default()
		};
		let rows = store.execute_with_options(&large, &Object::new(), limited).assert_ok();
		assert!(
			rows[0].result.is_err(),
			"partial traversal results must not escape resource exhaustion"
		);
		for step in ["reachable", "weightedShortestPath", "distanceTo"] {
			let query = duplicates.replace("shortestPath", step);
			assert_eq!(result(&mut read, &query).as_array().assert_some().len(), 2, "{step}");
		}
	}
}

/// Verifies landmark match hop all pairs modes bounds directions and canonical
/// ties.
#[test]
fn landmark_match_hop_all_pairs_modes_bounds_directions_and_canonical_ties() {
	let store = routes();
	let mut queries = Vec::new();
	for atom in [false, true] {
		for (left, right) in [("-", "->"), ("<-", "-"), ("-", "-"), ("<-", "->")] {
			for from in 0..4 {
				for to in 0..4 {
					for (min, max) in [(0, 3), (2, 2), (3, 4)] {
						for mode in ["WALK", "TRAIL", "ACYCLIC", "SIMPLE"] {
							let query = if atom {
								let (left, right) = match (left, right) {
									("-", "->") => ("~", "~>"),
									("<-", "-") => ("<~", "~"),
									("-", "-") => ("~", "~"),
									_ => ("<~", "~>"),
								};
								format!(
									"MATCH ATOM REPEATABLE ELEMENTS path = <(person:n{from} AS \
									 a)>{left}[link*{min}..{max} {mode} \
									 SHORTEST]{right}<(person:n{to} AS b)> IN GRAPH routes RETURN \
									 path::atoms(path) AS atoms, path::cost(path) AS cost, \
									 path::length(path) AS length;"
								)
							} else {
								format!(
									"MATCH REPEATABLE ELEMENTS path = (person:n{from} AS \
									 a){left}[link*{min}..{max} {mode} \
									 SHORTEST]{right}(person:n{to} AS b) IN GRAPH routes RETURN \
									 path::atoms(path) AS atoms, path::cost(path) AS cost, \
									 path::length(path) AS length;"
								)
							};
							queries.push(query);
						}
					}
				}
			}
		}
	}
	queries.push(
		"MATCH ATOM path = <(person:n0 AS a)>~[link*1 SHORTEST]~><[link:e0 AS b]> IN GRAPH routes \
		 RETURN path::atoms(path) AS atoms, path::cost(path) AS cost, path::length(path) AS \
		 length;"
			.to_owned(),
	);
	queries.push(
		"MATCH ATOM path = <[link:e0 AS a]>~[link*1 SHORTEST]~><(person:n3 AS b)> IN GRAPH routes \
		 RETURN path::atoms(path) AS atoms, path::cost(path) AS cost, path::length(path) AS \
		 length;"
			.to_owned(),
	);
	queries.extend(
		queries.clone().into_iter().map(|query| query.replace("SHORTEST", "WEIGHTED SHORTEST")),
	);
	let mut before = store.begin();
	let expected = queries.iter().map(|query| result(&mut before, query)).collect::<Vec<_>>();
	rebuild(&store);
	let mut after = store.begin();
	for (query, expected) in queries.iter().zip(&expected) {
		assert_eq!(&result(&mut after, query), expected, "{query}");
	}
	let tie = "MATCH path = (person:n0 AS a)-[link*2 SHORTEST]->(person:n3 AS b) IN GRAPH routes \
	           RETURN path::atoms(path) AS atoms;";
	let rows = result(&mut after, tie);
	let atoms = rows
		.as_array()
		.assert_some()
		.get(0)
		.assert_some()
		.as_object()
		.assert_some()
		.get("atoms")
		.assert_some()
		.as_array()
		.assert_some();
	assert_eq!(
		atoms.get(2).assert_some().as_atom_occurrence().assert_some().atom().id().to_string(),
		"person:n1"
	);
}

/// Verifies landmark match binding explain expansions fallback and private
/// snapshot.
#[test]
fn landmark_match_binding_explain_expansions_fallback_and_private_snapshot() {
	let store = routes();
	let query = "MATCH path = (person:n0 AS a)-[link*1..16 SHORTEST]->(person:n3 AS b) IN GRAPH \
	             routes RETURN path::atoms(path) AS atoms;";
	let explain = format!("EXPLAIN ANALYZE FORMAT JSON {query}");
	let mut old = store.begin();
	let exact = result(&mut old, &explain);
	rebuild(&store);
	let mut tx = store.begin();
	let ready = result(&mut tx, &explain);
	let ready = ready.as_object().assert_some();
	assert_ne!(ready.get("plan_hash"), exact.as_object().assert_some().get("plan_hash"));
	let metrics = ready.get("analyze").assert_some().as_object().assert_some();
	assert!(metrics.get("landmark_lookups").assert_some() > &Value::from(0_i64));
	assert!(metrics.get("landmark_artifact_bytes").assert_some() > &Value::from(0_i64));
	let baseline =
		exact.as_object().assert_some().get("analyze").assert_some().as_object().assert_some();
	assert!(
		metrics.get("path_expansions").assert_some()
			< baseline.get("path_expansions").assert_some()
	);
	let text = result(&mut tx, &format!("EXPLAIN ANALYZE FORMAT TEXT {query}"));
	assert!(text.as_text().assert_some().as_str().contains("landmark_lookups="));
	let all_targets = result(
		&mut tx,
		"EXPLAIN ANALYZE FORMAT JSON MATCH path = (person:n0 AS a)-[link*1..3 SHORTEST]->(person \
		 AS b) IN GRAPH routes RETURN b;",
	);
	let metrics = all_targets
		.as_object()
		.assert_some()
		.get("analyze")
		.assert_some()
		.as_object()
		.assert_some();
	assert_eq!(metrics.get("landmark_lookups"), Some(&Value::from(0_i64)));
	assert_eq!(metrics.get("landmark_fallbacks"), Some(&Value::from(1_i64)));
	let snapshot = result(&mut tx, query);
	let graph = tx.graphs().assert_ok()[0].reference().id();
	tx.set_occurrence_label(graph, &"link:e0".parse().assert_ok(), Some("changed".into()))
		.assert_ok();
	let private = result(&mut tx, &explain);
	assert_eq!(
		private.as_object().assert_some().get("plan_hash"),
		exact.as_object().assert_some().get("plan_hash")
	);
	drop(tx);
	assert_eq!(result(&mut store.begin(), query), snapshot);
	assert_eq!(
		result(&mut old, &explain).as_object().assert_some().get("plan_hash"),
		exact.as_object().assert_some().get("plan_hash")
	);
}

/// Exhaustive bounded NODE oracle; it does not use a priority queue or
/// landmarks.
fn weighted_oracle(
	edges: &[(usize, usize, Number)],
	source: usize,
	target: usize,
	backward: bool,
	min: usize,
	max: usize,
	mode: &str,
) -> Option<(Number, Vec<usize>)> {
	let mut pending = vec![(source, Number::Int(0), Vec::new(), vec![source])];
	let mut best: Option<OraclePath> = None;
	while let Some((vertex, cost, used, vertices)) = pending.pop() {
		let canonical =
			used.iter().copied().zip(vertices.iter().copied().skip(1)).collect::<Vec<_>>();
		if vertex == target
			&& used.len() >= min
			&& best.as_ref().is_none_or(|best| (cost, &canonical) < (best.0, &best.1))
		{
			best = Some((cost, canonical, used.clone()));
		}
		if used.len() == max || mode == "SIMPLE" && !used.is_empty() && vertex == source {
			continue;
		}
		for (ordinal, &(from, to, weight)) in edges.iter().enumerate() {
			let (from, to) = if backward {
				(to, from)
			} else {
				(from, to)
			};
			if vertex != from
				|| mode == "TRAIL" && used.contains(&ordinal)
				|| mode == "ACYCLIC" && vertices.contains(&to)
				|| mode == "SIMPLE" && to != source && vertices.contains(&to)
			{
				continue;
			}
			let cost = crate::gql::execution::cursor::add_path_cost(cost, weight).assert_ok();
			let mut next = used.clone();
			next.push(ordinal);
			let mut next_vertices = vertices.clone();
			next_vertices.push(to);
			pending.push((to, cost, next, next_vertices));
		}
	}
	best.map(|(cost, _, path)| (cost, path))
}

/// Verifies landmark weighted matches exhaustive reduction and zero cost ties.
#[test]
fn landmark_weighted_matches_exhaustive_reduction_and_zero_cost_ties() {
	weighted_matches_exhaustive(GraphKind::Graph, false);
}

/// Verifies scoped weighted matches exhaustive reduction and zero cost ties.
#[test]
fn scoped_weighted_matches_exhaustive_reduction_and_zero_cost_ties() {
	weighted_matches_exhaustive(GraphKind::Metagraph, false);
}

/// Verifies scoped weighted keeps distinct qualified targets and exact cross
/// level fallback.
#[test]
fn scoped_weighted_keeps_distinct_qualified_targets_and_exact_cross_level_fallback() {
	let store = Datastore::in_memory();
	let mut tx = store.begin();
	tx.define_class(ClassSchema::new("person", AtomKind::Node, []).assert_ok()).assert_ok();
	tx.define_class(ClassSchema::new("link", AtomKind::Edge, []).assert_ok()).assert_ok();
	let root = tx.define_graph("routes", GraphKind::Metagraph).assert_ok();
	let children = [
		tx.define_graph("left", GraphKind::Graph).assert_ok(),
		tx.define_graph("right", GraphKind::Graph).assert_ok(),
	];
	let records = ["person:n0", "person:n1", "person:n2", "link:e0", "link:e1"]
		.map(|record| record.parse::<RecordId>().assert_ok());
	for record in &records {
		tx.create_atom(record.clone(), PropertyMap::new()).assert_ok();
	}
	let owners = [
		tx.attach_node(root.id(), &records[0], None, Some(children[0].id())).assert_ok(),
		tx.attach_node(root.id(), &records[2], None, Some(children[1].id())).assert_ok(),
	];
	for index in 0..2 {
		let target = tx.attach_node(children[index].id(), &records[1], None, None).assert_ok();
		tx.attach_edge(
			root.id(),
			&records[3 + index],
			None,
			EdgeTopology::directed([EdgeEndpoint::Local(owners[0])], [EdgeEndpoint::Nested(
				QualifiedAtomId::new([owners[index]], target).assert_ok(),
			)]),
			Some(Number::Int(2 + index as i64)),
			None,
		)
		.assert_ok();
	}
	tx.define_graph_index(
		"routes",
		GraphIndex::new(
			"cross",
			GraphIndexDefinition::Path {
				algorithm: PathIndexAlgorithm::LandmarkSssp(2),
				cost: GraphIndexCost::Weighted,
				patterns: Vec::new(),
			},
			Some(1 << 20),
		)
		.assert_ok(),
	)
	.assert_ok();
	tx.commit().assert_ok();
	let query = "MATCH path = (person:n0 AS a)-[link*1..2 WEIGHTED SHORTEST]->(person:n1 AS b) IN \
	             GRAPH routes RETURN b, path::cost(path) AS cost;";
	let exact = result(&mut store.begin(), query);
	assert_eq!(exact.as_array().assert_some().len(), 2);
	let scopes = exact
		.as_array()
		.assert_some()
		.iter()
		.map(|row| {
			let row = row.as_object().assert_some();
			(
				row.get("b").assert_some().as_atom_occurrence().assert_some().graph().id(),
				row.get("cost").assert_some().clone(),
			)
		})
		.collect::<Vec<_>>();
	assert!(scopes.contains(&(children[0].id(), Value::from(2_i64))));
	assert!(scopes.contains(&(children[1].id(), Value::from(3_i64))));
	let rebuilt = store.execute("REBUILD INDEX cross ON GRAPH routes;", &Object::new()).assert_ok();
	assert!(rebuilt[0].result.is_ok(), "{rebuilt:?}");
	assert_eq!(result(&mut store.begin(), query), exact);
	let explain = result(&mut store.begin(), &format!("EXPLAIN ANALYZE FORMAT JSON {query}"));
	assert_eq!(
		explain
			.as_object()
			.assert_some()
			.get("analyze")
			.assert_some()
			.as_object()
			.assert_some()
			.get("landmark_lookups"),
		Some(&Value::from(0_i64))
	);
	assert_eq!(
		explain
			.as_object()
			.assert_some()
			.get("analyze")
			.assert_some()
			.as_object()
			.assert_some()
			.get("landmark_fallbacks"),
		Some(&Value::from(1_i64))
	);
}

/// Verifies stateful match cost uses exact prefixes and never additive
/// artifacts.
#[test]
fn stateful_match_cost_uses_exact_prefixes_and_never_additive_artifacts() {
	for kind in [GraphKind::Graph, GraphKind::Metagraph] {
		let store = routes_kind(kind);
		rebuild(&store);
		for outcome in store
			.execute(
				"DEFINE PURE COST FUNCTION fn::state(edge: any, prefix: number) -> number { \
				 RETURN $prefix + 1; }; DEFINE INDEX stateful ON GRAPH routes PATH ALGORITHM \
				 ON_DEMAND COST fn::state;",
				&Object::new(),
			)
			.assert_ok()
		{
			assert!(outcome.result.is_ok(), "{:?}", outcome.result);
		}
		assert!(
			store
				.execute(
					"DEFINE INDEX bad_state ON GRAPH routes PATH ALGORITHM LANDMARK_SSSP(2) COST \
					 fn::state WITH (MAX_BYTES = 1048576);",
					&Object::new()
				)
				.assert_ok()[0]
				.result
				.is_err()
		);
		for objective in ["", "WEIGHTED SHORTEST"] {
			let query = format!(
				"MATCH path = (person:n0 AS a)-[link*2 WALK {objective} COST \
				 fn::state]->(person:n3 AS b) IN GRAPH routes RETURN path::cost(path) AS cost;"
			);
			let rows = result(&mut store.begin(), &query);
			assert!(!rows.as_array().assert_some().is_empty());
			for row in rows.as_array().assert_some().iter() {
				assert_eq!(row.as_object().assert_some().get("cost"), Some(&Value::from(3_i64)));
			}
			let explain =
				result(&mut store.begin(), &format!("EXPLAIN ANALYZE FORMAT JSON {query}"));
			let explain = explain.as_object().assert_some();
			assert!(
				explain.get("nodes").assert_some().as_array().assert_some().iter().any(|node| node
					.as_object()
					.assert_some()
					.get("cost_stateful")
					== Some(&Value::Bool(true)))
			);
			assert_eq!(
				explain
					.get("analyze")
					.assert_some()
					.as_object()
					.assert_some()
					.get("landmark_lookups"),
				Some(&Value::from(0_i64))
			);
		}
		for outcome in store
			.execute(
				"DEFINE PURE COST FUNCTION fn::negative(edge: any) -> int { RETURN -1; }; DEFINE \
				 PURE FUNCTION fn::ordinary(edge: any) -> int { RETURN 1; };",
				&Object::new(),
			)
			.assert_ok()
		{
			assert!(outcome.result.is_ok(), "{:?}", outcome.result);
		}
		for objective in [
			"WEIGHTED SHORTEST COST fn::negative",
			"WEIGHTED SHORTEST COST fn::ordinary",
			"SHORTEST COST fn::state",
		] {
			let query = format!(
				"MATCH path = (person:n0 AS a)-[link*1..2 {objective}]->(person:n3 AS b) IN GRAPH \
				 routes RETURN path;"
			);
			assert!(
				store.execute(&query, &Object::new()).assert_ok()[0].result.is_err(),
				"{query}"
			);
		}
	}
}

/// Verifies global match cost normalizes local defaults overrides and stateful
/// fallback.
#[test]
fn global_match_cost_normalizes_local_defaults_overrides_and_stateful_fallback() {
	for kind in [GraphKind::Graph, GraphKind::Metagraph] {
		let store = routes_kind(kind);
		for outcome in store
			.execute(
				"UPDATE link SET fee = 2; DEFINE PURE COST FUNCTION fn::cost(edge: any) -> number \
				 { RETURN <number>$edge.fee; }; DEFINE PURE COST FUNCTION fn::override(edge: any) \
				 -> number { RETURN 1; }; DEFINE PURE COST FUNCTION fn::state(edge: any, prefix: \
				 number) -> number { RETURN $prefix + 1; }; DEFINE INDEX custom ON GRAPH routes \
				 PATH ALGORITHM LANDMARK_SSSP(256) COST fn::cost WITH (MAX_BYTES = 1048576);",
				&Object::new(),
			)
			.assert_ok()
		{
			assert!(outcome.result.is_ok(), "{:?}", outcome.result);
		}
		for ready in [false, true] {
			if ready {
				rebuild_named(&store, "custom");
			}
			let mut tx = store.begin();
			for (objective, function) in
				[("WEIGHTED SHORTEST", "cost"), ("", "cost"), ("WEIGHTED SHORTEST", "state")]
			{
				let local = format!(
					"MATCH REPEATABLE ELEMENTS path = (person:n0 AS a)-[link*1..3 WALK \
					 {objective} COST fn::{function}]->(person:n3 AS b) IN GRAPH routes WHERE \
					 path::length(path) <= 3 RETURN path::edges(path) AS edges, path::cost(path) \
					 AS cost, path::cost(path, fn::{function}) AS recomputed;"
				);
				let global = local.replace(&format!(" COST fn::{function}"), "").replace(
					" IN GRAPH routes WHERE",
					&format!(" IN GRAPH routes WITH COST fn::{function} WHERE"),
				);
				let local_rows = result(&mut tx, &local);
				assert!(!local_rows.as_array().assert_some().is_empty());
				for row in local_rows.as_array().assert_some() {
					let row = row.as_object().assert_some();
					assert_eq!(row.get("cost"), row.get("recomputed"));
				}
				assert_eq!(result(&mut tx, &global), local_rows);
				let local_explain =
					result(&mut tx, &format!("EXPLAIN ANALYZE FORMAT JSON {local}"));
				let global_explain =
					result(&mut tx, &format!("EXPLAIN ANALYZE FORMAT JSON {global}"));
				assert_eq!(
					local_explain.as_object().assert_some().get("plan_hash"),
					global_explain.as_object().assert_some().get("plan_hash")
				);
				let metrics = global_explain
					.as_object()
					.assert_some()
					.get("analyze")
					.assert_some()
					.as_object()
					.assert_some();
				assert_eq!(
					metrics.get("landmark_lookups").assert_some() > &Value::from(0_u64),
					ready
						&& kind == GraphKind::Graph
						&& objective == "WEIGHTED SHORTEST"
						&& function == "cost"
				);
			}
			let fixed = "MATCH path = (person:n0 AS a)-[link:e0 AS edge]->(person:n1 AS b) IN \
			             GRAPH routes WITH COST fn::cost RETURN edge, path::cost(path) AS cost;";
			let rows = result(&mut tx, fixed);
			let local_fixed = fixed
				.replace(" WITH COST fn::cost", "")
				.replace(" AS edge]", " AS edge COST fn::cost]");
			assert_eq!(result(&mut tx, &local_fixed), rows);
			let row = rows.as_array().assert_some().get(0).assert_some().as_object().assert_some();
			assert_eq!(row.get("cost"), Some(&Value::from(2_i64)));
			assert_eq!(
				row.get("edge")
					.assert_some()
					.as_atom_occurrence()
					.assert_some()
					.atom()
					.id()
					.to_string(),
				"link:e0"
			);
			for (query, expected) in [
				(
					"MATCH path = (person:n0)-[link:e0]->(person:n1)-[link:e1]->(person:n3) IN \
					 GRAPH routes WITH COST fn::state RETURN path::cost(path) AS cost;",
					2,
				),
				(
					"MATCH path = (person:n0)-[link:e0 COST \
					 fn::override]->(person:n1)-[link*1]->(person:n3) IN GRAPH routes WITH COST \
					 fn::cost RETURN path::cost(path) AS cost;",
					3,
				),
				(
					"MATCH REPEATABLE ELEMENTS path = (person:n0)-[link:e0 AS \
					 edge]->(person:n1)<-[link:e0 AS edge]-(person:n0) IN GRAPH routes WITH COST \
					 fn::cost RETURN edge, path::cost(path) AS cost;",
					4,
				),
				(
					"MATCH ATOM path = <(person:n0)>~[link:to_edge AS edge]~><[link:e0]> IN GRAPH \
					 routes WITH COST fn::cost RETURN edge, path::cost(path) AS cost;",
					2,
				),
			] {
				let rows = result(&mut tx, query);
				assert_eq!(rows.as_array().assert_some().len(), 1, "{query}");
				assert_eq!(
					rows.as_array()
						.assert_some()
						.get(0)
						.assert_some()
						.as_object()
						.assert_some()
						.get("cost"),
					Some(&Value::from(expected)),
					"{query}"
				);
			}
			let graph_query = "MATCH path = (person:n0)-[link*1..3 TRAIL WEIGHTED \
			                   SHORTEST]->(person:n3) IN GRAPH routes WITH COST fn::cost RETURN \
			                   GRAPH { nodes: [], edges: path::edges(path) };";
			let local_graph = graph_query
				.replace(" WITH COST fn::cost", "")
				.replace("WEIGHTED SHORTEST]", "WEIGHTED SHORTEST COST fn::cost]");
			let actual = result(&mut tx, graph_query);
			let expected = result(&mut tx, &local_graph);
			assert_eq!(
				actual.as_materialized_graph().assert_some().node_count(),
				expected.as_materialized_graph().assert_some().node_count()
			);
			assert_eq!(
				actual.as_materialized_graph().assert_some().edge_count(),
				expected.as_materialized_graph().assert_some().edge_count()
			);
			let override_query = "MATCH path = (person:n0)-[link*2 WALK WEIGHTED SHORTEST COST \
			                      fn::override]->(person:n3) IN GRAPH routes WITH COST fn::cost \
			                      RETURN path::cost(path) AS cost;";
			let rows = result(&mut tx, override_query);
			assert_eq!(
				rows.as_array()
					.assert_some()
					.get(0)
					.assert_some()
					.as_object()
					.assert_some()
					.get("cost"),
				Some(&Value::from(2_i64))
			);
		}
		for query in [
			"MATCH (person:n0) IN GRAPH routes WITH COST fn::cost RETURN 1;",
			"MATCH (person:n0)-[link*1..3 SHORTEST]->(person:n3) IN GRAPH routes WITH COST \
			 fn::cost RETURN 1;",
			"MATCH (person:n0)-[link*1..3]->(person:n3) IN GRAPH routes WITH COST fn::missing \
			 RETURN 1;",
			"MATCH (person:n0)-[link*1..3 COST fn::override]->(person:n3) IN GRAPH routes WITH \
			 COST fn::missing RETURN 1;",
		] {
			assert!(store.execute(query, &Object::new()).assert_ok()[0].result.is_err(), "{query}");
		}
	}
}

/// Verifies global match cost uses connector scope for nested and hyper paths.
#[test]
fn global_match_cost_uses_connector_scope_for_nested_and_hyper_paths() {
	let store = routes_kind(GraphKind::Metagraph);
	let mut tx = store.begin();
	let parent = tx.named_graph("routes").assert_ok().assert_some();
	let child = tx.define_graph("child", GraphKind::Graph).assert_ok();
	let fee = tx.intern_property("fee").assert_ok();
	let owner = "person:owner".parse::<RecordId>().assert_ok();
	let cross = "link:cross".parse::<RecordId>().assert_ok();
	for record in [&owner, &cross] {
		tx.create_atom(record.clone(), PropertyMap::new()).assert_ok();
	}
	let owner = tx.attach_node(parent.id(), &owner, None, Some(child.id())).assert_ok();
	let first =
		tx.attach_node(child.id(), &"person:n1".parse().assert_ok(), None, None).assert_ok();
	let last = tx.attach_node(child.id(), &"person:n3".parse().assert_ok(), None, None).assert_ok();
	tx.attach_edge(
		parent.id(),
		&cross,
		None,
		EdgeTopology::directed([EdgeEndpoint::Local(owner)], [EdgeEndpoint::Nested(
			QualifiedAtomId::new([owner], first).assert_ok(),
		)]),
		None,
		None,
	)
	.assert_ok();
	let local_edge = "link:e1".parse::<RecordId>().assert_ok();
	tx.attach_edge(
		child.id(),
		&local_edge,
		None,
		EdgeTopology::directed([first], [last]),
		None,
		None,
	)
	.assert_ok();
	tx.set_base_property(&cross, fee, Value::from(2_i64)).assert_ok();
	tx.set_base_property(&local_edge, fee, Value::from(2_i64)).assert_ok();
	tx.set_overlay(parent.id(), &cross, fee, Value::from(7_i64)).assert_ok();
	tx.set_overlay(child.id(), &local_edge, fee, Value::from(11_i64)).assert_ok();
	tx.commit().assert_ok();
	assert!(
		store
			.execute(
				"DEFINE PURE COST FUNCTION fn::cost(edge: any) -> number { RETURN \
				 <number>$edge.fee; };",
				&Object::new(),
			)
			.assert_ok()[0]
			.result
			.is_ok()
	);
	let query = "MATCH path = (person:owner)-[link:cross AS cross]->(person:n1)-[link:e1 AS \
	             local]->(person:n3) IN GRAPH routes WITH COST fn::cost RETURN cross, local, \
	             path::cost(path) AS cost, path::cost(path, fn::cost) AS recomputed;";
	let rows = result(&mut store.begin(), query);
	assert_eq!(rows.as_array().assert_some().len(), 1);
	let row = rows.as_array().assert_some().get(0).assert_some().as_object().assert_some();
	assert_eq!(row.get("cost"), Some(&Value::from(18_i64)));
	assert_eq!(row.get("recomputed"), row.get("cost"));
	assert_eq!(
		row.get("cross").assert_some().as_atom_occurrence().assert_some().graph().id(),
		parent.id()
	);
	assert_eq!(
		row.get("local").assert_some().as_atom_occurrence().assert_some().graph().id(),
		child.id()
	);
	let local_query = query
		.replace(" WITH COST fn::cost", "")
		.replace(" AS cross]", " AS cross COST fn::cost]")
		.replace(" AS local]", " AS local COST fn::cost]");
	assert_eq!(result(&mut store.begin(), &local_query), rows);
	let paths = result(
		&mut store.begin(),
		"TRAVERSE FROM person:owner IN GRAPH routes WITH COST fn::cost |> tv::out() |> tv::out() \
		 |> tv::path();",
	);
	let path = paths.as_array().assert_some().get(0).assert_some().as_path().assert_some();
	assert_eq!(paths.as_array().assert_some().len(), 1);
	assert_eq!(path.cost(), Some(Number::Int(18)));
	assert_eq!(path.steps()[1].via_edge().assert_some().graph().id(), parent.id());
	assert_eq!(path.steps()[2].via_edge().assert_some().graph().id(), child.id());

	let hyper = weighted_traversal_routes(GraphKind::Hypergraph, [Number::Int(1); 8]);
	assert!(
		hyper
			.execute(
				"DEFINE PURE COST FUNCTION fn::cost(edge: any) -> number { RETURN 7; };",
				&Object::new(),
			)
			.assert_ok()[0]
			.result
			.is_ok()
	);
	let rows = result(
		&mut hyper.begin(),
		"MATCH path = (person:n1)-[link:hyper AS edge HYPEREDGES]->(person:n3) IN GRAPH routes \
		 WITH COST fn::cost RETURN path::cost(path) AS cost;",
	);
	assert_eq!(rows.as_array().assert_some().len(), 1);
	assert_eq!(
		rows.as_array().assert_some().get(0).assert_some().as_object().assert_some().get("cost"),
		Some(&Value::from(7_i64))
	);
	let outcomes = hyper
		.execute(
			"DEFINE PURE COST FUNCTION fn::bad(edge: any) -> number { RETURN -1; }; MATCH \
			 (person:n1)-[link:hyper]->(person:n3) IN GRAPH routes WITH COST fn::bad RETURN 1;",
			&Object::new(),
		)
		.assert_ok();
	assert!(outcomes[0].result.is_ok());
	assert!(outcomes[1].result.is_err());
}

/// Verifies custom path cost recomputes in order and preserves snapshot
/// occurrences.
#[test]
fn custom_path_cost_recomputes_in_order_and_preserves_snapshot_occurrences() {
	for kind in [GraphKind::Graph, GraphKind::Metagraph] {
		let store = routes_kind(kind);
		for outcome in store
			.execute(
				"UPDATE link SET fee = 2; DEFINE PURE COST FUNCTION fn::cost(edge: any) -> number \
				 { RETURN <number>$edge.fee; }; DEFINE PURE COST FUNCTION fn::state(edge: any, \
				 prefix: number) -> number { RETURN $prefix + 1; };",
				&Object::new(),
			)
			.assert_ok()
		{
			assert!(outcome.result.is_ok(), "{:?}", outcome.result);
		}
		let mut tx = store.begin();
		let rows = result(
			&mut tx,
			"MATCH path = (person:n0)-[link:e0]->(person:n1)-[link:e1]->(person:n3) IN GRAPH \
			 routes WITH COST fn::cost RETURN path;",
		);
		let path_value = rows
			.as_array()
			.assert_some()
			.get(0)
			.assert_some()
			.as_object()
			.assert_some()
			.get("path")
			.assert_some()
			.clone();
		let mut variables = Object::new();
		variables.insert("path", path_value.clone());
		for (query, expected) in [
			("RETURN path::cost($path, fn::cost);", Value::from(4_i64)),
			("RETURN path::cost($path, fn::state);", Value::from(3_i64)),
			(
				"TRAVERSE FROM [$path] |> tv::map(|$p| path::cost($p, fn::cost));",
				Value::Array(vec![Value::from(4_i64)].into()),
			),
			(
				"TRAVERSE FROM [$path] |> tv::limit(1) RETURN path::cost(current, fn::cost);",
				Value::Array(vec![Value::from(4_i64)].into()),
			),
			(
				"TRAVERSE FROM [path::cost($path, fn::cost)] |> tv::limit(1);",
				Value::Array(vec![Value::from(4_i64)].into()),
			),
			(
				"TRAVERSE FROM [$path] |> tv::limit(path::cost($path, fn::cost)) RETURN \
				 path::cost(current, fn::cost);",
				Value::Array(vec![Value::from(4_i64)].into()),
			),
		] {
			let rows = tx.execute(query, &variables).assert_ok();
			assert_eq!(rows[0].result.as_ref().assert_ok().value(), &expected, "{query}");
		}
		let select = tx
			.execute("SELECT path::cost($path, fn::cost) AS cost FROM person LIMIT 1;", &variables)
			.assert_ok();
		assert_eq!(
			select[0]
				.result
				.as_ref()
				.assert_ok()
				.value()
				.as_array()
				.assert_some()
				.get(0)
				.assert_some()
				.as_object()
				.assert_some()
				.get("cost"),
			Some(&Value::from(4_i64))
		);
		assert_eq!(
			result(
				&mut tx,
				"MATCH path = (person:n0) IN GRAPH routes RETURN path::cost(path, fn::cost) AS \
				 cost;"
			)
			.as_array()
			.assert_some()
			.get(0)
			.assert_some()
			.as_object()
			.assert_some()
			.get("cost"),
			Some(&Value::from(0_i64))
		);
		let mut writer = store.begin();
		let fee = writer.intern_property("fee").assert_ok();
		writer
			.set_base_property(&"link:e0".parse().assert_ok(), fee, Value::from(0_i64))
			.assert_ok();
		writer.commit().assert_ok();
		assert_eq!(
			tx.execute("RETURN path::cost($path, fn::cost);", &variables).assert_ok()[0]
				.result
				.as_ref()
				.assert_ok()
				.value(),
			&Value::from(4_i64)
		);
		let mut latest = store.begin();
		assert_eq!(
			latest.execute("RETURN path::cost($path);", &variables).assert_ok()[0]
				.result
				.as_ref()
				.assert_ok()
				.value(),
			&Value::from(4_i64)
		);
		assert!(
			latest.execute("RETURN path::cost($path, fn::cost);", &variables).assert_ok()[0]
				.result
				.is_err()
		);
		let mut private = store.begin();
		let graph = private.named_graph("routes").assert_ok().assert_some();
		private
			.set_overlay(graph.id(), &"link:e0".parse().assert_ok(), fee, Value::from(5_i64))
			.assert_ok();
		let rows = result(
			&mut private,
			"MATCH path = (person:n0)-[link:e0]->(person:n1)-[link:e1]->(person:n3) IN GRAPH \
			 routes RETURN path::cost(path, fn::cost) AS cost;",
		);
		assert_eq!(
			rows.as_array()
				.assert_some()
				.get(0)
				.assert_some()
				.as_object()
				.assert_some()
				.get("cost"),
			Some(&Value::from(7_i64))
		);
		drop(private);
		let mut serializable = store.begin();
		result(
			&mut serializable,
			"MATCH path = (person:n0)-[link:e0]->(person:n1)-[link:e1]->(person:n3) IN GRAPH \
			 routes RETURN path::cost(path, fn::cost) AS cost;",
		);
		serializable
			.create_atom("person:decision".parse().assert_ok(), PropertyMap::new())
			.assert_ok();
		let mut writer = store.begin();
		writer
			.set_base_property(&"link:e1".parse().assert_ok(), fee, Value::from(8_i64))
			.assert_ok();
		writer.commit().assert_ok();
		assert!(serializable.commit().is_err());
	}
}

/// Verifies custom path cost rejects invalid scopes functions arithmetic and
/// work limits.
#[test]
fn custom_path_cost_rejects_invalid_scopes_functions_arithmetic_and_work_limits() {
	let store = routes();
	for outcome in store
		.execute(
			"DEFINE PURE COST FUNCTION fn::unit(edge: any) -> number { RETURN 1; }; DEFINE PURE \
			 COST FUNCTION fn::max(edge: any) -> number { RETURN 9223372036854775807; }; DEFINE \
			 PURE COST FUNCTION fn::bad(edge: any) -> number { RETURN -1; }; DEFINE PURE FUNCTION \
			 fn::ordinary(edge: any) -> number { RETURN 1; };",
			&Object::new(),
		)
		.assert_ok()
	{
		assert!(outcome.result.is_ok(), "{:?}", outcome.result);
	}
	let query = "MATCH path = (person:n0)-[link:e0]->(person:n1)-[link:e1]->(person:n3) IN GRAPH \
	             routes RETURN path::cost(path, fn::max) AS cost;";
	assert!(
		store
			.execute(
				"ALTER FUNCTION fn::unit(any) { RETURN path::cost($edge, fn::unit); };",
				&Object::new(),
			)
			.assert_ok()[0]
			.result
			.is_err()
	);
	for invalid in [
		query.to_owned(),
		query.replace("fn::max", "fn::bad"),
		query.replace("fn::max", "fn::ordinary"),
		query.replace("fn::max", "fn::missing"),
		query.replace("fn::max", "1"),
		query.replace("path, fn::max", "1, fn::unit"),
		query.replace("path, fn::max", "path, fn::unit, 1"),
	] {
		assert!(
			store.execute(&invalid, &Object::new()).assert_ok()[0].result.is_err(),
			"{invalid}"
		);
	}
	for case in 0..4 {
		let mut tx = store.begin();
		let rows = result(
			&mut tx,
			&query.replace("RETURN path::cost(path, fn::max) AS cost;", "RETURN path;"),
		);
		let original = rows
			.as_array()
			.assert_some()
			.get(0)
			.assert_some()
			.as_object()
			.assert_some()
			.get("path")
			.assert_some()
			.as_path()
			.assert_some();
		let mut steps = original.steps().to_vec();
		let origin = if case == 0 {
			uuid::Uuid::nil().into()
		} else {
			original.origin()
		};
		let snapshot = if case == 1 {
			crate::graph::SnapshotId::new(0, 0)
		} else {
			original.snapshot()
		};
		if case == 2 {
			steps[1] = crate::graph::PathStep::new(
				steps[1].atom().clone(),
				Some(steps[0].atom().clone()),
				steps[1].direction(),
				steps[1].role(),
			);
		}
		if case == 3 {
			let graph = tx.named_graph("routes").assert_ok().assert_some();
			tx.detach_occurrence(graph.id(), &"link:e0".parse().assert_ok(), true).assert_ok();
		}
		let mut variables = Object::new();
		variables.insert(
			"path",
			Value::Path(Box::new(crate::graph::Path::new(origin, snapshot, steps, None))),
		);
		assert!(
			tx.execute("RETURN path::cost($path, fn::unit);", &variables).assert_ok()[0]
				.result
				.is_err(),
			"case={case}"
		);
	}
	let mut tx = store.begin();
	let rows = result(
		&mut tx,
		&query.replace("RETURN path::cost(path, fn::max) AS cost;", "RETURN path;"),
	);
	let mut variables = Object::new();
	variables.insert(
		"path",
		rows.as_array()
			.assert_some()
			.get(0)
			.assert_some()
			.as_object()
			.assert_some()
			.get("path")
			.assert_some()
			.clone(),
	);
	let options = crate::gql::QueryExecutionOptions {
		max_path_expansions: 1,
		..Default::default()
	};
	let outcomes = tx
		.execute_with_options("RETURN path::cost($path, fn::unit);", &variables, options)
		.assert_ok();
	assert_eq!(
		outcomes[0].result.as_ref().assert_err().status().code(),
		crate::gql_status::FerrosGrynnError::path_expansion_limit_exceeded(1).status().code()
	);
}

/// Verifies traverse cost matches full state paths and ready bindings.
#[test]
fn traverse_cost_matches_full_state_paths_and_ready_bindings() {
	for family in [GraphKind::Graph, GraphKind::Metagraph, GraphKind::Hypergraph] {
		let store = weighted_traversal_routes(family, [Number::Int(1); 8]);
		for outcome in store
			.execute(
				"UPDATE link SET fee = 2; DEFINE PURE COST FUNCTION fn::cost(edge: any) -> number \
				 { RETURN <number>$edge.fee; }; DEFINE PURE COST FUNCTION fn::state(edge: any, \
				 prefix: number) -> number { RETURN $prefix + <number>$edge.fee; }; DEFINE INDEX \
				 custom ON GRAPH routes PATH ALGORITHM LANDMARK_SSSP(4) COST fn::cost WITH \
				 (MAX_BYTES = 1048576);",
				&Object::new(),
			)
			.assert_ok()
		{
			assert!(outcome.result.is_ok(), "{:?}", outcome.result);
		}
		let mut baseline = Vec::new();
		for ready in [false, true] {
			if ready {
				rebuild_named(&store, "custom");
			}
			let mut tx = store.begin();
			let mut results = Vec::new();
			for function in ["cost", "state"] {
				for mode in ["TRAIL", "ACYCLIC", "SIMPLE", "WALK"] {
					let oracle_max = if mode == "WALK" {
						4
					} else {
						30
					};
					for (from, to) in [(0, 3), (1, 0), (0, 0), (3, 0), (0, 4)] {
						let oracle = result(
							&mut tx,
							&format!(
								"MATCH REPEATABLE ELEMENTS path = \
								 (person:n{from})-[link*0..{oracle_max} {mode} WEIGHTED SHORTEST \
								 COST fn::{function}]->(person:n{to}) IN GRAPH routes RETURN path;"
							),
						);
						let expected = oracle
							.as_array()
							.assert_some()
							.get(0)
							.map(|row| {
								row.as_object().assert_some().get("path").assert_some().clone()
							})
							.unwrap_or(Value::None);
						for atom in [false, true] {
							for method in ["weightedShortestPath", "distanceTo"] {
								let query = format!(
									"TRAVERSE {} CONSTRAINTS <{mode}> FROM [person:n{from}, \
									 person:n{from}] IN GRAPH routes WITH COST fn::{function} |> \
									 tv::{method}(person:n{to});",
									if atom {
										"ATOM"
									} else {
										""
									}
								);
								let rows = result(&mut tx, &query);
								let expected_value = if method == "distanceTo" {
									expected
										.as_path()
										.and_then(|path| path.cost())
										.map_or(Value::None, Value::Number)
								} else {
									expected.clone()
								};
								assert_eq!(
									rows,
									Value::Array(
										vec![expected_value.clone(), expected_value].into()
									),
									"{query}"
								);
								let explain = result(
									&mut tx,
									&format!("EXPLAIN ANALYZE FORMAT JSON {query}"),
								);
								let explain = explain.as_object().assert_some();
								let lookups = explain
									.get("analyze")
									.assert_some()
									.as_object()
									.assert_some()
									.get("landmark_lookups")
									.assert_some();
								if ready && function == "cost" && !expected.is_none() {
									assert!(lookups > &Value::from(0_i64), "{query}");
								} else if !ready || function == "state" {
									assert_eq!(lookups, &Value::from(0_i64), "{query}");
								}
								results.push(rows);
							}
						}
					}
				}
			}
			// Paths carry a transaction nonce; compare their public steps/cost.
			let normalize = |values: &[Value]| {
				values
					.iter()
					.map(|value| {
						value
							.as_array()
							.assert_some()
							.iter()
							.map(|item| {
								item.as_path().map(|path| (path.steps().to_vec(), path.cost()))
							})
							.collect::<Vec<_>>()
					})
					.collect::<Vec<_>>()
			};
			if ready {
				assert_eq!(normalize(&results), normalize(&baseline));
			} else {
				baseline = results;
			}
		}
	}
}

/// Verifies traverse cost navigation snapshots and fail closed limits.
#[test]
fn traverse_cost_navigation_snapshots_and_fail_closed_limits() {
	let store = routes();
	for outcome in store
		.execute(
			"UPDATE link SET fee = 2; DEFINE PURE COST FUNCTION fn::cost(edge: any) -> number { \
			 RETURN <number>$edge.fee; }; DEFINE PURE COST FUNCTION fn::state(edge: any, prefix: \
			 number) -> number { RETURN $prefix + 1; }; DEFINE PURE COST FUNCTION fn::bad(edge: \
			 any) -> number { RETURN -1; }; DEFINE PURE COST FUNCTION fn::zero(edge: any) -> \
			 number { RETURN 0; }; DEFINE INDEX custom ON GRAPH routes PATH ALGORITHM \
			 LANDMARK_SSSP(4) COST fn::cost WITH (MAX_BYTES = 1048576);",
			&Object::new(),
		)
		.assert_ok()
	{
		assert!(outcome.result.is_ok(), "{:?}", outcome.result);
	}
	rebuild_named(&store, "custom");
	let query = "TRAVERSE FROM person:n0 IN GRAPH routes WITH COST fn::cost |> \
	             tv::weightedShortestPath(person:n3);";
	let mut old = store.begin();
	let path = result(&mut old, query);
	assert_eq!(
		path.as_array().assert_some().get(0).assert_some().as_path().assert_some().cost(),
		Some(Number::Int(4))
	);
	let undirected = result(
		&mut old,
		&query.replace("FROM person:n0", "FROM person:n3").replace("(person:n3)", "(person:n0)"),
	);
	assert_eq!(
		undirected.as_array().assert_some().get(0).assert_some().as_path().assert_some().cost(),
		Some(Number::Int(6))
	);
	let navigation = "TRAVERSE FROM person:n0 IN GRAPH routes WITH COST fn::state |> tv::out() |> \
	                  tv::out() |> tv::path();";
	for value in result(&mut old, navigation).as_array().assert_some().iter() {
		assert_eq!(value.as_path().assert_some().cost(), Some(Number::Int(3)));
	}
	let atom = result(
		&mut old,
		"TRAVERSE ATOM FROM link:e0 IN GRAPH routes WITH COST fn::state |> tv::out() |> \
		 tv::path();",
	);
	assert_eq!(atom.as_array().assert_some().len(), 1);
	let atom = atom.as_array().assert_some().get(0).assert_some().as_path().assert_some();
	assert_eq!(atom.cost(), Some(Number::Int(1)));
	assert_eq!(atom.steps()[1].via_edge().assert_some().atom().id().to_string(), "link:from_edge");
	for invalid in [
		query.replace("fn::cost", "fn::bad"),
		query.replace("fn::cost", "fn::missing"),
		query.replace("tv::weightedShortestPath(person:n3)", "tv::shortestPath(person:n3)"),
		query.replace("tv::weightedShortestPath(person:n3)", "tv::limit(1)"),
		query.replace("TRAVERSE FROM", "TRAVERSE CONSTRAINTS <SHORTEST> FROM"),
		"TRAVERSE FROM person:n0 IN GRAPH routes WITH COST fn::bad |> tv::out() |> tv::count();"
			.to_owned(),
	] {
		assert!(
			store.execute(&invalid, &Object::new()).assert_ok()[0].result.is_err(),
			"{invalid}"
		);
	}
	let options = crate::gql::QueryExecutionOptions {
		max_path_expansions: 2,
		..Default::default()
	};
	let zero_walk = "TRAVERSE CONSTRAINTS <WALK> FROM person:n0 IN GRAPH routes WITH COST \
	                 fn::zero |> tv::weightedShortestPath(person:n3);";
	let failures =
		store.begin().execute_with_options(zero_walk, &Object::new(), options).assert_ok();
	assert_eq!(
		failures[0].result.as_ref().assert_err().status().code(),
		crate::gql_status::FerrosGrynnError::path_expansion_limit_exceeded(2).status().code()
	);
	let options = crate::gql::QueryExecutionOptions {
		barrier_memory_bytes: std::num::NonZeroUsize::new(128).assert_some(),
		..Default::default()
	};
	assert!(
		store.begin().execute_with_options(query, &Object::new(), options).assert_ok()[0]
			.result
			.is_err()
	);
	let mut private = store.begin();
	let graph = private.named_graph("routes").assert_ok().assert_some();
	let key = private.intern_property("fee").assert_ok();
	private
		.set_overlay(graph.id(), &"link:e0".parse().assert_ok(), key, Value::from(0_i64))
		.assert_ok();
	let private_path = result(&mut private, query);
	assert_eq!(
		private_path.as_array().assert_some().get(0).assert_some().as_path().assert_some().cost(),
		Some(Number::Int(2))
	);
	drop(private);
	let mut writer = store.begin();
	writer
		.set_overlay(graph.id(), &"link:e0".parse().assert_ok(), key, Value::from(0_i64))
		.assert_ok();
	writer.commit().assert_ok();
	assert_eq!(result(&mut old, query), path);
	let mut serializable = store.begin();
	result(&mut serializable, query);
	let key2 = serializable.intern_property("marker").assert_ok();
	serializable
		.set_overlay(graph.id(), &"link:e1".parse().assert_ok(), key2, Value::Bool(true))
		.assert_ok();
	let mut writer = store.begin();
	writer
		.set_overlay(graph.id(), &"link:e0".parse().assert_ok(), key, Value::from(1_i64))
		.assert_ok();
	writer.commit().assert_ok();
	assert!(serializable.commit().is_err());
}

/// Verifies traverse cost respects connector filters.
#[test]
fn traverse_cost_respects_connector_filters() {
	for family in [GraphKind::Graph, GraphKind::Hypergraph] {
		let store = weighted_traversal_routes(family, [Number::Int(1); 8]);
		assert!(
			store
				.execute(
					"DEFINE PURE COST FUNCTION fn::cost(edge: any) -> number { RETURN 1; };",
					&Object::new(),
				)
				.assert_ok()[0]
				.result
				.is_ok()
		);
		let mut tx = store.begin();
		for filter in ["WEIGHTED", "HYPEREDGES", "METAEDGES", "METAVERTICES", "META", "CROSS_LEVEL"]
		{
			for method in ["weightedShortestPath", "distanceTo"] {
				let rows = result(
					&mut tx,
					&format!(
						"TRAVERSE CONSTRAINTS <TRAIL {filter}> FROM person:n1 IN GRAPH routes \
						 WITH COST fn::cost |> tv::{method}(person:n3);"
					),
				);
				let value = rows.as_array().assert_some().get(0).assert_some();
				let accepted = filter == "WEIGHTED"
					|| filter == "HYPEREDGES" && family == GraphKind::Hypergraph;
				if !accepted {
					assert_eq!(value, &Value::None, "{family:?} {filter} {method}");
				} else if method == "distanceTo" {
					assert_eq!(value, &Value::from(1_i64));
				} else {
					assert_eq!(value.as_path().assert_some().cost(), Some(Number::Int(1)));
					if filter == "HYPEREDGES" {
						assert_eq!(
							value
								.as_path()
								.assert_some()
								.edges()
								.next()
								.assert_some()
								.atom()
								.id()
								.to_string(),
							"link:hyper"
						);
					}
				}
			}
		}
	}
}

/// Verifies landmark custom cost matches exhaustive reduction and zero cost
/// ties.
#[test]
fn landmark_custom_cost_matches_exhaustive_reduction_and_zero_cost_ties() {
	weighted_matches_exhaustive(GraphKind::Graph, true);
	weighted_matches_exhaustive(GraphKind::Metagraph, true);
}

/// Verifies landmark custom cost private writes old snapshots dependencies and
/// ssi.
#[test]
fn landmark_custom_cost_private_writes_old_snapshots_dependencies_and_ssi() {
	for consumer in [0, 1, 2] {
		let store = routes();
		for outcome in store
			.execute(
				"UPDATE link SET fee = 3; DEFINE PURE FUNCTION fn::fee(edge: any) -> number { \
				 RETURN <number>$edge.fee; }; DEFINE PURE COST FUNCTION fn::cost(edge: any) -> \
				 number { RETURN fn::fee($edge); }; DEFINE INDEX custom ON GRAPH routes PATH \
				 ALGORITHM LANDMARK_SSSP(4) COST fn::cost WITH (MAX_BYTES = 1048576);",
				&Object::new(),
			)
			.assert_ok()
		{
			assert!(outcome.result.is_ok(), "{:?}", outcome.result);
		}
		let rebuild = || {
			let outcomes =
				store.execute("REBUILD INDEX custom ON GRAPH routes;", &Object::new()).assert_ok();
			assert!(outcomes[0].result.is_ok(), "{outcomes:?}");
		};
		rebuild();
		let query = "MATCH path = (person:n0 AS a)-[link*1..3 WEIGHTED SHORTEST COST \
		             fn::cost]->(person:n3 AS b) IN GRAPH routes RETURN path::cost(path) AS cost;";
		let global_query = query
			.replace(" COST fn::cost", "")
			.replace("IN GRAPH routes RETURN", "IN GRAPH routes WITH COST fn::cost RETURN");
		let traversal_query = "TRAVERSE FROM person:n0 IN GRAPH routes WITH COST fn::cost |> \
		                       tv::distanceTo(person:n3);";
		let query = match consumer {
			1 => global_query.as_str(),
			2 => traversal_query,
			_ => query,
		};
		let explain = format!("EXPLAIN ANALYZE FORMAT JSON {query}");
		let lookups = |value: &Value| {
			value
				.as_object()
				.assert_some()
				.get("analyze")
				.assert_some()
				.as_object()
				.assert_some()
				.get("landmark_lookups")
				.assert_some()
				> &Value::from(0_i64)
		};
		let mut old = store.begin();
		let old_rows = result(&mut old, query);
		assert!(lookups(&result(&mut old, &explain)));
		let graph = store.schema_catalog().named_graph("routes").assert_some().id();
		let record = "link:e0".parse::<RecordId>().assert_ok();
		let mut private = store.begin();
		let key = private.intern_property("fee").assert_ok();
		private.set_overlay(graph, &record, key, Value::from(0_i64)).assert_ok();
		assert!(!lookups(&result(&mut private, &explain)));
		let private_rows = result(&mut private, query);
		assert_ne!(private_rows, old_rows);
		drop(private);
		assert_eq!(result(&mut store.begin(), query), old_rows);
		assert!(lookups(&result(&mut store.begin(), &explain)));

		// Pruning unvisited connectors still observes their captured
		// properties.
		let mut serializable = store.begin();
		assert!(lookups(&result(&mut serializable, &explain)));
		serializable
			.create_atom("person:decision".parse().assert_ok(), PropertyMap::new())
			.assert_ok();
		let mut writer = store.begin();
		writer
			.set_base_property(&"link:e7".parse().assert_ok(), key, Value::from(1_i64))
			.assert_ok();
		writer.commit().assert_ok();
		assert!(serializable.commit().is_err());
		assert_eq!(result(&mut old, query), old_rows);
		assert!(lookups(&result(&mut old, &explain)));
		assert!(!lookups(&result(&mut store.begin(), &explain)));
		rebuild();
		assert!(lookups(&result(&mut store.begin(), &explain)));
		let before = result(&mut store.begin(), &explain);
		let mut old_fallback = store.begin();
		old_fallback.set_overlay(graph, &record, key, Value::from(3_i64)).assert_ok();
		let fallback_before = result(&mut old_fallback, &explain);
		assert!(!lookups(&fallback_before));
		let altered =
			store.execute("ALTER FUNCTION fn::fee(any) { RETURN 1; };", &Object::new()).assert_ok();
		assert!(altered[0].result.is_ok(), "{altered:?}");
		let fallback_after = result(&mut store.begin(), &explain);
		assert!(!lookups(&fallback_after));
		assert_ne!(
			fallback_before.as_object().assert_some().get("plan_hash"),
			fallback_after.as_object().assert_some().get("plan_hash")
		);
		assert_eq!(
			result(&mut old_fallback, &explain).as_object().assert_some().get("plan_hash"),
			fallback_before.as_object().assert_some().get("plan_hash")
		);
		let exact = result(&mut store.begin(), query);
		assert_ne!(exact, old_rows);
		assert_eq!(result(&mut old, query), old_rows);
		rebuild();
		assert_eq!(result(&mut store.begin(), query), exact);
		let after = result(&mut store.begin(), &explain);
		assert!(lookups(&after));
		assert_ne!(
			before.as_object().assert_some().get("plan_hash"),
			after.as_object().assert_some().get("plan_hash")
		);
		assert!(lookups(&result(&mut old, &explain)));
	}
}

/// The same independent oracle verifies local ALT and scoped exact fallback.
fn weighted_matches_exhaustive(kind: GraphKind, custom: bool) {
	for weights in [
		[Number::Int(0), Number::Int(0), Number::Int(1), Number::Int(0), Number::Int(0)],
		[
			Number::Int(9_007_199_254_740_993),
			Number::Float(0.0),
			Number::Int(9_007_199_254_740_993),
			Number::Int(0),
			Number::Float(f64::from_bits(1)),
		],
		[
			Number::Float(0.1),
			Number::Float(0.2),
			Number::Float(0.3),
			Number::Float(0.0),
			Number::Int(0),
		],
		[
			Number::Decimal("0.0000000000000000000000000001".parse().assert_ok()),
			Number::Decimal("9007199254740993.123456789".parse().assert_ok()),
			Number::Int(9_007_199_254_740_993),
			Number::Int(0),
			Number::Int(0),
		],
	] {
		let edges = [(0, 1), (1, 2), (0, 2), (2, 0), (1, 1)]
			.into_iter()
			.zip(weights)
			.map(|((from, to), cost)| (from, to, cost))
			.collect::<Vec<_>>();
		let store = Datastore::in_memory();
		let index_cost = if custom {
			let definitions = store
				.execute(
					"DEFINE PURE FUNCTION fn::fee(edge: any) -> number { RETURN \
					 <number>$edge.fee; }; DEFINE PURE COST FUNCTION fn::cost(edge: any) -> \
					 number { RETURN fn::fee($edge); };",
					&Object::new(),
				)
				.assert_ok();
			assert!(definitions.iter().all(|result| result.result.is_ok()), "{definitions:?}");
			let catalog = store.schema_catalog();
			let function = catalog
				.functions()
				.find(|function| function.name().as_str() == "cost")
				.assert_some();
			GraphIndexCost::Function {
				function: function.id().assert_some(),
				version: function.version(),
			}
		} else {
			GraphIndexCost::Weighted
		};
		let objective = if custom {
			"WEIGHTED SHORTEST COST fn::cost"
		} else {
			"WEIGHTED SHORTEST"
		};
		let mut tx = store.begin();
		tx.define_class(ClassSchema::new("person", AtomKind::Node, []).assert_ok()).assert_ok();
		tx.define_class(ClassSchema::new("link", AtomKind::Edge, []).assert_ok()).assert_ok();
		let graph = tx.define_graph("routes", kind).assert_ok();
		let fee = tx.intern_property("fee").assert_ok();
		let mut nodes = Vec::new();
		for vertex in 0..3 {
			let record = format!("person:n{vertex}").parse::<RecordId>().assert_ok();
			tx.create_atom(record.clone(), PropertyMap::new()).assert_ok();
			nodes.push(tx.attach_node(graph.id(), &record, None, None).assert_ok());
		}
		for (ordinal, &(from, to, cost)) in edges.iter().enumerate() {
			let record = format!("link:e{ordinal}").parse::<RecordId>().assert_ok();
			let mut properties = PropertyMap::new();
			properties.insert(fee, Value::Number(cost));
			tx.create_atom(record.clone(), properties).assert_ok();
			tx.attach_edge(
				graph.id(),
				&record,
				None,
				EdgeTopology::directed([nodes[from]], [nodes[to]]),
				Some(if custom {
					Number::Int(7)
				} else {
					cost
				}),
				None,
			)
			.assert_ok();
		}
		for (name, direction) in [
			("weighted_out", TraversalDirection::Forward),
			("weighted_in", TraversalDirection::Backward),
		] {
			tx.define_graph_index(
				"routes",
				GraphIndex::new(
					name,
					GraphIndexDefinition::Path {
						algorithm: PathIndexAlgorithm::LandmarkSssp(3),
						cost: index_cost,
						patterns: Vec::new(),
					},
					Some(1 << 20),
				)
				.assert_ok()
				.with_direction(direction),
			)
			.assert_ok();
		}
		tx.commit().assert_ok();
		for ready in [false, true] {
			if ready {
				for name in ["weighted_out", "weighted_in"] {
					let rows = store
						.execute(&format!("REBUILD INDEX {name} ON GRAPH routes;"), &Object::new())
						.assert_ok();
					assert!(rows[0].result.is_ok(), "{:?}", rows[0].result);
				}
			}
			let all_targets = result(
				&mut store.begin(),
				&format!(
					"MATCH REPEATABLE ELEMENTS path = (person:n0 AS a)-[link*2..4 WALK \
					 {objective}]->(person AS b) IN GRAPH routes RETURN b, path::edges(path) AS \
					 edges, path::cost(path) AS cost;"
				),
			);
			let rows = all_targets.as_array().assert_some();
			assert_eq!(rows.len(), 3);
			for target in 0..3 {
				let expected =
					weighted_oracle(&edges, 0, target, false, 2, 4, "WALK").assert_some();
				let row = rows
					.iter()
					.map(|value| value.as_object().assert_some())
					.find(|row| {
						row.get("b")
							.assert_some()
							.as_atom_occurrence()
							.assert_some()
							.atom()
							.id()
							.to_string()
							== format!("person:n{target}")
					})
					.assert_some();
				assert_eq!(row.get("cost"), Some(&Value::Number(expected.0)));
				let records = row
					.get("edges")
					.assert_some()
					.as_array()
					.assert_some()
					.iter()
					.map(|atom| atom.as_atom_occurrence().assert_some().atom().id().to_string())
					.collect::<Vec<_>>();
				assert_eq!(
					records,
					expected.1.iter().map(|edge| format!("link:e{edge}")).collect::<Vec<_>>()
				);
			}
			let explain = result(
				&mut store.begin(),
				&format!(
					"EXPLAIN ANALYZE FORMAT JSON MATCH path = (person:n0 AS a)-[link*1..3 \
					 {objective}]->(person:n2 AS b) IN GRAPH routes RETURN path;"
				),
			);
			let lookups = explain
				.as_object()
				.assert_some()
				.get("analyze")
				.assert_some()
				.as_object()
				.assert_some()
				.get("landmark_lookups")
				.assert_some();
			assert_eq!(lookups > &Value::from(0_i64), ready && kind == GraphKind::Graph);
			for backward in [false, true] {
				let (left, right) = if backward {
					("<-", "-")
				} else {
					("-", "->")
				};
				for source in 0..3 {
					for target in 0..3 {
						for mode in ["WALK", "TRAIL", "ACYCLIC", "SIMPLE"] {
							for (min, max) in [(0, 3), (2, 4)] {
								let query = format!(
									"MATCH REPEATABLE ELEMENTS path = (person:n{source} AS \
									 a){left}[link*{min}..{max} {mode} \
									 {objective}]{right}(person:n{target} AS b) IN GRAPH routes \
									 RETURN path::edges(path) AS edges, path::cost(path) AS cost;"
								);
								let rows = result(&mut store.begin(), &query);
								let expected = weighted_oracle(
									&edges, source, target, backward, min, max, mode,
								);
								let rows = rows.as_array().assert_some();
								assert_eq!(
									rows.len(),
									usize::from(expected.is_some()),
									"{query} ready={ready}"
								);
								if let Some((cost, path)) = expected {
									let row = rows.get(0).assert_some().as_object().assert_some();
									assert_eq!(
										row.get("cost"),
										Some(&Value::Number(cost)),
										"{query} ready={ready}"
									);
									let records = row
										.get("edges")
										.assert_some()
										.as_array()
										.assert_some()
										.iter()
										.map(|atom| {
											atom.as_atom_occurrence()
												.assert_some()
												.atom()
												.id()
												.to_string()
										})
										.collect::<Vec<_>>();
									assert_eq!(
										records,
										path.iter()
											.map(|edge| format!("link:e{edge}"))
											.collect::<Vec<_>>(),
										"{query} ready={ready}"
									);
								}
							}
						}
					}
				}
			}
		}
	}
}
