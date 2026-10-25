//! Lifecycle payloads must share sparse graph publication with stable metadata.

use super::*;
use crate::values::{Array, Object, Set};

/// NODE attach with a payload keeps unrelated database-global files.
#[test]
fn overlay_lifecycle_node_sync_preserves_sparse_files() {
	lifecycle_attach(IoMode::Sync, AtomKind::Node);
}

/// The actual NativeAsync backend keeps the same NODE publication contract.
#[cfg(feature = "native-async-io")]
#[test]
fn overlay_lifecycle_node_native_async_preserves_sparse_files() {
	lifecycle_attach(IoMode::NativeAsync, AtomKind::Node);
}

/// EDGE payloads share the commit with endpoint adjacency and label metadata.
#[test]
fn overlay_lifecycle_edge_sync_preserves_sparse_files() {
	lifecycle_attach(IoMode::Sync, AtomKind::Edge);
}

/// NativeAsync preserves the complete EDGE image across cold reopen.
#[cfg(feature = "native-async-io")]
#[test]
fn overlay_lifecycle_edge_native_async_preserves_sparse_files() {
	lifecycle_attach(IoMode::NativeAsync, AtomKind::Edge);
}

/// Hidden, explicit null and nested values exercise distinct sparse states.
fn lifecycle_attach(io_mode: IoMode, kind: AtomKind) {
	for variant in 0..3 {
		let (mut fixture, pending) = membership_fixture(io_mode);
		let graph = fixture.graph.id();
		let record = match kind {
			AtomKind::Node => &pending[0],
			AtomKind::Edge => &pending[1],
		};
		let local = local_store(&fixture.store);
		let atoms = local.state.lock().bindings.atoms.atoms_hot;
		let files = local.state.lock().file_catalog.files.clone();
		let boundary = local.wal.snapshot().visible_lsn();
		let mut old = fixture.store.begin();
		let mut write = fixture.store.begin();
		match kind {
			AtomKind::Node => {
				write.attach_node(graph, record, Some("A".into()), None).assert_ok();
			}
			AtomKind::Edge => {
				write
					.attach_edge(
						graph,
						record,
						Some("A".into()),
						EdgeTopology::directed([fixture.nodes[0]], [fixture.nodes[1]]),
						None,
						None,
					)
					.assert_ok();
			}
		}
		let value = match variant {
			0 => {
				write.hide_property(graph, record, fixture.keys[1]).assert_ok();
				None
			}
			1 => Some(Value::Null),
			_ => Some(Value::Object(Object::from_iter([(
				"lifecycle_nested",
				Value::Array(Array::new(vec![
					Value::Set(Set::from(vec![3_i64, 1, 2])),
					Value::DateTime("2024-01-01T12:00:00-05:00[US/Eastern]".parse().assert_ok()),
				])),
			)]))),
		};
		if let Some(value) = &value {
			write.set_overlay(graph, record, fixture.keys[1], value.clone()).assert_ok();
		}
		// Both paths must use one overlay append range in the same graph.
		write.set_overlay(graph, &fixture.records[0], fixture.keys[1], "stable".into()).assert_ok();
		write.commit().assert_ok();
		assert_eq!(
			local.state.lock().bindings.atoms.atoms_hot,
			atoms,
			"overlay lifecycle rebuilt database-global files"
		);
		super::super::super::namespace_publication_tests::assert_namespace_files(&fixture, &files);
		let frames = local
			.wal
			.recovery_frames()
			.into_iter()
			.filter(|frame| frame.frame_lsn() > boundary)
			.collect::<Vec<_>>();
		let keys =
			frames.iter().map(|frame| frame.key()).collect::<std::collections::BTreeSet<_>>();
		assert!(!frames.is_empty());
		assert_eq!(keys.len(), frames.len());
		assert!(old.membership(graph, record).assert_ok().is_none());
		assert_eq!(
			fixture.store.begin().effective_property(graph, record, fixture.keys[1]).assert_ok(),
			value
		);
		if let Some(value) = &value {
			assert_overlay_property(&fixture.store, graph, record, fixture.keys[1], value.clone());
		}
		assert_overlay_property(
			&fixture.store,
			graph,
			&fixture.records[0],
			fixture.keys[1],
			"stable".into(),
		);
		oracles::assert_image(&fixture);
		drop(old);
		let record = record.clone();
		fixture = reopen_checkpointed(fixture, io_mode);
		assert_eq!(fixture.store.local_storage_status().assert_some().options().io_mode, io_mode);
		assert_eq!(
			fixture.store.begin().effective_property(graph, &record, fixture.keys[1]).assert_ok(),
			value
		);
		if let Some(value) = value {
			assert_overlay_property(&fixture.store, graph, &record, fixture.keys[1], value);
		}
		assert_overlay_property(
			&fixture.store,
			graph,
			&fixture.records[0],
			fixture.keys[1],
			"stable".into(),
		);
		oracles::assert_image(&fixture);
		let source = fixture.store.inner.current_root();
		let global = source.resolve_atom(&record).assert_some();
		let old_local = source.membership(graph, global).assert_some();
		let local = local_store(&fixture.store);
		let pin = local.capture_query_snapshot(source.commit_ts).assert_ok().assert_some();
		let boundary = local.wal.snapshot().visible_lsn();
		let mut retained = fixture.store.begin();
		let mut remove = fixture.store.begin();
		remove.detach_occurrence(graph, &record, false).assert_ok();
		remove.commit().assert_ok();
		assert_eq!(local.state.lock().bindings.atoms.atoms_hot, atoms);
		assert!(fixture.store.begin().membership(graph, &record).assert_ok().is_none());
		let frames = local
			.wal
			.recovery_frames()
			.into_iter()
			.filter(|frame| frame.frame_lsn() > boundary)
			.collect::<Vec<_>>();
		let keys =
			frames.iter().map(|frame| frame.key()).collect::<std::collections::BTreeSet<_>>();
		assert!(!frames.is_empty());
		assert_eq!(keys.len(), frames.len());
		fixture.store.checkpoint(CheckpointMode::Passive).assert_ok();
		assert_eq!(retained.membership(graph, &record).assert_ok(), Some(old_local));
		// This pin first reads its old Hot/overlay image after source home
		// backfill.
		let context =
			crate::datastore::index_context(&source.schema, &source.dictionary).assert_ok();
		let fields = local
			.physical_traversal_fields(&pin, graph, old_local, &context.decoder())
			.assert_ok()
			.assert_some();
		let occurrence = source.occurrence(graph, old_local).assert_some();
		assert_eq!(fields.global, global);
		assert_eq!(fields.overlay.as_ref(), occurrence.overlay());
		assert_eq!(&fields.base_properties, source.atom(global).assert_some().properties());
		oracles::assert_image(&fixture);
		drop(pin);
		drop(source);
		drop(retained);
		fixture = reopen_checkpointed(fixture, io_mode);
		assert!(fixture.store.begin().membership(graph, &record).assert_ok().is_none());
		oracles::assert_image(&fixture);
	}
}
