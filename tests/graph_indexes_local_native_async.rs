#![cfg(all(windows, feature = "native-async-io"))]
#![allow(clippy::unwrap_used, reason = "Native backend contract stops on an unexpected error")]

use std::path::PathBuf;

use ferros_grynn::{
	datastore::{
		CheckpointMode,
		ClassIndexState,
		ClassSchema,
		Datastore,
		GraphIndex,
		GraphIndexCost,
		GraphIndexDefinition,
		IoMode,
		LocalOptions,
		NeighbourhoodAlgorithm,
		PathIndexAlgorithm,
		ReachabilityAlgorithm,
	},
	graph::{AtomKind, EdgeTopology, GraphKind},
	values::{Object, PropertyMap, RecordId, Uuid, Value},
};

/// Removes the isolated database after all native handles are released.
struct Directory(PathBuf);

impl Drop for Directory {
	fn drop(&mut self) {
		drop(std::fs::remove_dir_all(&self.0));
	}
}

#[test]
fn derived_graph_artifacts_native_async_publication_checkpoint_and_reopen() {
	let directory = Directory(
		std::env::temp_dir()
			.join(format!("fgdb-{:016x}", Uuid::now_v7().as_inner().as_u128() as u64,)),
	);
	let mut options = LocalOptions::default();
	options.io_mode = IoMode::NativeAsync;
	options.buffer_pool_pages = 16;
	let store = Datastore::create_local(&directory.0, options).unwrap();
	let mut tx = store.begin();
	tx.define_class(ClassSchema::new("person", AtomKind::Node, []).unwrap()).unwrap();
	tx.define_class(ClassSchema::new("link", AtomKind::Edge, []).unwrap()).unwrap();
	let graph = tx.define_graph("network", GraphKind::Graph).unwrap();
	let records =
		["person:a", "person:b", "link:ab"].map(|record| record.parse::<RecordId>().unwrap());
	for record in &records {
		tx.create_atom(record.clone(), PropertyMap::new()).unwrap();
	}
	let a = tx.attach_node(graph.id(), &records[0], None, None).unwrap();
	let b = tx.attach_node(graph.id(), &records[1], None, None).unwrap();
	tx.attach_edge(graph.id(), &records[2], None, EdgeTopology::directed([a], [b]), None, None)
		.unwrap();
	for (name, algorithm) in
		[("grail", ReachabilityAlgorithm::Grail(2)), ("chain", ReachabilityAlgorithm::ChainCover)]
	{
		tx.define_graph_index(
			"network",
			GraphIndex::new(
				name,
				GraphIndexDefinition::Reachability {
					algorithm,
				},
				Some(1 << 20),
			)
			.unwrap(),
		)
		.unwrap();
	}
	for (name, algorithm) in [
		("near_exact", NeighbourhoodAlgorithm::Exact),
		("near_sketch", NeighbourhoodAlgorithm::Sketch(12)),
		("near_landmark", NeighbourhoodAlgorithm::Landmark(2)),
	] {
		tx.define_graph_index(
			"network",
			GraphIndex::new(
				name,
				GraphIndexDefinition::Neighbourhood {
					radius: 4,
					algorithm,
				},
				Some(1 << 20),
			)
			.unwrap(),
		)
		.unwrap();
	}
	for (name, cost) in
		[("path_hop", GraphIndexCost::Hop), ("path_weight", GraphIndexCost::Weighted)]
	{
		tx.define_graph_index(
			"network",
			GraphIndex::new(
				name,
				GraphIndexDefinition::Path {
					algorithm: PathIndexAlgorithm::LandmarkSssp(2),
					cost,
					patterns: Vec::new(),
				},
				Some(1 << 20),
			)
			.unwrap(),
		)
		.unwrap();
	}
	tx.commit().unwrap();
	for outcome in store
		.execute(
			"UPDATE link SET fee = 3; UPDATE link:ab IN GRAPH network SET fee = 2; DEFINE PURE \
			 FUNCTION fn::fee(edge: any) -> number { RETURN <number>$edge.fee; }; DEFINE PURE \
			 COST FUNCTION fn::cost(edge: any) -> number { RETURN fn::fee($edge); }; DEFINE INDEX \
			 path_custom ON GRAPH network PATH ALGORITHM LANDMARK_SSSP(2) COST fn::cost WITH \
			 (MAX_BYTES = 1048576);",
			&Object::new(),
		)
		.unwrap()
	{
		assert!(outcome.result.is_ok(), "{:?}", outcome.result);
	}
	for result in store
		.execute(
			"REBUILD INDEX grail ON GRAPH network; REBUILD INDEX chain ON GRAPH network; REBUILD \
			 INDEX near_exact ON GRAPH network; REBUILD INDEX near_sketch ON GRAPH network; \
			 REBUILD INDEX near_landmark ON GRAPH network; REBUILD INDEX path_hop ON GRAPH \
			 network; REBUILD INDEX path_weight ON GRAPH network; REBUILD INDEX path_custom ON \
			 GRAPH network;",
			&Object::new(),
		)
		.unwrap()
	{
		assert!(result.result.is_ok(), "{:?}", result.result);
	}
	let mut old = store.begin();
	let estimate = "EXPLAIN ANALYZE FORMAT JSON TRAVERSE FROM person:a IN GRAPH @network |> \
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
	assert_eq!(
		hits(old.execute(estimate, &Object::new()).unwrap()[0].result.as_ref().unwrap().value()),
		Some(Value::from(1_u64))
	);
	assert!(store.checkpoint(CheckpointMode::Passive).unwrap());
	let traversal = "EXPLAIN ANALYZE FORMAT JSON TRAVERSE FROM person:a IN GRAPH @network |> \
	                 tv::shortestPath(person:b);";
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
	let shortest = "EXPLAIN ANALYZE FORMAT JSON MATCH path = (person:a AS a)-[link*1..3 \
	                SHORTEST]->(person:b AS b) IN GRAPH network RETURN path;";
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
	assert_eq!(row.get("cost"), Some(&Value::from(2_i64)));
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
	let mut remove = store.begin();
	remove.detach_occurrence(graph.id(), &records[2], false).unwrap();
	assert_eq!(
		hits(remove.execute(estimate, &Object::new()).unwrap()[0].result.as_ref().unwrap().value()),
		Some(Value::from(0_u64))
	);
	assert!(
		remove.graphs().unwrap()[0].indexes().all(|index| index.state() == ClassIndexState::Stale)
	);
	drop(remove);
	assert!(
		old.graphs().unwrap()[0].indexes().all(|index| index.state() == ClassIndexState::Ready)
	);
	drop(old);
	drop(store);
	let reopened = Datastore::open_local(&directory.0, options).unwrap();
	let rows = reopened.execute(traversal, &Object::new()).unwrap();
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
	let paths = reopened
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
	assert_eq!(path.steps().len(), 2);
	assert_eq!(path.cost(), Some(ferros_grynn::values::Number::Int(1)));
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
			2
		} else {
			1
		};
		let rows = reopened.execute(&query, &Object::new()).unwrap();
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
		let rows = reopened
			.execute(&query.replace("EXPLAIN ANALYZE FORMAT JSON ", ""), &Object::new())
			.unwrap();
		let value = rows[0].result.as_ref().unwrap().value().as_array().unwrap().get(0).unwrap();
		if let Some(path) = value.as_path() {
			assert_eq!(path.steps().len(), 2);
			assert_eq!(path.cost(), Some(ferros_grynn::values::Number::Int(expected_cost)));
		} else {
			assert_eq!(value, &Value::from(expected_cost));
		}
	}
	let costs = reopened.execute(&cost_query, &Object::new()).unwrap();
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
	assert_eq!(row.get("cost"), Some(&Value::from(2_i64)));
	assert_eq!(row.get("recomputed"), row.get("cost"));
	for query in [&weighted, &custom, &global] {
		let weighted_rows = reopened.execute(query, &Object::new()).unwrap();
		let weighted_metrics = weighted_rows[0]
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
		assert!(weighted_metrics.get("landmark_lookups").unwrap() > &Value::from(0_i64));
	}
	let result = reopened.execute(shortest, &Object::new()).unwrap();
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
	assert_eq!(
		hits(
			reopened.execute(estimate, &Object::new()).unwrap()[0].result.as_ref().unwrap().value()
		),
		Some(Value::from(1_u64))
	);
	assert!(
		reopened.begin().graphs().unwrap()[0]
			.indexes()
			.all(|index| index.state() == ClassIndexState::Ready)
	);
	let mut remove = reopened.begin();
	remove.detach_occurrence(graph.id(), &records[2], false).unwrap();
	remove.commit().unwrap();
	drop(reopened);
	let reopened = Datastore::open_local(&directory.0, options).unwrap();
	assert!(
		reopened.begin().graphs().unwrap()[0]
			.indexes()
			.all(|index| index.state() == ClassIndexState::Stale)
	);
}

#[test]
fn edge_endpoint_native_async_abort_old_snapshot_rebuild_and_reopen() {
	let directory = Directory(
		std::env::temp_dir()
			.join(format!("fgdb-{:016x}", Uuid::now_v7().as_inner().as_u128() as u64)),
	);
	let mut options = LocalOptions::default();
	options.io_mode = IoMode::NativeAsync;
	options.buffer_pool_pages = 16;
	let store = Datastore::create_local(&directory.0, options).unwrap();
	let mut tx = store.begin();
	tx.define_class(ClassSchema::new("person", AtomKind::Node, []).unwrap()).unwrap();
	tx.define_class(ClassSchema::new("relation", AtomKind::Edge, []).unwrap()).unwrap();
	let graph = tx.define_graph("network", GraphKind::Graph).unwrap();
	let records = ["person:a", "person:b", "relation:base", "relation:connector"]
		.map(|record| record.parse::<RecordId>().unwrap());
	for record in &records {
		tx.create_atom(record.clone(), PropertyMap::new()).unwrap();
	}
	let a = tx.attach_node(graph.id(), &records[0], None, None).unwrap();
	let b = tx.attach_node(graph.id(), &records[1], None, None).unwrap();
	let base = tx
		.attach_edge(graph.id(), &records[2], None, EdgeTopology::directed([a], [b]), None, None)
		.unwrap();
	tx.attach_edge(graph.id(), &records[3], None, EdgeTopology::directed([base], [b]), None, None)
		.unwrap();
	tx.commit().unwrap();
	let capability = store.begin().graphs().unwrap()[0].edge_endpoint_index().unwrap();
	let query =
		"TRAVERSE ATOM FROM relation:base IN GRAPH @network |> tv::out() |> tv::values('id');";
	let value = |store: &Datastore| {
		store.execute(query, &Object::new()).unwrap().remove(0).result.unwrap().into_parts().0
	};
	let before = value(&store);
	assert_eq!(before.as_array().unwrap().len(), 1);
	let mut old = store.begin();
	assert!(store.checkpoint(CheckpointMode::Passive).unwrap());
	let mut abort = store.begin();
	abort.detach_occurrence(graph.id(), &records[3], false).unwrap();
	drop(abort);
	assert_eq!(value(&store), before);
	let mut remove = store.begin();
	remove.detach_occurrence(graph.id(), &records[3], false).unwrap();
	remove.commit().unwrap();
	assert_eq!(
		old.execute(query, &Object::new()).unwrap().remove(0).result.unwrap().into_parts().0,
		before
	);
	assert!(value(&store).as_array().unwrap().is_empty());
	for result in store
		.execute(
			"INFO FOR INDEX edge_endpoint ON GRAPH network; REBUILD INDEX edge_endpoint ON GRAPH \
			 network;",
			&Object::new(),
		)
		.unwrap()
	{
		assert!(result.result.is_ok(), "{:?}", result.result);
	}
	drop(old);
	drop(store);
	let reopened = Datastore::open_local(&directory.0, options).unwrap();
	assert_eq!(reopened.begin().graphs().unwrap()[0].edge_endpoint_index(), Some(capability));
	assert!(value(&reopened).as_array().unwrap().is_empty());
}
