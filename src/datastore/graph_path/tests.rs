//! Definition, interval payload and immutable publication contracts.

use super::*;
#[cfg(test)]
use crate::test_assertions::AssertOption as _;
#[cfg(test)]
use crate::test_assertions::AssertResult as _;
use crate::{
	datastore::{ClassIndexState, ClassSchema, Datastore, OccurrenceState},
	graph::{AtomKind, EdgeTopology, GraphKind},
	values::{Object, PropertyMap, RecordId, Value},
};

/// Requests a synchronous maintenance job and checks its real result.
fn rebuild(store: &Datastore, name: &str) {
	let result = store
		.execute(&format!("REBUILD INDEX {name} ON GRAPH network;"), &Object::new())
		.assert_ok();
	assert!(result[0].result.is_ok(), "{name}: {:?}", result[0].result);
}

/// Creates one definition with an explicit complete-page budget.
fn definition_for(name: &str, cost: GraphIndexCost) -> GraphIndex {
	GraphIndex::new(
		name,
		GraphIndexDefinition::Path {
			algorithm: PathIndexAlgorithm::LandmarkSssp(256),
			cost,
			patterns: Vec::new(),
		},
		Some(1 << 20),
	)
	.assert_ok()
}

/// Verifies landmark path artifact projections roundtrip budgets and snapshots.
#[test]
fn landmark_path_artifact_projections_roundtrip_budgets_and_snapshots() {
	let store = Datastore::in_memory();
	let mut tx = store.begin();
	tx.define_class(ClassSchema::new("person", AtomKind::Node, []).assert_ok()).assert_ok();
	tx.define_class(ClassSchema::new("link", AtomKind::Edge, []).assert_ok()).assert_ok();
	let graph = tx.define_graph("network", GraphKind::Graph).assert_ok();
	let records = [
		"person:a",
		"person:b",
		"person:c",
		"link:ab",
		"link:bc",
		"link:parallel",
		"link:endpoint",
	]
	.map(|value| value.parse::<RecordId>().assert_ok());
	for record in &records {
		tx.create_atom(record.clone(), PropertyMap::new()).assert_ok();
	}
	let a = tx.attach_node(graph.id(), &records[0], None, None).assert_ok();
	let b = tx.attach_node(graph.id(), &records[1], None, None).assert_ok();
	let c = tx.attach_node(graph.id(), &records[2], None, None).assert_ok();
	let ab = tx
		.attach_edge(
			graph.id(),
			&records[3],
			Some("keep".into()),
			EdgeTopology::directed([a], [b]),
			Some(Number::Int(0)),
			None,
		)
		.assert_ok();
	tx.attach_edge(
		graph.id(),
		&records[4],
		Some("keep".into()),
		EdgeTopology::directed([b], [c]),
		Some(Number::Int(5)),
		None,
	)
	.assert_ok();
	tx.attach_edge(
		graph.id(),
		&records[5],
		Some("keep".into()),
		EdgeTopology::directed([a], [b]),
		Some(Number::Int(9)),
		None,
	)
	.assert_ok();
	tx.attach_edge(
		graph.id(),
		&records[6],
		Some("keep".into()),
		EdgeTopology::directed([ab], [c]),
		Some(Number::Int(7)),
		None,
	)
	.assert_ok();
	let mut names = Vec::new();
	for mode in [TraversalSemantics::Node, TraversalSemantics::Atom] {
		for direction in
			[TraversalDirection::Forward, TraversalDirection::Backward, TraversalDirection::Both]
		{
			for cost in [GraphIndexCost::Hop, GraphIndexCost::Weighted] {
				let name = format!("path{}", names.len());
				let index = tx
					.define_graph_index(
						"network",
						definition_for(&name, cost)
							.with_mode(mode)
							.with_direction(direction)
							.with_edge_labels(["keep"])
							.assert_ok(),
					)
					.assert_ok();
				names.push((name, index));
			}
		}
	}
	tx.commit().assert_ok();
	let before = store.inner.current_root();
	for (name, _) in &names {
		rebuild(&store, name);
	}
	let ready = store.inner.current_root();
	assert!(before.path_artifacts.is_empty());
	assert_eq!(ready.path_artifacts.len(), names.len());
	for (name, id) in &names {
		let artifact = &ready.path_artifacts[id];
		let schema = ready.schema.graph(graph.id()).assert_some();
		let index = schema.indexes().find(|index| index.id() == Some(*id)).assert_some();
		let mut data: [Vec<u8>; 5] = std::array::from_fn(|_| Vec::new());
		for (section, bytes) in artifact.layout.sections.iter().zip(&mut data) {
			bytes.resize((section.count * u64::from(section.width)) as usize, 0);
			for (ordinal, output) in bytes.chunks_exact_mut(usize::from(section.width)).enumerate()
			{
				artifact.encode_element(section.kind, ordinal as u64, output).assert_ok();
			}
		}
		let restored = LandmarkPathArtifact::restore(
			ready.graph(graph.id()).assert_some(),
			index,
			artifact.layout.clone(),
			data.clone(),
			4 << 20,
			&mut || Ok(()),
		)
		.assert_ok();
		assert_eq!(restored.vertices, artifact.vertices);
		assert_eq!(restored.distances.landmarks, artifact.distances.landmarks);
		for vertex in 0..artifact.vertices.len() {
			for target in 0..artifact.vertices.len() {
				assert_eq!(
					restored.distances.lower_bound(vertex, target).assert_ok(),
					artifact.distances.lower_bound(vertex, target).assert_ok(),
					"{name}"
				);
			}
		}
		assert!(
			LandmarkPathArtifact::restore(
				ready.graph(graph.id()).assert_some(),
				index,
				artifact.layout.clone(),
				data.clone(),
				64,
				&mut || Ok(())
			)
			.is_err()
		);
		for section in 0..5 {
			let mut bad = data.clone();
			if section == 4 {
				bad[section][0] ^= 1;
			} else {
				bad[section][..usize::from(artifact.layout.sections[section].width)].fill(255);
			}
			assert!(
				LandmarkPathArtifact::restore(
					ready.graph(graph.id()).assert_some(),
					index,
					artifact.layout.clone(),
					bad,
					4 << 20,
					&mut || Ok(())
				)
				.is_err(),
				"{name}/{section}"
			);
		}
	}
	let weighted = &ready.path_artifacts[&names[1].1];
	let bound = weighted
		.distances
		.lower_bound(weighted.ordinal_by_atom[&a], weighted.ordinal_by_atom[&c])
		.assert_ok();
	assert!(bound > Number::Float(4.99) && bound <= Number::Int(5));
	let mut old = store.begin();
	let mut props = store.begin();
	let key = props.intern_property("other").assert_ok();
	props.set_base_property(&records[0], key, Value::from(7_i64)).assert_ok();
	props.commit().assert_ok();
	for (_, id) in &names {
		assert!(std::sync::Arc::ptr_eq(
			&ready.path_artifacts[id],
			&store.inner.current_root().path_artifacts[id]
		));
	}
	let mut abort = store.begin();
	abort.set_occurrence_label(graph.id(), &records[3], Some("drop".into())).assert_ok();
	assert!(
		abort.graphs().assert_ok()[0]
			.indexes()
			.all(|index| index.state() == ClassIndexState::Stale)
	);
	drop(abort);
	assert_eq!(store.inner.current_root().path_artifacts.len(), names.len());
	let mut mutation = store.begin();
	mutation
		.replace_occurrence_state(
			graph.id(),
			&records[4],
			OccurrenceState::edge(EdgeTopology::directed([b], [c]), Some(Number::Int(3)), None),
		)
		.assert_ok();
	assert_eq!(
		mutation.graphs().assert_ok()[0]
			.indexes()
			.filter(|index| index.state() == ClassIndexState::Stale)
			.count(),
		6
	);
	mutation.commit().assert_ok();
	assert_eq!(store.inner.current_root().path_artifacts.len(), 6);
	assert!(
		old.graphs().assert_ok()[0].indexes().all(|index| index.state() == ClassIndexState::Ready)
	);
}

/// Verifies landmark path pure cost nested calls overlay and transitive
/// invalidation.
#[test]
fn landmark_path_pure_cost_nested_calls_overlay_and_transitive_invalidation() {
	let store = Datastore::in_memory();
	let mut tx = store.begin();
	tx.define_class(ClassSchema::new("person", AtomKind::Node, []).assert_ok()).assert_ok();
	tx.define_class(ClassSchema::new("link", AtomKind::Edge, []).assert_ok()).assert_ok();
	let graph = tx.define_graph("network", GraphKind::Graph).assert_ok();
	let key = tx.intern_property("fee").assert_ok();
	let records =
		["person:a", "person:b", "link:ab"].map(|value| value.parse::<RecordId>().assert_ok());
	for record in &records {
		let mut properties = PropertyMap::new();
		properties.insert(key, Value::from(9_i64));
		tx.create_atom(record.clone(), properties).assert_ok();
	}
	let a = tx.attach_node(graph.id(), &records[0], None, None).assert_ok();
	let b = tx.attach_node(graph.id(), &records[1], None, None).assert_ok();
	tx.attach_edge(graph.id(), &records[2], None, EdgeTopology::directed([a], [b]), None, None)
		.assert_ok();
	tx.set_overlay(graph.id(), &records[2], key, Value::from(4_i64)).assert_ok();
	tx.commit().assert_ok();
	for result in store
		.execute(
			"DEFINE PURE FUNCTION fn::fee(edge: any) -> int { RETURN <int>$edge.fee; }; DEFINE \
			 PURE COST FUNCTION fn::cost(edge: any) -> int { LET $cost = fn::fee($edge); RETURN \
			 $cost; }; DEFINE INDEX costs ON GRAPH network PATH ALGORITHM LANDMARK_SSSP(2) COST \
			 fn::cost WITH (MAX_BYTES = 1048576);",
			&Object::new(),
		)
		.assert_ok()
	{
		assert!(result.result.is_ok(), "{:?}", result.result);
	}
	rebuild(&store, "costs");
	let ready = store.inner.current_root();
	let schema = ready.schema.graph(graph.id()).assert_some();
	let index = schema.indexes().next().assert_some();
	let id = index.id().assert_some();
	assert_eq!(index.dependencies().len(), 2);
	let artifact = &ready.path_artifacts[&id];
	let bound = artifact
		.distances
		.lower_bound(artifact.ordinal_by_atom[&a], artifact.ordinal_by_atom[&b])
		.assert_ok();
	assert!(bound > Number::Float(3.99) && bound <= Number::Int(4));
	let mut old = store.begin();
	let mut private = store.begin();
	private.set_overlay(graph.id(), &records[2], key, Value::from(2_i64)).assert_ok();
	assert_eq!(
		private.graphs().assert_ok()[0].indexes().next().assert_some().state(),
		ClassIndexState::Stale
	);
	drop(private);
	let result = store
		.execute("ALTER FUNCTION fn::fee(any) { RETURN <int>$edge.fee * 2; };", &Object::new())
		.assert_ok();
	assert!(result[0].result.is_ok(), "{:?}", result[0].result);
	assert!(store.inner.current_root().path_artifacts.is_empty());
	assert_eq!(
		old.graphs().assert_ok()[0].indexes().next().assert_some().state(),
		ClassIndexState::Ready
	);
	rebuild(&store, "costs");
	let next = store.inner.current_root();
	let artifact = &next.path_artifacts[&id];
	let bound = artifact
		.distances
		.lower_bound(artifact.ordinal_by_atom[&a], artifact.ordinal_by_atom[&b])
		.assert_ok();
	assert!(bound > Number::Float(7.99) && bound <= Number::Int(8));
	for result in store
		.execute(
			"DEFINE PURE FUNCTION fn::discount(edge: any) -> int { RETURN <int>$edge.fee; }; \
			 ALTER FUNCTION fn::fee(any) { RETURN fn::discount($edge); };",
			&Object::new(),
		)
		.assert_ok()
	{
		assert!(result.result.is_ok(), "{:?}", result.result);
	}
	rebuild(&store, "costs");
	let refreshed = store.inner.current_root();
	assert_eq!(
		refreshed
			.schema
			.graph(graph.id())
			.assert_some()
			.indexes()
			.next()
			.assert_some()
			.dependencies()
			.len(),
		3
	);
	let result = store
		.execute("ALTER FUNCTION fn::discount(any) { RETURN -1; };", &Object::new())
		.assert_ok();
	assert!(result[0].result.is_ok(), "{:?}", result[0].result);
	assert!(store.inner.current_root().path_artifacts.is_empty());
	let result = store.execute("REBUILD INDEX costs ON GRAPH network;", &Object::new()).assert_ok();
	assert!(result[0].result.is_err());
	assert!(store.inner.current_root().path_artifacts.is_empty());
}

/// Verifies landmark path empty budget and cancel never publish partial.
#[test]
fn landmark_path_empty_budget_and_cancel_never_publish_partial() {
	let store = Datastore::in_memory();
	let mut tx = store.begin();
	tx.define_graph("network", GraphKind::Graph).assert_ok();
	let id =
		tx.define_graph_index("network", definition_for("empty", GraphIndexCost::Hop)).assert_ok();
	let small = GraphIndex::new(
		"tiny",
		GraphIndexDefinition::Path {
			algorithm: PathIndexAlgorithm::LandmarkSssp(2),
			cost: GraphIndexCost::Hop,
			patterns: Vec::new(),
		},
		Some(1),
	)
	.assert_ok();
	let tiny = tx.define_graph_index("network", small).assert_ok();
	tx.commit().assert_ok();
	rebuild(&store, "empty");
	let root = store.inner.current_root();
	assert_eq!(root.path_artifacts[&id].layout.page_count, 2);
	assert!(root.path_artifacts[&id].distances.landmarks.is_empty());
	let result = store.execute("REBUILD INDEX tiny ON GRAPH network;", &Object::new()).assert_ok();
	assert!(result[0].result.is_err());
	assert!(!store.inner.current_root().path_artifacts.contains_key(&tiny));
	let index = root
		.schema
		.named_graph("network")
		.assert_some()
		.indexes()
		.find(|index| index.id() == Some(id))
		.assert_some()
		.clone();
	assert!(
		LandmarkPathArtifact::build(&root, &index, 99, 1 << 20, &mut || Err(
			FerrosGrynnError::query_cancelled()
		))
		.is_err()
	);
	assert!(LandmarkPathArtifact::build(&root, &index, 99, 64, &mut || Ok(())).is_err());
}
