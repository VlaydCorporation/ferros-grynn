//! Stable scalar output and WAL copies share the complete publication budget.

use super::{
	base_resource_tests::{assert_mode, local},
	*,
};
use crate::{
	datastore::{IoMode, PublicationDelta, Transaction},
	gql_status::Status,
	storage::{
		graph_index::with_external_sort_budget,
		local_store::{
			image::{initial_context_reservation, point_value_context},
			point_namespace::{LabelPlan, PublicationBudget},
			point_update::PointUpdatePlan,
		},
		page::{AlignedPage, PAGE_SIZE},
		types::PageKey,
		wal::TxHandle,
	},
	values::PropertyKeyId,
};

/// Enough independent leaves exceed the cap even without their WAL copies.
const INDEX_COUNT: usize = 32;
/// The payload allowance stays fixed when initial context ownership is added.
const MAXIMUM: usize = 512 * 1024;

/// Sync must reject the cumulative stable-leaf output before publication.
#[test]
fn scalar_output_budget_sync_rejects_cumulative_pages_before_wal() {
	reject_scalar_output(IoMode::Sync);
}

/// Actual NativeAsync retains the same admission contract after cold open.
#[cfg(feature = "native-async-io")]
#[test]
fn scalar_output_budget_native_async_rejects_cumulative_pages_before_wal() {
	reject_scalar_output(IoMode::NativeAsync);
}

/// Distinct indexed fields prevent duplicate-index validation confounding RED.
fn seed(mode: IoMode) -> (TestDirectory, LocalOptions, Vec<PropertyKeyId>) {
	let directory = TestDirectory::new();
	let options = LocalOptions {
		io_mode: mode,
		buffer_pool_pages: 16,
		..LocalOptions::default()
	};
	let store = Datastore::create_local(&directory.0, options).assert_ok();
	assert_mode(&store, mode);
	store.inner.maintenance.shutdown();
	let names = (0..INDEX_COUNT).map(|index| format!("field{index}")).collect::<Vec<_>>();
	let fields = names.iter().map(|name| ClassField::new(name.as_str(), Type::Int));
	let mut class = ClassSchema::new("budget_item", AtomKind::Node, fields).assert_ok();
	for (index, name) in names.iter().enumerate() {
		let kind = if index % 2 == 0 {
			ClassIndexKind::Standard
		} else {
			ClassIndexKind::Unique
		};
		class = class
			.with_index(
				ClassIndex::new(
					format!("index{index}"),
					kind,
					IndexScope::Base,
					Some(name.as_str()),
				)
				.assert_ok(),
			)
			.assert_ok();
	}
	let mut setup = store.begin();
	setup.define_class(class).assert_ok();
	let keys = names
		.iter()
		.map(|name| setup.intern_property(name.as_str()).assert_ok())
		.collect::<Vec<_>>();
	for row in 0..32_i64 {
		let mut properties = PropertyMap::new();
		for key in &keys {
			properties.insert(*key, Value::from(row));
		}
		setup
			.create_atom(format!("budget_item:n{row}").parse().assert_ok(), properties)
			.assert_ok();
	}
	setup.commit().assert_ok();
	store.checkpoint(CheckpointMode::Restart).assert_ok();
	drop(store);
	(directory, options, keys)
}

/// All writes replace interior keys without changing dictionary or identities.
fn change(store: &Datastore, record: &RecordId, keys: &[PropertyKeyId]) -> Transaction {
	let mut write = store.begin();
	for key in keys {
		write.set_base_property(record, *key, Value::from(1000_i64)).assert_ok();
	}
	write
}

/// The whole Base/statistics prefix must fit before blaming index outputs.
fn assert_prefix_admitted(
	store: &Datastore,
	record: &RecordId,
	keys: &[PropertyKeyId],
	maximum: usize,
) {
	let storage = local(store);
	let mut before = store.inner.current_root().as_ref().clone();
	storage.hydrate_statistics(&mut before).assert_ok();
	let global = before.resolve_atom(record).assert_some();
	let mut after = before.clone();
	for key in keys {
		after.set_base_property(global, *key, Value::from(1000_i64)).assert_ok();
	}
	after.commit_ts = before.commit_ts + 1;
	let delta = PublicationDelta {
		records: BTreeSet::from([record.clone()]),
		global_slots: BTreeSet::from([global.slot()]),
		overlays: BTreeSet::new(),
		incidence: BTreeSet::new(),
		memberships: BTreeSet::new(),
		point_graph_only: true,
	};
	assert!(PointUpdatePlan::eligible(&before, &after, &delta).assert_ok());
	with_external_sort_budget(maximum, || {
		let state = storage.state.lock();
		let mut budget = PublicationBudget::new();
		let mut context =
			point_value_context(&after, &state._database_catalog.time_zones, &mut budget)?;
		let mut labels =
			LabelPlan::build_with_budget(storage, &state, &before, &after, &delta, budget)?;
		let point = PointUpdatePlan::build(
			storage,
			&state,
			&before,
			&after,
			&delta,
			&mut labels,
			&mut context,
		)?;
		drop(point);
		Ok(())
	})
	.assert_ok();
}

/// Rejection preserves root, files, WAL, recovery, and the default-policy path.
fn reject_scalar_output(mode: IoMode) {
	let (directory, defaults, keys) = seed(mode);
	let cold = Datastore::open_local(&directory.0, defaults).assert_ok();
	assert_mode(&cold, mode);
	cold.inner.maintenance.shutdown();
	let (initial, overlap) = initial_context_reservation(&cold.inner.current_root()).assert_ok();
	assert!(initial > 0);
	assert_eq!(overlap, 0);
	drop(cold);
	let maximum = MAXIMUM.checked_add(initial).assert_some();
	let mut limited = defaults;
	limited.maintenance.max_memory_bytes = NonZeroUsize::new(maximum).assert_some();
	let store = Datastore::open_local(&directory.0, limited).assert_ok();
	assert_mode(&store, mode);
	store.inner.maintenance.shutdown();
	let record = "budget_item:n16".parse::<RecordId>().assert_ok();
	let storage = local(&store);
	let before = store.inner.current_root();
	assert_eq!(initial_context_reservation(&before).assert_ok(), (initial, 0));
	let class = before.schema.class("budget_item").assert_some();
	assert_eq!(class.indexes().count(), INDEX_COUNT);
	assert!(class.indexes().all(|index| index.state() == ClassIndexState::Ready));
	let pin = storage.capture_query_snapshot(before.commit_ts).assert_ok().assert_some();
	let bindings = storage.state.lock().bindings.physical_indexes.clone();
	assert_eq!(bindings.len(), INDEX_COUNT);
	for index in class.indexes() {
		let decoded =
			storage.physical_index(&pin, index.id().assert_some()).assert_ok().assert_some();
		let crate::storage::PhysicalIndexEntries::Scalar(entries) = &decoded.entries else {
			unreachable!("scalar budget fixture decoded another index kind");
		};
		assert_eq!(entries.len(), 32);
	}
	assert_prefix_admitted(&store, &record, &keys, maximum);
	let boundary = storage.wal.snapshot().visible_lsn();
	let files = storage.state.lock().file_catalog.files.clone();
	let directories = generation_directory_paths(&directory.0);
	let outcome = change(&store, &record, &keys).commit();
	if outcome.is_ok() {
		// Before the fix, prove this is retained-leaf publication, not
		// fallback.
		assert_eq!(storage.state.lock().file_catalog.files, files);
		assert_eq!(storage.state.lock().bindings.physical_indexes, bindings);
		let index_files = bindings.values().map(|binding| binding.file_id).collect::<BTreeSet<_>>();
		let frames = storage
			.wal
			.recovery_frames()
			.into_iter()
			.filter(|frame| {
				frame.frame_lsn() > boundary && index_files.contains(&frame.key().file_id())
			})
			.collect::<Vec<_>>();
		let observed = frames.iter().map(|frame| frame.key().file_id()).collect::<BTreeSet<_>>();
		assert_eq!(observed, index_files);
		let output_bytes = frames.len() * (PAGE_SIZE + size_of::<(PageKey, Box<AlignedPage>)>())
			+ TxHandle::staging_bytes(frames.len()).assert_ok();
		assert!(output_bytes > MAXIMUM);
		base_lifecycle_tests::assert_base_raw_postings(&store);
		println!(
			"SCALAR_OUTPUT_UNADMITTED {mode:?} pages={} lower_bytes={output_bytes} \
			 maximum={MAXIMUM}",
			frames.len()
		);
	}
	assert!(outcome.is_err(), "SCALAR_OUTPUT_SHARED_BUDGET_NOT_REJECTED");
	let error = outcome.assert_err();
	assert!(matches!(error.status().status(),
		Status::StorageDecodeLimitExceeded { actual, maximum, .. }
		if *actual > *maximum && *maximum == (MAXIMUM + initial) as u64
	));
	assert!(Arc::ptr_eq(&store.inner.current_root(), &before));
	assert_eq!(storage.wal.snapshot().visible_lsn(), boundary);
	assert_eq!(storage.state.lock().file_catalog.files, files);
	assert_eq!(generation_directory_paths(&directory.0), directories);
	drop((pin, before, store));
	let reopened = Datastore::open_local(&directory.0, limited).assert_ok();
	assert_mode(&reopened, mode);
	assert_eq!(
		initial_context_reservation(&reopened.inner.current_root()).assert_ok(),
		(initial, 0)
	);
	for key in &keys {
		assert_eq!(
			reopened.begin().atom(&record).assert_ok().assert_some().properties().get(*key),
			Some(&Value::from(16_i64))
		);
	}
	base_lifecycle_tests::assert_base_raw_postings(&reopened);
	drop(reopened);
	let normal = Datastore::open_local(&directory.0, defaults).assert_ok();
	assert_mode(&normal, mode);
	normal.inner.maintenance.shutdown();
	change(&normal, &record, &keys).commit().assert_ok();
	base_lifecycle_tests::assert_base_raw_postings(&normal);
	normal.checkpoint(CheckpointMode::Restart).assert_ok();
	drop(normal);
	let recovered = Datastore::open_local(&directory.0, defaults).assert_ok();
	assert_mode(&recovered, mode);
	for key in &keys {
		assert_eq!(
			recovered.begin().atom(&record).assert_ok().assert_some().properties().get(*key),
			Some(&Value::from(1000_i64))
		);
	}
	base_lifecycle_tests::assert_base_raw_postings(&recovered);
}
