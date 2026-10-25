//! EDGE publication shares the ordinary atomic WAL and resource boundaries.

use super::{support::*, *};
use crate::storage::{
	graph_pages::{decode_adjacency_overflow, encode_adjacency_overflow_into},
	page::{PageLsn, PageNo},
	types::PageKey,
	wal::{CommitFailureGuard, CommitFailurePoint},
};

/// Sync source/index/catalog rejection and uncertain append recover atomically.
#[test]
fn edge_membership_sync_failures_recover_complete_images() {
	failures(IoMode::Sync);
}

/// NativeAsync preserves the same accepted/uncertain recovery outcomes.
#[cfg(feature = "native-async-io")]
#[test]
fn edge_membership_native_async_failures_recover_complete_images() {
	failures(IoMode::NativeAsync);
}

/// A source-relative sparse graph update cannot publish after admission
/// rejects.
#[test]
fn edge_membership_small_budget_preserves_root_wal_and_files() {
	let mut fixture = fixture(IoMode::Sync, 1, 0);
	fixture.options.maintenance.max_memory_bytes = NonZeroUsize::new(32 * 1024).assert_some();
	fixture = reopen(fixture);
	let store = local(&fixture.store);
	let stamp = fixture.store.inner.current_root().commit_ts;
	let boundary = store.wal.snapshot().visible_lsn();
	let files = store.state.lock().file_catalog.files.clone();
	let directories = generation_directories(&fixture.directory.0);
	let error = candidate(&fixture, 0).commit().assert_err();
	assert!(matches!(
		error.status().status(),
		crate::gql_status::Status::StorageDecodeLimitExceeded { .. }
	));
	assert_eq!(fixture.store.inner.current_root().commit_ts, stamp);
	assert_eq!(store.wal.snapshot().visible_lsn(), boundary);
	assert_eq!(store.state.lock().file_catalog.files, files);
	assert_eq!(generation_directories(&fixture.directory.0), directories);
	assert_bulk_image(&fixture);
}

/// Independent edges share a NODE Hot page but must merge final replay
/// adjacency.
#[test]
fn edge_membership_late_writer_preserves_independent_edge_and_adjacency() {
	for io_mode in [IoMode::Sync, IoMode::NativeAsync] {
		if io_mode == IoMode::NativeAsync && !cfg!(feature = "native-async-io") {
			continue;
		}
		let mut fixture = fixture(io_mode, 2, 0);
		let late = candidate(&fixture, 0);
		candidate(&fixture, 1).commit().assert_ok();
		late.commit().assert_ok();
		assert_bulk_image(&fixture);
		fixture.store.checkpoint(CheckpointMode::Restart).assert_ok();
		fixture = reopen(fixture);
		assert_bulk_image(&fixture);
		let root = fixture.store.inner.current_root();
		let local = root
			.membership(fixture.graph.id(), root.resolve_atom(&fixture.records[0]).assert_some())
			.assert_some();
		assert_eq!((local.slot(), local.generation()), (1, 0));
	}
}

/// Checksummed bad chains cannot be treated as query fallback during
/// publication.
#[test]
fn edge_membership_invalid_adjacency_sources_publish_nothing() {
	use std::collections::BTreeSet;

	use crate::{
		datastore::PublicationDelta,
		storage::local_store::{
			point_graph::GraphUpdatePlan,
			point_incidence,
			point_namespace::LabelPlan,
		},
	};
	for fault in 0..5 {
		let fixture = fixture(IoMode::Sync, 8, 7);
		let store = local(&fixture.store);
		let mut before = fixture.store.inner.current_root().as_ref().clone();
		store.hydrate_statistics(&mut before).assert_ok();
		let mut after = before.clone();
		let global = before.resolve_atom(&fixture.records[7]).assert_some();
		let edge = after
			.attach_edge(
				fixture.graph.id(),
				global,
				None,
				EdgeTopology::directed([fixture.nodes[0]], [fixture.nodes[1]]),
				None,
				None,
			)
			.assert_ok();
		let delta = PublicationDelta {
			records: BTreeSet::from([fixture.records[7].clone()]),
			global_slots: BTreeSet::new(),
			overlays: BTreeSet::new(),
			incidence: BTreeSet::from([(fixture.graph.id(), edge)]),
			memberships: BTreeSet::from([(fixture.graph.id(), global)]),
			point_graph_only: true,
		};
		let state = store.state.lock();
		assert!(point_incidence::eligible(&before, &after, &delta));
		let mut control = LabelPlan::build(store, &state, &before, &after, &delta).assert_ok();
		let mut context = crate::storage::atom_store_image::value_context(
			&after.schema,
			after.dictionary.as_ref().clone(),
		)
		.assert_ok();
		GraphUpdatePlan::build(store, &state, &before, &after, &delta, &mut control, &mut context)
			.assert_ok();
		let files = state
			.bindings
			.graphs
			.iter()
			.find(|binding| binding.graph_id == fixture.graph.id())
			.assert_some()
			.files;
		let key = PageKey::new(files.space(), files.adjacency, PageNo::new(0));
		let mut page = store.read_update_page(&state, key).assert_ok();
		let (_, mut entries) = decode_adjacency_overflow(&page, key.page_no()).assert_ok();
		let next = match fault {
			0 => Some(key.page_no()),
			1 => Some(PageNo::new(
				state
					.graph_superblocks
					.get(&fixture.graph.id())
					.assert_some()
					.adjacency_overflow_pages,
			)),
			2 => {
				entries.truncate(1);
				None
			}
			3 => {
				entries[0] = crate::values::storage::DiskAtomRef::from_parts(AtomKind::Edge, 9_999)
					.assert_ok();
				None
			}
			_ => {
				entries.swap(0, 1);
				None
			}
		};
		encode_adjacency_overflow_into(&mut page, key.page_no(), PageLsn::ZERO, next, &entries)
			.assert_ok();
		// A test-only valid WAL frame bypasses the logical root to model
		// corrupt input.
		let mut tx = store.wal.begin_tx().assert_ok();
		tx.stage_page(key, &page).assert_ok();
		store.wal.commit_tx(tx).assert_ok();
		let files_before = state.file_catalog.files.clone();
		let stamp = fixture.store.inner.current_root().commit_ts;
		let boundary = store.wal.snapshot().visible_lsn();
		let mut labels = LabelPlan::build(store, &state, &before, &after, &delta).assert_ok();
		// Direct preparation avoids the unrelated logical/WAL clock mismatch
		// deliberately introduced by the test-only malformed frame above.
		let error = GraphUpdatePlan::build(
			store,
			&state,
			&before,
			&after,
			&delta,
			&mut labels,
			&mut context,
		)
		.err()
		.assert_some();
		let expected = match fault {
			0 => "contains a cycle",
			1 => "exceeds its ceiling",
			2 => "shorter than its declared count",
			3 => "EDGE slot exceeds allocator",
			_ => "not ordered by generation-aware identity",
		};
		assert!(matches!(
			error.status().status(),
			crate::gql_status::Status::InvalidStorageFile { reason } if reason.contains(expected)
		));
		assert_eq!(fixture.store.inner.current_root().commit_ts, stamp);
		assert_eq!(store.wal.snapshot().visible_lsn(), boundary);
		assert_eq!(state.file_catalog.files, files_before);
	}
}

/// Failure guards distinguish a known old result from recovery of complete WAL.
fn failures(io_mode: IoMode) {
	{
		let fixture = fixture(io_mode, 1, 0);
		let store = local(&fixture.store);
		let stamp = fixture.store.inner.current_root().commit_ts;
		let boundary = store.wal.snapshot().visible_lsn();
		let files = store.state.lock().file_catalog.files.clone();
		let directories = generation_directories(&fixture.directory.0);
		for point in [
			GenerationFailurePoint::AfterBuild,
			GenerationFailurePoint::AfterFileSync,
			GenerationFailurePoint::AfterCatalogStage,
			GenerationFailurePoint::BeforeWalCommit,
		] {
			{
				let _failure = GenerationFailureGuard::set(point);
				candidate(&fixture, 0).commit().assert_err();
			}
			assert_eq!(fixture.store.inner.current_root().commit_ts, stamp);
			assert_eq!(store.wal.snapshot().visible_lsn(), boundary);
			assert_eq!(store.state.lock().file_catalog.files, files);
			assert_eq!(generation_directories(&fixture.directory.0), directories);
			assert_bulk_image(&fixture);
		}
		candidate(&fixture, 0).commit().assert_ok();
		assert_bulk_image(&fixture);
	}
	for point in [
		CommitFailurePoint::BeforeAppend,
		CommitFailurePoint::AfterAppend,
		CommitFailurePoint::AfterFlush,
		CommitFailurePoint::AfterSync,
	] {
		let mut fixture = fixture(io_mode, 1, 0);
		let stamp = fixture.store.inner.current_root().commit_ts;
		let files = local(&fixture.store).state.lock().file_catalog.files.clone();
		{
			let _failure = CommitFailureGuard::set(point);
			candidate(&fixture, 0).commit().assert_err();
		}
		assert_eq!(fixture.store.inner.current_root().commit_ts, stamp);
		assert_eq!(local(&fixture.store).state.lock().file_catalog.files, files);
		assert_bulk_image(&fixture);
		fixture = reopen(fixture);
		let accepted = point != CommitFailurePoint::BeforeAppend;
		assert_eq!(fixture.store.inner.current_root().commit_ts, stamp + u64::from(accepted));
		assert_eq!(
			fixture
				.store
				.inner
				.current_root()
				.statistics
				.graph(fixture.graph.id())
				.assert_some()
				.edge_count,
			u64::from(accepted)
		);
		assert_bulk_image(&fixture);
		fixture.store.checkpoint(CheckpointMode::Restart).assert_ok();
		fixture = reopen(fixture);
		assert_bulk_image(&fixture);
	}
}
