//! Ownerless transient state is durable only through the common WAL barrier.

use super::*;
use crate::storage::wal::{CommitFailureGuard, CommitFailurePoint};

/// Known failures preserve the source; uncertain commits recover both
/// tombstones.
#[test]
fn global_node_transient_failures_recover_both_allocator_generations() {
	for io_mode in [IoMode::Sync, IoMode::NativeAsync] {
		if io_mode == IoMode::NativeAsync && !cfg!(feature = "native-async-io") {
			continue;
		}
		let record = "item:gone".parse::<RecordId>().assert_ok();
		{
			let fixture = fixture_with_anchor(io_mode, false);
			let local = local_store(&fixture.store);
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
				assert_eq!(fixture.store.inner.current_root().atoms.slot_capacity(), 0);
				assert_image(&fixture, &[]);
			}
		}
		for point in [
			CommitFailurePoint::BeforeAppend,
			CommitFailurePoint::AfterAppend,
			CommitFailurePoint::AfterFlush,
			CommitFailurePoint::AfterSync,
		] {
			let mut fixture = fixture_with_anchor(io_mode, false);
			let local = local_store(&fixture.store);
			let stamp = fixture.store.inner.current_root().commit_ts;
			let files = local.state.lock().file_catalog.files.clone();
			{
				let write = transient_candidate(&fixture, &record).0;
				let _failure = CommitFailureGuard::set(point);
				write.commit().assert_err();
			}
			assert_eq!(fixture.store.inner.current_root().commit_ts, stamp);
			assert_eq!(local.state.lock().file_catalog.files, files);
			fixture = reopen(fixture);
			let accepted = point != CommitFailurePoint::BeforeAppend;
			let root = fixture.store.inner.current_root();
			assert_eq!(root.commit_ts, stamp + u64::from(accepted));
			let generation = accepted.then_some(1);
			assert_eq!(root.atoms.slot_generation(0), generation);
			assert_eq!(
				root.graph(fixture.graph.id()).assert_some().slot_generation(AtomKind::Node, 0),
				generation
			);
			drop(root);
			assert_image(&fixture, &[]);
			assert_base_raw_postings(&fixture.store);
			candidate(&fixture, &"item:live".parse::<RecordId>().assert_ok(), 20)
				.commit()
				.assert_ok();
			fixture.store.checkpoint(CheckpointMode::Restart).assert_ok();
			fixture = reopen(fixture);
			assert_image(&fixture, &[("item:live".parse::<RecordId>().assert_ok(), 20)]);
		}
	}
}

/// Even a net-empty mutation must be admitted before physical publication.
#[test]
fn global_node_transient_small_budget_preserves_root_wal_and_files() {
	for io_mode in [IoMode::Sync, IoMode::NativeAsync] {
		if io_mode == IoMode::NativeAsync && !cfg!(feature = "native-async-io") {
			continue;
		}
		let mut fixture = fixture_with_anchor(io_mode, false);
		fixture.options.maintenance.max_memory_bytes = NonZeroUsize::new(32 * 1024).assert_some();
		fixture = reopen(fixture);
		let local = local_store(&fixture.store);
		let stamp = fixture.store.inner.current_root().commit_ts;
		let boundary = local.wal.snapshot().visible_lsn();
		let files = local.state.lock().file_catalog.files.clone();
		let directories = generation_directories(&fixture.directory.0);
		let error = transient_candidate(&fixture, &"item:gone".parse::<RecordId>().assert_ok())
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
		assert_image(&fixture, &[]);
	}
}
