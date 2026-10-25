//! Independent output admission probes use real transaction-private roots.
//! These builder tests do not substitute for commit and recovery regressions.

use std::sync::Arc;

use super::*;
use crate::{
	datastore::{Datastore, IoMode},
	gql_status::Status,
	storage::{
		graph_index::with_external_sort_budget,
		local_store::tests::base_resource_tests::{assert_mode, local, seed},
		page::PAGE_SIZE,
	},
	test_assertions::{AssertOption as _, AssertResult as _},
	values::PropertyMap,
};

/// Sync Count output must be admitted independently of the Primary builder.
#[test]
fn count_output_budget_sync_rejects_before_retaining_frame() {
	count_output(IoMode::Sync);
}

/// Actual NativeAsync uses the same isolated Count admission boundary.
#[cfg(feature = "native-async-io")]
#[test]
fn count_output_budget_native_async_rejects_before_retaining_frame() {
	count_output(IoMode::NativeAsync);
}

/// Sync generation replacement must admit its retained Primary leaf.
#[test]
fn primary_output_budget_sync_rejects_before_retaining_leaf() {
	primary_output(IoMode::Sync);
}

/// NativeAsync must not bypass the same Primary output envelope.
#[cfg(feature = "native-async-io")]
#[test]
fn primary_output_budget_native_async_rejects_before_retaining_leaf() {
	primary_output(IoMode::NativeAsync);
}

/// Three slots cover simultaneous probe and destination vector backing.
fn output_boundary() -> usize {
	PAGE_SIZE
		+ 3 * size_of::<(PageKey, Box<AlignedPage>)>()
		+ TxHandle::staging_bytes(1).assert_ok()
		+ 4096
}

/// This probe never invokes Primary or stages the prepared Count page.
///
/// # Errors
/// Preserves source validation and builder admission errors.
fn count_candidate(
	store: &Datastore,
	binding: &IndexBinding<GlobalAtomId>,
	maximum: usize,
) -> FerrosGrynnResult<(BTreeSet<IndexId>, PointPages)> {
	with_external_sort_budget(maximum, || {
		let storage = local(store);
		let state = storage.state.lock();
		let mut changed = BTreeSet::new();
		let mut pages = Vec::new();
		let mut budget = super::super::point_namespace::PublicationBudget::new();
		append_count_page(storage, &state, None, binding, &mut changed, &mut pages, &mut budget)?;
		Ok((changed, pages))
	})
}

/// Population comes from an actual public create in an uncommitted writer.
fn count_output(mode: IoMode) {
	let (directory, options, record, key) = seed(mode);
	let store = Datastore::open_local(&directory.0, options).assert_ok();
	assert_mode(&store, mode);
	store.inner.maintenance.shutdown();
	let mut setup = store.begin();
	setup
		.define_index(
			"item",
			crate::datastore::ClassIndex::new(
				"population",
				ClassIndexKind::Count,
				IndexScope::Base,
				None::<&str>,
			)
			.assert_ok(),
		)
		.assert_ok();
	setup.commit().assert_ok();
	store.checkpoint(crate::datastore::CheckpointMode::Restart).assert_ok();
	drop(store);
	let store = Datastore::open_local(&directory.0, options).assert_ok();
	assert_mode(&store, mode);
	store.inner.maintenance.shutdown();
	let storage = local(&store);
	let before = store.inner.current_root();
	let boundary = storage.wal.snapshot().visible_lsn();
	let files = storage.state.lock().file_catalog.files.clone();
	// The public setter materializes deferred indexes without changing
	// population.
	let mut writer = store.begin();
	writer.set_base_property(&record, key, crate::values::Value::from("count-private")).assert_ok();
	let unchanged_root = writer.query_snapshot().assert_ok().logical_root().clone();
	let source = unchanged_root
		.indexes
		.base
		.values()
		.find(|binding| binding.kind == ClassIndexKind::Count)
		.assert_some();
	assert!(matches!(source.entries, IndexEntries::Count(1)));
	let (unchanged, pages) = count_candidate(&store, source, PAGE_SIZE).assert_ok();
	assert!(unchanged.is_empty() && pages.is_empty());
	writer.create_atom("item:b".parse().assert_ok(), PropertyMap::new()).assert_ok();
	let after = writer.query_snapshot().assert_ok().logical_root().clone();
	let binding = after.indexes.base.get(&source.id).assert_some();
	assert!(matches!(binding.entries, IndexEntries::Count(2)));
	let (changed, pages) =
		count_candidate(&store, binding, crate::storage::graph_index::external_sort_run_budget())
			.assert_ok();
	assert_eq!(changed, BTreeSet::from([binding.id]));
	assert_eq!(pages.len(), 1);
	assert_eq!(decode_count_index(&pages[0].1).assert_ok().atom_count, 2);
	assert_eq!(
		pages[0].0,
		PageKey::new(
			DataSpaceId::Database,
			storage.state.lock().bindings.physical_indexes[&binding.id].file_id,
			PageNo::new(0)
		)
	);
	drop(pages);
	let (changed, pages) = count_candidate(&store, binding, output_boundary()).assert_ok();
	assert_eq!(changed, BTreeSet::from([binding.id]));
	assert_eq!(pages.len(), 1);
	assert_eq!(decode_count_index(&pages[0].1).assert_ok().atom_count, 2);
	drop(pages);
	let maximum = output_boundary() - 1;
	let result = count_candidate(&store, binding, maximum);
	if let Ok((changed, pages)) = &result {
		assert_eq!(changed, &BTreeSet::from([binding.id]));
		assert_eq!(pages.len(), 1);
		println!("COUNT_OUTPUT_UNADMITTED {mode:?} pages=1 maximum={maximum}");
	}
	assert!(result.is_err(), "COUNT_OUTPUT_BUDGET_NOT_ENFORCED");
	let error = result.err().assert_some();
	assert!(matches!(error.status().status(),
		Status::StorageDecodeLimitExceeded { actual, maximum: limit, .. }
		if *actual == output_boundary() as u64 && *limit == maximum as u64
	));
	assert!(Arc::ptr_eq(&store.inner.current_root(), &before));
	assert_eq!(storage.wal.snapshot().visible_lsn(), boundary);
	assert_eq!(storage.state.lock().file_catalog.files, files);
}

/// Stable Primary replacement has no scalar or Count builder to reject first.
///
/// # Errors
/// Propagates canonical-key, physical source and admission failures.
fn primary_candidate(
	store: &Datastore,
	after: &DatabaseRoot,
	mutations: &[PrimaryIndexMutation<GlobalAtomId>],
	maximum: usize,
) -> FerrosGrynnResult<(PhysicalIndexBinding, PointPages, bool)> {
	with_external_sort_budget(maximum, || {
		let storage = local(store);
		let state = storage.state.lock();
		let mut context = crate::storage::atom_store_image::value_context(
			&after.schema,
			after.dictionary.as_ref().clone(),
		)?;
		let mut layout = LayoutBuilder::new(state.file_catalog.next_file_instance_id);
		let mut cleanup = None;
		let generation = Uuid::now_v7().to_string();
		let mut budget = super::super::point_namespace::PublicationBudget::new();
		let (binding, pages) = prepare_primary(
			storage,
			&state,
			after,
			mutations,
			&mut context,
			&generation,
			&mut layout,
			&mut cleanup,
			&mut budget,
		)?;
		Ok((binding, pages, layout.has_new_files() || cleanup.is_some()))
	})
}

/// Canonical keys keep the complete before and after global generations.
fn primary_output(mode: IoMode) {
	let (directory, options, record, _) = seed(mode);
	let store = Datastore::open_local(&directory.0, options).assert_ok();
	assert_mode(&store, mode);
	store.inner.maintenance.shutdown();
	let storage = local(&store);
	let before = store.inner.current_root();
	assert!(before.indexes.base.is_empty());
	let old = before.resolve_atom(&record).assert_some();
	let boundary = storage.wal.snapshot().visible_lsn();
	let files = storage.state.lock().file_catalog.files.clone();
	let physical = storage.state.lock()._database_catalog.atom_store.primary;
	let mut writer = store.begin();
	writer.delete_atom(&record).assert_ok();
	writer.create_atom(record.clone(), PropertyMap::new()).assert_ok();
	let after = writer.query_snapshot().assert_ok().logical_root().clone();
	let new = after.resolve_atom(&record).assert_some();
	assert_eq!(new.slot(), old.slot());
	assert_eq!(new.generation(), old.generation() + 1);
	let mut context = crate::storage::atom_store_image::value_context(
		&after.schema,
		after.dictionary.as_ref().clone(),
	)
	.assert_ok();
	let key = encode_record_id(&record, &mut context).assert_ok();
	let mutations = [
		PrimaryIndexMutation {
			key: key.clone(),
			atom: old,
			insert: false,
		},
		PrimaryIndexMutation {
			key,
			atom: new,
			insert: true,
		},
	];
	let (binding, pages, streamed) = primary_candidate(
		&store,
		&after,
		&mutations,
		crate::storage::graph_index::external_sort_run_budget(),
	)
	.assert_ok();
	assert_eq!(binding, physical);
	assert_eq!(pages.len(), 1);
	assert_eq!(pages[0].0, PageKey::new(DataSpaceId::Database, physical.file_id, physical.root));
	assert!(!streamed);
	drop(pages);
	let (binding, pages, streamed) =
		primary_candidate(&store, &after, &mutations, output_boundary()).assert_ok();
	assert_eq!(binding, physical);
	assert_eq!(pages.len(), 1);
	assert!(!streamed);
	drop(pages);
	let maximum = output_boundary() - 1;
	let result = primary_candidate(&store, &after, &mutations, maximum);
	if let Ok((binding, pages, streamed)) = &result {
		assert_eq!(*binding, physical);
		assert_eq!(pages.len(), 1);
		assert!(!streamed);
		println!("PRIMARY_OUTPUT_UNADMITTED {mode:?} pages=1 maximum={maximum}");
	}
	assert!(result.is_err(), "PRIMARY_OUTPUT_BUDGET_NOT_ENFORCED");
	let error = result.err().assert_some();
	assert!(matches!(error.status().status(),
		Status::StorageDecodeLimitExceeded { actual, maximum: limit, .. }
		if *actual == output_boundary() as u64 && *limit == maximum as u64
	));
	assert!(Arc::ptr_eq(&store.inner.current_root(), &before));
	assert_eq!(storage.wal.snapshot().visible_lsn(), boundary);
	assert_eq!(storage.state.lock().file_catalog.files, files);
}
