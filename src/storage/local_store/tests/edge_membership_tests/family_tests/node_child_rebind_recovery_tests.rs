//! NODE containment publishes its arena and changed directory as one commit.

use std::sync::Arc;

use super::{
	metagraph::with_replacement,
	node_child_rebind_tests::{assert_node_state, node_change, node_owner},
	*,
};
use crate::storage::wal::{CommitFailureGuard, CommitFailurePoint};

/// Sync restores complete old/final child identities and nonuniform counters.
#[test]
fn node_child_rebind_sync_failures_recover_complete_arena_and_directory() {
	failures(IoMode::Sync);
}

/// NativeAsync reaches the same known and uncertain publication boundaries.
#[cfg(feature = "native-async-io")]
#[test]
fn node_child_rebind_native_async_failures_recover_complete_arena_and_directory() {
	failures(IoMode::NativeAsync);
}

/// Three reached generation hooks reject before publication; retry then
/// succeeds.
fn known_failures(mode: IoMode, family: GraphKind) {
	let (fixture, _) = super::metagraph::container_fixture(mode, family);
	let (fixture, replacement) = with_replacement(fixture);
	let (global, owner, old_child) = node_owner(&fixture);
	let witness = metagraph::Witness::capture(&fixture);
	let root = fixture.store.inner.current_root();
	let store = local(&fixture.store);
	let files = store.state.lock().file_catalog.files.clone();
	let boundary = store.wal.snapshot().visible_lsn();
	let directories = super::super::super::generation_directory_paths(&fixture.directory.0);
	let source_arena = {
		let state = store.state.lock();
		state
			.bindings
			.graphs
			.iter()
			.find(|binding| binding.graph_id == fixture.graph.id())
			.assert_some()
			.arena
			.clone()
	};
	for point in [
		GenerationFailurePoint::AfterBuild,
		GenerationFailurePoint::AfterCatalogStage,
		GenerationFailurePoint::BeforeWalCommit,
	] {
		let error = {
			let _failure = GenerationFailureGuard::set(point);
			node_change(&fixture, replacement).commit().assert_err()
		};
		assert!(
			matches!(error.status().status(), crate::gql_status::Status::StorageIoFailure { operation, .. }
			if operation.as_str() == "injected generation publication failure")
		);
		assert!(Arc::ptr_eq(&root, &fixture.store.inner.current_root()));
		assert_eq!(store.wal.snapshot().visible_lsn(), boundary);
		let state = store.state.lock();
		assert_eq!(state.file_catalog.files, files);
		assert!(
			state
				.bindings
				.graphs
				.iter()
				.find(|binding| binding.graph_id == fixture.graph.id())
				.assert_some()
				.arena
				.same_owner(&source_arena)
		);
		assert!(
			state
				._database_catalog
				.graphs
				.iter()
				.find(|entry| entry.id == fixture.graph.id())
				.assert_some()
				.arena
				.same_owner(&source_arena)
		);
		drop(state);
		assert_eq!(
			super::super::super::generation_directory_paths(&fixture.directory.0),
			directories
		);
		assert_node_state(&fixture, global, owner, old_child);
		witness.assert_structure(&fixture);
		assert_bulk_image(&fixture);
	}
	node_change(&fixture, replacement).commit().assert_ok();
	assert_node_state(&fixture, global, owner, replacement);
	assert_retained(&fixture, &files, false);
	assert_bulk_image(&fixture);
}

/// Controlled WAL faults decide old/final authority only during cold recovery.
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
		let (global, owner, old_child) = node_owner(&fixture);
		let witness = metagraph::Witness::capture(&fixture);
		let root = fixture.store.inner.current_root();
		let store = local(&fixture.store);
		let (files, source_arena) = {
			let state = store.state.lock();
			(
				state.file_catalog.files.clone(),
				state
					.bindings
					.graphs
					.iter()
					.find(|binding| binding.graph_id == fixture.graph.id())
					.assert_some()
					.arena
					.clone(),
			)
		};
		let error = {
			let _failure = CommitFailureGuard::set(point);
			node_change(&fixture, replacement).commit().assert_err()
		};
		assert!(
			matches!(error.status().status(), crate::gql_status::Status::StorageIoFailure { operation, .. }
			if operation.as_str() == "injected WAL durability failure")
		);
		assert!(Arc::ptr_eq(&root, &fixture.store.inner.current_root()));
		let state = store.state.lock();
		assert_eq!(state.file_catalog.files, files);
		assert!(
			state
				.bindings
				.graphs
				.iter()
				.find(|binding| binding.graph_id == fixture.graph.id())
				.assert_some()
				.arena
				.same_owner(&source_arena)
		);
		assert!(
			state
				._database_catalog
				.graphs
				.iter()
				.find(|entry| entry.id == fixture.graph.id())
				.assert_some()
				.arena
				.same_owner(&source_arena)
		);
		drop(state);
		witness.assert_structure(&fixture);
		assert_node_state(&fixture, global, owner, old_child);
		assert_bulk_image(&fixture);
		let boundary = store.wal.snapshot().visible_lsn();
		let retry = node_change(&fixture, replacement).commit().assert_err();
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
		let child = if accepted {
			replacement
		} else {
			old_child
		};
		assert_eq!(fixture.store.inner.current_root().commit_ts, stamp + u64::from(accepted));
		assert_eq!(fixture.store.local_storage_status().assert_some().options().io_mode, mode);
		assert_node_state(&fixture, global, owner, child);
		assert_retained(&fixture, &files, false);
		// Cold reopen checked all four directory counters against opened
		// children. The bulk oracle independently checks the unchanged
		// Hot and role image.
		assert_bulk_image(&fixture);
	}
}
