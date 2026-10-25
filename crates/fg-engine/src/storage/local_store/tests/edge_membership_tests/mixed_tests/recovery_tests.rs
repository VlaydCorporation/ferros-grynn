//! Mixed pages share the existing known-failure and uncertain-WAL boundary.

use super::*;
use crate::storage::wal::{CommitFailureGuard, CommitFailurePoint};

/// Both allocator files and adjacency recover as one accepted image.
#[test]
fn mixed_membership_sync_failures_recover_complete_images() {
	failures(IoMode::Sync);
}

/// NativeAsync uses the same atomic completion/recovery contract.
#[cfg(feature = "native-async-io")]
#[test]
fn mixed_membership_native_async_failures_recover_complete_images() {
	failures(IoMode::NativeAsync);
}

/// The combined plan cannot publish a partial NODE or EDGE after admission.
#[test]
fn mixed_membership_small_budget_preserves_root_wal_and_files() {
	let (mut fixture, records) = mixed_fixture(IoMode::Sync, 0);
	fixture.options.maintenance.max_memory_bytes = NonZeroUsize::new(32 * 1024).assert_some();
	fixture = reopen(fixture);
	let store = local(&fixture.store);
	let stamp = fixture.store.inner.current_root().commit_ts;
	let boundary = store.wal.snapshot().visible_lsn();
	let files = store.state.lock().file_catalog.files.clone();
	let directories = generation_directories(&fixture.directory.0);
	let error = mixed_candidate(&fixture, &records[0], 0).commit().assert_err();
	assert!(matches!(
		error.status().status(),
		crate::gql_status::Status::StorageDecodeLimitExceeded { .. }
	));
	assert_eq!(fixture.store.inner.current_root().commit_ts, stamp);
	assert_eq!(store.wal.snapshot().visible_lsn(), boundary);
	assert_eq!(store.state.lock().file_catalog.files, files);
	assert_eq!(generation_directories(&fixture.directory.0), directories);
	assert_bulk_image(&fixture);
}

/// Pre-WAL rollback and uncertain post-append recovery have distinct oracles.
fn failures(io_mode: IoMode) {
	{
		let (fixture, records) = mixed_fixture(io_mode, 0);
		let store = local(&fixture.store);
		let stamp = fixture.store.inner.current_root().commit_ts;
		let boundary = store.wal.snapshot().visible_lsn();
		let files = store.state.lock().file_catalog.files.clone();
		let directories = generation_directories(&fixture.directory.0);
		for point in [
			GenerationFailurePoint::AfterBuild,
			GenerationFailurePoint::AfterFileSync,
			GenerationFailurePoint::AfterCatalogStage,
			GenerationFailurePoint::BeforeWalCommit,
		] {
			{
				let _failure = GenerationFailureGuard::set(point);
				mixed_candidate(&fixture, &records[0], 0).commit().assert_err();
			}
			assert_eq!(fixture.store.inner.current_root().commit_ts, stamp);
			assert_eq!(store.wal.snapshot().visible_lsn(), boundary);
			assert_eq!(store.state.lock().file_catalog.files, files);
			assert_eq!(generation_directories(&fixture.directory.0), directories);
			assert_bulk_image(&fixture);
		}
		mixed_candidate(&fixture, &records[0], 0).commit().assert_ok();
		assert_bulk_image(&fixture);
		assert_fixed_data(&fixture, &files);
	}
	for point in [
		CommitFailurePoint::BeforeAppend,
		CommitFailurePoint::AfterAppend,
		CommitFailurePoint::AfterFlush,
		CommitFailurePoint::AfterSync,
	] {
		let (mut fixture, records) = mixed_fixture(io_mode, 0);
		let stamp = fixture.store.inner.current_root().commit_ts;
		let files = local(&fixture.store).state.lock().file_catalog.files.clone();
		{
			let _failure = CommitFailureGuard::set(point);
			mixed_candidate(&fixture, &records[0], 0).commit().assert_err();
		}
		assert_eq!(fixture.store.inner.current_root().commit_ts, stamp);
		assert_eq!(local(&fixture.store).state.lock().file_catalog.files, files);
		assert_bulk_image(&fixture);
		fixture = reopen(fixture);
		let accepted = point != CommitFailurePoint::BeforeAppend;
		let root = fixture.store.inner.current_root();
		assert_eq!(root.commit_ts, stamp + u64::from(accepted));
		for record in [&records[0], &fixture.records[0]] {
			assert_eq!(
				root.membership(fixture.graph.id(), root.resolve_atom(record).assert_some())
					.is_some(),
				accepted
			);
		}
		drop(root);
		assert_bulk_image(&fixture);
		assert_fixed_data(&fixture, &files);
		fixture.store.checkpoint(CheckpointMode::Restart).assert_ok();
		fixture = reopen(fixture);
		assert_bulk_image(&fixture);
	}
}
