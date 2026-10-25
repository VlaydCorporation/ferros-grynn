//! Format differential and operation admission for borrowed scalar keys.

use std::ops::Bound;

use fg_runtime::__internal::TextInternal as _;

use super::*;
use crate::{
	gql_status::Status,
	test_assertions::{AssertOption as _, AssertResult as _},
	values::{
		Array,
		AtomKind,
		AtomRef,
		GraphId,
		GraphKind,
		GraphRef,
		GraphStorageMode,
		Object,
		Range,
		RecordId,
		RecordIdPart,
		Set,
		Text,
		Ulid,
		Uuid,
		storage::{DiskGraphRef, encode_index_key},
	},
};

/// Independent legacy bytes are compared for complete and composite keys.
fn compare(value: &Value, value_type: &Type) {
	let tail = Value::from(0_i64);
	let mut before = context();
	let mut after = context();
	for components in [vec![(value, value_type)], vec![(value, value_type), (&tail, &Type::Int)]] {
		let expected = encode_index_key(&components, &mut before).assert_ok();
		let (actual, _) =
			StorageValue::encode_index_key_bounded(&components, &mut after, 1 << 20).assert_ok();
		assert_eq!(actual, expected, "{value_type}");
	}
}

/// Class IDs deliberately differ from lexical class order.
fn context() -> EncodeContext {
	let mut context = EncodeContext::default();
	context.register_class_name(7, "z".into()).assert_ok();
	context.register_class_name(9, "a".into()).assert_ok();
	let id = GraphId::new(8).assert_some();
	let graph = GraphRef::new(id, "g", GraphKind::Dag, GraphStorageMode::DiskBacked).assert_ok();
	context.register_graph(graph, DiskGraphRef::new(id)).assert_ok();
	context
}

/// Every scalar family retains its format, sentinels and composite escaping.
#[test]
fn bounded_order_keys_preserve_scalar_and_composite_bytes() {
	let cases = [
		(Value::None, Type::String),
		(Value::Null, Type::Int),
		(Value::Bool(false), Type::Bool),
		(Value::Bool(true), Type::Bool),
		(Value::from(i64::MIN), Type::Int),
		(Value::from(i64::MAX), Type::Int),
		(Value::from(-0.0_f64), Type::Float),
		(Value::from(f64::INFINITY), Type::Float),
		(Value::from(rust_decimal::Decimal::new(-1230, 3)), Type::Decimal),
		(Value::from("a\0é"), Type::String),
		(Value::Bytes(vec![0, 255, 0].into()), Type::Bytes),
		(Value::Uuid(Uuid::default()), Type::Uuid),
		(Value::Ulid(Ulid::default()), Type::Ulid),
		(
			Value::DateTime(
				"1969-12-31T23:59:59.500000000+01:00[Europe/Paris]".parse().assert_ok(),
			),
			Type::DateTime,
		),
		(
			Value::Duration(crate::values::Duration::new(
				jiff::Span::new().hours(-3).nanoseconds(-7),
			)),
			Type::Duration,
		),
		(
			Value::GraphRef(Box::new(
				context()
					.registered_graph(DiskGraphRef::new(GraphId::new(8).assert_some()))
					.assert_some()
					.clone(),
			)),
			Type::GraphRef(GraphKind::Dag),
		),
		(Value::from(7_i64), Type::Option(Box::new(Type::Int))),
	];
	for (value, value_type) in cases {
		compare(&value, &value_type);
	}
}

/// Set order is canonical storage order, including signed integers and nesting.
#[test]
fn bounded_order_keys_preserve_nested_collections_ranges_and_records() {
	compare(&Value::Set(Set::from(vec![-2_i64, 0, 2])), &Type::Set(Box::new(Type::Int)));
	let strings = Value::Array(Array::new(vec![Value::from("\0"), Value::from("a\0b")]));
	compare(&strings, &Type::Array(Box::new(Type::String)));
	let nested = Value::Array(Array::new(vec![strings]));
	compare(&nested, &Type::Array(Box::new(Type::Array(Box::new(Type::String)))));
	for start in [Bound::Unbounded, Bound::Included(0_i64.into()), Bound::Excluded(0_i64.into())] {
		for end in [Bound::Unbounded, Bound::Included(7_i64.into()), Bound::Excluded(7_i64.into())]
		{
			let range = Value::Range(Range::new(start.clone(), end).assert_ok());
			compare(&range, &Type::Range(Box::new(Type::Int)));
		}
	}
	let record = RecordId::new(
		"a",
		RecordIdPart::Object(Object::from_iter([(
			"x\0",
			Value::Array(Array::new(vec![Value::from("\0"), Value::from(-1_i64)])),
		)])),
	)
	.assert_ok();
	compare(&Value::from(record.clone()), &Type::Record);
	compare(&Value::from(AtomRef::new(AtomKind::Edge, record)), &Type::Atom(AtomKind::Edge));
}

/// The input is borrowed; new DTO backing and output must coexist at B.
#[test]
fn bounded_order_key_admission_checks_exact_dto_output_peak() {
	let text = "x".repeat(4096);
	let value = Value::from(text);
	let components = [(&value, &Type::String)];
	let expected = encode_index_key(&components, &mut context()).assert_ok();
	let maximum =
		size_of::<StorageValue>() + Text::estimated_copy_heap_bytes(4096) + expected.len();
	let (actual, retained) =
		StorageValue::encode_index_key_bounded(&components, &mut context(), maximum).assert_ok();
	assert_eq!(actual, expected);
	assert_eq!(retained, 0);
	let error = StorageValue::encode_index_key_bounded(&components, &mut context(), maximum - 1)
		.assert_err();
	assert!(matches!(error.status().status(), Status::StorageDecodeLimitExceeded { .. }));
	let error = StorageValue::encode_index_key_bounded(&components, &mut context(), 1).assert_err();
	assert!(matches!(error.status().status(), Status::StorageDecodeLimitExceeded { .. }));
}

/// Context growth survives the operation; repeated timezone names need no copy.
#[test]
fn bounded_order_key_context_and_invalid_values_keep_existing_contracts() {
	let value = Value::DateTime("2024-01-01T12:00:00+01:00[Europe/Paris]".parse().assert_ok());
	let mut context = context();
	let (bytes, growth) =
		StorageValue::encode_index_key_bounded(&[(&value, &Type::DateTime)], &mut context, 1 << 20)
			.assert_ok();
	assert!(growth > 0);
	assert_eq!(context.time_zone_names().len(), 1);
	let (repeat, growth) = StorageValue::encode_index_key_bounded(
		&[(&value, &Type::DateTime)],
		&mut context,
		size_of::<StorageValue>() + bytes.len(),
	)
	.assert_ok();
	assert_eq!(repeat, bytes);
	assert_eq!(growth, 0);
	for (value, value_type) in [
		(Value::from(f64::NAN), Type::Float),
		(Value::from("x"), Type::Int),
		(Value::None, Type::Object),
	] {
		assert!(
			StorageValue::encode_index_key_bounded(&[(&value, &value_type)], &mut context, 1 << 20)
				.is_err()
		);
	}
	assert!(StorageValue::encode_index_key_bounded(&[], &mut context, 1 << 20).is_err());
}

/// Depth is restored on errors and count overflow never reaches allocation.
#[test]
fn bounded_order_key_writer_checks_depth_count_and_output_consistency() {
	let mut writer = KeyWriter {
		output: None,
		length: 0,
		escapes: 0,
	};
	writer.escaped(|writer| writer.escaped(|writer| writer.bytes(&[0, 1, 0]))).assert_ok();
	assert_eq!(writer.length, 7);
	assert_eq!(writer.escapes, 0);
	assert!(writer.escaped(|_| Err(FerrosGrynnError::invalid_argument("test failure"))).is_err());
	assert_eq!(writer.escapes, 0);
	writer.length = usize::MAX;
	assert!(writer.bytes(&[1]).is_err());
	let mut writer = KeyWriter {
		output: Some(Vec::new()),
		length: 0,
		escapes: 0,
	};
	assert!(writer.bytes(&[0]).is_err());
	assert!(writer.output.assert_some().is_empty());
}
