//! Newly created NODE/EDGE owners share accepted replay and one Hot-page pass.

use super::*;

/// Replay must remap the new endpoint before assembling its new EDGE image.
#[test]
fn global_edge_mixed_new_node_replay_and_cascade_match_bulk() {
	for io_mode in [IoMode::Sync, IoMode::NativeAsync] {
		if io_mode == IoMode::NativeAsync && !cfg!(feature = "native-async-io") {
			continue;
		}
		let mut fixture = fixture(io_mode);
		let node_record = "node:late".parse::<RecordId>().assert_ok();
		let edge_record = "edge:mixed".parse::<RecordId>().assert_ok();
		let mut late = fixture.store.begin();
		late.create_atom(node_record.clone(), PropertyMap::new()).assert_ok();
		let provisional =
			late.attach_node(fixture.graph.id(), &node_record, None, None).assert_ok();
		let edge = insert_edge(
			&fixture,
			&mut late,
			&edge_record,
			10,
			EdgeTopology::directed([provisional], [fixture.nodes[1]]),
		);
		let racer = "node:racer".parse::<RecordId>().assert_ok();
		let mut early = fixture.store.begin();
		early.create_atom(racer.clone(), PropertyMap::new()).assert_ok();
		early.attach_node(fixture.graph.id(), &racer, None, None).assert_ok();
		early.commit().assert_ok();
		let files = support::local(&fixture.store).state.lock().file_catalog.files.clone();
		let boundary = support::local(&fixture.store).wal.snapshot().visible_lsn();
		late.commit().assert_ok();
		let root = fixture.store.inner.current_root();
		let global_node = root.resolve_atom(&node_record).assert_some();
		let global_edge = root.resolve_atom(&edge_record).assert_some();
		let node = root.membership(fixture.graph.id(), global_node).assert_some();
		assert_eq!((global_node.slot(), global_edge.slot()), (3, 4));
		assert_eq!((provisional.slot(), node.slot()), (2, 3));
		assert_eq!(root.membership(fixture.graph.id(), global_edge), Some(edge));
		let crate::datastore::OccurrenceStateRef::Edge {
			topology,
			..
		} = root.occurrence(fixture.graph.id(), edge).assert_some().state().view()
		else {
			unreachable!("EDGE fixture changed its occurrence kind");
		};
		assert_eq!(topology, &EdgeTopology::directed([node], [fixture.nodes[1]]));
		drop(root);
		assert_fixed_files(&fixture.store, &files, &[fixture.graph.id()]);
		assert_hot_frames(&fixture, boundary, &[0], &[0]);
		assert_class_slots(&fixture, "node", &[0, 1, 2, 3]);
		assert_class_slots(&fixture, "edge", &[0]);
		assert_indexes(&fixture, "edge", &[(edge_record.clone(), 10)]);
		support::assert_bulk_image(&fixture);
		let mut old = fixture.store.begin();
		let snapshot = old.query_snapshot().assert_ok();
		let files = support::local(&fixture.store).state.lock().file_catalog.files.clone();
		let boundary = support::local(&fixture.store).wal.snapshot().visible_lsn();
		let mut remove = fixture.store.begin();
		remove.delete_atom_cascade(&node_record, true).assert_ok();
		remove.delete_atom_cascade(&edge_record, true).assert_ok();
		remove.commit().assert_ok();
		assert_fixed_files(&fixture.store, &files, &[fixture.graph.id()]);
		assert_hot_frames(&fixture, boundary, &[0], &[0]);
		assert_indexes(&fixture, "edge", &[]);
		assert_class_slots(&fixture, "node", &[0, 1, 2]);
		assert_class_slots(&fixture, "edge", &[]);
		support::assert_bulk_image(&fixture);
		fixture.store.checkpoint(CheckpointMode::Passive).assert_ok();
		assert_adjacency(&old, &snapshot, &fixture.graph, &[node, fixture.nodes[1]], Some(edge));
		drop(snapshot);
		drop(old);
		fixture.store.checkpoint(CheckpointMode::Restart).assert_ok();
		fixture = support::reopen(fixture);
		let root = fixture.store.inner.current_root();
		assert!(root.resolve_atom(&node_record).is_none());
		assert!(root.resolve_atom(&edge_record).is_none());
		assert!(root.resolve_atom(&racer).is_some());
		assert_eq!(root.atoms.slot_generation(3), Some(1));
		assert_eq!(root.atoms.slot_generation(4), Some(1));
		drop(root);
		assert_indexes(&fixture, "edge", &[]);
		assert_class_slots(&fixture, "node", &[0, 1, 2]);
		assert_class_slots(&fixture, "edge", &[]);
		support::assert_bulk_image(&fixture);
	}
}
