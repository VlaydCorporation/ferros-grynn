//! Qualified preparations share ordinary resource, replay and durability
//! barriers.

use super::*;
use crate::storage::wal::{CommitFailureGuard, CommitFailurePoint};

/// Sync known and uncertain failures recover one complete qualified topology.
#[test]
fn qualified_sync_failures_recover_complete_topologies() {
	failures(IoMode::Sync);
}

/// NativeAsync uses identical before/final recovery oracles.
#[cfg(feature = "native-async-io")]
#[test]
fn qualified_native_async_failures_recover_complete_topologies() {
	failures(IoMode::NativeAsync);
}

/// Label COW reaches the file-sync hook; every other hook uses only incidence
/// work.
fn failure_candidate(fixture: &Fixture, label: bool) -> Transaction {
	let mut write = rewrite(fixture, wide_topology(fixture));
	if label {
		write
			.set_occurrence_label(fixture.graph.id(), &fixture.records[2], Some("Q".into()))
			.assert_ok();
	}
	write
}

/// Known failures publish nothing; uncertain durability is decided by cold
/// recovery.
fn failures(mode: IoMode) {
	{
		let fixture = qualified_fixture(mode);
		attachment(&fixture, short_topology(&fixture)).0.commit().assert_ok();
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
			assert_bulk_image(&fixture);
		}
		failure_candidate(&fixture, false).commit().assert_ok();
		assert_retained(&fixture, &files, false);
		assert_bulk_image(&fixture);
	}
	for point in [
		CommitFailurePoint::BeforeAppend,
		CommitFailurePoint::AfterAppend,
		CommitFailurePoint::AfterFlush,
		CommitFailurePoint::AfterSync,
	] {
		let mut fixture = qualified_fixture(mode);
		let (write, edge) = attachment(&fixture, short_topology(&fixture));
		write.commit().assert_ok();
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
		assert_eq!(current.commit_ts, stamp + u64::from(accepted));
		assert_eq!(
			current.occurrence(fixture.graph.id(), edge).assert_some().state(),
			&OccurrenceState::edge(topology, None, None)
		);
		assert_eq!(fixture.store.local_storage_status().assert_some().options().io_mode, mode);
		assert_retained(&fixture, &files, false);
		assert_bulk_image(&fixture);
	}
}

/// Sync budget rejection keeps root, files and WAL unchanged and has a passing
/// control.
#[test]
fn qualified_sync_small_budget_preserves_source() {
	small_budget(IoMode::Sync);
}

/// NativeAsync uses the same unchanged production defaults for the passing
/// control.
#[cfg(feature = "native-async-io")]
#[test]
fn qualified_native_async_small_budget_preserves_source() {
	small_budget(IoMode::NativeAsync);
}

/// This checks a logical preparation envelope, not an allocator or RSS quota.
fn small_budget(mode: IoMode) {
	let mut fixture = qualified_fixture(mode);
	attachment(&fixture, short_topology(&fixture)).0.commit().assert_ok();
	fixture.store.checkpoint(CheckpointMode::Restart).assert_ok();
	let normal = fixture.options.maintenance.max_memory_bytes;
	fixture.options.maintenance.max_memory_bytes =
		std::num::NonZeroUsize::new(32 * 1024).assert_some();
	fixture = reopen(fixture);
	let root = fixture.store.inner.current_root();
	let files = local(&fixture.store).state.lock().file_catalog.files.clone();
	let boundary = local(&fixture.store).wal.snapshot().visible_lsn();
	let directories = super::super::super::generation_directory_paths(&fixture.directory.0);
	let error = rewrite(&fixture, wide_topology(&fixture)).commit().assert_err();
	assert!(matches!(
		error.status().status(),
		crate::gql_status::Status::StorageDecodeLimitExceeded { .. }
	));
	assert!(std::sync::Arc::ptr_eq(&root, &fixture.store.inner.current_root()));
	assert_eq!(local(&fixture.store).wal.snapshot().visible_lsn(), boundary);
	assert_eq!(local(&fixture.store).state.lock().file_catalog.files, files);
	assert_eq!(super::super::super::generation_directory_paths(&fixture.directory.0), directories);
	assert_bulk_image(&fixture);
	drop(root);
	fixture.options.maintenance.max_memory_bytes = normal;
	fixture = reopen(fixture);
	rewrite(&fixture, wide_topology(&fixture)).commit().assert_ok();
	assert_retained(&fixture, &files, false);
	assert_bulk_image(&fixture);
	fixture.store.checkpoint(CheckpointMode::Restart).assert_ok();
	fixture = reopen(fixture);
	assert_eq!(fixture.store.local_storage_status().assert_some().options().io_mode, mode);
	assert_bulk_image(&fixture);
}

/// Sync replay merges an independent connector before local/global generation
/// reuse.
#[test]
fn qualified_sync_replay_reuse_and_transient_preserve_generations() {
	replay_reuse(IoMode::Sync);
}

/// NativeAsync recovers the final qualified topology after all allocator
/// transitions.
#[cfg(feature = "native-async-io")]
#[test]
fn qualified_native_async_replay_reuse_and_transient_preserve_generations() {
	replay_reuse(IoMode::NativeAsync);
}

/// The late provisional slot is deliberately consumed by an independent writer.
fn replay_reuse(mode: IoMode) {
	let mut fixture = qualified_fixture(mode);
	let files = local(&fixture.store).state.lock().file_catalog.files.clone();
	let (late, provisional) = attachment(&fixture, wide_topology(&fixture));
	let mut other = fixture.store.begin();
	let independent = other
		.attach_edge(
			fixture.graph.id(),
			&fixture.records[3],
			None,
			short_topology(&fixture),
			None,
			None,
		)
		.assert_ok();
	other.commit().assert_ok();
	assert_eq!(independent, provisional);
	late.commit().assert_ok();
	let root = fixture.store.inner.current_root();
	let global = root.resolve_atom(&fixture.records[2]).assert_some();
	let accepted = root.membership(fixture.graph.id(), global).assert_some();
	assert_eq!(accepted.slot(), provisional.slot() + 1);
	assert_eq!(
		root.occurrence(fixture.graph.id(), independent).assert_some().state(),
		&OccurrenceState::edge(short_topology(&fixture), None, None)
	);
	assert_eq!(
		root.occurrence(fixture.graph.id(), accepted).assert_some().state(),
		&OccurrenceState::edge(wide_topology(&fixture), None, None)
	);
	assert_retained(&fixture, &files, false);
	assert_bulk_image(&fixture);
	drop(root);
	for replace_global in [false, true] {
		let old = fixture.store.inner.current_root();
		let old_global = old.resolve_atom(&fixture.records[2]).assert_some();
		let old_edge = old.membership(fixture.graph.id(), old_global).assert_some();
		let mut write = fixture.store.begin();
		if replace_global {
			write.delete_atom_cascade(&fixture.records[2], true).assert_ok();
			write.create_atom(fixture.records[2].clone(), PropertyMap::new()).assert_ok();
		} else {
			write.detach_occurrence(fixture.graph.id(), &fixture.records[2], false).assert_ok();
		}
		let topology = if replace_global {
			wide_topology(&fixture)
		} else {
			short_topology(&fixture)
		};
		let new_edge = write
			.attach_edge(
				fixture.graph.id(),
				&fixture.records[2],
				None,
				topology.clone(),
				None,
				None,
			)
			.assert_ok();
		write.commit().assert_ok();
		let current = fixture.store.inner.current_root();
		let new_global = current.resolve_atom(&fixture.records[2]).assert_some();
		assert_eq!(new_global.slot(), old_global.slot());
		assert_eq!(new_global.generation(), old_global.generation() + u32::from(replace_global));
		assert_eq!(
			(new_edge.slot(), new_edge.generation()),
			(old_edge.slot(), old_edge.generation() + 1)
		);
		assert_eq!(current.membership(fixture.graph.id(), new_global), Some(new_edge));
		assert_eq!(
			current.occurrence(fixture.graph.id(), new_edge).assert_some().state(),
			&OccurrenceState::edge(topology, None, None)
		);
		assert_eq!(old.membership(fixture.graph.id(), old_global), Some(old_edge));
		assert!(old.occurrence(fixture.graph.id(), new_edge).is_none());
		assert_retained(&fixture, &files, false);
		assert_bulk_image(&fixture);
	}
	let root = fixture.store.inner.current_root();
	let capacity = root.graph(fixture.graph.id()).assert_some().slot_capacity(AtomKind::Edge);
	let base_capacity = root.atoms.slot_capacity();
	assert_eq!(root.atoms.atoms().len(), base_capacity);

	let mut transient = fixture.store.begin();
	let record = "edge:qualified_transient".parse::<RecordId>().assert_ok();
	transient.create_atom(record.clone(), PropertyMap::new()).assert_ok();
	let consumed = transient
		.attach_edge(fixture.graph.id(), &record, None, wide_topology(&fixture), None, None)
		.assert_ok();
	transient.delete_atom_cascade(&record, true).assert_ok();
	transient.commit().assert_ok();
	drop(root);
	for cold in [false, true] {
		if cold {
			fixture.store.checkpoint(CheckpointMode::Restart).assert_ok();
			fixture = reopen(fixture);
			assert_eq!(fixture.store.local_storage_status().assert_some().options().io_mode, mode);
		}
		let current = fixture.store.inner.current_root();
		assert!(current.resolve_atom(&record).is_none());
		assert!(current.occurrence(fixture.graph.id(), consumed).is_none());
		assert_eq!(current.atoms.slot_capacity(), base_capacity + 1);
		assert_eq!(current.atoms.slot_generation(base_capacity), Some(1));
		let graph = current.graph(fixture.graph.id()).assert_some();
		assert_eq!(graph.slot_capacity(AtomKind::Edge), capacity + 1);
		assert_eq!(
			graph.slot_generation(AtomKind::Edge, usize::try_from(consumed.slot()).assert_ok()),
			Some(consumed.generation() + 1)
		);
		assert_retained(&fixture, &files, false);
		assert_bulk_image(&fixture);
	}
}
