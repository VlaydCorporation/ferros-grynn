//! Overlay references belong to full local/global occurrence generations.

use super::{label_lifecycle_reuse_tests::attach, *};
use crate::storage::{
	atom_page::PropertyRef,
	graph_pages::{hot_slot_address, read_edge_hot_slot, read_node_hot_slot},
	types::{DataFileKind, PageKey},
};

/// Equal payload bytes must not retain the prior local/global owner's ref.
#[test]
fn overlay_lifecycle_local_and_global_reuse_append_new_references() {
	for io_mode in [IoMode::Sync, IoMode::NativeAsync] {
		if io_mode == IoMode::NativeAsync && !cfg!(feature = "native-async-io") {
			continue;
		}
		for kind in [AtomKind::Node, AtomKind::Edge] {
			for global_reuse in [false, true] {
				reuse(io_mode, kind, global_reuse);
			}
		}
	}
}

/// One commit reuses both allocators only when the Base owner was deleted.
fn reuse(io_mode: IoMode, kind: AtomKind, global_reuse: bool) {
	let (mut fixture, pending) = membership_fixture(io_mode);
	let graph = fixture.graph.id();
	let record = &pending[usize::from(kind == AtomKind::Edge)];
	let payload = Value::Bytes(vec![7; 40_000].into());
	let mut seed = fixture.store.begin();
	let first = attach(&mut seed, &fixture, record, kind, "M");
	seed.set_overlay(graph, record, fixture.keys[1], payload.clone()).assert_ok();
	seed.commit().assert_ok();
	fixture = reopen_checkpointed(fixture, io_mode);
	let local = local_store(&fixture.store);
	let before = fixture.store.inner.current_root();
	let old_global = before.resolve_atom(record).assert_some();
	let old_ref = overlay_ref(&fixture, first);
	assert!(matches!(old_ref, PropertyRef::Large { .. }));
	let pin = local.capture_query_snapshot(before.commit_ts).assert_ok().assert_some();
	let atoms = local.state.lock().bindings.atoms.atoms_hot;
	let mut old = fixture.store.begin();
	let mut write = fixture.store.begin();
	let accepted_record = if global_reuse {
		write.delete_atom_cascade(record, true).assert_ok();
		let name = if kind == AtomKind::Node {
			"item:overlay_replacement"
		} else {
			"link:overlay_replacement"
		};
		let replacement = name.parse::<RecordId>().assert_ok();
		write.create_atom(replacement.clone(), membership_properties(&fixture, kind)).assert_ok();
		replacement
	} else {
		write.detach_occurrence(graph, record, false).assert_ok();
		record.clone()
	};
	let next = attach(&mut write, &fixture, &accepted_record, kind, "A");
	write.set_overlay(graph, &accepted_record, fixture.keys[1], payload.clone()).assert_ok();
	write.set_overlay(graph, &fixture.records[0], fixture.keys[1], "stable".into()).assert_ok();
	write.commit().assert_ok();
	assert_eq!((next.slot(), next.generation()), (first.slot(), first.generation() + 1));
	let after = fixture.store.inner.current_root();
	let accepted_global = after.resolve_atom(&accepted_record).assert_some();
	if global_reuse {
		assert_eq!(
			(accepted_global.slot(), accepted_global.generation()),
			(old_global.slot(), old_global.generation() + 1)
		);
		assert!(after.resolve_atom(record).is_none());
	} else {
		assert_eq!(accepted_global, old_global);
	}
	assert_eq!(local.state.lock().bindings.atoms.atoms_hot, atoms);
	assert_ne!(overlay_ref(&fixture, next), old_ref);
	assert_overlay_property(
		&fixture.store,
		graph,
		&accepted_record,
		fixture.keys[1],
		payload.clone(),
	);
	oracles::assert_image(&fixture);
	fixture.store.checkpoint(CheckpointMode::Passive).assert_ok();
	assert_eq!(old.membership(graph, record).assert_ok(), Some(first));
	let context = crate::datastore::index_context(&before.schema, &before.dictionary).assert_ok();
	// The retained pin first reads this old physical image after home backfill.
	let fields = local
		.physical_traversal_fields(&pin, graph, first, &context.decoder())
		.assert_ok()
		.assert_some();
	assert_eq!(fields.global, old_global);
	assert_eq!(fields.overlay.as_ref(), before.occurrence(graph, first).assert_some().overlay());
	drop(fields);
	drop(pin);
	drop(old);
	drop(before);
	drop(after);
	fixture = reopen_checkpointed(fixture, io_mode);
	assert_overlay_property(&fixture.store, graph, &accepted_record, fixture.keys[1], payload);
	let mut write = fixture.store.begin();
	write.set_overlay(graph, &accepted_record, fixture.keys[1], "inline".into()).assert_ok();
	write.commit().assert_ok();
	assert!(matches!(overlay_ref(&fixture, next), PropertyRef::Inline { .. }));
	assert_overlay_property(
		&fixture.store,
		graph,
		&accepted_record,
		fixture.keys[1],
		"inline".into(),
	);
	let mut write = fixture.store.begin();
	write.reset_property(graph, &accepted_record, fixture.keys[1]).assert_ok();
	write.commit().assert_ok();
	assert_eq!(overlay_ref(&fixture, next), PropertyRef::None);
	oracles::assert_image(&fixture);
	fixture = reopen_checkpointed(fixture, io_mode);
	assert_eq!(overlay_ref(&fixture, next), PropertyRef::None);
	oracles::assert_image(&fixture);
}

/// Reads the exact current physical reference without normalizing append
/// offsets.
fn overlay_ref(fixture: &LabelFixture, atom: AtomId) -> PropertyRef {
	let local = local_store(&fixture.store);
	let state = local.state.lock();
	let files = state
		.bindings
		.graphs
		.iter()
		.find(|binding| binding.graph_id == fixture.graph.id())
		.assert_some()
		.files;
	let (number, within) = hot_slot_address(atom.slot()).assert_ok();
	let key = PageKey::new(
		files.space(),
		if atom.is_node() {
			files.nodes
		} else {
			files.edges
		},
		number,
	);
	let page = local.read_update_page(&state, key).assert_ok();
	if atom.is_node() {
		read_node_hot_slot(&page, number, within).assert_ok().overlay
	} else {
		read_edge_hot_slot(&page, number, within).assert_ok().overlay
	}
}

/// Ownerless final states consume generations without appending a dead payload.
#[test]
fn overlay_lifecycle_transient_payload_emits_no_overlay_frames() {
	for io_mode in [IoMode::Sync, IoMode::NativeAsync] {
		if io_mode == IoMode::NativeAsync && !cfg!(feature = "native-async-io") {
			continue;
		}
		for kind in [AtomKind::Node, AtomKind::Edge] {
			let (mut fixture, _) = membership_fixture(io_mode);
			let graph = fixture.graph.id();
			let record = (if kind == AtomKind::Node {
				"item:overlay_transient"
			} else {
				"link:overlay_transient"
			})
			.parse::<RecordId>()
			.assert_ok();
			let local = local_store(&fixture.store);
			let files = local
				.state
				.lock()
				.bindings
				.graphs
				.iter()
				.find(|binding| binding.graph_id == graph)
				.assert_some()
				.files;
			let atoms = local.state.lock().bindings.atoms.atoms_hot;
			let boundary = local.wal.snapshot().visible_lsn();
			let mut write = fixture.store.begin();
			write.create_atom(record.clone(), membership_properties(&fixture, kind)).assert_ok();
			attach(&mut write, &fixture, &record, kind, "A");
			write
				.set_overlay(graph, &record, fixture.keys[1], Value::Bytes(vec![7; 40_000].into()))
				.assert_ok();
			write.detach_occurrence(graph, &record, false).assert_ok();
			write.delete_atom(&record).assert_ok();
			write.commit().assert_ok();
			assert_eq!(local.state.lock().bindings.atoms.atoms_hot, atoms);
			assert!(fixture.store.inner.current_root().resolve_atom(&record).is_none());
			let frames = local.wal.recovery_frames();
			let added =
				frames.iter().filter(|frame| frame.frame_lsn() > boundary).collect::<Vec<_>>();
			assert!(!added.is_empty());
			assert!(!added.iter().any(|frame| frame.key().space_id() == files.space()
				&& frame.key().file_id().kind() == Some(DataFileKind::Overlays)));
			oracles::assert_image(&fixture);
			fixture = reopen_checkpointed(fixture, io_mode);
			assert!(fixture.store.inner.current_root().resolve_atom(&record).is_none());
			oracles::assert_image(&fixture);
		}
	}
}
