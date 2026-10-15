//! Order-preserving keys for format-v1 scalar B+tree indexes.

use super::{EncodeContext, StorageBound, StorageRecordIdPart, StorageRecordValue, StorageValue};
use crate::{
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	values::runtime::{Type, Value},
};

/// Maximum decimal coefficient width supported by the i128 storage format.
const DECIMAL_DIGITS: usize = 39;
/// Ordered byte-string escape marker.
const ESCAPE: u8 = 0;
/// Escaped zero byte.
const ESCAPED_ZERO: u8 = 0xff;
/// End of a sequence or non-terminal composite component.
const END: u8 = 0;
/// Boundary between adjacent sequence elements.
const NEXT: u8 = 1;

/// Rejects schema types without a total, format-v1 scalar B+tree order.
pub fn validate_index_type(value_type: &Type) -> FerrosGrynnResult<()> {
	match value_type {
		Type::None
		| Type::Null
		| Type::Bool
		| Type::Int
		| Type::Float
		| Type::Decimal
		| Type::String
		| Type::Duration
		| Type::DateTime
		| Type::Ulid
		| Type::Uuid
		| Type::Bytes
		| Type::Record
		| Type::GraphRef(_)
		| Type::Atom(_) => Ok(()),
		Type::Array(element)
		| Type::Set(element)
		| Type::Range(element)
		| Type::Option(element) => validate_index_type(element),
		Type::Any
		| Type::Number
		| Type::Object
		| Type::Geometry
		| Type::Vector(_)
		| Type::Regex
		| Type::Matcher
		| Type::Path
		| Type::Graph(_)
		| Type::Union(_)
		| Type::RecordShape(_)
		| Type::AtomOccurrence(_)
		| Type::Error
		| Type::Cursor
		| Type::Closure {
			..
		} => Err(FerrosGrynnError::invalid_argument(format!(
			"type '{value_type}' has no scalar B+tree ordering"
		))),
	}
}

/// Encodes one complete composite key without decoding during B+tree search.
pub fn encode_index_key(
	components: &[(&Value, &Type)],
	context: &mut EncodeContext,
) -> FerrosGrynnResult<Vec<u8>> {
	if components.is_empty() {
		return Err(FerrosGrynnError::invalid_argument(
			"a scalar index requires at least one field",
		));
	}
	let mut output = Vec::new();
	for (index, (value, value_type)) in components.iter().enumerate() {
		validate_index_type(value_type)?;
		let stored = StorageValue::from_runtime((*value).clone(), context)?;
		let raw = encode_typed(&stored, value_type, context)?;
		if index + 1 == components.len() {
			output.extend_from_slice(&raw);
		} else {
			append_escaped(&mut output, &raw);
			output.extend_from_slice(&[ESCAPE, END]);
		}
	}
	if output.is_empty() {
		return Err(FerrosGrynnError::invalid_argument("a scalar index key must not be empty"));
	}
	Ok(output)
}

/// Encodes complete leading composite components for a prefix/range search.
/// Every component receives the same escaping and separator it has before a
/// following field in a complete key.
pub fn encode_index_prefix(
	components: &[(&Value, &Type)],
	context: &mut EncodeContext,
) -> FerrosGrynnResult<Vec<u8>> {
	if components.is_empty() {
		return Ok(Vec::new());
	}
	let mut output = Vec::new();
	for (value, value_type) in components {
		validate_index_type(value_type)?;
		let stored = StorageValue::from_runtime((*value).clone(), context)?;
		let raw = encode_typed(&stored, value_type, context)?;
		append_escaped(&mut output, &raw);
		output.extend_from_slice(&[ESCAPE, END]);
	}
	Ok(output)
}

/// Encodes typed for this module.
fn encode_typed(
	value: &StorageValue,
	value_type: &Type,
	context: &mut EncodeContext,
) -> FerrosGrynnResult<Vec<u8>> {
	if matches!(value, StorageValue::None) {
		return Ok(vec![0]);
	}
	if matches!(value, StorageValue::Null) {
		return Ok(vec![1]);
	}
	if let Type::Option(inner) = value_type {
		return encode_typed(value, inner, context);
	}
	let mut encoded = vec![2];
	encoded.extend(match (value, value_type) {
		(StorageValue::Bool(false), Type::Bool) => Ok(vec![0]),
		(StorageValue::Bool(true), Type::Bool) => Ok(vec![1]),
		(StorageValue::Int(value), Type::Int) => Ok(ordered_i64(*value).to_vec()),
		(StorageValue::Float(bits), Type::Float) => encode_float(*bits),
		(StorageValue::Decimal(value), Type::Decimal) => {
			encode_decimal(value.mantissa, value.scale)
		}
		(StorageValue::String(value), Type::String) => {
			let mut encoded = value.as_bytes().to_vec();
			encoded.push(0);
			Ok(encoded)
		}
		(StorageValue::Bytes(value), Type::Bytes) => {
			let mut encoded = value.as_slice().to_vec();
			encoded.push(0);
			Ok(encoded)
		}
		(StorageValue::DateTime(value), Type::DateTime) => {
			if value.nanos >= 1_000_000_000 {
				return Err(index_value_error("datetime nanoseconds exceed one second"));
			}
			let mut encoded = ordered_i64(value.seconds).to_vec();
			encoded.extend_from_slice(&value.nanos.to_be_bytes());
			Ok(encoded)
		}
		(StorageValue::Duration(value), Type::Duration) => encode_duration(value),
		(StorageValue::Uuid(value), Type::Uuid) => Ok(value.as_inner().as_bytes().to_vec()),
		(StorageValue::Ulid(value), Type::Ulid) => Ok(value.as_inner().to_bytes().to_vec()),
		(StorageValue::Record(value), Type::Record) => encode_record(value, context),
		(StorageValue::GraphRef(value), Type::GraphRef(expected)) => {
			let runtime = context.registered_graph(*value).ok_or_else(|| {
				index_value_error("indexed graph reference is absent from its conversion context")
			})?;
			if runtime.kind() != *expected {
				return Err(index_value_error(format!(
					"graph reference does not match indexed field type '{value_type}'"
				)));
			}
			Ok(value.raw().to_be_bytes().to_vec())
		}
		(StorageValue::Array(values), Type::Array(element)) => {
			encode_sequence(values, element, context)
		}
		(StorageValue::Set(values), Type::Set(element)) => {
			encode_sequence(values, element, context)
		}
		(StorageValue::Range(value), Type::Range(element)) => encode_range(value, element, context),
		(StorageValue::AtomRef(value), Type::Atom(expected)) if value.kind() == *expected => {
			encode_record(&value.record_id, context)
		}
		_ => Err(index_value_error(format!(
			"value does not match indexed field type '{value_type}'"
		))),
	}?);
	Ok(encoded)
}

/// Encodes float for this module.
fn encode_float(bits: u64) -> FerrosGrynnResult<Vec<u8>> {
	let value = f64::from_bits(bits);
	if value.is_nan() {
		return Err(index_value_error("NaN cannot be inserted into a scalar B+tree index"));
	}
	let bits = if value == 0.0 {
		0
	} else {
		bits
	};
	let ordered = if bits >> 63 == 0 {
		bits ^ (1_u64 << 63)
	} else {
		!bits
	};
	Ok(ordered.to_be_bytes().to_vec())
}

/// Encodes decimal for this module.
fn encode_decimal(mut mantissa: i128, mut scale: u8) -> FerrosGrynnResult<Vec<u8>> {
	while scale > 0 && mantissa % 10 == 0 {
		mantissa /= 10;
		scale -= 1;
	}
	if mantissa == 0 {
		return Ok(vec![1; 1]
			.into_iter()
			.chain(std::iter::repeat_n(0, 2 + DECIMAL_DIGITS))
			.collect());
	}
	let negative = mantissa < 0;
	let digits = mantissa.unsigned_abs().to_string();
	if digits.len() > DECIMAL_DIGITS {
		return Err(index_value_error("decimal coefficient exceeds 39 digits"));
	}
	let digit_count = i16::try_from(digits.len())
		.map_err(|_error| index_value_error("decimal digit count exceeds i16"))?;
	let adjusted = digit_count
		.checked_sub(i16::from(scale))
		.and_then(|value| value.checked_sub(1))
		.ok_or_else(|| index_value_error("decimal adjusted exponent overflows i16"))?;
	let mut exponent = adjusted.to_be_bytes();
	exponent[0] ^= 0x80;
	let mut encoded = Vec::with_capacity(1 + 2 + DECIMAL_DIGITS);
	encoded.push(if negative {
		0
	} else {
		2
	});
	encoded.extend_from_slice(&exponent);
	encoded.extend(digits.bytes().map(|digit| digit - b'0' + 1));
	encoded.resize(1 + 2 + DECIMAL_DIGITS, 0);
	if negative {
		for byte in &mut encoded[1..] {
			*byte = !*byte;
		}
	}
	Ok(encoded)
}

/// Encodes duration for this module.
fn encode_duration(value: &super::StorageDuration) -> FerrosGrynnResult<Vec<u8>> {
	let sign = if value.negative {
		-1_i128
	} else {
		1_i128
	};
	let unit = |index: usize| i128::from(value.units[index]) * sign;
	let months = unit(0)
		.checked_mul(12)
		.and_then(|value| value.checked_add(unit(1)))
		.ok_or_else(|| index_value_error("duration months overflow"))?;
	let days = unit(2)
		.checked_mul(7)
		.and_then(|value| value.checked_add(unit(3)))
		.ok_or_else(|| index_value_error("duration days overflow"))?;
	let nanos = unit(4)
		.checked_mul(3_600_000_000_000)
		.and_then(|value| value.checked_add(unit(5) * 60_000_000_000))
		.and_then(|value| value.checked_add(unit(6) * 1_000_000_000))
		.and_then(|value| value.checked_add(unit(7) * 1_000_000))
		.and_then(|value| value.checked_add(unit(8) * 1_000))
		.and_then(|value| value.checked_add(unit(9)))
		.ok_or_else(|| index_value_error("duration nanoseconds overflow"))?;
	let months = i32::try_from(months)
		.map_err(|_error| index_value_error("duration months exceed the index format"))?;
	let days = i32::try_from(days)
		.map_err(|_error| index_value_error("duration days exceed the index format"))?;
	let nanos = i64::try_from(nanos)
		.map_err(|_error| index_value_error("duration nanoseconds exceed the index format"))?;
	let mut encoded = Vec::with_capacity(16);
	encoded.extend_from_slice(&ordered_i32(months));
	encoded.extend_from_slice(&ordered_i32(days));
	encoded.extend_from_slice(&ordered_i64(nanos));
	Ok(encoded)
}

/// Encodes record for this module.
fn encode_record(
	value: &super::StorageRecordId,
	context: &EncodeContext,
) -> FerrosGrynnResult<Vec<u8>> {
	let class = context
		.class_name(value.class_id)
		.ok_or_else(|| index_value_error("indexed RecordId class is absent from its dictionary"))?;
	let mut encoded = Vec::new();
	append_escaped(&mut encoded, class.as_bytes());
	encoded.extend_from_slice(&[ESCAPE, END]);
	encoded.extend_from_slice(&encode_record_part(&value.part)?);
	Ok(encoded)
}

/// Encodes record part for this module.
fn encode_record_part(value: &StorageRecordIdPart) -> FerrosGrynnResult<Vec<u8>> {
	let mut encoded = Vec::new();
	match value {
		StorageRecordIdPart::Int(value) => {
			encoded.push(0);
			encoded.extend_from_slice(&ordered_i64(*value));
		}
		StorageRecordIdPart::String(value) => {
			encoded.push(1);
			append_escaped(&mut encoded, value.as_bytes());
			encoded.extend_from_slice(&[ESCAPE, END]);
		}
		StorageRecordIdPart::Uuid(value) => {
			encoded.push(2);
			encoded.extend_from_slice(value.as_inner().as_bytes());
		}
		StorageRecordIdPart::Ulid(value) => {
			encoded.push(3);
			encoded.extend_from_slice(&value.as_inner().to_bytes());
		}
		StorageRecordIdPart::Array(values) => {
			encoded.push(4);
			encode_record_sequence(&mut encoded, values)?;
		}
		StorageRecordIdPart::Object(values) => {
			encoded.push(5);
			for (key, value) in values {
				append_escaped(&mut encoded, key.as_bytes());
				encoded.extend_from_slice(&[ESCAPE, NEXT]);
				let value = encode_record_value(value)?;
				append_escaped(&mut encoded, &value);
				encoded.extend_from_slice(&[ESCAPE, NEXT]);
			}
			encoded.extend_from_slice(&[ESCAPE, END]);
		}
	}
	Ok(encoded)
}

/// Encodes record sequence for this module.
fn encode_record_sequence(
	output: &mut Vec<u8>,
	values: &[StorageRecordValue],
) -> FerrosGrynnResult<()> {
	for value in values {
		let value = encode_record_value(value)?;
		append_escaped(output, &value);
		output.extend_from_slice(&[ESCAPE, NEXT]);
	}
	output.extend_from_slice(&[ESCAPE, END]);
	Ok(())
}

/// Encodes record value for this module.
fn encode_record_value(value: &StorageRecordValue) -> FerrosGrynnResult<Vec<u8>> {
	let part = match value {
		StorageRecordValue::Int(value) => StorageRecordIdPart::Int(*value),
		StorageRecordValue::String(value) => StorageRecordIdPart::String(value.clone()),
		StorageRecordValue::Uuid(value) => StorageRecordIdPart::Uuid(*value),
		StorageRecordValue::Ulid(value) => StorageRecordIdPart::Ulid(*value),
		StorageRecordValue::Array(values) => StorageRecordIdPart::Array(values.clone()),
		StorageRecordValue::Object(values) => StorageRecordIdPart::Object(values.clone()),
	};
	encode_record_part(&part)
}

/// Encodes sequence for this module.
fn encode_sequence(
	values: &[StorageValue],
	element: &Type,
	context: &mut EncodeContext,
) -> FerrosGrynnResult<Vec<u8>> {
	let mut encoded = Vec::new();
	for value in values {
		let value = encode_typed(value, element, context)?;
		append_escaped(&mut encoded, &value);
		encoded.extend_from_slice(&[ESCAPE, NEXT]);
	}
	encoded.extend_from_slice(&[ESCAPE, END]);
	Ok(encoded)
}

/// Encodes range for this module.
fn encode_range(
	value: &super::StorageRange,
	element: &Type,
	context: &mut EncodeContext,
) -> FerrosGrynnResult<Vec<u8>> {
	let mut encoded = Vec::new();
	match &value.start {
		StorageBound::Unbounded => encoded.push(0),
		StorageBound::Included(bound) => {
			encoded.push(1);
			append_escaped(&mut encoded, &encode_typed(bound, element, context)?);
			encoded.extend_from_slice(&[ESCAPE, 0]);
		}
		StorageBound::Excluded(bound) => {
			encoded.push(1);
			append_escaped(&mut encoded, &encode_typed(bound, element, context)?);
			encoded.extend_from_slice(&[ESCAPE, 1]);
		}
	}
	match &value.end {
		StorageBound::Unbounded => encoded.push(1),
		StorageBound::Included(bound) => {
			encoded.push(0);
			append_escaped(&mut encoded, &encode_typed(bound, element, context)?);
			encoded.extend_from_slice(&[ESCAPE, 0]);
		}
		StorageBound::Excluded(bound) => {
			encoded.push(0);
			append_escaped(&mut encoded, &encode_typed(bound, element, context)?);
			encoded.extend_from_slice(&[ESCAPE, 1]);
		}
	}
	Ok(encoded)
}

/// Appends escaped for this module.
fn append_escaped(output: &mut Vec<u8>, bytes: &[u8]) {
	for byte in bytes {
		if *byte == ESCAPE {
			output.extend_from_slice(&[ESCAPE, ESCAPED_ZERO]);
		} else {
			output.push(*byte);
		}
	}
}

/// Returns ordered i32 for this module.
fn ordered_i32(value: i32) -> [u8; 4] {
	let mut bytes = value.to_be_bytes();
	bytes[0] ^= 0x80;
	bytes
}

/// Returns ordered i64 for this module.
fn ordered_i64(value: i64) -> [u8; 8] {
	let mut bytes = value.to_be_bytes();
	bytes[0] ^= 0x80;
	bytes
}

/// Returns index value error for this module.
fn index_value_error(message: impl Into<String>) -> FerrosGrynnError {
	FerrosGrynnError::invalid_argument(message)
}

#[cfg(test)]
/// Regression tests for order key behavior.
mod tests {
	use std::ops::Bound;

	use rust_decimal::Decimal;

	use super::*;
	use crate::values::runtime::{
		PropertyDictionary,
		Range,
		RecordId,
		graph::{AtomKind, AtomRef},
	};

	/// Returns encode for this module.
	fn encode(value: &Value, value_type: &Type) -> FerrosGrynnResult<Vec<u8>> {
		encode_index_key(&[(value, value_type)], &mut EncodeContext::new(PropertyDictionary::new()))
	}

	/// Verifies integer float and decimal keys preserve order.
	#[test]
	fn integer_float_and_decimal_keys_preserve_order() {
		use crate::test_assertions::AssertResult as _;

		for pair in [(-10_i64, -1_i64), (-1, 0), (0, 1), (1, i64::MAX)] {
			assert!(
				encode(&Value::from(pair.0), &Type::Int).assert_ok()
					< encode(&Value::from(pair.1), &Type::Int).assert_ok()
			);
		}
		for pair in [(-f64::INFINITY, -1.5), (-0.5, 0.0), (0.0, 3.0), (3.0, f64::INFINITY)] {
			assert!(
				encode(&Value::from(pair.0), &Type::Float).assert_ok()
					< encode(&Value::from(pair.1), &Type::Float).assert_ok()
			);
		}
		let decimal = |value: &str| {
			Value::from(value.parse::<Decimal>().assert_ok_with("valid test decimal"))
		};
		for pair in [("-10", "-1"), ("-0.5", "0"), ("0.5", "1"), ("1", "10")] {
			assert!(
				encode(&decimal(pair.0), &Type::Decimal).assert_ok()
					< encode(&decimal(pair.1), &Type::Decimal).assert_ok()
			);
		}
		assert_eq!(
			encode(&decimal("1.0"), &Type::Decimal).assert_ok(),
			encode(&decimal("1.00"), &Type::Decimal).assert_ok()
		);
		assert_eq!(
			encode(&Value::from(-0.0_f64), &Type::Float).assert_ok(),
			encode(&Value::from(0.0_f64), &Type::Float).assert_ok(),
		);
	}

	/// Verifies composite strings escape zero without losing lexicographic
	/// order.
	#[test]
	fn composite_strings_escape_zero_without_losing_lexicographic_order() {
		use crate::test_assertions::AssertResult as _;

		let left = Value::from("ab");
		let middle = Value::from("ab\0");
		let right = Value::from("ac");
		let tail = Value::from(7_i64);
		let mut context = EncodeContext::new(PropertyDictionary::new());
		let left = encode_index_key(&[(&left, &Type::String), (&tail, &Type::Int)], &mut context)
			.assert_ok();
		let middle =
			encode_index_key(&[(&middle, &Type::String), (&tail, &Type::Int)], &mut context)
				.assert_ok();
		let right = encode_index_key(&[(&right, &Type::String), (&tail, &Type::Int)], &mut context)
			.assert_ok();
		assert!(left < middle && middle < right);
	}

	/// Verifies none null and present values preserve runtime order.
	#[test]
	fn none_null_and_present_values_preserve_runtime_order() {
		use crate::test_assertions::AssertResult as _;

		let none = encode(&Value::None, &Type::Int).assert_ok();
		let null = encode(&Value::Null, &Type::Int).assert_ok();
		let present = encode(&Value::from(i64::MIN), &Type::Int).assert_ok();
		assert!(none < null && null < present);
	}

	/// Verifies record and atom keys ignore dictionary and physical allocation
	/// order.
	#[test]
	fn record_and_atom_keys_ignore_dictionary_and_physical_allocation_order() {
		use crate::test_assertions::AssertResult as _;

		let late: RecordId = "z:1".parse().assert_ok();
		let early: RecordId = "a:1".parse().assert_ok();
		let late_value = Value::from(late.clone());
		let early_value = Value::from(early.clone());
		let mut context = EncodeContext::new(PropertyDictionary::new());
		context.register_class_name(7, "z".into()).assert_ok();
		context.register_class_name(9, "a".into()).assert_ok();
		let late_key = encode_index_key(&[(&late_value, &Type::Record)], &mut context).assert_ok();
		let early_key =
			encode_index_key(&[(&early_value, &Type::Record)], &mut context).assert_ok();
		assert!(early_key < late_key);

		let late_atom = AtomRef::new(AtomKind::Node, late);
		let early_atom = AtomRef::new(AtomKind::Node, early);
		let late_value = Value::from(late_atom);
		let early_value = Value::from(early_atom);
		let value_type = Type::Atom(AtomKind::Node);
		let late_key = encode_index_key(&[(&late_value, &value_type)], &mut context).assert_ok();
		let early_key = encode_index_key(&[(&early_value, &value_type)], &mut context).assert_ok();
		assert!(early_key < late_key);
	}

	/// Verifies range keys compare bounds after their values.
	#[test]
	fn range_keys_compare_bounds_after_their_values() {
		use crate::test_assertions::AssertResult as _;

		let ranges = [
			(
				Range::new(Bound::Included(0_i64.into()), Bound::Included(200_i64.into()))
					.assert_ok(),
				Range::new(Bound::Excluded(100_i64.into()), Bound::Included(200_i64.into()))
					.assert_ok(),
			),
			(
				Range::new(Bound::Included(0_i64.into()), Bound::Included(10_i64.into()))
					.assert_ok(),
				Range::new(Bound::Excluded(0_i64.into()), Bound::Included(10_i64.into()))
					.assert_ok(),
			),
			(
				Range::new(Bound::Included(0_i64.into()), Bound::Included(10_i64.into()))
					.assert_ok(),
				Range::new(Bound::Included(0_i64.into()), Bound::Excluded(10_i64.into()))
					.assert_ok(),
			),
			(
				Range::new(Bound::Included(0_i64.into()), Bound::Included(10_i64.into()))
					.assert_ok(),
				Range::new(Bound::Included(0_i64.into()), Bound::Unbounded).assert_ok(),
			),
		];
		let value_type = Type::Range(Box::new(Type::Int));
		for (left, right) in ranges {
			let left = Value::from(left);
			let right = Value::from(right);
			assert!(left < right);
			assert!(
				encode(&left, &value_type).assert_ok() < encode(&right, &value_type).assert_ok()
			);
		}
	}

	/// Verifies nan and types without total scalar order are rejected.
	#[test]
	fn nan_and_types_without_total_scalar_order_are_rejected() {
		assert!(encode(&Value::from(f64::NAN), &Type::Float).is_err());
		assert!(validate_index_type(&Type::Number).is_err());
		assert!(validate_index_type(&Type::Object).is_err());
	}
}
