//! Production point commits must publish the changed page slice.

use super::*;
use crate::{datastore::IoMode, values::PropertyKeyId};

/// Scalar publication must replace only the final changed bindings, including
/// inherited Effective keys while retaining overridden and unrelated indexes.
#[test]
fn scalar_point_update_preserves_unaffected_files_and_snapshots() {
	scalar_point_update(IoMode::Sync, false);
}

/// Inline leaves and an external-posting fallback publish through one barrier.
#[test]
fn scalar_point_mixed_leaves_and_fallback_preserve_snapshots() {
	scalar_point_update(IoMode::Sync, true);
}

/// NativeAsync scalar publication must retain its actual mode through reopen.
#[cfg(feature = "native-async-io")]
#[test]
fn scalar_point_native_async_preserves_unaffected_files_and_snapshots() {
	scalar_point_update(IoMode::NativeAsync, false);
}

/// NativeAsync uses the same atomic mixed publication and recovery contract.
#[cfg(feature = "native-async-io")]
#[test]
fn scalar_point_native_async_mixed_leaves_and_fallback_preserve_snapshots() {
	scalar_point_update(IoMode::NativeAsync, true);
}

/// Runs the same physical identity, index and recovery oracle per backend.
fn scalar_point_update(io_mode: IoMode, mixed: bool) {
	use std::collections::{BTreeMap, BTreeSet};

	use crate::storage::types::DataFileKind;

	let directory = TestDirectory::new();
	let options = LocalOptions {
		io_mode,
		buffer_pool_pages: 256,
		..LocalOptions::default()
	};
	let store = Datastore::create_local(&directory.0, options).assert_ok();
	assert_eq!(store.local_storage_status().assert_some().options().io_mode, io_mode);
	let mut seed = store.begin();
	let inherited = seed.define_graph("inherited", GraphKind::Graph).assert_ok();
	let overridden = seed.define_graph("overridden", GraphKind::Graph).assert_ok();
	let (value_key, other_key) =
		seed_scalar_point_class(&mut seed, inherited.id(), overridden.id(), mixed);
	seed.commit().assert_ok();
	store.checkpoint(CheckpointMode::Restart).assert_ok();
	drop(store);
	let store = Datastore::open_local(&directory.0, options).assert_ok();
	assert_eq!(store.local_storage_status().assert_some().options().io_mode, io_mode);
	store.inner.maintenance.shutdown();
	let local = match &store.inner.backend {
		StoreBackend::Local(local) => Some(local),
		StoreBackend::InMemory => None,
	}
	.assert_some();
	let old = local
		.capture_query_snapshot(store.inner.current_root().commit_ts)
		.assert_ok()
		.assert_some();
	let before_files = local.state.lock().file_catalog.files.clone();
	let indexes = local
		.state
		.lock()
		._database_catalog
		.classes
		.iter()
		.flat_map(|class| &class.indexes)
		.map(|index| (index.name.as_str().to_owned(), index.id))
		.collect::<BTreeMap<_, _>>();
	let before_bindings = local.state.lock().bindings.physical_indexes.clone();
	let mut changed = ["base_value", "base_unique", "effective_value"]
		.map(|name| indexes[name])
		.into_iter()
		.collect::<BTreeSet<_>>();
	let replaced = if mixed {
		changed.insert(indexes["base_other"]);
		BTreeSet::from([before_bindings[&indexes["base_other"]].file_id])
	} else {
		BTreeSet::new()
	};
	let before_owners = local.state.lock().physical_indexes.clone();
	let warm_old = local.physical_index(&old, indexes["base_value"]).assert_ok().assert_some();
	let old_paths = before_files
		.iter()
		.filter(|entry| replaced.contains(&entry.file_id))
		.map(|entry| entry.path.resolve(&directory.0))
		.collect::<Vec<_>>();
	assert_eq!(old_paths.len(), usize::from(mixed));
	let boundary = local.wal.snapshot().visible_lsn();
	let record = "item:n16".parse::<RecordId>().assert_ok();
	let mut write = store.begin();
	write.set_base_property(&record, value_key, Value::from(100)).assert_ok();
	if mixed {
		write.set_base_property(&record, other_key, Value::from(5000)).assert_ok();
	}
	write.commit().assert_ok();
	{
		let state = local.state.lock();
		for entry in before_files.iter().filter(|entry| !replaced.contains(&entry.file_id)) {
			assert!(
				state.file_catalog.files.contains(entry),
				"unaffected physical identity changed: {:?}",
				entry.file_id
			);
		}
		for (id, binding) in &before_bindings {
			if !replaced.contains(&binding.file_id) {
				assert_eq!(state.bindings.physical_indexes[id], *binding);
			} else {
				assert_ne!(state.bindings.physical_indexes[id], *binding);
			}
			assert_eq!(
				Arc::ptr_eq(&before_owners[id], &state.physical_indexes[id]),
				!changed.contains(id),
				"changed leaves need a fresh decoded owner; unrelated indexes reuse theirs"
			);
			let old_file =
				before_files.iter().find(|entry| entry.file_id == binding.file_id).assert_some();
			let next_file = state
				.file_catalog
				.files
				.iter()
				.find(|entry| entry.file_id == state.bindings.physical_indexes[id].file_id)
				.assert_some();
			assert_eq!(
				Path::new(next_file.path.as_str()).file_name(),
				Path::new(old_file.path.as_str()).file_name(),
				"selected images must preserve final-scope canonical ordinals"
			);
		}
		assert_eq!(state.file_catalog.files.len(), before_files.len());
		assert!(
			state
				.file_catalog
				.files
				.iter()
				.filter(|entry| !before_files.contains(entry))
				.all(|entry| entry.file_id.kind() == Some(DataFileKind::Index))
		);
	}
	let frames = local
		.wal
		.recovery_frames()
		.into_iter()
		.filter(|frame| frame.frame_lsn() > boundary)
		.filter(|frame| frame.key().file_id().kind() == Some(DataFileKind::Index))
		.collect::<Vec<_>>();
	let leaf_files = changed
		.iter()
		.map(|id| before_bindings[id].file_id)
		.filter(|file| !replaced.contains(file))
		.collect::<BTreeSet<_>>();
	assert_eq!(frames.len(), leaf_files.len(), "one root leaf per stable binding is sufficient");
	assert!(frames.iter().all(|frame| leaf_files.contains(&frame.key().file_id())));
	let current = local
		.capture_query_snapshot(store.inner.current_root().commit_ts)
		.assert_ok()
		.assert_some();
	store.checkpoint(CheckpointMode::Passive).assert_ok();
	assert!(old_paths.iter().all(|path| path.is_file()));
	// One old cache is warm; the other old leaves first decode after
	// checkpoint.
	assert!(Arc::ptr_eq(
		&warm_old,
		&local.physical_index(&old, indexes["base_value"]).assert_ok().assert_some()
	));
	for id in indexes.values() {
		let old_read = local.physical_index(&old, *id).assert_ok().assert_some();
		let current_read = local.physical_index(&current, *id).assert_ok().assert_some();
		assert_scalar_index_transition(&old_read, &current_read, changed.contains(id));
	}
	drop(old);
	drop(warm_old);
	drop(before_owners);
	super::super::sweep_retired_generations(&mut local.state.lock());
	assert!(old_paths.iter().all(|path| !path.exists()));
	drop(current);
	drop(store);
	let reopened = Datastore::open_local(&directory.0, options).assert_ok();
	assert_eq!(reopened.local_storage_status().assert_some().options().io_mode, io_mode);
	let mut read = reopened.begin();
	let snapshot = read.query_snapshot().assert_ok();
	for name in ["base_value", "base_unique", "effective_value"] {
		let graph = (name == "effective_value").then_some(&inherited);
		let mut cursor = read
			.query_index_probe_cursor(&snapshot, "item", graph, indexes[name], &[Value::from(100)])
			.assert_ok();
		assert_eq!(cursor.next_row().assert_ok().assert_some().record_id, record);
		assert!(cursor.next_row().assert_ok().is_none());
	}
}

/// Compares decoded values independently of the physical identity oracle.
fn assert_scalar_index_transition(
	old: &crate::storage::PhysicalIndexRead,
	current: &crate::storage::PhysicalIndexRead,
	changed: bool,
) {
	use crate::storage::PhysicalIndexEntries;
	match (&old.entries, &current.entries) {
		(PhysicalIndexEntries::Scalar(old), PhysicalIndexEntries::Scalar(next)) => {
			assert_eq!(next != old, changed);
			assert_eq!(old.iter().map(|group| group.atoms.len()).sum::<usize>(), 32);
			assert_eq!(next.iter().map(|group| group.atoms.len()).sum::<usize>(), 32);
		}
		(PhysicalIndexEntries::Count(old), PhysicalIndexEntries::Count(next)) => {
			assert_eq!((*old, *next), (32, 32));
		}
		_ => unreachable!("scalar publication changed index kind"),
	}
}

/// Seeds inherited keys and a fixed overlay for the same publication oracle.
fn seed_scalar_point_class(
	seed: &mut crate::datastore::Transaction,
	inherited: crate::graph::GraphId,
	overridden: crate::graph::GraphId,
	mixed: bool,
) -> (PropertyKeyId, PropertyKeyId) {
	let mut class = ClassSchema::new("item", AtomKind::Node, [
		ClassField::new("value", Type::Int),
		ClassField::new("other", Type::Int),
	])
	.assert_ok();
	for (name, kind, scope, field) in [
		("base_value", ClassIndexKind::Standard, IndexScope::Base, Some("value")),
		("base_unique", ClassIndexKind::Unique, IndexScope::Base, Some("value")),
		("base_other", ClassIndexKind::Standard, IndexScope::Base, Some("other")),
		("base_count", ClassIndexKind::Count, IndexScope::Base, None),
		(
			"effective_value",
			ClassIndexKind::Unique,
			IndexScope::Effective {
				graph: inherited,
			},
			Some("value"),
		),
		(
			"fixed_value",
			ClassIndexKind::Standard,
			IndexScope::Effective {
				graph: overridden,
			},
			Some("value"),
		),
	] {
		class = class.with_index(ClassIndex::new(name, kind, scope, field).assert_ok()).assert_ok();
	}
	seed.define_class(class).assert_ok();
	let value_key = seed.intern_property("value").assert_ok();
	let other_key = seed.intern_property("other").assert_ok();
	for index in 0..32 {
		let record = format!("item:n{index}").parse::<RecordId>().assert_ok();
		let mut properties = PropertyMap::new();
		properties.insert(value_key, Value::from(index));
		properties.insert(
			other_key,
			Value::from(if mixed {
				1000
			} else {
				1000 + index
			}),
		);
		seed.create_atom(record.clone(), properties).assert_ok();
		seed.attach_node(inherited, &record, None, None).assert_ok();
		seed.attach_node(overridden, &record, None, None).assert_ok();
		seed.set_overlay(overridden, &record, value_key, Value::from(999)).assert_ok();
	}
	(value_key, other_key)
}

/// Recovery removes an individual orphan scalar file even while its UUID
/// directory remains reachable through unchanged AtomStore files.
#[test]
fn scalar_point_reopen_retires_file_in_retained_directory() {
	let (directory, store, key, record) = single_atom_fixture();
	let mut schema = store.begin();
	schema.define_field("item", ClassField::new("value", Type::String)).assert_ok();
	schema
		.define_index(
			"item",
			ClassIndex::new("by_value", ClassIndexKind::Standard, IndexScope::Base, ["value"])
				.assert_ok(),
		)
		.assert_ok();
	add_posting_neighbours(&mut schema, key, 8);
	schema.commit().assert_ok();
	store.checkpoint(CheckpointMode::Restart).assert_ok();
	drop(store);
	let options = LocalOptions::default();
	let store = Datastore::open_local(&directory.0, options).assert_ok();
	store.inner.maintenance.shutdown();
	let old_path = physical_index_base_path(&store, &directory.0, "by_value");
	let pin = match &store.inner.backend {
		StoreBackend::Local(local) => {
			local.capture_query_snapshot(store.inner.current_root().commit_ts).assert_ok()
		}
		StoreBackend::InMemory => None,
	}
	.assert_some();
	let mut write = store.begin();
	write.set_base_property(&record, key, Value::from("changed")).assert_ok();
	write.commit().assert_ok();
	let current_path = physical_index_base_path(&store, &directory.0, "by_value");
	assert_ne!(old_path, current_path);
	assert!(old_path.is_file());
	drop(store);
	drop(pin);
	assert!(old_path.is_file());
	let reopened = Datastore::open_local(&directory.0, options).assert_ok();
	assert!(!old_path.exists());
	assert!(current_path.is_file());
	assert_eq!(
		reopened.begin().atom(&record).assert_ok().assert_some().properties().get(key),
		Some(&Value::from("changed"))
	);
	drop(reopened);
	assert!(Datastore::open_local(&directory.0, options).is_ok());
}

/// Reopens a small source so every test starts with deferred auxiliary maps.
fn single_atom_fixture() -> (TestDirectory, Datastore, PropertyKeyId, RecordId) {
	let directory = TestDirectory::new();
	let options = LocalOptions {
		buffer_pool_pages: 256,
		..LocalOptions::default()
	};
	let store = Datastore::create_local(&directory.0, options).assert_ok();
	let mut seed = store.begin();
	seed.define_class(ClassSchema::new("item", AtomKind::Node, []).assert_ok()).assert_ok();
	let key = seed.intern_property("value").assert_ok();
	let record = "item:a".parse::<RecordId>().assert_ok();
	let mut properties = PropertyMap::new();
	properties.insert(key, Value::from("before"));
	seed.create_atom(record.clone(), properties).assert_ok();
	seed.commit().assert_ok();
	store.checkpoint(CheckpointMode::Restart).assert_ok();
	drop(store);
	let store = Datastore::open_local(&directory.0, options).assert_ok();
	assert!(store.inner.current_root().statistics.auxiliary_deferred());
	(directory, store, key, record)
}

/// A transaction prepared before checkpoint uses the latest durable authority;
/// a later failed checkpoint must not leak its ahead subordinate tuple.
#[test]
fn point_update_uses_database_checkpoint_authority_after_failure() {
	let (directory, store, key, record) = single_atom_fixture();
	let local = match &store.inner.backend {
		StoreBackend::Local(local) => Some(local),
		StoreBackend::InMemory => None,
	}
	.assert_some();
	let mut first = store.begin();
	first.set_base_property(&record, key, Value::from("first")).assert_ok();
	first.commit().assert_ok();
	let mut pending = store.begin();
	pending.set_base_property(&record, key, Value::from("second")).assert_ok();
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
	next.set_base_property(&record, key, Value::from("third")).assert_ok();
	next.commit().assert_ok();
	let header = local.state.lock().atom_superblock;
	assert_eq!(header.checkpoint_generation, authority.generation);
	assert_eq!(header.backfilled_lsn, authority.checkpoint_lsn);
	assert_eq!(header.checkpoint_record_lsn, authority.checkpoint_record_lsn);
	drop(store);
	let reopened = Datastore::open_local(&directory.0, LocalOptions::default()).assert_ok();
	assert_eq!(
		reopened.begin().atom(&record).assert_ok().assert_some().properties().get(key),
		Some(&Value::from("third"))
	);
}

/// Unsafe pending deltas hydrate before ReadCommitted replay against a lazy
/// point-commit root; a later new-key setter keeps exact counters intact.
#[test]
fn point_update_read_committed_replay_preserves_exact_counters() {
	use crate::datastore::{IsolationLevel, TransactionAccess, TransactionOptions};
	let (_directory, store, key, record) = single_atom_fixture();
	let mut pending = store.begin_with(TransactionOptions::new(
		IsolationLevel::ReadCommitted,
		TransactionAccess::ReadWrite,
	));
	let added = "item:added".parse::<RecordId>().assert_ok();
	pending.create_atom(added.clone(), PropertyMap::new()).assert_ok();
	let mut other = store.begin();
	other.set_base_property(&record, key, Value::from("concurrent")).assert_ok();
	other.commit().assert_ok();
	assert!(store.inner.current_root().statistics.auxiliary_deferred());
	assert!(pending.atom(&record).assert_ok().is_some());
	let extra = pending.intern_property("extra").assert_ok();
	pending.set_base_property(&added, extra, Value::from(1)).assert_ok();
	pending.commit().assert_ok();
	let mut root = store.inner.current_root().as_ref().clone();
	store.inner.hydrate_statistics(&mut root).assert_ok();
	assert_eq!(root.statistics.base().classes.values().copied().sum::<u64>(), 2);
	assert_eq!(
		root.atom_by_record(&record).assert_some().properties().get(key),
		Some(&Value::from("concurrent"))
	);
	assert_eq!(
		root.atom_by_record(&added).assert_some().properties().get(extra),
		Some(&Value::from(1))
	);
}

/// Passive checkpoint materializes the pinned version while the latest WAL
/// keeps a later append ceiling reachable for current readers and recovery.
#[test]
fn point_update_passive_checkpoint_materializes_ticket_superblock() {
	use crate::storage::{
		atom_store_superblock::AtomStoreSuperblock,
		direct::DirectFile,
		page::{AlignedPage, PageNo},
	};
	let (directory, store, key, record) = single_atom_fixture();
	let local = match &store.inner.backend {
		StoreBackend::Local(local) => Some(local),
		StoreBackend::InMemory => None,
	}
	.assert_some();
	let mut first = store.begin();
	first.set_base_property(&record, key, Value::from("first")).assert_ok();
	first.commit().assert_ok();
	let pin = local
		.capture_query_snapshot(store.inner.current_root().commit_ts)
		.assert_ok()
		.assert_some();
	let first_ceiling = local.state.lock().atom_superblock.properties_pages;
	let mut second = store.begin();
	second.set_base_property(&record, key, Value::from("second")).assert_ok();
	second.commit().assert_ok();
	let second_ceiling = local.state.lock().atom_superblock.properties_pages;
	assert!(second_ceiling > first_ceiling);
	store.checkpoint(CheckpointMode::Passive).assert_ok();
	let path = {
		let state = local.state.lock();
		state
			.registry
			.resolve_file(crate::datastore::DataSpaceId::Database, state.bindings.atoms.superblock)
			.assert_ok()
			.path()
			.to_path_buf()
	};
	let mut page = AlignedPage::zeroed();
	DirectFile::open_read_only(&path).assert_ok().read_page(PageNo::new(0), &mut page).assert_ok();
	let home = AtomStoreSuperblock::decode(&page).assert_ok();
	assert_eq!(home.properties_pages, first_ceiling);
	assert!(home.page_lsn.get() > 0);
	assert_eq!(local.state.lock().atom_superblock.properties_pages, second_ceiling);
	drop(pin);
	drop(store);
	let reopened = Datastore::open_local(&directory.0, LocalOptions::default()).assert_ok();
	assert_eq!(
		reopened.begin().atom(&record).assert_ok().assert_some().properties().get(key),
		Some(&Value::from("second"))
	);
	reopened.checkpoint(CheckpointMode::Restart).assert_ok();
}

/// A full-generation fallback cannot let checkpoint prune the earlier sparse
/// version before its pinned retired files have received their WAL pages.
#[test]
fn point_update_retired_snapshot_survives_fallback_checkpoint() {
	let (_directory, store, key, record) = single_atom_fixture();
	let mut setup = store.begin();
	let graph = setup.define_graph("items", GraphKind::Graph).assert_ok();
	let owner = setup.attach_node(graph.id(), &record, None, None).assert_ok();
	setup.commit().assert_ok();
	store.checkpoint(CheckpointMode::Restart).assert_ok();
	let mut first = store.begin();
	first.set_base_property(&record, key, Value::from("first")).assert_ok();
	first.commit().assert_ok();
	let mut old = store.begin();
	let snapshot = old.query_snapshot().assert_ok();
	let mut fallback = store.begin();
	fallback
		.create_atom("item:new".parse::<RecordId>().assert_ok(), PropertyMap::new())
		.assert_ok();
	fallback.commit().assert_ok();
	store.checkpoint(CheckpointMode::Passive).assert_ok();
	assert_eq!(
		old.query_physical_fields(&snapshot, &graph, owner)
			.assert_ok()
			.assert_some()
			.properties
			.get(key),
		Some(&Value::from("first"))
	);
}

/// Catalog continuation growth must update the same cached length used by
/// checkpoint, including the reserved gap before FileCatalog continuations.
#[test]
fn point_update_catalog_growth_preserves_shared_file_bounds() {
	use crate::{
		datastore::DataSpaceId,
		storage::types::{DataFileId, DataFileKind},
	};
	let (_directory, store, _key, _record) = single_atom_fixture();
	let local = match &store.inner.backend {
		StoreBackend::Local(local) => Some(local),
		StoreBackend::InMemory => None,
	}
	.assert_some();
	let file = local
		.state
		.lock()
		.registry
		.resolve_file(DataSpaceId::Database, DataFileId::for_kind(DataFileKind::SystemCatalog))
		.assert_ok();
	let handle = file.file().assert_ok();
	let mut write = store.begin();
	for ordinal in 0..12 {
		write.define_graph(format!("graph_{ordinal}"), GraphKind::Graph).assert_ok();
	}
	write.commit().assert_ok();
	let physical_pages =
		fs::metadata(file.path()).assert_ok().len() / crate::storage::page::PAGE_SIZE as u64;
	assert!(physical_pages > 1030, "fixture must cross the catalog continuation gap");
	store.checkpoint(CheckpointMode::Restart).assert_ok();
	assert_eq!(handle.page_count(), physical_pages);
}

/// A full fallback must hydrate the root that actually becomes current, not
/// just the temporary image consumed by its physical encoder.
#[test]
fn point_update_indexed_fallback_preserves_following_mutations() {
	let (directory, store, key, record) = single_atom_fixture();
	let mut schema = store.begin();
	schema.define_field("item", ClassField::new("value", Type::String)).assert_ok();
	schema
		.define_index(
			"item",
			ClassIndex::new("by_value", ClassIndexKind::Standard, IndexScope::Base, ["value"])
				.assert_ok(),
		)
		.assert_ok();
	schema.commit().assert_ok();
	drop(store);
	let store = Datastore::open_local(&directory.0, LocalOptions::default()).assert_ok();
	for value in ["first", "second"] {
		let mut write = store.begin();
		let extra = write.intern_property(&format!("extra_{value}")).assert_ok();
		write.set_base_property(&record, extra, Value::from(1)).assert_ok();
		write.set_base_property(&record, key, Value::from(value)).assert_ok();
		write.commit().assert_ok();
		assert!(!store.inner.current_root().statistics.auxiliary_deferred());
	}
	let mut next = store.begin();
	next.create_atom("item:new".parse::<RecordId>().assert_ok(), PropertyMap::new()).assert_ok();
	next.commit().assert_ok();
}

/// Sparse preparation failures publish neither root nor frames, and leave the
/// same physical source available for a later successful transaction.
#[test]
fn point_update_prepublication_failures_preserve_source() {
	prepublication_failures(TestPointIndexes::Absent);
}

/// A rejected leaf publication preserves the prior decoded owner map.
#[test]
fn scalar_leaf_prepublication_failures_preserve_source() {
	prepublication_failures(TestPointIndexes::Leaves);
}

/// Selected scalar images remain unreachable on every known failure before
/// the shared WAL barrier, including after their home files are synced.
#[test]
fn scalar_point_prepublication_failures_preserve_source() {
	prepublication_failures(TestPointIndexes::Streamed);
}

/// Physical paths exercised by the shared prepublication fault oracle.
#[derive(Clone, Copy, PartialEq, Eq)]
enum TestPointIndexes {
	/// Sparse AtomStore pages without an explicit scalar index.
	Absent,
	/// A stable scalar leaf retaining its physical binding.
	Leaves,
	/// An external posting forces the selected streaming fallback.
	Streamed,
}

/// Uses the same source/root/frame oracle for sparse pages and scalar images.
fn prepublication_failures(indexes: TestPointIndexes) {
	let (directory, store, key, record) = single_atom_fixture();
	if indexes != TestPointIndexes::Absent {
		let mut schema = store.begin();
		schema.define_field("item", ClassField::new("value", Type::String)).assert_ok();
		schema
			.define_index(
				"item",
				ClassIndex::new("by_value", ClassIndexKind::Standard, IndexScope::Base, ["value"])
					.assert_ok(),
			)
			.assert_ok();
		if indexes == TestPointIndexes::Streamed {
			add_posting_neighbours(&mut schema, key, 8);
		}
		schema.commit().assert_ok();
	}
	let local = match &store.inner.backend {
		StoreBackend::Local(local) => Some(local),
		StoreBackend::InMemory => None,
	}
	.assert_some();
	let stamp = store.inner.current_root().commit_ts;
	let boundary = local.wal.snapshot().visible_lsn();
	let files = local.state.lock().file_catalog.files.clone();
	let owners = local.state.lock().physical_indexes.clone();
	let directories = generation_directories(&directory.0);
	let points = if indexes == TestPointIndexes::Streamed {
		&[
			GenerationFailurePoint::AfterBuild,
			GenerationFailurePoint::AfterFileSync,
			GenerationFailurePoint::AfterCatalogStage,
			GenerationFailurePoint::BeforeWalCommit,
		][..]
	} else {
		&[
			GenerationFailurePoint::AfterBuild,
			GenerationFailurePoint::AfterCatalogStage,
			GenerationFailurePoint::BeforeWalCommit,
		][..]
	};
	for &point in points {
		let mut write = store.begin();
		write.set_base_property(&record, key, Value::from("rejected")).assert_ok();
		let guard = GenerationFailureGuard::set(point);
		write.commit().assert_err();
		drop(guard);
		assert_eq!(store.inner.current_root().commit_ts, stamp);
		assert_eq!(local.wal.snapshot().visible_lsn(), boundary);
		assert!(local.state.lock().file_catalog.files == files);
		{
			let state = local.state.lock();
			assert_eq!(state.physical_indexes.len(), owners.len());
			assert!(
				owners.iter().all(|(id, owner)| Arc::ptr_eq(owner, &state.physical_indexes[id]))
			);
		}
		assert_eq!(generation_directories(&directory.0), directories);
		assert_eq!(
			store.begin().atom(&record).assert_ok().assert_some().properties().get(key),
			Some(&Value::from("before"))
		);
	}
	let mut accepted = store.begin();
	accepted.set_base_property(&record, key, Value::from("accepted")).assert_ok();
	assert_eq!(accepted.commit().assert_ok().commit_ts(), stamp + 1);
}

/// Adds equal keys for inline-boundary and external-posting fixtures.
fn add_posting_neighbours(
	tx: &mut crate::datastore::Transaction,
	key: PropertyKeyId,
	count: usize,
) {
	for ordinal in 0..count {
		let mut properties = PropertyMap::new();
		properties.insert(key, Value::from("before"));
		tx.create_atom(
			format!("item:posting{ordinal}").parse::<RecordId>().assert_ok(),
			properties,
		)
		.assert_ok();
	}
}

/// Checkpoint backfills leaf versions even after their file becomes retired.
#[test]
fn scalar_leaf_versions_survive_streamed_fallback_and_checkpoint() {
	use crate::storage::PhysicalIndexEntries;
	let (directory, store, key, record) = single_atom_fixture();
	let mut setup = store.begin();
	setup.define_field("item", ClassField::new("value", Type::String)).assert_ok();
	setup
		.define_index(
			"item",
			ClassIndex::new("by_value", ClassIndexKind::Standard, IndexScope::Base, ["value"])
				.assert_ok(),
		)
		.assert_ok();
	add_posting_neighbours(&mut setup, key, 7);
	let spares = ["item:spare1", "item:spare2"].map(|text| text.parse::<RecordId>().assert_ok());
	for spare in &spares {
		let mut properties = PropertyMap::new();
		properties.insert(key, Value::from("spare"));
		setup.create_atom(spare.clone(), properties).assert_ok();
	}
	setup.commit().assert_ok();
	store.checkpoint(CheckpointMode::Restart).assert_ok();
	store.inner.maintenance.shutdown();
	let local = match &store.inner.backend {
		StoreBackend::Local(local) => Some(local),
		StoreBackend::InMemory => None,
	}
	.assert_some();
	let index = local
		.state
		.lock()
		._database_catalog
		.classes
		.iter()
		.flat_map(|class| &class.indexes)
		.find(|index| index.name == "by_value")
		.assert_some()
		.id;
	let path = physical_index_base_path(&store, &directory.0, "by_value");
	let old = local
		.capture_query_snapshot(store.inner.current_root().commit_ts)
		.assert_ok()
		.assert_some();
	for value in ["first", "second"] {
		let mut write = store.begin();
		write.set_base_property(&record, key, Value::from(value)).assert_ok();
		write.commit().assert_ok();
		assert_eq!(physical_index_base_path(&store, &directory.0, "by_value"), path);
	}
	let second = local
		.capture_query_snapshot(store.inner.current_root().commit_ts)
		.assert_ok()
		.assert_some();
	let mut fallback = store.begin();
	for spare in &spares {
		fallback.set_base_property(spare, key, Value::from("before")).assert_ok();
	}
	fallback.commit().assert_ok();
	assert_ne!(physical_index_base_path(&store, &directory.0, "by_value"), path);
	store.checkpoint(CheckpointMode::Passive).assert_ok();
	let check = |read: &crate::storage::PhysicalIndexRead, expected: &[usize]| {
		let PhysicalIndexEntries::Scalar(groups) = &read.entries else {
			unreachable!("fixture has one Standard index");
		};
		let mut sizes = groups.iter().map(|group| group.atoms.len()).collect::<Vec<_>>();
		sizes.sort_unstable();
		assert_eq!(sizes, expected);
	};
	check(&local.physical_index(&old, index).assert_ok().assert_some(), &[2, 8]);
	drop(old);
	// This second snapshot first decodes after checkpoint advances the retired
	// home file.
	store.checkpoint(CheckpointMode::Passive).assert_ok();
	assert!(path.is_file());
	check(&local.physical_index(&second, index).assert_ok().assert_some(), &[1, 2, 7]);
	drop(second);
	super::super::sweep_retired_generations(&mut local.state.lock());
	assert!(!path.exists());
	drop(store);
	let reopened = Datastore::open_local(&directory.0, LocalOptions::default()).assert_ok();
	assert_eq!(
		reopened.begin().atom(&record).assert_ok().assert_some().properties().get(key),
		Some(&Value::from("second"))
	);
	let local = match &reopened.inner.backend {
		StoreBackend::Local(local) => Some(local),
		StoreBackend::InMemory => None,
	}
	.assert_some();
	let snapshot = local
		.capture_query_snapshot(reopened.inner.current_root().commit_ts)
		.assert_ok()
		.assert_some();
	check(&local.physical_index(&snapshot, index).assert_ok().assert_some(), &[1, 9]);
}

/// Exercises repeated WAL-only appends, a pinned snapshot and cold recovery.
fn existing_property_update(io_mode: IoMode) {
	let directory = TestDirectory::new();
	let options = LocalOptions {
		io_mode,
		buffer_pool_pages: 256,
		..LocalOptions::default()
	};
	let store = Datastore::create_local(&directory.0, options).assert_ok();
	assert_eq!(store.local_storage_status().assert_some().options().io_mode, io_mode);
	let mut seed = store.begin();
	seed.define_class(ClassSchema::new("item", AtomKind::Node, []).assert_ok()).assert_ok();
	let graph = seed.define_graph("items", GraphKind::Graph).assert_ok();
	let key = seed.intern_property("value").assert_ok();
	let mut owner = None;
	for index in 0..512 {
		let record = format!("item:n{index}").parse::<RecordId>().assert_ok();
		let mut properties = PropertyMap::new();
		properties.insert(key, Value::from("before"));
		seed.create_atom(record.clone(), properties).assert_ok();
		let local = seed.attach_node(graph.id(), &record, None, None).assert_ok();
		if index == 0 {
			owner = Some(local);
		}
	}
	seed.commit().assert_ok();
	store.checkpoint(CheckpointMode::Restart).assert_ok();
	drop(store);
	let store = Datastore::open_local(&directory.0, options).assert_ok();
	let mut old = store.begin();
	let snapshot = old.query_snapshot().assert_ok();
	let owner = owner.assert_some();
	let record = "item:n0".parse::<RecordId>().assert_ok();
	let local = match &store.inner.backend {
		StoreBackend::Local(local) => Some(local),
		StoreBackend::InMemory => None,
	}
	.assert_some();
	let files = local.state.lock().file_catalog.files.clone();
	let authority = local.state.lock().database_superblock.generation;
	assert!(store.inner.current_root().statistics.auxiliary_deferred());
	let values = [Value::from("first"), Value::from("second"), Value::from("x".repeat(64 * 1024))];
	for (value, maximum_frames) in values.iter().zip([8, 8, 12]) {
		let boundary = local.wal.snapshot().visible_lsn();
		let mut write = store.begin();
		write.set_base_property(&record, key, value.clone()).assert_ok();
		write.commit().assert_ok();
		assert!(store.inner.current_root().statistics.auxiliary_deferred());
		assert!(
			local.state.lock().file_catalog.files == files,
			"point update replaced existing files"
		);
		assert_eq!(local.state.lock().database_superblock.generation, authority);
		let frames = local
			.wal
			.recovery_frames()
			.into_iter()
			.filter(|frame| frame.frame_lsn() > boundary)
			.collect::<Vec<_>>();
		assert!(frames.len() <= maximum_frames, "one point update staged {} frames", frames.len());
		for frame in &frames {
			use crate::storage::types::DataFileKind;
			assert!(matches!(
				frame.key().file_id().kind(),
				Some(
					DataFileKind::AtomsHot
						| DataFileKind::Properties
						| DataFileKind::AtomStoreSuperblock
						| DataFileKind::ClassStatistics
						| DataFileKind::Statistics
						| DataFileKind::SystemCatalog
				)
			));
		}
		assert_eq!(
			old.query_physical_fields(&snapshot, &graph, owner)
				.assert_ok()
				.assert_some()
				.properties
				.get(key),
			Some(&Value::from("before"))
		);
		let mut read = store.begin();
		let current = read.query_snapshot().assert_ok();
		assert_eq!(
			read.query_physical_fields(&current, &graph, owner)
				.assert_ok()
				.assert_some()
				.properties
				.get(key),
			Some(value)
		);
	}
	assert_eq!(
		old.atom(&record).assert_ok().assert_some().properties().get(key),
		Some(&Value::from("before"))
	);
	drop(old);
	drop(snapshot);
	drop(store);
	let store = Datastore::open_local(&directory.0, options).assert_ok();
	assert_eq!(store.local_storage_status().assert_some().options().io_mode, io_mode);
	assert_eq!(
		store.begin().atom(&record).assert_ok().assert_some().properties().get(key),
		Some(&values[2])
	);
	store.checkpoint(CheckpointMode::Restart).assert_ok();
}

/// Sync point updates preserve files and stay bounded across reopen.
#[test]
fn point_update_sync_reuses_files_and_bounds_wal() {
	existing_property_update(IoMode::Sync);
}

/// NativeAsync must actually use its requested mode after create and reopen.
#[cfg(feature = "native-async-io")]
#[test]
fn point_update_native_async_reuses_files_and_bounds_wal() {
	existing_property_update(IoMode::NativeAsync);
}
