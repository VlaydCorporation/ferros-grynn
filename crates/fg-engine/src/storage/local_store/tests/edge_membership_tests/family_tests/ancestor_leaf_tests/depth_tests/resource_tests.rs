//! Opened maintenance admission cannot publish part of a four-owner bundle.

use super::*;

/// Sync preserves T when the opened maintenance envelope rejects the commit.
#[test]
fn ancestor_depth_sync_resource_rejection_preserves_four_owners() {
	reject(IoMode::Sync);
}

/// NativeAsync restores the same default policy before the positive control.
#[cfg(feature = "native-async-io")]
#[test]
fn ancestor_depth_native_async_resource_rejection_preserves_four_owners() {
	reject(IoMode::NativeAsync);
}

/// The separate eight-map unit isolates T's cumulative canonical arena cost.
fn reject(mode: IoMode) {
	reject_with_families(mode, [GraphKind::Metagraph; 3]);
}

/// Graph ancestors use exactly the same four-owner rejection and control gates.
pub(super) fn reject_with_families(mode: IoMode, families: [GraphKind; 3]) {
	reject_with_parent(mode, families, GraphKind::Metagraph);
}

/// Runs the unchanged ancestor gates for the selected public owner family.
pub(super) fn reject_with_parent(mode: IoMode, families: [GraphKind; 3], parent_family: GraphKind) {
	for kind in [AtomKind::Node, AtomKind::Edge] {
		let mut case = depth_with_parent(mode, kind, false, families, parent_family);
		let normal = case.routes.fixture.options.maintenance.max_memory_bytes;
		case.routes.fixture.options.maintenance.max_memory_bytes =
			std::num::NonZeroUsize::new(8 * 1024).assert_some();
		case.routes.fixture = reopen(case.routes.fixture);
		assert_eq!(
			case.routes
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
		let root = case.routes.fixture.store.inner.current_root();
		let store = local(&case.routes.fixture.store);
		let (files, sources) = {
			let state = store.state.lock();
			(state.file_catalog.files.clone(), depth_sources(&case, &state))
		};
		let paths = generation_directory_paths(&case.routes.fixture.directory.0);
		let boundary = store.wal.snapshot().visible_lsn();
		let error = change(&case.routes).commit().assert_err();
		assert!(matches!(
			error.status().status(),
			crate::gql_status::Status::StorageDecodeLimitExceeded { .. }
		));
		assert!(Arc::ptr_eq(&root, &case.routes.fixture.store.inner.current_root()));
		assert_eq!(store.wal.snapshot().visible_lsn(), boundary);
		let state = store.state.lock();
		assert_depth_sources(&state, &sources);
		assert_eq!(state.file_catalog.files, files);
		drop(state);
		assert_eq!(generation_directory_paths(&case.routes.fixture.directory.0), paths);
		drop(root);
		case.routes.fixture.options.maintenance.max_memory_bytes = normal;
		case.routes.fixture = reopen(case.routes.fixture);
		assert_eq!(
			case.routes
				.fixture
				.store
				.local_storage_status()
				.assert_some()
				.options()
				.maintenance
				.max_memory_bytes,
			normal
		);
		old_depth(&case, &sources);
		change(&case.routes).commit().assert_ok();
		final_depth(&case, &sources);
		assert_retained(&case.routes.fixture, &files, false);
		case.routes.fixture = reopen(case.routes.fixture);
		final_depth(&case, &sources);
		assert_retained(&case.routes.fixture, &files, false);
		eprintln!("ANCESTOR_DEPTH_RESOURCE_REJECT_PASS {mode:?} {kind:?}");
	}
}
