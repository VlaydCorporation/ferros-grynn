//! Coupled child NODE/EDGE lifecycles update every copied capacity and count.

use super::{support::*, *};

/// CASCADE, reuse and transient capacity changes share Sync publication.
#[test]
fn bounded_child_directory_sync_cascade_reuse_and_transient_capacities() {
	lifecycle(IoMode::Sync);
}

/// NativeAsync restores the same final generations and directory counters.
#[cfg(feature = "native-async-io")]
#[test]
fn bounded_child_directory_native_async_cascade_reuse_and_transient_capacities() {
	lifecycle(IoMode::NativeAsync);
}

/// Existing Base owners isolate graph-local capacity changes from global
/// allocation.
fn lifecycle(io_mode: IoMode) {
	let mut fixture = fixture(io_mode, 3);
	let records =
		["edge:branch", "edge:transient"].map(|name| name.parse::<RecordId>().assert_ok());
	let mut seed = fixture.store.begin();
	for record in &records {
		seed.create_atom(record.clone(), PropertyMap::new()).assert_ok();
	}
	seed.commit().assert_ok();
	fixture.store.checkpoint(CheckpointMode::Restart).assert_ok();
	fixture = reopen(fixture);
	let parent_files = files(&fixture, fixture.parents[0].id());
	let graph = fixture.children[0].id();
	let existing = "node:existing".parse::<RecordId>().assert_ok();
	let root = fixture.store.inner.current_root();
	let endpoint = root.membership(graph, root.resolve_atom(&existing).assert_some()).assert_some();
	drop(root);
	let mut attach = fixture.store.begin();
	let first_node = attach.attach_node(graph, &fixture.pending[0], None, None).assert_ok();
	let first_edge = attach
		.attach_edge(
			graph,
			&records[0],
			None,
			EdgeTopology::directed([endpoint], [first_node]),
			None,
			None,
		)
		.assert_ok();
	attach.commit().assert_ok();
	assert_directories(&fixture);
	assert_eq!(files(&fixture, fixture.parents[0].id()).subgraphs, parent_files.subgraphs);
	let mut remove = fixture.store.begin();
	remove.detach_occurrence(graph, &fixture.pending[0], true).assert_ok();
	remove.commit().assert_ok();
	let root = fixture.store.inner.current_root();
	assert!(root.membership(graph, root.resolve_atom(&records[0]).assert_some()).is_none());
	drop(root);
	assert_directories(&fixture);
	assert_eq!(files(&fixture, fixture.parents[0].id()).subgraphs, parent_files.subgraphs);
	fixture.store.checkpoint(CheckpointMode::Restart).assert_ok();
	fixture = reopen(fixture);
	let mut reuse = fixture.store.begin();
	let node = reuse.attach_node(graph, &fixture.pending[0], None, None).assert_ok();
	let edge = reuse
		.attach_edge(
			graph,
			&records[0],
			None,
			EdgeTopology::directed([endpoint], [node]),
			None,
			None,
		)
		.assert_ok();
	assert_eq!((node.slot(), node.generation()), (first_node.slot(), first_node.generation() + 1));
	assert_eq!((edge.slot(), edge.generation()), (first_edge.slot(), first_edge.generation() + 1));
	reuse.commit().assert_ok();
	assert_directories(&fixture);
	assert_eq!(files(&fixture, fixture.parents[0].id()).subgraphs, parent_files.subgraphs);
	let mut transient = fixture.store.begin();
	let temporary = transient.attach_node(graph, &fixture.pending[1], None, None).assert_ok();
	transient
		.attach_edge(
			graph,
			&records[1],
			None,
			EdgeTopology::directed([endpoint], [temporary]),
			None,
			None,
		)
		.assert_ok();
	transient.detach_occurrence(graph, &fixture.pending[1], true).assert_ok();
	transient.commit().assert_ok();
	let header = *local(&fixture.store).state.lock().graph_superblocks.get(&graph).assert_some();
	assert_eq!(
		(header.node_capacity, header.edge_capacity, header.node_count, header.edge_count),
		(3, 2, 2, 1)
	);
	assert_directories(&fixture);
	assert_eq!(files(&fixture, fixture.parents[0].id()).subgraphs, parent_files.subgraphs);
	fixture.store.checkpoint(CheckpointMode::Restart).assert_ok();
	fixture = reopen(fixture);
	assert_directories(&fixture);
	assert_eq!(files(&fixture, fixture.parents[0].id()).subgraphs, parent_files.subgraphs);
	let root = fixture.store.inner.current_root();
	assert_eq!(
		root.membership(graph, root.resolve_atom(&fixture.pending[0]).assert_some()),
		Some(node)
	);
	assert_eq!(root.membership(graph, root.resolve_atom(&records[0]).assert_some()), Some(edge));
	assert!(root.membership(graph, root.resolve_atom(&fixture.pending[1]).assert_some()).is_none());
	assert!(root.membership(graph, root.resolve_atom(&records[1]).assert_some()).is_none());
}
