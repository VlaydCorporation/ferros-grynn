//! Every statistics output shares the same admitted preparation envelope.

use super::{
	base_resource_tests::{assert_mode, local, seed},
	*,
};
use crate::{
	datastore::{DataSpaceId, IoMode, PublicationDelta},
	gql_status::Status,
	storage::{
		graph_index::with_external_sort_budget,
		local_store::{point_namespace::LabelPlan, point_statistics::append_statistics_pages},
		page::PAGE_SIZE,
		types::DataFileKind,
	},
};

/// Four ordinary deltas exercise each retained statistics output path.
#[derive(Clone, Copy)]
enum StatisticsCase {
	/// Inherited properties change Base, graph and label roots together.
	Base,
	/// An unattached deletion also changes the Base class page.
	Lifecycle,
	/// Existing-class attach changes graph class and label pages.
	Membership,
	/// An overlay changes only the graph and label roots.
	Overlay,
}

/// Sync admits source scratch and every selected Base frame.
#[test]
fn statistics_accounting_sync_base_fanout() {
	statistics_outputs(IoMode::Sync, StatisticsCase::Base);
}

/// Sync admits source scratch and every selected Lifecycle frame.
#[test]
fn statistics_accounting_sync_base_lifecycle() {
	statistics_outputs(IoMode::Sync, StatisticsCase::Lifecycle);
}

/// Sync admits source scratch and every selected Membership frame.
#[test]
fn statistics_accounting_sync_membership() {
	statistics_outputs(IoMode::Sync, StatisticsCase::Membership);
}

/// Sync admits source scratch and every selected Overlay frame.
#[test]
fn statistics_accounting_sync_overlay() {
	statistics_outputs(IoMode::Sync, StatisticsCase::Overlay);
}

/// NativeAsync admits source scratch and every selected Base frame.
#[cfg(feature = "native-async-io")]
#[test]
fn statistics_accounting_native_async_base_fanout() {
	statistics_outputs(IoMode::NativeAsync, StatisticsCase::Base);
}

/// NativeAsync admits source scratch and every selected Lifecycle frame.
#[cfg(feature = "native-async-io")]
#[test]
fn statistics_accounting_native_async_base_lifecycle() {
	statistics_outputs(IoMode::NativeAsync, StatisticsCase::Lifecycle);
}

/// NativeAsync admits source scratch and every selected Membership frame.
#[cfg(feature = "native-async-io")]
#[test]
fn statistics_accounting_native_async_membership() {
	statistics_outputs(IoMode::NativeAsync, StatisticsCase::Membership);
}

/// NativeAsync admits source scratch and every selected Overlay frame.
#[cfg(feature = "native-async-io")]
#[test]
fn statistics_accounting_native_async_overlay() {
	statistics_outputs(IoMode::NativeAsync, StatisticsCase::Overlay);
}

/// Preparation-only checks do not substitute for the separate commit
/// regressions.
fn statistics_outputs(mode: IoMode, case: StatisticsCase) {
	const MAXIMUM: usize = 512 * 1024;
	let (directory, options, record, key) = seed(mode);
	let store = Datastore::open_local(&directory.0, options).assert_ok();
	assert_mode(&store, mode);
	store.inner.maintenance.shutdown();
	let spare = "item:spare".parse::<RecordId>().assert_ok();
	let label = crate::values::Text::from(
		"statistics_resource_heap_label_which_exceeds_the_inline_text_capacity",
	);
	let mut setup = store.begin();
	let added = setup.intern_property("added").assert_ok();
	let graph = setup.define_graph("statistics", GraphKind::Graph).assert_ok().id();
	setup.create_atom(spare.clone(), PropertyMap::new()).assert_ok();
	let selected = setup.attach_node(graph, &record, Some(label.clone()), None).assert_ok();
	setup.commit().assert_ok();
	store.checkpoint(CheckpointMode::Restart).assert_ok();
	drop(store);
	let store = Datastore::open_local(&directory.0, options).assert_ok();
	assert_mode(&store, mode);
	store.inner.maintenance.shutdown();
	let storage = local(&store);
	let published = store.inner.current_root();
	let mut before = published.as_ref().clone();
	storage.hydrate_statistics(&mut before).assert_ok();
	let mut after = before.clone();
	let global = before.resolve_atom(&record).assert_some();
	let other = before.resolve_atom(&spare).assert_some();
	let mut delta = PublicationDelta {
		records: BTreeSet::new(),
		global_slots: BTreeSet::new(),
		overlays: BTreeSet::new(),
		incidence: BTreeSet::new(),
		memberships: BTreeSet::new(),
		point_graph_only: true,
	};
	match case {
		StatisticsCase::Base => {
			after.set_base_property(global, added, Value::from(7_i64)).assert_ok();
			delta.records.insert(record.clone());
			delta.global_slots.insert(global.slot());
		}
		StatisticsCase::Lifecycle => {
			after.remove_atom(other).assert_ok();
			delta.records.insert(spare);
			delta.global_slots.insert(other.slot());
		}
		StatisticsCase::Membership => {
			let local = after.attach_node(graph, other, Some(label), None).assert_ok();
			delta.incidence.insert((graph, local));
			delta.memberships.insert((graph, other));
		}
		StatisticsCase::Overlay => {
			after.set_overlay(graph, selected, added, Value::from(7_i64)).assert_ok();
			delta.overlays.insert((graph, global));
		}
	}
	let boundary = storage.wal.snapshot().visible_lsn();
	let files = storage.state.lock().file_catalog.files.clone();
	let directories = generation_directories(&directory.0);
	let base = DataSpaceId::Database;
	let effective = DataSpaceId::Graph(graph);
	let expected = match case {
		StatisticsCase::Base => BTreeSet::from([
			(base, Some(DataFileKind::ClassStatistics), true),
			(effective, Some(DataFileKind::Statistics), true),
			(effective, Some(DataFileKind::LabelStatistics), true),
		]),
		StatisticsCase::Lifecycle => BTreeSet::from([
			(base, Some(DataFileKind::ClassStatistics), true),
			(base, Some(DataFileKind::ClassStatistics), false),
		]),
		StatisticsCase::Membership => BTreeSet::from([
			(effective, Some(DataFileKind::Statistics), true),
			(effective, Some(DataFileKind::Statistics), false),
			(effective, Some(DataFileKind::LabelStatistics), true),
		]),
		StatisticsCase::Overlay => BTreeSet::from([
			(effective, Some(DataFileKind::Statistics), true),
			(effective, Some(DataFileKind::LabelStatistics), true),
		]),
	};
	with_external_sort_budget(MAXIMUM, || {
		let state = storage.state.lock();
		let mut labels = LabelPlan::build(storage, &state, &before, &after, &delta)?;
		let remaining = labels.budget.remaining()?;
		let mut pages = Vec::new();
		append_statistics_pages(storage, &state, &before, &after, &delta, &mut labels, &mut pages)?;
		assert_eq!(pages.len(), expected.len());
		let shapes = pages
			.iter()
			.map(|(key, _)| (key.space_id(), key.file_id().kind(), key.page_no().get() == 0))
			.collect::<BTreeSet<_>>();
		assert_eq!(shapes, expected);
		let charged = remaining.checked_sub(labels.budget.remaining()?).assert_some();
		assert!(charged >= 2 * PAGE_SIZE * pages.len(), "STATISTICS_FRAMES_NOT_CHARGED");
		Ok(())
	})
	.assert_ok();
	with_external_sort_budget(MAXIMUM, || {
		let state = storage.state.lock();
		let mut labels = LabelPlan::build(storage, &state, &before, &after, &delta)?;
		let remaining = labels.budget.remaining()?;
		labels.budget.retain(remaining.checked_sub(PAGE_SIZE - 1).assert_some())?;
		let mut pages = Vec::new();
		let outcome = append_statistics_pages(storage, &state, &before, &after, &delta, &mut labels, &mut pages);
		assert!(outcome.is_err(), "STATISTICS_SHARED_TARGETS_NOT_ADMITTED");
		let error = outcome.assert_err();
		assert!(matches!(error.status().status(), Status::StorageDecodeLimitExceeded { actual, maximum, .. } if actual > maximum));
		assert!(pages.is_empty());
		Ok(())
	}).assert_ok();
	assert!(Arc::ptr_eq(&store.inner.current_root(), &published));
	assert_eq!(storage.wal.snapshot().visible_lsn(), boundary);
	assert_eq!(storage.state.lock().file_catalog.files, files);
	assert_eq!(generation_directories(&directory.0), directories);
	drop(after);
	drop(before);
	drop(published);
	drop(store);
	let recovered = Datastore::open_local(&directory.0, options).assert_ok();
	assert_mode(&recovered, mode);
	assert_eq!(recovered.inner.current_root().graph(graph).assert_some().occurrences().len(), 1);
	assert_eq!(
		recovered.begin().atom(&record).assert_ok().assert_some().properties().get(key),
		Some(&Value::from("seed"))
	);
}
