use std::{
	cmp::Ordering,
	collections::hash_map::DefaultHasher,
	hash::{Hash, Hasher},
};

use proptest::prelude::*;
use rust_decimal::Decimal;

use super::{Number, Object, PropertyKeyId, PropertyMap, Value};

fn number() -> impl Strategy<Value = Number> {
	prop_oneof![
		any::<i64>().prop_map(Number::Int),
		any::<u64>().prop_map(|bits| Number::Float(f64::from_bits(bits))),
		(any::<i64>(), 0_u32..=28)
			.prop_map(|(mantissa, scale)| Number::Decimal(Decimal::new(mantissa, scale))),
	]
}

fn scalar_value() -> impl Strategy<Value = Value> {
	prop_oneof![
		Just(Value::None),
		Just(Value::Null),
		any::<bool>().prop_map(Value::Bool),
		number().prop_map(Value::Number),
		".{0,32}".prop_map(Value::from),
	]
}

fn hash(value: &Value) -> u64 {
	let mut hasher = DefaultHasher::new();
	value.hash(&mut hasher);
	hasher.finish()
}

proptest! {
	#[test]
	fn value_eq_ord_hash_are_reflexive(value in scalar_value()) {
		prop_assert_eq!(&value, &value);
		prop_assert_eq!(value.cmp(&value), Ordering::Equal);
		prop_assert_eq!(hash(&value), hash(&value));
	}

	#[test]
	fn value_order_is_antisymmetric(left in scalar_value(), right in scalar_value()) {
		prop_assert_eq!(left.cmp(&right), right.cmp(&left).reverse());
		if left == right {
			prop_assert_eq!(hash(&left), hash(&right));
		}
	}

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

	#[test]
	fn exact_integer_float_equality_has_equal_hash(value in -(1_i64 << 53)..=(1_i64 << 53)) {
		let integer = Value::from(value);
		let float = Value::from(value as f64);
		prop_assert_eq!(&integer, &float);
		prop_assert_eq!(hash(&integer), hash(&float));
	}

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
