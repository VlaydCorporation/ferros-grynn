//! Both detached sources are validated before the common publication barrier.

use super::*;
use crate::storage::{
	atom_page::{AtomHotPage, atom_location},
	local_store::tests::generation_directory_paths,
};

/// A legal page with the wrong second generation must fail before publication.
fn invalid_second_source(mode: IoMode) {
	for family in [GraphKind::Graph, GraphKind::Metagraph] {
		for kind in [AtomKind::Node, AtomKind::Edge] {
			let case = case_fixture(mode, family, kind);
			let other = companion(&case);
			let (before, mut after, delta) = batch_roots(&case, &other, true);
			assert!(prepare_publication(&case, (&before, &after), &delta).assert_ok());
			let published = case.fixture.store.inner.current_root();
			let arena = source_arena(&case);
			let prefix = property_prefix(&case);
			let store = local(&case.fixture.store);
			let state = store.state.lock();
			let (number, within) = atom_location(other.1.slot());
			let key = PageKey::new(DataSpaceId::Database, state.bindings.atoms.atoms_hot, number);
			let original = store.read_update_page(&state, key).assert_ok();
			let mut hot =
				AtomHotPage::from_page(store.read_update_page(&state, key).assert_ok(), number)
					.assert_ok();
			let (global, slot) = hot.read(within).assert_ok();
			assert_eq!(global, other.1);
			let stale = GlobalAtomId::from_raw(
				AtomId::from_parts(
					global.kind().assert_some(),
					global.slot(),
					global.generation() + 1,
				)
				.assert_ok()
				.raw(),
			);
			hot.write(stale, slot).assert_ok();
			let mut injection = store.wal.begin_tx().assert_ok();
			injection.stage_page(key, &hot.finish()).assert_ok();
			store.wal.commit_tx(injection).assert_ok();
			let files = state.file_catalog.files.clone();
			let boundary = store.wal.snapshot().visible_lsn();
			let paths = generation_directory_paths(&case.fixture.directory.0);
			drop(state);
			after.commit_ts = store.wal.snapshot().commit_ts().get() + 1;
			let error = prepare_publication(&case, (&before, &after), &delta).assert_err();
			assert!(
				matches!(
					error.status().status(),
					crate::gql_status::Status::InvalidStorageFile { .. }
						| crate::gql_status::Status::InvalidStoragePage { .. }
				),
				"{error}"
			);
			assert!(Arc::ptr_eq(&published, &case.fixture.store.inner.current_root()));
			assert_eq!(store.wal.snapshot().visible_lsn(), boundary);
			assert_eq!(store.state.lock().file_catalog.files, files);
			assert_eq!(generation_directory_paths(&case.fixture.directory.0), paths);
			assert_source_arena(&case, &arena);
			assert_prefix(&case, &prefix);
			let mut restoration = store.wal.begin_tx().assert_ok();
			restoration.stage_page(key, &original).assert_ok();
			store.wal.commit_tx(restoration).assert_ok();
			after.commit_ts = store.wal.snapshot().commit_ts().get() + 1;
			assert!(prepare_publication(&case, (&before, &after), &delta).assert_ok());
			assert_batch_old(&case, &other);
			println!("MIXED_BASE_BATCH_SECOND_SOURCE_PASS {mode:?} {family:?} {kind:?}");
		}
	}
}

/// Sync validates the second source independently of the first valid record.
#[test]
fn mixed_base_batch_sync_rejects_invalid_second_source() {
	invalid_second_source(IoMode::Sync);
}

/// NativeAsync preserves both accepted records after preparation rejection.
#[cfg(feature = "native-async-io")]
#[test]
fn mixed_base_batch_native_async_rejects_invalid_second_source() {
	invalid_second_source(IoMode::NativeAsync);
}
