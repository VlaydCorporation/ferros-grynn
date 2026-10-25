//! Restored catalog IANA names must share the point publication envelope.

use super::*;
use crate::storage::local_store::image::point_value_context;

/// A restored table must be admitted even when no runtime payload adds names.
#[test]
fn point_restored_zone_table_rejects_before_copy() {
	let directory = TestDirectory::new();
	let store = Datastore::create_local(&directory.0, LocalOptions::default()).assert_ok();
	store.inner.maintenance.shutdown();
	let mut setup = store.begin();
	setup.define_parameter(SchemaParameter::new("a_zone", zone_value()).assert_ok()).assert_ok();
	setup.commit().assert_ok();
	let root = store.inner.current_root();
	assert_initial_resolvers_empty(&root);
	assert_eq!(root.schema.parameters().count(), 1);
	let storage = super::super::base_resource_tests::local(&store);
	let state = storage.state.lock();
	let names = state._database_catalog.time_zones.as_ref();
	assert_eq!(names.iter().map(Text::as_str).collect::<Vec<_>>(), ["Europe/Paris"]);
	let wal = storage.wal.snapshot().commit_ts();
	let retained = single_zone_growth();
	assert!(retained > 1);
	let remaining = with_external_sort_budget(retained, || -> FerrosGrynnResult<usize> {
		let mut budget = PublicationBudget::new();
		let mut context = point_value_context(&root, names, &mut budget)?;
		assert_zone_identity(&mut context);
		budget.available_bytes()
	})
	.assert_ok();
	let maximum = retained - 1;
	let outcome = with_external_sort_budget(maximum, || -> FerrosGrynnResult<()> {
		let mut budget = PublicationBudget::new();
		point_value_context(&root, names, &mut budget).map(drop)
	});
	if outcome.is_ok() {
		println!(
			"RESTORED_ZONE_UNADMITTED required={retained} maximum={maximum} \
			 high_available={remaining}"
		);
	}
	assert!(outcome.is_err(), "RESTORED_ZONE_TABLE_BUDGET_NOT_ENFORCED");
	let error = outcome.assert_err();
	assert!(matches!(
		error.status().status(),
		Status::StorageDecodeLimitExceeded { limit, actual, maximum: admitted }
			if limit == "runtime storage encoding"
				&& *actual == retained as u64
				&& *admitted == maximum as u64
	));
	assert_eq!(remaining, 0);
	assert_eq!(names[0].as_str(), "Europe/Paris");
	assert!(Arc::ptr_eq(&root, &store.inner.current_root()));
	assert_eq!(storage.wal.snapshot().commit_ts(), wal);
}

/// Published names remain live while a later unchanged parameter is copied.
#[test]
fn point_restored_zone_growth_reaches_parameter_admission() {
	let directory = TestDirectory::new();
	let store = Datastore::create_local(&directory.0, LocalOptions::default()).assert_ok();
	store.inner.maintenance.shutdown();
	let mut setup = store.begin();
	setup.define_parameter(SchemaParameter::new("a_zone", zone_value()).assert_ok()).assert_ok();
	setup
		.define_parameter(
			SchemaParameter::new("b_payload", Value::from("x".repeat(64 * 1024))).assert_ok(),
		)
		.assert_ok();
	setup.commit().assert_ok();
	let root = store.inner.current_root();
	assert_initial_resolvers_empty(&root);
	assert_eq!(root.schema.parameters().count(), 2);
	let parameter = root.schema.parameter("b_payload").assert_some();
	let identity = parameter.id().assert_some();
	let storage = super::super::base_resource_tests::local(&store);
	let state = storage.state.lock();
	let names = state._database_catalog.time_zones.as_ref();
	assert_eq!(names.iter().map(Text::as_str).collect::<Vec<_>>(), ["Europe/Paris"]);
	let wal = storage.wal.snapshot().commit_ts();
	let retained = single_zone_growth();
	let copy = Text::estimated_copy_heap_bytes(64 * 1024);
	let remaining = with_external_sort_budget(retained + copy, || -> FerrosGrynnResult<usize> {
		let mut budget = PublicationBudget::new();
		let mut context = point_value_context(&root, names, &mut budget)?;
		assert_zone_identity(&mut context);
		budget.available_bytes()
	})
	.assert_ok();
	let outcome = with_external_sort_budget(retained + copy - 1, || -> FerrosGrynnResult<()> {
		let mut budget = PublicationBudget::new();
		point_value_context(&root, names, &mut budget).map(drop)
	});
	if outcome.is_ok() {
		println!(
			"RESTORED_ZONE_GROWTH_UNADMITTED required_growth={retained} required_copy={copy} \
			 high_available={remaining}"
		);
	}
	assert!(outcome.is_err(), "RESTORED_ZONE_GROWTH_NOT_RETAINED");
	let error = outcome.assert_err();
	assert!(matches!(
		error.status().status(),
		Status::StorageDecodeLimitExceeded { limit, actual, maximum }
			if limit == "runtime storage encoding"
				&& *actual == copy as u64
				&& *maximum == (copy - 1) as u64
	));
	assert_eq!(remaining, copy);
	let current = store.inner.current_root();
	assert!(Arc::ptr_eq(&root, &current));
	let accepted = current.schema.parameter("b_payload").assert_some();
	assert_eq!(accepted.id(), Some(identity));
	assert_eq!(accepted.value(), parameter.value());
	assert_eq!(storage.wal.snapshot().commit_ts(), wal);
}

/// One existing bounded runtime conversion gives an independent table bound.
fn single_zone_growth() -> usize {
	let value = zone_value();
	let retained =
		StorageValue::prime_context_bounded(&value, &mut EncodeContext::default(), 1 << 20)
			.assert_ok();
	assert!(retained > 0);
	retained
}

/// No initial dictionary/class/graph ownership obscures the restored-name case.
fn assert_initial_resolvers_empty(root: &crate::datastore::DatabaseRoot) {
	assert_eq!(root.dictionary.len(), 0);
	assert_eq!(root.schema.classes().count(), 0);
	assert_eq!(root.schema.all_graphs().count(), 0);
	assert_eq!(root.schema.analyzers().count(), 0);
}

/// Restored order and the resulting durable timestamp identity remain exact.
fn assert_zone_identity(context: &mut EncodeContext) {
	assert_eq!(context.property_dictionary().len(), 0);
	assert_eq!(context.class_names().count(), 0);
	assert_eq!(context.graph_references().count(), 0);
	assert_eq!(context.time_zone_names().map(Text::as_str).collect::<Vec<_>>(), ["Europe/Paris"]);
	let value = zone_value();
	let mut legacy = EncodeContext::default();
	assert_eq!(legacy.register_time_zone_name("Europe/Paris".into()).assert_ok(), 1);
	let expected = StorageValue::from_runtime(value, &mut legacy).assert_ok();
	let decoded = expected.clone().into_runtime(&context.decoder()).assert_ok();
	assert_eq!(StorageValue::from_runtime(decoded, context).assert_ok(), expected);
}

/// The same named timestamp supplies publication and independent conversion.
fn zone_value() -> Value {
	Value::DateTime("2024-01-01T12:00:00+01:00[Europe/Paris]".parse().assert_ok())
}
