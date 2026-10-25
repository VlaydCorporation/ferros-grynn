//! Transient Base and graph EDGE allocators retain their consumed generations.

use super::*;

/// Replay and same-slot reuse keep the two local allocator kinds distinct.
mod lifecycle_tests;
/// Transient endpoints leave no adjacency and retain both tombstones.
mod mixed_tests;
/// Known/uncertain failures, resource limits and source corruption are atomic.
mod recovery_tests;

/// A zero-EDGE class remains usable immediately and after cold recovery.
#[test]
fn global_edge_transient_sync_retains_empty_class_authority() {
	for cold in [false, true] {
		for analyze in [false, true] {
			transient(IoMode::Sync, cold, analyze);
		}
	}
}

/// Every open keeps the explicitly selected NativeAsync backend.
#[cfg(feature = "native-async-io")]
#[test]
fn global_edge_transient_native_async_retains_empty_class_authority() {
	for cold in [false, true] {
		for analyze in [false, true] {
			transient(IoMode::NativeAsync, cold, analyze);
		}
	}
}

/// The second transient retains the empty file before an immediate warm reuse.
fn transient(io_mode: IoMode, cold: bool, analyze: bool) {
	let mut fixture = warm_fixture(io_mode);
	if analyze {
		use crate::datastore::{MaintenanceAnalyzeMode, MaintenanceTarget};
		fixture
			.store
			.run_foreground_maintenance_with_analysis(
				MaintenanceJobKind::Analyze,
				MaintenanceTarget::Graph(fixture.graph.id()),
				fixture.options.maintenance,
				Some(MaintenanceAnalyzeMode::Full),
			)
			.assert_ok();
	}
	if cold {
		fixture = support::reopen(fixture);
	}
	let gone = "edge:gone".parse::<RecordId>().assert_ok();
	for generation in 1..=2 {
		if generation == 2 && cold {
			fixture.store.checkpoint(CheckpointMode::Restart).assert_ok();
			fixture = support::reopen(fixture);
		}
		let previous = (generation == 2).then(|| class_file(&fixture));
		let files = support::local(&fixture.store).state.lock().file_catalog.files.clone();
		let boundary = support::local(&fixture.store).wal.snapshot().visible_lsn();
		transient_candidate(&fixture, &gone).0.commit().assert_ok();
		assert_fixed_files(&fixture.store, &files, &[fixture.graph.id()]);
		oracles::assert_hot_frames_including_nodes(&fixture, boundary, &[0], &[], &[0]);
		assert_tombstones(&fixture, &gone, generation);
		if let Some(previous) = previous {
			assert_eq!(class_file(&fixture), previous);
		}
	}
	let live = "edge:live".parse::<RecordId>().assert_ok();
	let (write, edge) = candidate(&fixture, &live, 20);
	write.commit().assert_ok();
	let root = fixture.store.inner.current_root();
	let global = root.resolve_atom(&live).assert_some();
	assert_eq!((global.slot(), global.generation()), (2, 2));
	assert_eq!((edge.slot(), edge.generation()), (0, 2));
	assert_eq!(root.membership(fixture.graph.id(), global), Some(edge));
	drop(root);
	assert_indexes(&fixture, "edge", &[(live.clone(), 20)]);
	assert_class_slots(&fixture, "edge", &[0]);
	support::assert_bulk_image(&fixture);
	fixture.store.checkpoint(CheckpointMode::Restart).assert_ok();
	fixture = support::reopen(fixture);
	assert_indexes(&fixture, "edge", &[(live, 20)]);
	assert_class_slots(&fixture, "edge", &[0]);
	support::assert_bulk_image(&fixture);
}

/// Both owners disappear before commit, while both journal targets survive.
fn transient_candidate(fixture: &support::Fixture, record: &RecordId) -> (Transaction, AtomId) {
	let (mut write, edge) = candidate(fixture, record, 10);
	write.detach_occurrence(fixture.graph.id(), record, false).assert_ok();
	write.delete_atom_cascade(record, false).assert_ok();
	(write, edge)
}

/// Logical and raw physical oracles agree without a remaining Base owner.
fn assert_tombstones(fixture: &support::Fixture, record: &RecordId, generation: u32) {
	let root = fixture.store.inner.current_root();
	assert!(root.resolve_atom(record).is_none());
	assert_eq!(root.atoms.slot_generation(2), Some(generation));
	assert_eq!(
		root.graph(fixture.graph.id()).assert_some().slot_generation(AtomKind::Edge, 0),
		Some(generation)
	);
	drop(root);
	assert_indexes(fixture, "edge", &[]);
	assert_class_slots(fixture, "edge", &[]);
	support::assert_bulk_image(fixture);
}

/// A canonical empty class array keeps exactly one physical authority.
fn class_file(fixture: &support::Fixture) -> FileCatalogEntry {
	let root = fixture.store.inner.current_root();
	let class = root.schema.class("edge").assert_some().id().assert_some();
	let suffix = format!("/idx/cls_{}.fgidx", class.get());
	let state = support::local(&fixture.store).state.lock();
	let mut files = state.file_catalog.files.iter().filter(|entry| {
		entry.space_id == DataSpaceId::Graph(fixture.graph.id())
			&& entry.path.as_str().ends_with(&suffix)
	});
	let file = files.next().assert_some().clone();
	assert!(files.next().is_none());
	file
}
