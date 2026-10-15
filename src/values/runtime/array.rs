use std::slice::{Iter, IterMut};

use super::Value;

/// Owned ordered sequence of runtime values.
#[derive(Debug, Default, Clone, PartialOrd, PartialEq, Eq, Ord, Hash)]
pub struct Array(
	/// Owned values in their runtime sequence order.
	Vec<Value>,
);

impl Array {
	/// Constructs `Array` from the supplied initial state.
	pub fn new(values: Vec<Value>) -> Self {
		Self(values)
	}

	/// Creates an empty collection with the requested initial capacity.
	pub fn with_capacity(capacity: usize) -> Self {
		Self(Vec::with_capacity(capacity))
	}

	/// Returns the logical element count of this `Array`.
	pub fn len(&self) -> usize {
		self.0.len()
	}

	/// Reports whether this `Array` contains no logical elements.
	pub fn is_empty(&self) -> bool {
		self.0.is_empty()
	}

	/// Borrows the elements as a contiguous slice.
	pub fn as_slice(&self) -> &[Value] {
		&self.0
	}

	/// Borrows the elements of this `Array` for iteration.
	pub fn iter(&self) -> Iter<'_, Value> {
		self.0.iter()
	}

	/// Borrows the elements of this `Array` for mutable iteration.
	pub fn iter_mut(&mut self) -> IterMut<'_, Value> {
		self.0.iter_mut()
	}

	/// Looks up the requested entry without modifying the collection.
	pub fn get(&self, index: usize) -> Option<&Value> {
		self.0.get(index)
	}

	/// Borrows the requested entry mutably when it exists.
	pub fn get_mut(&mut self, index: usize) -> Option<&mut Value> {
		self.0.get_mut(index)
	}

	/// Appends one value to the collection.
	pub fn push<IntoValue: Into<Value>>(&mut self, value: IntoValue) {
		self.0.push(value.into());
	}

	/// Removes and returns the last value, if present.
	pub fn pop(&mut self) -> Option<Value> {
		self.0.pop()
	}

	/// Removes all logical elements from the collection.
	pub fn clear(&mut self) {
		self.0.clear();
	}

	/// Consumes the collection and returns its owned element vector.
	pub fn into_vec(self) -> Vec<Value> {
		self.0
	}
}

impl IntoIterator for Array {
	type IntoIter = std::vec::IntoIter<Value>;
	type Item = Value;

	/// Consumes this `Array` and yields its elements.
	fn into_iter(self) -> Self::IntoIter {
		self.0.into_iter()
	}
}

impl<'a> IntoIterator for &'a Array {
	type IntoIter = Iter<'a, Value>;
	type Item = &'a Value;

	/// Iterates over borrowed elements without consuming the collection.
	fn into_iter(self) -> Self::IntoIter {
		self.iter()
	}
}

impl<'a> IntoIterator for &'a mut Array {
	type IntoIter = IterMut<'a, Value>;
	type Item = &'a mut Value;

	/// Iterates over borrowed elements without consuming the collection.
	fn into_iter(self) -> Self::IntoIter {
		self.iter_mut()
	}
}

impl FromIterator<Value> for Array {
	/// Builds this `Array` from the supplied elements.
	fn from_iter<I: IntoIterator<Item = Value>>(iter: I) -> Self {
		Self(iter.into_iter().collect())
	}
}

impl<T> From<Vec<T>> for Array
where
	Value: From<T>,
{
	/// Converts `Vec<T>` into `Array`.
	fn from(values: Vec<T>) -> Self {
		values.into_iter().map(Value::from).collect()
	}
}

impl From<Array> for Vec<Value> {
	/// Converts `Array` into `Vec<Value>`.
	fn from(value: Array) -> Self {
		value.0
	}
}

#[cfg(test)]
/// Regression tests for array behavior.
mod tests {
	use super::Array;
	use crate::values::Value;

	/// Verifies new preserves values.
	#[test]
	fn new_preserves_values() {
		let values = vec![Value::from(1_i64), Value::from("two")];
		assert_eq!(Array::new(values.clone()).as_slice(), values);
	}

	/// Verifies supports controlled mutation.
	#[test]
	fn supports_controlled_mutation() {
		let mut array = Array::with_capacity(2);
		array.push(1_i64);
		array.push(true);
		assert_eq!(array.len(), 2);
		assert_eq!(array.pop(), Some(Value::Bool(true)));
	}
}
