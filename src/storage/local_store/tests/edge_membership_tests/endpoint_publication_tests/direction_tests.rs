//! Literal direction and role checks for local EDGE endpoint publication.

use super::*;
use crate::graph::{EdgeDirection, EndpointRole, PathDirection};

/// Binary bidirectional and n-ary undirected connectors retain physical
/// semantics.
#[test]
fn endpoint_selected_sync_direction_axes_survive_reopen() {
	direction_axes(IoMode::Sync);
}

/// NativeAsync uses the requested direction axes after replacement and reopen.
#[cfg(feature = "native-async-io")]
#[test]
fn endpoint_selected_native_async_direction_axes_survive_reopen() {
	direction_axes(IoMode::NativeAsync);
}

/// A row includes orientation and role so equality cannot hide a direction
/// error.
type Step = (AtomId, AtomId, PathDirection, EndpointRole, EdgeDirection);

/// Reads every requested physical axis without accepting materialized fallback.
fn assert_axes(
	read: &Transaction,
	snapshot: &crate::datastore::QuerySnapshot,
	fixture: &Fixture,
	endpoint: AtomId,
	expected: &[Vec<Step>; 3],
) {
	for (axis, flags) in
		[[true, false, false], [false, true, false], [false, false, true]].into_iter().enumerate()
	{
		let walk = read
			.query_physical_traversal(
				snapshot,
				&fixture.graph,
				endpoint,
				flags[0],
				flags[1],
				flags[2],
			)
			.assert_ok()
			.assert_some();
		let mut actual = walk
			.candidates
			.iter()
			.map(|step| (step.edge, step.target, step.direction, step.role, step.topology))
			.collect::<Vec<_>>();
		actual.sort();
		let mut expected = expected[axis].clone();
		expected.sort();
		assert_eq!(actual, expected);
	}
}

/// An unchanged canonical posting set still carries new Hot direction/role
/// data.
fn direction_axes(io_mode: IoMode) {
	let (mut fixture, endpoint, connector) = endpoint_fixture(io_mode);
	let files = local(&fixture.store).state.lock().file_catalog.files.clone();
	let mut write = fixture.store.begin();
	let added = write
		.attach_edge(
			fixture.graph.id(),
			&fixture.records[2],
			None,
			EdgeTopology::bidirectional([endpoint], [fixture.nodes[0]]),
			None,
			None,
		)
		.assert_ok();
	write.commit().assert_ok();
	let raw = |edge: AtomId| DiskAtomRef::from_parts(AtomKind::Edge, edge.slot()).assert_ok().raw();
	let rows = [(0, raw(endpoint), raw(connector), 0), (0, raw(endpoint), raw(added), 0)];
	assert_postings(&fixture, &rows);
	assert_data_files(&fixture, &files);
	assert_bulk_image(&fixture);
	let binding = local(&fixture.store)
		.state
		.lock()
		.bindings
		.graphs
		.iter()
		.find(|binding| binding.graph_id == fixture.graph.id())
		.assert_some()
		.edge_endpoint
		.assert_some();
	let base = (
		connector,
		fixture.nodes[1],
		PathDirection::Forward,
		EndpointRole::Target,
		EdgeDirection::Directed,
	);
	let bidirectional = (
		added,
		fixture.nodes[0],
		PathDirection::Forward,
		EndpointRole::Target,
		EdgeDirection::Bidirectional,
	);
	let previous = [vec![base, bidirectional], vec![bidirectional], vec![]];
	let mut old = fixture.store.begin();
	let snapshot = old.query_snapshot().assert_ok();
	assert_axes(&old, &snapshot, &fixture, endpoint, &previous);
	let mut write = fixture.store.begin();
	write
		.replace_occurrence_state(
			fixture.graph.id(),
			&fixture.records[2],
			OccurrenceState::edge(
				EdgeTopology::undirected([endpoint, fixture.nodes[0], fixture.nodes[1]]),
				None,
				None,
			),
		)
		.assert_ok();
	write.commit().assert_ok();
	let expected = [
		vec![base],
		vec![],
		fixture
			.nodes
			.map(|target| {
				(
					added,
					target,
					PathDirection::Undirected,
					EndpointRole::Undirected,
					EdgeDirection::Undirected,
				)
			})
			.to_vec(),
	];
	let current = local(&fixture.store)
		.state
		.lock()
		.bindings
		.graphs
		.iter()
		.find(|binding| binding.graph_id == fixture.graph.id())
		.assert_some()
		.edge_endpoint
		.assert_some();
	assert_eq!(current, binding);
	assert_postings(&fixture, &rows);
	assert_bulk_image(&fixture);
	assert_data_files(&fixture, &files);
	{
		let mut read = fixture.store.begin();
		let pin = read.query_snapshot().assert_ok();
		assert_axes(&read, &pin, &fixture, endpoint, &expected);
	}
	assert!(fixture.store.checkpoint(CheckpointMode::Passive).assert_ok());
	assert_axes(&old, &snapshot, &fixture, endpoint, &previous);
	drop(snapshot);
	drop(old);
	fixture.store.checkpoint(CheckpointMode::Restart).assert_ok();
	fixture = reopen(fixture);
	let mut read = fixture.store.begin();
	let pin = read.query_snapshot().assert_ok();
	assert_axes(&read, &pin, &fixture, endpoint, &expected);
	assert_postings(&fixture, &rows);
	assert_data_files(&fixture, &files);
}
