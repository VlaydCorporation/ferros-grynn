//! Directory pages share source admission and the child commit's WAL boundary.

use super::{support::*, *};
use crate::storage::{
	graph_pages::{decode_subgraph_directory, encode_subgraph_directory},
	page::{AlignedPage, PageLsn},
	wal::{CommitFailureGuard, CommitFailurePoint},
};

/// A later untouched page must be validated even after useful pages were
/// prepared.
#[test]
fn bounded_child_directory_malformed_sources_publish_nothing() {
	let fixture = fixture(IoMode::Sync, 511);
	let store = local(&fixture.store);
	let parent_files = files(&fixture, fixture.parents[0].id());
	let (before, after, delta) = roots(&fixture, &[(0, 0)]);
	prepare(store, &before, &after, &delta).assert_ok();
	let originals = {
		let state = store.state.lock();
		(0..2)
			.map(|number| {
				store
					.read_update_page(
						&state,
						PageKey::new(
							parent_files.space(),
							parent_files.subgraphs,
							PageNo::new(number),
						),
					)
					.assert_ok()
			})
			.collect::<Vec<_>>()
	};
	for fault in 0..16 {
		let number = PageNo::new(u64::from(fault >= 13));
		let (mut next, mut entries) =
			decode_subgraph_directory(&originals[usize::from(fault >= 13)], number).assert_ok();
		let expected = match fault {
			0 => {
				next = Some(number);
				"contains a cycle"
			}
			1 => {
				next = Some(PageNo::new(2));
				"exceeds its ceiling"
			}
			2 => {
				next = None;
				"unreachable pages"
			}
			3 => {
				entries[1].slot = entries[0].slot;
				"duplicate slot"
			}
			4 => {
				entries[0].slot = 0;
				"reserved"
			}
			5 => {
				entries[0].slot = u32::MAX;
				"no scope binding"
			}
			6 => {
				entries.remove(0);
				"missing direct entries"
			}
			7 => {
				entries[0].generation += 1;
				"generation disagrees"
			}
			8 => {
				entries[0].node_capacity += 1;
				"counters disagree"
			}
			9 => {
				entries[0].edge_capacity += 1;
				"counters disagree"
			}
			10 => {
				entries[0].node_count += 1;
				"counters disagree"
			}
			11 => {
				entries[0].edge_count += 1;
				"counters disagree"
			}
			12 => {
				let state = store.state.lock();
				let binding = state
					.bindings
					.graphs
					.iter()
					.find(|binding| binding.graph_id == fixture.parents[0].id())
					.assert_some();
				entries[0].slot = *binding
					.arena
					.scope_by_slot()
					.iter()
					.find(|(_, scope)| scope.len() > 1)
					.assert_some()
					.0;
				"direct scope"
			}
			13 => {
				entries[0].edge_count += 1;
				"counters disagree"
			}
			14 => {
				next = Some(number);
				"contains a cycle"
			}
			_ => "counts are invalid",
		};
		let key = PageKey::new(parent_files.space(), parent_files.subgraphs, number);
		let mut page = encode_subgraph_directory(number, PageLsn::ZERO, next, &entries).assert_ok();
		if fault == 15 {
			// Format-v1 live_count at byte44 must agree with all mapped live
			// entries.
			let live = u32::try_from(entries.len() - 1).assert_ok();
			page.as_bytes_mut()[44..48].copy_from_slice(&live.to_le_bytes());
			page.seal().assert_ok();
		}
		stage_source(store, key, &page);
		let root = fixture.store.inner.current_root();
		let boundary = store.wal.snapshot().visible_lsn();
		let catalog = store.state.lock().file_catalog.files.clone();
		let generations = generation_directories(&fixture.directory.0);
		let result = prepare(store, &before, &after, &delta);
		assert!(result.is_err(), "PDIR05_LOWER_LIVE_COUNT_WAS_ACCEPTED: fault {fault}");
		let error = result.err().assert_some();
		if fault == 15 {
			assert!(
				matches!(error.status().status(), crate::gql_status::Status::InvalidStoragePage { page_no, reason } if *page_no == number.get() && reason.contains(expected)),
				"fault {fault}: {error}"
			);
		} else {
			assert!(
				matches!(error.status().status(), crate::gql_status::Status::InvalidStorageFile { reason } if reason.contains(expected)),
				"fault {fault}: {error}"
			);
		}
		assert!(Arc::ptr_eq(&root, &fixture.store.inner.current_root()));
		assert_eq!(store.wal.snapshot().visible_lsn(), boundary);
		assert_eq!(store.state.lock().file_catalog.files, catalog);
		assert_eq!(generation_directories(&fixture.directory.0), generations);
		stage_source(store, key, &originals[usize::from(fault >= 13)]);
		prepare(store, &before, &after, &delta).assert_ok();
	}
}

/// Valid source frames deliberately avoid logical/WAL clock equality in
/// direct-plan tests.
fn stage_source(store: &LocalStore, key: PageKey, page: &AlignedPage) {
	let mut tx = store.wal.begin_tx().assert_ok();
	tx.stage_page(key, page).assert_ok();
	store.wal.commit_tx(tx).assert_ok();
}

/// Sync known rejection and uncertain append cannot mix child and parent
/// results.
#[test]
fn bounded_child_directory_sync_failures_recover_complete_images() {
	failures(IoMode::Sync);
}

/// NativeAsync recovers the same complete child and every containing directory.
#[cfg(feature = "native-async-io")]
#[test]
fn bounded_child_directory_native_async_failures_recover_complete_images() {
	failures(IoMode::NativeAsync);
}

/// Existing guards exercise both preparation and the shared durable publication
/// barrier.
fn failures(io_mode: IoMode) {
	{
		let fixture = fixture(io_mode, 3);
		let store = local(&fixture.store);
		let root = fixture.store.inner.current_root();
		let boundary = store.wal.snapshot().visible_lsn();
		let catalog = store.state.lock().file_catalog.files.clone();
		let generations = generation_directories(&fixture.directory.0);
		for point in [
			GenerationFailurePoint::AfterBuild,
			GenerationFailurePoint::AfterFileSync,
			GenerationFailurePoint::AfterCatalogStage,
			GenerationFailurePoint::BeforeWalCommit,
		] {
			{
				let _failure = GenerationFailureGuard::set(point);
				candidate(&fixture, &[(0, 0)]).commit().assert_err();
			}
			assert!(Arc::ptr_eq(&root, &fixture.store.inner.current_root()));
			assert_eq!(store.wal.snapshot().visible_lsn(), boundary);
			assert_eq!(store.state.lock().file_catalog.files, catalog);
			assert_eq!(generation_directories(&fixture.directory.0), generations);
			assert_directories(&fixture);
		}
		candidate(&fixture, &[(0, 0)]).commit().assert_ok();
		assert_directories(&fixture);
	}
	for point in [
		CommitFailurePoint::BeforeAppend,
		CommitFailurePoint::AfterAppend,
		CommitFailurePoint::AfterFlush,
		CommitFailurePoint::AfterSync,
	] {
		let mut fixture = fixture(io_mode, 3);
		let root = fixture.store.inner.current_root();
		{
			let _failure = CommitFailureGuard::set(point);
			candidate(&fixture, &[(0, 0)]).commit().assert_err();
		}
		assert!(Arc::ptr_eq(&root, &fixture.store.inner.current_root()));
		let files_before = local(&fixture.store).state.lock().file_catalog.files.clone();
		candidate(&fixture, &[(0, 1)]).commit().assert_err();
		assert_eq!(local(&fixture.store).state.lock().file_catalog.files, files_before);
		drop(root);
		fixture = reopen(fixture);
		let root = fixture.store.inner.current_root();
		let accepted = point != CommitFailurePoint::BeforeAppend;
		assert_eq!(
			root.membership(
				fixture.children[0].id(),
				root.resolve_atom(&fixture.pending[0]).assert_some()
			)
			.is_some(),
			accepted
		);
		assert!(
			root.membership(
				fixture.children[0].id(),
				root.resolve_atom(&fixture.pending[1]).assert_some()
			)
			.is_none()
		);
		drop(root);
		assert_directories(&fixture);
		fixture.store.checkpoint(CheckpointMode::Restart).assert_ok();
		fixture = reopen(fixture);
		assert_directories(&fixture);
	}
}
