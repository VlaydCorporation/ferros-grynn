//! Sparse namespace publication across transitions and the WAL uncertainty
//! fence.

use super::*;
use crate::datastore::Transaction;

/// Sync covers two namespaces, same-ID rename and empty/nonempty transitions.
#[test]
fn label_namespace_sync_transitions_preserve_files_and_reopen() {
	namespace_transitions(IoMode::Sync);
}

/// Actual NativeAsync must satisfy the same physical identity contract.
#[cfg(feature = "native-async-io")]
#[test]
fn label_namespace_native_async_transitions_preserve_files_and_reopen() {
	namespace_transitions(IoMode::NativeAsync);
}

/// Sync faults leave the complete old or recovered new namespace reachable.
#[test]
fn label_namespace_sync_failures_recover_atomic_metadata() {
	namespace_failures(IoMode::Sync);
}

/// NativeAsync keeps metadata and selected arrays under the same barrier.
#[cfg(feature = "native-async-io")]
#[test]
fn label_namespace_native_async_failures_recover_atomic_metadata() {
	namespace_failures(IoMode::NativeAsync);
}

/// Combines leading names with effective-property changes and an edge weight.
fn namespace_candidate(fixture: &LabelFixture) -> Transaction {
	let graph = fixture.graph.id();
	let mut write = fixture.store.begin();
	for offset in [0, 3] {
		write.set_occurrence_label(graph, &fixture.records[offset], Some("A".into())).assert_ok();
	}
	write.set_overlay(graph, &fixture.records[0], fixture.keys[0], 100.into()).assert_ok();
	write.hide_property(graph, &fixture.records[2], fixture.keys[1]).assert_ok();
	write
		.replace_occurrence_state(
			graph,
			&fixture.records[3],
			OccurrenceState::edge(
				EdgeTopology::directed([fixture.nodes[0]], [fixture.nodes[1]]),
				Some(3_i64.into()),
				None,
			),
		)
		.assert_ok();
	write
}

/// Namespace changes preserve all files outside the selected metadata/index
/// scope.
pub(super) fn assert_namespace_files(fixture: &LabelFixture, before: &[FileCatalogEntry]) {
	let local = local_store(&fixture.store);
	let state = local.state.lock();
	for entry in before {
		let selected = entry.space_id == DataSpaceId::Graph(fixture.graph.id())
			&& matches!(
				entry.file_id.kind(),
				Some(
					DataFileKind::Labels
						| DataFileKind::Statistics
						| DataFileKind::LabelStatistics
						| DataFileKind::Index
				)
			);
		if !selected {
			assert!(
				state.file_catalog.files.contains(entry),
				"unrelated namespace file changed: {:?}",
				entry.path
			);
		}
	}
}

/// Checks logical values and physical arrays independently after
/// failure/reopen.
fn assert_namespace_image(fixture: &LabelFixture, published: bool) {
	let root = fixture.store.inner.current_root();
	let labels = if published {
		["A", "N", "M", "A", "M"]
	} else {
		["N", "N", "M", "N", "M"]
	};
	for (record, expected) in fixture.records.iter().zip(labels) {
		let global = root.resolve_atom(record).assert_some();
		let local = root.membership(fixture.graph.id(), global).assert_some();
		assert_eq!(
			root.occurrence(fixture.graph.id(), local)
				.assert_some()
				.label()
				.map(|label| label.as_str()),
			Some(expected)
		);
	}
	if published {
		assert_overlay_label_statistic(
			&fixture.store,
			fixture.graph.id(),
			AtomKind::Node,
			"A",
			1,
			2,
		);
		assert_overlay_label_statistic(
			&fixture.store,
			fixture.graph.id(),
			AtomKind::Node,
			"M",
			1,
			1,
		);
	}
	assert_overlay_property(
		&fixture.store,
		fixture.graph.id(),
		&fixture.records[0],
		fixture.keys[0],
		if published {
			100_i64.into()
		} else {
			0_i64.into()
		},
	);
	let mut read = fixture.store.begin();
	assert_eq!(
		read.effective_property(fixture.graph.id(), &fixture.records[2], fixture.keys[1])
			.assert_ok(),
		if published {
			None
		} else {
			Some("base".into())
		}
	);
	if published {
		let owner =
			read.membership(fixture.graph.id(), &fixture.records[2]).assert_ok().assert_some();
		let snapshot = read.query_snapshot().assert_ok();
		let physical = read
			.query_physical_fields(
				&snapshot,
				root.graph(fixture.graph.id()).assert_some().reference(),
				owner,
			)
			.assert_ok()
			.assert_some();
		assert_eq!(physical.properties.get(fixture.keys[1]), None);
	}
	let global = root.resolve_atom(&fixture.records[3]).assert_some();
	let local = root.membership(fixture.graph.id(), global).assert_some();
	assert_eq!(
		root.occurrence(fixture.graph.id(), local).assert_some().state(),
		&OccurrenceState::edge(
			EdgeTopology::directed([fixture.nodes[0]], [fixture.nodes[1]]),
			published.then(|| 3_i64.into()),
			None,
		)
	);
	assert_label_arrays(&fixture.store, fixture.graph.id());
}

/// Independent expected labels prevent a no-op mutation from becoming the
/// oracle.
fn assert_empty_or_first_labels(fixture: &LabelFixture, first: bool) {
	let root = fixture.store.inner.current_root();
	let graph = fixture.graph.id();
	let expected = if first {
		[0, 3]
			.into_iter()
			.map(|index| root.resolve_atom(&fixture.records[index]).assert_some())
			.collect::<Vec<_>>()
	} else {
		Vec::new()
	};
	let mut labeled = 0;
	for occurrence in root.graph(graph).assert_some().occurrences() {
		let name = expected.contains(&occurrence.global_id()).then_some("R");
		assert_eq!(occurrence.label().map(|label| label.as_str()), name);
		labeled += usize::from(name.is_some());
	}
	assert_eq!(
		labeled,
		if first {
			2
		} else {
			0
		}
	);
	assert_label_arrays(&fixture.store, graph);
}

/// The edge-only oracle must reject an implementation that silently skips AA.
fn assert_edge_only_label(fixture: &LabelFixture) {
	let root = fixture.store.inner.current_root();
	let global = root.resolve_atom(&fixture.records[4]).assert_some();
	let local = root.membership(fixture.graph.id(), global).assert_some();
	assert_eq!(
		root.occurrence(fixture.graph.id(), local).assert_some().label().map(|name| name.as_str()),
		Some("AA")
	);
}

/// Repeated metadata publication must use the latest binding before checkpoint.
fn namespace_transitions(io_mode: IoMode) {
	let mut fixture = label_fixture(io_mode);
	let graph = fixture.graph.id();
	let files = local_store(&fixture.store).state.lock().file_catalog.files.clone();
	let old_arrays = capture_label_arrays(&fixture.store, graph);
	namespace_candidate(&fixture).commit().assert_ok();
	assert_namespace_files(&fixture, &files);
	assert_namespace_image(&fixture, true);
	let scalar_files = local_store(&fixture.store)
		.state
		.lock()
		.file_catalog
		.files
		.iter()
		.filter(|entry| {
			entry.file_id.kind() == Some(DataFileKind::Index)
				&& !entry.path.as_str().contains("/idx/lbl_")
		})
		.cloned()
		.collect::<Vec<_>>();
	// A→B retains numeric ID zero independently in both namespaces.
	let mut same_id = fixture.store.begin();
	for offset in [0, 3] {
		same_id.set_occurrence_label(graph, &fixture.records[offset], Some("B".into())).assert_ok();
	}
	same_id.commit().assert_ok();
	for entry in &scalar_files {
		assert!(local_store(&fixture.store).state.lock().file_catalog.files.contains(entry));
	}
	let node_arrays = local_store(&fixture.store)
		.state
		.lock()
		.file_catalog
		.files
		.iter()
		.filter(|entry| {
			entry.space_id == DataSpaceId::Graph(graph)
				&& entry.path.as_str().contains("/idx/lbl_node_")
		})
		.cloned()
		.collect::<Vec<_>>();
	let boundary = local_store(&fixture.store).wal.snapshot().visible_lsn();
	let mut edge_only = fixture.store.begin();
	edge_only.set_occurrence_label(graph, &fixture.records[4], Some("AA".into())).assert_ok();
	edge_only.commit().assert_ok();
	assert_edge_only_label(&fixture);
	for entry in &node_arrays {
		assert!(local_store(&fixture.store).state.lock().file_catalog.files.contains(entry));
	}
	assert!(
		local_store(&fixture.store)
			.wal
			.recovery_frames()
			.into_iter()
			.filter(|frame| frame.frame_lsn() > boundary)
			.all(|frame| frame.key().file_id().kind() != Some(DataFileKind::NodesHot))
	);
	assert!(local_store(&fixture.store).wal.recovery_frames().into_iter().any(|frame| {
		frame.frame_lsn() > boundary
			&& frame.key().space_id() == DataSpaceId::Graph(graph)
			&& frame.key().file_id().kind() == Some(DataFileKind::EdgesHot)
	}));
	assert_namespace_files(&fixture, &files);
	assert_label_arrays(&fixture.store, graph);
	for array in &old_arrays {
		array.assert_unchanged();
	}
	drop(old_arrays);
	fixture.store.checkpoint(CheckpointMode::Restart).assert_ok();
	drop(fixture.store);
	fixture.store = Datastore::open_local(&fixture.directory.0, fixture.options).assert_ok();
	assert_eq!(fixture.store.local_storage_status().assert_some().options().io_mode, io_mode);
	fixture.store.inner.maintenance.shutdown();
	assert_edge_only_label(&fixture);
	let root = fixture.store.inner.current_root();
	for offset in [0, 3] {
		let global = root.resolve_atom(&fixture.records[offset]).assert_some();
		let local = root.membership(graph, global).assert_some();
		assert_eq!(
			root.occurrence(graph, local).assert_some().label().map(|name| name.as_str()),
			Some("B")
		);
	}
	let records = root
		.graph(graph)
		.assert_some()
		.occurrences()
		.map(|occurrence| root.atom(occurrence.global_id()).assert_some().record_id().clone())
		.collect::<Vec<_>>();
	let mut empty = fixture.store.begin();
	for record in records {
		empty.set_occurrence_label(graph, &record, None).assert_ok();
	}
	empty.commit().assert_ok();
	assert_empty_or_first_labels(&fixture, false);
	fixture.store.checkpoint(CheckpointMode::Restart).assert_ok();
	drop(fixture.store);
	fixture.store = Datastore::open_local(&fixture.directory.0, fixture.options).assert_ok();
	assert_eq!(fixture.store.local_storage_status().assert_some().options().io_mode, io_mode);
	fixture.store.inner.maintenance.shutdown();
	assert_empty_or_first_labels(&fixture, false);
	let mut first = fixture.store.begin();
	for offset in [0, 3] {
		first.set_occurrence_label(graph, &fixture.records[offset], Some("R".into())).assert_ok();
	}
	first.commit().assert_ok();
	assert_namespace_files(&fixture, &files);
	assert_empty_or_first_labels(&fixture, true);
	fixture.store.checkpoint(CheckpointMode::Restart).assert_ok();
	drop(fixture.store);
	fixture.store = Datastore::open_local(&fixture.directory.0, fixture.options).assert_ok();
	assert_eq!(fixture.store.local_storage_status().assert_some().options().io_mode, io_mode);
	fixture.store.inner.maintenance.shutdown();
	assert_empty_or_first_labels(&fixture, true);
}

/// Exercises both known pre-WAL rejection and each uncertain append boundary.
fn namespace_failures(io_mode: IoMode) {
	use crate::storage::wal::{CommitFailureGuard, CommitFailurePoint};
	{
		let fixture = label_fixture(io_mode);
		let local = local_store(&fixture.store);
		let stamp = fixture.store.inner.current_root().commit_ts;
		let boundary = local.wal.snapshot().visible_lsn();
		let files = local.state.lock().file_catalog.files.clone();
		let generations = generation_directories(&fixture.directory.0);
		for point in [
			GenerationFailurePoint::AfterBuild,
			GenerationFailurePoint::AfterFileSync,
			GenerationFailurePoint::AfterCatalogStage,
			GenerationFailurePoint::BeforeWalCommit,
		] {
			let write = namespace_candidate(&fixture);
			{
				let _failure = GenerationFailureGuard::set(point);
				write.commit().assert_err();
			}
			assert_eq!(fixture.store.inner.current_root().commit_ts, stamp);
			assert_eq!(local.wal.snapshot().visible_lsn(), boundary);
			assert_eq!(local.state.lock().file_catalog.files, files);
			assert_eq!(generation_directories(&fixture.directory.0), generations);
			assert_namespace_image(&fixture, false);
		}
		namespace_candidate(&fixture).commit().assert_ok();
		assert_namespace_image(&fixture, true);
	}
	for point in [
		CommitFailurePoint::BeforeAppend,
		CommitFailurePoint::AfterAppend,
		CommitFailurePoint::AfterFlush,
		CommitFailurePoint::AfterSync,
	] {
		let mut fixture = label_fixture(io_mode);
		let stamp = fixture.store.inner.current_root().commit_ts;
		let files = local_store(&fixture.store).state.lock().file_catalog.files.clone();
		{
			let _failure = CommitFailureGuard::set(point);
			namespace_candidate(&fixture).commit().assert_err();
		}
		assert_eq!(fixture.store.inner.current_root().commit_ts, stamp);
		assert_eq!(local_store(&fixture.store).state.lock().file_catalog.files, files);
		assert_namespace_image(&fixture, false);
		drop(fixture.store);
		fixture.store = Datastore::open_local(&fixture.directory.0, fixture.options).assert_ok();
		assert_eq!(fixture.store.local_storage_status().assert_some().options().io_mode, io_mode);
		fixture.store.inner.maintenance.shutdown();
		let published = point != CommitFailurePoint::BeforeAppend;
		assert_eq!(fixture.store.inner.current_root().commit_ts, stamp + u64::from(published));
		assert_namespace_files(&fixture, &files);
		assert_namespace_image(&fixture, published);
		fixture.store.checkpoint(CheckpointMode::Restart).assert_ok();
		drop(fixture.store);
		fixture.store = Datastore::open_local(&fixture.directory.0, fixture.options).assert_ok();
		assert_eq!(fixture.store.local_storage_status().assert_some().options().io_mode, io_mode);
		fixture.store.inner.maintenance.shutdown();
		assert_namespace_image(&fixture, published);
	}
}

/// A source tail remains authoritative even when its entire label is removed.
#[test]
fn label_namespace_removed_source_tail_corruption_publishes_nothing() {
	for omitted in [false, true] {
		let mut fixture = label_fixture(IoMode::Sync);
		let local = local_store(&fixture.store);
		let state = local.state.lock();
		let entry = state
			.file_catalog
			.files
			.iter()
			.find(|entry| {
				entry.space_id == DataSpaceId::Graph(fixture.graph.id())
					&& entry.path.as_str().ends_with("/idx/lbl_node_1.fgidx")
			})
			.assert_some();
		let mut wrong = vec![local_address(fixture.nodes[0]).assert_ok()];
		if !omitted {
			wrong.push(local_address(fixture.nodes[2]).assert_ok());
		}
		let pages = build_label_index(&wrong, AtomKind::Node, crate::storage::page::PageLsn::ZERO)
			.assert_ok();
		state
			.registry
			.resolve_file(entry.space_id, entry.file_id)
			.assert_ok()
			.file()
			.assert_ok()
			.write_page_raw(crate::storage::page::PageNo::new(0), &pages[0])
			.assert_ok();
		drop(state);
		drop(fixture.store);
		fixture.store = Datastore::open_local(&fixture.directory.0, fixture.options).assert_ok();
		fixture.store.inner.maintenance.shutdown();
		let stamp = fixture.store.inner.current_root().commit_ts;
		let boundary = local_store(&fixture.store).wal.snapshot().visible_lsn();
		let files = local_store(&fixture.store).state.lock().file_catalog.files.clone();
		let mut remove = fixture.store.begin();
		for record in &fixture.records[..2] {
			remove.set_occurrence_label(fixture.graph.id(), record, None).assert_ok();
		}
		remove.commit().assert_err();
		assert_eq!(fixture.store.inner.current_root().commit_ts, stamp);
		assert_eq!(local_store(&fixture.store).wal.snapshot().visible_lsn(), boundary);
		assert_eq!(local_store(&fixture.store).state.lock().file_catalog.files, files);
	}
}

/// Builder admission rejects before any selected side-file is created.
#[test]
fn label_namespace_small_budget_preserves_root_wal_and_files() {
	let mut fixture = label_fixture(IoMode::Sync);
	fixture.options.maintenance.max_memory_bytes =
		std::num::NonZeroUsize::new(128 * 1024).assert_some();
	drop(fixture.store);
	fixture.store = Datastore::open_local(&fixture.directory.0, fixture.options).assert_ok();
	fixture.store.inner.maintenance.shutdown();
	let stamp = fixture.store.inner.current_root().commit_ts;
	let boundary = local_store(&fixture.store).wal.snapshot().visible_lsn();
	let files = local_store(&fixture.store).state.lock().file_catalog.files.clone();
	let generations = generation_directories(&fixture.directory.0);
	let error = namespace_candidate(&fixture).commit().assert_err();
	assert!(matches!(
		error.status().status(),
		crate::gql_status::Status::StorageDecodeLimitExceeded { .. }
	));
	assert_eq!(fixture.store.inner.current_root().commit_ts, stamp);
	assert_eq!(local_store(&fixture.store).wal.snapshot().visible_lsn(), boundary);
	assert_eq!(local_store(&fixture.store).state.lock().file_catalog.files, files);
	assert_eq!(generation_directories(&fixture.directory.0), generations);
	assert_namespace_image(&fixture, false);
}
