//! Property-key append must preserve file identities, snapshots and durability.

use super::*;
use crate::values::Object;

/// Sync admits new top-level and nested keys without rebuilding the graph.
#[test]
fn dictionary_point_sync_preserves_files_and_reopens() {
	dictionary_point_publication(IoMode::Sync);
}

/// Actual NativeAsync uses the same atomic dictionary and data publication.
#[cfg(feature = "native-async-io")]
#[test]
fn dictionary_point_native_async_preserves_files_and_reopens() {
	dictionary_point_publication(IoMode::NativeAsync);
}

/// Sync failures retain one atomic dictionary/data publication boundary.
#[test]
fn dictionary_point_sync_failures_preserve_atomicity() {
	dictionary_failure_contract(IoMode::Sync);
}

/// NativeAsync rejects partial preparation and recovers uncertain WAL together.
#[cfg(feature = "native-async-io")]
#[test]
fn dictionary_point_native_async_failures_preserve_atomicity() {
	dictionary_failure_contract(IoMode::NativeAsync);
}

/// Pure dictionary and overlay commits reject ahead subordinate checkpoint
/// metadata.
#[test]
fn dictionary_point_sync_uses_database_checkpoint_authority() {
	dictionary_checkpoint_authority(IoMode::Sync);
}

/// Actual NativeAsync shares the database checkpoint fence after a failed
/// write.
#[cfg(feature = "native-async-io")]
#[test]
fn dictionary_point_native_async_uses_database_checkpoint_authority() {
	dictionary_checkpoint_authority(IoMode::NativeAsync);
}

/// Exercises both dictionary-only and overlay-only combined AtomStore headers.
fn dictionary_checkpoint_authority(io_mode: IoMode) {
	let (directory, store, graph, keys, records) = overlay_failure_fixture(io_mode);
	let local = match &store.inner.backend {
		StoreBackend::Local(local) => Some(local),
		StoreBackend::InMemory => None,
	}
	.assert_some();
	let files = local.state.lock().file_catalog.files.clone();
	let mut source = store.begin();
	source.set_base_property(&records[0], keys[1], "source".into()).assert_ok();
	source.commit().assert_ok();
	let mut pending = store.begin();
	let unused = pending.intern_property("unused_after_checkpoint").assert_ok();
	store.checkpoint(CheckpointMode::Restart).assert_ok();
	let authority = local.state.lock().database_superblock.clone();
	pending.commit().assert_ok();
	assert_eq!(local.state.lock().atom_superblock.checkpoint_generation, authority.generation);
	let blocker =
		crate::storage::metadata_cow::temporary_path(&local.state.lock().database_superblock_path);
	fs::create_dir(&blocker).assert_ok();
	store.checkpoint(CheckpointMode::Restart).assert_err();
	fs::remove_dir(&blocker).assert_ok();
	assert_eq!(local.state.lock().database_superblock.generation, authority.generation);
	let mut next = store.begin();
	let fresh = next.intern_property("overlay_after_failed_checkpoint").assert_ok();
	next.set_overlay(graph.id(), &records[1], fresh, Value::Null).assert_ok();
	next.commit().assert_ok();
	let header = local.state.lock().atom_superblock;
	assert_eq!(header.checkpoint_generation, authority.generation);
	assert_eq!(header.backfilled_lsn, authority.checkpoint_lsn);
	assert_eq!(header.checkpoint_record_lsn, authority.checkpoint_record_lsn);
	assert_eq!(local.state.lock().file_catalog.files, files);
	drop(store);
	let options = LocalOptions {
		io_mode,
		buffer_pool_pages: 64,
		..LocalOptions::default()
	};
	let reopened = Datastore::open_local(&directory.0, options).assert_ok();
	assert_eq!(reopened.local_storage_status().assert_some().options().io_mode, io_mode);
	assert_eq!(
		reopened.inner.current_root().dictionary.get("unused_after_checkpoint"),
		Some(unused)
	);
	assert_overlay_property(&reopened, graph.id(), &records[1], fresh, Value::Null);
}

/// Adds one nested Base key and one overlay key in the same candidate.
fn dictionary_candidate(
	write: &mut crate::datastore::Transaction,
	graph: crate::graph::GraphId,
	keys: [PropertyKeyId; 2],
	records: &[RecordId; 2],
	resize: bool,
) {
	let mut object = Object::new();
	object.insert("new_nested", Value::Null);
	write.set_base_property(&records[0], keys[1], Value::Object(object)).assert_ok();
	let fresh = write.intern_property("new_overlay").assert_ok();
	write.set_overlay(graph, &records[1], fresh, Value::Null).assert_ok();
	if resize {
		intern_resize_keys(write);
	}
}

/// Exceeds a single hash page's 612-entry load threshold in one append.
fn intern_resize_keys(write: &mut crate::datastore::Transaction) -> Vec<PropertyKeyId> {
	(0..620).map(|id| write.intern_property(&format!("resize-key-{id}")).assert_ok()).collect()
}

/// Validates both decoded physical values and their dictionary IDs after
/// recovery.
fn assert_dictionary_candidate(
	store: &Datastore,
	graph: crate::graph::GraphId,
	keys: [PropertyKeyId; 2],
	records: &[RecordId; 2],
	published: bool,
	resize: bool,
) {
	let root = store.inner.current_root();
	assert_eq!(root.dictionary.get("new_nested").is_some(), published);
	assert_eq!(root.dictionary.get("new_overlay").is_some(), published);
	for id in 0..620 {
		assert_eq!(root.dictionary.get(&format!("resize-key-{id}")).is_some(), published && resize);
	}
	let payload = if published {
		let mut object = Object::new();
		object.insert("new_nested", Value::Null);
		Value::Object(object)
	} else {
		Value::from("base")
	};
	assert_overlay_property(store, graph, &records[0], keys[1], payload);
	if published {
		assert_overlay_property(
			store,
			graph,
			&records[1],
			root.dictionary.get("new_overlay").assert_some(),
			Value::Null,
		);
	}
	assert_overlay_label_property_count(
		store,
		graph,
		if published {
			5
		} else {
			4
		},
	);
}

/// Preparation errors publish nothing; uncertain writers require recovery.
fn dictionary_failure_contract(io_mode: IoMode) {
	for resize in [false, true] {
		dictionary_failure_case(io_mode, resize);
	}
}

/// Runs identical failure boundaries with reused capacity and a replacement
/// range.
fn dictionary_failure_case(io_mode: IoMode, resize: bool) {
	use crate::storage::wal::{CommitFailureGuard, CommitFailurePoint};
	{
		let (_directory, store, graph, keys, records) = overlay_failure_fixture(io_mode);
		let local = match &store.inner.backend {
			StoreBackend::Local(local) => Some(local),
			StoreBackend::InMemory => None,
		}
		.assert_some();
		let stamp = store.inner.current_root().commit_ts;
		let boundary = local.wal.snapshot().visible_lsn();
		let files = local.state.lock().file_catalog.files.clone();
		let ceiling = local.state.lock().atom_superblock.property_dictionary_pages;
		for point in [
			GenerationFailurePoint::AfterBuild,
			GenerationFailurePoint::AfterCatalogStage,
			GenerationFailurePoint::BeforeWalCommit,
		] {
			let mut write = store.begin();
			dictionary_candidate(&mut write, graph.id(), keys, &records, resize);
			let failure = GenerationFailureGuard::set(point);
			write.commit().assert_err();
			drop(failure);
			assert_eq!(store.inner.current_root().commit_ts, stamp);
			assert_eq!(local.wal.snapshot().visible_lsn(), boundary);
			assert_eq!(local.state.lock().file_catalog.files, files);
			assert_eq!(local.state.lock().atom_superblock.property_dictionary_pages, ceiling);
			assert_dictionary_candidate(&store, graph.id(), keys, &records, false, resize);
		}
		let mut write = store.begin();
		dictionary_candidate(&mut write, graph.id(), keys, &records, resize);
		write.commit().assert_ok();
		assert_eq!(local.state.lock().file_catalog.files, files);
		assert_dictionary_candidate(&store, graph.id(), keys, &records, true, resize);
	}
	for point in [
		CommitFailurePoint::BeforeAppend,
		CommitFailurePoint::AfterAppend,
		CommitFailurePoint::AfterFlush,
		CommitFailurePoint::AfterSync,
	] {
		let (directory, store, graph, keys, records) = overlay_failure_fixture(io_mode);
		let local = match &store.inner.backend {
			StoreBackend::Local(local) => Some(local),
			StoreBackend::InMemory => None,
		}
		.assert_some();
		let stamp = store.inner.current_root().commit_ts;
		let files = local.state.lock().file_catalog.files.clone();
		let ceiling = local.state.lock().atom_superblock.property_dictionary_pages;
		let mut write = store.begin();
		dictionary_candidate(&mut write, graph.id(), keys, &records, resize);
		{
			let _failure = CommitFailureGuard::set(point);
			write.commit().assert_err();
		}
		assert_eq!(store.inner.current_root().commit_ts, stamp);
		assert_eq!(local.state.lock().file_catalog.files, files);
		assert_eq!(local.state.lock().atom_superblock.property_dictionary_pages, ceiling);
		let mut retry = store.begin();
		dictionary_candidate(&mut retry, graph.id(), keys, &records, resize);
		retry.commit().assert_err();
		drop(store);
		let options = LocalOptions {
			io_mode,
			buffer_pool_pages: 64,
			..LocalOptions::default()
		};
		let reopened = Datastore::open_local(&directory.0, options).assert_ok();
		assert_eq!(reopened.local_storage_status().assert_some().options().io_mode, io_mode);
		let published = point != CommitFailurePoint::BeforeAppend;
		assert_eq!(reopened.inner.current_root().commit_ts, stamp + u64::from(published));
		assert_dictionary_candidate(&reopened, graph.id(), keys, &records, published, resize);
		reopened.checkpoint(CheckpointMode::Restart).assert_ok();
		drop(reopened);
		let cold = Datastore::open_local(&directory.0, options).assert_ok();
		assert_eq!(cold.local_storage_status().assert_some().options().io_mode, io_mode);
		assert_dictionary_candidate(&cold, graph.id(), keys, &records, published, resize);
	}
}

/// Covers Base, overlay and dictionary-only growth with an old physical reader.
fn dictionary_point_publication(io_mode: IoMode) {
	let (directory, store, graph, keys, records) = overlay_failure_fixture(io_mode);
	let options = LocalOptions {
		io_mode,
		buffer_pool_pages: 64,
		..LocalOptions::default()
	};
	let local = match &store.inner.backend {
		StoreBackend::Local(local) => Some(local),
		StoreBackend::InMemory => None,
	}
	.assert_some();
	let files = local.state.lock().file_catalog.files.clone();
	let prefix = store.inner.current_root().dictionary.clone();
	let mut source = store.begin();
	source.set_base_property(&records[1], keys[1], "source".into()).assert_ok();
	source.commit().assert_ok();
	let source_boundary = local.wal.snapshot().visible_lsn();
	assert!(source_boundary > local.state.lock().database_superblock.checkpoint_lsn);
	let mut old = store.begin();
	let old_snapshot = old.query_snapshot().assert_ok();
	let old_local = old.membership(graph.id(), &records[0]).assert_ok().assert_some();
	let mut leaf = Object::new();
	leaf.insert("fresh_leaf", Value::Null);
	let mut payload = Object::new();
	payload.insert("fresh_child", Value::Object(leaf));
	let payload = Value::Object(payload);
	let mut base = store.begin();
	base.set_base_property(&records[0], keys[1], payload.clone()).assert_ok();
	let dictionary_ceiling = local.state.lock().atom_superblock.property_dictionary_pages;
	let boundary = local.wal.snapshot().visible_lsn();
	base.commit().assert_ok();
	assert_eq!(local.state.lock().file_catalog.files, files);
	let dictionary_frames = local
		.wal
		.recovery_frames()
		.into_iter()
		.filter(|frame| {
			frame.frame_lsn() > boundary
				&& frame.key().file_id().kind()
					== Some(crate::storage::types::DataFileKind::PropertyDictionary)
		})
		.collect::<Vec<_>>();
	assert_eq!(dictionary_frames.len(), 4, "root, old entry tail, one hash page and new entries");
	assert_eq!(
		dictionary_frames
			.iter()
			.filter(|frame| frame.key().page_no().get() >= dictionary_ceiling)
			.count(),
		1
	);
	assert_eq!(
		local.state.lock().atom_superblock.property_dictionary_pages,
		dictionary_ceiling + 1
	);
	assert_overlay_property(&store, graph.id(), &records[0], keys[1], payload.clone());
	assert_overlay_label_property_count(&store, graph.id(), 4);
	let mut overlay_payload = Object::new();
	overlay_payload.insert("fresh_overlay_child", Value::Null);
	let overlay_payload = Value::Object(overlay_payload);
	let mut overlay = store.begin();
	let overlay_key = overlay.intern_property("fresh_overlay").assert_ok();
	overlay.set_overlay(graph.id(), &records[1], overlay_key, overlay_payload.clone()).assert_ok();
	overlay.commit().assert_ok();
	assert_eq!(local.state.lock().file_catalog.files, files);
	assert_overlay_label_property_count(&store, graph.id(), 5);
	let mut top = store.begin();
	let top_key = top.intern_property("fresh_base").assert_ok();
	top.set_base_property(&records[0], top_key, Value::Null).assert_ok();
	top.commit().assert_ok();
	assert_eq!(local.state.lock().file_catalog.files, files);
	assert_overlay_label_property_count(&store, graph.id(), 6);
	let mut intern = store.begin();
	let unused = intern.intern_property("unused_key").assert_ok();
	let resize_keys = intern_resize_keys(&mut intern);
	let resize_start = local.state.lock().atom_superblock.property_dictionary_pages;
	let resize_boundary = local.wal.snapshot().visible_lsn();
	intern.commit().assert_ok();
	assert_eq!(local.state.lock().file_catalog.files, files);
	let frames = local
		.wal
		.recovery_frames()
		.into_iter()
		.filter(|frame| {
			frame.frame_lsn() > resize_boundary
				&& frame.key().file_id().kind()
					== Some(crate::storage::types::DataFileKind::PropertyDictionary)
		})
		.collect::<Vec<_>>();
	assert_eq!(frames.len(), 5, "root, old tail, one entry and two replacement hash pages");
	assert_eq!(
		frames.iter().filter(|frame| frame.key().page_no().get() >= resize_start).count(),
		3
	);
	assert_eq!(local.state.lock().atom_superblock.property_dictionary_pages, resize_start + 3);
	for (id, name) in prefix.iter() {
		assert_eq!(store.inner.current_root().dictionary.resolve(id), Some(name));
	}
	// Automatic checkpoint may already have reached this pin at the 16-page
	// fixture threshold; either path must put its source boundary in home.
	store.checkpoint(CheckpointMode::Passive).assert_ok();
	assert!(local.state.lock().database_superblock.checkpoint_lsn >= source_boundary);
	let old_fields =
		old.query_physical_fields(&old_snapshot, &graph, old_local).assert_ok().assert_some();
	assert_eq!(old_fields.properties.len(), 2);
	assert_eq!(old_fields.properties.get(keys[1]), Some(&Value::from("base")));
	drop(old_snapshot);
	drop(old);
	drop(store);
	let mut reopened = Datastore::open_local(&directory.0, options).assert_ok();
	for _ in 0..2 {
		assert_eq!(reopened.local_storage_status().assert_some().options().io_mode, io_mode);
		reopened.inner.maintenance.shutdown();
		let root = reopened.inner.current_root();
		for (id, key) in resize_keys.iter().enumerate() {
			assert_eq!(root.dictionary.get(&format!("resize-key-{id}")), Some(*key));
		}
		for (name, id) in
			[("fresh_overlay", overlay_key), ("fresh_base", top_key), ("unused_key", unused)]
		{
			assert_eq!(root.dictionary.get(name), Some(id));
		}
		for name in ["fresh_child", "fresh_leaf", "fresh_overlay_child"] {
			assert!(root.dictionary.get(name).is_some());
		}
		assert_overlay_property(&reopened, graph.id(), &records[0], keys[1], payload.clone());
		assert_overlay_property(&reopened, graph.id(), &records[0], top_key, Value::Null);
		assert_overlay_property(
			&reopened,
			graph.id(),
			&records[1],
			overlay_key,
			overlay_payload.clone(),
		);
		assert_overlay_label_property_count(&reopened, graph.id(), 6);
		reopened.checkpoint(CheckpointMode::Restart).assert_ok();
		drop(reopened);
		reopened = Datastore::open_local(&directory.0, options).assert_ok();
	}
}
