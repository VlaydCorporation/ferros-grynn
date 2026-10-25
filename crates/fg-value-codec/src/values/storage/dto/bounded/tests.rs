use super::*;
use crate::{
	test_assertions::{AssertOption as _, AssertResult as _},
	values::{
		Array,
		AtomKind,
		AtomRef,
		Geometry,
		GraphId,
		GraphKind,
		GraphRef,
		GraphStorageMode,
		Object,
		Position,
		Range,
		RecordIdPart,
		Set,
		Ulid,
		Uuid,
	},
};

/// Runs the private converter with an explicit policy for boundary oracles.
fn encode(
	value: &Value,
	context: &mut EncodeContext,
	maximum: usize,
	limits: DecodeLimits,
) -> FerrosGrynnResult<(Vec<u8>, usize)> {
	let mut converter = Converter::new(context, maximum, limits);
	let value = converter.value(value, 0)?;
	converter.finish(value)
}

/// Compares every supported family with the unchanged owned conversion/control.
#[test]
fn borrowed_conversion_preserves_canonical_bytes_and_reader_results() {
	let record = RecordId::new(
		"item",
		RecordIdPart::Object(Object::from_iter([(
			"array",
			Value::Array(Array::new(vec![Value::from("key"), Value::from(7_i64)])),
		)])),
	)
	.assert_ok();
	let point = Geometry::Point(Position::new([1.0, 2.0]).assert_ok());
	let mut values = vec![
		Value::None,
		Value::Null,
		Value::Bool(true),
		Value::from(-7_i64),
		Value::from(f64::NAN),
		Value::from(rust_decimal::Decimal::new(123, 2)),
		Value::from("x".repeat(25)),
		Value::Bytes(vec![1, 2, 3].into()),
		Value::DateTime("1969-12-31T23:59:59.500000000+01:00[Europe/Paris]".parse().assert_ok()),
		Value::Duration(crate::values::Duration::new(jiff::Span::new().seconds(7))),
		Value::Uuid(Uuid::default()),
		Value::Ulid(Ulid::default()),
		Value::from(record.clone()),
		Value::from(AtomRef::new(AtomKind::Edge, record)),
		Value::Range(
			Range::new(Bound::Included(1_i64.into()), Bound::Excluded(4_i64.into())).assert_ok(),
		),
		Value::Geometry(Geometry::Collection(vec![point])),
	];
	for vector in [
		Vector::F64(vec![1.0, -0.0]),
		Vector::F32(vec![1.0, -0.0]),
		Vector::F16(vec![half::f16::ONE]),
		Vector::I64(vec![-1, 2]),
		Vector::I32(vec![-1, 2]),
		Vector::I16(vec![-1, 2]),
		Vector::I8(vec![-1, 2]),
		Vector::U8(vec![1, 2]),
	] {
		values.push(Value::Vector(vector));
	}
	values.push(Value::Set(Set::from(vec![3_i64, 1, 2])));
	values.push(Value::Object(Object::from_iter([(
		"nested",
		Value::Array(Array::new(values.clone())),
	)])));
	let mut context = EncodeContext::default();
	context.register_class_name(7, "item".into()).assert_ok();
	let graph_id = GraphId::new(9).assert_some();
	let graph =
		GraphRef::new(graph_id, "graph", GraphKind::Dag, GraphStorageMode::DiskBacked).assert_ok();
	context
		.register_graph(graph.clone(), crate::values::storage::DiskGraphRef::new(graph_id))
		.assert_ok();
	values.push(Value::from(graph));
	let a = Position::new([0.0, 0.0]).assert_ok();
	let b = Position::new([1.0, 0.0]).assert_ok();
	let c = Position::new([0.0, 1.0]).assert_ok();
	let ring = vec![a.clone(), b.clone(), c, a.clone()];
	values.extend([
		Value::Geometry(Geometry::MultiPoint(vec![a.clone(), b.clone()])),
		Value::Geometry(Geometry::LineString(vec![a.clone(), b.clone()])),
		Value::Geometry(Geometry::MultiLineString(vec![vec![a, b]])),
		Value::Geometry(Geometry::Polygon(vec![ring.clone()])),
		Value::Geometry(Geometry::MultiPolygon(vec![vec![ring]])),
	]);
	for value in values {
		let control = StorageValue::from_runtime(value.clone(), &mut context).assert_ok();
		let expected = CanonicalCodec::encode(control.as_ref()).assert_ok();
		let (actual, retained) =
			encode(&value, &mut context, 16 << 20, DecodeLimits::default()).assert_ok();
		assert_eq!(actual, expected);
		assert_eq!(retained, 0);
		assert_eq!(
			StorageValue::prime_context_bounded(&value, &mut context, 16 << 20).assert_ok(),
			0
		);
		assert_eq!(CanonicalCodec::decode(&actual, DecodeLimits::default()).assert_ok(), control);
	}
}

/// Output fitting alone is insufficient while its Text DTO remains alive.
#[test]
fn string_copy_and_output_share_the_exact_operation_peak() {
	let value = Value::from("x".repeat(31));
	let peak = Text::estimated_copy_heap_bytes(31) + 33;
	let mut context = EncodeContext::default();
	assert!(encode(&value, &mut context, peak - 1, DecodeLimits::default()).is_err());
	let (bytes, retained) = encode(&value, &mut context, peak, DecodeLimits::default()).assert_ok();
	assert_eq!(bytes.len(), 33);
	assert_eq!(retained, 0);
}

/// Entered empty Record/Atom/object frames must agree with the actual decoder.
#[test]
fn borrowed_helper_depth_matches_reader_empty_and_nonempty_frames() {
	let mut context = EncodeContext::default();
	context.register_class_name(1, "item".into()).assert_ok();
	for part in [
		RecordIdPart::Int(1),
		RecordIdPart::Array(Array::new(Vec::new())),
		RecordIdPart::Object(Object::new()),
		RecordIdPart::Array(Array::new(vec![1_i64.into()])),
	] {
		let record = RecordId::new("item", part).assert_ok();
		for value in
			[Value::from(record.clone()), Value::from(AtomRef::new(AtomKind::Node, record))]
		{
			let control = StorageValue::from_runtime(value.clone(), &mut context).assert_ok();
			let bytes = CanonicalCodec::encode(control.as_ref()).assert_ok();
			for maximum in 0..=4 {
				let limits = DecodeLimits {
					max_depth: maximum,
					..DecodeLimits::default()
				};
				assert_eq!(
					encode(&value, &mut context, 1 << 20, limits).is_ok(),
					CanonicalCodec::decode(&bytes, limits).is_ok()
				);
			}
		}
	}
}

/// A prepared dictionary contradiction must not silently assign a different ID.
#[test]
fn borrowed_object_requires_prepared_keys() {
	let value = Value::Object(Object::from_iter([("missing", Value::from(1_i64))]));
	let mut context = EncodeContext::default();
	assert!(encode(&value, &mut context, 1 << 20, DecodeLimits::default()).is_err());
	assert!(StorageValue::prime_context_bounded(&value, &mut context, 1 << 20).is_err());
	assert!(context.property_dictionary().is_empty());
}

/// Reader charges do not require output backing, but depth limits still apply.
#[test]
fn context_priming_keeps_zero_copy_and_reader_limits() {
	let fixed = crate::values::DateTime::new(
		jiff::Timestamp::new(1_700_000_000, 123).assert_ok().to_zoned(jiff::tz::TimeZone::fixed(
			jiff::tz::Offset::from_seconds(19_800).assert_ok(),
		)),
	);
	for value in [
		Value::None,
		Value::Null,
		Value::Bool(true),
		Value::from(42_i64),
		Value::from("inline"),
		Value::Array(Array::new(Vec::new())),
		Value::Set(Set::new()),
		Value::Object(Object::new()),
		Value::DateTime("1969-12-31T23:59:59.500000000+00:00[UTC]".parse().assert_ok()),
		Value::DateTime(fixed),
	] {
		let mut context = EncodeContext::default();
		assert_eq!(StorageValue::prime_context_bounded(&value, &mut context, 0).assert_ok(), 0);
		assert_eq!(context.time_zone_names().len(), 0);
	}
	let mut nested = Value::None;
	for _ in 0..DecodeLimits::default().max_depth {
		nested = Value::Array(Array::new(vec![nested]));
	}
	let mut context = EncodeContext::default();
	StorageValue::prime_context_bounded(&nested, &mut context, 1 << 20).assert_ok();
	let deeper = Value::Array(Array::new(vec![nested]));
	assert!(StorageValue::prime_context_bounded(&deeper, &mut context, 1 << 20).is_err());
}

/// Only new IANA ownership survives; a repeated identifier needs no copy
/// budget.
#[test]
fn context_priming_retains_new_iana_and_reuses_published_names() {
	let value = Value::DateTime("2024-01-01T12:00:00+01:00[Europe/Paris]".parse().assert_ok());
	let mut context = EncodeContext::default();
	let retained = StorageValue::prime_context_bounded(&value, &mut context, 1 << 20).assert_ok();
	assert!(retained > 0);
	assert_eq!(context.time_zone_names().len(), 1);
	let first = StorageValue::from_runtime(value.clone(), &mut context).assert_ok();
	assert_eq!(StorageValue::prime_context_bounded(&value, &mut context, 0).assert_ok(), 0);
	assert_eq!(context.time_zone_names().len(), 1);
	assert_eq!(context.time_zone_names().next().assert_some().as_str(), "Europe/Paris");
	assert_eq!(StorageValue::from_runtime(value.clone(), &mut context).assert_ok(), first);
	let mut rejected = EncodeContext::default();
	assert!(StorageValue::prime_context_bounded(&value, &mut rejected, 0).is_err());
	assert_eq!(rejected.time_zone_names().len(), 0);
}

/// Set keys coexist with the source DTO backing and are dropped before output.
#[test]
fn set_keys_have_an_independent_peak_and_keep_duplicate_validation() {
	let value = Value::Set(Set::from(vec!["a".repeat(31), "b".repeat(31)]));
	let retained = 2 * size_of::<StorageValue>() + 2 * Text::estimated_copy_heap_bytes(31);
	let scratch = 2 * size_of::<(Vec<u8>, StorageValue)>() + 66;
	let peak = retained + scratch;
	let mut context = EncodeContext::default();
	assert!(encode(&value, &mut context, peak - 1, DecodeLimits::default()).is_err());
	assert!(encode(&value, &mut context, peak, DecodeLimits::default()).is_ok());
	let converter = Converter::new(&mut context, 1 << 20, DecodeLimits::default());
	let mut duplicate = vec![StorageValue::Int(1), StorageValue::Int(1)];
	assert!(converter.canonicalize_set(&mut duplicate).is_err());
	assert!(duplicate.is_empty());
	assert_eq!(converter.budget.context_bytes(), 0);
}

/// Clone backing is admitted before copying; scratch is admitted before
/// validation.
#[test]
fn geometry_copy_validation_and_reader_limits_are_admitted() {
	let geometry = Geometry::Point(Position::new([1.0, 2.0]).assert_ok());
	let peak = 2 * size_of::<f64>() + 17 + geometry.validation_scratch_bytes().assert_ok();
	let value = Value::Geometry(geometry);
	let mut context = EncodeContext::default();
	assert!(encode(&value, &mut context, peak - 1, DecodeLimits::default()).is_err());
	assert!(encode(&value, &mut context, peak, DecodeLimits::default()).is_ok());
	let nested = Value::Geometry(Geometry::Collection(vec![Geometry::Collection(Vec::new())]));
	let limits = DecodeLimits {
		max_depth: 0,
		..DecodeLimits::default()
	};
	assert!(encode(&nested, &mut context, 1 << 20, limits).is_err());
}

/// Borrowed spare capacity is not retained by the newly cloned DTO.
#[test]
fn borrowed_geometry_ignores_source_spare_capacity() {
	let capacity = (128 * 1024 * 1024) / size_of::<Geometry>() + 1;
	let value = Value::Geometry(Geometry::Collection(Vec::with_capacity(capacity)));
	let mut context = EncodeContext::default();
	let control = StorageValue::from_runtime(value.clone(), &mut context).assert_ok();
	let expected = CanonicalCodec::encode(control.as_ref()).assert_ok();
	let (actual, retained) =
		encode(&value, &mut context, 2048, DecodeLimits::default()).assert_ok();
	assert_eq!(actual, expected);
	assert_eq!(retained, 0);
}

/// Success carries only new context reservations into the following record.
#[test]
fn time_zone_growth_is_retained_but_repeated_names_need_no_copy_budget() {
	let value = Value::DateTime("2024-01-01T12:00:00+01:00[Europe/Paris]".parse().assert_ok());
	let mut context = EncodeContext::default();
	let (bytes, retained) =
		encode(&value, &mut context, 1 << 20, DecodeLimits::default()).assert_ok();
	assert!(retained > 0);
	assert_eq!(context.time_zone_names().len(), 1);
	let (repeat, retained) =
		encode(&value, &mut context, bytes.len(), DecodeLimits::default()).assert_ok();
	assert_eq!(repeat, bytes);
	assert_eq!(retained, 0);
}
