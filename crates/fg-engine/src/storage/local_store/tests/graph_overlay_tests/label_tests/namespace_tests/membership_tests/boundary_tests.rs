//! Explicit targets cross page and graph boundaries without full publication.

use super::*;
use crate::storage::{PhysicalIndexEntries, graph_pages::hot_slot_address, types::DataFileKind};

/// Changes only the pending owner's label in the existing mixed transaction.
fn candidate(
	fixture: &LabelFixture,
	record: &RecordId,
	kind: AtomKind,
	new_owner: bool,
	labelled: bool,
	payload: Option<&Value>,
) -> Transaction {
	let mut write = membership_candidate(fixture, record, kind, new_owner);
	if labelled {
		write.set_occurrence_label(fixture.graph.id(), record, Some("A".into())).assert_ok();
	}
	if let Some(payload) = payload {
		write.set_overlay(fixture.graph.id(), record, fixture.keys[1], payload.clone()).assert_ok();
	}
	write
}

/// The last slot and the next page keep exact Hot sets and old physical owners.
#[test]
fn metadata_membership_node_hot_boundaries_keep_old_snapshots() {
	node_hot_boundaries(false, None);
}

/// Labelled lifecycle shares the same boundary and replay contract.
#[test]
fn label_lifecycle_node_hot_boundaries() {
	node_hot_boundaries(true, None);
}

/// Keeps all adjacent metadata mutations identical across admission variants.
fn node_hot_boundaries(labelled: bool, payload: Option<&Value>) {
	for io_mode in [IoMode::Sync, IoMode::NativeAsync] {
		if io_mode == IoMode::NativeAsync && !cfg!(feature = "native-async-io") {
			continue;
		}
		for next_slot in [101_u64, 102] {
			let (mut fixture, pending) = membership_fixture(io_mode);
			let graph = fixture.graph.id();
			let mut seed = fixture.store.begin();
			for slot in 4..next_slot {
				let record = format!("item:padding{slot}").parse::<RecordId>().assert_ok();
				let mut properties = membership_properties(&fixture, AtomKind::Node);
				properties.insert(fixture.keys[0], i64::try_from(slot).assert_ok());
				seed.create_atom(record.clone(), properties).assert_ok();
				assert_eq!(
					seed.attach_node(graph, &record, Some("N".into()), None).assert_ok().slot(),
					slot
				);
			}
			seed.commit().assert_ok();
			fixture = reopen_checkpointed(fixture, io_mode);
			let local = local_store(&fixture.store);
			let source = fixture.store.inner.current_root();
			let pin = local.capture_query_snapshot(source.commit_ts).assert_ok().assert_some();
			let arrays = capture_label_arrays(&fixture.store, graph);
			let count = source
				.schema
				.class("item")
				.assert_some()
				.indexes()
				.find(|index| index.kind() == ClassIndexKind::Count)
				.assert_some()
				.id()
				.assert_some();
			let files = local.state.lock().file_catalog.files.clone();
			let boundary = local.wal.snapshot().visible_lsn();
			candidate(&fixture, &pending[0], AtomKind::Node, false, labelled, payload)
				.commit()
				.assert_ok();
			assert_eq!(
				fixture
					.store
					.begin()
					.membership(graph, &pending[0])
					.assert_ok()
					.assert_some()
					.slot(),
				next_slot
			);
			super::super::super::namespace_publication_tests::assert_namespace_files(
				&fixture, &files,
			);
			let frames = local
				.wal
				.recovery_frames()
				.into_iter()
				.filter(|frame| frame.frame_lsn() > boundary)
				.collect::<Vec<_>>();
			let keys =
				frames.iter().map(|frame| frame.key()).collect::<std::collections::BTreeSet<_>>();
			assert_eq!(keys.len(), frames.len());
			let pages = keys
				.iter()
				.filter(|key| {
					key.space_id() == DataSpaceId::Graph(graph)
						&& key.file_id().kind() == Some(DataFileKind::NodesHot)
				})
				.map(|key| key.page_no().get())
				.collect::<std::collections::BTreeSet<_>>();
			assert_eq!(
				pages,
				[0, hot_slot_address(next_slot).assert_ok().0.get()].into_iter().collect()
			);
			assert_requested_payload(&fixture, &pending[0], payload);
			oracles::assert_image(&fixture);
			fixture.store.checkpoint(CheckpointMode::Passive).assert_ok();
			// First physical Count read occurs after home backfill and
			// retirement.
			assert!(matches!(local.physical_index(&pin, count).assert_ok().assert_some().entries,
				PhysicalIndexEntries::Count(actual) if actual == next_slot));
			let context =
				crate::datastore::index_context(&source.schema, &source.dictionary).assert_ok();
			let fields = local
				.physical_traversal_fields(&pin, graph, fixture.nodes[0], &context.decoder())
				.assert_ok()
				.assert_some();
			let occurrence = source.occurrence(graph, fixture.nodes[0]).assert_some();
			assert_eq!(
				&fields.base_properties,
				source.atom(occurrence.global_id()).assert_some().properties()
			);
			assert_eq!(fields.overlay.as_ref(), occurrence.overlay());
			for array in &arrays {
				array.assert_unchanged();
			}
			drop(arrays);
			drop(pin);
			drop(source);
			let mut remove = fixture.store.begin();
			remove.detach_occurrence(graph, &pending[0], false).assert_ok();
			remove.set_occurrence_label(graph, &fixture.records[0], None).assert_ok();
			remove.set_occurrence_label(graph, &fixture.records[3], None).assert_ok();
			remove.commit().assert_ok();
			oracles::assert_image(&fixture);
			fixture = reopen_checkpointed(fixture, io_mode);
			oracles::assert_image(&fixture);
		}
	}
}

/// A graph with metadata only still receives its own complete sparse header.
#[test]
fn metadata_membership_other_graph_keeps_both_publications_sparse() {
	other_graph(false, None);
}

/// Labelled lifecycle shares the same boundary and replay contract.
#[test]
fn label_lifecycle_other_graph() {
	other_graph(true, None);
}

/// Keeps all adjacent metadata mutations identical across admission variants.
fn other_graph(labelled: bool, payload: Option<&Value>) {
	for io_mode in [IoMode::Sync, IoMode::NativeAsync] {
		if io_mode == IoMode::NativeAsync && !cfg!(feature = "native-async-io") {
			continue;
		}
		let (mut fixture, pending) = membership_fixture(io_mode);
		let other = fixture
			.store
			.inner
			.current_root()
			.schema
			.named_graph("other")
			.assert_some()
			.reference()
			.clone();
		let mut seed = fixture.store.begin();
		seed.attach_node(other.id(), &fixture.records[0], Some("N".into()), None).assert_ok();
		seed.commit().assert_ok();
		fixture = reopen_checkpointed(fixture, io_mode);
		let files = local_store(&fixture.store).state.lock().file_catalog.files.clone();
		let mut write = fixture.store.begin();
		write
			.attach_node(fixture.graph.id(), &pending[0], labelled.then(|| "A".into()), None)
			.assert_ok();
		if let Some(payload) = payload {
			write
				.set_overlay(fixture.graph.id(), &pending[0], fixture.keys[1], payload.clone())
				.assert_ok();
		}
		write
			.set_overlay(other.id(), &fixture.records[0], fixture.keys[0], 10_000.into())
			.assert_ok();
		write.set_occurrence_label(other.id(), &fixture.records[0], Some("A".into())).assert_ok();
		write.commit().assert_ok();
		for entry in &files {
			let selected = matches!(entry.space_id, DataSpaceId::Graph(graph) if graph == fixture.graph.id() || graph == other.id())
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
					local_store(&fixture.store).state.lock().file_catalog.files.contains(entry)
				);
			}
		}
		assert_requested_payload(&fixture, &pending[0], payload);
		oracles::assert_image(&fixture);
		assert_label_arrays(&fixture.store, other.id());
		assert_overlay_property(
			&fixture.store,
			other.id(),
			&fixture.records[0],
			fixture.keys[0],
			10_000.into(),
		);
		fixture = reopen_checkpointed(fixture, io_mode);
		assert_requested_payload(&fixture, &pending[0], payload);
		oracles::assert_image(&fixture);
		assert_label_arrays(&fixture.store, other.id());
		assert_overlay_property(
			&fixture.store,
			other.id(),
			&fixture.records[0],
			fixture.keys[0],
			10_000.into(),
		);
	}
}

/// Replay must reassign both pending slots before composing stable metadata.
#[test]
fn metadata_membership_late_replay_uses_final_allocator_targets() {
	late_replay(false, None);
}

/// Labelled lifecycle shares the same boundary and replay contract.
#[test]
fn label_lifecycle_late_replay() {
	late_replay(true, None);
}

/// Keeps all adjacent metadata mutations identical across admission variants.
fn late_replay(labelled: bool, payload: Option<&Value>) {
	for io_mode in [IoMode::Sync, IoMode::NativeAsync] {
		if io_mode == IoMode::NativeAsync && !cfg!(feature = "native-async-io") {
			continue;
		}
		let (mut fixture, _) = membership_fixture(io_mode);
		let created = "item:late".parse::<RecordId>().assert_ok();
		let late = candidate(&fixture, &created, AtomKind::Node, true, labelled, payload);
		let mut first = fixture.store.begin();
		let competing = "item:first".parse::<RecordId>().assert_ok();
		let mut properties = membership_properties(&fixture, AtomKind::Node);
		properties.insert(fixture.keys[0], 2000_i64);
		first.create_atom(competing.clone(), properties).assert_ok();
		first.attach_node(fixture.graph.id(), &competing, None, None).assert_ok();
		first.commit().assert_ok();
		late.commit().assert_ok();
		assert_eq!(
			fixture
				.store
				.begin()
				.membership(fixture.graph.id(), &created)
				.assert_ok()
				.assert_some()
				.slot(),
			5
		);
		assert_requested_payload(&fixture, &created, payload);
		oracles::assert_image(&fixture);
		fixture = reopen_checkpointed(fixture, io_mode);
		assert_requested_payload(&fixture, &created, payload);
		oracles::assert_image(&fixture);
	}
}

/// Lifecycle payloads preserve sparse publication on both Hot-page boundaries.
#[test]
fn overlay_lifecycle_node_hot_boundaries() {
	node_hot_boundaries(true, Some(&Value::Bytes(vec![7; 40_000].into())));
}

/// Graph-local allocators share one context across structural and metadata
/// graphs.
#[test]
fn overlay_lifecycle_other_graph() {
	other_graph(true, Some(&Value::Bytes(vec![7; 40_000].into())));
}

/// Replay publishes the payload on the final remapped occurrence identity.
#[test]
fn overlay_lifecycle_late_replay() {
	late_replay(true, Some(&Value::Bytes(vec![7; 40_000].into())));
}

/// Plain slots reuse generations while another occurrence appends a large
/// overlay.
#[test]
fn metadata_membership_detach_reuse_preserves_large_overlay() {
	detach_large_overlay(false);
}

/// Labelled lifecycle shares the same boundary and replay contract.
#[test]
fn label_lifecycle_detach_large_overlay() {
	detach_large_overlay(true);
}

/// Keeps all adjacent metadata mutations identical across admission variants.
fn detach_large_overlay(labelled: bool) {
	for io_mode in [IoMode::Sync, IoMode::NativeAsync] {
		if io_mode == IoMode::NativeAsync && !cfg!(feature = "native-async-io") {
			continue;
		}
		for kind in [AtomKind::Node, AtomKind::Edge] {
			let (mut fixture, pending) = membership_fixture(io_mode);
			let record = &pending[usize::from(kind == AtomKind::Edge)];
			candidate(&fixture, record, kind, false, labelled, None).commit().assert_ok();
			let previous = fixture
				.store
				.begin()
				.membership(fixture.graph.id(), record)
				.assert_ok()
				.assert_some();
			let payload = Value::from("overlay".repeat(5000));
			let mut remove = fixture.store.begin();
			remove.detach_occurrence(fixture.graph.id(), record, false).assert_ok();
			remove
				.set_overlay(
					fixture.graph.id(),
					&fixture.records[0],
					fixture.keys[1],
					payload.clone(),
				)
				.assert_ok();
			for offset in [0, 3] {
				remove
					.set_occurrence_label(fixture.graph.id(), &fixture.records[offset], None)
					.assert_ok();
			}
			remove.commit().assert_ok();
			oracles::assert_image(&fixture);
			candidate(&fixture, record, kind, false, labelled, None).commit().assert_ok();
			let accepted = fixture
				.store
				.begin()
				.membership(fixture.graph.id(), record)
				.assert_ok()
				.assert_some();
			assert_eq!(
				(accepted.slot(), accepted.generation()),
				(previous.slot(), previous.generation() + 1)
			);
			oracles::assert_image(&fixture);
			assert_overlay_property(
				&fixture.store,
				fixture.graph.id(),
				&fixture.records[0],
				fixture.keys[1],
				payload.clone(),
			);
			fixture = reopen_checkpointed(fixture, io_mode);
			oracles::assert_image(&fixture);
			assert_overlay_property(
				&fixture.store,
				fixture.graph.id(),
				&fixture.records[0],
				fixture.keys[1],
				payload,
			);
		}
	}
}

/// The requested value is checked independently of the logical-image oracle.
fn assert_requested_payload(fixture: &LabelFixture, record: &RecordId, payload: Option<&Value>) {
	if let Some(payload) = payload {
		assert_overlay_property(
			&fixture.store,
			fixture.graph.id(),
			record,
			fixture.keys[1],
			payload.clone(),
		);
	}
}
