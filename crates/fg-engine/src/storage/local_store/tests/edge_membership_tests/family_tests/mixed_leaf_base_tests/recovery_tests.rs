//! A single WAL decision owns the detached Base and replacement child together.

use super::*;
use crate::storage::{
	local_store::tests::generation_directory_paths,
	wal::{CommitFailureGuard, CommitFailurePoint},
};

/// Private failure must leave the accepted writer usable without replaying
/// half.
fn known_failures(mode: IoMode, family: GraphKind, kind: AtomKind) {
	for (index, point) in [
		GenerationFailurePoint::AfterBuild,
		GenerationFailurePoint::AfterCatalogStage,
		GenerationFailurePoint::BeforeWalCommit,
	]
	.into_iter()
	.enumerate()
	{
		let mut case = case_fixture(mode, family, kind);
		let published = case.fixture.store.inner.current_root();
		let arena = source_arena(&case);
		let prefix = property_prefix(&case);
		let store = local(&case.fixture.store);
		let files = store.state.lock().file_catalog.files.clone();
		let paths = generation_directory_paths(&case.fixture.directory.0);
		let boundary = store.wal.snapshot().visible_lsn();
		let error = {
			let _failure = GenerationFailureGuard::set(point);
			change(&case, 0).commit().assert_err()
		};
		assert!(
			matches!(error.status().status(), crate::gql_status::Status::StorageIoFailure { operation, .. } if operation.as_str() == "injected generation publication failure")
		);
		assert!(Arc::ptr_eq(&published, &case.fixture.store.inner.current_root()));
		assert_eq!(store.wal.snapshot().visible_lsn(), boundary);
		assert_eq!(store.state.lock().file_catalog.files, files);
		assert_eq!(generation_directory_paths(&case.fixture.directory.0), paths);
		assert_source_arena(&case, &arena);
		assert_prefix(&case, &prefix);
		assert_old(&case);
		let mut marker = case.fixture.store.begin();
		marker.intern_property("after_known_mixed_failure").assert_ok();
		marker.commit().assert_ok();
		assert_source_arena(&case, &arena);
		let stamp = published.commit_ts + 1;
		drop(published);
		case.fixture = reopen(case.fixture);
		assert_eq!(case.fixture.store.inner.current_root().commit_ts, stamp);
		assert_old(&case);
		assert_files(&case, &files);
		assert_prefix(&case, &prefix);
		change(&case, 0).commit().assert_ok();
		assert_current(&case, 0);
		assert_files(&case, &files);
		assert_prefix(&case, &prefix);
		eprintln!(
			"MIXED_LEAF_BASE_GENERATION_FAILURE_PASS {mode:?} {family:?} {kind:?} point={index}"
		);
	}
}

/// Uncertain durability fences retries until reopen resolves one complete pair.
fn wal_failures(mode: IoMode, family: GraphKind, kind: AtomKind) {
	for (index, point) in [
		CommitFailurePoint::BeforeAppend,
		CommitFailurePoint::AfterAppend,
		CommitFailurePoint::AfterFlush,
		CommitFailurePoint::AfterSync,
	]
	.into_iter()
	.enumerate()
	{
		let mut case = case_fixture(mode, family, kind);
		let published = case.fixture.store.inner.current_root();
		let arena = source_arena(&case);
		let prefix = property_prefix(&case);
		let store = local(&case.fixture.store);
		let files = store.state.lock().file_catalog.files.clone();
		let error = {
			let _failure = CommitFailureGuard::set(point);
			change(&case, 0).commit().assert_err()
		};
		assert!(
			matches!(error.status().status(), crate::gql_status::Status::StorageIoFailure { operation, .. } if operation.as_str() == "injected WAL durability failure")
		);
		assert!(Arc::ptr_eq(&published, &case.fixture.store.inner.current_root()));
		assert_eq!(store.state.lock().file_catalog.files, files);
		assert_source_arena(&case, &arena);
		assert_prefix(&case, &prefix);
		assert_old(&case);
		let boundary = store.wal.snapshot().visible_lsn();
		let retry = change(&case, 0).commit().assert_err();
		assert!(
			matches!(retry.status().status(), crate::gql_status::Status::StorageIoFailure { operation, .. } if operation.as_str() == "prepare publication with poisoned WAL; reopen to recover")
		);
		assert_eq!(store.wal.snapshot().visible_lsn(), boundary);
		assert!(Arc::ptr_eq(&published, &case.fixture.store.inner.current_root()));
		assert_source_arena(&case, &arena);
		let stamp = published.commit_ts;
		drop(published);
		case.fixture = reopen(case.fixture);
		let accepted = point != CommitFailurePoint::BeforeAppend;
		assert_eq!(case.fixture.store.inner.current_root().commit_ts, stamp + u64::from(accepted));
		if accepted {
			assert_current(&case, 0);
		} else {
			assert_old(&case);
		}
		assert_files(&case, &files);
		assert_prefix(&case, &prefix);
		case.fixture.store.checkpoint(CheckpointMode::Restart).assert_ok();
		case.fixture = reopen(case.fixture);
		if accepted {
			assert_current(&case, 0);
		} else {
			assert_old(&case);
		}
		assert_files(&case, &files);
		assert_prefix(&case, &prefix);
		eprintln!("MIXED_LEAF_BASE_WAL_RECOVERY_PASS {mode:?} {family:?} {kind:?} point={index}");
	}
}

/// Every reached fault observes both sides of the same admitted mutation.
fn failures(mode: IoMode) {
	for family in [GraphKind::Graph, GraphKind::Metagraph] {
		for kind in [AtomKind::Node, AtomKind::Edge] {
			known_failures(mode, family, kind);
			wal_failures(mode, family, kind);
		}
	}
}

/// Sync reaches three generation and four WAL faults on each owner/family pair.
#[test]
fn mixed_leaf_base_sync_failures_recover_atomic_pairs() {
	failures(IoMode::Sync);
}

/// NativeAsync preserves the same failure, fencing and cold recovery contract.
#[cfg(feature = "native-async-io")]
#[test]
fn mixed_leaf_base_native_async_failures_recover_atomic_pairs() {
	failures(IoMode::NativeAsync);
}
