//! EDGE child arena and directory cross one known/uncertain publication
//! barrier.

use std::sync::Arc;

use super::{
	edge_child_rebind_support::{Source, edge_change},
	metagraph::with_replacement,
	*,
};
use crate::storage::wal::{CommitFailureGuard, CommitFailurePoint};

/// Sync generation/WAL failures preserve the complete old or final leaf image.
#[test]
fn edge_child_rebind_sync_failures_recover_complete_arena_and_directory() {
	failures(IoMode::Sync);
}

/// NativeAsync retains the same reached failure hooks and cold authority.
#[cfg(feature = "native-async-io")]
#[test]
fn edge_child_rebind_native_async_failures_recover_complete_arena_and_directory() {
	failures(IoMode::NativeAsync);
}

/// Every generation hook is reached without adding another mutation kind.
fn known_failures(mode: IoMode, family: GraphKind) {
	let (fixture, _) = super::metagraph::container_fixture(mode, family);
	let (fixture, replacement) = with_replacement(fixture);
	let source = Source::capture(&fixture);
	let root = fixture.store.inner.current_root();
	let store = local(&fixture.store);
	let boundary = store.wal.snapshot().visible_lsn();
	let directories = super::super::super::generation_directory_paths(&fixture.directory.0);
	for point in [
		GenerationFailurePoint::AfterBuild,
		GenerationFailurePoint::AfterCatalogStage,
		GenerationFailurePoint::BeforeWalCommit,
	] {
		let error = {
			let _failure = GenerationFailureGuard::set(point);
			edge_change(&fixture, replacement).commit().assert_err()
		};
		assert!(
			matches!(error.status().status(), crate::gql_status::Status::StorageIoFailure { operation, .. }
            if operation.as_str() == "injected generation publication failure")
		);
		assert!(Arc::ptr_eq(&root, &fixture.store.inner.current_root()));
		assert_eq!(store.wal.snapshot().visible_lsn(), boundary);
		assert_eq!(
			super::super::super::generation_directory_paths(&fixture.directory.0),
			directories
		);
		assert_accepted_owner(&fixture, &source);
		source.assert_current(&fixture, source.child, (1, 0, 1, 0));
		assert_bulk_image(&fixture);
	}
	edge_change(&fixture, replacement).commit().assert_ok();
	source.assert_current(&fixture, replacement, (2, 1, 2, 1));
	assert_bulk_image(&fixture);
}

/// Known private failures must not install even a value-equal arena owner.
fn assert_accepted_owner(fixture: &Fixture, source: &Source) {
	let store = local(&fixture.store);
	let state = store.state.lock();
	let binding = state
		.bindings
		.graphs
		.iter()
		.find(|binding| binding.graph_id == fixture.graph.id())
		.assert_some();
	let catalog = state
		._database_catalog
		.graphs
		.iter()
		.find(|graph| graph.id == fixture.graph.id())
		.assert_some();
	assert!(binding.arena.same_owner(&source.arena));
	assert!(catalog.arena.same_owner(&source.arena));
}

/// A poisoned writer cannot retry; reopen decides complete old/final authority.
fn failures(mode: IoMode) {
	failures_with_family(mode, GraphKind::Metagraph);
}

/// Runs the same fixture contract for the selected public parent family.
pub(super) fn failures_with_family(mode: IoMode, family: GraphKind) {
	known_failures(mode, family);
	for point in [
		CommitFailurePoint::BeforeAppend,
		CommitFailurePoint::AfterAppend,
		CommitFailurePoint::AfterFlush,
		CommitFailurePoint::AfterSync,
	] {
		let (fixture, _) = super::metagraph::container_fixture(mode, family);
		let (mut fixture, replacement) = with_replacement(fixture);
		let source = Source::capture(&fixture);
		let root = fixture.store.inner.current_root();
		let store = local(&fixture.store);
		let error = {
			let _failure = CommitFailureGuard::set(point);
			edge_change(&fixture, replacement).commit().assert_err()
		};
		assert!(
			matches!(error.status().status(), crate::gql_status::Status::StorageIoFailure { operation, .. }
            if operation.as_str() == "injected WAL durability failure")
		);
		assert!(Arc::ptr_eq(&root, &fixture.store.inner.current_root()));
		assert_accepted_owner(&fixture, &source);
		source.assert_current(&fixture, source.child, (1, 0, 1, 0));
		assert_bulk_image(&fixture);
		let boundary = store.wal.snapshot().visible_lsn();
		let retry = edge_change(&fixture, replacement).commit().assert_err();
		assert!(
			matches!(retry.status().status(), crate::gql_status::Status::StorageIoFailure { operation, .. }
            if operation.as_str() == "prepare publication with poisoned WAL; reopen to recover")
		);
		assert_eq!(store.wal.snapshot().visible_lsn(), boundary);
		assert!(Arc::ptr_eq(&root, &fixture.store.inner.current_root()));
		let stamp = root.commit_ts;
		drop(root);
		fixture = reopen(fixture);
		let accepted = point != CommitFailurePoint::BeforeAppend;
		let (child, counts) = if accepted {
			(replacement, (2, 1, 2, 1))
		} else {
			(source.child, (1, 0, 1, 0))
		};
		assert_eq!(fixture.store.inner.current_root().commit_ts, stamp + u64::from(accepted));
		assert_eq!(fixture.store.local_storage_status().assert_some().options().io_mode, mode);
		source.assert_current(&fixture, child, counts);
		assert_bulk_image(&fixture);
	}
}
