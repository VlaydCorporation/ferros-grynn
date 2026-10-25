//! Local durability evidence for recursive write-set admission.

use super::*;
use crate::datastore::IoMode;

/// Runs the same rejection and recovery contract in one actual I/O mode.
fn rejection_preserves_pending_write(io_mode: IoMode) {
	let directory = TestDirectory::new();
	let defaults = LocalOptions::default();
	let mut options = LocalOptions {
		buffer_pool_pages: 16,
		io_mode,
		..defaults
	};
	let store = Datastore::create_local(&directory.0, options).assert_ok();
	assert_eq!(store.local_storage_status().assert_some().options().io_mode, io_mode);
	let mut seed = store.begin();
	seed.define_class(ClassSchema::new("item", AtomKind::Node, []).assert_ok()).assert_ok();
	let key = seed.intern_property("value").assert_ok();
	let record = "item:a".parse::<RecordId>().assert_ok();
	let mut properties = PropertyMap::new();
	properties.insert(key, Value::from("before"));
	seed.create_atom(record.clone(), properties).assert_ok();
	let stamp = seed.commit().assert_ok().commit_ts();
	drop(store);

	options.max_write_set_bytes = 4096;
	let store = Datastore::open_local(&directory.0, options).assert_ok();
	assert_eq!(store.local_storage_status().assert_some().options().io_mode, io_mode);
	let local = match &store.inner.backend {
		StoreBackend::Local(local) => Some(local),
		StoreBackend::InMemory => None,
	}
	.assert_some();
	let pending_pages = local.wal.unbackfilled_page_count();
	let mut write = store.begin();
	write.set_base_property(&record, key, Value::from("accepted")).assert_ok();
	let admitted = write.write_set_bytes();
	assert_eq!(
		write
			.set_base_property(&record, key, Value::from("x".repeat(64 * 1024)))
			.map(|_previous| ())
			.assert_err()
			.status()
			.code()
			.to_string(),
		"FG-54F07"
	);
	assert_eq!(write.write_set_bytes(), admitted);
	assert_eq!(local.wal.snapshot().commit_ts().get(), stamp);
	assert_eq!(local.wal.unbackfilled_page_count(), pending_pages);
	assert_eq!(write.commit().assert_ok().commit_ts(), stamp + 1);
	drop(store);

	let reopened = Datastore::open_local(&directory.0, options).assert_ok();
	assert_eq!(reopened.local_storage_status().assert_some().options().io_mode, io_mode);
	assert_eq!(
		reopened.begin().atom(&record).assert_ok().assert_some().properties().get(key),
		Some(&Value::from("accepted"))
	);
}

/// Sync rejection writes no frame and preserves previously admitted state.
#[test]
fn write_set_local_sync_rejection_preserves_pending_write_and_reopens() {
	rejection_preserves_pending_write(IoMode::Sync);
}

/// NativeAsync uses the same resource boundary and verifies its actual mode.
#[cfg(feature = "native-async-io")]
#[test]
fn write_set_local_native_async_rejection_preserves_pending_write_and_reopens() {
	rejection_preserves_pending_write(IoMode::NativeAsync);
}
