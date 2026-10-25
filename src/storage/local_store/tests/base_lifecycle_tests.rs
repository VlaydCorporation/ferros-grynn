//! Base lifecycle publication includes allocator state even without net rows.

use super::*;
use crate::datastore::{IoMode, QueryStatisticsDemand, Transaction};

/// Appending across a hot-page boundary must preserve unrelated file
/// identities.
#[test]
fn base_lifecycle_sync_preserves_files_and_exact_counters() {
	base_lifecycle(IoMode::Sync);
}

/// NativeAsync must publish the same slot and counter deltas through reopen.
#[cfg(feature = "native-async-io")]
#[test]
fn base_lifecycle_native_async_preserves_files_and_exact_counters() {
	base_lifecycle(IoMode::NativeAsync);
}

/// Create followed by Delete still advances the persisted allocator state.
#[test]
fn base_transient_allocation_publishes_capacity_without_replacing_files() {
	let directory = TestDirectory::new();
	let options = LocalOptions::default();
	let store = Datastore::create_local(&directory.0, options).assert_ok();
	let key = seed_base_lifecycle(&store);
	store.checkpoint(CheckpointMode::Restart).assert_ok();
	store.inner.maintenance.shutdown();
	let local = match &store.inner.backend {
		StoreBackend::Local(local) => local,
		StoreBackend::InMemory => unreachable!("local fixture has an in-memory backend"),
	};
	let files = local.state.lock().file_catalog.files.clone();
	let property_ceiling = local.state.lock().atom_superblock.properties_pages;
	let mut write = store.begin();
	insert_base_lifecycle_item(&mut write, key, "item:transient", 99999);
	write.delete_atom(&"item:transient".parse::<RecordId>().assert_ok()).assert_ok();
	write.commit().assert_ok();
	{
		let state = local.state.lock();
		assert_eq!(state.file_catalog.files, files, "transient allocation rebuilt the generation");
		assert_eq!(state.atom_superblock.atom_capacity, 340);
		assert_eq!(state.atom_superblock.properties_pages, property_ceiling);
	}
	drop(store);
	let reopened = Datastore::open_local(&directory.0, options).assert_ok();
	let root = reopened.inner.current_root();
	assert_eq!(root.atoms.slot_capacity(), 340);
	assert_eq!(root.atoms.slot_generation(339), Some(1));
	assert!(root.resolve_atom(&"item:transient".parse::<RecordId>().assert_ok()).is_none());
	assert_base_lifecycle_counts(&reopened, 339, 0);
}

/// A late old writer hydrates its own root and publishes remapped final slots.
#[test]
fn base_late_deferred_writer_preserves_independent_current_rows() {
	use crate::datastore::{IsolationLevel, TransactionAccess, TransactionOptions};
	let directory = TestDirectory::new();
	let options = LocalOptions::default();
	let store = Datastore::create_local(&directory.0, options).assert_ok();
	let key = seed_base_lifecycle(&store);
	store.checkpoint(CheckpointMode::Restart).assert_ok();
	drop(store);
	let store = Datastore::open_local(&directory.0, options).assert_ok();
	assert!(store.inner.current_root().statistics.auxiliary_deferred());
	store.inner.maintenance.shutdown();
	let local = match &store.inner.backend {
		StoreBackend::Local(local) => local,
		StoreBackend::InMemory => unreachable!("local fixture has an in-memory backend"),
	};
	let files = local.state.lock().file_catalog.files.clone();
	let mut old = store.begin_with(TransactionOptions::new(
		IsolationLevel::Snapshot,
		TransactionAccess::ReadWrite,
	));
	let mut other = store.begin();
	insert_base_lifecycle_item(&mut other, key, "item:n0339", 678);
	other.commit().assert_ok();
	let old_snapshot = old.query_snapshot().assert_ok();
	assert!(local.capture_query_snapshot(old_snapshot.commit_ts()).assert_ok().is_none());
	assert!(old.atom(&"item:n0001".parse::<RecordId>().assert_ok()).assert_ok().is_some());
	insert_base_lifecycle_item(&mut old, key, "item:late", 99999);
	old.commit().assert_ok();
	assert_eq!(local.state.lock().file_catalog.files, files);
	assert_base_lifecycle_counts(&store, 341, 0);
	let root = store.inner.current_root();
	let concurrent = root.resolve_atom(&"item:n0339".parse::<RecordId>().assert_ok()).assert_some();
	let late = root.resolve_atom(&"item:late".parse::<RecordId>().assert_ok()).assert_some();
	assert_eq!((concurrent.slot(), late.slot()), (339, 340));
	drop(root);
	drop(old_snapshot);
	store.checkpoint(CheckpointMode::Restart).assert_ok();
	drop(store);
	let reopened = Datastore::open_local(&directory.0, options).assert_ok();
	assert_base_lifecycle_counts(&reopened, 341, 0);
	assert!(
		reopened
			.inner
			.current_root()
			.resolve_atom(&"item:n0339".parse::<RecordId>().assert_ok())
			.is_some()
	);
	assert!(
		reopened
			.inner
			.current_root()
			.resolve_atom(&"item:late".parse::<RecordId>().assert_ok())
			.is_some()
	);
}

/// Primary split and Standard posting fallback allocate distinct files while
/// Unique and Count keep their existing physical identities.
#[test]
fn base_lifecycle_mixed_fallback_is_atomic_and_recovers() {
	use std::collections::BTreeSet;

	use crate::storage::types::DataFileKind;
	let directory = TestDirectory::new();
	let options = LocalOptions::default();
	let store = Datastore::create_local(&directory.0, options).assert_ok();
	let keys = seed_mixed_lifecycle(&store);
	store.checkpoint(CheckpointMode::Restart).assert_ok();
	store.inner.maintenance.shutdown();
	let local = match &store.inner.backend {
		StoreBackend::Local(local) => local,
		StoreBackend::InMemory => unreachable!("local fixture has an in-memory backend"),
	};
	let files = local.state.lock().file_catalog.files.clone();
	let primary = local.state.lock()._database_catalog.atom_store.primary;
	let primary_pin = local
		.state
		.lock()
		.registry
		.resolve_file(crate::datastore::DataSpaceId::Database, primary.file_id)
		.assert_ok();
	let old_path = files
		.iter()
		.find(|entry| entry.file_id == primary.file_id)
		.assert_some()
		.path
		.resolve(&directory.0);
	let pin = local
		.capture_query_snapshot(store.inner.current_root().commit_ts)
		.assert_ok()
		.assert_some();
	let stamp = store.inner.current_root().commit_ts;
	let boundary = local.wal.snapshot().visible_lsn();
	let freelists = base_freelist_images(local, &directory.0);
	for point in [
		GenerationFailurePoint::AfterBuild,
		GenerationFailurePoint::AfterFileSync,
		GenerationFailurePoint::AfterCatalogStage,
		GenerationFailurePoint::BeforeWalCommit,
	] {
		let mut write = store.begin();
		insert_mixed_lifecycle(&mut write, keys);
		let guard = super::GenerationFailureGuard::set(point);
		write.commit().assert_err();
		drop(guard);
		assert_eq!(store.inner.current_root().commit_ts, stamp);
		assert_eq!(local.wal.snapshot().visible_lsn(), boundary);
		assert_eq!(local.state.lock().file_catalog.files, files);
		assert_base_lifecycle_counts(&store, 8, 0);
	}
	let mut write = store.begin();
	insert_mixed_lifecycle(&mut write, keys);
	write.commit().assert_ok();
	{
		let state = local.state.lock();
		assert_ne!(state._database_catalog.atom_store.primary.file_id, primary.file_id);
		let replaced = files
			.iter()
			.filter(|entry| !state.file_catalog.files.contains(entry))
			.collect::<Vec<_>>();
		assert_eq!(replaced.len(), 2, "only Primary and Standard require new files");
		assert!(replaced.iter().all(|entry| entry.file_id.kind() == Some(DataFileKind::Index)));
		assert!(replaced.iter().any(|entry| entry.file_id == primary.file_id));
		assert_eq!(
			state
				.file_catalog
				.files
				.iter()
				.map(|entry| (entry.space_id, entry.file_id))
				.collect::<BTreeSet<_>>()
				.len(),
			state.file_catalog.files.len()
		);
	}
	assert_eq!(base_freelist_images(local, &directory.0), freelists);
	assert_base_lifecycle_counts(&store, 10, 0);
	assert!(old_path.is_file(), "the exact registered-file handle pins its Primary");
	drop(store);
	drop(pin);
	drop(primary_pin);
	assert!(old_path.is_file(), "the interrupted retirement remains for startup cleanup");
	let reopened = Datastore::open_local(&directory.0, options).assert_ok();
	assert!(
		!old_path.exists(),
		"startup removes the unreachable Primary inside a retained directory"
	);
	assert_base_lifecycle_counts(&reopened, 10, 0);
	reopened.checkpoint(CheckpointMode::Restart).assert_ok();
	drop(reopened);
	assert_base_lifecycle_counts(&Datastore::open_local(&directory.0, options).assert_ok(), 10, 0);
}

/// Uncertain WAL outcomes recover the complete mixed fallback or its source.
#[test]
fn base_lifecycle_uncertain_mixed_fallback_recovers_atomically() {
	uncertain_mixed_fallback(IoMode::Sync);
}

/// NativeAsync preserves the same catalog/Primary/scalar recovery boundary.
#[cfg(feature = "native-async-io")]
#[test]
fn base_lifecycle_uncertain_native_mixed_fallback_recovers_atomically() {
	uncertain_mixed_fallback(IoMode::NativeAsync);
}

/// Complete End records survive an uncertain append/flush/sync result; the
/// in-process root stays unchanged until reopen resolves the writer fence.
fn uncertain_mixed_fallback(io_mode: IoMode) {
	use crate::storage::wal::{CommitFailureGuard, CommitFailurePoint};
	for point in [
		CommitFailurePoint::BeforeAppend,
		CommitFailurePoint::AfterAppend,
		CommitFailurePoint::AfterFlush,
		CommitFailurePoint::AfterSync,
	] {
		let directory = TestDirectory::new();
		let options = LocalOptions {
			io_mode,
			..LocalOptions::default()
		};
		let store = Datastore::create_local(&directory.0, options).assert_ok();
		assert_eq!(store.local_storage_status().assert_some().options().io_mode, io_mode);
		let keys = seed_mixed_lifecycle(&store);
		store.checkpoint(CheckpointMode::Restart).assert_ok();
		store.inner.maintenance.shutdown();
		let local = match &store.inner.backend {
			StoreBackend::Local(local) => local,
			StoreBackend::InMemory => unreachable!("local fixture has an in-memory backend"),
		};
		let stamp = store.inner.current_root().commit_ts;
		let files = local.state.lock().file_catalog.files.clone();
		let primary = local.state.lock()._database_catalog.atom_store.primary;
		let mut candidate = store.begin();
		insert_mixed_lifecycle(&mut candidate, keys);
		{
			let _failure = CommitFailureGuard::set(point);
			assert!(candidate.commit().is_err());
		}
		assert_eq!(store.inner.current_root().commit_ts, stamp);
		assert_eq!(local.state.lock().file_catalog.files, files);
		let mut retry = store.begin();
		insert_mixed_lifecycle(&mut retry, keys);
		assert!(retry.commit().is_err(), "the uncertain writer remains fenced");
		drop(store);
		let reopened = Datastore::open_local(&directory.0, options).assert_ok();
		assert_eq!(reopened.local_storage_status().assert_some().options().io_mode, io_mode);
		reopened.inner.maintenance.shutdown();
		let published = point != CommitFailurePoint::BeforeAppend;
		assert_eq!(reopened.inner.current_root().commit_ts, stamp + u64::from(published));
		let recovered_primary = match &reopened.inner.backend {
			StoreBackend::Local(local) => local.state.lock()._database_catalog.atom_store.primary,
			StoreBackend::InMemory => unreachable!("local fixture has an in-memory backend"),
		};
		assert_eq!(recovered_primary != primary, published);
		let count = if published {
			10
		} else {
			8
		};
		assert_base_lifecycle_counts(&reopened, count, 0);
		reopened.checkpoint(CheckpointMode::Restart).assert_ok();
		drop(reopened);
		let cold = Datastore::open_local(&directory.0, options).assert_ok();
		assert_eq!(cold.local_storage_status().assert_some().options().io_mode, io_mode);
		assert_base_lifecycle_counts(&cold, count, 0);
	}
}

/// Empty root leaves and reused tombstones retain their physical layout.
#[test]
fn base_lifecycle_delete_all_and_transient_reuse_reopen() {
	use crate::storage::PhysicalIndexEntries;
	let directory = TestDirectory::new();
	let options = LocalOptions::default();
	let store = Datastore::create_local(&directory.0, options).assert_ok();
	let keys = seed_mixed_lifecycle(&store);
	store.checkpoint(CheckpointMode::Restart).assert_ok();
	store.inner.maintenance.shutdown();
	let local = match &store.inner.backend {
		StoreBackend::Local(local) => local,
		StoreBackend::InMemory => unreachable!("local fixture has an in-memory backend"),
	};
	let files = local.state.lock().file_catalog.files.clone();
	let pin = local
		.capture_query_snapshot(store.inner.current_root().commit_ts)
		.assert_ok()
		.assert_some();
	let index = store
		.inner
		.current_root()
		.schema
		.class("item")
		.assert_some()
		.indexes()
		.find(|index| index.kind() == ClassIndexKind::Count)
		.assert_some()
		.id()
		.assert_some();
	let mut remove = store.begin();
	for ordinal in 0..8 {
		remove.delete_atom(&format!("item:n{ordinal}").parse::<RecordId>().assert_ok()).assert_ok();
	}
	remove.commit().assert_ok();
	assert_eq!(local.state.lock().file_catalog.files, files);
	assert_base_lifecycle_counts(&store, 0, 0);
	let slot = store.inner.current_root().atoms.slot_generation(7).assert_some();
	let mut transient = store.begin();
	insert_base_lifecycle_item(&mut transient, keys[0], "item:transient", 0);
	transient.delete_atom(&"item:transient".parse::<RecordId>().assert_ok()).assert_ok();
	transient.commit().assert_ok();
	assert_eq!(store.inner.current_root().atoms.slot_capacity(), 8);
	assert_eq!(store.inner.current_root().atoms.slot_generation(7), Some(slot + 1));
	assert_eq!(local.state.lock().file_catalog.files, files);
	store.checkpoint(CheckpointMode::Passive).assert_ok();
	let decoded = local.physical_index(&pin, index).assert_ok().assert_some();
	let PhysicalIndexEntries::Count(count) = &decoded.entries else {
		unreachable!("fixture has a Count index");
	};
	assert_eq!(*count, 8, "a first decode after checkpoint retains the old Count population");
	drop(decoded);
	drop(pin);
	store.checkpoint(CheckpointMode::Restart).assert_ok();
	drop(store);
	let reopened = Datastore::open_local(&directory.0, options).assert_ok();
	assert_base_lifecycle_counts(&reopened, 0, 0);
	assert_eq!(reopened.inner.current_root().atoms.slot_generation(7), Some(slot + 1));
	let mut write = reopened.begin();
	let mut properties = PropertyMap::new();
	properties.insert(keys[0], Value::from(1));
	properties.insert(keys[1], Value::from(1234));
	write.create_atom("item:replacement".parse::<RecordId>().assert_ok(), properties).assert_ok();
	write.commit().assert_ok();
	assert_eq!(
		reopened
			.inner
			.current_root()
			.resolve_atom(&"item:replacement".parse::<RecordId>().assert_ok())
			.assert_some()
			.generation(),
		slot + 1
	);
	assert_base_lifecycle_counts(&reopened, 1, 0);
}

/// Eight equal Standard postings fit inline; Unique uses a separate field.
fn seed_mixed_lifecycle(store: &Datastore) -> [crate::values::PropertyKeyId; 3] {
	let mut seed = store.begin();
	let mut class = ClassSchema::new("item", AtomKind::Node, [
		ClassField::new("value", Type::Int),
		ClassField::new("serial", Type::Int),
		ClassField::new("payload", Type::String),
	])
	.assert_ok();
	for (name, kind, field) in [
		("base_standard", ClassIndexKind::Standard, Some("value")),
		("base_unique", ClassIndexKind::Unique, Some("serial")),
		("base_count", ClassIndexKind::Count, None),
	] {
		class = class
			.with_index(ClassIndex::new(name, kind, IndexScope::Base, field).assert_ok())
			.assert_ok();
	}
	seed.define_class(class).assert_ok();
	seed.define_class(ClassSchema::new("link", AtomKind::Edge, []).assert_ok()).assert_ok();
	let keys = ["value", "serial", "payload"].map(|name| seed.intern_property(name).assert_ok());
	for index in 0..8 {
		let mut properties = PropertyMap::new();
		properties.insert(keys[0], Value::from(0));
		properties.insert(keys[1], Value::from(index));
		seed.create_atom(format!("item:n{index}").parse::<RecordId>().assert_ok(), properties)
			.assert_ok();
	}
	seed.commit().assert_ok();
	keys
}

/// Two long canonical keys split Primary and large properties append a LOB.
fn insert_mixed_lifecycle(write: &mut Transaction, keys: [crate::values::PropertyKeyId; 3]) {
	for index in 8..10 {
		let mut properties = PropertyMap::new();
		properties.insert(keys[0], Value::from(0));
		properties.insert(keys[1], Value::from(index));
		properties.insert(keys[2], Value::from("x".repeat(64 * 1024)));
		write
			.create_atom(
				format!("item:{}{index}", "z".repeat(8200)).parse::<RecordId>().assert_ok(),
				properties,
			)
			.assert_ok();
	}
}

/// Exercises net inserts, cross-kind reuse, transient allocation and identity
/// reuse.
fn base_lifecycle(io_mode: IoMode) {
	let directory = TestDirectory::new();
	let options = LocalOptions {
		io_mode,
		buffer_pool_pages: 256,
		..LocalOptions::default()
	};
	let store = Datastore::create_local(&directory.0, options).assert_ok();
	assert_eq!(store.local_storage_status().assert_some().options().io_mode, io_mode);
	let key = seed_base_lifecycle(&store);
	store.checkpoint(CheckpointMode::Restart).assert_ok();
	drop(store);
	let store = Datastore::open_local(&directory.0, options).assert_ok();
	assert_eq!(store.local_storage_status().assert_some().options().io_mode, io_mode);
	store.inner.maintenance.shutdown();
	let local = match &store.inner.backend {
		StoreBackend::Local(local) => local,
		StoreBackend::InMemory => unreachable!("local fixture has an in-memory backend"),
	};
	let files = local.state.lock().file_catalog.files.clone();
	let freelists = base_freelist_images(local, &directory.0);
	let mut old = store.begin();
	let old_snapshot = old.query_snapshot().assert_ok();
	let demand = QueryStatisticsDemand {
		base_classes: true,
		..QueryStatisticsDemand::default()
	};
	assert_eq!(
		old.query_statistics(&old_snapshot, &demand)
			.assert_ok()
			.base()
			.classes
			.values()
			.sum::<u64>(),
		339
	);
	let original = store
		.inner
		.current_root()
		.resolve_atom(&"item:n0001".parse::<RecordId>().assert_ok())
		.assert_some();
	for (step, nodes, edges, capacity) in
		[(0, 341, 0, 341), (1, 340, 1, 341), (2, 340, 1, 342), (3, 340, 1, 342)]
	{
		let boundary = local.wal.snapshot().visible_lsn();
		let mut write = store.begin();
		apply_base_lifecycle(&mut write, key, step);
		write.commit().assert_ok();
		let state = local.state.lock();
		assert_eq!(
			state.file_catalog.files, files,
			"Base lifecycle replaced unrelated files at step {step}"
		);
		assert_eq!(state.atom_superblock.atom_capacity, capacity);
		assert_eq!(state.atom_superblock.atoms_hot_pages, 2);
		assert_eq!(
			(state.atom_superblock.node_count, state.atom_superblock.edge_count),
			(nodes, edges)
		);
		drop(state);
		assert_eq!(base_freelist_images(local, &directory.0), freelists);
		let frames = local
			.wal
			.recovery_frames()
			.into_iter()
			.filter(|frame| frame.frame_lsn() > boundary)
			.collect::<Vec<_>>();
		use crate::storage::types::DataFileKind;
		assert!(
			frames.iter().all(|frame| frame.key().file_id().kind() != Some(DataFileKind::Freelist))
		);
		assert_eq!(
			frames
				.iter()
				.filter(|frame| frame.key().file_id().kind() == Some(DataFileKind::AtomsHot))
				.count(),
			if step == 0 {
				2
			} else {
				1
			}
		);
		assert_eq!(
			frames
				.iter()
				.filter(|frame| frame.key().file_id().kind() == Some(DataFileKind::Index))
				.count(),
			[4, 4, 0, 3][usize::from(step)]
		);
		assert_base_lifecycle_counts(&store, nodes, edges);
	}
	let root = store.inner.current_root();
	let edge = root.resolve_atom(&"link:replacement".parse::<RecordId>().assert_ok()).assert_some();
	assert_eq!(edge.slot(), original.slot());
	assert_eq!(edge.generation(), original.generation() + 1);
	assert_eq!(edge.kind(), Some(AtomKind::Edge));
	assert_eq!(root.atoms.slot_generation(341), Some(1));
	assert!(root.resolve_atom(&"item:transient".parse::<RecordId>().assert_ok()).is_none());
	store.checkpoint(CheckpointMode::Passive).assert_ok();
	assert_eq!(
		old.query_statistics(&old_snapshot, &demand)
			.assert_ok()
			.base()
			.classes
			.values()
			.sum::<u64>(),
		339,
		"a warm old statistics owner must keep its original counts"
	);
	assert!(old.atom(&"item:n0001".parse::<RecordId>().assert_ok()).assert_ok().is_some());
	drop(old_snapshot);
	drop(old);
	drop(root);
	store.checkpoint(CheckpointMode::Restart).assert_ok();
	drop(store);
	let reopened = Datastore::open_local(&directory.0, options).assert_ok();
	assert_eq!(reopened.local_storage_status().assert_some().options().io_mode, io_mode);
	assert_base_lifecycle_counts(&reopened, 340, 1);
	assert_eq!(reopened.inner.current_root().atoms.slot_capacity(), 342);
	assert_eq!(reopened.inner.current_root().atoms.slot_generation(341), Some(1));
}

/// Seeds one almost-full hot page and three distinct Base index contracts.
fn seed_base_lifecycle(store: &Datastore) -> crate::values::PropertyKeyId {
	let mut seed = store.begin();
	let mut class =
		ClassSchema::new("item", AtomKind::Node, [ClassField::new("value", Type::Int)]).assert_ok();
	for (name, kind, field) in [
		("base_standard", ClassIndexKind::Standard, Some("value")),
		("base_unique", ClassIndexKind::Unique, Some("value")),
		("base_count", ClassIndexKind::Count, None),
	] {
		class = class
			.with_index(ClassIndex::new(name, kind, IndexScope::Base, field).assert_ok())
			.assert_ok();
	}
	seed.define_class(class).assert_ok();
	seed.define_class(ClassSchema::new("link", AtomKind::Edge, []).assert_ok()).assert_ok();
	let key = seed.intern_property("value").assert_ok();
	for index in 0..339 {
		insert_base_lifecycle_item(&mut seed, key, &format!("item:n{index:04}"), index * 2);
	}
	seed.commit().assert_ok();
	key
}

/// Applies each lifecycle operation through the ordinary public transaction
/// API.
fn apply_base_lifecycle(write: &mut Transaction, key: crate::values::PropertyKeyId, step: u8) {
	match step {
		0 => {
			insert_base_lifecycle_item(write, key, "item:n0339", 678);
			insert_base_lifecycle_item(write, key, "item:n0340", 680);
		}
		1 => {
			write.delete_atom(&"item:n0001".parse::<RecordId>().assert_ok()).assert_ok();
			write
				.create_atom("link:replacement".parse::<RecordId>().assert_ok(), PropertyMap::new())
				.assert_ok();
		}
		2 => {
			insert_base_lifecycle_item(write, key, "item:transient", 99999);
			write.delete_atom(&"item:transient".parse::<RecordId>().assert_ok()).assert_ok();
		}
		3 => {
			write.delete_atom(&"item:n0002".parse::<RecordId>().assert_ok()).assert_ok();
			insert_base_lifecycle_item(write, key, "item:n0002", 22222);
		}
		_ => unreachable!("unknown lifecycle fixture step"),
	}
}

/// Keeps the fixture's logical value independent of its physical slot.
fn insert_base_lifecycle_item(
	write: &mut Transaction,
	key: crate::values::PropertyKeyId,
	name: &str,
	value: i32,
) {
	let mut properties = PropertyMap::new();
	properties.insert(key, Value::from(value));
	write.create_atom(name.parse::<RecordId>().assert_ok(), properties).assert_ok();
}

/// Compares planner class counts and durable Count against the runtime root.
fn assert_base_lifecycle_counts(store: &Datastore, nodes: u64, edges: u64) {
	use crate::storage::PhysicalIndexEntries;
	let mut read = store.begin();
	let snapshot = read.query_snapshot().assert_ok();
	let demand = QueryStatisticsDemand {
		base_classes: true,
		..QueryStatisticsDemand::default()
	};
	let root = store.inner.current_root();
	let item = root.schema.class("item").assert_some();
	let link = root.schema.class("link").assert_some();
	let statistics = read.query_statistics(&snapshot, &demand).assert_ok();
	assert_eq!(statistics.base().classes.get(&item.id().assert_some()), Some(&nodes));
	assert_eq!(statistics.base().classes.get(&link.id().assert_some()), Some(&edges));
	let local = match &store.inner.backend {
		StoreBackend::Local(local) => local,
		StoreBackend::InMemory => unreachable!("local fixture has an in-memory backend"),
	};
	let physical = local.capture_query_snapshot(root.commit_ts).assert_ok().assert_some();
	let count = item.indexes().find(|index| index.kind() == ClassIndexKind::Count).assert_some();
	let decoded =
		local.physical_index(&physical, count.id().assert_some()).assert_ok().assert_some();
	let PhysicalIndexEntries::Count(actual) = &decoded.entries else {
		unreachable!("Count binding decoded another index kind");
	};
	assert_eq!(*actual, nodes);
	assert_base_raw_postings(store);
}

/// Reopen depends on Hot tombstones; reserved freelist companions stay byte
/// identical and are never staged by ordinary lifecycle publication.
fn base_freelist_images(local: &crate::storage::LocalStore, root: &Path) -> Vec<Vec<u8>> {
	let state = local.state.lock();
	let images = state
		.file_catalog
		.files
		.iter()
		.filter(|entry| entry.file_id.kind() == Some(crate::storage::types::DataFileKind::Freelist))
		.map(|entry| fs::read(entry.path.resolve(root)).assert_ok())
		.collect::<Vec<_>>();
	assert_eq!(images.len(), 2);
	images
}

/// Raw codecs must retain generations that physical query adapters normalize.
pub(super) fn assert_base_raw_postings(store: &Datastore) {
	use std::collections::{BTreeMap, BTreeSet};

	use crate::{
		datastore::{DataSpaceId, GlobalAtomId},
		storage::{
			atom_store_image::value_context,
			graph_index::{ScalarIndexMode, scan_primary_index, scan_scalar_index},
			property_page::encode_record_id,
			types::PageKey,
		},
	};
	let root = store.inner.current_root();
	let local = match &store.inner.backend {
		StoreBackend::Local(local) => local,
		StoreBackend::InMemory => unreachable!("local fixture has an in-memory backend"),
	};
	let state = local.state.lock();
	let primary = state._database_catalog.atom_store.primary;
	let entries = scan_primary_index::<GlobalAtomId>(
		|number, output| {
			local.read_update_page_into(
				&state,
				PageKey::new(DataSpaceId::Database, primary.file_id, number),
				output,
			)
		},
		primary.root,
	)
	.assert_ok();
	let mut context = value_context(&root.schema, root.dictionary.as_ref().clone()).assert_ok();
	let expected = root
		.atoms
		.atoms()
		.map(|atom| (encode_record_id(atom.record_id(), &mut context).assert_ok(), atom.id()))
		.collect::<BTreeSet<_>>();
	assert_eq!(entries.len(), expected.len());
	assert_eq!(
		entries.into_iter().map(|entry| (entry.key, entry.atom)).collect::<BTreeSet<_>>(),
		expected
	);
	for binding in
		root.indexes.base.values().filter(|binding| binding.kind != ClassIndexKind::Count)
	{
		let mode = match binding.kind {
			ClassIndexKind::Standard => ScalarIndexMode::Standard,
			ClassIndexKind::Unique => ScalarIndexMode::Unique,
			_ => unreachable!("fixture has a non-scalar Base index"),
		};
		let physical = state.bindings.physical_indexes[&binding.id];
		let entries = scan_scalar_index::<GlobalAtomId>(
			|number, output| {
				local.read_update_page_into(
					&state,
					PageKey::new(DataSpaceId::Database, physical.file_id, number),
					output,
				)
			},
			physical.root,
			mode,
		)
		.assert_ok();
		let mut actual = BTreeMap::new();
		for entry in entries {
			let count = entry.atoms.len();
			let atoms = entry.atoms.into_iter().collect::<BTreeSet<_>>();
			assert_eq!(atoms.len(), count, "a posting cannot contain duplicate identities");
			assert!(actual.insert(entry.key, atoms).is_none());
		}
		let mut expected = BTreeMap::<_, BTreeSet<_>>::new();
		for atom in root.atoms.atoms().filter(|atom| atom.class_id() == binding.class_id) {
			let missing = Value::None;
			let components = binding
				.fields
				.iter()
				.map(|field| {
					(atom.properties().get(field.key).unwrap_or(&missing), &field.value_type)
				})
				.collect::<Vec<_>>();
			let key =
				crate::values::storage::encode_index_key(&components, &mut context).assert_ok();
			expected.entry(key).or_default().insert(atom.id());
		}
		assert_eq!(actual, expected);
	}
}
