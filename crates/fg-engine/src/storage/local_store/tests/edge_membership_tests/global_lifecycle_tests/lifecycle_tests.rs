//! Replay, class reuse and reverse memberships retain accepted EDGE identities.

use super::*;

/// Rejected non-CASCADE deletion must leave private state usable for DETACH.
#[test]
fn global_edge_explicit_detach_delete_preserves_private_rejection() {
	for io_mode in [IoMode::Sync, IoMode::NativeAsync] {
		if io_mode == IoMode::NativeAsync && !cfg!(feature = "native-async-io") {
			continue;
		}
		let mut fixture = fixture(io_mode);
		let record = "edge:old".parse::<RecordId>().assert_ok();
		let (write, edge) = candidate(&fixture, &record, 10);
		write.commit().assert_ok();
		let local = support::local(&fixture.store);
		let files = local.state.lock().file_catalog.files.clone();
		let boundary = local.wal.snapshot().visible_lsn();
		let mut write = fixture.store.begin();
		write.delete_atom_cascade(&record, false).assert_err();
		assert_eq!(write.membership(fixture.graph.id(), &record).assert_ok(), Some(edge));
		write.detach_occurrence(fixture.graph.id(), &record, false).assert_ok();
		write.delete_atom_cascade(&record, false).assert_ok();
		write.commit().assert_ok();
		assert_fixed_files(&fixture.store, &files, &[fixture.graph.id()]);
		assert_hot_frames(&fixture, boundary, &[0], &[0]);
		assert_indexes(&fixture, "edge", &[]);
		support::assert_bulk_image(&fixture);
		fixture.store.checkpoint(CheckpointMode::Restart).assert_ok();
		fixture = support::reopen(fixture);
		assert_indexes(&fixture, "edge", &[]);
		support::assert_bulk_image(&fixture);
	}
}

/// Both allocator slots can change class while their old snapshot stays pinned.
#[test]
fn global_edge_same_commit_reuse_preserves_class_and_generation_authority() {
	for io_mode in [IoMode::Sync, IoMode::NativeAsync] {
		if io_mode == IoMode::NativeAsync && !cfg!(feature = "native-async-io") {
			continue;
		}
		for class in ["edge", "replacement"] {
			for replacement_value in [10_i64, 20] {
				let mut fixture = fixture(io_mode);
				if class != "edge" {
					let mut seed = fixture.store.begin();
					seed.define_class(
						ClassSchema::new(class, AtomKind::Edge, [ClassField::new(
							"value",
							Type::Int,
						)])
						.assert_ok(),
					)
					.assert_ok();
					define_indexes(&mut seed, class, fixture.graph.id());
					seed.commit().assert_ok();
				}
				let record = "edge:old".parse::<RecordId>().assert_ok();
				let (write, old_edge) = candidate(&fixture, &record, 10);
				write.commit().assert_ok();
				fixture.store.checkpoint(CheckpointMode::Restart).assert_ok();
				fixture = support::reopen(fixture);
				let before = fixture.store.inner.current_root();
				let old_global = before.resolve_atom(&record).assert_some();
				let old_class = before.schema.class("edge").assert_some();
				let unique = old_class
					.indexes()
					.find(|index| index.kind() == ClassIndexKind::Unique)
					.assert_some()
					.id()
					.assert_some();
				let count = old_class
					.indexes()
					.find(|index| index.kind() == ClassIndexKind::Count)
					.assert_some()
					.id()
					.assert_some();
				let old_physical = support::local(&fixture.store)
					.capture_query_snapshot(before.commit_ts)
					.assert_ok()
					.assert_some();
				let files = support::local(&fixture.store).state.lock().file_catalog.files.clone();
				let boundary = support::local(&fixture.store).wal.snapshot().visible_lsn();
				let mut old = fixture.store.begin();
				let snapshot = old.query_snapshot().assert_ok();
				let replacement = format!("{class}:new").parse::<RecordId>().assert_ok();
				let mut write = fixture.store.begin();
				write.delete_atom_cascade(&record, true).assert_ok();
				let edge = insert_edge(
					&fixture,
					&mut write,
					&replacement,
					replacement_value,
					EdgeTopology::directed([fixture.nodes[0]], [fixture.nodes[1]]),
				);
				write.commit().assert_ok();
				let root = fixture.store.inner.current_root();
				let global = root.resolve_atom(&replacement).assert_some();
				assert_eq!(
					(global.slot(), global.generation()),
					(old_global.slot(), old_global.generation() + 1)
				);
				assert_eq!(
					(edge.slot(), edge.generation()),
					(old_edge.slot(), old_edge.generation() + 1)
				);
				assert_fixed_files(&fixture.store, &files, &[fixture.graph.id()]);
				assert_hot_frames(&fixture, boundary, &[0], &[0]);
				assert_indexes(&fixture, class, &[(replacement.clone(), replacement_value)]);
				assert_class_slots(&fixture, class, &[0]);
				if class != "edge" {
					assert_indexes(&fixture, "edge", &[]);
				}
				if class != "edge" {
					assert_class_slots(&fixture, "edge", &[]);
				}
				support::assert_bulk_image(&fixture);
				fixture.store.checkpoint(CheckpointMode::Passive).assert_ok();
				assert_indexes(&fixture, class, &[(replacement.clone(), replacement_value)]);
				assert_adjacency(&old, &snapshot, &fixture.graph, &fixture.nodes, Some(old_edge));
				// First physical reads occur after publication and backfill, so
				// current pages cannot satisfy these old index assertions.
				{
					let entries = support::local(&fixture.store)
						.physical_index(&old_physical, count)
						.assert_ok()
						.assert_some();
					assert!(matches!(entries.entries, PhysicalIndexEntries::Count(1)));
					let mut cursor = old
						.query_index_probe_cursor(
							&snapshot,
							"edge",
							Some(&fixture.graph),
							unique,
							&[Value::from(10)],
						)
						.assert_ok();
					assert_eq!(cursor.next_row().assert_ok().assert_some().record_id, record);
					assert!(cursor.next_row().assert_ok().is_none());
				}
				drop(old_physical);
				drop(snapshot);
				drop(old);
				drop(before);
				drop(root);
				fixture.store.checkpoint(CheckpointMode::Restart).assert_ok();
				fixture = support::reopen(fixture);
				assert_indexes(&fixture, class, &[(replacement, replacement_value)]);
				assert_class_slots(&fixture, class, &[0]);
				if class != "edge" {
					assert_indexes(&fixture, "edge", &[]);
				}
				if class != "edge" {
					assert_class_slots(&fixture, "edge", &[]);
				}
				support::assert_bulk_image(&fixture);
			}
		}
	}
}

/// Independent EDGE writers may share endpoints and remap both allocations.
#[test]
fn global_edge_late_replay_merges_shared_endpoint_adjacency() {
	for io_mode in [IoMode::Sync, IoMode::NativeAsync] {
		if io_mode == IoMode::NativeAsync && !cfg!(feature = "native-async-io") {
			continue;
		}
		let mut fixture = fixture(io_mode);
		let first = "edge:first".parse::<RecordId>().assert_ok();
		let late_record = "edge:late".parse::<RecordId>().assert_ok();
		let (late, provisional) = candidate(&fixture, &late_record, 20);
		candidate(&fixture, &first, 10).0.commit().assert_ok();
		let files = support::local(&fixture.store).state.lock().file_catalog.files.clone();
		let boundary = support::local(&fixture.store).wal.snapshot().visible_lsn();
		late.commit().assert_ok();
		let root = fixture.store.inner.current_root();
		let a = root.resolve_atom(&first).assert_some();
		let b = root.resolve_atom(&late_record).assert_some();
		assert_eq!((a.slot(), b.slot()), (2, 3));
		let actual = root.membership(fixture.graph.id(), b).assert_some();
		assert_eq!((provisional.slot(), actual.slot()), (0, 1));
		drop(root);
		assert_fixed_files(&fixture.store, &files, &[fixture.graph.id()]);
		assert_hot_frames(&fixture, boundary, &[0], &[0]);
		assert_indexes(&fixture, "edge", &[(first.clone(), 10), (late_record.clone(), 20)]);
		support::assert_bulk_image(&fixture);
		fixture.store.checkpoint(CheckpointMode::Restart).assert_ok();
		fixture = support::reopen(fixture);
		assert_indexes(&fixture, "edge", &[(first, 10), (late_record, 20)]);
		support::assert_bulk_image(&fixture);
	}
}

/// A stale endpoint is rejected before either Base or incidence publication.
#[test]
fn global_edge_late_replay_rejects_deleted_endpoint_without_publication() {
	for io_mode in [IoMode::Sync, IoMode::NativeAsync] {
		if io_mode == IoMode::NativeAsync && !cfg!(feature = "native-async-io") {
			continue;
		}
		let fixture = fixture(io_mode);
		let (late, _) = candidate(&fixture, &"edge:late".parse::<RecordId>().assert_ok(), 20);
		let mut remove = fixture.store.begin();
		remove.delete_atom_cascade(&"node:a".parse::<RecordId>().assert_ok(), true).assert_ok();
		remove.commit().assert_ok();
		let local = support::local(&fixture.store);
		let stamp = fixture.store.inner.current_root().commit_ts;
		let boundary = local.wal.snapshot().visible_lsn();
		let files = local.state.lock().file_catalog.files.clone();
		let directories = generation_directories(&fixture.directory.0);
		late.commit().assert_err();
		assert_eq!(fixture.store.inner.current_root().commit_ts, stamp);
		assert_eq!(local.wal.snapshot().visible_lsn(), boundary);
		assert_eq!(local.state.lock().file_catalog.files, files);
		assert_eq!(generation_directories(&fixture.directory.0), directories);
		assert_indexes(&fixture, "edge", &[]);
		support::assert_bulk_image(&fixture);
	}
}

/// CASCADE must remove every graph occurrence and preserve both pinned views.
#[test]
fn global_edge_cascade_publishes_all_reverse_memberships_atomically() {
	for io_mode in [IoMode::Sync, IoMode::NativeAsync] {
		if io_mode == IoMode::NativeAsync && !cfg!(feature = "native-async-io") {
			continue;
		}
		let mut fixture = fixture(io_mode);
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
		let nodes = ["node:a", "node:b"].map(|record| {
			seed.attach_node(other.id(), &record.parse::<RecordId>().assert_ok(), None, None)
				.assert_ok()
		});
		seed.commit().assert_ok();
		let record = "edge:shared".parse::<RecordId>().assert_ok();
		let (mut write, edge) = candidate(&fixture, &record, 10);
		let other_edge = write
			.attach_edge(
				other.id(),
				&record,
				None,
				EdgeTopology::directed([nodes[0]], [nodes[1]]),
				None,
				None,
			)
			.assert_ok();
		let files = support::local(&fixture.store).state.lock().file_catalog.files.clone();
		write.commit().assert_ok();
		assert_fixed_files(&fixture.store, &files, &[fixture.graph.id(), other.id()]);
		assert_class_slots(&fixture, "edge", &[edge.slot()]);
		oracles::assert_class_slots_in(&fixture, other.id(), "edge", &[other_edge.slot()]);
		fixture.store.checkpoint(CheckpointMode::Restart).assert_ok();
		fixture = support::reopen(fixture);
		assert_class_slots(&fixture, "edge", &[edge.slot()]);
		oracles::assert_class_slots_in(&fixture, other.id(), "edge", &[other_edge.slot()]);
		let mut old = fixture.store.begin();
		let snapshot = old.query_snapshot().assert_ok();
		let files = support::local(&fixture.store).state.lock().file_catalog.files.clone();
		let mut remove = fixture.store.begin();
		remove.delete_atom_cascade(&record, true).assert_ok();
		remove.commit().assert_ok();
		assert_fixed_files(&fixture.store, &files, &[fixture.graph.id(), other.id()]);
		assert_class_slots(&fixture, "edge", &[]);
		oracles::assert_class_slots_in(&fixture, other.id(), "edge", &[]);
		assert_indexes(&fixture, "edge", &[]);
		support::assert_bulk_image(&fixture);
		fixture.store.checkpoint(CheckpointMode::Passive).assert_ok();
		assert_adjacency(&old, &snapshot, &fixture.graph, &fixture.nodes, Some(edge));
		assert_adjacency(&old, &snapshot, &other, &nodes, Some(other_edge));
		drop(snapshot);
		drop(old);
		fixture.store.checkpoint(CheckpointMode::Restart).assert_ok();
		fixture = support::reopen(fixture);
		assert_class_slots(&fixture, "edge", &[]);
		oracles::assert_class_slots_in(&fixture, other.id(), "edge", &[]);
		let root = fixture.store.inner.current_root();
		assert!(root.resolve_atom(&record).is_none());
		assert_eq!(root.statistics.graph(other.id()).assert_some().edge_count, 0);
		let mut read = fixture.store.begin();
		let snapshot = read.query_snapshot().assert_ok();
		assert_adjacency(&read, &snapshot, &other, &nodes, None);
		assert_indexes(&fixture, "edge", &[]);
		support::assert_bulk_image(&fixture);
	}
}
