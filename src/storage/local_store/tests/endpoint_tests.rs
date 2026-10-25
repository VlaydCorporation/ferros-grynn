//! Unchanged endpoint postings must remain usable at each database timestamp.

use super::*;
use crate::{
	datastore::{IoMode, QuerySnapshot, SchemaParameter, Transaction},
	graph::{AtomId, GraphRef},
	storage::wal::{CommitFailureGuard, CommitFailurePoint},
};

/// Data/catalog commits retain the endpoint file and its atomic WAL boundary.
#[test]
fn endpoint_revision_sync_point_commits_preserve_current_and_old_traversal() {
	endpoint_revisions(IoMode::Sync);
}

/// NativeAsync must expose the same exact snapshot headers through cold reopen.
#[cfg(feature = "native-async-io")]
#[test]
fn endpoint_revision_native_async_point_commits_preserve_current_and_old_traversal() {
	endpoint_revisions(IoMode::NativeAsync);
}

/// Establishes a real E-E capability before unrelated ordinary point commits.
fn endpoint_revisions(io_mode: IoMode) {
	let directory = TestDirectory::new();
	let options = LocalOptions {
		io_mode,
		..LocalOptions::default()
	};
	let store = Datastore::create_local(&directory.0, options).assert_ok();
	assert_eq!(store.local_storage_status().assert_some().options().io_mode, io_mode);
	store.inner.maintenance.shutdown();
	let mut seed = store.begin();
	let graph = seed.define_graph("g", GraphKind::Graph).assert_ok();
	let other = seed.define_graph("other", GraphKind::Graph).assert_ok();
	seed.define_class(
		ClassSchema::new("item", AtomKind::Node, [ClassField::new("value", Type::Int)]).assert_ok(),
	)
	.assert_ok();
	seed.define_class(ClassSchema::new("link", AtomKind::Edge, []).assert_ok()).assert_ok();
	let key = seed.intern_property("value").assert_ok();
	let records = ["item:a", "item:b", "item:c", "link:first", "link:connector"]
		.map(|record| record.parse::<RecordId>().assert_ok());
	let mut nodes = Vec::new();
	for record in &records[..3] {
		let mut properties = PropertyMap::new();
		properties.insert(key, 0_i64);
		seed.create_atom(record.clone(), properties).assert_ok();
		nodes.push(seed.attach_node(graph.id(), record, None, None).assert_ok());
	}
	for record in &records[3..] {
		seed.create_atom(record.clone(), PropertyMap::new()).assert_ok();
	}
	let endpoint = seed
		.attach_edge(
			graph.id(),
			&records[3],
			None,
			EdgeTopology::directed([nodes[0]], [nodes[1]]),
			None,
			None,
		)
		.assert_ok();
	let connector = seed
		.attach_edge(
			graph.id(),
			&records[4],
			None,
			EdgeTopology::directed([endpoint], [nodes[2]]),
			None,
			None,
		)
		.assert_ok();
	seed.commit().assert_ok();
	store.checkpoint(CheckpointMode::Restart).assert_ok();
	let variables = Object::from_iter([
		("graph", Value::GraphRef(Box::new(graph.clone()))),
		("source", Value::Record(Box::new(records[3].clone()))),
	]);
	let query = "TRAVERSE ATOM FROM $source IN GRAPH $graph |> tv::out() |> tv::values('id');";
	let expected =
		store.execute(query, &variables).assert_ok().remove(0).result.assert_ok().into_parts().0;
	assert_eq!(expected.as_array().assert_some().len(), 1);
	let mut old = store.begin();
	let snapshot = old.query_snapshot().assert_ok();
	assert_endpoint(&old, &snapshot, &graph, [endpoint, connector, nodes[2]]);
	let file = endpoint_file(&store, graph.id());
	for operation in 0..5 {
		let mutate = |write: &mut Transaction| match operation {
			0 => {
				write.set_base_property(&records[0], key, 1_i64.into()).assert_ok();
			}
			1 => write.set_overlay(graph.id(), &records[0], key, 2_i64.into()).assert_ok(),
			2 => {
				write.attach_node(other.id(), &records[0], None, None).assert_ok();
			}
			3 => {
				write.intern_property("catalog_only").assert_ok();
			}
			_ => {
				write
					.define_parameter(
						SchemaParameter::new("endpoint_parameter", Value::from(1)).assert_ok(),
					)
					.assert_ok();
			}
		};
		let stamp = store.inner.current_root().commit_ts;
		let boundary = endpoint_store(&store).wal.snapshot().visible_lsn();
		for point in [
			GenerationFailurePoint::AfterBuild,
			GenerationFailurePoint::AfterCatalogStage,
			GenerationFailurePoint::BeforeWalCommit,
		] {
			let mut write = store.begin();
			mutate(&mut write);
			{
				let _failure = GenerationFailureGuard::set(point);
				write.commit().assert_err();
			}
			assert_eq!(store.inner.current_root().commit_ts, stamp);
			assert_eq!(endpoint_store(&store).wal.snapshot().visible_lsn(), boundary);
			let mut read = store.begin();
			let current = read.query_snapshot().assert_ok();
			assert_endpoint(&read, &current, &graph, [endpoint, connector, nodes[2]]);
			assert_endpoint(&old, &snapshot, &graph, [endpoint, connector, nodes[2]]);
		}
		let mut write = store.begin();
		mutate(&mut write);
		write.commit().assert_ok();
		assert_eq!(endpoint_file(&store, graph.id()), file);
		let mut read = store.begin();
		let current = read.query_snapshot().assert_ok();
		assert_endpoint(&read, &current, &graph, [endpoint, connector, nodes[2]]);
		assert_endpoint(&old, &snapshot, &graph, [endpoint, connector, nodes[2]]);
		assert_eq!(
			store
				.execute(query, &variables)
				.assert_ok()
				.remove(0)
				.result
				.assert_ok()
				.into_parts()
				.0,
			expected
		);
	}
	store.checkpoint(CheckpointMode::Passive).assert_ok();
	assert_endpoint(&old, &snapshot, &graph, [endpoint, connector, nodes[2]]);
	drop(snapshot);
	drop(old);
	store.checkpoint(CheckpointMode::Restart).assert_ok();
	drop(store);
	let mut store = Datastore::open_local(&directory.0, options).assert_ok();
	store.inner.maintenance.shutdown();
	assert_eq!(store.local_storage_status().assert_some().options().io_mode, io_mode);
	assert_eq!(endpoint_file(&store, graph.id()), file);
	let mut read = store.begin();
	let snapshot = read.query_snapshot().assert_ok();
	assert_endpoint(&read, &snapshot, &graph, [endpoint, connector, nodes[2]]);
	assert_eq!(
		store.execute(query, &variables).assert_ok().remove(0).result.assert_ok().into_parts().0,
		expected
	);
	drop(snapshot);
	drop(read);
	for point in [
		CommitFailurePoint::BeforeAppend,
		CommitFailurePoint::AfterAppend,
		CommitFailurePoint::AfterFlush,
		CommitFailurePoint::AfterSync,
	] {
		let stamp = store.inner.current_root().commit_ts;
		let mut write = store.begin();
		write
			.set_base_property(&records[0], key, Value::from(i64::try_from(stamp).assert_ok()))
			.assert_ok();
		{
			let _failure = CommitFailureGuard::set(point);
			write.commit().assert_err();
		}
		assert_eq!(store.inner.current_root().commit_ts, stamp);
		drop(store);
		store = Datastore::open_local(&directory.0, options).assert_ok();
		store.inner.maintenance.shutdown();
		assert_eq!(store.local_storage_status().assert_some().options().io_mode, io_mode);
		assert_eq!(
			store.inner.current_root().commit_ts,
			stamp + u64::from(point != CommitFailurePoint::BeforeAppend)
		);
		assert_eq!(endpoint_file(&store, graph.id()), file);
		let mut read = store.begin();
		let current = read.query_snapshot().assert_ok();
		assert_endpoint(&read, &current, &graph, [endpoint, connector, nodes[2]]);
		assert_eq!(
			store
				.execute(query, &variables)
				.assert_ok()
				.remove(0)
				.result
				.assert_ok()
				.into_parts()
				.0,
			expected
		);
		drop(current);
		drop(read);
		store.checkpoint(CheckpointMode::Restart).assert_ok();
	}
}

/// Independently expects the connector and its target, rather than a fallback.
fn assert_endpoint(
	read: &Transaction,
	snapshot: &QuerySnapshot,
	graph: &GraphRef,
	ids: [AtomId; 3],
) {
	let result = read
		.query_physical_traversal(snapshot, graph, ids[0], true, false, true)
		.assert_ok()
		.assert_some();
	assert_eq!(result.candidates.len(), 1);
	assert_eq!((result.candidates[0].edge, result.candidates[0].target), (ids[1], ids[2]));
}

/// Exact file identity is retained even when its header receives a WAL version.
fn endpoint_file(
	store: &Datastore,
	graph: crate::graph::GraphId,
) -> crate::storage::types::DataFileId {
	endpoint_store(store)
		.state
		.lock()
		.bindings
		.graphs
		.iter()
		.find(|binding| binding.graph_id == graph)
		.assert_some()
		.edge_endpoint
		.assert_some()
		.file_id
}

/// Source snapshots and WAL assertions share the fixture's actual backend.
fn endpoint_store(store: &Datastore) -> &super::super::LocalStore {
	match &store.inner.backend {
		StoreBackend::Local(local) => local,
		StoreBackend::InMemory => unreachable!("endpoint fixture has a RAM backend"),
	}
}
