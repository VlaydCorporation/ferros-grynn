//! Graph-schema priming must share parameter admission and context ownership.

use super::*;
use crate::{
	datastore::DatabaseRoot,
	values::{GraphRef, PropertyKeyId, storage::DiskGraphRef},
};

/// An unchanged graph payload must be admitted before its heap copy.
#[test]
fn point_graph_property_priming_rejects_unchanged_heap_text() {
	let (store, graph, key) = graph_fixture(Value::from("x".repeat(64 * 1024)));
	let root = store.inner.current_root();
	assert_graph_fixture(&root, &graph, key);
	assert_eq!(root.schema.parameters().count(), 0);
	let schema = root.schema.graph(graph.id()).assert_some();
	let text = match schema.properties().get(key).assert_some() {
		Value::String(value) => Some(value),
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
		assert_graph_context(&context, &graph, key);
		assert_eq!(context.time_zone_names().len(), 0);
		let mut budget = PublicationBudget::new();
		prime_point_context(&root, &mut context, &mut budget)?;
		assert_graph_context(&context, &graph, key);
		assert_eq!(context.time_zone_names().len(), 0);
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
		println!(
			"PRIME_GRAPH_PROPERTY_COPY_UNADMITTED maximum={maximum} required_copy={copy_bytes}"
		);
	}
	assert!(outcome.is_err(), "PRIME_GRAPH_PROPERTY_COPY_BUDGET_NOT_ENFORCED");
	let error = outcome.assert_err();
	assert!(matches!(error.status().status(),
		Status::StorageDecodeLimitExceeded { limit, actual, maximum: admitted }
			if limit == "runtime storage encoding" && *actual == copy_bytes as u64
				&& *admitted == maximum as u64
	));
	let current = store.inner.current_root();
	assert!(Arc::ptr_eq(&root, &current));
	let accepted = current.schema.graph(graph.id()).assert_some();
	assert_eq!(accepted.reference(), &graph);
	assert_eq!(accepted.properties().get(key), schema.properties().get(key));
}

/// A graph-phase IANA must reduce the following parameter's copy allowance.
#[test]
fn point_graph_property_context_growth_reaches_parameter_admission() {
	let zone = Value::DateTime("2024-01-01T12:00:00+01:00[Europe/Paris]".parse().assert_ok());
	let retained =
		StorageValue::prime_context_bounded(&zone, &mut EncodeContext::default(), 1 << 20)
			.assert_ok();
	assert!(retained > 0);
	let copy_bytes = Text::estimated_copy_heap_bytes(64 * 1024);
	let (store, graph, key) = graph_fixture(zone);
	let mut setup = store.begin();
	setup
		.define_parameter(
			SchemaParameter::new("payload", Value::from("x".repeat(64 * 1024))).assert_ok(),
		)
		.assert_ok();
	setup.commit().assert_ok();
	let root = store.inner.current_root();
	assert_graph_fixture(&root, &graph, key);
	assert_eq!(root.schema.parameters().count(), 1);
	let high_available =
		with_external_sort_budget(retained + copy_bytes, || -> FerrosGrynnResult<usize> {
			let mut context = value_context(&root.schema, root.dictionary.as_ref().clone())?;
			assert_graph_context(&context, &graph, key);
			assert_eq!(context.time_zone_names().len(), 0);
			let mut budget = PublicationBudget::new();
			prime_point_context(&root, &mut context, &mut budget)?;
			assert_graph_context(&context, &graph, key);
			assert_eq!(context.time_zone_names().len(), 1);
			assert_eq!(context.time_zone_names().next().assert_some().as_str(), "Europe/Paris");
			budget.available_bytes()
		})
		.assert_ok();
	let outcome =
		with_external_sort_budget(retained + copy_bytes - 1, || -> FerrosGrynnResult<()> {
			let mut context = value_context(&root.schema, root.dictionary.as_ref().clone())?;
			let mut budget = PublicationBudget::new();
			prime_point_context(&root, &mut context, &mut budget)
		});
	if outcome.is_ok() {
		println!(
			"PRIME_GRAPH_CONTEXT_GROWTH_UNADMITTED required_growth={retained} \
			 required_copy={copy_bytes} high_available={high_available}"
		);
	}
	assert!(outcome.is_err(), "PRIME_GRAPH_CONTEXT_GROWTH_NOT_RETAINED");
	let error = outcome.assert_err();
	assert!(matches!(error.status().status(),
		Status::StorageDecodeLimitExceeded { limit, actual, maximum }
			if limit == "runtime storage encoding" && *actual == copy_bytes as u64
				&& *maximum == (copy_bytes - 1) as u64
	));
	assert_eq!(high_available, copy_bytes);
	assert!(Arc::ptr_eq(&root, &store.inner.current_root()));
}

/// The committed graph has schema properties and no occurrence payloads.
fn graph_fixture(value: Value) -> (Datastore, GraphRef, PropertyKeyId) {
	let store = Datastore::in_memory();
	store.inner.maintenance.shutdown();
	let mut setup = store.begin();
	let key = setup.intern_property("payload").assert_ok();
	let mut properties = PropertyMap::new();
	properties.insert(key, value);
	let graph = setup.define_graph_with_properties("g", GraphKind::Graph, properties).assert_ok();
	setup.commit().assert_ok();
	(store, graph, key)
}

/// Excludes classes, analyzers, extra graphs and node/edge state as causes.
fn assert_graph_fixture(root: &DatabaseRoot, graph: &GraphRef, key: PropertyKeyId) {
	assert_eq!(root.schema.all_graphs().count(), 1);
	assert_eq!(root.schema.classes().count(), 0);
	assert_eq!(root.schema.analyzers().count(), 0);
	assert_eq!(root.dictionary.len(), 1);
	assert_eq!(root.dictionary.get("payload"), Some(key));
	let schema = root.schema.graph(graph.id()).assert_some();
	assert_eq!(schema.reference(), graph);
	assert_eq!(root.schema.named_graph("g").assert_some().reference(), graph);
	assert_eq!(schema.properties().len(), 1);
	assert!(schema.properties().get(key).is_some());
	assert!(schema.root().is_none());
	assert!(schema.owner_root().is_none());
	assert_eq!(root.graph(graph.id()).assert_some().occurrences().len(), 0);
}

/// Existing graph/property resolvers are prepared independently of priming.
fn assert_graph_context(context: &EncodeContext, graph: &GraphRef, key: PropertyKeyId) {
	assert_eq!(context.property_dictionary().len(), 1);
	assert_eq!(context.property_dictionary().get("payload"), Some(key));
	assert_eq!(context.class_names().count(), 0);
	let mut references = context.graph_references();
	let (disk, runtime) = references.next().assert_some();
	assert_eq!(disk, DiskGraphRef::new(graph.id()));
	assert_eq!(runtime, graph);
	assert!(references.next().is_none());
}
