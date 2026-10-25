//! Mixed membership writes retain the same graph and Base data identities.

/// Reused generations, page boundaries and independent replay.
mod boundary_tests;
/// Shared admission, rollback and uncertain WAL recovery.
mod recovery_tests;

use super::{support::*, *};
use crate::storage::catalog::FileCatalogEntry;

/// A new endpoint and its EDGE can share a NODE Hot source page.
#[test]
fn mixed_membership_sync_attach_retains_data_files() {
	attach(IoMode::Sync);
}

/// NativeAsync must select the same sparse plan through cold reopen.
#[cfg(feature = "native-async-io")]
#[test]
fn mixed_membership_native_async_attach_retains_data_files() {
	attach(IoMode::NativeAsync);
}

/// Existing Base owners isolate graph incidence from global atom allocation.
fn attach(io_mode: IoMode) {
	let (mut fixture, records) = mixed_fixture(io_mode, 0);
	let record = &records[0];
	let files = local(&fixture.store).state.lock().file_catalog.files.clone();
	let (watched, expected_pages) =
		watch_data_pages(local(&fixture.store), fixture.graph.id(), None);
	let boundary = local(&fixture.store).wal.snapshot().visible_lsn();
	let mut old = fixture.store.begin();
	let snapshot = old.query_snapshot().assert_ok();
	let mut write = fixture.store.begin();
	let node = write.attach_node(fixture.graph.id(), record, None, None).assert_ok();
	write
		.attach_edge(
			fixture.graph.id(),
			&fixture.records[0],
			None,
			EdgeTopology::directed([node, fixture.nodes[0]], [fixture.nodes[1]]),
			None,
			None,
		)
		.assert_ok();
	write.commit().assert_ok();
	let changed_pages = {
		let store = local(&fixture.store);
		let frames = store.wal.visible_frames(&watched, store.wal.snapshot());
		frames
			.frames()
			.iter()
			.flatten()
			.filter(|frame| frame.frame_lsn() > boundary)
			.map(|frame| frame.key())
			.collect::<BTreeSet<_>>()
	};
	assert_eq!(changed_pages, expected_pages);
	assert_bulk_image(&fixture);
	assert_fixed_data(&fixture, &files);
	let physical = old
		.query_physical_traversal(&snapshot, &fixture.graph, fixture.nodes[0], true, false, false)
		.assert_ok()
		.assert_some();
	assert!(physical.candidates.is_empty());
	drop(snapshot);
	drop(old);
	fixture.store.checkpoint(CheckpointMode::Restart).assert_ok();
	fixture = reopen(fixture);
	assert_bulk_image(&fixture);
}

/// Setup owns all Base atoms before the graph-only candidate begins.
fn mixed_fixture(io_mode: IoMode, existing: usize) -> (Fixture, Vec<RecordId>) {
	let mut fixture = fixture(io_mode, 4, 0);
	let records = (0..existing + 3)
		.map(|index| format!("node:new{index}").parse::<RecordId>().assert_ok())
		.collect::<Vec<_>>();
	let mut seed = fixture.store.begin();
	for (index, record) in records.iter().enumerate() {
		seed.create_atom(record.clone(), PropertyMap::new()).assert_ok();
		if index < existing {
			seed.attach_node(fixture.graph.id(), record, None, None).assert_ok();
		}
	}
	seed.commit().assert_ok();
	fixture.store.checkpoint(CheckpointMode::Restart).assert_ok();
	fixture = reopen(fixture);
	(fixture, records)
}

/// Independent candidates allocate different global owners in the same graph.
fn mixed_candidate(fixture: &Fixture, record: &RecordId, edge: usize) -> Transaction {
	let mut write = fixture.store.begin();
	let node = write.attach_node(fixture.graph.id(), record, None, None).assert_ok();
	write
		.attach_edge(
			fixture.graph.id(),
			&fixture.records[edge],
			None,
			EdgeTopology::directed([node], [fixture.nodes[0]]),
			None,
			None,
		)
		.assert_ok();
	write
}

/// Selected metadata may change while all data-file identities stay fixed.
fn assert_fixed_data(fixture: &Fixture, files: &[FileCatalogEntry]) {
	for entry in files {
		let selected = (entry.space_id == DataSpaceId::Graph(fixture.graph.id())
			&& matches!(
				entry.file_id.kind(),
				Some(DataFileKind::Index | DataFileKind::Statistics)
			))
			|| (entry.space_id == DataSpaceId::Database
				&& entry.file_id.kind() == Some(DataFileKind::Index)
				&& entry.path.as_str().ends_with("/memberships.fgidx"));
		if !selected {
			assert!(
				local(&fixture.store).state.lock().file_catalog.files.contains(entry),
				"mixed membership rebuilt unrelated file: {}",
				entry.path.as_str()
			);
		}
	}
}

/// Requires pinned physical traversal rather than a materialized fallback.
fn pairs(
	read: &Transaction,
	snapshot: &QuerySnapshot,
	fixture: &Fixture,
	node: AtomId,
) -> Vec<(AtomId, AtomId)> {
	read.query_physical_traversal(snapshot, &fixture.graph, node, true, false, false)
		.assert_ok()
		.assert_some()
		.candidates
		.iter()
		.map(|candidate| (candidate.edge, candidate.target))
		.collect()
}
