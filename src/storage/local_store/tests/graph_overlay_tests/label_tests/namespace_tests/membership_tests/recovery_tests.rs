//! Mixed metadata cannot escape a failed or resource-rejected publication.

use super::*;
use crate::storage::wal::{CommitFailureGuard, CommitFailurePoint};

/// Reuses the mixed candidate so every other mutation stays identical.
fn candidate(
	fixture: &LabelFixture,
	record: &RecordId,
	kind: AtomKind,
	labelled: bool,
	with_payload: bool,
) -> Transaction {
	let mut write = membership_candidate(fixture, record, kind, false);
	if labelled {
		write.set_occurrence_label(fixture.graph.id(), record, Some("A".into())).assert_ok();
	}
	if with_payload {
		write
			.set_overlay(
				fixture.graph.id(),
				record,
				fixture.keys[1],
				Value::Bytes(vec![7; 40_000].into()),
			)
			.assert_ok();
	}
	write
}

/// The logical result is independent of the physical bulk-image oracle.
fn assert_published_label(fixture: &LabelFixture, record: &RecordId, labelled: bool) {
	let root = fixture.store.inner.current_root();
	let global = root.resolve_atom(record).assert_some();
	let local = root.membership(fixture.graph.id(), global).assert_some();
	assert_eq!(
		root.occurrence(fixture.graph.id(), local)
			.assert_some()
			.label()
			.map(|label| label.as_str()),
		labelled.then_some("A")
	);
}

/// Each backend and membership kind keeps known failures before publication.
#[test]
fn metadata_membership_known_failures_publish_nothing() {
	known_failures(false, false);
}

/// The same failure boundary covers labelled lifecycle admission.
#[test]
fn label_lifecycle_known_failures() {
	known_failures(true, false);
}

/// Shares the physical failure matrix across plain and labelled candidates.
fn known_failures(labelled: bool, with_payload: bool) {
	for io_mode in [IoMode::Sync, IoMode::NativeAsync] {
		if io_mode == IoMode::NativeAsync && !cfg!(feature = "native-async-io") {
			continue;
		}
		for kind in [AtomKind::Node, AtomKind::Edge] {
			let (fixture, pending) = membership_fixture(io_mode);
			let record = &pending[usize::from(kind == AtomKind::Edge)];
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
				let write = candidate(&fixture, record, kind, labelled, with_payload);
				{
					let _failure = GenerationFailureGuard::set(point);
					write.commit().assert_err();
				}
				assert_eq!(fixture.store.inner.current_root().commit_ts, stamp);
				assert_eq!(local.wal.snapshot().visible_lsn(), boundary);
				assert_eq!(local.state.lock().file_catalog.files, files);
				assert_eq!(generation_directories(&fixture.directory.0), directories);
				oracles::assert_image(&fixture);
				assert!(
					fixture
						.store
						.begin()
						.membership(fixture.graph.id(), record)
						.assert_ok()
						.is_none()
				);
			}
			candidate(&fixture, record, kind, labelled, with_payload).commit().assert_ok();
			assert_published_label(&fixture, record, labelled);
			if with_payload {
				assert_overlay_property(
					&fixture.store,
					fixture.graph.id(),
					record,
					fixture.keys[1],
					Value::Bytes(vec![7; 40_000].into()),
				);
			}
			assert!(
				fixture.store.begin().membership(fixture.graph.id(), record).assert_ok().is_some()
			);
			oracles::assert_image(&fixture);
		}
	}
}

/// Recovery selects one complete image after every uncertain writer boundary.
#[test]
fn metadata_membership_uncertain_failures_recover_once() {
	uncertain_failures(false, false);
}

/// The same failure boundary covers labelled lifecycle admission.
#[test]
fn label_lifecycle_uncertain_failures() {
	uncertain_failures(true, false);
}

/// Shares the physical failure matrix across plain and labelled candidates.
fn uncertain_failures(labelled: bool, with_payload: bool) {
	for io_mode in [IoMode::Sync, IoMode::NativeAsync] {
		if io_mode == IoMode::NativeAsync && !cfg!(feature = "native-async-io") {
			continue;
		}
		for kind in [AtomKind::Node, AtomKind::Edge] {
			for point in [
				CommitFailurePoint::BeforeAppend,
				CommitFailurePoint::AfterAppend,
				CommitFailurePoint::AfterFlush,
				CommitFailurePoint::AfterSync,
			] {
				let (mut fixture, pending) = membership_fixture(io_mode);
				let record = &pending[usize::from(kind == AtomKind::Edge)];
				let stamp = fixture.store.inner.current_root().commit_ts;
				let files = local_store(&fixture.store).state.lock().file_catalog.files.clone();
				let write = candidate(&fixture, record, kind, labelled, with_payload);
				{
					let _failure = CommitFailureGuard::set(point);
					write.commit().assert_err();
				}
				assert_eq!(fixture.store.inner.current_root().commit_ts, stamp);
				assert_eq!(local_store(&fixture.store).state.lock().file_catalog.files, files);
				drop(fixture.store);
				fixture.store =
					Datastore::open_local(&fixture.directory.0, fixture.options).assert_ok();
				assert_eq!(
					fixture.store.local_storage_status().assert_some().options().io_mode,
					io_mode
				);
				fixture.store.inner.maintenance.shutdown();
				let accepted = point != CommitFailurePoint::BeforeAppend;
				if accepted {
					assert_published_label(&fixture, record, labelled);
					if with_payload {
						assert_overlay_property(
							&fixture.store,
							fixture.graph.id(),
							record,
							fixture.keys[1],
							Value::Bytes(vec![7; 40_000].into()),
						);
					}
				}
				assert_eq!(
					fixture.store.inner.current_root().commit_ts,
					stamp + u64::from(accepted)
				);
				assert_eq!(
					fixture
						.store
						.begin()
						.membership(fixture.graph.id(), record)
						.assert_ok()
						.is_some(),
					accepted
				);
				assert_overlay_property(
					&fixture.store,
					fixture.graph.id(),
					&fixture.records[0],
					fixture.keys[0],
					if accepted {
						10_000.into()
					} else {
						0.into()
					},
				);
				oracles::assert_image(&fixture);
				fixture = reopen_checkpointed(fixture, io_mode);
				oracles::assert_image(&fixture);
			}
		}
	}
}

/// The shared logical envelope rejects before pages, side files and WAL.
#[test]
fn metadata_membership_small_budget_preserves_private_publication() {
	small_budget(false, false);
}

/// The same failure boundary covers labelled lifecycle admission.
#[test]
fn label_lifecycle_small_budget() {
	small_budget(true, false);
}

/// Shares the physical failure matrix across plain and labelled candidates.
fn small_budget(labelled: bool, with_payload: bool) {
	for io_mode in [IoMode::Sync, IoMode::NativeAsync] {
		if io_mode == IoMode::NativeAsync && !cfg!(feature = "native-async-io") {
			continue;
		}
		for kind in [AtomKind::Node, AtomKind::Edge] {
			let (mut fixture, pending) = membership_fixture(io_mode);
			fixture.options.maintenance.max_memory_bytes =
				std::num::NonZeroUsize::new(32 * 1024).assert_some();
			fixture = reopen_checkpointed(fixture, io_mode);
			let local = local_store(&fixture.store);
			let stamp = fixture.store.inner.current_root().commit_ts;
			let boundary = local.wal.snapshot().visible_lsn();
			let files = local.state.lock().file_catalog.files.clone();
			let directories = generation_directories(&fixture.directory.0);
			let record = &pending[usize::from(kind == AtomKind::Edge)];
			let error =
				candidate(&fixture, record, kind, labelled, with_payload).commit().assert_err();
			assert!(matches!(
				error.status().status(),
				crate::gql_status::Status::StorageDecodeLimitExceeded { .. }
			));
			assert_eq!(fixture.store.inner.current_root().commit_ts, stamp);
			assert_eq!(local.wal.snapshot().visible_lsn(), boundary);
			assert_eq!(local.state.lock().file_catalog.files, files);
			assert_eq!(generation_directories(&fixture.directory.0), directories);
		}
	}
}

/// Lifecycle-owned large payloads share the pre-WAL rejection boundaries.
#[test]
fn overlay_lifecycle_known_failures_publish_nothing() {
	known_failures(true, true);
}

/// Recovery accepts one complete candidate, including its lifecycle payload.
#[test]
fn overlay_lifecycle_uncertain_failures_recover_once() {
	uncertain_failures(true, true);
}

/// An oversized lifecycle payload cannot escape the shared publication ceiling.
#[test]
fn overlay_lifecycle_small_budget_publishes_nothing() {
	small_budget(true, true);
}

/// Checksummed source arrays still have to agree with the accepted old root.
#[test]
fn metadata_membership_corrupt_cold_label_source_publishes_nothing() {
	corrupt_source(false, false);
}

/// The same failure boundary covers labelled lifecycle admission.
#[test]
fn label_lifecycle_corrupt_source() {
	corrupt_source(true, false);
}

/// Shares the physical failure matrix across plain and labelled candidates.
fn corrupt_source(labelled: bool, with_payload: bool) {
	for io_mode in [IoMode::Sync, IoMode::NativeAsync] {
		if io_mode == IoMode::NativeAsync && !cfg!(feature = "native-async-io") {
			continue;
		}
		let (mut fixture, pending) = membership_fixture(io_mode);
		fixture = reopen_checkpointed(fixture, io_mode);
		{
			let local = local_store(&fixture.store);
			let state = local.state.lock();
			let entry = state
				.file_catalog
				.files
				.iter()
				.find(|entry| {
					entry.space_id == DataSpaceId::Graph(fixture.graph.id())
						&& entry.path.as_str().ends_with("/idx/lbl_node_1.fgidx")
				})
				.assert_some();
			let wrong = build_label_index(
				&[local_address(fixture.nodes[2]).assert_ok()],
				AtomKind::Node,
				crate::storage::page::PageLsn::ZERO,
			)
			.assert_ok();
			state
				.registry
				.resolve_file(entry.space_id, entry.file_id)
				.assert_ok()
				.file()
				.assert_ok()
				.write_page_raw(crate::storage::page::PageNo::new(0), &wrong[0])
				.assert_ok();
		}
		drop(fixture.store);
		fixture.store = Datastore::open_local(&fixture.directory.0, fixture.options).assert_ok();
		assert_eq!(fixture.store.local_storage_status().assert_some().options().io_mode, io_mode);
		fixture.store.inner.maintenance.shutdown();
		let local = local_store(&fixture.store);
		let stamp = fixture.store.inner.current_root().commit_ts;
		let boundary = local.wal.snapshot().visible_lsn();
		let files = local.state.lock().file_catalog.files.clone();
		candidate(&fixture, &pending[1], AtomKind::Edge, labelled, with_payload)
			.commit()
			.assert_err();
		assert_eq!(fixture.store.inner.current_root().commit_ts, stamp);
		assert_eq!(local.wal.snapshot().visible_lsn(), boundary);
		assert_eq!(local.state.lock().file_catalog.files, files);
	}
}
