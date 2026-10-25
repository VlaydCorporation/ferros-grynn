//! Qualified preparations share ordinary resource, replay and durability
//! barriers.

use super::{
	qualified_guard_tests::{short_topology, wide_topology},
	transition_rewire_tests::{assert_state, portal, rewrite},
	*,
};
use crate::{
	datastore::OccurrenceState,
	storage::wal::{CommitFailureGuard, CommitFailurePoint},
};

/// Sync known and uncertain failures recover one complete qualified topology.
#[test]
fn transition_rewire_sync_failures_recover_complete_topologies() {
	failures(IoMode::Sync);
}

/// NativeAsync uses identical before/final recovery oracles.
#[cfg(feature = "native-async-io")]
#[test]
fn transition_rewire_native_async_failures_recover_complete_topologies() {
	failures(IoMode::NativeAsync);
}

/// Label COW reaches the file-sync hook; every other hook uses only incidence
/// work.
fn failure_candidate(fixture: &Fixture, label: bool) -> Transaction {
	let (_, _, child) = portal(fixture);
	let mut write =
		rewrite(fixture, OccurrenceState::edge(wide_topology(fixture), None, Some(child)));
	if label {
		write
			.set_occurrence_label(
				fixture.graph.id(),
				&"edge:portal".parse().assert_ok(),
				Some("Q".into()),
			)
			.assert_ok();
	}
	write
}

/// Known failures publish nothing; uncertain durability is decided by cold
/// recovery.
fn failures(mode: IoMode) {
	{
		let (fixture, _) = family_fixture(mode, GraphKind::Metagraph);
		let (_, _, child) = portal(&fixture);
		let witness = metagraph::Witness::capture(&fixture);
		rewrite(&fixture, OccurrenceState::edge(short_topology(&fixture), None, Some(child)))
			.commit()
			.assert_ok();
		let root = fixture.store.inner.current_root();
		let files = local(&fixture.store).state.lock().file_catalog.files.clone();
		let boundary = local(&fixture.store).wal.snapshot().visible_lsn();
		let directories = super::super::super::generation_directory_paths(&fixture.directory.0);
		for (point, label) in [
			(GenerationFailurePoint::AfterBuild, false),
			(GenerationFailurePoint::AfterCatalogStage, false),
			(GenerationFailurePoint::BeforeWalCommit, false),
			(GenerationFailurePoint::AfterFileSync, true),
		] {
			let error = {
				let _failure = GenerationFailureGuard::set(point);
				failure_candidate(&fixture, label).commit().assert_err()
			};
			assert!(
				matches!(error.status().status(), crate::gql_status::Status::StorageIoFailure { operation, .. } if operation.as_str() == "injected generation publication failure")
			);
			assert!(std::sync::Arc::ptr_eq(&root, &fixture.store.inner.current_root()));
			assert_eq!(local(&fixture.store).wal.snapshot().visible_lsn(), boundary);
			assert_eq!(local(&fixture.store).state.lock().file_catalog.files, files);
			assert_eq!(
				super::super::super::generation_directory_paths(&fixture.directory.0),
				directories
			);
			witness.assert_structure(&fixture);
			assert_bulk_image(&fixture);
		}
		failure_candidate(&fixture, false).commit().assert_ok();
		assert_retained(&fixture, &files, false);
		witness.assert_structure(&fixture);
		assert_bulk_image(&fixture);
	}
	for point in [
		CommitFailurePoint::BeforeAppend,
		CommitFailurePoint::AfterAppend,
		CommitFailurePoint::AfterFlush,
		CommitFailurePoint::AfterSync,
	] {
		let (mut fixture, _) = family_fixture(mode, GraphKind::Metagraph);
		let (global, edge, child) = portal(&fixture);
		let witness = metagraph::Witness::capture(&fixture);
		rewrite(&fixture, OccurrenceState::edge(short_topology(&fixture), None, Some(child)))
			.commit()
			.assert_ok();
		let root = fixture.store.inner.current_root();
		let files = local(&fixture.store).state.lock().file_catalog.files.clone();
		let error = {
			let _failure = CommitFailureGuard::set(point);
			failure_candidate(&fixture, false).commit().assert_err()
		};
		assert!(
			matches!(error.status().status(), crate::gql_status::Status::StorageIoFailure { operation, .. } if operation.as_str() == "injected WAL durability failure")
		);
		assert!(std::sync::Arc::ptr_eq(&root, &fixture.store.inner.current_root()));
		assert_eq!(local(&fixture.store).state.lock().file_catalog.files, files);
		witness.assert_structure(&fixture);
		assert_bulk_image(&fixture);
		let stamp = root.commit_ts;
		drop(root);
		fixture = reopen(fixture);
		let accepted = point != CommitFailurePoint::BeforeAppend;
		let topology = if accepted {
			wide_topology(&fixture)
		} else {
			short_topology(&fixture)
		};
		let current = fixture.store.inner.current_root();
		assert_state(
			&fixture,
			global,
			edge,
			&OccurrenceState::edge(topology.clone(), None, Some(child)),
		);
		assert_eq!(current.commit_ts, stamp + u64::from(accepted));
		assert_eq!(
			current.occurrence(fixture.graph.id(), edge).assert_some().state(),
			&OccurrenceState::edge(topology, None, Some(child))
		);
		assert_eq!(fixture.store.local_storage_status().assert_some().options().io_mode, mode);
		assert_retained(&fixture, &files, false);
		witness.assert_structure(&fixture);
		assert_bulk_image(&fixture);
	}
}

/// Sync budget rejection keeps root, files and WAL unchanged and has a passing
/// control.
#[test]
fn transition_rewire_sync_small_budget_preserves_source() {
	small_budget(IoMode::Sync);
}

/// NativeAsync uses the same unchanged production defaults for the passing
/// control.
#[cfg(feature = "native-async-io")]
#[test]
fn transition_rewire_native_async_small_budget_preserves_source() {
	small_budget(IoMode::NativeAsync);
}

/// This checks a logical preparation envelope, not an allocator or RSS quota.
fn small_budget(mode: IoMode) {
	let (mut fixture, _) = family_fixture(mode, GraphKind::Metagraph);
	let (global, edge, child) = portal(&fixture);
	let witness = metagraph::Witness::capture(&fixture);
	rewrite(&fixture, OccurrenceState::edge(short_topology(&fixture), None, Some(child)))
		.commit()
		.assert_ok();
	fixture.store.checkpoint(CheckpointMode::Restart).assert_ok();
	let normal = fixture.options.maintenance.max_memory_bytes;
	fixture.options.maintenance.max_memory_bytes =
		std::num::NonZeroUsize::new(32 * 1024).assert_some();
	fixture = reopen(fixture);
	let root = fixture.store.inner.current_root();
	let files = local(&fixture.store).state.lock().file_catalog.files.clone();
	let boundary = local(&fixture.store).wal.snapshot().visible_lsn();
	let directories = super::super::super::generation_directory_paths(&fixture.directory.0);
	let error =
		rewrite(&fixture, OccurrenceState::edge(wide_topology(&fixture), None, Some(child)))
			.commit()
			.assert_err();
	assert!(matches!(
		error.status().status(),
		crate::gql_status::Status::StorageDecodeLimitExceeded { .. }
	));
	assert!(std::sync::Arc::ptr_eq(&root, &fixture.store.inner.current_root()));
	assert_eq!(local(&fixture.store).wal.snapshot().visible_lsn(), boundary);
	assert_eq!(local(&fixture.store).state.lock().file_catalog.files, files);
	assert_eq!(super::super::super::generation_directory_paths(&fixture.directory.0), directories);
	witness.assert_structure(&fixture);
	assert_bulk_image(&fixture);
	drop(root);
	fixture.options.maintenance.max_memory_bytes = normal;
	fixture = reopen(fixture);
	rewrite(&fixture, OccurrenceState::edge(wide_topology(&fixture), None, Some(child)))
		.commit()
		.assert_ok();
	assert_retained(&fixture, &files, false);
	witness.assert_structure(&fixture);
	assert_bulk_image(&fixture);
	fixture.store.checkpoint(CheckpointMode::Restart).assert_ok();
	fixture = reopen(fixture);
	assert_eq!(fixture.store.local_storage_status().assert_some().options().io_mode, mode);
	assert_state(
		&fixture,
		global,
		edge,
		&OccurrenceState::edge(wide_topology(&fixture), None, Some(child)),
	);
	witness.assert_structure(&fixture);
	assert_bulk_image(&fixture);
}

/// Sync replays the portal after an independent accepted EDGE allocation.
#[test]
fn transition_rewire_sync_replay_preserves_independent_commit() {
	replay(IoMode::Sync);
}

/// NativeAsync preserves both final states and child bindings after cold
/// recovery.
#[cfg(feature = "native-async-io")]
#[test]
fn transition_rewire_native_async_replay_preserves_independent_commit() {
	replay(IoMode::NativeAsync);
}

/// Starting the late transaction before the independent commit forces final
/// replay.
fn replay(mode: IoMode) {
	let (mut fixture, _) = family_fixture(mode, GraphKind::Metagraph);
	let witness = metagraph::Witness::capture(&fixture);
	let (global, edge, child) = portal(&fixture);
	let expected = OccurrenceState::edge(wide_topology(&fixture), Some(7_i64.into()), Some(child));
	let files = local(&fixture.store).state.lock().file_catalog.files.clone();
	let late = rewrite(&fixture, expected.clone());
	let mut other = fixture.store.begin();
	let independent = other
		.attach_edge(
			fixture.graph.id(),
			&fixture.records[2],
			None,
			EdgeTopology::directed([fixture.nodes[1]], [fixture.nodes[0]]),
			None,
			None,
		)
		.assert_ok();
	other.commit().assert_ok();
	let accepted = fixture.store.inner.current_root();
	let independent_state =
		accepted.occurrence(fixture.graph.id(), independent).assert_some().clone();
	let stamp = accepted.commit_ts;
	late.commit().assert_ok();
	assert_eq!(fixture.store.inner.current_root().commit_ts, stamp + 1);
	assert_eq!(
		fixture.store.inner.current_root().occurrence(fixture.graph.id(), independent),
		Some(&independent_state)
	);
	assert_state(&fixture, global, edge, &expected);
	assert_retained(&fixture, &files, false);
	witness.assert_structure(&fixture);
	assert_bulk_image(&fixture);
	drop(accepted);
	fixture.store.checkpoint(CheckpointMode::Restart).assert_ok();
	fixture = reopen(fixture);
	assert_eq!(fixture.store.local_storage_status().assert_some().options().io_mode, mode);
	assert_state(&fixture, global, edge, &expected);
	assert_eq!(
		fixture.store.inner.current_root().occurrence(fixture.graph.id(), independent),
		Some(&independent_state)
	);
	assert_retained(&fixture, &files, false);
	witness.assert_structure(&fixture);
	assert_bulk_image(&fixture);
}
