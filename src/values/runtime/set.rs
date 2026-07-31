#![allow(clippy::mutable_key_type)]

use std::collections::{BTreeSet, btree_set::Iter};

use super::Value;

#[derive(Debug, Default, Clone, PartialOrd, PartialEq, Eq, Ord, Hash)]
pub struct Set(BTreeSet<Value>);

impl Set {
	pub fn new() -> Self {
		Self::default()
	}

	pub fn len(&self) -> usize {
		self.0.len()
	}

	pub fn is_empty(&self) -> bool {
		self.0.is_empty()
	}

	pub fn contains(&self, value: &Value) -> bool {
		self.0.contains(value)
	}

	pub fn iter(&self) -> Iter<'_, Value> {
		self.0.iter()
	}

	pub fn insert(&mut self, value: impl Into<Value>) -> bool {
		self.0.insert(value.into())
	}

	pub fn remove(&mut self, value: &Value) -> bool {
		self.0.remove(value)
	}

	pub fn into_inner(self) -> BTreeSet<Value> {
		self.0
	}
}

impl From<BTreeSet<Value>> for Set {
	fn from(values: BTreeSet<Value>) -> Self {
		Self(values)
	}
}

impl From<Set> for BTreeSet<Value> {
	fn from(value: Set) -> Self {
		value.0
	}
}

impl<T> From<Vec<T>> for Set
where
	Value: From<T>,
{
	fn from(values: Vec<T>) -> Self {
		values.into_iter().map(Value::from).collect()
	}
}

impl FromIterator<Value> for Set {
	fn from_iter<I: IntoIterator<Item = Value>>(iter: I) -> Self {
		Self(iter.into_iter().collect())
	}
}

impl IntoIterator for Set {
	type IntoIter = std::collections::btree_set::IntoIter<Value>;
	type Item = Value;

	fn into_iter(self) -> Self::IntoIter {
		self.0.into_iter()
	}
}

impl<'a> IntoIterator for &'a Set {
	type IntoIter = Iter<'a, Value>;
	type Item = &'a Value;

	fn into_iter(self) -> Self::IntoIter {
		self.iter()
	}
}

#[cfg(test)]
mod tests {
	use super::Set;

	#[test]
	#[allow(clippy::mutable_key_type)]
	fn deduplicates_and_orders_values() {
		let set = Set::from(vec![3_i64, 1, 3, 2]);
		let values = set
			.iter()
			.map(|value| value.as_number().unwrap().as_i64().unwrap())
			.collect::<Vec<_>>();
		assert_eq!(values, vec![1, 2, 3]);
	}
}
