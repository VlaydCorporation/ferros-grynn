#![cfg(windows)]
#![allow(clippy::unwrap_used, reason = "Public catalog test stops on the first unexpected error")]

use std::path::PathBuf;

mod common;

use ferros_grynn::{
	datastore::{
		ClassIndexState,
		ClassSchema,
		Datastore,
		GraphIndex,
		GraphIndexCost,
		GraphIndexDefinition,
		LocalOptions,
		NeighbourhoodAlgorithm,
		PathIndexAlgorithm,
		ReachabilityAlgorithm,
	},
	graph::{AtomKind, EdgeTopology, GraphKind, TraversalDirection, TraversalSemantics},
	values::{Object, PropertyMap, RecordId, Uuid, Value},
};

/// Removes the isolated database after all native handles have been dropped.
struct Directory(PathBuf);

#[test]
fn tensor_cache_sync_sdk_formats_checkpoint_reopen_and_snapshot() {
	use ferros_grynn::{
		datastore::{CheckpointMode, MaintenanceOptions, TensorCacheFormat},
		graph::tensor::{TensorOptions, TensorScope},
	};
	let directory = Directory(
		std::env::temp_dir()
			.join(format!("fgdb-{:016x}", Uuid::now_v7().as_inner().as_u128() as u64)),
	);
	let options = LocalOptions::default();
	let store = Datastore::create_local(&directory.0, options).unwrap();
	let (graph, _, _) = common::commit_people_graph(&store);
	let baseline = store.begin().endpoint_tensor(&graph, TensorOptions::default()).unwrap();
	let mut define = store.begin();
	let ids =
		[TensorCacheFormat::Csr, TensorCacheFormat::Dense, TensorCacheFormat::Auto].map(|format| {
			define
				.define_graph_index(
					"people",
					GraphIndex::new_tensor(
						format!("tensor_{format:?}"),
						TensorScope::Local,
						format,
						1 << 20,
					)
					.unwrap(),
				)
				.unwrap()
		});
	define.commit().unwrap();
	for id in ids {
		store.rebuild_graph_index(graph.id(), id, MaintenanceOptions::default()).unwrap();
	}
	let mut old = store.begin();
	for id in ids {
		let cached = old.endpoint_tensor_using_index(&graph, TensorOptions::default(), id).unwrap();
		assert_eq!(cached.atoms(), baseline.atoms());
		assert_eq!(cached.connectors(), baseline.connectors());
	}
	let mut mutation = store.begin();
	mutation.detach_occurrence(graph.id(), &"knows:alice-bob".parse().unwrap(), false).unwrap();
	mutation.commit().unwrap();
	assert!(
		store
			.begin()
			.endpoint_tensor_using_index(&graph, TensorOptions::default(), ids[0])
			.is_err()
	);
	assert_eq!(
		old.endpoint_tensor_using_index(&graph, TensorOptions::default(), ids[0])
			.unwrap()
			.connectors()
			.len(),
		1
	);
	drop(old);
	for id in ids {
		store.rebuild_graph_index(graph.id(), id, MaintenanceOptions::default()).unwrap();
	}
	assert!(store.checkpoint(CheckpointMode::Restart).unwrap());
	drop(store);
	let reopened = Datastore::open_local(&directory.0, options).unwrap();
	for id in ids {
		let cached = reopened
			.begin()
			.endpoint_tensor_using_index(&graph, TensorOptions::default(), id)
			.unwrap();
		assert!(cached.connectors().is_empty());
		assert_eq!(cached.atoms().len(), 2);
	}
	drop(reopened);
	let mut constrained = options;
	constrained.maintenance.max_memory_bytes = std::num::NonZeroUsize::new(1024).unwrap();
	let constrained = Datastore::open_local(&directory.0, constrained).unwrap();
	for id in ids {
		assert!(
			constrained
				.begin()
				.endpoint_tensor_using_index(&graph, TensorOptions::default(), id)
				.is_err()
		);
	}
	assert!(constrained.begin().atom(&"person:alice".parse().unwrap()).unwrap().is_some());
	drop(constrained);
	let restored = Datastore::open_local(&directory.0, options).unwrap();
	for id in ids {
		assert!(
			restored
				.begin()
				.endpoint_tensor_using_index(&graph, TensorOptions::default(), id)
				.is_ok()
		);
	}
}

impl Drop for Directory {
	fn drop(&mut self) {
		drop(std::fs::remove_dir_all(&self.0));
	}
}

#[test]
fn graph_index_definition_commit_abort_conflict_snapshot_and_reopen() {
	let directory = Directory(
		std::env::temp_dir()
			.join(format!("fgdb-{:016x}", Uuid::now_v7().as_inner().as_u128() as u64)),
	);
	let options = LocalOptions::default();
	let store = Datastore::create_local(&directory.0, options).unwrap();
	let mut define = store.begin();
	define.define_graph("network", GraphKind::Graph).unwrap();
	define.commit().unwrap();
	let mut old = store.begin();
	let definition = || {
		GraphIndex::new(
			"reach",
			GraphIndexDefinition::Reachability {
				algorithm: ReachabilityAlgorithm::Grail(2),
			},
			Some(1024 * 1024),
		)
		.unwrap()
		.with_mode(TraversalSemantics::Atom)
		.with_direction(TraversalDirection::Both)
		.with_edge_labels(["road", "rail"])
		.unwrap()
	};
	let mut abort = store.begin();
	abort.define_graph_index("network", definition()).unwrap();
	drop(abort);
	assert!(store.begin().graphs().unwrap()[0].indexes().next().is_none());
	let mut first = store.begin();
	let mut second = store.begin();
	let id = first.define_graph_index("network", definition()).unwrap();
	second.define_graph_index("network", definition()).unwrap();
	first.commit().unwrap();
	assert!(second.commit().is_err());
	assert!(old.graphs().unwrap()[0].indexes().next().is_none());
	drop(old);
	let schemas = store.begin().graphs().unwrap();
	let index = schemas[0].indexes().next().unwrap();
	assert_eq!(index.id(), Some(id));
	assert_eq!(index.state(), ClassIndexState::Stale);
	let expected = index.fingerprint().unwrap();
	drop(store);
	let reopened = Datastore::open_local(&directory.0, options).unwrap();
	let schemas = reopened.begin().graphs().unwrap();
	let actual = schemas[0].indexes().next().unwrap();
	assert_eq!(actual.id(), Some(id));
	assert_eq!(actual.fingerprint().unwrap(), expected);
	let mut remove = reopened.begin();
	assert!(remove.remove_graph_index("network", "adjacency").is_err());
	remove.remove_graph_index("network", "reach").unwrap();
	remove.commit().unwrap();
	drop(reopened);
	assert!(
		Datastore::open_local(&directory.0, options).unwrap().begin().graphs().unwrap()[0]
			.indexes()
			.next()
			.is_none()
	);
}

#[test]
fn derived_graph_artifacts_publish_reuse_reopen_and_invalidate_in_own_graph() {
	let directory = Directory(
		std::env::temp_dir()
			.join(format!("fgdb-{:016x}", Uuid::now_v7().as_inner().as_u128() as u64)),
	);
	let options = LocalOptions::default();
	let store = Datastore::create_local(&directory.0, options).unwrap();
	let mut setup = store.begin();
	setup.define_class(ClassSchema::new("person", AtomKind::Node, []).unwrap()).unwrap();
	setup.define_class(ClassSchema::new("link", AtomKind::Edge, []).unwrap()).unwrap();
	let graph = setup.define_graph("network", GraphKind::Graph).unwrap();
	let forest = setup.define_graph("forest", GraphKind::Forest).unwrap();
	let records = ["person:a", "person:b", "person:c", "link:ab", "link:bc"]
		.map(|record| record.parse::<RecordId>().unwrap());
	for record in &records {
		setup.create_atom(record.clone(), PropertyMap::new()).unwrap();
	}
	for graph in [graph.id(), forest.id()] {
		let nodes = records[..3]
			.iter()
			.map(|record| setup.attach_node(graph, record, None, None).unwrap())
			.collect::<Vec<_>>();
		for (ordinal, pair) in nodes.windows(2).enumerate() {
			setup
				.attach_edge(
					graph,
					&records[3 + ordinal],
					Some("keep".into()),
					EdgeTopology::directed([pair[0]], [pair[1]]),
					None,
					None,
				)
				.unwrap();
		}
	}
	for (ordinal, algorithm) in [
		ReachabilityAlgorithm::Grail(2),
		ReachabilityAlgorithm::Ferrari(4),
		ReachabilityAlgorithm::Bfl(160),
		ReachabilityAlgorithm::ChainCover,
	]
	.into_iter()
	.enumerate()
	{
		setup
			.define_graph_index(
				"network",
				GraphIndex::new(
					format!("reach{ordinal}"),
					GraphIndexDefinition::Reachability {
						algorithm,
					},
					Some(1 << 20),
				)
				.unwrap()
				.with_mode(TraversalSemantics::Atom),
			)
			.unwrap();
	}
	setup
		.define_graph_index(
			"forest",
			GraphIndex::new(
				"tree",
				GraphIndexDefinition::Reachability {
					algorithm: ReachabilityAlgorithm::TreeCover,
				},
				Some(1 << 20),
			)
			.unwrap(),
		)
		.unwrap();
	for (ordinal, algorithm) in [
		NeighbourhoodAlgorithm::Exact,
		NeighbourhoodAlgorithm::Sketch(12),
		NeighbourhoodAlgorithm::Landmark(2),
		NeighbourhoodAlgorithm::Auto,
	]
	.into_iter()
	.enumerate()
	{
		setup
			.define_graph_index(
				"network",
				GraphIndex::new(
					format!("near{ordinal}"),
					GraphIndexDefinition::Neighbourhood {
						radius: 4,
						algorithm,
					},
					Some(if ordinal == 3 {
						1
					} else {
						1 << 20
					}),
				)
				.unwrap()
				.with_mode(TraversalSemantics::Atom),
			)
			.unwrap();
	}
	for (name, cost) in [
		("path_hop", GraphIndexCost::Hop),
		("path_weight", GraphIndexCost::Weighted),
		("path_node", GraphIndexCost::Hop),
		("path_node_weight", GraphIndexCost::Weighted),
	] {
		setup
			.define_graph_index(
				"network",
				GraphIndex::new(
					name,
					GraphIndexDefinition::Path {
						algorithm: PathIndexAlgorithm::LandmarkSssp(3),
						cost,
						patterns: Vec::new(),
					},
					Some(1 << 20),
				)
				.unwrap()
				.with_mode(if matches!(name, "path_node" | "path_node_weight") {
					TraversalSemantics::Node
				} else {
					TraversalSemantics::Atom
				}),
			)
			.unwrap();
	}
	setup.commit().unwrap();
	for outcome in store
		.execute(
			"UPDATE link SET fee = 3; UPDATE link:ab IN GRAPH network SET fee = 1; DEFINE PURE \
			 FUNCTION fn::fee(edge: any) -> number { RETURN <number>$edge.fee; }; DEFINE PURE \
			 COST FUNCTION fn::cost(edge: any) -> number { RETURN fn::fee($edge); }; DEFINE INDEX \
			 path_custom ON GRAPH network PATH MODE ATOM ALGORITHM LANDMARK_SSSP(3) COST fn::cost \
			 WITH (MAX_BYTES = 1048576); DEFINE INDEX path_node_custom ON GRAPH network PATH MODE \
			 NODE ALGORITHM LANDMARK_SSSP(3) COST fn::cost WITH (MAX_BYTES = 1048576);",
			&Object::new(),
		)
		.unwrap()
	{
		assert!(outcome.result.is_ok(), "{:?}", outcome.result);
	}
	let estimate = "EXPLAIN ANALYZE FORMAT JSON TRAVERSE ATOM FROM person:a IN GRAPH @network |> \
	                tv::kHopEstimate(4);";
	let hits = |value: &Value| {
		value
			.as_object()
			.unwrap()
			.get("analyze")
			.unwrap()
			.as_object()
			.unwrap()
			.get("neighbourhood_sketch_reads")
			.cloned()
	};
	for target in [
		"reach0 ON GRAPH network",
		"reach1 ON GRAPH network",
		"reach2 ON GRAPH network",
		"reach3 ON GRAPH network",
		"tree ON GRAPH forest",
		"near0 ON GRAPH network",
		"near1 ON GRAPH network",
		"near2 ON GRAPH network",
		"near3 ON GRAPH network",
		"path_hop ON GRAPH network",
		"path_node ON GRAPH network",
		"path_node_weight ON GRAPH network",
		"path_weight ON GRAPH network",
		"path_custom ON GRAPH network",
		"path_node_custom ON GRAPH network",
	] {
		let result = store.execute(&format!("REBUILD INDEX {target};"), &Object::new()).unwrap();
		assert!(result[0].result.is_ok(), "{target}: {:?}", result[0].result);
	}
	assert!(
		store
			.begin()
			.graphs()
			.unwrap()
			.iter()
			.flat_map(|graph| graph.indexes())
			.all(|index| index.state() == ClassIndexState::Ready)
	);
	let mut unrelated = store.begin();
	unrelated.create_atom("person:unrelated".parse().unwrap(), PropertyMap::new()).unwrap();
	unrelated.commit().unwrap();
	drop(store);
	let reopened = Datastore::open_local(&directory.0, options).unwrap();
	assert_eq!(
		hits(
			reopened.execute(estimate, &Object::new()).unwrap()[0].result.as_ref().unwrap().value()
		),
		Some(Value::from(1_u64))
	);
	let mut old = reopened.begin();
	let traversal = "EXPLAIN ANALYZE FORMAT JSON TRAVERSE FROM person:a IN GRAPH @network |> \
	                 tv::shortestPath(person:c);";
	let rows = old.execute(traversal, &Object::new()).unwrap();
	let metrics = rows[0]
		.result
		.as_ref()
		.unwrap()
		.value()
		.as_object()
		.unwrap()
		.get("analyze")
		.unwrap()
		.as_object()
		.unwrap();
	assert!(metrics.get("landmark_lookups").unwrap() > &Value::from(0_u64));
	let paths = old
		.execute(&traversal.replace("EXPLAIN ANALYZE FORMAT JSON ", ""), &Object::new())
		.unwrap();
	let path = paths[0]
		.result
		.as_ref()
		.unwrap()
		.value()
		.as_array()
		.unwrap()
		.get(0)
		.unwrap()
		.as_path()
		.unwrap();
	assert_eq!(path.steps().len(), 3);
	assert_eq!(path.cost(), Some(ferros_grynn::values::Number::Int(2)));
	for (custom, method) in [
		(false, "weightedShortestPath"),
		(false, "distanceTo"),
		(true, "weightedShortestPath"),
		(true, "distanceTo"),
	] {
		let mut query = traversal.replace("shortestPath", method);
		if custom {
			query = query.replace("IN GRAPH @network", "IN GRAPH @network WITH COST fn::cost");
		}
		let expected_cost = if custom {
			4
		} else {
			2
		};
		let rows = old.execute(&query, &Object::new()).unwrap();
		let metrics = rows[0]
			.result
			.as_ref()
			.unwrap()
			.value()
			.as_object()
			.unwrap()
			.get("analyze")
			.unwrap()
			.as_object()
			.unwrap();
		assert!(metrics.get("landmark_lookups").unwrap() > &Value::from(0_u64));
		let rows = old
			.execute(&query.replace("EXPLAIN ANALYZE FORMAT JSON ", ""), &Object::new())
			.unwrap();
		let value = rows[0].result.as_ref().unwrap().value().as_array().unwrap().get(0).unwrap();
		if let Some(path) = value.as_path() {
			assert_eq!(path.steps().len(), 3);
			assert_eq!(path.cost(), Some(ferros_grynn::values::Number::Int(expected_cost)));
		} else {
			assert_eq!(value, &Value::from(expected_cost));
		}
	}
	let shortest = "EXPLAIN ANALYZE FORMAT JSON MATCH ATOM path = <(person:a AS a)>~[link*1..3 \
	                SHORTEST]~><(person:c AS c)> IN GRAPH network RETURN path;";
	let result = old.execute(shortest, &Object::new()).unwrap();
	let metrics = result[0]
		.result
		.as_ref()
		.unwrap()
		.value()
		.as_object()
		.unwrap()
		.get("analyze")
		.unwrap()
		.as_object()
		.unwrap();
	assert!(metrics.get("landmark_lookups").unwrap() > &Value::from(0_i64));
	let weighted = shortest.replace("SHORTEST", "WEIGHTED SHORTEST");
	let custom = shortest.replace("SHORTEST", "WEIGHTED SHORTEST COST fn::cost");
	let global = custom
		.replace(" COST fn::cost", "")
		.replace("IN GRAPH network RETURN", "IN GRAPH network WITH COST fn::cost RETURN");
	let cost_query = custom.replace("EXPLAIN ANALYZE FORMAT JSON ", "").replace(
		"RETURN path;",
		"RETURN path::cost(path) AS cost, path::cost(path, fn::cost) AS recomputed;",
	);
	let costs = old.execute(&cost_query, &Object::new()).unwrap();
	let row = costs[0]
		.result
		.as_ref()
		.unwrap()
		.value()
		.as_array()
		.unwrap()
		.get(0)
		.unwrap()
		.as_object()
		.unwrap();
	assert_eq!(row.get("cost"), Some(&Value::from(4_i64)));
	assert_eq!(row.get("recomputed"), row.get("cost"));
	for query in [&weighted, &custom, &global] {
		let rows = old.execute(query, &Object::new()).unwrap();
		let metrics = rows[0]
			.result
			.as_ref()
			.unwrap()
			.value()
			.as_object()
			.unwrap()
			.get("analyze")
			.unwrap()
			.as_object()
			.unwrap();
		assert!(metrics.get("landmark_lookups").unwrap() > &Value::from(0_i64));
	}
	assert!(
		reopened
			.begin()
			.graphs()
			.unwrap()
			.iter()
			.flat_map(|graph| graph.indexes())
			.all(|index| index.state() == ClassIndexState::Ready)
	);
	let mut mutation = reopened.begin();
	mutation.detach_occurrence(graph.id(), &records[3], false).unwrap();
	let rows = mutation.execute(traversal, &Object::new()).unwrap();
	assert_eq!(
		rows[0]
			.result
			.as_ref()
			.unwrap()
			.value()
			.as_object()
			.unwrap()
			.get("analyze")
			.unwrap()
			.as_object()
			.unwrap()
			.get("landmark_lookups"),
		Some(&Value::from(0_u64))
	);
	let result = mutation.execute(shortest, &Object::new()).unwrap();
	let metrics = result[0]
		.result
		.as_ref()
		.unwrap()
		.value()
		.as_object()
		.unwrap()
		.get("analyze")
		.unwrap()
		.as_object()
		.unwrap();
	assert_eq!(metrics.get("landmark_lookups"), Some(&Value::from(0_i64)));
	assert_eq!(
		hits(
			mutation.execute(estimate, &Object::new()).unwrap()[0].result.as_ref().unwrap().value()
		),
		Some(Value::from(0_u64))
	);
	let schemas = mutation.graphs().unwrap();
	assert!(
		schemas
			.iter()
			.find(|schema| schema.id() == graph.id())
			.unwrap()
			.indexes()
			.all(|index| index.state() == ClassIndexState::Stale)
	);
	assert_eq!(
		schemas
			.iter()
			.find(|schema| schema.id() == forest.id())
			.unwrap()
			.indexes()
			.next()
			.unwrap()
			.state(),
		ClassIndexState::Ready
	);
	mutation.commit().unwrap();
	let rows = old.execute(traversal, &Object::new()).unwrap();
	assert!(
		rows[0]
			.result
			.as_ref()
			.unwrap()
			.value()
			.as_object()
			.unwrap()
			.get("analyze")
			.unwrap()
			.as_object()
			.unwrap()
			.get("landmark_lookups")
			.unwrap()
			> &Value::from(0_u64)
	);
	assert_eq!(
		hits(old.execute(estimate, &Object::new()).unwrap()[0].result.as_ref().unwrap().value()),
		Some(Value::from(1_u64))
	);
	drop(old);
	drop(reopened);
	let reopened = Datastore::open_local(&directory.0, options).unwrap();
	let schemas = reopened.begin().graphs().unwrap();
	assert!(
		schemas
			.iter()
			.find(|schema| schema.id() == graph.id())
			.unwrap()
			.indexes()
			.all(|index| index.state() == ClassIndexState::Stale)
	);
	assert_eq!(
		schemas
			.iter()
			.find(|schema| schema.id() == forest.id())
			.unwrap()
			.indexes()
			.next()
			.unwrap()
			.state(),
		ClassIndexState::Ready
	);
}
