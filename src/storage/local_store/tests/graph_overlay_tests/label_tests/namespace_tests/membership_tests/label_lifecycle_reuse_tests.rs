//! Labels follow final global and graph-local identities through lifecycle DML.

use super::*;

/// New Base owners and labelled occurrences publish together without
/// rebuilding.
#[test]
fn label_lifecycle_new_base_owner_preserves_atom_files_and_reopens() {
	for io_mode in [IoMode::Sync, IoMode::NativeAsync] {
		if io_mode == IoMode::NativeAsync && !cfg!(feature = "native-async-io") {
			continue;
		}
		for kind in [AtomKind::Node, AtomKind::Edge] {
			let (mut fixture, _) = membership_fixture(io_mode);
			let record = match kind {
				AtomKind::Node => "item:created_label",
				AtomKind::Edge => "link:created_label",
			}
			.parse::<RecordId>()
			.assert_ok();
			let atoms = local_store(&fixture.store).state.lock().bindings.atoms.atoms_hot;
			let mut write = membership_candidate(&fixture, &record, kind, true);
			write.set_occurrence_label(fixture.graph.id(), &record, Some("A".into())).assert_ok();
			write.commit().assert_ok();
			assert_eq!(local_store(&fixture.store).state.lock().bindings.atoms.atoms_hot, atoms);
			assert_label(&fixture, &record, "A");
			oracles::assert_image(&fixture);
			fixture = reopen_checkpointed(fixture, io_mode);
			assert_label(&fixture, &record, "A");
			oracles::assert_image(&fixture);
		}
	}
}

/// Graph-local reuse keeps the Base identity and advances only local
/// generation.
#[test]
fn label_lifecycle_detach_reattach_preserves_old_physical_arrays() {
	for io_mode in [IoMode::Sync, IoMode::NativeAsync] {
		if io_mode == IoMode::NativeAsync && !cfg!(feature = "native-async-io") {
			continue;
		}
		for kind in [AtomKind::Node, AtomKind::Edge] {
			let (mut fixture, pending) = membership_fixture(io_mode);
			let record = &pending[usize::from(kind == AtomKind::Edge)];
			let mut seed = fixture.store.begin();
			let first = attach(&mut seed, &fixture, record, kind, "M");
			seed.commit().assert_ok();
			fixture = reopen_checkpointed(fixture, io_mode);
			let before = fixture.store.inner.current_root();
			let global = before.resolve_atom(record).assert_some();
			let arrays = super::label_lifecycle_tests::capture_cold_label_arrays(
				&fixture.store,
				fixture.graph.id(),
			);
			let atoms = local_store(&fixture.store).state.lock().bindings.atoms.atoms_hot;
			let mut old = fixture.store.begin();
			let mut write = fixture.store.begin();
			write.detach_occurrence(fixture.graph.id(), record, false).assert_ok();
			let next = attach(&mut write, &fixture, record, kind, "Z");
			assert_eq!((next.slot(), next.generation()), (first.slot(), first.generation() + 1));
			write.commit().assert_ok();
			assert_eq!(fixture.store.inner.current_root().resolve_atom(record), Some(global));
			assert_eq!(local_store(&fixture.store).state.lock().bindings.atoms.atoms_hot, atoms);
			assert_label(&fixture, record, "Z");
			oracles::assert_image(&fixture);
			fixture.store.checkpoint(CheckpointMode::Passive).assert_ok();
			assert_eq!(old.membership(fixture.graph.id(), record).assert_ok(), Some(first));
			for array in &arrays {
				array.assert_unchanged();
			}
			drop(arrays);
			drop(old);
			drop(before);
			fixture = reopen_checkpointed(fixture, io_mode);
			assert_label(&fixture, record, "Z");
			oracles::assert_image(&fixture);
		}
	}
}

/// Deleting the Base owner advances both allocators and replaces its label
/// once.
#[test]
fn label_lifecycle_global_cascade_reuse_keeps_both_generations() {
	for io_mode in [IoMode::Sync, IoMode::NativeAsync] {
		if io_mode == IoMode::NativeAsync && !cfg!(feature = "native-async-io") {
			continue;
		}
		for kind in [AtomKind::Node, AtomKind::Edge] {
			let (mut fixture, pending) = membership_fixture(io_mode);
			let record = &pending[usize::from(kind == AtomKind::Edge)];
			let replacement = match kind {
				AtomKind::Node => "item:replacement_label",
				AtomKind::Edge => "link:replacement_label",
			}
			.parse::<RecordId>()
			.assert_ok();
			let mut seed = fixture.store.begin();
			let first = attach(&mut seed, &fixture, record, kind, "M");
			seed.commit().assert_ok();
			fixture = reopen_checkpointed(fixture, io_mode);
			let before = fixture.store.inner.current_root();
			let global = before.resolve_atom(record).assert_some();
			let atoms = local_store(&fixture.store).state.lock().bindings.atoms.atoms_hot;
			let mut old = fixture.store.begin();
			let mut write = fixture.store.begin();
			// The Base owner still has the labelled occurrence attached.
			write.delete_atom_cascade(record, true).assert_ok();
			write
				.create_atom(replacement.clone(), membership_properties(&fixture, kind))
				.assert_ok();
			let next = attach(&mut write, &fixture, &replacement, kind, "A");
			assert_eq!((next.slot(), next.generation()), (first.slot(), first.generation() + 1));
			write.commit().assert_ok();
			let root = fixture.store.inner.current_root();
			let accepted = root.resolve_atom(&replacement).assert_some();
			assert_eq!(
				(accepted.slot(), accepted.generation()),
				(global.slot(), global.generation() + 1)
			);
			assert!(root.resolve_atom(record).is_none());
			assert_eq!(local_store(&fixture.store).state.lock().bindings.atoms.atoms_hot, atoms);
			assert_eq!(old.membership(fixture.graph.id(), record).assert_ok(), Some(first));
			assert_label(&fixture, &replacement, "A");
			oracles::assert_image(&fixture);
			drop(old);
			drop(root);
			drop(before);
			fixture = reopen_checkpointed(fixture, io_mode);
			assert_label(&fixture, &replacement, "A");
			oracles::assert_image(&fixture);
		}
	}
}

/// A labelled connected NODE removes its connector and retains both old images.
#[test]
fn label_lifecycle_connected_node_cascade_preserves_old_physical_owner() {
	for io_mode in [IoMode::Sync, IoMode::NativeAsync] {
		if io_mode == IoMode::NativeAsync && !cfg!(feature = "native-async-io") {
			continue;
		}
		let (mut fixture, pending) = membership_fixture(io_mode);
		let graph = fixture.graph.id();
		let replacement = "item:connected_replacement".parse::<RecordId>().assert_ok();
		let mut seed = fixture.store.begin();
		let first = attach(&mut seed, &fixture, &pending[0], AtomKind::Node, "M");
		let connector = seed
			.attach_edge(
				graph,
				&pending[1],
				Some("Z".into()),
				EdgeTopology::directed([first], [fixture.nodes[0]]),
				None,
				None,
			)
			.assert_ok();
		seed.commit().assert_ok();
		fixture = reopen_checkpointed(fixture, io_mode);
		let local = local_store(&fixture.store);
		let before = fixture.store.inner.current_root();
		let old_global = before.resolve_atom(&pending[0]).assert_some();
		let edge_global = before.resolve_atom(&pending[1]).assert_some();
		let pin = local.capture_query_snapshot(before.commit_ts).assert_ok().assert_some();
		let arrays = super::label_lifecycle_tests::capture_cold_label_arrays(&fixture.store, graph);
		let atoms = local.state.lock().bindings.atoms.atoms_hot;
		let boundary = local.wal.snapshot().visible_lsn();
		let mut old = fixture.store.begin();
		let mut write = fixture.store.begin();
		write.delete_atom_cascade(&pending[0], true).assert_ok();
		write
			.create_atom(replacement.clone(), membership_properties(&fixture, AtomKind::Node))
			.assert_ok();
		let next = attach(&mut write, &fixture, &replacement, AtomKind::Node, "A");
		assert_eq!((next.slot(), next.generation()), (first.slot(), first.generation() + 1));
		write.commit().assert_ok();
		let root = fixture.store.inner.current_root();
		let accepted = root.resolve_atom(&replacement).assert_some();
		assert_eq!(
			(accepted.slot(), accepted.generation()),
			(old_global.slot(), old_global.generation() + 1)
		);
		assert!(root.resolve_atom(&pending[0]).is_none());
		assert_eq!(root.resolve_atom(&pending[1]), Some(edge_global));
		assert!(root.membership(graph, edge_global).is_none());
		assert_eq!(local.state.lock().bindings.atoms.atoms_hot, atoms);
		let frames = local
			.wal
			.recovery_frames()
			.into_iter()
			.filter(|frame| frame.frame_lsn() > boundary)
			.collect::<Vec<_>>();
		let keys =
			frames.iter().map(|frame| frame.key()).collect::<std::collections::BTreeSet<_>>();
		assert!(!keys.is_empty());
		assert_eq!(keys.len(), frames.len());
		assert_label(&fixture, &replacement, "A");
		oracles::assert_image(&fixture);
		fixture.store.checkpoint(CheckpointMode::Passive).assert_ok();
		assert_eq!(old.membership(graph, &pending[0]).assert_ok(), Some(first));
		assert_eq!(old.membership(graph, &pending[1]).assert_ok(), Some(connector));
		// The old physical Hot/Base owner is first resolved after home
		// backfill.
		let context =
			crate::datastore::index_context(&before.schema, &before.dictionary).assert_ok();
		let fields = local
			.physical_traversal_fields(&pin, graph, first, &context.decoder())
			.assert_ok()
			.assert_some();
		assert_eq!(fields.global, old_global);
		assert_eq!(fields.record_id, pending[0]);
		assert_eq!(&fields.base_properties, before.atom(old_global).assert_some().properties());
		for array in &arrays {
			array.assert_unchanged();
		}
		drop(fields);
		drop(arrays);
		drop(pin);
		drop(old);
		drop(root);
		drop(before);
		fixture = reopen_checkpointed(fixture, io_mode);
		assert_label(&fixture, &replacement, "A");
		assert!(fixture.store.inner.current_root().membership(graph, edge_global).is_none());
		oracles::assert_image(&fixture);
	}
}

/// Keeps the family-specific API calls explicit in the test fixture.
pub(super) fn attach(
	write: &mut Transaction,
	fixture: &LabelFixture,
	record: &RecordId,
	kind: AtomKind,
	label: &str,
) -> AtomId {
	match kind {
		AtomKind::Node => {
			write.attach_node(fixture.graph.id(), record, Some(label.into()), None).assert_ok()
		}
		AtomKind::Edge => write
			.attach_edge(
				fixture.graph.id(),
				record,
				Some(label.into()),
				EdgeTopology::directed([fixture.nodes[0]], [fixture.nodes[1]]),
				None,
				None,
			)
			.assert_ok(),
	}
}

/// Checks the requested name independently of physical self-consistency.
fn assert_label(fixture: &LabelFixture, record: &RecordId, expected: &str) {
	let root = fixture.store.inner.current_root();
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
