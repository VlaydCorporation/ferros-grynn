//! Replay and same-slot replacement preserve live population and both kinds.

use super::*;

/// A transient replay retains the independent live EDGE and its class source.
#[test]
fn global_edge_transient_late_replay_preserves_live_population() {
	for io_mode in [IoMode::Sync, IoMode::NativeAsync] {
		if io_mode == IoMode::NativeAsync && !cfg!(feature = "native-async-io") {
			continue;
		}
		let mut fixture = fixture(io_mode);
		let gone = "edge:gone".parse::<RecordId>().assert_ok();
		let live = "edge:live".parse::<RecordId>().assert_ok();
		let (late, _) = transient_candidate(&fixture, &gone);
		candidate(&fixture, &live, 20).0.commit().assert_ok();
		let source = class_file(&fixture);
		let files = support::local(&fixture.store).state.lock().file_catalog.files.clone();
		let boundary = support::local(&fixture.store).wal.snapshot().visible_lsn();
		late.commit().assert_ok();
		assert_fixed_files(&fixture.store, &files, &[fixture.graph.id()]);
		assert_eq!(class_file(&fixture), source);
		oracles::assert_hot_frames_including_nodes(&fixture, boundary, &[0], &[], &[0]);
		for cold in [false, true] {
			if cold {
				fixture.store.checkpoint(CheckpointMode::Restart).assert_ok();
				fixture = support::reopen(fixture);
			}
			let root = fixture.store.inner.current_root();
			assert!(root.resolve_atom(&gone).is_none());
			assert_eq!(root.atoms.slot_generation(3), Some(1));
			assert_eq!(
				root.graph(fixture.graph.id()).assert_some().slot_generation(AtomKind::Edge, 1),
				Some(1)
			);
			let global = root.resolve_atom(&live).assert_some();
			assert_eq!((global.slot(), global.generation()), (2, 0));
			assert_eq!(root.membership(fixture.graph.id(), global).assert_some().slot(), 0);
			drop(root);
			assert_indexes(&fixture, "edge", &[(live.clone(), 20)]);
			assert_class_slots(&fixture, "edge", &[0]);
			support::assert_bulk_image(&fixture);
		}
	}
}

/// The old empty Count is first read after Passive, independently of warm
/// probes.
#[test]
fn global_edge_transient_same_slot_replacement_preserves_class_authorities() {
	for io_mode in [IoMode::Sync, IoMode::NativeAsync] {
		if io_mode == IoMode::NativeAsync && !cfg!(feature = "native-async-io") {
			continue;
		}
		for other_class in [false, true] {
			let mut fixture = fixture(io_mode);
			if other_class {
				let mut seed = fixture.store.begin();
				seed.define_class(
					ClassSchema::new("replacement", AtomKind::Edge, [ClassField::new(
						"value",
						Type::Int,
					)])
					.assert_ok(),
				)
				.assert_ok();
				define_indexes(&mut seed, "replacement", fixture.graph.id());
				seed.commit().assert_ok();
				fixture.store.checkpoint(CheckpointMode::Restart).assert_ok();
				fixture = support::reopen(fixture);
			}
			let root = fixture.store.inner.current_root();
			let count = root
				.schema
				.class("edge")
				.assert_some()
				.indexes()
				.find(|index| index.kind() == ClassIndexKind::Count)
				.assert_some()
				.id()
				.assert_some();
			let old = support::local(&fixture.store)
				.capture_query_snapshot(root.commit_ts)
				.assert_ok()
				.assert_some();
			drop(root);
			let files = support::local(&fixture.store).state.lock().file_catalog.files.clone();
			let boundary = support::local(&fixture.store).wal.snapshot().visible_lsn();
			let (mut write, _) =
				transient_candidate(&fixture, &"edge:gone".parse::<RecordId>().assert_ok());
			let class = if other_class {
				"replacement"
			} else {
				"edge"
			};
			let record = format!("{class}:live").parse::<RecordId>().assert_ok();
			let edge = insert_edge(
				&fixture,
				&mut write,
				&record,
				20,
				EdgeTopology::directed([fixture.nodes[0]], [fixture.nodes[1]]),
			);
			write.commit().assert_ok();
			assert_fixed_files(&fixture.store, &files, &[fixture.graph.id()]);
			assert_hot_frames(&fixture, boundary, &[0], &[0]);
			let root = fixture.store.inner.current_root();
			let global = root.resolve_atom(&record).assert_some();
			assert_eq!((global.slot(), global.generation()), (2, 1));
			assert_eq!((edge.slot(), edge.generation()), (0, 1));
			assert_eq!(root.membership(fixture.graph.id(), global), Some(edge));
			drop(root);
			fixture.store.checkpoint(CheckpointMode::Passive).assert_ok();
			{
				let physical = support::local(&fixture.store)
					.physical_index(&old, count)
					.assert_ok()
					.assert_some();
				assert!(matches!(&physical.entries, PhysicalIndexEntries::Count(0)));
			}
			drop(old);
			for cold in [false, true] {
				if cold {
					fixture.store.checkpoint(CheckpointMode::Restart).assert_ok();
					fixture = support::reopen(fixture);
				}
				if other_class {
					assert_indexes(&fixture, "edge", &[]);
					assert_class_slots(&fixture, "edge", &[]);
				}
				assert_indexes(&fixture, class, &[(record.clone(), 20)]);
				assert_class_slots(&fixture, class, &[0]);
				support::assert_bulk_image(&fixture);
			}
		}
	}
}

/// Global slot reuse does not merge the separate NODE and EDGE allocators.
#[test]
fn global_edge_transient_cross_kind_reuse_preserves_both_hot_allocators() {
	for io_mode in [IoMode::Sync, IoMode::NativeAsync] {
		if io_mode == IoMode::NativeAsync && !cfg!(feature = "native-async-io") {
			continue;
		}
		for edge_first in [false, true] {
			let mut fixture = fixture(io_mode);
			let files = support::local(&fixture.store).state.lock().file_catalog.files.clone();
			let boundary = support::local(&fixture.store).wal.snapshot().visible_lsn();
			let (write, record, local, kind) = if edge_first {
				let (mut write, _) =
					transient_candidate(&fixture, &"edge:gone".parse::<RecordId>().assert_ok());
				let record = "node:live".parse::<RecordId>().assert_ok();
				write.create_atom(record.clone(), PropertyMap::new()).assert_ok();
				let node = write.attach_node(fixture.graph.id(), &record, None, None).assert_ok();
				(write, record, node, AtomKind::Node)
			} else {
				let mut write = fixture.store.begin();
				let gone = "node:gone".parse::<RecordId>().assert_ok();
				write.create_atom(gone.clone(), PropertyMap::new()).assert_ok();
				write.attach_node(fixture.graph.id(), &gone, None, None).assert_ok();
				write.detach_occurrence(fixture.graph.id(), &gone, false).assert_ok();
				write.delete_atom_cascade(&gone, false).assert_ok();
				let record = "edge:live".parse::<RecordId>().assert_ok();
				let edge = insert_edge(
					&fixture,
					&mut write,
					&record,
					20,
					EdgeTopology::directed([fixture.nodes[0]], [fixture.nodes[1]]),
				);
				(write, record, edge, AtomKind::Edge)
			};
			write.commit().assert_ok();
			assert_fixed_files(&fixture.store, &files, &[fixture.graph.id()]);
			assert_hot_frames(&fixture, boundary, &[0], &[0]);
			for cold in [false, true] {
				if cold {
					fixture.store.checkpoint(CheckpointMode::Restart).assert_ok();
					fixture = support::reopen(fixture);
				}
				let root = fixture.store.inner.current_root();
				let global = root.resolve_atom(&record).assert_some();
				assert_eq!((global.slot(), global.generation(), global.kind()), (2, 1, Some(kind)));
				assert_eq!(local.generation(), 0);
				assert_eq!(root.membership(fixture.graph.id(), global), Some(local));
				let graph = root.graph(fixture.graph.id()).assert_some();
				assert_eq!(graph.slot_generation(AtomKind::Node, 2), Some(u32::from(!edge_first)));
				assert_eq!(graph.slot_generation(AtomKind::Edge, 0), Some(u32::from(edge_first)));
				drop(root);
				if edge_first {
					assert_indexes(&fixture, "edge", &[]);
					assert_class_slots(&fixture, "node", &[0, 1, 2]);
					assert_class_slots(&fixture, "edge", &[]);
				} else {
					assert_indexes(&fixture, "edge", &[(record.clone(), 20)]);
					assert_class_slots(&fixture, "node", &[0, 1]);
					assert_class_slots(&fixture, "edge", &[0]);
				}
				support::assert_bulk_image(&fixture);
			}
		}
	}
}
