//! A published zero-atom class must retain its transient allocator work.

use super::*;
use crate::{
	graph::AtomId,
	storage::local_store::tests::base_lifecycle_tests::assert_base_raw_postings,
};

/// Class replacement and specialized-index admission preserve explicit scope.
mod class_tests;
/// Transient effects share the same admission and uncertain-WAL boundary.
mod recovery_tests;

/// Both no-reopen and cold sources must retain unrelated file identities.
#[test]
fn global_node_transient_sync_retains_empty_class_authority() {
	for cold in [false, true] {
		for analyze in [false, true] {
			transient(IoMode::Sync, cold, analyze);
		}
	}
}

/// NativeAsync must remain selected throughout the warm and cold lifecycle.
#[cfg(feature = "native-async-io")]
#[test]
fn global_node_transient_native_async_retains_empty_class_authority() {
	for cold in [false, true] {
		for analyze in [false, true] {
			transient(IoMode::NativeAsync, cold, analyze);
		}
	}
}

/// The immediate warm follow-up detects a missing physical empty class array.
fn transient(io_mode: IoMode, cold: bool, analyze: bool) {
	let mut fixture = fixture_with_anchor(io_mode, false);
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
		fixture = reopen(fixture);
	}
	assert_eq!(fixture.store.inner.current_root().atoms.atoms().len(), 0);
	let local = local_store(&fixture.store);
	let files = local.state.lock().file_catalog.files.clone();
	let boundary = local.wal.snapshot().visible_lsn();
	let record = "item:transient".parse::<RecordId>().assert_ok();
	let (write, node) = transient_candidate(&fixture, &record);
	write.commit().assert_ok();
	assert_fixed_files(&fixture, &files);
	assert_hot_frames(&fixture, boundary, &[0], &[0]);
	assert_image(&fixture, &[]);
	assert_base_raw_postings(&fixture.store);
	let root = fixture.store.inner.current_root();
	assert!(root.resolve_atom(&record).is_none());
	assert_eq!(root.atoms.slot_capacity(), 1);
	assert_eq!(root.atoms.slot_generation(0), Some(1));
	assert_eq!(
		root.graph(fixture.graph.id()).assert_some().slot_generation(AtomKind::Node, 0),
		Some(1)
	);
	drop(root);
	let replacement = "item:live".parse::<RecordId>().assert_ok();
	candidate(&fixture, &replacement, 20).commit().assert_ok();
	let root = fixture.store.inner.current_root();
	let global = root.resolve_atom(&replacement).assert_some();
	let actual = root.membership(fixture.graph.id(), global).assert_some();
	assert_eq!((global.slot(), global.generation()), (0, 1));
	assert_eq!((actual.slot(), actual.generation()), (node.slot(), node.generation() + 1));
	drop(root);
	assert_image(&fixture, &[(replacement.clone(), 20)]);
	assert_base_raw_postings(&fixture.store);
	fixture.store.checkpoint(CheckpointMode::Restart).assert_ok();
	fixture = reopen(fixture);
	assert_image(&fixture, &[(replacement, 20)]);
	assert_base_raw_postings(&fixture.store);
}

/// Keeps the ordinary mutation sequence identical across failure boundaries.
fn transient_candidate(fixture: &Fixture, record: &RecordId) -> (Transaction, AtomId) {
	let mut write = candidate(fixture, record, 10);
	let node = write.membership(fixture.graph.id(), record).assert_ok().assert_some();
	write.detach_occurrence(fixture.graph.id(), record, false).assert_ok();
	write.delete_atom_cascade(record, false).assert_ok();
	(write, node)
}

/// A retained empty source must remain canonical through repeated transients.
#[test]
fn global_node_transient_reuses_warm_and_cold_empty_class_sources() {
	for io_mode in [IoMode::Sync, IoMode::NativeAsync] {
		if io_mode == IoMode::NativeAsync && !cfg!(feature = "native-async-io") {
			continue;
		}
		let mut fixture = fixture_with_anchor(io_mode, false);
		let record = "item:gone".parse::<RecordId>().assert_ok();
		transient_candidate(&fixture, &record).0.commit().assert_ok();
		let source = class_file(&fixture);
		for cold in [false, true] {
			if cold {
				fixture.store.checkpoint(CheckpointMode::Restart).assert_ok();
				fixture = reopen(fixture);
			}
			let files = local_store(&fixture.store).state.lock().file_catalog.files.clone();
			transient_candidate(&fixture, &record).0.commit().assert_ok();
			assert_fixed_files(&fixture, &files);
			assert_eq!(class_file(&fixture), source);
			assert_image(&fixture, &[]);
			assert_base_raw_postings(&fixture.store);
		}
		candidate(&fixture, &"item:live".parse::<RecordId>().assert_ok(), 20).commit().assert_ok();
		fixture = reopen(fixture);
		assert_image(&fixture, &[("item:live".parse::<RecordId>().assert_ok(), 20)]);
	}
}

/// A replayed transient must not remove another writer's live class posting.
#[test]
fn global_node_transient_late_replay_preserves_live_population() {
	for io_mode in [IoMode::Sync, IoMode::NativeAsync] {
		if io_mode == IoMode::NativeAsync && !cfg!(feature = "native-async-io") {
			continue;
		}
		let mut fixture = fixture_with_anchor(io_mode, false);
		let gone = "item:gone".parse::<RecordId>().assert_ok();
		let live = "item:live".parse::<RecordId>().assert_ok();
		let (late, _) = transient_candidate(&fixture, &gone);
		candidate(&fixture, &live, 20).commit().assert_ok();
		let source = class_file(&fixture);
		let files = local_store(&fixture.store).state.lock().file_catalog.files.clone();
		let boundary = local_store(&fixture.store).wal.snapshot().visible_lsn();
		late.commit().assert_ok();
		assert_fixed_files(&fixture, &files);
		assert_eq!(class_file(&fixture), source);
		assert_hot_frames(&fixture, boundary, &[0], &[0]);
		let root = fixture.store.inner.current_root();
		assert_eq!(root.atoms.slot_generation(1), Some(1));
		assert_eq!(
			root.graph(fixture.graph.id()).assert_some().slot_generation(AtomKind::Node, 1),
			Some(1)
		);
		drop(root);
		assert_image(&fixture, &[(live.clone(), 20)]);
		assert_base_raw_postings(&fixture.store);
		fixture.store.checkpoint(CheckpointMode::Restart).assert_ok();
		fixture = reopen(fixture);
		assert_image(&fixture, &[(live, 20)]);
		assert_base_raw_postings(&fixture.store);
	}
}

/// A canonical class path must have exactly one live physical authority.
fn class_file(fixture: &Fixture) -> FileCatalogEntry {
	let root = fixture.store.inner.current_root();
	let class = root.schema.class("item").assert_some().id().assert_some();
	let suffix = format!("/idx/cls_{}.fgidx", class.get());
	let state = local_store(&fixture.store).state.lock();
	let mut files = state.file_catalog.files.iter().filter(|entry| {
		entry.space_id == DataSpaceId::Graph(fixture.graph.id())
			&& entry.path.as_str().ends_with(&suffix)
	});
	let file = files.next().assert_some().clone();
	assert!(files.next().is_none());
	file
}
