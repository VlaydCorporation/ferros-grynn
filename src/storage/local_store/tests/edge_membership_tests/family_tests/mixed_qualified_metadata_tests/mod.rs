//! Stable qualified metadata and independent membership share one point commit.

use super::*;
use crate::datastore::OccurrenceState;

/// Overflow, endpoint kinds and META metadata retain roles and old physical
/// pins.
mod matrix;

/// Sync mixed metadata must retain the same accepted physical generation.
#[test]
fn mixed_qualified_metadata_sync_retains_data_files() {
	mixed_metadata(IoMode::Sync);
}

/// NativeAsync proves metadata-only control before the mixed retention
/// regression.
#[cfg(feature = "native-async-io")]
#[test]
fn mixed_qualified_metadata_native_async_retains_data_files() {
	mixed_metadata(IoMode::NativeAsync);
}

/// A successful metadata-only control isolates the additional membership
/// trigger.
fn mixed_metadata(mode: IoMode) {
	let (mut fixture, key) = family_fixture(mode, GraphKind::Metagraph);
	let topology = qualified_guard_tests::short_topology(&fixture);
	let mut setup = fixture.store.begin();
	setup
		.attach_edge(fixture.graph.id(), &fixture.records[2], None, topology.clone(), None, None)
		.assert_ok();
	setup.commit().assert_ok();
	fixture.store.checkpoint(CheckpointMode::Restart).assert_ok();
	fixture = reopen(fixture);
	let witness = metagraph::Witness::capture(&fixture);
	let root = fixture.store.inner.current_root();
	let global = root.resolve_atom(&fixture.records[2]).assert_some();
	let edge = root.membership(fixture.graph.id(), global).assert_some();
	let independent_global = root.resolve_atom(&fixture.records[3]).assert_some();
	drop(root);
	let files = local(&fixture.store).state.lock().file_catalog.files.clone();
	let control = OccurrenceState::edge(topology.clone(), Some(3_i64.into()), None);
	let mut write = fixture.store.begin();
	write
		.replace_occurrence_state(fixture.graph.id(), &fixture.records[2], control.clone())
		.assert_ok();
	write
		.set_occurrence_label(fixture.graph.id(), &fixture.records[2], Some("qualified-old".into()))
		.assert_ok();
	write.set_overlay(fixture.graph.id(), &fixture.records[2], key, 111_i64.into()).assert_ok();
	write.commit().assert_ok();
	assert_metadata(&fixture, global, edge, key, &control, "qualified-old", 111);
	assert_retained(&fixture, &files, true);
	witness.assert_unchanged(&fixture);
	assert_bulk_image_with_overlays(&fixture, &[edge]);
	eprintln!("MQ_CONTROL_PASS {mode:?}");
	let files = local(&fixture.store).state.lock().file_catalog.files.clone();
	let expected = OccurrenceState::edge(topology, Some(7_i64.into()), None);
	let mut write = fixture.store.begin();
	write
		.replace_occurrence_state(fixture.graph.id(), &fixture.records[2], expected.clone())
		.assert_ok();
	write
		.set_occurrence_label(fixture.graph.id(), &fixture.records[2], Some("qualified-new".into()))
		.assert_ok();
	write.set_overlay(fixture.graph.id(), &fixture.records[2], key, 777_i64.into()).assert_ok();
	let independent = write
		.attach_edge(
			fixture.graph.id(),
			&fixture.records[3],
			None,
			EdgeTopology::directed([fixture.nodes[1]], [fixture.nodes[0]]),
			None,
			None,
		)
		.assert_ok();
	write.commit().assert_ok();
	assert_metadata(&fixture, global, edge, key, &expected, "qualified-new", 777);
	assert_independent(&fixture, independent_global, independent);
	eprintln!("MQ_MIXED_LOGICAL_PASS {mode:?}");
	// Both logical owners are checked before the physical retention assertion.
	assert_retained(&fixture, &files, true);
	witness.assert_unchanged(&fixture);
	assert_bulk_image_with_overlays(&fixture, &[edge]);
	fixture.store.checkpoint(CheckpointMode::Restart).assert_ok();
	fixture = reopen(fixture);
	assert_eq!(fixture.store.local_storage_status().assert_some().options().io_mode, mode);
	assert_metadata(&fixture, global, edge, key, &expected, "qualified-new", 777);
	assert_independent(&fixture, independent_global, independent);
	assert_retained(&fixture, &files, true);
	witness.assert_unchanged(&fixture);
	assert_bulk_image_with_overlays(&fixture, &[edge]);
}

/// Literal effective values are read before normalizing append-only overlay
/// offsets.
fn assert_metadata(
	fixture: &Fixture,
	global: crate::datastore::GlobalAtomId,
	edge: AtomId,
	key: PropertyKeyId,
	expected: &OccurrenceState,
	label: &str,
	value: i64,
) {
	let root = fixture.store.inner.current_root();
	assert_eq!(root.resolve_atom(&fixture.records[2]), Some(global));
	assert_eq!(root.membership(fixture.graph.id(), global), Some(edge));
	let occurrence = root.occurrence(fixture.graph.id(), edge).assert_some();
	assert_eq!(occurrence.state(), expected);
	assert_eq!(occurrence.label().map(|label| label.as_str()), Some(label));
	let mut read = fixture.store.begin();
	let snapshot = read.query_snapshot().assert_ok();
	let fields =
		read.query_physical_fields(&snapshot, &fixture.graph, edge).assert_ok().assert_some();
	assert_eq!(fields.properties.len(), 1);
	assert_eq!(fields.properties.get(key), Some(&crate::values::Value::from(value)));
}

/// Recovered-root bulk equality cannot substitute for this independent
/// membership oracle.
fn assert_independent(fixture: &Fixture, global: crate::datastore::GlobalAtomId, local: AtomId) {
	let root = fixture.store.inner.current_root();
	assert_eq!(root.resolve_atom(&fixture.records[3]), Some(global));
	assert_eq!(root.membership(fixture.graph.id(), global), Some(local));
	let occurrence = root.occurrence(fixture.graph.id(), local).assert_some();
	assert_eq!(occurrence.global_id(), global);
	assert_eq!(
		occurrence.state(),
		&OccurrenceState::edge(
			EdgeTopology::directed([fixture.nodes[1]], [fixture.nodes[0]]),
			None,
			None
		)
	);
}
