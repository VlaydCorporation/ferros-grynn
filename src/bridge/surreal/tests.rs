//! Native runtime/link checks complement the independent SDK compile spike.

use super::*;
use crate::values::{Number, RecordId};

/// Returns context for this module.
fn context() -> BridgeContext {
	let mut options = QueryExecutionOptions::default();
	options.context.bridge_memory = Some(Arc::new(parking_lot::Mutex::new(
		crate::gql::BarrierAccount::new(options.barrier_memory_bytes.get()),
	)));
	BridgeContext {
		options,
		cancellation: crate::gql::QueryCancellation::default(),
		parent_cancellation: None,
		deadline: None,
	}
}

/// Verifies bridge surreal typed values roundtrip.
#[test]
fn bridge_surreal_typed_values_roundtrip() {
	use crate::test_assertions::AssertResult as _;

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
		Value::Range(
			crate::values::Range::new(
				std::ops::Bound::Included(Value::from(1_i64)),
				std::ops::Bound::Excluded(Value::from(9_i64)),
			)
			.assert_ok(),
		),
		Value::Geometry(crate::values::Geometry::Point(
			crate::values::Position::new(vec![7.0, 8.0]).assert_ok(),
		)),
		Value::Record(Box::new(
			RecordId::from_value(
				"native",
				Value::Array(
					vec![Value::from(7_i64), Value::Uuid(uuid::Uuid::now_v7().into())].into(),
				),
			)
			.assert_ok(),
		)),
		Value::DateTime(crate::values::DateTime::new(
			jiff::Timestamp::from_nanosecond(-1)
				.map_err(|error| {
					FerrosGrynnError::bridge_operation_error("test timestamp failed")
						.with_technical_source(error)
				})
				.assert_ok()
				.to_zoned(jiff::tz::TimeZone::UTC),
		)),
	];
	for value in values {
		assert_eq!(
			conversion::incoming(
				conversion::outgoing(value.clone(), &context, 0).assert_ok(),
				&context,
				0
			)
			.assert_ok(),
			value
		);
	}
	assert!(
		conversion::outgoing(Value::Ulid(crate::values::Ulid::default()), &context, 0).is_err()
	);
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
}

/// Builds the recursive native key shape which exposes the SDK regex cache to
/// Clippy.
fn sdk_regex_record_key(regex: sdk::Regex) -> sdk::RecordId {
	let object = sdk::Object::from_iter([("pattern".to_owned(), sdk::Value::Regex(regex))]);
	sdk::RecordId::new("native", sdk::RecordIdKey::Array(vec![sdk::Value::Object(object)].into()))
}

/// Borrows the actual regex stored inside the nested native collection key.
///
/// # Panics
/// Panics if the fixture no longer has its expected array/object/regex shape.
fn sdk_record_key_regex(record: &sdk::RecordId) -> &sdk::Regex {
	use crate::test_assertions::AssertOption as _;

	match &record.key {
		sdk::RecordIdKey::Array(values) => values
			.first()
			.and_then(|value| match value {
				sdk::Value::Object(object) => object.get("pattern"),
				_ => None,
			})
			.and_then(|value| match value {
				sdk::Value::Regex(regex) => Some(regex),
				_ => None,
			}),
		_ => None,
	}
	.assert_some_with("native regex key fixture must retain its exact typed shape")
}

/// Hashes a native key with one fixed implementation before and after cache
/// use.
fn sdk_key_hash<Key: std::hash::Hash + ?Sized>(key: &Key) -> u64 {
	let mut hash = std::collections::hash_map::DefaultHasher::new();
	std::hash::Hash::hash(key, &mut hash);
	std::hash::Hasher::finish(&hash)
}

/// Search through stored native keys must not change pattern identity or
/// tree/hash lookup.
#[test]
fn sdk_regex_search_cache_preserves_native_record_key_identity() {
	use std::collections::{BTreeMap, HashMap};

	use crate::test_assertions::AssertResult as _;

	let regex = r"\p{L}+".parse::<sdk::Regex>().assert_ok();
	let other_regex = r"\d+".parse::<sdk::Regex>().assert_ok();
	let direct_digest = sdk_key_hash(&regex);
	let direct_order = regex.cmp(&other_regex);
	let key = sdk_regex_record_key(regex.clone());
	let equal = sdk_regex_record_key(r"\p{L}+".parse::<sdk::Regex>().assert_ok());
	let different = sdk_regex_record_key(other_regex.clone());
	let digest = sdk_key_hash(&key);
	let order = key.cmp(&different);
	let mut ordered = BTreeMap::from([(key.clone(), 7)]);
	let mut hashed = HashMap::from([(key.clone(), 7)]);
	for (text, expected) in [("Ferros-Grynn", true), ("граф", true), ("123", false), ("", false)]
	{
		// Touch the cache inside each stored key, rather than only a detached
		// clone.
		for stored in ordered.keys().chain(hashed.keys()) {
			let stored_regex = sdk_record_key_regex(stored);
			assert_eq!(stored_regex.is_match(text), expected);
			assert_eq!(stored_regex, &regex);
			assert_eq!(stored_regex.cmp(&other_regex), direct_order);
			assert_eq!(sdk_key_hash(stored_regex), direct_digest);
			assert_eq!(sdk_key_hash(stored), digest);
		}
		assert_eq!(key, equal);
		assert_eq!(key.cmp(&different), order);
		assert_eq!(sdk_key_hash(&key), digest);
		assert_eq!(sdk_key_hash(&equal), digest);
		assert_eq!(ordered.get(&equal), Some(&7));
		assert_eq!(hashed.get(&equal), Some(&7));
	}
	assert_eq!(ordered.insert(equal.clone(), 7), Some(7));
	assert_eq!(hashed.insert(equal.clone(), 7), Some(7));
	assert_eq!(ordered.len(), 1);
	assert_eq!(hashed.len(), 1);
	assert_eq!(ordered.remove(&equal), Some(7));
	assert_eq!(hashed.remove(&equal), Some(7));
}
