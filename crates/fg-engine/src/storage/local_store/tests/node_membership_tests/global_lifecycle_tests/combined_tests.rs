//! Review GNP-01: combined transitions beyond a NODE's own create/delete.

use super::*;
use crate::storage::local_store::tests::base_lifecycle_tests::assert_base_raw_postings;

/// Reusing both slots in one commit must replace generation-aware index owners.
#[test]
fn global_node_lifecycle_same_commit_reuse_preserves_old_physical_owner() {
	for io_mode in [IoMode::Sync, IoMode::NativeAsync] {
		if io_mode == IoMode::NativeAsync && !cfg!(feature = "native-async-io") {
			continue;
		}
		let mut fixture = fixture(io_mode);
		let record = "item:old".parse::<RecordId>().assert_ok();
		candidate(&fixture, &record, 10).commit().assert_ok();
		let root = fixture.store.inner.current_root();
		let global = root.resolve_atom(&record).assert_some();
		let node = root.membership(fixture.graph.id(), global).assert_some();
		let unique = root
			.schema
			.class("item")
			.assert_some()
			.indexes()
			.find(|index| index.kind() == ClassIndexKind::Unique)
			.assert_some()
			.id()
			.assert_some();
		let mut old = fixture.store.begin();
		let snapshot = old.query_snapshot().assert_ok();
		let local = local_store(&fixture.store);
		let files = local.state.lock().file_catalog.files.clone();
		let boundary = local.wal.snapshot().visible_lsn();
		let replacement = "item:replacement".parse::<RecordId>().assert_ok();
		let mut write = fixture.store.begin();
		write.delete_atom_cascade(&record, true).assert_ok();
		let mut properties = PropertyMap::new();
		properties.insert(fixture.key, 10_i64);
		write.create_atom(replacement.clone(), properties).assert_ok();
		let actual_node =
			write.attach_node(fixture.graph.id(), &replacement, None, None).assert_ok();
		write.commit().assert_ok();
		let current = fixture.store.inner.current_root();
		let actual = current.resolve_atom(&replacement).assert_some();
		assert_eq!((actual.slot(), actual.generation()), (global.slot(), global.generation() + 1));
		assert_eq!(
			(actual_node.slot(), actual_node.generation()),
			(node.slot(), node.generation() + 1)
		);
		assert!(current.resolve_atom(&record).is_none());
		assert_fixed_files(&fixture, &files);
		assert_hot_frames(&fixture, boundary, &[0], &[0]);
		assert_image(&fixture, &[(replacement.clone(), 10)]);
		assert_base_raw_postings(&fixture.store);
		fixture.store.checkpoint(CheckpointMode::Passive).assert_ok();
		assert_image(&fixture, &[(replacement.clone(), 10)]);
		assert_base_raw_postings(&fixture.store);
		assert!(old.query_physical_fields(&snapshot, &fixture.graph, node).assert_ok().is_some());
		let mut cursor = old
			.query_index_probe_cursor(&snapshot, "item", Some(&fixture.graph), unique, &[
				Value::from(10),
			])
			.assert_ok();
		assert_eq!(cursor.next_row().assert_ok().assert_some().record_id, record);
		assert!(cursor.next_row().assert_ok().is_none());
		drop(cursor);
		drop(snapshot);
		drop(old);
		drop(root);
		drop(current);
		fixture.store.checkpoint(CheckpointMode::Restart).assert_ok();
		fixture = reopen(fixture);
		assert_image(&fixture, &[(replacement, 10)]);
		assert_base_raw_postings(&fixture.store);
	}
}

/// A stable Base edit and new membership must update all inherited scopes once.
#[test]
fn global_node_lifecycle_mixed_base_edit_updates_other_graph_indexes_and_label_counts() {
	for io_mode in [IoMode::Sync, IoMode::NativeAsync] {
		if io_mode == IoMode::NativeAsync && !cfg!(feature = "native-async-io") {
			continue;
		}
		let mut fixture = fixture(io_mode);
		let anchor = "item:untouched".parse::<RecordId>().assert_ok();
		let mut seed = fixture.store.begin();
		seed.set_occurrence_label(fixture.other.id(), &anchor, Some("Kept".into())).assert_ok();
		seed.define_index(
			"item",
			ClassIndex::new(
				"other_effective",
				ClassIndexKind::Standard,
				IndexScope::Effective {
					graph: fixture.other.id(),
				},
				Some("value"),
			)
			.assert_ok(),
		)
		.assert_ok();
		let extra = seed.intern_property("extra").assert_ok();
		seed.commit().assert_ok();
		fixture.store.checkpoint(CheckpointMode::Restart).assert_ok();
		fixture = reopen(fixture);
		let local = local_store(&fixture.store);
		let files = local.state.lock().file_catalog.files.clone();
		let mut old = fixture.store.begin();
		let snapshot = old.query_snapshot().assert_ok();
		let owner = old.membership(fixture.other.id(), &anchor).assert_ok().assert_some();
		let record = "item:new".parse::<RecordId>().assert_ok();
		let mut write = candidate(&fixture, &record, 10);
		write.set_base_property(&anchor, fixture.key, Value::from(5)).assert_ok();
		write.set_base_property(&anchor, extra, Value::from(99)).assert_ok();
		write.commit().assert_ok();
		assert_fixed_files(&fixture, &files);
		assert_mixed_scopes(&fixture, &anchor, &record, extra);
		fixture.store.checkpoint(CheckpointMode::Passive).assert_ok();
		assert_mixed_scopes(&fixture, &anchor, &record, extra);
		let previous =
			old.query_physical_fields(&snapshot, &fixture.other, owner).assert_ok().assert_some();
		assert_eq!(previous.properties.get(fixture.key), Some(&Value::from(-1)));
		assert!(previous.properties.get(extra).is_none());
		drop(snapshot);
		drop(old);
		fixture.store.checkpoint(CheckpointMode::Restart).assert_ok();
		fixture = reopen(fixture);
		assert_mixed_scopes(&fixture, &anchor, &record, extra);
	}
}

/// Reads both inherited scalar scopes and the cold physical label counters.
fn assert_mixed_scopes(
	fixture: &Fixture,
	anchor: &RecordId,
	record: &RecordId,
	extra: PropertyKeyId,
) {
	let mut root = fixture.store.inner.current_root().as_ref().clone();
	fixture.store.inner.hydrate_statistics(&mut root).assert_ok();
	let label = crate::stats::LabelStatisticKey {
		kind: AtomKind::Node,
		label: "Kept".into(),
	};
	let counters =
		root.statistics.graph(fixture.other.id()).assert_some().labels.get(&label).assert_some();
	assert_eq!((counters.atom_count, counters.property_count), (1, 2));
	let global = root.resolve_atom(anchor).assert_some();
	assert_eq!(root.atom(global).assert_some().properties().get(extra), Some(&Value::from(99)));
	let class = root.schema.class("item").assert_some();
	let mut read = fixture.store.begin();
	let snapshot = read.query_snapshot().assert_ok();
	let physical = local_store(&fixture.store)
		.capture_query_snapshot(root.commit_ts)
		.assert_ok()
		.assert_some();
	let mut checked = 0;
	for index in class.indexes() {
		checked += 1;
		let (scope, count, rows) = match index.scope() {
			IndexScope::Base => (None, 2, [(5, Some(anchor)), (-1, None), (10, Some(record))]),
			IndexScope::Effective {
				graph,
			} if graph == fixture.other.id() => {
				(Some(&fixture.other), 1, [(5, Some(anchor)), (-1, None), (10, None)])
			}
			IndexScope::Effective {
				graph,
			} => {
				assert_eq!(graph, fixture.graph.id());
				(Some(&fixture.graph), 1, [(10, Some(record)), (5, None), (-1, None)])
			}
		};
		let entries = local_store(&fixture.store)
			.physical_index(&physical, index.id().assert_some())
			.assert_ok()
			.assert_some();
		if let PhysicalIndexEntries::Count(actual) = &entries.entries {
			assert_eq!(*actual, count);
			continue;
		}
		let PhysicalIndexEntries::Scalar(groups) = &entries.entries else {
			unreachable!("mixed fixture changed scalar kind");
		};
		assert_eq!(
			u64::try_from(groups.iter().map(|group| group.atoms.len()).sum::<usize>()).assert_ok(),
			count
		);
		for (key, expected) in rows {
			let mut cursor = read
				.query_index_probe_cursor(&snapshot, "item", scope, index.id().assert_some(), &[
					Value::from(key),
				])
				.assert_ok();
			let row = cursor.next_row().assert_ok();
			if let Some(expected) = expected {
				assert_eq!(&row.assert_some().record_id, expected);
			} else {
				assert!(row.is_none());
			}
			assert!(cursor.next_row().assert_ok().is_none());
		}
	}
	assert_eq!(checked, 5);
}
