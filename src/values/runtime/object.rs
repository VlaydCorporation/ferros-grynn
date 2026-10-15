use std::collections::{
	BTreeMap,
	btree_map::{IntoIter, Iter, IterMut, Keys, Values},
};

use super::{Text, Value};

/// Owned map from text property names to runtime values.
#[derive(Debug, Default, Clone, PartialOrd, PartialEq, Eq, Ord, Hash)]
pub struct Object(
	/// Owned text-keyed runtime values.
	BTreeMap<Text, Value>,
);

impl Object {
	/// Constructs `Object` from the supplied initial state.
	pub fn new() -> Self {
		Self::default()
	}

	/// Returns the logical element count of this `Object`.
	pub fn len(&self) -> usize {
		self.0.len()
	}

	/// Reports whether this `Object` contains no logical elements.
	pub fn is_empty(&self) -> bool {
		self.0.is_empty()
	}

	/// Looks up the requested entry without modifying the collection.
	pub fn get(&self, key: &str) -> Option<&Value> {
		self.0.get(key)
	}

	/// Borrows the requested entry mutably when it exists.
	pub fn get_mut(&mut self, key: &str) -> Option<&mut Value> {
		self.0.get_mut(key)
	}

	/// Tests membership of key for `Object`.
	pub fn contains_key(&self, key: &str) -> bool {
		self.0.contains_key(key)
	}

	/// Inserts a key and value using the collection's replacement rules.
	pub fn insert<IntoText, IntoValue>(&mut self, key: IntoText, value: IntoValue) -> Option<Value>
	where
		IntoText: Into<Text>,
		IntoValue: Into<Value>,
	{
		self.0.insert(key.into(), value.into())
	}

	/// Removes the requested entry and returns it when present.
	pub fn remove(&mut self, key: &str) -> Option<Value> {
		self.0.remove(key)
	}

	/// Borrows the elements of this `Object` for iteration.
	pub fn iter(&self) -> Iter<'_, Text, Value> {
		self.0.iter()
	}

	/// Borrows the elements of this `Object` for mutable iteration.
	pub fn iter_mut(&mut self) -> IterMut<'_, Text, Value> {
		self.0.iter_mut()
	}

	/// Returns keys for `Object`.
	pub fn keys(&self) -> Keys<'_, Text, Value> {
		self.0.keys()
	}

	/// Returns values for `Object`.
	pub fn values(&self) -> Values<'_, Text, Value> {
		self.0.values()
	}

	/// Borrows the wrapped map representation.
	pub const fn as_map(&self) -> &BTreeMap<Text, Value> {
		&self.0
	}

	/// Returns the wrapped inner representation.
	pub fn into_inner(self) -> BTreeMap<Text, Value> {
		self.0
	}
}

impl From<BTreeMap<Text, Value>> for Object {
	/// Converts `BTreeMap<Text, Value>` into `Object`.
	fn from(values: BTreeMap<Text, Value>) -> Self {
		Self(values)
	}
}

impl From<Object> for BTreeMap<Text, Value> {
	/// Converts `Object` into `BTreeMap<Text, Value>`.
	fn from(value: Object) -> Self {
		value.0
	}
}

impl<K, V> FromIterator<(K, V)> for Object
where
	K: Into<Text>,
	V: Into<Value>,
{
	/// Builds this `Object` from the supplied elements.
	fn from_iter<I: IntoIterator<Item = (K, V)>>(iter: I) -> Self {
		Self(iter.into_iter().map(|(key, value)| (key.into(), value.into())).collect())
	}
}

impl IntoIterator for Object {
	type IntoIter = IntoIter<Text, Value>;
	type Item = (Text, Value);

	/// Consumes this `Object` and yields its elements.
	fn into_iter(self) -> Self::IntoIter {
		self.0.into_iter()
	}
}

impl<'a> IntoIterator for &'a Object {
	type IntoIter = Iter<'a, Text, Value>;
	type Item = (&'a Text, &'a Value);

	/// Iterates over borrowed elements without consuming the collection.
	fn into_iter(self) -> Self::IntoIter {
		self.iter()
	}
}

impl<'a> IntoIterator for &'a mut Object {
	type IntoIter = IterMut<'a, Text, Value>;
	type Item = (&'a Text, &'a mut Value);

	/// Iterates over borrowed elements without consuming the collection.
	fn into_iter(self) -> Self::IntoIter {
		self.iter_mut()
	}
}

#[cfg(test)]
/// Regression tests for object behavior.
mod tests {
	use super::Object;

	/// Verifies has canonical key order independent of insertion.
	#[test]
	fn has_canonical_key_order_independent_of_insertion() {
		let mut first = Object::new();
		first.insert("z", 1_i64);
		first.insert("a", 2_i64);

		let mut second = Object::new();
		second.insert("a", 2_i64);
		second.insert("z", 1_i64);

		assert_eq!(first, second);
		assert_eq!(first.keys().map(|key| key.as_str()).collect::<Vec<_>>(), vec!["a", "z"],);
	}
}
