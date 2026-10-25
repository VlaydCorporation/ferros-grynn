#![cfg(test)]
/// Assertions shared with the root unit-test harness.
#[path = "common/assertions.rs"]
pub mod test_assertions;

use std::any::TypeId;

use ferros_grynn as sdk;
use ferros_grynn::{
	Measurable as _,
	datastore::Datastore,
	gql::ParserSettings,
	gql_status::StatusCode,
	graph::{Graph, GraphRead, Mutable},
	values::{PropertyMap, Value},
};
use test_assertions::AssertResult as _;

/// An inline downstream type exercises macro hygiene through the SDK alias.
struct InlineSdkProbe;
sdk::impl_zero_heap!(InlineSdkProbe);

/// Verifies public facades remain importable after internal moves.
#[test]
fn public_facades_remain_importable_after_internal_moves() {
	let _store = Datastore::in_memory();
	let _settings = ParserSettings::default();
	let _value = Value::None;
	let _graph_type = TypeId::of::<Graph<Mutable>>();
	assert!("FG-00000".parse::<StatusCode>().is_ok());
}

/// Re-exports preserve concrete types and trait method lookup.
#[test]
fn reexports_preserve_type_identity_and_graph_traits() {
	assert_eq!(
		TypeId::of::<Graph<Mutable>>(),
		TypeId::of::<ferros_grynn::values::Graph<Mutable>>()
	);
	assert_eq!(
		TypeId::of::<Graph<Mutable>>(),
		TypeId::of::<ferros_grynn::values::runtime::graph::Graph<Mutable>>()
	);
	assert_eq!(TypeId::of::<ParserSettings>(), TypeId::of::<ferros_grynn::syn::ParserSettings>());
	assert_eq!(TypeId::of::<Value>(), TypeId::of::<ferros_grynn::values::runtime::Value>());
	let _ecs_type = TypeId::of::<ferros_grynn::ecs::WorldComponents>();
	let _live_type = TypeId::of::<ferros_grynn::live::LiveExpr>();
	let _changefeed_type = TypeId::of::<ferros_grynn::changefeed::ChangefeedConfig>();
	let _bridge_type = TypeId::of::<Box<dyn ferros_grynn::bridge::Bridge>>();
	let mut graph = Graph::new();
	graph.add_node(None, PropertyMap::new()).assert_ok();
	assert_eq!(GraphRead::node_count(&graph).assert_ok(), 1);
	assert!(ferros_grynn::math::number_is_finite(ferros_grynn::values::Number::Int(1)));
}

/// Exported macros retain their hygiene and single evaluation downstream.
#[test]
fn sdk_macros_work_through_a_downstream_alias() {
	use std::{
		future::Future as _,
		pin::pin,
		task::{Context, Poll, Waker},
	};

	/// A ready branch selector keeps this SDK test independent of an executor.
	macro_rules! select_ready {
		($value:pat = $future:expr => $body:expr $(,)?) => {{
			let $value = $future.await;
			$body
		}};
	}
	assert_eq!(InlineSdkProbe.estimated_heap_usage(), 0);
	assert_eq!(
		InlineSdkProbe.estimated_heap_usage_cache(&sdk::NO_HEAP_ESTIMATOR_CACHE_INSTANCE),
		0
	);
	let mut calls = 0;
	let operation = async {
		sdk::live_graph_select! {
			select = select_ready;
			event = { calls += 1; std::future::ready(7) } => {
				7 => 19,
				_ => 0,
			},
		}
	};
	{
		let mut operation = pin!(operation);
		let mut context = Context::from_waker(Waker::noop());
		assert_eq!(operation.as_mut().poll(&mut context), Poll::Ready(19));
	}
	assert_eq!(calls, 1);
}

/// The facade exports the columnar owner's original concrete batch type.
#[cfg(feature = "columnar")]
#[test]
fn columnar_reexport_keeps_the_owned_batch_type() {
	assert_eq!(
		TypeId::of::<fg_columnar::PropertyBatch>(),
		TypeId::of::<ferros_grynn::values::columnar::PropertyBatch>()
	);
}
