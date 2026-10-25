//! First capability failures retain absence; replay preserves independent
//! peers.

use super::{
	first_capability_tests::{Change, assert_first_image, first_fixture},
	*,
};
use crate::storage::wal::{CommitFailureGuard, CommitFailurePoint};

/// Known and uncertain failures cannot split capability from topology.
#[test]
fn endpoint_first_sync_failures_keep_capability_atomic() {
	failures(IoMode::Sync);
}

/// NativeAsync recovery selects the same complete first-capability image.
#[cfg(feature = "native-async-io")]
#[test]
fn endpoint_first_native_async_failures_keep_capability_atomic() {
	failures(IoMode::NativeAsync);
}

/// Resource rejection cannot publish a capability without its first tree.
#[test]
fn endpoint_first_sync_small_budget_keeps_capability_absent() {
	small_budget(IoMode::Sync);
}

/// NativeAsync restores the ordinary policy after an admitted rejection.
#[cfg(feature = "native-async-io")]
#[test]
fn endpoint_first_native_async_small_budget_keeps_capability_absent() {
	small_budget(IoMode::NativeAsync);
}

/// A private capability ID may precede a later accepted peer reservation.
#[test]
fn endpoint_first_sync_late_writer_preserves_independent_capability() {
	late_writer(IoMode::Sync);
}

/// NativeAsync retains the peer tree and advances its header in the same WAL.
#[cfg(feature = "native-async-io")]
#[test]
fn endpoint_first_native_async_late_writer_preserves_independent_capability() {
	late_writer(IoMode::NativeAsync);
}

/// Uses the existing Base connector without allocating a global atom.
fn attachment(fixture: &Fixture, endpoint: AtomId) -> (Transaction, AtomId) {
	let mut write = fixture.store.begin();
	let connector = write
		.attach_edge(
			fixture.graph.id(),
			&fixture.records[1],
			None,
			EdgeTopology::directed([endpoint], [fixture.nodes[1]]),
			None,
			None,
		)
		.assert_ok();
	(write, connector)
}

/// Both logical and accepted physical authorities must retain absence.
fn assert_absent(fixture: &Fixture) {
	let root = fixture.store.inner.current_root();
	assert!(root.schema.graph(fixture.graph.id()).assert_some().edge_endpoint_index().is_none());
	let state = local(&fixture.store).state.lock();
	assert!(
		state
			.bindings
			.graphs
			.iter()
			.find(|graph| graph.graph_id == fixture.graph.id())
			.assert_some()
			.edge_endpoint
			.is_none()
	);
	let catalog = state
		._database_catalog
		.graphs
		.iter()
		.find(|graph| graph.id == fixture.graph.id())
		.assert_some();
	assert!(catalog.edge_endpoint_index.is_none());
	assert!(catalog.edge_endpoint.is_none());
}

/// Repeated known aborts leave reservation gaps but no durable definition.
fn failures(io_mode: IoMode) {
	{
		let (fixture, endpoint, _) = first_fixture(io_mode, Change::Attach);
		let root = fixture.store.inner.current_root();
		let store = local(&fixture.store);
		let files = store.state.lock().file_catalog.files.clone();
		let paths = generation_directory_paths(&fixture.directory.0);
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
			assert!(
				matches!(error.status().status(), crate::gql_status::Status::StorageIoFailure { operation, .. } if operation.as_str() == "injected generation publication failure")
			);
			assert!(Arc::ptr_eq(&root, &fixture.store.inner.current_root()));
			assert_eq!(store.wal.snapshot().visible_lsn(), boundary);
			assert_eq!(store.state.lock().file_catalog.files, files);
			assert_eq!(generation_directory_paths(&fixture.directory.0), paths);
			assert_absent(&fixture);
			assert_bulk_image(&fixture);
		}
		let (write, connector) = attachment(&fixture, endpoint);
		write.commit().assert_ok();
		assert_data_files(&fixture, &files);
		assert_first_image(&fixture, endpoint, Some(connector));
	}
	for point in [
		CommitFailurePoint::BeforeAppend,
		CommitFailurePoint::AfterAppend,
		CommitFailurePoint::AfterFlush,
		CommitFailurePoint::AfterSync,
	] {
		let (mut fixture, endpoint, _) = first_fixture(io_mode, Change::Attach);
		let root = fixture.store.inner.current_root();
		let stamp = root.commit_ts;
		let files = local(&fixture.store).state.lock().file_catalog.files.clone();
		let (write, connector) = attachment(&fixture, endpoint);
		let error = {
			let _failure = CommitFailureGuard::set(point);
			write.commit().assert_err()
		};
		assert!(
			matches!(error.status().status(), crate::gql_status::Status::StorageIoFailure { operation, .. } if operation.as_str() == "injected WAL durability failure")
		);
		assert!(Arc::ptr_eq(&root, &fixture.store.inner.current_root()));
		assert_eq!(local(&fixture.store).state.lock().file_catalog.files, files);
		assert_absent(&fixture);
		assert_bulk_image(&fixture);
		drop(root);
		fixture = reopen(fixture);
		let accepted = point != CommitFailurePoint::BeforeAppend;
		assert_eq!(fixture.store.inner.current_root().commit_ts, stamp + u64::from(accepted));
		if accepted {
			assert_first_image(&fixture, endpoint, Some(connector));
		} else {
			assert_absent(&fixture);
			assert_bulk_image(&fixture);
		}
		assert_data_files(&fixture, &files);
		fixture.store.checkpoint(CheckpointMode::Restart).assert_ok();
		fixture = reopen(fixture);
		if accepted {
			assert_first_image(&fixture, endpoint, Some(connector));
		} else {
			assert_absent(&fixture);
			assert_bulk_image(&fixture);
		}
	}
}

/// A passing control restores the caller's original default, never raises it.
fn small_budget(io_mode: IoMode) {
	let (mut fixture, endpoint, _) = first_fixture(io_mode, Change::Attach);
	let normal = fixture.options.maintenance.max_memory_bytes;
	fixture.options.maintenance.max_memory_bytes =
		std::num::NonZeroUsize::new(1024 * 1024).assert_some();
	fixture = reopen(fixture);
	let root = fixture.store.inner.current_root();
	let files = local(&fixture.store).state.lock().file_catalog.files.clone();
	let paths = generation_directory_paths(&fixture.directory.0);
	let boundary = local(&fixture.store).wal.snapshot().visible_lsn();
	let error = attachment(&fixture, endpoint).0.commit().assert_err();
	assert!(matches!(
		error.status().status(),
		crate::gql_status::Status::StorageDecodeLimitExceeded { .. }
	));
	assert!(Arc::ptr_eq(&root, &fixture.store.inner.current_root()));
	assert_eq!(local(&fixture.store).wal.snapshot().visible_lsn(), boundary);
	assert_eq!(local(&fixture.store).state.lock().file_catalog.files, files);
	assert_eq!(generation_directory_paths(&fixture.directory.0), paths);
	assert_absent(&fixture);
	assert_bulk_image(&fixture);
	drop(root);
	fixture.options.maintenance.max_memory_bytes = normal;
	fixture = reopen(fixture);
	let (write, connector) = attachment(&fixture, endpoint);
	write.commit().assert_ok();
	assert_data_files(&fixture, &files);
	assert_first_image(&fixture, endpoint, Some(connector));
}

/// Independent graph additions exercise replay and the shared schema clock.
fn late_writer(io_mode: IoMode) {
	let (mut fixture, endpoint, _) = first_fixture(io_mode, Change::Attach);
	let source = fixture.store.inner.current_root();
	let other = source.schema.named_graph("other").assert_some().reference().clone();
	let node_a = "node:a".parse::<RecordId>().assert_ok();
	let node_b = "node:b".parse::<RecordId>().assert_ok();
	let global_a = source.resolve_atom(&node_a).assert_some();
	let other_a = source.membership(other.id(), global_a).assert_some();
	drop(source);
	let mut seed = fixture.store.begin();
	let other_b = seed.attach_node(other.id(), &node_b, None, None).assert_ok();
	let peer_endpoint = seed
		.attach_edge(
			other.id(),
			&fixture.records[2],
			None,
			EdgeTopology::directed([other_a], [other_b]),
			None,
			None,
		)
		.assert_ok();
	seed.commit().assert_ok();
	fixture.store.checkpoint(CheckpointMode::Restart).assert_ok();
	fixture = reopen(fixture);
	let (late, connector) = attachment(&fixture, endpoint);
	let mut peer = fixture.store.begin();
	let peer_connector = peer
		.attach_edge(
			other.id(),
			&fixture.records[1],
			None,
			EdgeTopology::directed([peer_endpoint], [other_b]),
			None,
			None,
		)
		.assert_ok();
	peer.commit().assert_ok();
	let accepted = fixture.store.inner.current_root();
	let peer_capability =
		accepted.schema.graph(other.id()).assert_some().edge_endpoint_index().assert_some();
	let peer_clock = accepted.schema.next_schema_id;
	let peer_binding = local(&fixture.store)
		.state
		.lock()
		.bindings
		.graphs
		.iter()
		.find(|graph| graph.graph_id == other.id())
		.assert_some()
		.edge_endpoint
		.assert_some();
	// The peer's own class array is already accepted before the late update.
	let files = local(&fixture.store).state.lock().file_catalog.files.clone();
	late.commit().assert_ok();
	let current = fixture.store.inner.current_root();
	let capability =
		current.schema.graph(fixture.graph.id()).assert_some().edge_endpoint_index().assert_some();
	assert!(capability.id().get() < peer_capability.id().get());
	assert!(u64::from(capability.id().get()) < peer_clock);
	assert_eq!(
		current.schema.graph(other.id()).assert_some().edge_endpoint_index(),
		Some(peer_capability)
	);
	assert_data_files(&fixture, &files);
	assert_first_image(&fixture, endpoint, Some(connector));
	let store = local(&fixture.store);
	let state = store.state.lock();
	assert_eq!(
		state
			.bindings
			.graphs
			.iter()
			.find(|graph| graph.graph_id == other.id())
			.assert_some()
			.edge_endpoint,
		Some(peer_binding)
	);
	let space = DataSpaceId::Graph(other.id());
	let page = store
		.read_update_page(&state, PageKey::new(space, peer_binding.file_id, peer_binding.root))
		.assert_ok();
	decode_header(
		&page,
		other.id(),
		peer_capability,
		current.commit_ts,
		store.visible_pages(&state, space, peer_binding.file_id).assert_ok(),
	)
	.assert_ok();
	drop(state);
	drop(current);
	drop(accepted);
	fixture.store.checkpoint(CheckpointMode::Restart).assert_ok();
	fixture = reopen(fixture);
	assert_data_files(&fixture, &files);
	assert_first_image(&fixture, endpoint, Some(connector));
	let mut read = fixture.store.begin();
	let snapshot = read.query_snapshot().assert_ok();
	let walk = read
		.query_physical_traversal(&snapshot, &other, peer_endpoint, true, false, false)
		.assert_ok()
		.assert_some();
	assert_eq!(walk.candidates.iter().map(|step| (step.edge, step.target)).collect::<Vec<_>>(), [
		(peer_connector, other_b)
	]);
}
