use std::{
	cmp::Ordering,
	collections::hash_map::DefaultHasher,
	hash::{Hash, Hasher},
};

use proptest::prelude::*;
use rust_decimal::Decimal;

use super::{
	Array,
	Matcher,
	Number,
	Object,
	PropertyKeyId,
	PropertyMap,
	RecordId,
	Regex,
	Text,
	Value,
};
use crate::test_assertions::AssertResult as _;

/// Generates integer, floating-point and decimal cases for numeric law tests.
fn number() -> impl Strategy<Value = Number> {
	prop_oneof![
		any::<i64>().prop_map(Number::Int),
		any::<u64>().prop_map(|bits| Number::Float(f64::from_bits(bits))),
		(any::<i64>(), 0_u32..=28)
			.prop_map(|(mantissa, scale)| Number::Decimal(Decimal::new(mantissa, scale))),
	]
}

/// Generates scalar values for equality, ordering and hashing law tests.
fn scalar_value() -> impl Strategy<Value = Value> {
	prop_oneof![
		Just(Value::None),
		Just(Value::Null),
		any::<bool>().prop_map(Value::Bool),
		number().prop_map(Value::Number),
		".{0,32}".prop_map(Value::from),
	]
}

/// Computes a digest with the same hasher before and after key-state changes.
fn hash<T: Hash + ?Sized>(value: &T) -> u64 {
	let mut hasher = DefaultHasher::new();
	value.hash(&mut hasher);
	hasher.finish()
}

/// Verifies that compiled-regex cache changes cannot invalidate collection
/// keys.
#[test]
#[allow(
	clippy::mutable_key_type,
	reason = "Runtime key equality and ordering use stable logical identities"
)]
fn regex_cache_preserves_key_identity() {
	use std::collections::{BTreeMap, HashMap};

	let regex = Regex::new(r"\p{L}+").assert_ok();
	let key = Value::Regex(regex.clone());
	let equal = Value::Regex(Regex::new(r"\p{L}+").assert_ok());
	let different = Value::Regex(Regex::new(r"\d+").assert_ok());
	let digest = hash(&key);
	let order = key.cmp(&different);
	let mut ordered = BTreeMap::from([(key.clone(), 7)]);
	let mut hashed = HashMap::from([(key.clone(), 7)]);
	for text in ["Ferros-Grynn", "граф", "123", ""] {
		let expected = regex.is_match(text);
		for stored in ordered.keys().chain(hashed.keys()) {
			if let Value::Regex(stored) = stored {
				assert_eq!(stored.is_match(text), expected);
			}
		}
		assert_eq!(key, equal);
		assert_eq!(hash(&key), digest);
		assert_eq!(hash(&equal), digest);
		assert_eq!(key.cmp(&different), order);
		assert_eq!(ordered.get(&equal), Some(&7));
		assert_eq!(hashed.get(&equal), Some(&7));
	}
	assert_eq!(ordered.remove(&equal), Some(7));
	assert_eq!(hashed.remove(&equal), Some(7));
}

/// Verifies that shared matcher cursor progress preserves direct and nested
/// keys.
#[test]
#[allow(
	clippy::mutable_key_type,
	reason = "Runtime key equality and ordering use stable logical identities"
)]
fn matcher_cursor_preserves_key_identity() {
	use std::collections::{BTreeMap, HashMap};

	let matcher = Matcher::new(Text::from("åβ"), Regex::new("").assert_ok());
	let other = Matcher::new(Text::from("other"), Regex::new("").assert_ok());
	let direct = Value::Matcher(matcher.clone());
	let nested = Value::Array(Array::new(vec![direct.clone()]));
	let other = Value::Matcher(other);
	let keys = [direct.clone(), nested];
	let digests = keys.each_ref().map(hash);
	let order = direct.cmp(&other);
	let mut ordered = BTreeMap::from([(keys[0].clone(), 0), (keys[1].clone(), 1)]);
	let mut hashed = HashMap::from([(keys[0].clone(), 0), (keys[1].clone(), 1)]);
	for offset in 0..=2 {
		assert!(matcher.advance());
		assert_eq!(matcher.start(), Some(offset));
		assert_eq!(matcher.end(), Some(offset));
		for (index, key) in keys.iter().enumerate() {
			assert_eq!(hash(key), digests[index]);
			assert_eq!(ordered.get(key), Some(&index));
			assert_eq!(hashed.get(key), Some(&index));
		}
		assert_eq!(direct.cmp(&other), order);
		assert_eq!(ordered.len(), 2);
		assert_eq!(hashed.len(), 2);
	}
	assert!(!matcher.advance());
	for (index, key) in keys.iter().enumerate() {
		assert_eq!(ordered.remove(key), Some(index));
		assert_eq!(hashed.remove(key), Some(index));
	}
}

/// Verifies that composite persistent identifiers reject stateful runtime keys.
#[test]
fn record_identifiers_reject_nested_runtime_keys() {
	let regex = Regex::new("grynn").assert_ok();
	let matcher = Matcher::new(Text::from("grynn"), regex.clone());
	for value in [Value::Regex(regex), Value::Matcher(matcher)] {
		let nested =
			Value::Array(Array::new(vec![Value::Object(Object::from_iter([("nested", value)]))]));
		assert!(RecordId::from_value("item", nested).is_err());
	}
}

proptest! {
	/// Verifies value eq ord hash are reflexive.
	#[test]
	fn value_eq_ord_hash_are_reflexive(value in scalar_value()) {
		prop_assert_eq!(&value, &value);
		prop_assert_eq!(value.cmp(&value), Ordering::Equal);
		prop_assert_eq!(hash(&value), hash(&value));
	}

	/// Verifies value order is antisymmetric.
	#[test]
	fn value_order_is_antisymmetric(left in scalar_value(), right in scalar_value()) {
		prop_assert_eq!(left.cmp(&right), right.cmp(&left).reverse());
		if left == right {
			prop_assert_eq!(hash(&left), hash(&right));
		}
	}

	/// Verifies value order is transitive.
	#[test]
	fn value_order_is_transitive(
		first in scalar_value(),
		second in scalar_value(),
		third in scalar_value(),
	) {
		if first <= second && second <= third {
			prop_assert!(first <= third);
		}
	}

	/// Verifies exact integer float equality has equal hash.
	#[test]
	fn exact_integer_float_equality_has_equal_hash(value in -(1_i64 << 53)..=(1_i64 << 53)) {
		let integer = Value::from(value);
		let float = Value::from(value as f64);
		prop_assert_eq!(&integer, &float);
		prop_assert_eq!(hash(&integer), hash(&float));
	}

	/// Verifies object order is independent of insertion.
	#[test]
	fn object_order_is_independent_of_insertion(values in prop::collection::vec(any::<i64>(), 0..32)) {
		let mut forward = Object::new();
		let mut reverse = Object::new();
		for (key, value) in values.iter().enumerate() {
			forward.insert(key.to_string(), *value);
		}
		for (key, value) in values.iter().enumerate().rev() {
			reverse.insert(key.to_string(), *value);
		}
		prop_assert_eq!(forward, reverse);
	}

	/// Verifies property map representation does not affect semantics.
	#[test]
	fn property_map_representation_does_not_affect_semantics(
		values in prop::collection::vec(any::<i64>(), 0..32),
	) {
		let mut forward = PropertyMap::new();
		let mut reverse = PropertyMap::new();
		for (key, value) in values.iter().enumerate() {
			forward.insert(PropertyKeyId::new(key as u32), *value);
		}
		for (key, value) in values.iter().enumerate().rev() {
			reverse.insert(PropertyKeyId::new(key as u32), *value);
		}
		prop_assert_eq!(&forward, &reverse);

		let mut left_hash = DefaultHasher::new();
		forward.hash(&mut left_hash);
		let mut right_hash = DefaultHasher::new();
		reverse.hash(&mut right_hash);
		prop_assert_eq!(left_hash.finish(), right_hash.finish());
	}
}
