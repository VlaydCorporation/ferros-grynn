//! Every ancestor map crosses the existing known or uncertain commit barrier.

use super::*;
use crate::storage::{
	local_store::tests::generation_directory_paths,
	wal::{CommitFailureGuard, CommitFailurePoint},
};

/// Sync leaves all owners old on failure and reopens one complete WAL decision.
#[test]
fn ancestor_leaf_sync_failures_recover_complete_arenas() {
	failures(IoMode::Sync);
}

/// NativeAsync preserves the same complete old/final catalog authority.
#[cfg(feature = "native-async-io")]
#[test]
fn ancestor_leaf_native_async_failures_recover_complete_arenas() {
	failures(IoMode::NativeAsync);
}

/// Cold reconstruction may create a new owner with the same canonical value.
fn old_current(routes: &Fanout, sources: &ArenaSources) {
	let store = local(&routes.fixture.store);
	let state = store.state.lock();
	for (id, old, _) in sources {
		let binding =
			state.bindings.graphs.iter().find(|entry| entry.graph_id == *id).assert_some();
		let catalog =
			state._database_catalog.graphs.iter().find(|entry| entry.id == *id).assert_some();
		assert_eq!(&binding.arena, old);
		assert!(catalog.arena.same_owner(&binding.arena));
	}
	drop(state);
	let mut read = routes.fixture.store.begin();
	let query = read.query_snapshot().assert_ok();
	physical(routes, &read, &query, false);
	assert_bulk_image(&routes.fixture);
}

/// Known private failures leave the source writer usable for one later commit.
fn known_failures(mode: IoMode, kind: AtomKind) {
	for (point_index, point) in [
		GenerationFailurePoint::AfterBuild,
		GenerationFailurePoint::AfterCatalogStage,
		GenerationFailurePoint::BeforeWalCommit,
	]
	.into_iter()
	.enumerate()
	{
		let mut routes = fanout(mode, kind, false);
		let root = routes.fixture.store.inner.current_root();
		let store = local(&routes.fixture.store);
		let (files, sources) = {
			let state = store.state.lock();
			(state.file_catalog.files.clone(), arena_sources(&routes, &state))
		};
		let boundary = store.wal.snapshot().visible_lsn();
		let paths = generation_directory_paths(&routes.fixture.directory.0);

		let error = {
			let _failure = GenerationFailureGuard::set(point);
			change(&routes).commit().assert_err()
		};
		assert!(
			matches!(error.status().status(), crate::gql_status::Status::StorageIoFailure { operation, .. } if operation.as_str() == "injected generation publication failure")
		);
		assert!(Arc::ptr_eq(&root, &routes.fixture.store.inner.current_root()));
		assert_eq!(store.wal.snapshot().visible_lsn(), boundary);
		let state = store.state.lock();
		assert_sources(&state, &sources);
		assert_eq!(state.file_catalog.files, files);
		drop(state);
		assert_eq!(generation_directory_paths(&routes.fixture.directory.0), paths);
		old_current(&routes, &sources);
		// A real catalog-only commit proves the same writer remains usable.
		let mut marker = routes.fixture.store.begin();
		marker.intern_property("after_known_ancestor_failure").assert_ok();
		marker.commit().assert_ok();
		assert_sources(&store.state.lock(), &sources);
		let stamp = root.commit_ts + 1;
		drop(root);
		routes.fixture = reopen(routes.fixture);
		assert_eq!(routes.fixture.store.inner.current_root().commit_ts, stamp);
		old_current(&routes, &sources);
		assert_retained(&routes.fixture, &files, false);
		let old = sources.iter().map(|(id, arena, _)| (*id, arena.clone())).collect::<Vec<_>>();
		change(&routes).commit().assert_ok();
		current(&routes, &old);
		assert_retained(&routes.fixture, &files, false);
		eprintln!("ANCESTOR_GENERATION_REJECT_COLD_PASS {mode:?} {kind:?} point={point_index}");
	}
}

/// A poisoned writer requires reopen; durable append decides the recovered
/// root.
fn failures(mode: IoMode) {
	for kind in [AtomKind::Node, AtomKind::Edge] {
		known_failures(mode, kind);
		for (point_index, point) in [
			CommitFailurePoint::BeforeAppend,
			CommitFailurePoint::AfterAppend,
			CommitFailurePoint::AfterFlush,
			CommitFailurePoint::AfterSync,
		]
		.into_iter()
		.enumerate()
		{
			let mut routes = fanout(mode, kind, false);
			let root = routes.fixture.store.inner.current_root();
			let store = local(&routes.fixture.store);
			let (files, sources) = {
				let state = store.state.lock();
				(state.file_catalog.files.clone(), arena_sources(&routes, &state))
			};
			let error = {
				let _failure = CommitFailureGuard::set(point);
				change(&routes).commit().assert_err()
			};
			assert!(
				matches!(error.status().status(), crate::gql_status::Status::StorageIoFailure { operation, .. } if operation.as_str() == "injected WAL durability failure")
			);
			assert!(Arc::ptr_eq(&root, &routes.fixture.store.inner.current_root()));
			let state = store.state.lock();
			assert_sources(&state, &sources);
			assert_eq!(state.file_catalog.files, files);
			drop(state);
			old_current(&routes, &sources);
			let boundary = store.wal.snapshot().visible_lsn();
			let retry = change(&routes).commit().assert_err();
			assert!(
				matches!(retry.status().status(), crate::gql_status::Status::StorageIoFailure { operation, .. } if operation.as_str() == "prepare publication with poisoned WAL; reopen to recover")
			);
			assert_eq!(store.wal.snapshot().visible_lsn(), boundary);
			assert!(Arc::ptr_eq(&root, &routes.fixture.store.inner.current_root()));
			assert_sources(&store.state.lock(), &sources);
			let stamp = root.commit_ts;
			drop(root);
			routes.fixture = reopen(routes.fixture);
			let accepted = point != CommitFailurePoint::BeforeAppend;
			assert_eq!(
				routes.fixture.store.inner.current_root().commit_ts,
				stamp + u64::from(accepted)
			);
			if accepted {
				let old =
					sources.iter().map(|(id, arena, _)| (*id, arena.clone())).collect::<Vec<_>>();
				current(&routes, &old);
			} else {
				old_current(&routes, &sources);
			}
			assert_retained(&routes.fixture, &files, false);
			eprintln!("ANCESTOR_WAL_RECOVERY_PASS {mode:?} {kind:?} point={point_index}");
		}
	}
}
