//! Base and membership effects share the existing database commit boundary.

use super::*;
use crate::storage::wal::{CommitFailureGuard, CommitFailurePoint};

/// Every known rejection retains both old allocators and all file bindings.
#[test]
fn global_node_lifecycle_sync_failures_recover_one_complete_image() {
	failures(IoMode::Sync);
}

/// NativeAsync must recover the same complete image after uncertain append.
#[cfg(feature = "native-async-io")]
#[test]
fn global_node_lifecycle_native_async_failures_recover_one_complete_image() {
	failures(IoMode::NativeAsync);
}

/// Rejection before WAL differs from a durable but unacknowledged commit.
fn failures(io_mode: IoMode) {
	let record = "item:new".parse::<RecordId>().assert_ok();
	{
		let fixture = fixture(io_mode);
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
				let write = candidate(&fixture, &record, 10);
				let _failure = GenerationFailureGuard::set(point);
				write.commit().assert_err();
			}
			assert_eq!(fixture.store.inner.current_root().commit_ts, stamp);
			assert_eq!(local.wal.snapshot().visible_lsn(), boundary);
			assert_eq!(local.state.lock().file_catalog.files, files);
			assert_eq!(generation_directories(&fixture.directory.0), directories);
			assert_image(&fixture, &[]);
		}
		candidate(&fixture, &record, 10).commit().assert_ok();
		assert_image(&fixture, &[(record.clone(), 10)]);
	}
	for point in [
		CommitFailurePoint::BeforeAppend,
		CommitFailurePoint::AfterAppend,
		CommitFailurePoint::AfterFlush,
		CommitFailurePoint::AfterSync,
	] {
		let mut fixture = fixture(io_mode);
		let local = local_store(&fixture.store);
		let stamp = fixture.store.inner.current_root().commit_ts;
		let files = local.state.lock().file_catalog.files.clone();
		{
			let write = candidate(&fixture, &record, 10);
			let _failure = CommitFailureGuard::set(point);
			write.commit().assert_err();
		}
		assert_eq!(fixture.store.inner.current_root().commit_ts, stamp);
		assert_eq!(local.state.lock().file_catalog.files, files);
		assert_image(&fixture, &[]);
		fixture = reopen(fixture);
		let accepted = point != CommitFailurePoint::BeforeAppend;
		assert_eq!(fixture.store.inner.current_root().commit_ts, stamp + u64::from(accepted));
		let expected = if accepted {
			vec![(record.clone(), 10)]
		} else {
			Vec::new()
		};
		assert_image(&fixture, &expected);
		fixture.store.checkpoint(CheckpointMode::Restart).assert_ok();
		fixture = reopen(fixture);
		assert_image(&fixture, &expected);
	}
}

/// The shared builder envelope rejects before either part becomes durable.
#[test]
fn global_node_lifecycle_small_budget_preserves_root_wal_and_files() {
	for io_mode in [IoMode::Sync, IoMode::NativeAsync] {
		if io_mode == IoMode::NativeAsync && !cfg!(feature = "native-async-io") {
			continue;
		}
		let mut fixture = fixture(io_mode);
		fixture.options.maintenance.max_memory_bytes = NonZeroUsize::new(32 * 1024).assert_some();
		fixture = reopen(fixture);
		let local = local_store(&fixture.store);
		let stamp = fixture.store.inner.current_root().commit_ts;
		let boundary = local.wal.snapshot().visible_lsn();
		let files = local.state.lock().file_catalog.files.clone();
		let directories = generation_directories(&fixture.directory.0);
		let error = candidate(&fixture, &"item:new".parse::<RecordId>().assert_ok(), 10)
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

/// A cold empty class source must be validated even when its new Base owner
/// did not exist in the source root.
#[test]
fn global_node_lifecycle_corrupt_class_source_publishes_nothing() {
	use crate::{
		storage::{
			graph_index::build_label_index,
			page::{PageLsn, PageNo},
		},
		values::storage::DiskAtomRef,
	};
	let mut fixture = fixture(IoMode::Sync);
	let record = "item:old".parse::<RecordId>().assert_ok();
	candidate(&fixture, &record, 10).commit().assert_ok();
	let mut remove = fixture.store.begin();
	remove.delete_atom_cascade(&record, true).assert_ok();
	remove.commit().assert_ok();
	fixture.store.checkpoint(CheckpointMode::Restart).assert_ok();
	{
		let local = local_store(&fixture.store);
		let class = fixture
			.store
			.inner
			.current_root()
			.schema
			.class("item")
			.assert_some()
			.id()
			.assert_some();
		let suffix = format!("/idx/cls_{}.fgidx", class.get());
		let state = local.state.lock();
		let entry = state
			.file_catalog
			.files
			.iter()
			.find(|entry| {
				entry.space_id == DataSpaceId::Graph(fixture.graph.id())
					&& entry.path.as_str().ends_with(&suffix)
			})
			.assert_some();
		let pages = build_label_index(
			&[DiskAtomRef::from_parts(AtomKind::Node, 0).assert_ok()],
			AtomKind::Node,
			PageLsn::ZERO,
		)
		.assert_ok();
		state
			.registry
			.resolve_file(entry.space_id, entry.file_id)
			.assert_ok()
			.file()
			.assert_ok()
			.write_page_raw(PageNo::new(0), &pages[0])
			.assert_ok();
	}
	fixture = reopen(fixture);
	let local = local_store(&fixture.store);
	let stamp = fixture.store.inner.current_root().commit_ts;
	let boundary = local.wal.snapshot().visible_lsn();
	let files = local.state.lock().file_catalog.files.clone();
	let directories = generation_directories(&fixture.directory.0);
	candidate(&fixture, &"item:new".parse::<RecordId>().assert_ok(), 20).commit().assert_err();
	assert_eq!(fixture.store.inner.current_root().commit_ts, stamp);
	assert_eq!(local.wal.snapshot().visible_lsn(), boundary);
	assert_eq!(local.state.lock().file_catalog.files, files);
	assert_eq!(generation_directories(&fixture.directory.0), directories);
}
