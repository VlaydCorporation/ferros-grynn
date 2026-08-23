use std::slice::{Iter, IterMut};

use super::Value;

#[derive(Debug, Default, Clone, PartialOrd, PartialEq, Eq, Ord, Hash)]
pub struct Array(Vec<Value>);

impl Array {
	pub fn new(values: Vec<Value>) -> Self {
		Self(values)
	}

	pub fn with_capacity(capacity: usize) -> Self {
		Self(Vec::with_capacity(capacity))
	}

	pub fn len(&self) -> usize {
		self.0.len()
	}

	pub fn is_empty(&self) -> bool {
		self.0.is_empty()
	}

	pub fn as_slice(&self) -> &[Value] {
		&self.0
	}

	pub fn iter(&self) -> Iter<'_, Value> {
		self.0.iter()
	}

	pub fn iter_mut(&mut self) -> IterMut<'_, Value> {
		self.0.iter_mut()
	}

	pub fn get(&self, index: usize) -> Option<&Value> {
		self.0.get(index)
	}

	pub fn get_mut(&mut self, index: usize) -> Option<&mut Value> {
		self.0.get_mut(index)
	}

	pub fn push<IntoValue: Into<Value>>(&mut self, value: IntoValue) {
		self.0.push(value.into());
	}

	pub fn pop(&mut self) -> Option<Value> {
		self.0.pop()
	}

	pub fn into_vec(self) -> Vec<Value> {
		self.0
	}
}

impl IntoIterator for Array {
	type IntoIter = std::vec::IntoIter<Value>;
	type Item = Value;

	fn into_iter(self) -> Self::IntoIter {
		self.0.into_iter()
	}
}

impl<'a> IntoIterator for &'a Array {
	type IntoIter = Iter<'a, Value>;
	type Item = &'a Value;

	fn into_iter(self) -> Self::IntoIter {
		self.iter()
	}
}

impl<'a> IntoIterator for &'a mut Array {
	type IntoIter = IterMut<'a, Value>;
	type Item = &'a mut Value;

	fn into_iter(self) -> Self::IntoIter {
		self.iter_mut()
	}
}

impl FromIterator<Value> for Array {
	fn from_iter<I: IntoIterator<Item = Value>>(iter: I) -> Self {
		Self(iter.into_iter().collect())
	}
}

impl<T> From<Vec<T>> for Array
where
	Value: From<T>,
{
	fn from(values: Vec<T>) -> Self {
		values.into_iter().map(Value::from).collect()
	}
}

impl From<Array> for Vec<Value> {
	fn from(value: Array) -> Self {
		value.0
	}
}

#[cfg(test)]
mod tests {
	use super::Array;
	use crate::values::Value;

	#[test]
	fn new_preserves_values() {
		let values = vec![Value::from(1_i64), Value::from("two")];
		assert_eq!(Array::new(values.clone()).as_slice(), values);
	}

	#[test]
	fn supports_controlled_mutation() {
		let mut array = Array::with_capacity(2);
		array.push(1_i64);
		array.push(true);
		assert_eq!(array.len(), 2);
		assert_eq!(array.pop(), Some(Value::Bool(true)));
	}
}
