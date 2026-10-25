//! Plain topology replacement shares the existing incidence publication path.

use super::{support::*, *};
use crate::{datastore::OccurrenceState, storage::catalog::FileCatalogEntry};

/// Reversing a binary EDGE must preserve unrelated physical identities.
#[test]
fn edge_rewire_sync_retains_files_and_old_snapshots() {
	plain_rewire(IoMode::Sync);
}

/// NativeAsync remains selected through both rewires and cold reopen.
#[cfg(feature = "native-async-io")]
#[test]
fn edge_rewire_native_async_retains_files_and_old_snapshots() {
	plain_rewire(IoMode::NativeAsync);
}

/// Exact frames prove the binary delta; a second delta leaves a WAL-only pin.
fn plain_rewire(io_mode: IoMode) {
	let mut fixture = fixture(io_mode, 1, 1);
	let files = local(&fixture.store).state.lock().file_catalog.files.clone();
	let source = fixture.store.inner.current_root();
	let global = source.resolve_atom(&fixture.records[0]).assert_some();
	let edge = source.membership(fixture.graph.id(), global).assert_some();
	let (keys, expected) = watch_data_pages(local(&fixture.store), fixture.graph.id(), None);
	let boundary = local(&fixture.store).wal.snapshot().visible_lsn();
	let reversed = EdgeTopology::directed([fixture.nodes[1]], [fixture.nodes[0]]);
	write_topology(&fixture, reversed.clone()).commit().assert_ok();
	assert_retained(&fixture, &files, false);
	assert_topology(&fixture, global, edge, &reversed);
	let changed = {
		let backend = local(&fixture.store);
		let frames = backend.wal.visible_frames(&keys, backend.wal.snapshot());
		frames
			.frames()
			.iter()
			.flatten()
			.filter(|frame| frame.frame_lsn() > boundary)
			.map(|frame| frame.key())
			.collect::<BTreeSet<_>>()
	};
	assert_eq!(changed, expected);
	assert_bulk_image(&fixture);
	let home = local(&fixture.store).state.lock().database_superblock.checkpoint_lsn;
	let mut old = fixture.store.begin();
	let pin = old.query_snapshot().assert_ok();
	let pinned_boundary = local(&fixture.store).wal.snapshot().visible_lsn();
	assert!(pinned_boundary > home);
	let undirected = EdgeTopology::undirected(fixture.nodes);
	write_topology(&fixture, undirected.clone()).commit().assert_ok();
	assert_retained(&fixture, &files, false);
	assert_topology(&fixture, global, edge, &undirected);
	assert_bulk_image(&fixture);
	assert!(fixture.store.checkpoint(CheckpointMode::Passive).assert_ok());
	let checkpoint = local(&fixture.store).state.lock().database_superblock.checkpoint_lsn;
	assert!(checkpoint > home && checkpoint <= pinned_boundary);
	// The first physical read of this old WAL pin happens after backfill.
	let physical = old
		.query_physical_traversal(&pin, &fixture.graph, fixture.nodes[1], true, false, false)
		.assert_ok()
		.assert_some();
	assert_eq!(
		physical
			.candidates
			.iter()
			.map(|candidate| (candidate.edge, candidate.target))
			.collect::<Vec<_>>(),
		vec![(edge, fixture.nodes[0])]
	);
	drop(physical);
	drop(pin);
	drop(old);
	drop(source);
	fixture.store.checkpoint(CheckpointMode::Restart).assert_ok();
	fixture = reopen(fixture);
	assert_retained(&fixture, &files, false);
	assert_topology(&fixture, global, edge, &undirected);
	assert_bulk_image(&fixture);
}

/// Uses the production state replacement path that records exact incidence.
pub(super) fn write_topology(fixture: &Fixture, topology: EdgeTopology) -> Transaction {
	let mut write = fixture.store.begin();
	write
		.replace_occurrence_state(
			fixture.graph.id(),
			&fixture.records[0],
			OccurrenceState::edge(topology, None, None),
		)
		.assert_ok();
	write
}

/// Topology changes retain both identities and the requested final state.
pub(super) fn assert_topology(
	fixture: &Fixture,
	global: crate::datastore::GlobalAtomId,
	edge: AtomId,
	topology: &EdgeTopology,
) {
	let root = fixture.store.inner.current_root();
	assert_eq!(root.membership(fixture.graph.id(), global), Some(edge));
	let occurrence = root.occurrence(fixture.graph.id(), edge).assert_some();
	assert_eq!(occurrence.global_id(), global);
	assert_eq!(occurrence.state(), &OccurrenceState::edge(topology.clone(), None, None));
}

/// Exact catalog entries expose a full rebuild independently of bulk equality.
pub(super) fn assert_retained(fixture: &Fixture, files: &[FileCatalogEntry], mixed: bool) {
	let backend = local(&fixture.store);
	let state = backend.state.lock();
	for entry in files {
		let selected = entry.space_id == DataSpaceId::Graph(fixture.graph.id())
			&& (matches!(
				entry.file_id.kind(),
				Some(DataFileKind::Index | DataFileKind::Statistics)
			) || (mixed
				&& (entry.file_id.kind() == Some(DataFileKind::LabelStatistics)
					|| entry.path.as_str().ends_with("/labels.fgb"))));
		if !selected {
			assert!(
				state.file_catalog.files.contains(entry),
				"EDGE_REWIRE_REBUILT_DATA_FILES: {}",
				entry.path.as_str()
			);
		}
	}
}

/// Role changes preserve both old chains and metadata in shared Hot pages.
#[test]
fn edge_rewire_sync_roles_and_mixed_metadata_match_bulk() {
	roles_and_metadata(IoMode::Sync);
}

/// NativeAsync covers the same multiplicity and multi-page role transitions.
#[cfg(feature = "native-async-io")]
#[test]
fn edge_rewire_native_async_roles_and_mixed_metadata_match_bulk() {
	roles_and_metadata(IoMode::NativeAsync);
}

/// Two real overlay owners coexist with a shifted, untouched EDGE label.
fn roles_and_metadata(io_mode: IoMode) {
	let mut fixture = fixture(io_mode, 2, 2);
	let node = "node:a".parse::<crate::values::RecordId>().assert_ok();
	let mut seed = fixture.store.begin();
	let key = seed.intern_property("value").assert_ok();
	seed.set_occurrence_label(fixture.graph.id(), &node, Some("N".into())).assert_ok();
	seed.set_occurrence_label(fixture.graph.id(), &fixture.records[0], Some("A".into()))
		.assert_ok();
	seed.set_occurrence_label(fixture.graph.id(), &fixture.records[1], Some("Z".into()))
		.assert_ok();
	seed.set_overlay(fixture.graph.id(), &node, key, 101_i64.into()).assert_ok();
	seed.set_overlay(fixture.graph.id(), &fixture.records[0], key, 100_i64.into()).assert_ok();
	seed.commit().assert_ok();
	fixture.store.checkpoint(CheckpointMode::Restart).assert_ok();
	fixture = reopen(fixture);
	let files = local(&fixture.store).state.lock().file_catalog.files.clone();
	let root = fixture.store.inner.current_root();
	let global = root.resolve_atom(&fixture.records[0]).assert_some();
	let edge = root.membership(fixture.graph.id(), global).assert_some();
	let other_global = root.resolve_atom(&fixture.records[1]).assert_some();
	let other = root.membership(fixture.graph.id(), other_global).assert_some();
	let retained_edge = root.occurrence(fixture.graph.id(), other).assert_some().clone();
	drop(root);
	let owners = [fixture.nodes[0], edge];
	assert_overlay_value(&fixture, owners[0], key, 101);
	assert_overlay_value(&fixture, owners[1], key, 100);
	assert_bulk_image_with_overlays(&fixture, &owners);
	let [a, b] = fixture.nodes;
	for (index, topology) in [
		EdgeTopology::directed([a], [a]),
		// Format v1 admits binary bidirectional edges; repeated roles use directed/undirected
		// cases.
		EdgeTopology::bidirectional([a], [b]),
		EdgeTopology::undirected([a, a, b]),
		EdgeTopology::directed(std::iter::repeat_n(a, 2_050), [b, b, a]),
		EdgeTopology::directed([a], std::iter::repeat_n(b, 514)),
		EdgeTopology::directed([b], [a]),
	]
	.into_iter()
	.enumerate()
	{
		let value = i64::try_from(index).assert_ok();
		let state = OccurrenceState::edge(topology, Some((10 + value).into()), None);
		let label = if index % 2 == 0 {
			"A"
		} else {
			"ZZ"
		};
		let mut write = fixture.store.begin();
		write
			.replace_occurrence_state(fixture.graph.id(), &fixture.records[0], state.clone())
			.assert_ok();
		write
			.set_occurrence_label(fixture.graph.id(), &fixture.records[0], Some(label.into()))
			.assert_ok();
		write
			.set_overlay(fixture.graph.id(), &fixture.records[0], key, (200 + value).into())
			.assert_ok();
		write.set_overlay(fixture.graph.id(), &node, key, (300 + value).into()).assert_ok();
		write.commit().assert_ok();
		let root = fixture.store.inner.current_root();
		assert_eq!(root.membership(fixture.graph.id(), global), Some(edge));
		let occurrence = root.occurrence(fixture.graph.id(), edge).assert_some();
		assert_eq!(occurrence.state(), &state);
		assert_eq!(occurrence.label().map(|label| label.as_str()), Some(label));
		assert_eq!(root.occurrence(fixture.graph.id(), other), Some(&retained_edge));
		drop(root);
		assert_retained(&fixture, &files, true);
		assert_overlay_value(&fixture, owners[0], key, 300 + value);
		assert_overlay_value(&fixture, owners[1], key, 200 + value);
		assert_bulk_image_with_overlays(&fixture, &owners);
		super::super::graph_overlay_tests::label_tests::assert_label_arrays(
			&fixture.store,
			fixture.graph.id(),
		);
	}
	fixture.store.checkpoint(CheckpointMode::Restart).assert_ok();
	fixture = reopen(fixture);
	assert_retained(&fixture, &files, true);
	assert_overlay_value(&fixture, owners[0], key, 305);
	assert_overlay_value(&fixture, owners[1], key, 205);
	let root = fixture.store.inner.current_root();
	assert_eq!(root.resolve_atom(&fixture.records[0]), Some(global));
	assert_eq!(root.membership(fixture.graph.id(), global), Some(edge));
	let occurrence = root.occurrence(fixture.graph.id(), edge).assert_some();
	assert_eq!(occurrence.global_id(), global);
	assert_eq!(
		occurrence.state(),
		&OccurrenceState::edge(EdgeTopology::directed([b], [a]), Some(15_i64.into()), None,)
	);
	assert_eq!(occurrence.label().map(|label| label.as_str()), Some("ZZ"));
	assert_eq!(root.occurrence(fixture.graph.id(), other), Some(&retained_edge));
	drop(root);
	assert_bulk_image_with_overlays(&fixture, &owners);
	super::super::graph_overlay_tests::label_tests::assert_label_arrays(
		&fixture.store,
		fixture.graph.id(),
	);
}

/// Literal expected values check complete physical payloads before
/// normalization.
fn assert_overlay_value(
	fixture: &Fixture,
	owner: AtomId,
	key: crate::values::PropertyKeyId,
	value: i64,
) {
	let mut read = fixture.store.begin();
	let snapshot = read.query_snapshot().assert_ok();
	let physical =
		read.query_physical_fields(&snapshot, &fixture.graph, owner).assert_ok().assert_some();
	assert_eq!(physical.properties.len(), 1);
	assert_eq!(physical.properties.get(key), Some(&crate::values::Value::from(value)));
}
