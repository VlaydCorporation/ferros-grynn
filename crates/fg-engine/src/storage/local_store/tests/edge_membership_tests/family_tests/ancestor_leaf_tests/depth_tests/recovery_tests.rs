//! Every ancestor map crosses the existing known or uncertain commit barrier.

use super::*;
use crate::storage::{
	local_store::tests::generation_directory_paths,
	wal::{CommitFailureGuard, CommitFailurePoint},
};

/// Sync leaves all owners old on failure and reopens one complete WAL decision.
#[test]
fn ancestor_depth_sync_failures_recover_four_arenas() {
	failures(IoMode::Sync);
}

/// NativeAsync preserves the same complete old/final catalog authority.
#[cfg(feature = "native-async-io")]
#[test]
fn ancestor_depth_native_async_failures_recover_four_arenas() {
	failures(IoMode::NativeAsync);
}

/// Known private failures leave the source writer usable for one later commit.
fn known_failures(
	mode: IoMode,
	kind: AtomKind,
	families: [GraphKind; 3],
	parent_family: GraphKind,
) {
	for (point_index, point) in [
		GenerationFailurePoint::AfterBuild,
		GenerationFailurePoint::AfterCatalogStage,
		GenerationFailurePoint::BeforeWalCommit,
	]
	.into_iter()
	.enumerate()
	{
		let mut case = depth_with_parent(mode, kind, false, families, parent_family);
		let root = case.routes.fixture.store.inner.current_root();
		let store = local(&case.routes.fixture.store);
		let (files, sources) = {
			let state = store.state.lock();
			(state.file_catalog.files.clone(), depth_sources(&case, &state))
		};
		let boundary = store.wal.snapshot().visible_lsn();
		let paths = generation_directory_paths(&case.routes.fixture.directory.0);

		let error = {
			let _failure = GenerationFailureGuard::set(point);
			change(&case.routes).commit().assert_err()
		};
		assert!(
			matches!(error.status().status(), crate::gql_status::Status::StorageIoFailure { operation, .. } if operation.as_str() == "injected generation publication failure")
		);
		assert!(Arc::ptr_eq(&root, &case.routes.fixture.store.inner.current_root()));
		assert_eq!(store.wal.snapshot().visible_lsn(), boundary);
		let state = store.state.lock();
		assert_depth_sources(&state, &sources);
		assert_eq!(state.file_catalog.files, files);
		drop(state);
		assert_eq!(generation_directory_paths(&case.routes.fixture.directory.0), paths);
		old_depth(&case, &sources);
		// A real catalog-only commit proves the same writer remains usable.
		let mut marker = case.routes.fixture.store.begin();
		marker.intern_property("after_known_depth_failure").assert_ok();
		marker.commit().assert_ok();
		assert_depth_sources(&store.state.lock(), &sources);
		let stamp = root.commit_ts + 1;
		drop(root);
		case.routes.fixture = reopen(case.routes.fixture);
		assert_eq!(case.routes.fixture.store.inner.current_root().commit_ts, stamp);
		old_depth(&case, &sources);
		assert_retained(&case.routes.fixture, &files, false);
		change(&case.routes).commit().assert_ok();
		final_depth(&case, &sources);
		assert_retained(&case.routes.fixture, &files, false);
		eprintln!(
			"ANCESTOR_DEPTH_GENERATION_REJECT_COLD_PASS {mode:?} {kind:?} point={point_index}"
		);
	}
}

/// A poisoned writer requires reopen; durable append decides the recovered
/// root.
fn failures(mode: IoMode) {
	failures_with_families(mode, [GraphKind::Metagraph; 3]);
}

/// Both public ancestor families cross the unchanged known and uncertain
/// barriers.
pub(super) fn failures_with_families(mode: IoMode, families: [GraphKind; 3]) {
	failures_with_parent(mode, families, GraphKind::Metagraph);
}

/// Runs the unchanged ancestor gates for the selected public owner family.
pub(super) fn failures_with_parent(
	mode: IoMode,
	families: [GraphKind; 3],
	parent_family: GraphKind,
) {
	for kind in [AtomKind::Node, AtomKind::Edge] {
		known_failures(mode, kind, families, parent_family);
		for (point_index, point) in [
			CommitFailurePoint::BeforeAppend,
			CommitFailurePoint::AfterAppend,
			CommitFailurePoint::AfterFlush,
			CommitFailurePoint::AfterSync,
		]
		.into_iter()
		.enumerate()
		{
			let mut case = depth_with_parent(mode, kind, false, families, parent_family);
			let root = case.routes.fixture.store.inner.current_root();
			let store = local(&case.routes.fixture.store);
			let (files, sources) = {
				let state = store.state.lock();
				(state.file_catalog.files.clone(), depth_sources(&case, &state))
			};
			let error = {
				let _failure = CommitFailureGuard::set(point);
				change(&case.routes).commit().assert_err()
			};
			assert!(
				matches!(error.status().status(), crate::gql_status::Status::StorageIoFailure { operation, .. } if operation.as_str() == "injected WAL durability failure")
			);
			assert!(Arc::ptr_eq(&root, &case.routes.fixture.store.inner.current_root()));
			let state = store.state.lock();
			assert_depth_sources(&state, &sources);
			assert_eq!(state.file_catalog.files, files);
			drop(state);
			old_depth(&case, &sources);
			let boundary = store.wal.snapshot().visible_lsn();
			let retry = change(&case.routes).commit().assert_err();
			assert!(
				matches!(retry.status().status(), crate::gql_status::Status::StorageIoFailure { operation, .. } if operation.as_str() == "prepare publication with poisoned WAL; reopen to recover")
			);
			assert_eq!(store.wal.snapshot().visible_lsn(), boundary);
			assert!(Arc::ptr_eq(&root, &case.routes.fixture.store.inner.current_root()));
			assert_depth_sources(&store.state.lock(), &sources);
			let stamp = root.commit_ts;
			drop(root);
			case.routes.fixture = reopen(case.routes.fixture);
			let accepted = point != CommitFailurePoint::BeforeAppend;
			assert_eq!(
				case.routes.fixture.store.inner.current_root().commit_ts,
				stamp + u64::from(accepted)
			);
			if accepted {
				final_depth(&case, &sources);
			} else {
				old_depth(&case, &sources);
			}
			assert_retained(&case.routes.fixture, &files, false);
			eprintln!("ANCESTOR_DEPTH_WAL_RECOVERY_PASS {mode:?} {kind:?} point={point_index}");
		}
	}
}
