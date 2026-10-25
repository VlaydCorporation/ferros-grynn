//! Initial resolver ownership must be admitted before context construction.

use super::*;
use crate::{
	datastore::DatabaseRoot,
	storage::local_store::image::point_value_context,
	values::storage::DiskGraphRef,
};

/// A long catalog name makes class/graph name copies visible beside tree
/// backing.
const RESOLVER_NAME: &str = "resolver_with_more_than_inline_text_for_preparation";

/// Each resolver owns a different collection representation.
#[derive(Clone, Copy, Debug)]
enum ResolverCase {
	/// The runtime property dictionary owns a hash map and a dense name vector.
	Property,
	/// Classes own two name/identity trees.
	Class,
	/// Graphs own two reference/identity trees.
	Graph,
}

/// Even one inline property key requires new hash/vector backing.
#[test]
fn point_initial_property_dictionary_rejects_before_clone() {
	assert_initial_admission(ResolverCase::Property);
}

/// Published class names and their two trees are publication ownership.
#[test]
fn point_initial_class_resolvers_reject_before_clone() {
	assert_initial_admission(ResolverCase::Class);
}

/// Published graph descriptors and both directions must be admitted together.
#[test]
fn point_initial_graph_resolvers_reject_before_clone() {
	assert_initial_admission(ResolverCase::Graph);
}

/// A later parameter cannot reuse budget held by an initial resolver table.
#[test]
fn point_initial_dictionary_growth_reaches_parameter_admission() {
	let store = resolver_store(ResolverCase::Property);
	let mut setup = store.begin();
	setup
		.define_parameter(
			SchemaParameter::new("payload", Value::from("x".repeat(64 * 1024))).assert_ok(),
		)
		.assert_ok();
	setup.commit().assert_ok();
	let root = store.inner.current_root();
	assert_eq!(root.schema.parameters().count(), 1);
	let parameter = root.schema.parameter("payload").assert_some();
	let identity = parameter.id().assert_some();
	let copy = Text::estimated_copy_heap_bytes(64 * 1024);
	let high = 1 << 20;
	let remaining = with_external_sort_budget(high, || -> FerrosGrynnResult<usize> {
		let mut budget = PublicationBudget::new();
		let mut context = point_value_context(&root, &[], &mut budget)?;
		assert_resolvers(ResolverCase::Property, &root, &mut context);
		budget.available_bytes()
	})
	.assert_ok();
	let outcome = with_external_sort_budget(copy, || -> FerrosGrynnResult<()> {
		let mut budget = PublicationBudget::new();
		point_value_context(&root, &[], &mut budget).map(drop)
	});
	if outcome.is_ok() {
		println!(
			"INITIAL_DICTIONARY_GROWTH_UNADMITTED required_copy={copy} high_available={remaining}"
		);
	}
	assert!(outcome.is_err(), "INITIAL_DICTIONARY_GROWTH_NOT_RETAINED");
	let error = outcome.assert_err();
	assert!(matches!(
		error.status().status(),
		Status::StorageDecodeLimitExceeded { limit, actual, maximum }
			if limit == "runtime storage encoding"
				&& *actual == copy as u64 && *maximum > 0 && *maximum < copy as u64
	));
	assert!(remaining < high && high - remaining < copy);
	let current = store.inner.current_root();
	assert!(Arc::ptr_eq(&root, &current));
	let accepted = current.schema.parameter("payload").assert_some();
	assert_eq!(accepted.id(), Some(identity));
	assert_eq!(accepted.value(), parameter.value());
}

/// A single resolver separates initial ownership from later payload conversion.
fn assert_initial_admission(case: ResolverCase) {
	let store = resolver_store(case);
	let root = store.inner.current_root();
	assert_eq!(root.schema.parameters().count(), 0);
	let high = 1 << 20;
	let remaining = with_external_sort_budget(high, || -> FerrosGrynnResult<usize> {
		let mut budget = PublicationBudget::new();
		let mut context = point_value_context(&root, &[], &mut budget)?;
		assert_resolvers(case, &root, &mut context);
		budget.available_bytes()
	})
	.assert_ok();
	let outcome = with_external_sort_budget(1, || -> FerrosGrynnResult<()> {
		let mut budget = PublicationBudget::new();
		point_value_context(&root, &[], &mut budget).map(drop)
	});
	if outcome.is_ok() {
		println!("INITIAL_CONTEXT_UNADMITTED case={case:?} maximum=1 high_available={remaining}");
	}
	assert!(outcome.is_err(), "INITIAL_CONTEXT_BUDGET_NOT_ENFORCED case={case:?}");
	let error = outcome.assert_err();
	assert!(matches!(
		error.status().status(),
		Status::StorageDecodeLimitExceeded { limit, actual, maximum: 1 }
			if limit == "namespace publication" && *actual > 1
	));
	assert!(remaining < high);
	assert!(Arc::ptr_eq(&root, &store.inner.current_root()));
}

/// Commit real catalog identities before exercising the physical context owner.
fn resolver_store(case: ResolverCase) -> Datastore {
	let store = Datastore::in_memory();
	store.inner.maintenance.shutdown();
	let mut setup = store.begin();
	match case {
		ResolverCase::Property => {
			let _key = setup.intern_property("p").assert_ok();
		}
		ResolverCase::Class => {
			setup
				.define_class(ClassSchema::new(RESOLVER_NAME, AtomKind::Node, []).assert_ok())
				.assert_ok();
		}
		ResolverCase::Graph => {
			let _graph = setup.define_graph(RESOLVER_NAME, GraphKind::Graph).assert_ok();
		}
	}
	setup.commit().assert_ok();
	store
}

/// Check both resolver directions without repairing any missing entry.
fn assert_resolvers(case: ResolverCase, root: &DatabaseRoot, context: &mut EncodeContext) {
	assert_eq!(root.schema.analyzers().count(), 0);
	assert_eq!(context.time_zone_names().count(), 0);
	match case {
		ResolverCase::Property => {
			assert_eq!(root.dictionary.len(), 1);
			assert_eq!(root.schema.classes().count(), 0);
			assert_eq!(root.schema.all_graphs().count(), 0);
			assert_eq!(context.property_dictionary().len(), 1);
			let key = root.dictionary.get("p").assert_some();
			assert_eq!(context.property_dictionary().get("p"), Some(key));
			assert_eq!(context.property_dictionary().resolve(key).map(Text::as_str), Some("p"));
			assert_eq!(context.class_names().count(), 0);
			assert_eq!(context.graph_references().count(), 0);
		}
		ResolverCase::Class => {
			assert_eq!(root.dictionary.len(), 0);
			assert_eq!(root.schema.classes().count(), 1);
			assert_eq!(root.schema.all_graphs().count(), 0);
			let class = root.schema.class(RESOLVER_NAME).assert_some();
			let id = class.id().assert_some().get();
			assert_eq!(context.property_dictionary().len(), 0);
			assert_eq!(context.class_names().count(), 1);
			assert_eq!(context.class_id(class.name()), Some(id));
			assert_eq!(context.class_name(id), Some(class.name()));
			assert_eq!(context.graph_references().count(), 0);
		}
		ResolverCase::Graph => {
			assert_eq!(root.dictionary.len(), 0);
			assert_eq!(root.schema.classes().count(), 0);
			assert_eq!(root.schema.all_graphs().count(), 1);
			let graph = root.schema.named_graph(RESOLVER_NAME).assert_some();
			let reference = graph.reference();
			let disk = DiskGraphRef::new(reference.id());
			assert_eq!(context.property_dictionary().len(), 0);
			assert_eq!(context.class_names().count(), 0);
			assert_eq!(context.graph_references().count(), 1);
			assert_eq!(context.registered_graph(disk), Some(reference));
			let value = Value::from(reference.clone());
			let encoded = StorageValue::from_runtime(value.clone(), context).assert_ok();
			assert_eq!(encoded, StorageValue::GraphRef(disk));
			assert_eq!(encoded.into_runtime(&context.decoder()).assert_ok(), value);
		}
	}
}

/// The exact peak includes the graph constructor's temporary; only retained
/// resolver ownership remains after its scope ends.
#[test]
fn point_initial_context_exact_peak_preserves_resolvers_and_retention() {
	for case in [ResolverCase::Property, ResolverCase::Class, ResolverCase::Graph] {
		let store = resolver_store(case);
		let root = store.inner.current_root();
		let (retained, overlap) =
			crate::storage::local_store::image::initial_context_reservation(&root).assert_ok();
		assert!(retained > 1);
		assert_eq!(overlap > 0, matches!(case, ResolverCase::Graph));
		let peak = retained.checked_add(overlap).assert_some();
		let error = with_external_sort_budget(peak - 1, || -> FerrosGrynnResult<()> {
			let mut budget = PublicationBudget::new();
			point_value_context(&root, &[], &mut budget).map(drop)
		})
		.assert_err();
		assert!(matches!(error.status().status(),
			Status::StorageDecodeLimitExceeded { limit, actual, maximum }
				if limit == "namespace publication"
					&& *actual == peak as u64 && *maximum == (peak - 1) as u64));
		with_external_sort_budget(peak, || -> FerrosGrynnResult<()> {
			let mut budget = PublicationBudget::new();
			let mut context = point_value_context(&root, &[], &mut budget)?;
			assert_resolvers(case, &root, &mut context);
			assert_eq!(budget.available_bytes()?, overlap);
			Ok(())
		})
		.assert_ok();
		assert!(Arc::ptr_eq(&root, &store.inner.current_root()));
	}
}
