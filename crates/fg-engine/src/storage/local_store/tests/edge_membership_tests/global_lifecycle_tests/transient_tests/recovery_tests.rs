//! Transient tombstones share the existing known and uncertain WAL boundary.

use super::*;
use crate::storage::wal::{CommitFailureGuard, CommitFailurePoint};

/// Known failures retain both allocators; complete uncertain WAL tails recover
/// once.
#[test]
fn global_edge_transient_failures_recover_complete_allocator_authority() {
	for io_mode in [IoMode::Sync, IoMode::NativeAsync] {
		if io_mode == IoMode::NativeAsync && !cfg!(feature = "native-async-io") {
			continue;
		}
		let record = "edge:gone".parse::<RecordId>().assert_ok();
		{
			let fixture = fixture(io_mode);
			let local = support::local(&fixture.store);
			let stamp = fixture.store.inner.current_root().commit_ts;
			let boundary = local.wal.snapshot().visible_lsn();
			let files = local.state.lock().file_catalog.files.clone();
			let directories = generation_directories(&fixture.directory.0);
			for point in [
				GenerationFailurePoint::AfterBuild,
				GenerationFailurePoint::AfterFileSync,
				GenerationFailurePoint::AfterCatalogStage,
				GenerationFailurePoint::BeforeWalCommit,
			] {
				{
					let write = transient_candidate(&fixture, &record).0;
					let _failure = GenerationFailureGuard::set(point);
					write.commit().assert_err();
				}
				assert_eq!(fixture.store.inner.current_root().commit_ts, stamp);
				assert_eq!(local.wal.snapshot().visible_lsn(), boundary);
				assert_eq!(local.state.lock().file_catalog.files, files);
				assert_eq!(generation_directories(&fixture.directory.0), directories);
				assert_unallocated(&fixture);
			}
			assert_live_reuse(&fixture, 0);
		}
		for point in [
			CommitFailurePoint::BeforeAppend,
			CommitFailurePoint::AfterAppend,
			CommitFailurePoint::AfterFlush,
			CommitFailurePoint::AfterSync,
		] {
			let mut fixture = fixture(io_mode);
			let stamp = fixture.store.inner.current_root().commit_ts;
			let files = support::local(&fixture.store).state.lock().file_catalog.files.clone();
			{
				let write = transient_candidate(&fixture, &record).0;
				let _failure = CommitFailureGuard::set(point);
				write.commit().assert_err();
			}
			assert_eq!(fixture.store.inner.current_root().commit_ts, stamp);
			assert_eq!(support::local(&fixture.store).state.lock().file_catalog.files, files);
			fixture = support::reopen(fixture);
			let accepted = point != CommitFailurePoint::BeforeAppend;
			assert_eq!(fixture.store.inner.current_root().commit_ts, stamp + u64::from(accepted));
			if accepted {
				assert_tombstones(&fixture, &record, 1);
			} else {
				assert_unallocated(&fixture);
			}
			fixture.store.checkpoint(CheckpointMode::Restart).assert_ok();
			fixture = support::reopen(fixture);
			assert_live_reuse(&fixture, u32::from(accepted));
			fixture.store.checkpoint(CheckpointMode::Restart).assert_ok();
			fixture = support::reopen(fixture);
			assert_indexes(&fixture, "edge", &[("edge:live".parse::<RecordId>().assert_ok(), 20)]);
			support::assert_bulk_image(&fixture);
		}
	}
}

/// The unchanged source has no allocated EDGE slot in either namespace.
fn assert_unallocated(fixture: &support::Fixture) {
	let root = fixture.store.inner.current_root();
	assert_eq!(root.atoms.slot_generation(2), None);
	assert_eq!(
		root.graph(fixture.graph.id()).assert_some().slot_generation(AtomKind::Edge, 0),
		None
	);
	drop(root);
	assert_indexes(fixture, "edge", &[]);
	support::assert_bulk_image(fixture);
}

/// A successful subsequent insert consumes exactly the recovered next
/// generation.
fn assert_live_reuse(fixture: &support::Fixture, generation: u32) {
	let record = "edge:live".parse::<RecordId>().assert_ok();
	let (write, edge) = candidate(fixture, &record, 20);
	write.commit().assert_ok();
	let root = fixture.store.inner.current_root();
	let global = root.resolve_atom(&record).assert_some();
	assert_eq!((global.slot(), global.generation()), (2, generation));
	assert_eq!((edge.slot(), edge.generation()), (0, generation));
	assert_eq!(root.membership(fixture.graph.id(), global), Some(edge));
	drop(root);
	assert_indexes(fixture, "edge", &[(record, 20)]);
	assert_class_slots(fixture, "edge", &[0]);
	support::assert_bulk_image(fixture);
}

/// Publication admission rejects before advancing either allocator or WAL.
#[test]
fn global_edge_transient_small_budget_retains_source_and_wal() {
	for io_mode in [IoMode::Sync, IoMode::NativeAsync] {
		if io_mode == IoMode::NativeAsync && !cfg!(feature = "native-async-io") {
			continue;
		}
		let mut fixture = fixture(io_mode);
		fixture.options.maintenance.max_memory_bytes = NonZeroUsize::new(32 * 1024).assert_some();
		fixture = support::reopen(fixture);
		let local = support::local(&fixture.store);
		let stamp = fixture.store.inner.current_root().commit_ts;
		let boundary = local.wal.snapshot().visible_lsn();
		let files = local.state.lock().file_catalog.files.clone();
		let directories = generation_directories(&fixture.directory.0);
		let error = transient_candidate(&fixture, &"edge:gone".parse::<RecordId>().assert_ok())
			.0
			.commit()
			.assert_err();
		assert!(matches!(
			error.status().status(),
			crate::gql_status::Status::StorageDecodeLimitExceeded { .. }
		));
		assert_eq!(fixture.store.inner.current_root().commit_ts, stamp);
		assert_eq!(local.wal.snapshot().visible_lsn(), boundary);
		assert_eq!(local.state.lock().file_catalog.files, files);
		assert_eq!(generation_directories(&fixture.directory.0), directories);
		assert_unallocated(&fixture);
	}
}
/// An ownerless transient must validate the retained cold empty class source.
#[test]
fn global_edge_transient_corrupt_cold_class_source_publishes_nothing() {
	for io_mode in [IoMode::Sync, IoMode::NativeAsync] {
		if io_mode == IoMode::NativeAsync && !cfg!(feature = "native-async-io") {
			continue;
		}
		let mut fixture = fixture(io_mode);
		let record = "edge:gone".parse::<RecordId>().assert_ok();
		for generation in 1..=2 {
			transient_candidate(&fixture, &record).0.commit().assert_ok();
			assert_tombstones(&fixture, &record, generation);
			fixture.store.checkpoint(CheckpointMode::Restart).assert_ok();
			fixture = support::reopen(fixture);
		}
		super::super::recovery_tests::corrupt_empty_class(&fixture);
		fixture = support::reopen(fixture);
		let local = support::local(&fixture.store);
		let stamp = fixture.store.inner.current_root().commit_ts;
		let boundary = local.wal.snapshot().visible_lsn();
		let files = local.state.lock().file_catalog.files.clone();
		let directories = generation_directories(&fixture.directory.0);
		transient_candidate(&fixture, &record).0.commit().assert_err();
		let root = fixture.store.inner.current_root();
		assert_eq!(root.commit_ts, stamp);
		assert!(root.resolve_atom(&record).is_none());
		assert_eq!(root.atoms.slot_generation(2), Some(2));
		assert_eq!(
			root.graph(fixture.graph.id()).assert_some().slot_generation(AtomKind::Edge, 0),
			Some(2)
		);
		assert_eq!(local.wal.snapshot().visible_lsn(), boundary);
		assert_eq!(local.state.lock().file_catalog.files, files);
		assert_eq!(generation_directories(&fixture.directory.0), directories);
	}
}
