//! Same-child META rewires keep both role encodings and metadata exact.

use super::{
	transition_rewire_tests::{assert_state, portal, rewrite},
	*,
};
use crate::{datastore::OccurrenceState, graph::EdgeEndpoint};

/// Sync exercises local and qualified representation churn with metadata.
#[test]
fn transition_rewire_sync_roles_and_metadata_match_bulk() {
	roles_and_metadata(IoMode::Sync);
}

/// NativeAsync uses the same real child and cold physical oracle.
#[cfg(feature = "native-async-io")]
#[test]
fn transition_rewire_native_async_roles_and_metadata_match_bulk() {
	roles_and_metadata(IoMode::NativeAsync);
}

/// Alternating shapes cover shrink, append, empty targets and both side tags.
fn roles_and_metadata(mode: IoMode) {
	let (mut fixture, key) = family_fixture(mode, GraphKind::Metagraph);
	let witness = metagraph::Witness::capture(&fixture);
	let (global, edge, child) = portal(&fixture);
	let files = local(&fixture.store).state.lock().file_catalog.files.clone();
	let [a, b] = fixture.nodes;
	let qualified = qualified_guard_tests::short_topology(&fixture);
	let wide = qualified_guard_tests::wide_topology(&fixture);
	let topologies = [
		EdgeTopology::directed([b], [a]),
		EdgeTopology::bidirectional([a], [b]),
		EdgeTopology::undirected([a, a, b]),
		EdgeTopology::directed(std::iter::repeat_n(a, 2050), [b, b, a]),
		EdgeTopology::directed([a], std::iter::repeat_n(b, 514)),
		qualified.clone(),
		EdgeTopology::directed([qualified.source()[0].clone(), qualified.source()[0].clone()], [
			EdgeEndpoint::Local(b),
		]),
		EdgeTopology::directed(std::iter::repeat_n(qualified.source()[0].clone(), 3), [
			EdgeEndpoint::Local(b),
		]),
		EdgeTopology::directed([EdgeEndpoint::Local(a), qualified.source()[0].clone()], [
			EdgeEndpoint::Local(b),
		]),
		wide.clone(),
		EdgeTopology::directed(qualified.target().iter().cloned(), wide.source().iter().cloned()),
		EdgeTopology::undirected(wide.source().iter().cloned()),
		qualified,
		wide,
		EdgeTopology::directed([b], [a]),
	];
	for (index, topology) in topologies.into_iter().enumerate() {
		let value = i64::try_from(index).assert_ok();
		let expected = OccurrenceState::edge(topology, Some((value + 10).into()), Some(child));
		let label = if index % 2 == 0 {
			"portal"
		} else {
			"portal-long"
		};
		let mut write = rewrite(&fixture, expected.clone());
		write
			.set_occurrence_label(
				fixture.graph.id(),
				&"edge:portal".parse().assert_ok(),
				Some(label.into()),
			)
			.assert_ok();
		write
			.set_overlay(
				fixture.graph.id(),
				&"edge:portal".parse().assert_ok(),
				key,
				(200 + value).into(),
			)
			.assert_ok();
		write
			.set_overlay(
				fixture.graph.id(),
				&"node:a".parse().assert_ok(),
				key,
				(300 + value).into(),
			)
			.assert_ok();
		write.commit().assert_ok();
		assert_state(&fixture, global, edge, &expected);
		assert_eq!(
			fixture
				.store
				.inner
				.current_root()
				.occurrence(fixture.graph.id(), edge)
				.assert_some()
				.label()
				.map(|label| label.as_str()),
			Some(label)
		);
		assert_retained(&fixture, &files, true);
		witness.assert_structure(&fixture);
		assert_payloads(&fixture, edge, key, value);
		assert_bulk_image_with_overlays(&fixture, &[a, edge]);
	}
	fixture.store.checkpoint(CheckpointMode::Restart).assert_ok();
	fixture = reopen(fixture);
	assert_eq!(fixture.store.local_storage_status().assert_some().options().io_mode, mode);
	assert_state(
		&fixture,
		global,
		edge,
		&OccurrenceState::edge(EdgeTopology::directed([b], [a]), Some(24_i64.into()), Some(child)),
	);
	assert_payloads(&fixture, edge, key, 14);
	assert_retained(&fixture, &files, true);
	witness.assert_structure(&fixture);
	assert_bulk_image_with_overlays(&fixture, &[a, edge]);
	let reset = OccurrenceState::edge(EdgeTopology::undirected([a, b]), None, Some(child));
	let mut write = rewrite(&fixture, reset.clone());
	write
		.set_occurrence_label(fixture.graph.id(), &"edge:portal".parse().assert_ok(), None)
		.assert_ok();
	write.reset_property(fixture.graph.id(), &"edge:portal".parse().assert_ok(), key).assert_ok();
	write.reset_property(fixture.graph.id(), &"node:a".parse().assert_ok(), key).assert_ok();
	write.commit().assert_ok();
	fixture.store.checkpoint(CheckpointMode::Restart).assert_ok();
	fixture = reopen(fixture);
	assert_state(&fixture, global, edge, &reset);
	let mut read = fixture.store.begin();
	let snapshot = read.query_snapshot().assert_ok();
	let fields =
		read.query_physical_fields(&snapshot, &fixture.graph, edge).assert_ok().assert_some();
	assert!(fields.properties.is_empty());
	let fields = read.query_physical_fields(&snapshot, &fixture.graph, a).assert_ok().assert_some();
	assert_eq!(fields.properties.len(), 1);
	assert_eq!(fields.properties.get(key), Some(&crate::values::Value::from(10_i64)));
	assert!(
		fixture
			.store
			.inner
			.current_root()
			.occurrence(fixture.graph.id(), edge)
			.assert_some()
			.label()
			.is_none()
	);
	assert_retained(&fixture, &files, true);
	witness.assert_structure(&fixture);
	assert_bulk_image(&fixture);
}

/// Literal physical payloads are checked before the bulk oracle normalizes
/// offsets.
fn assert_payloads(fixture: &Fixture, edge: AtomId, key: PropertyKeyId, value: i64) {
	let mut read = fixture.store.begin();
	let snapshot = read.query_snapshot().assert_ok();
	for (owner, expected) in [(fixture.nodes[0], value + 300), (edge, value + 200)] {
		let fields =
			read.query_physical_fields(&snapshot, &fixture.graph, owner).assert_ok().assert_some();
		assert_eq!(fields.properties.len(), 1);
		assert_eq!(fields.properties.get(key), Some(&crate::values::Value::from(expected)));
	}
}
