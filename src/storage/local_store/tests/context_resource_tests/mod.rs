//! Point-context priming must admit unchanged catalog payload copies.

use super::*;
use crate::{
	datastore::{PublicationDelta, SchemaParameter},
	gql_status::{FerrosGrynnResult, Status},
	storage::{
		atom_store_image::value_context,
		graph_index::with_external_sort_budget,
		local_store::{
			image::prime_point_context,
			point_namespace::{LabelPlan, PublicationBudget},
		},
	},
	values::{
		Text,
		storage::{EncodeContext, StorageValue},
	},
};

/// Graph-schema values share the point envelope with parameters.
mod graph_property_tests;

/// A heap copy is the only nonempty priming input in this owner fixture.
#[test]
fn point_parameter_priming_rejects_before_copying_unchanged_heap_text() {
	let store = Datastore::in_memory();
	store.inner.maintenance.shutdown();
	let mut setup = store.begin();
	setup
		.define_parameter(
			SchemaParameter::new("payload", Value::from("x".repeat(64 * 1024))).assert_ok(),
		)
		.assert_ok();
	setup.commit().assert_ok();
	let root = store.inner.current_root();
	assert_eq!(root.schema.parameters().count(), 1);
	assert_eq!(root.schema.classes().count(), 0);
	assert_eq!(root.schema.all_graphs().count(), 0);
	assert_eq!(root.schema.analyzers().count(), 0);
	assert_eq!(root.dictionary.len(), 0);
	let parameter = root.schema.parameter("payload").assert_some();
	let identity = parameter.id().assert_some();
	let text = match parameter.value() {
		Value::String(text) => Some(text),
		_ => None,
	}
	.assert_some();
	let clone_bytes = text.estimated_clone_heap_bytes();
	assert!(clone_bytes > 0);
	let maximum = clone_bytes - 1;
	let copy_bytes = Text::estimated_copy_heap_bytes(text.len());
	assert!(copy_bytes > maximum);
	with_external_sort_budget(copy_bytes, || -> FerrosGrynnResult<()> {
		let mut context = value_context(&root.schema, root.dictionary.as_ref().clone())?;
		let mut budget = PublicationBudget::new();
		prime_point_context(&root, &mut context, &mut budget)?;
		assert_empty_context(&context);
		assert_eq!(budget.available_bytes()?, copy_bytes);
		Ok(())
	})
	.assert_ok();
	let outcome = with_external_sort_budget(maximum, || -> FerrosGrynnResult<()> {
		let mut context = value_context(&root.schema, root.dictionary.as_ref().clone())?;
		let mut budget = PublicationBudget::new();
		prime_point_context(&root, &mut context, &mut budget)
	});
	if outcome.is_ok() {
		println!("PRIME_PARAMETER_COPY_UNADMITTED maximum={maximum} required_copy={copy_bytes}");
	}
	assert!(outcome.is_err(), "PRIME_PARAMETER_COPY_BUDGET_NOT_ENFORCED");
	let error = outcome.assert_err();
	assert!(matches!(
		error.status().status(),
		Status::StorageDecodeLimitExceeded { limit, actual, maximum: admitted }
			if limit == "runtime storage encoding"
				&& *actual == copy_bytes as u64
				&& *admitted == maximum as u64
				&& actual > admitted
	));
	let current = store.inner.current_root();
	assert!(Arc::ptr_eq(&root, &current));
	let accepted = current.schema.parameter("payload").assert_some();
	assert_eq!(accepted.id(), Some(identity));
	assert_eq!(accepted.value(), parameter.value());
}

/// Empty resolvers exclude unrelated dictionary, class, graph or IANA growth.
fn assert_empty_context(context: &EncodeContext) {
	assert_eq!(context.property_dictionary().len(), 0);
	assert_eq!(context.class_names().count(), 0);
	assert_eq!(context.graph_references().count(), 0);
	assert_eq!(context.time_zone_names().count(), 0);
}

/// A later parameter must share the budget already retained by an earlier IANA.
#[test]
fn point_parameter_priming_keeps_context_growth_between_parameters() {
	let zone = Value::DateTime("2024-01-01T12:00:00+01:00[Europe/Paris]".parse().assert_ok());
	let copy_bytes = Text::estimated_copy_heap_bytes(64 * 1024);
	let retained =
		StorageValue::prime_context_bounded(&zone, &mut EncodeContext::default(), 1 << 20)
			.assert_ok();
	assert!(retained > 0);
	let store = Datastore::in_memory();
	store.inner.maintenance.shutdown();
	let mut setup = store.begin();
	setup.define_parameter(SchemaParameter::new("a_zone", zone).assert_ok()).assert_ok();
	setup
		.define_parameter(
			SchemaParameter::new("b_payload", Value::from("x".repeat(64 * 1024))).assert_ok(),
		)
		.assert_ok();
	setup.commit().assert_ok();
	let root = store.inner.current_root();
	assert_eq!(
		root.schema.parameters().map(|parameter| parameter.name().as_str()).collect::<Vec<_>>(),
		["a_zone", "b_payload"]
	);
	with_external_sort_budget(retained + copy_bytes, || -> FerrosGrynnResult<()> {
		let mut context = value_context(&root.schema, root.dictionary.as_ref().clone())?;
		let mut budget = PublicationBudget::new();
		prime_point_context(&root, &mut context, &mut budget)?;
		assert_eq!(context.time_zone_names().len(), 1);
		assert_eq!(budget.available_bytes()?, copy_bytes);
		Ok(())
	})
	.assert_ok();
	let error =
		with_external_sort_budget(retained + copy_bytes - 1, || -> FerrosGrynnResult<()> {
			let mut context = value_context(&root.schema, root.dictionary.as_ref().clone())?;
			let mut budget = PublicationBudget::new();
			prime_point_context(&root, &mut context, &mut budget)
		})
		.assert_err();
	assert!(matches!(
		error.status().status(),
		Status::StorageDecodeLimitExceeded { limit, actual, maximum }
			if limit == "runtime storage encoding"
				&& *actual == copy_bytes as u64
				&& *maximum == (copy_bytes - 1) as u64
	));
	assert!(Arc::ptr_eq(&root, &store.inner.current_root()));
}

/// Label metadata cannot reset a previously admitted, still-live context.
#[test]
fn point_context_budget_survives_label_plan_handoff() {
	let directory = TestDirectory::new();
	let store = Datastore::create_local(&directory.0, LocalOptions::default()).assert_ok();
	store.inner.maintenance.shutdown();
	let storage = super::base_resource_tests::local(&store);
	let root = store.inner.current_root();
	let value = Value::DateTime("2024-01-01T12:00:00+01:00[Europe/Paris]".parse().assert_ok());
	let mut context = EncodeContext::default();
	let retained = StorageValue::prime_context_bounded(&value, &mut context, 1 << 20).assert_ok();
	assert!(retained > 0);
	let delta = PublicationDelta {
		records: BTreeSet::new(),
		global_slots: BTreeSet::new(),
		overlays: BTreeSet::new(),
		incidence: BTreeSet::new(),
		memberships: BTreeSet::new(),
		point_graph_only: true,
	};
	let maximum = retained + 4096;
	with_external_sort_budget(maximum, || -> FerrosGrynnResult<()> {
		let mut budget = PublicationBudget::new();
		budget.retain(retained)?;
		let state = storage.state.lock();
		let labels = LabelPlan::build_with_budget(storage, &state, &root, &root, &delta, budget)?;
		assert_eq!(labels.budget.available_bytes()?, 0);
		assert_eq!(context.time_zone_names().len(), 1);
		Ok(())
	})
	.assert_ok();
	let error = with_external_sort_budget(maximum - 1, || -> FerrosGrynnResult<()> {
		let mut budget = PublicationBudget::new();
		budget.retain(retained)?;
		let state = storage.state.lock();
		LabelPlan::build_with_budget(storage, &state, &root, &root, &delta, budget).map(drop)
	})
	.assert_err();
	assert!(matches!(
		error.status().status(),
		Status::StorageDecodeLimitExceeded { limit, actual, maximum: admitted }
			if limit == "namespace publication"
				&& *actual == maximum as u64
				&& *admitted == (maximum - 1) as u64
	));
	assert!(Arc::ptr_eq(&root, &store.inner.current_root()));
}

/// Published time-zone prefixes share the point envelope with later values.
mod restored_zone_tests;

/// Initial resolver tables must fit before their first allocation.
mod initial_context_tests;
