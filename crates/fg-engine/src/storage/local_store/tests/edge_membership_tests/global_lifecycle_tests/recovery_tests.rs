//! Both live lifecycle sides retain one durable publication and cleanup owner.

use super::*;
use crate::storage::wal::{CommitFailureGuard, CommitFailurePoint};

/// A deletion baseline already contains the same fully published EDGE.
fn source(io_mode: IoMode, deleting: bool, record: &RecordId) -> support::Fixture {
	let fixture = fixture(io_mode);
	if deleting {
		candidate(&fixture, record, 10).0.commit().assert_ok();
		fixture.store.checkpoint(CheckpointMode::Restart).assert_ok();
	}
	support::reopen(fixture)
}

/// Both mutations use the same boundary injection and independent oracles.
fn write(fixture: &support::Fixture, record: &RecordId, deleting: bool) -> Transaction {
	if deleting {
		let mut write = fixture.store.begin();
		write.delete_atom_cascade(record, true).assert_ok();
		write
	} else {
		candidate(fixture, record, 10).0
	}
}

/// A known failure retains old state; uncertain WAL completion recovers once.
#[test]
fn global_edge_lifecycle_failures_recover_one_complete_image() {
	for io_mode in [IoMode::Sync, IoMode::NativeAsync] {
		if io_mode == IoMode::NativeAsync && !cfg!(feature = "native-async-io") {
			continue;
		}
		for deleting in [false, true] {
			let record = "edge:change".parse::<RecordId>().assert_ok();
			let previous = if deleting {
				vec![(record.clone(), 10)]
			} else {
				Vec::new()
			};
			{
				let fixture = source(io_mode, deleting, &record);
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
						let candidate = write(&fixture, &record, deleting);
						let _failure = GenerationFailureGuard::set(point);
						candidate.commit().assert_err();
					}
					assert_eq!(fixture.store.inner.current_root().commit_ts, stamp);
					assert_eq!(local.wal.snapshot().visible_lsn(), boundary);
					assert_eq!(local.state.lock().file_catalog.files, files);
					assert_eq!(generation_directories(&fixture.directory.0), directories);
					assert_indexes(&fixture, "edge", &previous);
					support::assert_bulk_image(&fixture);
				}
			}
			for point in [
				CommitFailurePoint::BeforeAppend,
				CommitFailurePoint::AfterAppend,
				CommitFailurePoint::AfterFlush,
				CommitFailurePoint::AfterSync,
			] {
				let mut fixture = source(io_mode, deleting, &record);
				let local = support::local(&fixture.store);
				let stamp = fixture.store.inner.current_root().commit_ts;
				let files = local.state.lock().file_catalog.files.clone();
				{
					let candidate = write(&fixture, &record, deleting);
					let _failure = CommitFailureGuard::set(point);
					candidate.commit().assert_err();
				}
				assert_eq!(fixture.store.inner.current_root().commit_ts, stamp);
				assert_eq!(local.state.lock().file_catalog.files, files);
				fixture = support::reopen(fixture);
				let accepted = point != CommitFailurePoint::BeforeAppend;
				let root = fixture.store.inner.current_root();
				assert_eq!(root.commit_ts, stamp + u64::from(accepted));
				let generation = if deleting {
					Some(u32::from(accepted))
				} else {
					accepted.then_some(0)
				};
				assert_eq!(root.atoms.slot_generation(2), generation);
				assert_eq!(
					root.graph(fixture.graph.id()).assert_some().slot_generation(AtomKind::Edge, 0),
					generation
				);
				drop(root);
				let expected = if accepted != deleting {
					vec![(record.clone(), 10)]
				} else {
					Vec::new()
				};
				assert_indexes(&fixture, "edge", &expected);
				support::assert_bulk_image(&fixture);
				fixture.store.checkpoint(CheckpointMode::Restart).assert_ok();
				fixture = support::reopen(fixture);
				assert_indexes(&fixture, "edge", &expected);
				support::assert_bulk_image(&fixture);
			}
		}
	}
}

/// Shared admission must reject before either Base or topology becomes durable.
#[test]
fn global_edge_small_budget_preserves_both_lifecycle_sides() {
	for io_mode in [IoMode::Sync, IoMode::NativeAsync] {
		if io_mode == IoMode::NativeAsync && !cfg!(feature = "native-async-io") {
			continue;
		}
		for deleting in [false, true] {
			let record = "edge:change".parse::<RecordId>().assert_ok();
			let mut fixture = source(io_mode, deleting, &record);
			fixture.options.maintenance.max_memory_bytes =
				NonZeroUsize::new(32 * 1024).assert_some();
			fixture = support::reopen(fixture);
			let local = support::local(&fixture.store);
			let stamp = fixture.store.inner.current_root().commit_ts;
			let boundary = local.wal.snapshot().visible_lsn();
			let files = local.state.lock().file_catalog.files.clone();
			let directories = generation_directories(&fixture.directory.0);
			let error = write(&fixture, &record, deleting).commit().assert_err();
			assert!(matches!(
				error.status().status(),
				crate::gql_status::Status::StorageDecodeLimitExceeded { .. }
			));
			assert_eq!(fixture.store.inner.current_root().commit_ts, stamp);
			assert_eq!(local.wal.snapshot().visible_lsn(), boundary);
			assert_eq!(local.state.lock().file_catalog.files, files);
			assert_eq!(generation_directories(&fixture.directory.0), directories);
			let expected = if deleting {
				vec![(record.clone(), 10)]
			} else {
				Vec::new()
			};
			assert_indexes(&fixture, "edge", &expected);
			support::assert_bulk_image(&fixture);
		}
	}
}

/// A cold empty EDGE array must be validated before an attached Base insertion.
#[test]
fn global_edge_corrupt_cold_class_source_publishes_nothing() {
	for io_mode in [IoMode::Sync, IoMode::NativeAsync] {
		if io_mode == IoMode::NativeAsync && !cfg!(feature = "native-async-io") {
			continue;
		}
		let mut fixture = fixture(io_mode);
		let record = "edge:old".parse::<RecordId>().assert_ok();
		for _ in 0..2 {
			candidate(&fixture, &record, 10).0.commit().assert_ok();
			let mut remove = fixture.store.begin();
			remove.delete_atom_cascade(&record, true).assert_ok();
			remove.commit().assert_ok();
			fixture.store.checkpoint(CheckpointMode::Restart).assert_ok();
			fixture = support::reopen(fixture);
		}
		corrupt_empty_class(&fixture);
		fixture = support::reopen(fixture);
		let local = support::local(&fixture.store);
		let stamp = fixture.store.inner.current_root().commit_ts;
		let boundary = local.wal.snapshot().visible_lsn();
		let files = local.state.lock().file_catalog.files.clone();
		let directories = generation_directories(&fixture.directory.0);
		candidate(&fixture, &"edge:new".parse::<RecordId>().assert_ok(), 20)
			.0
			.commit()
			.assert_err();
		assert_eq!(fixture.store.inner.current_root().commit_ts, stamp);
		assert_eq!(local.wal.snapshot().visible_lsn(), boundary);
		assert_eq!(local.state.lock().file_catalog.files, files);
		assert_eq!(generation_directories(&fixture.directory.0), directories);
		assert_eq!(fixture.store.inner.current_root().atoms.slot_generation(2), Some(2));
	}
}

/// A checksummed nonempty array contradicts the accepted empty EDGE class.
pub(super) fn corrupt_empty_class(fixture: &support::Fixture) {
	use crate::{
		storage::{graph_index::build_label_index, page::PageLsn},
		values::storage::DiskAtomRef,
	};
	let root = fixture.store.inner.current_root();
	let class = root.schema.class("edge").assert_some().id().assert_some();
	let suffix = format!("/idx/cls_{}.fgidx", class.get());
	let state = support::local(&fixture.store).state.lock();
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
		&[DiskAtomRef::from_parts(AtomKind::Edge, 0).assert_ok()],
		AtomKind::Edge,
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
