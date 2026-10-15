#![allow(
	clippy::mutable_key_type,
	reason = "Value has interior mutability only in opaque runtime variants with stable identity"
)]

use std::collections::{BTreeSet, btree_set::Iter};

use super::Value;

/// Owned set of runtime values ordered by their value comparison contract.
#[derive(Debug, Default, Clone, PartialOrd, PartialEq, Eq, Ord, Hash)]
pub struct Set(
	/// Owned values maintained in runtime comparison order.
	BTreeSet<Value>,
);

impl Set {
	/// Constructs `Set` from the supplied initial state.
	pub fn new() -> Self {
		Self::default()
	}

	/// Returns the logical element count of this `Set`.
	pub fn len(&self) -> usize {
		self.0.len()
	}

	/// Reports whether this `Set` contains no logical elements.
	pub fn is_empty(&self) -> bool {
		self.0.is_empty()
	}

	/// Reports whether the contains condition holds for `Set`.
	pub fn contains(&self, value: &Value) -> bool {
		self.0.contains(value)
	}

	/// Borrows the elements of this `Set` for iteration.
	pub fn iter(&self) -> Iter<'_, Value> {
		self.0.iter()
	}

	/// Reports whether the insert condition holds for `Set`.
	pub fn insert<IntoValue: Into<Value>>(&mut self, value: IntoValue) -> bool {
		self.0.insert(value.into())
	}

	/// Reports whether the remove condition holds for `Set`.
	pub fn remove(&mut self, value: &Value) -> bool {
		self.0.remove(value)
	}

	/// Removes all logical elements from the collection.
	pub fn clear(&mut self) {
		self.0.clear();
	}

	/// Returns the wrapped inner representation.
	pub fn into_inner(self) -> BTreeSet<Value> {
		self.0
	}
}

impl From<BTreeSet<Value>> for Set {
	/// Converts `BTreeSet<Value>` into `Set`.
	fn from(values: BTreeSet<Value>) -> Self {
		Self(values)
	}
}

impl From<Set> for BTreeSet<Value> {
	/// Converts `Set` into `BTreeSet<Value>`.
	fn from(value: Set) -> Self {
		value.0
	}
}

impl<T> From<Vec<T>> for Set
where
	Value: From<T>,
{
	/// Converts `Vec<T>` into `Set`.
	fn from(values: Vec<T>) -> Self {
		values.into_iter().map(Value::from).collect()
	}
}

impl FromIterator<Value> for Set {
	/// Builds this `Set` from the supplied elements.
	fn from_iter<I: IntoIterator<Item = Value>>(iter: I) -> Self {
		Self(iter.into_iter().collect())
	}
}

impl IntoIterator for Set {
	type IntoIter = std::collections::btree_set::IntoIter<Value>;
	type Item = Value;

	/// Consumes this `Set` and yields its elements.
	fn into_iter(self) -> Self::IntoIter {
		self.0.into_iter()
	}
}

impl<'a> IntoIterator for &'a Set {
	type IntoIter = Iter<'a, Value>;
	type Item = &'a Value;

	/// Iterates over borrowed elements without consuming the collection.
	fn into_iter(self) -> Self::IntoIter {
		self.iter()
	}
}

#[cfg(test)]
/// Regression tests for set behavior.
mod tests {
	use super::Set;
	#[cfg(test)]
	use crate::test_assertions::AssertOption as _;

	/// Verifies deduplicates and orders values.
	#[test]
	#[allow(
		clippy::mutable_key_type,
		reason = "Value keys compare immutable data or stable runtime identities"
	)]
	fn deduplicates_and_orders_values() {
		let set = Set::from(vec![3_i64, 1, 3, 2]);
		let values = set
			.iter()
			.map(|value| value.as_number().assert_some().as_i64().assert_some())
			.collect::<Vec<_>>();
		assert_eq!(values, vec![1, 2, 3]);
	}
}
