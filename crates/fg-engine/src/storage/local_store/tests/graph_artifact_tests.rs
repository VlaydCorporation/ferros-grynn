//! Catalog-only replacements must not publish an obsolete graph artifact.

use super::*;
use crate::datastore::{GraphIndex, GraphIndexDefinition, IoMode, ReachabilityAlgorithm};

/// Fresh definitions discard the old binding before a catalog-only commit.
#[test]
fn graph_artifact_sync_stale_and_on_demand_replacements_drop_old_binding() {
	replacements(IoMode::Sync, false);
}

/// A public Ready clone does not carry an artifact into its next version.
#[test]
fn graph_artifact_sync_ready_clone_requires_rebuild() {
	replacements(IoMode::Sync, true);
}

/// Native publication obeys the same availability contract as Sync.
#[cfg(feature = "native-async-io")]
#[test]
fn graph_artifact_native_async_replacements_drop_old_binding() {
	replacements(IoMode::NativeAsync, false);
	replacements(IoMode::NativeAsync, true);
}

/// Checks hot and cold replacements, rebuild, and the materialized-to-OnDemand
/// path.
fn replacements(io_mode: IoMode, ready_clone: bool) {
	for cold in [false, true] {
		let directory = TestDirectory::new();
		let options = LocalOptions {
			io_mode,
			..LocalOptions::default()
		};
		let mut store = Datastore::create_local(&directory.0, options).assert_ok();
		assert_eq!(store.local_storage_status().assert_some().options().io_mode, io_mode);
		let definition = GraphIndexDefinition::Reachability {
			algorithm: ReachabilityAlgorithm::Grail(2),
		};
		let mut seed = store.begin();
		seed.define_graph("g", GraphKind::Graph).assert_ok();
		let id = seed
			.define_graph_index(
				"g",
				GraphIndex::new("reach", definition.clone(), Some(1 << 20)).assert_ok(),
			)
			.assert_ok();
		seed.commit().assert_ok();
		let rebuild = "REBUILD INDEX reach ON GRAPH g;";
		store.execute(rebuild, &Object::new()).assert_ok().remove(0).result.assert_ok();
		if cold {
			drop(store);
			store = Datastore::open_local(&directory.0, options).assert_ok();
			assert_eq!(store.local_storage_status().assert_some().options().io_mode, io_mode);
		}
		let mut old = store.begin();
		let previous = old.graph_index("g", "reach").assert_ok().assert_some();
		assert_eq!(previous.state(), ClassIndexState::Ready);
		let replacement = if ready_clone {
			previous.clone()
		} else {
			GraphIndex::new("reach", definition, Some(1 << 20)).assert_ok()
		};
		let mut write = store.begin();
		write.overwrite_graph_index("g", replacement).assert_ok();
		write.commit().assert_ok();
		let current = store.begin().graph_index("g", "reach").assert_ok().assert_some();
		assert_eq!(current.state(), ClassIndexState::Stale);
		assert_eq!(current.version().get(), previous.version().get() + 1);
		assert_eq!(current.build_revision(), 0);
		assert!(!store.inner.current_root().reachability_artifacts.contains_key(&id));
		assert_no_binding(&store, id);
		assert_eq!(
			old.graph_index("g", "reach").assert_ok().assert_some().state(),
			ClassIndexState::Ready
		);
		drop(old);
		drop(store);
		let store = Datastore::open_local(&directory.0, options).assert_ok();
		assert_eq!(
			store.begin().graph_index("g", "reach").assert_ok().assert_some().state(),
			ClassIndexState::Stale
		);
		assert_no_binding(&store, id);
		store.execute(rebuild, &Object::new()).assert_ok().remove(0).result.assert_ok();
		assert_eq!(
			store.begin().graph_index("g", "reach").assert_ok().assert_some().state(),
			ClassIndexState::Ready
		);
		let mut write = store.begin();
		write
			.overwrite_graph_index(
				"g",
				GraphIndex::new(
					"reach",
					GraphIndexDefinition::Reachability {
						algorithm: ReachabilityAlgorithm::OnDemand,
					},
					None,
				)
				.assert_ok(),
			)
			.assert_ok();
		write.commit().assert_ok();
		assert_eq!(
			store.begin().graph_index("g", "reach").assert_ok().assert_some().state(),
			ClassIndexState::Ready
		);
		assert_no_binding(&store, id);
		store.checkpoint(CheckpointMode::Restart).assert_ok();
		drop(store);
		let store = Datastore::open_local(&directory.0, options).assert_ok();
		assert_eq!(
			store.begin().graph_index("g", "reach").assert_ok().assert_some().state(),
			ClassIndexState::Ready
		);
		assert_no_binding(&store, id);
	}
}

/// Checks the persisted catalog rather than only the runtime artifact map.
fn assert_no_binding(store: &Datastore, id: crate::datastore::IndexId) {
	let local = match &store.inner.backend {
		StoreBackend::Local(local) => Some(local),
		StoreBackend::InMemory => None,
	}
	.assert_some();
	assert!(
		local
			.state
			.lock()
			._database_catalog
			.graphs
			.iter()
			.all(|graph| !graph.index_bindings.contains_key(&id))
	);
}

/// PURE COST invalidation and the catalog binding change share one commit.
#[test]
fn graph_artifact_pure_cost_replacement_publishes_stale_and_rebuilds() {
	for io_mode in [IoMode::Sync, IoMode::NativeAsync] {
		if io_mode == IoMode::NativeAsync && !cfg!(feature = "native-async-io") {
			continue;
		}
		let directory = TestDirectory::new();
		let options = LocalOptions {
			io_mode,
			..LocalOptions::default()
		};
		let mut store = Datastore::create_local(&directory.0, options).assert_ok();
		let mut seed = store.begin();
		seed.define_class(ClassSchema::new("person", AtomKind::Node, []).assert_ok()).assert_ok();
		seed.define_class(ClassSchema::new("link", AtomKind::Edge, []).assert_ok()).assert_ok();
		let graph = seed.define_graph("g", GraphKind::Graph).assert_ok();
		let records =
			["person:a", "person:b", "link:ab"].map(|value| value.parse::<RecordId>().assert_ok());
		for record in &records {
			seed.create_atom(record.clone(), PropertyMap::new()).assert_ok();
		}
		let a = seed.attach_node(graph.id(), &records[0], None, None).assert_ok();
		let b = seed.attach_node(graph.id(), &records[1], None, None).assert_ok();
		seed.attach_edge(
			graph.id(),
			&records[2],
			None,
			EdgeTopology::directed([a], [b]),
			None,
			None,
		)
		.assert_ok();
		seed.commit().assert_ok();
		for outcome in store
			.execute(
				"DEFINE PURE COST FUNCTION fn::cost(edge: any) -> int { RETURN 1; }; DEFINE INDEX \
				 costs ON GRAPH g PATH ALGORITHM LANDMARK_SSSP(2) COST fn::cost WITH (MAX_BYTES = \
				 1048576); REBUILD INDEX costs ON GRAPH g;",
				&Object::new(),
			)
			.assert_ok()
		{
			outcome.result.assert_ok();
		}
		let id =
			store.begin().graph_index("g", "costs").assert_ok().assert_some().id().assert_some();
		for cold in [false, true] {
			if cold {
				drop(store);
				store = Datastore::open_local(&directory.0, options).assert_ok();
			}
			let mut old = store.begin();
			let before = old.graph_index("g", "costs").assert_ok().assert_some();
			assert_eq!(before.state(), ClassIndexState::Ready);
			let result = store
				.execute("ALTER FUNCTION fn::cost(any) { RETURN 2; };", &Object::new())
				.assert_ok();
			assert!(result[0].result.is_ok(), "{result:?}");
			assert_eq!(
				store.begin().graph_index("g", "costs").assert_ok().assert_some().state(),
				ClassIndexState::Stale
			);
			assert_no_binding(&store, id);
			assert!(!store.inner.current_root().path_artifacts.contains_key(&id));
			assert_eq!(
				old.graph_index("g", "costs").assert_ok().assert_some().state(),
				ClassIndexState::Ready
			);
			drop(old);
			store
				.execute("REBUILD INDEX costs ON GRAPH g;", &Object::new())
				.assert_ok()
				.remove(0)
				.result
				.assert_ok();
			let query =
				"TRAVERSE FROM person:a IN GRAPH g WITH COST fn::cost |> tv::distanceTo(person:b);";
			let result = store
				.execute(query, &Object::new())
				.assert_ok()
				.remove(0)
				.result
				.assert_ok()
				.into_parts()
				.0;
			assert_eq!(result.as_array().assert_some().get(0), Some(&Value::from(2_i64)));
		}
		store.checkpoint(CheckpointMode::Restart).assert_ok();
		drop(store);
		let store = Datastore::open_local(&directory.0, options).assert_ok();
		assert!(store.inner.current_root().path_artifacts.contains_key(&id));
	}
}
