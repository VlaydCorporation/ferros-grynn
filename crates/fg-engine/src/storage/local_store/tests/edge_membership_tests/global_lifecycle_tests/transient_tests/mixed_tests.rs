//! A transient EDGE may refer to a NODE that disappears in the same commit.

use super::*;

/// Both local allocators advance without retaining any transient adjacency.
#[test]
fn global_edge_transient_mixed_endpoint_publishes_both_tombstones() {
	for io_mode in [IoMode::Sync, IoMode::NativeAsync] {
		if io_mode == IoMode::NativeAsync && !cfg!(feature = "native-async-io") {
			continue;
		}
		let mut fixture = fixture(io_mode);
		let files = support::local(&fixture.store).state.lock().file_catalog.files.clone();
		let boundary = support::local(&fixture.store).wal.snapshot().visible_lsn();
		let node_record = "node:gone".parse::<RecordId>().assert_ok();
		let edge_record = "edge:gone".parse::<RecordId>().assert_ok();
		let mut write = fixture.store.begin();
		write.create_atom(node_record.clone(), PropertyMap::new()).assert_ok();
		let node = write.attach_node(fixture.graph.id(), &node_record, None, None).assert_ok();
		let edge = insert_edge(
			&fixture,
			&mut write,
			&edge_record,
			10,
			EdgeTopology::directed([node], [fixture.nodes[1]]),
		);
		assert_eq!((node.slot(), edge.slot()), (2, 0));
		write.detach_occurrence(fixture.graph.id(), &edge_record, false).assert_ok();
		write.delete_atom_cascade(&edge_record, false).assert_ok();
		write.detach_occurrence(fixture.graph.id(), &node_record, false).assert_ok();
		write.delete_atom_cascade(&node_record, false).assert_ok();
		write.commit().assert_ok();
		assert_fixed_files(&fixture.store, &files, &[fixture.graph.id()]);
		assert_hot_frames(&fixture, boundary, &[0], &[0]);
		{
			let local = support::local(&fixture.store);
			let graph = {
				let state = local.state.lock();
				state
					.bindings
					.graphs
					.iter()
					.find(|binding| binding.graph_id == fixture.graph.id())
					.assert_some()
					.files
			};
			assert!(local.wal.recovery_frames().iter().all(|frame| {
				frame.frame_lsn() <= boundary
					|| frame.key().space_id() != graph.space()
					|| frame.key().file_id() != graph.adjacency
			}));
		}
		for cold in [false, true] {
			if cold {
				fixture.store.checkpoint(CheckpointMode::Restart).assert_ok();
				fixture = support::reopen(fixture);
			}
			let root = fixture.store.inner.current_root();
			assert!(root.resolve_atom(&node_record).is_none());
			assert!(root.resolve_atom(&edge_record).is_none());
			assert_eq!(root.atoms.slot_generation(2), Some(1));
			assert_eq!(root.atoms.slot_generation(3), Some(1));
			let graph = root.graph(fixture.graph.id()).assert_some();
			assert_eq!(graph.slot_generation(AtomKind::Node, 2), Some(1));
			assert_eq!(graph.slot_generation(AtomKind::Edge, 0), Some(1));
			drop(root);
			assert_indexes(&fixture, "edge", &[]);
			assert_class_slots(&fixture, "node", &[0, 1]);
			assert_class_slots(&fixture, "edge", &[]);
			support::assert_bulk_image(&fixture);
		}
	}
}
