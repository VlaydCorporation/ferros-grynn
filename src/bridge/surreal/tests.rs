//! Native runtime/link checks complement the independent SDK compile spike.

use super::*;
use crate::values::{Number, RecordId};

fn context() -> BridgeContext {
	let mut options = QueryExecutionOptions::default();
	options.bridge_memory = Some(Arc::new(parking_lot::Mutex::new(
		crate::gql::BarrierAccount::new(options.barrier_memory_bytes.get()),
	)));
	BridgeContext {
		options,
		cancellation: Default::default(),
		parent_cancellation: None,
		deadline: None,
	}
}

#[test]
fn bridge_surreal_typed_values_roundtrip() -> FerrosGrynnResult<()> {
	let context = context();
	let values = [
		Value::None,
		Value::Null,
		Value::from(9_007_199_254_740_993_i64),
		Value::Number(Number::Decimal(rust_decimal::Decimal::MAX)),
		Value::Array(vec![Value::None, Value::Null].into()),
		Value::Object(crate::values::Object::from_iter([
			("scalar", Value::from(7_i64)),
			("array", Value::Array(vec![Value::Null].into())),
		])),
		Value::String("typed text".into()),
		Value::Bytes(vec![0_u8, 255].into()),
		Value::Uuid(uuid::Uuid::now_v7().into()),
		Value::Set(vec![Value::from(1_i64), Value::from(2_i64)].into_iter().collect()),
		Value::Range(crate::values::Range::new(
			std::ops::Bound::Included(Value::from(1_i64)),
			std::ops::Bound::Excluded(Value::from(9_i64)),
		)?),
		Value::Geometry(crate::values::Geometry::Point(crate::values::Position::new(vec![
			7.0, 8.0,
		])?)),
		Value::Record(Box::new(RecordId::from_value(
			"native",
			Value::Array(vec![Value::from(7_i64), Value::Uuid(uuid::Uuid::now_v7().into())].into()),
		)?)),
		Value::DateTime(crate::values::DateTime::new(
			jiff::Timestamp::from_nanosecond(-1)
				.map_err(|error| {
					FerrosGrynnError::bridge_operation_error("test timestamp failed")
						.with_technical_source(error)
				})?
				.to_zoned(jiff::tz::TimeZone::UTC),
		)),
	];
	for value in values {
		assert_eq!(
			conversion::incoming(conversion::outgoing(value.clone(), &context, 0)?, &context, 0)?,
			value
		);
	}
	assert!(conversion::outgoing(Value::Ulid(Default::default()), &context, 0).is_err());
	assert!(
		conversion::outgoing(
			Value::Geometry(crate::values::Geometry::Polygon(Vec::new())),
			&context,
			0
		)
		.is_err()
	);
	assert!(
		conversion::outgoing(
			Value::Duration(crate::values::Duration::new(jiff::Span::new().days(1))),
			&context,
			0
		)
		.is_err()
	);
	let nan_set = sdk::Set::from(vec![
		sdk::Value::Number(sdk::Number::Float(f64::from_bits(0x7ff8_0000_0000_0001))),
		sdk::Value::Number(sdk::Number::Float(f64::from_bits(0x7ff8_0000_0000_0002))),
	]);
	assert_eq!(nan_set.len(), 2);
	assert!(conversion::incoming(sdk::Value::Set(nan_set), &context, 0).is_err());
	Ok(())
}
