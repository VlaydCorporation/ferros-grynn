//! Final class evidence survives same-slot reuse and malformed cold sources.

use super::*;
use crate::{
	storage::{
		graph_index::{build_label_index, visit_label_index_pages},
		page::{PageLsn, PageNo},
		types::PageKey,
	},
	values::storage::DiskAtomRef,
};

/// A transient and live owner of different classes share both allocator slots.
#[test]
fn global_node_transient_same_slot_class_replacement_keeps_both_class_arrays() {
	for io_mode in [IoMode::Sync, IoMode::NativeAsync] {
		if io_mode == IoMode::NativeAsync && !cfg!(feature = "native-async-io") {
			continue;
		}
		let mut fixture = fixture_with_anchor(io_mode, false);
		let mut seed = fixture.store.begin();
		let mut class =
			ClassSchema::new("replacement", AtomKind::Node, [ClassField::new("value", Type::Int)])
				.assert_ok();
		for (name, kind, scope, field) in [
			("base", ClassIndexKind::Standard, IndexScope::Base, Some("value")),
			(
				"effective",
				ClassIndexKind::Unique,
				IndexScope::Effective {
					graph: fixture.graph.id(),
				},
				Some("value"),
			),
			(
				"count",
				ClassIndexKind::Count,
				IndexScope::Effective {
					graph: fixture.graph.id(),
				},
				None,
			),
		] {
			class =
				class.with_index(ClassIndex::new(name, kind, scope, field).assert_ok()).assert_ok();
		}
		seed.define_class(class).assert_ok();
		seed.commit().assert_ok();
		fixture.store.checkpoint(CheckpointMode::Restart).assert_ok();
		fixture = reopen(fixture);
		let files = local_store(&fixture.store).state.lock().file_catalog.files.clone();
		let boundary = local_store(&fixture.store).wal.snapshot().visible_lsn();
		let (mut write, old_node) =
			transient_candidate(&fixture, &"item:gone".parse::<RecordId>().assert_ok());
		let replacement = "replacement:live".parse::<RecordId>().assert_ok();
		let mut properties = PropertyMap::new();
		properties.insert(fixture.key, 20_i64);
		write.create_atom(replacement.clone(), properties).assert_ok();
		let node = write.attach_node(fixture.graph.id(), &replacement, None, None).assert_ok();
		assert_eq!((node.slot(), node.generation()), (old_node.slot(), 1));
		write.commit().assert_ok();
		assert_fixed_files(&fixture, &files);
		assert_hot_frames(&fixture, boundary, &[0], &[0]);
		for cold in [false, true] {
			if cold {
				fixture.store.checkpoint(CheckpointMode::Restart).assert_ok();
				fixture = reopen(fixture);
			}
			let root = fixture.store.inner.current_root();
			let global = root.resolve_atom(&replacement).assert_some();
			assert_eq!((global.slot(), global.generation()), (0, 1));
			assert_eq!(root.membership(fixture.graph.id(), global), Some(node));
			assert_class_slots(&fixture, "item", &[]);
			assert_class_slots(&fixture, "replacement", &[0]);
			assert_base_raw_postings(&fixture.store);
			let mut read = fixture.store.begin();
			let snapshot = read.query_snapshot().assert_ok();
			assert!(
				read.query_physical_fields(&snapshot, &fixture.graph, node).assert_ok().is_some()
			);
			for index in root.schema.class("replacement").assert_some().indexes() {
				let physical = local_store(&fixture.store)
					.capture_query_snapshot(root.commit_ts)
					.assert_ok()
					.assert_some();
				let entries = local_store(&fixture.store)
					.physical_index(&physical, index.id().assert_some())
					.assert_ok()
					.assert_some();
				if index.kind() == ClassIndexKind::Count {
					assert!(matches!(entries.entries, PhysicalIndexEntries::Count(1)));
				} else {
					let scope = (index.scope() != IndexScope::Base).then_some(&fixture.graph);
					let mut cursor = read
						.query_index_probe_cursor(
							&snapshot,
							"replacement",
							scope,
							index.id().assert_some(),
							&[Value::from(20)],
						)
						.assert_ok();
					assert_eq!(cursor.next_row().assert_ok().assert_some().record_id, replacement);
					assert!(cursor.next_row().assert_ok().is_none());
				}
			}
		}
	}
}

/// Physical class postings are compared with explicit expected slot sets.
fn assert_class_slots(fixture: &Fixture, name: &str, expected: &[u64]) {
	let root = fixture.store.inner.current_root();
	let class = root.schema.class(name).assert_some().id().assert_some();
	let suffix = format!("/idx/cls_{}.fgidx", class.get());
	let local = local_store(&fixture.store);
	let state = local.state.lock();
	let mut files = state.file_catalog.files.iter().filter(|entry| {
		entry.space_id == DataSpaceId::Graph(fixture.graph.id())
			&& entry.path.as_str().ends_with(&suffix)
	});
	let file = files.next().assert_some().file_id;
	assert!(files.next().is_none());
	let mut actual = Vec::new();
	visit_label_index_pages::<DiskAtomRef>(
		|number, page| {
			local.read_update_page_into(
				&state,
				PageKey::new(DataSpaceId::Graph(fixture.graph.id()), file, number),
				page,
			)
		},
		AtomKind::Node,
		|atom| {
			actual.push(atom.slot());
			Ok(())
		},
	)
	.assert_ok();
	assert_eq!(actual, expected);
}

/// The ownerless retention branch must reject a nonempty cold empty source.
#[test]
fn global_node_transient_corrupt_cold_class_source_publishes_nothing() {
	for io_mode in [IoMode::Sync, IoMode::NativeAsync] {
		if io_mode == IoMode::NativeAsync && !cfg!(feature = "native-async-io") {
			continue;
		}
		let mut fixture = fixture_with_anchor(io_mode, false);
		let record = "item:gone".parse::<RecordId>().assert_ok();
		transient_candidate(&fixture, &record).0.commit().assert_ok();
		fixture.store.checkpoint(CheckpointMode::Restart).assert_ok();
		let entry = class_file(&fixture);
		{
			let state = local_store(&fixture.store).state.lock();
			let pages = build_label_index(
				&[DiskAtomRef::from_parts(AtomKind::Node, 0).assert_ok()],
				AtomKind::Node,
				PageLsn::ZERO,
			)
			.assert_ok();
			state
				.registry
				.resolve_file(entry.space_id, entry.file_id)
				.assert_ok()
				.file()
				.assert_ok()
				.write_page_raw(PageNo::new(0), &pages[0])
				.assert_ok();
		}
		fixture = reopen(fixture);
		let local = local_store(&fixture.store);
		let stamp = fixture.store.inner.current_root().commit_ts;
		let boundary = local.wal.snapshot().visible_lsn();
		let files = local.state.lock().file_catalog.files.clone();
		let directories = generation_directories(&fixture.directory.0);
		transient_candidate(&fixture, &record).0.commit().assert_err();
		assert_eq!(fixture.store.inner.current_root().commit_ts, stamp);
		assert_eq!(local.wal.snapshot().visible_lsn(), boundary);
		assert_eq!(local.state.lock().file_catalog.files, files);
		assert_eq!(generation_directories(&fixture.directory.0), directories);
		assert_eq!(fixture.store.inner.current_root().atoms.slot_generation(0), Some(1));
	}
}
