//! Global and local allocator transitions must use their final replay owners.

use super::*;

/// The last class member can disappear before both slots are reused cold.
#[test]
fn global_node_lifecycle_sync_delete_reuse_preserves_snapshots() {
	delete_reuse(IoMode::Sync);
}

/// NativeAsync preserves old physical Count and allocator tombstones too.
#[cfg(feature = "native-async-io")]
#[test]
fn global_node_lifecycle_native_async_delete_reuse_preserves_snapshots() {
	delete_reuse(IoMode::NativeAsync);
}

/// Pins the successful insertion before deleting and reusing its two owners.
fn delete_reuse(io_mode: IoMode) {
	let mut fixture = fixture(io_mode);
	let record = "item:old".parse::<RecordId>().assert_ok();
	candidate(&fixture, &record, 10).commit().assert_ok();
	let source = fixture.store.inner.current_root();
	let global = source.resolve_atom(&record).assert_some();
	let node = source.membership(fixture.graph.id(), global).assert_some();
	let local = local_store(&fixture.store);
	let files = local.state.lock().file_catalog.files.clone();
	let snapshot = local.capture_query_snapshot(source.commit_ts).assert_ok().assert_some();
	let mut old = fixture.store.begin();
	let old_query = old.query_snapshot().assert_ok();
	let count = source
		.schema
		.class("item")
		.assert_some()
		.indexes()
		.find(|index| index.kind() == ClassIndexKind::Count)
		.assert_some()
		.id()
		.assert_some();
	let boundary = local.wal.snapshot().visible_lsn();
	let mut delete = fixture.store.begin();
	delete.delete_atom_cascade(&record, true).assert_ok();
	delete.commit().assert_ok();
	assert_fixed_files(&fixture, &files);
	assert_hot_frames(&fixture, boundary, &[0], &[0]);
	assert_image(&fixture, &[]);
	assert!(fixture.store.inner.current_root().resolve_atom(&record).is_none());
	fixture.store.checkpoint(CheckpointMode::Passive).assert_ok();
	assert!(matches!(
		local.physical_index(&snapshot, count).assert_ok().assert_some().entries,
		PhysicalIndexEntries::Count(1)
	));
	assert!(source.atom(global).is_some());
	assert_eq!(
		old.query_physical_fields(&old_query, &fixture.graph, node)
			.assert_ok()
			.assert_some()
			.properties
			.get(fixture.key),
		Some(&Value::from(10))
	);
	drop(old_query);
	drop(old);
	drop(snapshot);
	drop(source);
	fixture.store.checkpoint(CheckpointMode::Restart).assert_ok();
	fixture = reopen(fixture);
	assert_image(&fixture, &[]);
	let replacement = "item:replacement".parse::<RecordId>().assert_ok();
	let files = local_store(&fixture.store).state.lock().file_catalog.files.clone();
	candidate(&fixture, &replacement, 20).commit().assert_ok();
	assert_fixed_files(&fixture, &files);
	let root = fixture.store.inner.current_root();
	let actual = root.resolve_atom(&replacement).assert_some();
	let actual_node = root.membership(fixture.graph.id(), actual).assert_some();
	assert_eq!((actual.slot(), actual.generation()), (global.slot(), global.generation() + 1));
	assert_eq!(
		(actual_node.slot(), actual_node.generation()),
		(node.slot(), node.generation() + 1)
	);
	drop(root);
	fixture = reopen(fixture);
	assert_image(&fixture, &[(replacement, 20)]);
}

/// Independent commits force both allocator handles to move during replay.
#[test]
fn global_node_lifecycle_late_writer_remaps_both_allocators() {
	for io_mode in [IoMode::Sync, IoMode::NativeAsync] {
		if io_mode == IoMode::NativeAsync && !cfg!(feature = "native-async-io") {
			continue;
		}
		let mut fixture = fixture(io_mode);
		let first = "item:first".parse::<RecordId>().assert_ok();
		let late_record = "item:late".parse::<RecordId>().assert_ok();
		let late = candidate(&fixture, &late_record, 20);
		candidate(&fixture, &first, 10).commit().assert_ok();
		let files = local_store(&fixture.store).state.lock().file_catalog.files.clone();
		late.commit().assert_ok();
		assert_fixed_files(&fixture, &files);
		let root = fixture.store.inner.current_root();
		let first_global = root.resolve_atom(&first).assert_some();
		let late_global = root.resolve_atom(&late_record).assert_some();
		assert_eq!((first_global.slot(), late_global.slot()), (1, 2));
		assert_eq!(
			(
				root.membership(fixture.graph.id(), first_global).assert_some().slot(),
				root.membership(fixture.graph.id(), late_global).assert_some().slot()
			),
			(0, 1)
		);
		drop(root);
		fixture.store.checkpoint(CheckpointMode::Restart).assert_ok();
		fixture = reopen(fixture);
		assert_image(&fixture, &[(first, 10), (late_record, 20)]);
	}
}

/// The two physical Hot layouts cross different page boundaries together.
#[test]
fn global_node_lifecycle_sync_hot_boundaries_match_bulk() {
	boundaries(IoMode::Sync);
}

/// Actual NativeAsync publishes the same two-page cuts and final identities.
#[cfg(feature = "native-async-io")]
#[test]
fn global_node_lifecycle_native_async_hot_boundaries_match_bulk() {
	boundaries(IoMode::NativeAsync);
}

/// Pads global capacity to339 and local capacity to101 before the candidate.
fn boundaries(io_mode: IoMode) {
	let mut fixture = fixture(io_mode);
	let mut seed = fixture.store.begin();
	let mut expected = Vec::new();
	for index in 0..338 {
		let record = format!("item:seed{index}").parse::<RecordId>().assert_ok();
		let value = 1000 + i64::from(index);
		let mut properties = PropertyMap::new();
		properties.insert(fixture.key, value);
		seed.create_atom(record.clone(), properties).assert_ok();
		if index < 101 {
			seed.attach_node(fixture.graph.id(), &record, None, None).assert_ok();
			expected.push((record, value));
		}
	}
	seed.commit().assert_ok();
	fixture.store.checkpoint(CheckpointMode::Restart).assert_ok();
	fixture = reopen(fixture);
	let local = local_store(&fixture.store);
	let files = local.state.lock().file_catalog.files.clone();
	let boundary = local.wal.snapshot().visible_lsn();
	let mut write = fixture.store.begin();
	for index in 0..2 {
		let record = format!("item:appended{index}").parse::<RecordId>().assert_ok();
		let value = 4000 + i64::from(index);
		let mut properties = PropertyMap::new();
		properties.insert(fixture.key, value);
		write.create_atom(record.clone(), properties).assert_ok();
		write.attach_node(fixture.graph.id(), &record, None, None).assert_ok();
		expected.push((record, value));
	}
	write.commit().assert_ok();
	assert_fixed_files(&fixture, &files);
	assert_hot_frames(&fixture, boundary, &[0, 1], &[0, 1]);
	assert_eq!(fixture.store.inner.current_root().atoms.slot_capacity(), 341);
	assert_image(&fixture, &expected);
	fixture.store.checkpoint(CheckpointMode::Restart).assert_ok();
	fixture = reopen(fixture);
	assert_image(&fixture, &expected);
}

/// A real overflow replaces selected trees while keeping unrelated graph files.
#[test]
fn global_node_lifecycle_selected_tree_fallback_is_atomic() {
	for io_mode in [IoMode::Sync, IoMode::NativeAsync] {
		if io_mode == IoMode::NativeAsync && !cfg!(feature = "native-async-io") {
			continue;
		}
		let mut fixture = fixture(io_mode);
		let local = local_store(&fixture.store);
		let files = local.state.lock().file_catalog.files.clone();
		let primary = local.state.lock()._database_catalog.atom_store.primary.file_id;
		let make_write = || {
			let mut write = fixture.store.begin();
			for index in 0..1024 {
				let record = format!("item:fallback{index}").parse::<RecordId>().assert_ok();
				let mut properties = PropertyMap::new();
				properties.insert(fixture.key, i64::from(index));
				write.create_atom(record.clone(), properties).assert_ok();
				write.attach_node(fixture.graph.id(), &record, None, None).assert_ok();
			}
			write
		};
		let stamp = fixture.store.inner.current_root().commit_ts;
		let boundary = local.wal.snapshot().visible_lsn();
		{
			let write = make_write();
			let _failure = GenerationFailureGuard::set(GenerationFailurePoint::BeforeWalCommit);
			write.commit().assert_err();
		}
		assert_eq!(fixture.store.inner.current_root().commit_ts, stamp);
		assert_eq!(local.wal.snapshot().visible_lsn(), boundary);
		assert_eq!(local.state.lock().file_catalog.files, files);
		make_write().commit().assert_ok();
		assert_fixed_files(&fixture, &files);
		assert_ne!(local.state.lock()._database_catalog.atom_store.primary.file_id, primary);
		let expected = (0..1024)
			.map(|index| {
				(format!("item:fallback{index}").parse::<RecordId>().assert_ok(), i64::from(index))
			})
			.collect::<Vec<_>>();
		assert_image(&fixture, &expected);
		fixture.store.checkpoint(CheckpointMode::Restart).assert_ok();
		fixture = reopen(fixture);
		assert_image(&fixture, &expected);
	}
}
