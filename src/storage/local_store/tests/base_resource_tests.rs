//! Base property preparation shares the admitted publication envelope.

use super::*;
use crate::{
	datastore::IoMode,
	gql_status::Status,
	values::{Array, Set},
};

/// The final header must recheck source buffers that remain live until boxing.
#[test]
fn base_property_backing_survives_late_frame_reservations() {
	use std::collections::BTreeSet;

	use crate::{
		datastore::{DataSpaceId, PublicationDelta},
		memory::tree_retained_bytes,
		storage::{
			atom_page::AtomHotPage,
			atom_store_image::value_context,
			graph_index::with_external_sort_options,
			local_store::{point_base::BaseUpdatePlan, point_namespace::LabelPlan},
			page::{AlignedPage, PAGE_SIZE, PageNo},
			property_page::MAX_INLINE_BYTES,
			types::PageKey,
			wal::TxHandle,
		},
	};

	let (directory, options, first, key) = seed(IoMode::Sync);
	let store = Datastore::open_local(&directory.0, options).assert_ok();
	store.inner.maintenance.shutdown();
	let second = "item:b".parse::<RecordId>().assert_ok();
	let mut properties = PropertyMap::new();
	properties.insert(key, Value::from("seed"));
	let mut write = store.begin();
	write.create_atom(second.clone(), properties).assert_ok();
	write.commit().assert_ok();
	store.checkpoint(CheckpointMode::Restart).assert_ok();
	let local = local(&store);
	let published = store.inner.current_root();
	let boundary = local.wal.snapshot().visible_lsn();
	let mut before = published.as_ref().clone();
	local.hydrate_statistics(&mut before).assert_ok();
	let mut after = before.clone();
	let mut slots = BTreeSet::new();
	for record in [&first, &second] {
		let global = before.resolve_atom(record).assert_some();
		slots.insert(global.slot());
		after
			.set_base_property(global, key, Value::from("x".repeat(MAX_INLINE_BYTES - 9)))
			.assert_ok();
	}
	let delta = PublicationDelta {
		records: BTreeSet::from([first, second]),
		global_slots: slots,
		overlays: BTreeSet::new(),
		incidence: BTreeSet::new(),
		memberships: BTreeSet::new(),
		point_graph_only: true,
	};
	let final_peak = 4096
		+ tree_retained_bytes::<(PageNo, AtomHotPage)>(1).assert_ok()
		+ 4 * (PAGE_SIZE + 3 * size_of::<(PageKey, Box<AlignedPage>)>())
		+ TxHandle::staging_bytes(4).assert_ok()
		+ 2 * PAGE_SIZE;
	let state = local.state.lock();
	let files = state.file_catalog.files.clone();
	let start = state.atom_superblock.properties_pages;
	for gap in [0, 1] {
		with_external_sort_options(
			NonZeroUsize::new(final_peak - gap).assert_some(),
			NonZeroUsize::new(1).assert_some(),
			|| {
				let mut labels = LabelPlan::build(local, &state, &before, &after, &delta)?;
				let mut context = value_context(&after.schema, after.dictionary.as_ref().clone())?;
				let candidate = BaseUpdatePlan::build(
					local,
					&state,
					&before,
					&after,
					&delta,
					&mut context,
					&mut labels.budget,
				);
				if gap == 0 {
					let plan = candidate.assert_ok();
					let atoms = state.bindings.atoms;
					assert_eq!(plan.pages.len(), 4);
					assert_eq!(
						plan.pages.iter().map(|(key, _)| *key).collect::<BTreeSet<_>>(),
						BTreeSet::from([
							PageKey::new(DataSpaceId::Database, atoms.atoms_hot, PageNo::new(0)),
							PageKey::new(
								DataSpaceId::Database,
								atoms.properties,
								PageNo::new(start)
							),
							PageKey::new(
								DataSpaceId::Database,
								atoms.properties,
								PageNo::new(start + 1)
							),
							PageKey::new(DataSpaceId::Database, atoms.superblock, PageNo::new(0)),
						])
					);
					assert_eq!(plan.superblock.properties_pages, start + 2);
					assert!(!plan.replaces_statistics);
				} else {
					let error = candidate.err().assert_some();
					assert!(matches!(error.status().status(),
						Status::StorageDecodeLimitExceeded { actual, maximum, .. }
						if *actual == final_peak as u64 && *maximum == (final_peak - 1) as u64
					));
				}
				Ok(())
			},
		)
		.assert_ok();
	}
	assert!(Arc::ptr_eq(&store.inner.current_root(), &published));
	assert_eq!(local.wal.snapshot().visible_lsn(), boundary);
	assert_eq!(state.file_catalog.files, files);
}

/// A large Base value must fail before the shared durable publication.
#[test]
fn base_preparation_budget_sync_rejects_large_values_before_wal() {
	reject_large_values(IoMode::Sync);
}

/// Actual NativeAsync observes the same resource rejection through reopen.
#[cfg(feature = "native-async-io")]
#[test]
fn base_preparation_budget_native_async_rejects_large_values_before_wal() {
	reject_large_values(IoMode::NativeAsync);
}

/// The payload remains supported under the unchanged production defaults.
#[test]
fn base_preparation_default_budget_sync_accepts_large_values() {
	accept_large_values(IoMode::Sync);
}

/// NativeAsync must accept the same value shapes under default policy.
#[cfg(feature = "native-async-io")]
#[test]
fn base_preparation_default_budget_native_async_accepts_large_values() {
	accept_large_values(IoMode::NativeAsync);
}

/// Small positive control distinguishes a Base payload gap from fixed metadata.
fn reject_large_values(io_mode: IoMode) {
	let (directory, mut options, record, key) = seed(io_mode);
	options.maintenance.max_memory_bytes = NonZeroUsize::new(512 * 1024).assert_some();
	let store = Datastore::open_local(&directory.0, options).assert_ok();
	assert_mode(&store, io_mode);
	store.inner.maintenance.shutdown();
	let mut control = store.begin();
	control.set_base_property(&record, key, Value::from("small")).assert_ok();
	control.commit().assert_ok();
	let local = local(&store);
	let before = store.inner.current_root();
	let boundary = local.wal.snapshot().visible_lsn();
	let files = local.state.lock().file_catalog.files.clone();
	let directories = generation_directories(&directory.0);
	for case in 0..4 {
		let mut candidate = store.begin();
		candidate.set_base_property(&record, key, payload(case)).assert_ok();
		let error = candidate.commit().assert_err();
		assert!(matches!(
			error.status().status(),
			Status::StorageDecodeLimitExceeded { actual, maximum, .. } if actual > maximum
		));
		assert!(Arc::ptr_eq(&store.inner.current_root(), &before));
		assert_eq!(local.wal.snapshot().visible_lsn(), boundary);
		assert_eq!(local.state.lock().file_catalog.files, files);
		assert_eq!(generation_directories(&directory.0), directories);
		assert_value(&store, &record, key, &Value::from("small"));
	}
	drop(before);
	drop(store);
	let reopened = Datastore::open_local(&directory.0, options).assert_ok();
	assert_mode(&reopened, io_mode);
	assert_value(&reopened, &record, key, &Value::from("small"));
}

/// Large admitted records keep exact values after checkpoint and cold recovery.
fn accept_large_values(io_mode: IoMode) {
	let (directory, options, record, key) = seed(io_mode);
	for case in 0..4 {
		let store = Datastore::open_local(&directory.0, options).assert_ok();
		assert_mode(&store, io_mode);
		store.inner.maintenance.shutdown();
		let mut write = store.begin();
		write.set_base_property(&record, key, payload(case)).assert_ok();
		write.commit().assert_ok();
		assert_value(&store, &record, key, &payload(case));
		store.checkpoint(CheckpointMode::Restart).assert_ok();
		drop(store);
		let reopened = Datastore::open_local(&directory.0, options).assert_ok();
		assert_mode(&reopened, io_mode);
		assert_value(&reopened, &record, key, &payload(case));
	}
}

/// No user index or history consumer can reject the candidate first.
pub(in crate::storage::local_store) fn seed(
	io_mode: IoMode,
) -> (TestDirectory, LocalOptions, RecordId, crate::values::PropertyKeyId) {
	let directory = TestDirectory::new();
	let options = LocalOptions {
		io_mode,
		buffer_pool_pages: 16,
		..LocalOptions::default()
	};
	let store = Datastore::create_local(&directory.0, options).assert_ok();
	assert_mode(&store, io_mode);
	store.inner.maintenance.shutdown();
	let mut seed = store.begin();
	seed.define_class(ClassSchema::new("item", AtomKind::Node, []).assert_ok()).assert_ok();
	let key = seed.intern_property("value").assert_ok();
	let record = "item:a".parse::<RecordId>().assert_ok();
	let mut properties = PropertyMap::new();
	properties.insert(key, Value::from("seed"));
	seed.create_atom(record.clone(), properties).assert_ok();
	seed.commit().assert_ok();
	store.checkpoint(CheckpointMode::Restart).assert_ok();
	drop(store);
	(directory, options, record, key)
}

/// Runtime-owned payloads exercise distinct conversion and Set scratch peaks.
fn payload(case: usize) -> Value {
	const MIB: usize = 1024 * 1024;
	match case {
		0 => Value::from("x".repeat(MIB)),
		1 => Value::Object(Object::from_iter([(
			"inner",
			Value::Array(Array::new(vec![
				Value::from("a".repeat(MIB / 2)),
				Value::from("b".repeat(MIB / 2)),
			])),
		)])),
		2 => {
			let mut values = Set::new();
			values.insert(Value::from("a".repeat(MIB / 2)));
			values.insert(Value::from("b".repeat(MIB / 2)));
			Value::Set(values)
		}
		3 => Value::from(
			RecordId::new(
				"item",
				RecordIdPart::Array(Array::new(vec![Value::from("r".repeat(MIB))])),
			)
			.assert_ok(),
		),
		_ => unreachable!("fixture has four payload shapes"),
	}
}

/// Avoids dumping megabyte-sized operands when a regression loses one value.
fn assert_value(
	store: &Datastore,
	record: &RecordId,
	key: crate::values::PropertyKeyId,
	value: &Value,
) {
	assert!(
		store.begin().atom(record).assert_ok().assert_some().properties().get(key) == Some(value)
	);
}

/// The test never accepts the in-memory backend or an implicit I/O fallback.
pub(in crate::storage::local_store) fn local(store: &Datastore) -> &super::super::LocalStore {
	match &store.inner.backend {
		StoreBackend::Local(local) => local,
		StoreBackend::InMemory => unreachable!("local resource fixture uses RAM backend"),
	}
}

/// Effective I/O mode is checked after every create and reopen.
pub(in crate::storage::local_store) fn assert_mode(store: &Datastore, io_mode: IoMode) {
	assert_eq!(store.local_storage_status().assert_some().options().io_mode, io_mode);
}

/// Statistics frames must share the budget already consumed by Base frames.
#[test]
fn statistics_cumulative_budget_sync_rejects_before_wal() {
	reject_statistics_after_base(IoMode::Sync);
}

/// NativeAsync must reject the same cumulative envelope after cold open.
#[cfg(feature = "native-async-io")]
#[test]
fn statistics_cumulative_budget_native_async_rejects_before_wal() {
	reject_statistics_after_base(IoMode::NativeAsync);
}

/// A small Base payload fits by itself; its statistics/WAL frame does not.
fn reject_statistics_after_base(io_mode: IoMode) {
	use crate::{
		datastore::PublicationDelta,
		storage::{
			atom_store_image::value_context,
			graph_index::with_external_sort_budget,
			local_store::{point_base::BaseUpdatePlan, point_namespace::LabelPlan},
			page::PAGE_SIZE,
		},
	};

	const MAXIMUM: usize = 128 * 1024;
	let (directory, mut options, record, key) = seed(io_mode);
	let defaults = options;
	options.maintenance.max_memory_bytes = NonZeroUsize::new(MAXIMUM).assert_some();
	let store = Datastore::open_local(&directory.0, options).assert_ok();
	assert_mode(&store, io_mode);
	store.inner.maintenance.shutdown();
	let storage = local(&store);
	let published = store.inner.current_root();
	let mut before = published.as_ref().clone();
	storage.hydrate_statistics(&mut before).assert_ok();
	let mut after = before.clone();
	let global = before.resolve_atom(&record).assert_some();
	let value = Value::from("candidate");
	after.set_base_property(global, key, value.clone()).assert_ok();
	let delta = PublicationDelta {
		records: BTreeSet::from([record.clone()]),
		global_slots: BTreeSet::from([global.slot()]),
		overlays: BTreeSet::new(),
		incidence: BTreeSet::new(),
		memberships: BTreeSet::new(),
		point_graph_only: true,
	};
	// The positive prefix prevents another encoder from explaining rejection.
	with_external_sort_budget(MAXIMUM, || {
		let state = storage.state.lock();
		let mut labels = LabelPlan::build(storage, &state, &before, &after, &delta)?;
		let mut context = value_context(&after.schema, after.dictionary.as_ref().clone())?;
		let base = BaseUpdatePlan::build(
			storage,
			&state,
			&before,
			&after,
			&delta,
			&mut context,
			&mut labels.budget,
		)?;
		assert_eq!(base.pages.len(), 3, "Hot, property append and AtomStore header");
		assert!(labels.budget.remaining()? < 2 * PAGE_SIZE);
		Ok(())
	})
	.assert_ok();
	let boundary = storage.wal.snapshot().visible_lsn();
	let files = storage.state.lock().file_catalog.files.clone();
	let directories = generation_directories(&directory.0);
	let mut write = store.begin();
	write.set_base_property(&record, key, value.clone()).assert_ok();
	let outcome = write.commit();
	assert!(outcome.is_err(), "STATISTICS_CUMULATIVE_BUDGET_NOT_ENFORCED");
	let error = outcome.assert_err();
	assert!(matches!(
		error.status().status(),
		Status::StorageDecodeLimitExceeded { actual, maximum, .. }
			if actual > maximum && *maximum == MAXIMUM as u64
	));
	assert!(Arc::ptr_eq(&store.inner.current_root(), &published));
	assert_eq!(storage.wal.snapshot().visible_lsn(), boundary);
	assert_eq!(storage.state.lock().file_catalog.files, files);
	assert_eq!(generation_directories(&directory.0), directories);
	assert_value(&store, &record, key, &Value::from("seed"));
	drop(published);
	drop(store);
	let reopened = Datastore::open_local(&directory.0, options).assert_ok();
	assert_mode(&reopened, io_mode);
	assert_value(&reopened, &record, key, &Value::from("seed"));
	drop(reopened);
	// The unchanged defaults must still admit and durably publish this value.
	let control = Datastore::open_local(&directory.0, defaults).assert_ok();
	assert_mode(&control, io_mode);
	let mut accepted = control.begin();
	accepted.set_base_property(&record, key, value.clone()).assert_ok();
	accepted.commit().assert_ok();
	control.checkpoint(CheckpointMode::Restart).assert_ok();
	drop(control);
	let recovered = Datastore::open_local(&directory.0, defaults).assert_ok();
	assert_mode(&recovered, io_mode);
	assert_value(&recovered, &record, key, &value);
}
