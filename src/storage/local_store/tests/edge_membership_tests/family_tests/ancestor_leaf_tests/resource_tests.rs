//! Local admission failure cannot publish a partial ancestor arena bundle.

use super::*;
use crate::storage::local_store::tests::generation_directory_paths;

/// Sync rejects a small envelope and restores the unchanged production policy.
#[test]
fn ancestor_leaf_sync_resource_rejection_preserves_all_owners() {
	reject(IoMode::Sync);
}

/// NativeAsync shares the same commit admission and cold final authority.
#[cfg(feature = "native-async-io")]
#[test]
fn ancestor_leaf_native_async_resource_rejection_preserves_all_owners() {
	reject(IoMode::NativeAsync);
}

/// The cumulative builder unit separately isolates the multi-arena accounting.
fn reject(mode: IoMode) {
	for kind in [AtomKind::Node, AtomKind::Edge] {
		let mut routes = fanout(mode, kind, false);
		let normal = routes.fixture.options.maintenance.max_memory_bytes;
		// Persist installs the opened policy, overriding caller TLS limits.
		routes.fixture.options.maintenance.max_memory_bytes =
			std::num::NonZeroUsize::new(8 * 1024).assert_some();
		routes.fixture = reopen(routes.fixture);
		assert_eq!(
			routes
				.fixture
				.store
				.local_storage_status()
				.assert_some()
				.options()
				.maintenance
				.max_memory_bytes
				.get(),
			8 * 1024
		);
		let root = routes.fixture.store.inner.current_root();
		let store = local(&routes.fixture.store);
		let (files, sources) = {
			let state = store.state.lock();
			(state.file_catalog.files.clone(), arena_sources(&routes, &state))
		};
		let boundary = store.wal.snapshot().visible_lsn();
		let paths = generation_directory_paths(&routes.fixture.directory.0);
		let error = change(&routes).commit().assert_err();
		assert!(matches!(
			error.status().status(),
			crate::gql_status::Status::StorageDecodeLimitExceeded { .. }
		));
		assert!(Arc::ptr_eq(&root, &routes.fixture.store.inner.current_root()));
		assert_eq!(store.wal.snapshot().visible_lsn(), boundary);
		let state = store.state.lock();
		assert_sources(&state, &sources);
		assert_eq!(state.file_catalog.files, files);
		drop(state);
		assert_eq!(generation_directory_paths(&routes.fixture.directory.0), paths);
		let old = sources.iter().map(|(id, arena, _)| (*id, arena.clone())).collect::<Vec<_>>();
		drop(root);
		routes.fixture.options.maintenance.max_memory_bytes = normal;
		routes.fixture = reopen(routes.fixture);
		assert_eq!(
			routes
				.fixture
				.store
				.local_storage_status()
				.assert_some()
				.options()
				.maintenance
				.max_memory_bytes,
			normal
		);
		change(&routes).commit().assert_ok();
		current(&routes, &old);
		assert_retained(&routes.fixture, &files, false);
		routes.fixture = reopen(routes.fixture);
		current(&routes, &old);
		assert_retained(&routes.fixture, &files, false);
		eprintln!("ANCESTOR_RESOURCE_REJECT_PASS {mode:?} {kind:?}");
	}
}
