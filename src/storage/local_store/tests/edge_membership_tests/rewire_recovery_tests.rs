//! Rewire replay, admission and WAL errors preserve complete accepted images.

use super::{
	rewire_tests::{assert_retained, assert_topology, write_topology},
	support::*,
	*,
};
use crate::storage::wal::{CommitFailureGuard, CommitFailurePoint};

/// Different EDGE writers must merge their shared NODE Hot page after replay.
#[test]
fn edge_rewire_sync_late_writer_preserves_both_topologies() {
	late_writer(IoMode::Sync);
}

/// NativeAsync uses accepted final roots rather than the stale private image.
#[cfg(feature = "native-async-io")]
#[test]
fn edge_rewire_native_async_late_writer_preserves_both_topologies() {
	late_writer(IoMode::NativeAsync);
}

/// Both endpoints are stable; only the two independently owned EDGE states
/// vary.
fn late_writer(io_mode: IoMode) {
	let mut fixture = fixture(io_mode, 2, 2);
	let files = local(&fixture.store).state.lock().file_catalog.files.clone();
	let root = fixture.store.inner.current_root();
	let ids: [(crate::datastore::GlobalAtomId, AtomId); 2] = std::array::from_fn(|index| {
		let global = root.resolve_atom(&fixture.records[index]).assert_some();
		(global, root.membership(fixture.graph.id(), global).assert_some())
	});
	drop(root);
	let [a, b] = fixture.nodes;
	let topologies = [EdgeTopology::directed([b], [a]), EdgeTopology::directed([b], [b])];
	let late = write_topology(&fixture, topologies[0].clone());
	let mut other = fixture.store.begin();
	other
		.replace_occurrence_state(
			fixture.graph.id(),
			&fixture.records[1],
			crate::datastore::OccurrenceState::edge(topologies[1].clone(), None, None),
		)
		.assert_ok();
	other.commit().assert_ok();
	late.commit().assert_ok();
	for (index, (global, edge)) in ids.iter().copied().enumerate() {
		assert_topology(&fixture, global, edge, &topologies[index]);
	}
	assert_retained(&fixture, &files, false);
	assert_bulk_image(&fixture);
	fixture.store.checkpoint(CheckpointMode::Restart).assert_ok();
	fixture = reopen(fixture);
	for (index, (global, edge)) in ids.iter().copied().enumerate() {
		assert_eq!(
			fixture.store.inner.current_root().resolve_atom(&fixture.records[index]),
			Some(global)
		);
		assert_topology(&fixture, global, edge, &topologies[index]);
	}
	assert_retained(&fixture, &files, false);
	assert_bulk_image(&fixture);
}

/// A rejected physical plan must leave root, WAL, files and generations intact.
#[test]
fn edge_rewire_sync_small_budget_preserves_source() {
	small_budget(IoMode::Sync);
}

/// NativeAsync admission has a passing control under restored production
/// limits.
#[cfg(feature = "native-async-io")]
#[test]
fn edge_rewire_native_async_small_budget_preserves_source() {
	small_budget(IoMode::NativeAsync);
}

/// This is a logical preparation budget, not an allocator or RSS measurement.
fn small_budget(io_mode: IoMode) {
	let mut fixture = fixture(io_mode, 1, 1);
	let default_limit = fixture.options.maintenance.max_memory_bytes;
	fixture.options.maintenance.max_memory_bytes =
		std::num::NonZeroUsize::new(32 * 1024).assert_some();
	fixture = reopen(fixture);
	let root = fixture.store.inner.current_root();
	let boundary = local(&fixture.store).wal.snapshot().visible_lsn();
	let files = local(&fixture.store).state.lock().file_catalog.files.clone();
	let directories = generation_directories(&fixture.directory.0);
	let global = root.resolve_atom(&fixture.records[0]).assert_some();
	let edge = root.membership(fixture.graph.id(), global).assert_some();
	let reversed = EdgeTopology::directed([fixture.nodes[1]], [fixture.nodes[0]]);
	let error = write_topology(&fixture, reversed.clone()).commit().assert_err();
	assert!(matches!(
		error.status().status(),
		crate::gql_status::Status::StorageDecodeLimitExceeded { .. }
	));
	assert!(std::sync::Arc::ptr_eq(&root, &fixture.store.inner.current_root()));
	assert_eq!(local(&fixture.store).wal.snapshot().visible_lsn(), boundary);
	assert_eq!(local(&fixture.store).state.lock().file_catalog.files, files);
	assert_eq!(generation_directories(&fixture.directory.0), directories);
	assert_bulk_image(&fixture);
	drop(root);
	fixture.options.maintenance.max_memory_bytes = default_limit;
	fixture = reopen(fixture);
	write_topology(&fixture, reversed.clone()).commit().assert_ok();
	assert_topology(&fixture, global, edge, &reversed);
	assert_retained(&fixture, &files, false);
	assert_bulk_image(&fixture);
	fixture.store.checkpoint(CheckpointMode::Restart).assert_ok();
	fixture = reopen(fixture);
	assert_topology(&fixture, global, edge, &reversed);
	assert_bulk_image(&fixture);
}

/// Preparation rejection and uncertain WAL completion have separate oracles.
#[test]
fn edge_rewire_sync_failures_recover_complete_images() {
	failures(IoMode::Sync);
}

/// NativeAsync recovery admits the same complete before/final topology.
#[cfg(feature = "native-async-io")]
#[test]
fn edge_rewire_native_async_failures_recover_complete_images() {
	failures(IoMode::NativeAsync);
}

/// Only a label-COW candidate reaches the file-sync failure hook.
fn failure_candidate(fixture: &Fixture, label: bool) -> Transaction {
	let mut write =
		write_topology(fixture, EdgeTopology::directed([fixture.nodes[1]], [fixture.nodes[0]]));
	if label {
		write
			.set_occurrence_label(fixture.graph.id(), &fixture.records[0], Some("R".into()))
			.assert_ok();
	}
	write
}

/// The source stays readable before reopen, even after an uncertain append.
fn failures(io_mode: IoMode) {
	{
		let fixture = fixture(io_mode, 1, 1);
		let root = fixture.store.inner.current_root();
		let global = root.resolve_atom(&fixture.records[0]).assert_some();
		let edge = root.membership(fixture.graph.id(), global).assert_some();
		let boundary = local(&fixture.store).wal.snapshot().visible_lsn();
		let files = local(&fixture.store).state.lock().file_catalog.files.clone();
		let directories = generation_directories(&fixture.directory.0);
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
				matches!(error.status().status(), crate::gql_status::Status::StorageIoFailure { operation, .. }
				if operation.as_str() == "injected generation publication failure")
			);
			assert!(std::sync::Arc::ptr_eq(&root, &fixture.store.inner.current_root()));
			assert_eq!(local(&fixture.store).wal.snapshot().visible_lsn(), boundary);
			assert_eq!(local(&fixture.store).state.lock().file_catalog.files, files);
			assert_eq!(generation_directories(&fixture.directory.0), directories);
			assert_bulk_image(&fixture);
		}
		failure_candidate(&fixture, false).commit().assert_ok();
		assert_topology(
			&fixture,
			global,
			edge,
			&EdgeTopology::directed([fixture.nodes[1]], [fixture.nodes[0]]),
		);
		assert_retained(&fixture, &files, false);
		assert_bulk_image(&fixture);
	}
	for point in [
		CommitFailurePoint::BeforeAppend,
		CommitFailurePoint::AfterAppend,
		CommitFailurePoint::AfterFlush,
		CommitFailurePoint::AfterSync,
	] {
		let mut fixture = fixture(io_mode, 1, 1);
		let root = fixture.store.inner.current_root();
		let stamp = root.commit_ts;
		let global = root.resolve_atom(&fixture.records[0]).assert_some();
		let edge = root.membership(fixture.graph.id(), global).assert_some();
		let files = local(&fixture.store).state.lock().file_catalog.files.clone();
		let error = {
			let _failure = CommitFailureGuard::set(point);
			failure_candidate(&fixture, false).commit().assert_err()
		};
		assert!(
			matches!(error.status().status(), crate::gql_status::Status::StorageIoFailure { operation, .. }
			if operation.as_str() == "injected WAL durability failure")
		);
		assert!(std::sync::Arc::ptr_eq(&root, &fixture.store.inner.current_root()));
		assert_eq!(local(&fixture.store).state.lock().file_catalog.files, files);
		assert_bulk_image(&fixture);
		drop(root);
		fixture = reopen(fixture);
		let accepted = point != CommitFailurePoint::BeforeAppend;
		assert_eq!(fixture.store.inner.current_root().commit_ts, stamp + u64::from(accepted));
		let [a, b] = fixture.nodes;
		let expected = if accepted {
			EdgeTopology::directed([b], [a])
		} else {
			EdgeTopology::directed([a], [b])
		};
		assert_topology(&fixture, global, edge, &expected);
		assert_retained(&fixture, &files, false);
		assert_bulk_image(&fixture);
		fixture.store.checkpoint(CheckpointMode::Restart).assert_ok();
		fixture = reopen(fixture);
		assert_topology(&fixture, global, edge, &expected);
		assert_bulk_image(&fixture);
	}
}

/// Individually valid DAG rewires must be checked again against the merged
/// root.
#[test]
fn edge_rewire_sync_late_merged_cycle_publishes_nothing() {
	late_merged_cycle(IoMode::Sync);
}

/// NativeAsync must reject a merged cycle before physical publication.
#[cfg(feature = "native-async-io")]
#[test]
fn edge_rewire_native_async_late_merged_cycle_publishes_nothing() {
	late_merged_cycle(IoMode::NativeAsync);
}

/// EDGE identities are disjoint; the topology invariant is the only rejection.
fn late_merged_cycle(io_mode: IoMode) {
	let (mut fixture, _) = super::family_tests::family_fixture(io_mode, GraphKind::Dag);
	let source = fixture.store.inner.current_root();
	let ids: [(crate::datastore::GlobalAtomId, AtomId); 2] = std::array::from_fn(|index| {
		let global = source.resolve_atom(&fixture.records[index]).assert_some();
		(global, source.membership(fixture.graph.id(), global).assert_some())
	});
	let c = source
		.membership(
			fixture.graph.id(),
			source.resolve_atom(&"node:c".parse().assert_ok()).assert_some(),
		)
		.assert_some();
	let [a, b] = fixture.nodes;
	let late = write_topology(&fixture, EdgeTopology::directed([c], [a]));
	let mut other = fixture.store.begin();
	other
		.replace_occurrence_state(
			fixture.graph.id(),
			&fixture.records[1],
			crate::datastore::OccurrenceState::edge(EdgeTopology::directed([a], [c]), None, None),
		)
		.assert_ok();
	other.commit().assert_ok();
	let accepted = fixture.store.inner.current_root();
	let boundary = local(&fixture.store).wal.snapshot().visible_lsn();
	let files = local(&fixture.store).state.lock().file_catalog.files.clone();
	let directories = generation_directories(&fixture.directory.0);
	let error = late.commit().assert_err();
	assert!(
		matches!(error.status().status(), crate::gql_status::Status::GraphConstraintViolation { reason, .. }
		if reason.contains("cycle"))
	);
	assert!(std::sync::Arc::ptr_eq(&accepted, &fixture.store.inner.current_root()));
	assert_eq!(local(&fixture.store).wal.snapshot().visible_lsn(), boundary);
	assert_eq!(local(&fixture.store).state.lock().file_catalog.files, files);
	assert_eq!(generation_directories(&fixture.directory.0), directories);
	let expected = [EdgeTopology::directed([a], [b]), EdgeTopology::directed([a], [c])];
	for (index, (global, edge)) in ids.iter().copied().enumerate() {
		assert_topology(&fixture, global, edge, &expected[index]);
	}
	assert_bulk_image(&fixture);
	drop(accepted);
	drop(source);
	fixture.store.checkpoint(CheckpointMode::Restart).assert_ok();
	fixture = reopen(fixture);
	for (index, (global, edge)) in ids.iter().copied().enumerate() {
		assert_eq!(
			fixture.store.inner.current_root().resolve_atom(&fixture.records[index]),
			Some(global)
		);
		assert_topology(&fixture, global, edge, &expected[index]);
	}
	assert_retained(&fixture, &files, false);
	assert_bulk_image(&fixture);
}

/// The unsupported bidirectional hyperedge must fail without a WAL publication.
#[test]
fn edge_rewire_sync_v1_bidirectional_hyperedge_publishes_nothing() {
	rejected_bidirectional_hyperedge(IoMode::Sync);
}

/// NativeAsync preserves the accepted binary source on the same format error.
#[cfg(feature = "native-async-io")]
#[test]
fn edge_rewire_native_async_v1_bidirectional_hyperedge_publishes_nothing() {
	rejected_bidirectional_hyperedge(IoMode::NativeAsync);
}

/// Disk specification section 11.1 fixes binary bidirectional endpoint counts.
fn rejected_bidirectional_hyperedge(io_mode: IoMode) {
	let mut fixture = fixture(io_mode, 1, 1);
	let root = fixture.store.inner.current_root();
	let global = root.resolve_atom(&fixture.records[0]).assert_some();
	let edge = root.membership(fixture.graph.id(), global).assert_some();
	let boundary = local(&fixture.store).wal.snapshot().visible_lsn();
	let files = local(&fixture.store).state.lock().file_catalog.files.clone();
	let directories = generation_directories(&fixture.directory.0);
	let [a, b] = fixture.nodes;
	let error = write_topology(&fixture, EdgeTopology::bidirectional([a, a, b], [b, a]))
		.commit()
		.assert_err();
	assert!(
		matches!(error.status().status(), crate::gql_status::Status::InvalidArgument { message }
		if message.as_str() == "bidirectional topology requires exactly two endpoints in format v1")
	);
	assert!(std::sync::Arc::ptr_eq(&root, &fixture.store.inner.current_root()));
	assert_eq!(local(&fixture.store).wal.snapshot().visible_lsn(), boundary);
	assert_eq!(local(&fixture.store).state.lock().file_catalog.files, files);
	assert_eq!(generation_directories(&fixture.directory.0), directories);
	let original = EdgeTopology::directed([a], [b]);
	assert_topology(&fixture, global, edge, &original);
	assert_bulk_image(&fixture);
	drop(root);
	fixture.store.checkpoint(CheckpointMode::Restart).assert_ok();
	fixture = reopen(fixture);
	assert_topology(&fixture, global, edge, &original);
	assert_retained(&fixture, &files, false);
	assert_bulk_image(&fixture);
}
