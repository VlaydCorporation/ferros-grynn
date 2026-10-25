//! Selected endpoint publication preserves replay, budgets and recovery
//! boundaries.

use super::*;
use crate::storage::wal::{CommitFailureGuard, CommitFailurePoint};

/// Known failures clean unreachable trees; uncertain WAL chooses one whole
/// image.
#[test]
fn endpoint_selected_sync_failures_recover_complete_trees() {
	failures(IoMode::Sync);
}

/// NativeAsync exposes the same atomic physical tree after recovery.
#[cfg(feature = "native-async-io")]
#[test]
fn endpoint_selected_native_async_failures_recover_complete_trees() {
	failures(IoMode::NativeAsync);
}

/// A source capability is retained when the new subtree exceeds the caller
/// limit.
#[test]
fn endpoint_selected_sync_small_budget_preserves_source() {
	small_budget(IoMode::Sync);
}

/// NativeAsync rejection does not publish Hot, bindings or a partial tree.
#[cfg(feature = "native-async-io")]
#[test]
fn endpoint_selected_native_async_small_budget_preserves_source() {
	small_budget(IoMode::NativeAsync);
}

/// NODE-only and duplicate-role rewires retain an unchanged canonical endpoint
/// set.
#[test]
fn endpoint_selected_sync_same_postings_keep_tree_identity() {
	same_postings(IoMode::Sync);
}

/// The retained tree still receives a current revision in NativeAsync.
#[cfg(feature = "native-async-io")]
#[test]
fn endpoint_selected_native_async_same_postings_keep_tree_identity() {
	same_postings(IoMode::NativeAsync);
}

/// Late replay merges independent connector changes into one exact final tree.
#[test]
fn endpoint_selected_sync_late_writer_keeps_other_connector() {
	late_writer(IoMode::Sync);
}

/// NativeAsync selects postings from the accepted replay candidate.
#[cfg(feature = "native-async-io")]
#[test]
fn endpoint_selected_native_async_late_writer_keeps_other_connector() {
	late_writer(IoMode::NativeAsync);
}

/// Both local and global generation reuse retain old physical traversal owners.
#[test]
fn endpoint_selected_sync_generation_reuse_keeps_old_snapshot() {
	generation_reuse(IoMode::Sync);
}

/// Cold reopen resolves the reused generation through NativeAsync Hot pages.
#[cfg(feature = "native-async-io")]
#[test]
fn endpoint_selected_native_async_generation_reuse_keeps_old_snapshot() {
	generation_reuse(IoMode::NativeAsync);
}

/// Returns the prepared local identity so recovery assertions need no
/// prediction.
fn attachment(fixture: &Fixture, endpoint: AtomId) -> (Transaction, AtomId) {
	let mut write = fixture.store.begin();
	let added = write
		.attach_edge(
			fixture.graph.id(),
			&fixture.records[2],
			None,
			EdgeTopology::directed([endpoint], [fixture.nodes[0]]),
			None,
			None,
		)
		.assert_ok();
	(write, added)
}

/// Exact physical addresses remain independent of generation-aware runtime
/// handles.
fn edge_ref(edge: AtomId) -> u64 {
	DiskAtomRef::from_parts(AtomKind::Edge, edge.slot()).assert_ok().raw()
}

/// Literal rows describe both accepted branches of the failure matrix.
fn expected_rows(endpoint: AtomId, connector: AtomId, added: Option<AtomId>) -> Vec<Posting> {
	let mut rows = vec![(0, edge_ref(endpoint), edge_ref(connector), 0)];
	if let Some(added) = added {
		rows.push((0, edge_ref(endpoint), edge_ref(added), 0));
	}
	rows
}

/// Checks visibility and cleanup separately from uncertain durability outcomes.
fn failures(io_mode: IoMode) {
	{
		let (fixture, endpoint, connector) = endpoint_fixture(io_mode);
		let root = fixture.store.inner.current_root();
		let store = local(&fixture.store);
		let files = store.state.lock().file_catalog.files.clone();
		let directories = generation_directory_paths(&fixture.directory.0);
		let boundary = store.wal.snapshot().visible_lsn();
		for point in [
			GenerationFailurePoint::AfterBuild,
			GenerationFailurePoint::AfterFileSync,
			GenerationFailurePoint::AfterCatalogStage,
			GenerationFailurePoint::BeforeWalCommit,
		] {
			let error = {
				let _failure = GenerationFailureGuard::set(point);
				attachment(&fixture, endpoint).0.commit().assert_err()
			};
			assert!(matches!(error.status().status(),
                crate::gql_status::Status::StorageIoFailure { operation, .. }
                if operation.as_str() == "injected generation publication failure"));
			assert!(Arc::ptr_eq(&root, &fixture.store.inner.current_root()));
			assert_eq!(store.wal.snapshot().visible_lsn(), boundary);
			assert_eq!(store.state.lock().file_catalog.files, files);
			assert_eq!(generation_directory_paths(&fixture.directory.0), directories);
			assert_postings(&fixture, &expected_rows(endpoint, connector, None));
			assert_bulk_image(&fixture);
		}
		let (write, added) = attachment(&fixture, endpoint);
		write.commit().assert_ok();
		assert_data_files(&fixture, &files);
		assert_postings(&fixture, &expected_rows(endpoint, connector, Some(added)));
	}
	for point in [
		CommitFailurePoint::BeforeAppend,
		CommitFailurePoint::AfterAppend,
		CommitFailurePoint::AfterFlush,
		CommitFailurePoint::AfterSync,
	] {
		let (mut fixture, endpoint, connector) = endpoint_fixture(io_mode);
		let root = fixture.store.inner.current_root();
		let stamp = root.commit_ts;
		let files = local(&fixture.store).state.lock().file_catalog.files.clone();
		let (write, added) = attachment(&fixture, endpoint);
		let error = {
			let _failure = CommitFailureGuard::set(point);
			write.commit().assert_err()
		};
		assert!(matches!(error.status().status(),
			crate::gql_status::Status::StorageIoFailure { operation, .. }
			if operation.as_str() == "injected WAL durability failure"));
		assert!(Arc::ptr_eq(&root, &fixture.store.inner.current_root()));
		assert_eq!(local(&fixture.store).state.lock().file_catalog.files, files);
		assert_postings(&fixture, &expected_rows(endpoint, connector, None));
		drop(root);
		fixture = reopen(fixture);
		let accepted = point != CommitFailurePoint::BeforeAppend;
		assert_eq!(fixture.store.inner.current_root().commit_ts, stamp + u64::from(accepted));
		let rows = expected_rows(endpoint, connector, accepted.then_some(added));
		assert_postings(&fixture, &rows);
		assert_bulk_image(&fixture);
		assert_data_files(&fixture, &files);
		fixture.store.checkpoint(CheckpointMode::Restart).assert_ok();
		fixture = reopen(fixture);
		assert_postings(&fixture, &rows);
	}
}

/// Restore the original policy for a passing control, never raise its defaults.
fn small_budget(io_mode: IoMode) {
	let (mut fixture, endpoint, connector) = endpoint_fixture(io_mode);
	let normal = fixture.options.maintenance.max_memory_bytes;
	fixture.options.maintenance.max_memory_bytes =
		std::num::NonZeroUsize::new(1024 * 1024).assert_some();
	fixture = reopen(fixture);
	let root = fixture.store.inner.current_root();
	let files = local(&fixture.store).state.lock().file_catalog.files.clone();
	let directories = generation_directory_paths(&fixture.directory.0);
	let boundary = local(&fixture.store).wal.snapshot().visible_lsn();
	let error = attachment(&fixture, endpoint).0.commit().assert_err();
	assert!(matches!(
		error.status().status(),
		crate::gql_status::Status::StorageDecodeLimitExceeded { .. }
	));
	assert!(Arc::ptr_eq(&root, &fixture.store.inner.current_root()));
	assert_eq!(local(&fixture.store).wal.snapshot().visible_lsn(), boundary);
	assert_eq!(local(&fixture.store).state.lock().file_catalog.files, files);
	assert_eq!(generation_directory_paths(&fixture.directory.0), directories);
	assert_postings(&fixture, &expected_rows(endpoint, connector, None));
	drop(root);
	fixture.options.maintenance.max_memory_bytes = normal;
	fixture = reopen(fixture);
	let (write, added) = attachment(&fixture, endpoint);
	write.commit().assert_ok();
	assert_data_files(&fixture, &files);
	assert_postings(&fixture, &expected_rows(endpoint, connector, Some(added)));
	assert_bulk_image(&fixture);
}

/// Canonical postings ignore NODE endpoints, role order and duplicates per
/// side.
fn same_postings(io_mode: IoMode) {
	let (mut fixture, endpoint, connector) = endpoint_fixture(io_mode);
	let file = endpoint_file(&fixture);
	for (record, topology) in [
		(&fixture.records[0], EdgeTopology::directed([fixture.nodes[1]], [fixture.nodes[0]])),
		(
			&fixture.records[1],
			EdgeTopology::directed([endpoint, endpoint], [fixture.nodes[1], fixture.nodes[1]]),
		),
	] {
		let mut write = fixture.store.begin();
		write
			.replace_occurrence_state(
				fixture.graph.id(),
				record,
				OccurrenceState::edge(topology, None, None),
			)
			.assert_ok();
		write.commit().assert_ok();
		assert_eq!(endpoint_file(&fixture), file);
		assert_postings(&fixture, &expected_rows(endpoint, connector, None));
		assert_bulk_image(&fixture);
	}
	fixture.store.checkpoint(CheckpointMode::Restart).assert_ok();
	fixture = reopen(fixture);
	assert_eq!(endpoint_file(&fixture), file);
	assert_postings(&fixture, &expected_rows(endpoint, connector, None));
}

/// Reads the engine-owned endpoint binding, rather than an explicit class
/// index.
fn endpoint_file(fixture: &Fixture) -> crate::storage::types::DataFileId {
	local(&fixture.store)
		.state
		.lock()
		.bindings
		.graphs
		.iter()
		.find(|binding| binding.graph_id == fixture.graph.id())
		.assert_some()
		.edge_endpoint
		.assert_some()
		.file_id
}

/// The late transaction must rebuild from the accepted B root, not stale A
/// state.
fn late_writer(io_mode: IoMode) {
	let (mut fixture, endpoint, connector) = endpoint_fixture(io_mode);
	let files = local(&fixture.store).state.lock().file_catalog.files.clone();
	let (late, added) = attachment(&fixture, endpoint);
	let mut other = fixture.store.begin();
	other
		.replace_occurrence_state(
			fixture.graph.id(),
			&fixture.records[1],
			OccurrenceState::edge(
				EdgeTopology::directed([fixture.nodes[1]], [endpoint]),
				None,
				None,
			),
		)
		.assert_ok();
	other.commit().assert_ok();
	late.commit().assert_ok();
	let rows = [
		(0, edge_ref(endpoint), edge_ref(connector), 1),
		(0, edge_ref(endpoint), edge_ref(added), 0),
	];
	assert_postings(&fixture, &rows);
	assert_walk(&fixture, endpoint, &[(added, fixture.nodes[0])]);
	assert_data_files(&fixture, &files);
	assert_bulk_image(&fixture);
	fixture.store.checkpoint(CheckpointMode::Restart).assert_ok();
	fixture = reopen(fixture);
	assert_postings(&fixture, &rows);
}

/// Old snapshots retain old owners even when physical connector slots are
/// reused.
fn generation_reuse(io_mode: IoMode) {
	for global in [false, true] {
		let (mut fixture, endpoint, connector) = endpoint_fixture(io_mode);
		let (write, old_added) = attachment(&fixture, endpoint);
		write.commit().assert_ok();
		let old_global =
			fixture.store.inner.current_root().resolve_atom(&fixture.records[2]).assert_some();
		let mut old = fixture.store.begin();
		let snapshot = old.query_snapshot().assert_ok();
		let mut write = fixture.store.begin();
		if global {
			write.delete_atom_cascade(&fixture.records[2], true).assert_ok();
			write.create_atom(fixture.records[2].clone(), PropertyMap::new()).assert_ok();
		} else {
			write.detach_occurrence(fixture.graph.id(), &fixture.records[2], false).assert_ok();
		}
		let new_added = write
			.attach_edge(
				fixture.graph.id(),
				&fixture.records[2],
				None,
				EdgeTopology::directed([endpoint], [fixture.nodes[1]]),
				None,
				None,
			)
			.assert_ok();
		write.commit().assert_ok();
		let new_global =
			fixture.store.inner.current_root().resolve_atom(&fixture.records[2]).assert_some();
		if global {
			assert_eq!(old_global.slot(), new_global.slot());
			assert_eq!(old_global.generation() + 1, new_global.generation());
		} else {
			assert_eq!(old_global, new_global);
		}
		assert_eq!(old_added.slot(), new_added.slot());
		assert_ne!(old_added, new_added);
		let walk = old
			.query_physical_traversal(&snapshot, &fixture.graph, endpoint, true, false, true)
			.assert_ok()
			.assert_some();
		let mut actual =
			walk.candidates.iter().map(|step| (step.edge, step.target)).collect::<Vec<_>>();
		actual.sort();
		let mut expected = vec![(connector, fixture.nodes[1]), (old_added, fixture.nodes[0])];
		expected.sort();
		assert_eq!(actual, expected);
		assert_walk(&fixture, endpoint, &[
			(connector, fixture.nodes[1]),
			(new_added, fixture.nodes[1]),
		]);
		assert_postings(&fixture, &expected_rows(endpoint, connector, Some(new_added)));
		assert_bulk_image(&fixture);
		drop(snapshot);
		drop(old);
		fixture.store.checkpoint(CheckpointMode::Restart).assert_ok();
		fixture = reopen(fixture);
		assert_walk(&fixture, endpoint, &[
			(connector, fixture.nodes[1]),
			(new_added, fixture.nodes[1]),
		]);
	}
}
