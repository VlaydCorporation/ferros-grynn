//! Presence edits share resource, final-replay and durable publication gates.

use std::sync::Arc;

use self::support::Case;
use super::{
	metagraph::{leaf_roots, prepare_leaf},
	*,
};
use crate::storage::{
	graph_index::with_external_sort_budget,
	graph_pages::GraphArenaBindings,
	page::AlignedPage,
	wal::{CommitFailureGuard, CommitFailurePoint},
};

/// Sync rejects a fixed logical envelope before changing source state.
#[test]
fn child_presence_sync_resource_rejection_preserves_complete_source() {
	resources(IoMode::Sync, Case::new);
}

/// NativeAsync uses unchanged defaults for the positive publication control.
#[cfg(feature = "native-async-io")]
#[test]
fn child_presence_native_async_resource_rejection_preserves_complete_source() {
	resources(IoMode::NativeAsync, Case::new);
}

/// Sync controlled failures recover either the complete old or final image.
#[test]
fn child_presence_sync_failures_recover_complete_image() {
	failures(IoMode::Sync, Case::new);
}

/// NativeAsync reaches all supported generation and WAL fault hooks.
#[cfg(feature = "native-async-io")]
#[test]
fn child_presence_native_async_failures_recover_complete_image() {
	failures(IoMode::NativeAsync, Case::new);
}

/// Sync final replay preserves an independently accepted EDGE occurrence.
#[test]
fn child_presence_sync_replay_and_wal_reopen_preserve_independent_commit() {
	replay(IoMode::Sync, Case::new);
}

/// NativeAsync cold reopen reads final WAL metadata without a checkpoint.
#[cfg(feature = "native-async-io")]
#[test]
fn child_presence_native_async_replay_and_wal_reopen_preserve_independent_commit() {
	replay(IoMode::NativeAsync, Case::new);
}

/// The new retained Hot/directory/header and WAL copies exceed this fixed
/// budget.
fn resources(mode: IoMode, make_case: fn(IoMode, AtomKind, bool) -> Case) {
	for kind in [AtomKind::Node, AtomKind::Edge] {
		for attach in [false, true] {
			let mut case = make_case(mode, kind, attach);
			case.check(false);
			let normal = case.fixture.options.maintenance.max_memory_bytes;
			// Commit installs the opened datastore policy, overriding caller
			// TLS.
			case.fixture.options.maintenance.max_memory_bytes =
				std::num::NonZeroUsize::new(128 * 1024).assert_some();
			case.fixture = reopen(case.fixture);
			assert_eq!(
				case.fixture
					.store
					.local_storage_status()
					.assert_some()
					.options()
					.maintenance
					.max_memory_bytes
					.get(),
				128 * 1024
			);

			let root = case.fixture.store.inner.current_root();
			let store = local(&case.fixture.store);
			let files = store.state.lock().file_catalog.files.clone();
			let boundary = store.wal.snapshot().visible_lsn();
			let directories =
				super::super::super::generation_directory_paths(&case.fixture.directory.0);
			let arena = case.arena();
			let pages = case.all_pages();
			let (before, after, delta) =
				leaf_roots(&case.fixture, case.owner.1, case.states.1.clone());
			prepare_leaf(store, (&before, &after), &delta).assert_ok();
			let error = with_external_sort_budget(128 * 1024, || {
				prepare_leaf(store, (&before, &after), &delta)
			})
			.err()
			.assert_some();
			assert!(matches!(
				error.status().status(),
				crate::gql_status::Status::StorageDecodeLimitExceeded { .. }
			));
			let error = case.change().commit().assert_err();
			assert!(matches!(
				error.status().status(),
				crate::gql_status::Status::StorageDecodeLimitExceeded { .. }
			));
			assert!(Arc::ptr_eq(&root, &case.fixture.store.inner.current_root()));
			assert_eq!(store.wal.snapshot().visible_lsn(), boundary);
			assert_eq!(store.state.lock().file_catalog.files, files);
			assert_eq!(
				super::super::super::generation_directory_paths(&case.fixture.directory.0),
				directories
			);
			case.assert_owners(&arena);
			case.assert_pages(&pages);
			case.check(false);
			drop(root);
			drop(before);
			drop(after);
			case.fixture.options.maintenance.max_memory_bytes = normal;
			case.fixture = reopen(case.fixture);
			assert_eq!(
				case.fixture
					.store
					.local_storage_status()
					.assert_some()
					.options()
					.maintenance
					.max_memory_bytes,
				normal
			);
			let (before, after, delta) =
				leaf_roots(&case.fixture, case.owner.1, case.states.1.clone());
			prepare_leaf(local(&case.fixture.store), (&before, &after), &delta).assert_ok();
			case.change().commit().assert_ok();
			case.check(true);
			assert_retained(&case.fixture, &files, false);
			drop(before);
			drop(after);
			case.fixture = reopen(case.fixture);
			assert_eq!(
				case.fixture.store.local_storage_status().assert_some().options().io_mode,
				mode
			);
			case.check(true);
			assert_retained(&case.fixture, &files, false);
		}
	}
}

/// Only hooks reached by retained-file publication count as failure evidence.
fn failures(mode: IoMode, make_case: fn(IoMode, AtomKind, bool) -> Case) {
	for kind in [AtomKind::Node, AtomKind::Edge] {
		for attach in [false, true] {
			let case = make_case(mode, kind, attach);
			let root = case.fixture.store.inner.current_root();
			let store = local(&case.fixture.store);
			let files = store.state.lock().file_catalog.files.clone();
			let arena = case.arena();
			let boundary = store.wal.snapshot().visible_lsn();
			let directories =
				super::super::super::generation_directory_paths(&case.fixture.directory.0);
			let pages = case.all_pages();
			for (fault, point) in [
				Some(GenerationFailurePoint::AfterBuild),
				case.qualified_edge.is_some().then_some(GenerationFailurePoint::AfterFileSync),
				Some(GenerationFailurePoint::AfterCatalogStage),
				Some(GenerationFailurePoint::BeforeWalCommit),
			]
			.into_iter()
			.flatten()
			.enumerate()
			{
				let error = {
					let _failure = GenerationFailureGuard::set(point);
					case.change().commit().assert_err()
				};
				assert!(
					matches!(error.status().status(), crate::gql_status::Status::StorageIoFailure { operation, .. }
					if operation.as_str() == "injected generation publication failure")
				);
				assert!(Arc::ptr_eq(&root, &case.fixture.store.inner.current_root()));
				assert_eq!(store.wal.snapshot().visible_lsn(), boundary);
				assert_eq!(store.state.lock().file_catalog.files, files);
				assert_eq!(
					super::super::super::generation_directory_paths(&case.fixture.directory.0),
					directories
				);
				case.assert_owners(&arena);
				case.assert_pages(&pages);
				case.check(false);
				if case.qualified_edge.is_some() {
					eprintln!(
						"QUALIFIED_EDGE_GENERATION_FAULT_PASS {mode:?} {kind:?} {attach} {fault}"
					);
				}
			}
			case.change().commit().assert_ok();
			case.check(true);
			assert_retained(&case.fixture, &files, false);
			drop(root);
			drop(case);
			for (fault, point) in [
				CommitFailurePoint::BeforeAppend,
				CommitFailurePoint::AfterAppend,
				CommitFailurePoint::AfterFlush,
				CommitFailurePoint::AfterSync,
			]
			.into_iter()
			.enumerate()
			{
				let mut case = make_case(mode, kind, attach);
				let root = case.fixture.store.inner.current_root();
				let stamp = root.commit_ts;
				let store = local(&case.fixture.store);
				let files = store.state.lock().file_catalog.files.clone();
				let arena = case.arena();
				let pages = case.all_pages();
				let old_endpoint = case
					.qualified_edge
					.as_ref()
					.map(|_| qualified_endpoint_tests::endpoint_path(&case));
				let directories =
					super::super::super::generation_directory_paths(&case.fixture.directory.0);
				let error = {
					let _failure = CommitFailureGuard::set(point);
					case.change().commit().assert_err()
				};
				assert!(
					matches!(error.status().status(), crate::gql_status::Status::StorageIoFailure { operation, .. }
					if operation.as_str() == "injected WAL durability failure")
				);
				assert!(Arc::ptr_eq(&root, &case.fixture.store.inner.current_root()));
				assert_eq!(store.state.lock().file_catalog.files, files);
				let pending =
					super::super::super::generation_directory_paths(&case.fixture.directory.0);
				if case.qualified_edge.is_some() {
					// The cleanup guard preserves uncertain output until
					// recovery decides.
					assert!(directories.is_subset(&pending));
					assert_eq!(pending.difference(&directories).count(), 1);
				} else {
					assert_eq!(pending, directories);
				}
				case.assert_owners(&arena);
				case.assert_pages(&pages);
				case.check(false);
				let boundary = store.wal.snapshot().visible_lsn();
				let retry = case.change().commit().assert_err();
				assert!(
					matches!(retry.status().status(), crate::gql_status::Status::StorageIoFailure { operation, .. }
					if operation.as_str() == "prepare publication with poisoned WAL; reopen to recover")
				);
				assert_eq!(store.wal.snapshot().visible_lsn(), boundary);
				assert!(Arc::ptr_eq(&root, &case.fixture.store.inner.current_root()));
				drop(root);
				case.fixture = reopen(case.fixture);
				let accepted = point != CommitFailurePoint::BeforeAppend;
				assert_eq!(
					case.fixture.store.inner.current_root().commit_ts,
					stamp + u64::from(accepted)
				);
				assert_eq!(
					case.fixture.store.local_storage_status().assert_some().options().io_mode,
					mode
				);
				case.check(accepted);
				assert_retained(&case.fixture, &files, false);
				if let Some(path) = old_endpoint {
					qualified_endpoint_tests::recovered(&case, &path, accepted);
					case.fixture.store.checkpoint(CheckpointMode::Restart).assert_ok();
					case.fixture = reopen(case.fixture);
					case.check(accepted);
					assert_retained(&case.fixture, &files, false);
					qualified_endpoint_tests::recovered(&case, &path, accepted);
					eprintln!(
						"QUALIFIED_EDGE_WAL_RECOVERY_PASS {mode:?} {kind:?} {attach} {fault}"
					);
				}
			}
		}
	}
}

/// The actual final candidate must include accepted independent structural DML.
fn replay(mode: IoMode, make_case: fn(IoMode, AtomKind, bool) -> Case) {
	for kind in [AtomKind::Node, AtomKind::Edge] {
		for attach in [false, true] {
			let mut case = make_case(mode, kind, attach);
			let record = case.fixture.records[if case.survivors.is_some() {
				3
			} else {
				2
			}]
			.clone();
			let source = case.fixture.store.inner.current_root();
			let global = source.resolve_atom(&record).assert_some();
			assert!(source.membership(case.fixture.graph.id(), global).is_none());
			drop(source);
			let late = case.change();
			let mut independent = case.fixture.store.begin();
			let other = independent
				.attach_edge(
					case.fixture.graph.id(),
					&record,
					None,
					EdgeTopology::directed([case.fixture.nodes[1]], [case.fixture.nodes[0]]),
					None,
					None,
				)
				.assert_ok();
			independent.commit().assert_ok();
			let root = case.fixture.store.inner.current_root();
			let occurrence = root.occurrence(case.fixture.graph.id(), other).assert_some().clone();
			let stamp = root.commit_ts;
			let files = local(&case.fixture.store).state.lock().file_catalog.files.clone();
			let arena = case.arena();
			let original = GraphArenaBindings::new(
				arena.graph_by_slot().clone(),
				arena.scope_by_slot().clone(),
			);
			late.commit().assert_ok();
			let current = case.fixture.store.inner.current_root();
			assert_eq!(current.commit_ts, stamp + 1);
			assert_eq!(current.occurrence(case.fixture.graph.id(), other), Some(&occurrence));
			assert!(!case.arena().same_owner(&arena));
			assert_eq!(arena, original);
			case.check(true);
			assert_retained(&case.fixture, &files, false);
			drop(current);
			drop(root);
			case.fixture = reopen(case.fixture);
			assert_eq!(
				case.fixture.store.local_storage_status().assert_some().options().io_mode,
				mode
			);
			assert_eq!(
				case.fixture.store.inner.current_root().occurrence(case.fixture.graph.id(), other),
				Some(&occurrence)
			);
			case.check(true);
			assert_retained(&case.fixture, &files, false);
		}
	}
}

/// Zero/one directory sources have no spare accepted slot for fault fixtures.
mod source_tests;

/// Accepted state and page capture belong to the test scenario.
mod support;

/// Sync keeps the selected owner and captured survivor through resources.
#[test]
fn child_scope_remap_sync_resource_rejection_preserves_all_source_pages() {
	resources(IoMode::Sync, Case::surviving);
}

/// Sync keeps the selected owner and captured survivor through failures.
#[test]
fn child_scope_remap_sync_failures_recover_complete_survivor_image() {
	failures(IoMode::Sync, Case::surviving);
}

/// Sync keeps the selected owner and captured survivor through replay.
#[test]
fn child_scope_remap_sync_replay_preserves_independent_commit_and_survivor() {
	replay(IoMode::Sync, Case::surviving);
}

/// NativeAsync keeps the selected owner and captured survivor through
/// resources.
#[cfg(feature = "native-async-io")]
#[test]
fn child_scope_remap_native_async_resource_rejection_preserves_all_source_pages() {
	resources(IoMode::NativeAsync, Case::surviving);
}

/// NativeAsync keeps the selected owner and captured survivor through failures.
#[cfg(feature = "native-async-io")]
#[test]
fn child_scope_remap_native_async_failures_recover_complete_survivor_image() {
	failures(IoMode::NativeAsync, Case::surviving);
}

/// NativeAsync keeps the selected owner and captured survivor through replay.
#[cfg(feature = "native-async-io")]
#[test]
fn child_scope_remap_native_async_replay_preserves_independent_commit_and_survivor() {
	replay(IoMode::NativeAsync, Case::surviving);
}

/// Validly encoded selected and survivor source faults share one publication
/// boundary.
mod survivor_source_tests;

/// Retained empty tails use accepted chain order in preparation.
mod extended_chain_tests;

/// Sync admits wide qualified controls and rejects the fixed resource envelope.
#[test]
fn qualified_child_scope_sync_resource_rejection_preserves_complete_source() {
	resources(IoMode::Sync, Case::qualified);
}

/// NativeAsync keeps the same resource policy through qualified cold reopen.
#[cfg(feature = "native-async-io")]
#[test]
fn qualified_child_scope_native_async_resource_rejection_preserves_complete_source() {
	resources(IoMode::NativeAsync, Case::qualified);
}

/// Sync generation/WAL faults preserve one complete qualified remap image.
#[test]
fn qualified_child_scope_sync_failures_recover_complete_image() {
	failures(IoMode::Sync, Case::qualified);
}

/// NativeAsync faults retain full old or final scope and role authority.
#[cfg(feature = "native-async-io")]
#[test]
fn qualified_child_scope_native_async_failures_recover_complete_image() {
	failures(IoMode::NativeAsync, Case::qualified);
}

/// Sync final replay preserves an independently accepted EDGE and wide roles.
#[test]
fn qualified_child_scope_sync_replay_preserves_independent_commit() {
	replay(IoMode::Sync, Case::qualified);
}

/// NativeAsync reopens the final replayed qualified image from WAL.
#[cfg(feature = "native-async-io")]
#[test]
fn qualified_child_scope_native_async_replay_preserves_independent_commit() {
	replay(IoMode::NativeAsync, Case::qualified);
}

/// Qualified-chain faults reject remap preparation before publication.
mod qualified_source_tests;

/// Coupled graph/index preparation, retained headers and recovery cleanup.
mod qualified_endpoint_tests;

/// Sync wide qualified EDGE resource preserves coupled publication boundaries.
#[test]
fn qualified_edge_child_scope_sync_resource_preserves_coupled_publication() {
	resources(IoMode::Sync, Case::qualified_edge);
	qualified_endpoint_tests::resources(IoMode::Sync);
}

/// Sync wide qualified EDGE failure preserves coupled publication boundaries.
#[test]
fn qualified_edge_child_scope_sync_failure_preserves_coupled_publication() {
	failures(IoMode::Sync, Case::qualified_edge);
}

/// Sync wide qualified EDGE replay preserves coupled publication boundaries.
#[test]
fn qualified_edge_child_scope_sync_replay_preserves_coupled_publication() {
	replay(IoMode::Sync, Case::qualified_edge);
}

/// NativeAsync wide qualified EDGE resource preserves coupled publication
/// boundaries.
#[cfg(feature = "native-async-io")]
#[test]
fn qualified_edge_child_scope_native_async_resource_preserves_coupled_publication() {
	resources(IoMode::NativeAsync, Case::qualified_edge);
	qualified_endpoint_tests::resources(IoMode::NativeAsync);
}

/// NativeAsync wide qualified EDGE failure preserves coupled publication
/// boundaries.
#[cfg(feature = "native-async-io")]
#[test]
fn qualified_edge_child_scope_native_async_failure_preserves_coupled_publication() {
	failures(IoMode::NativeAsync, Case::qualified_edge);
}

/// NativeAsync wide qualified EDGE replay preserves coupled publication
/// boundaries.
#[cfg(feature = "native-async-io")]
#[test]
fn qualified_edge_child_scope_native_async_replay_preserves_coupled_publication() {
	replay(IoMode::NativeAsync, Case::qualified_edge);
}

/// Graph owners reuse the same source and publication contracts.
mod graph_owner_tests;
