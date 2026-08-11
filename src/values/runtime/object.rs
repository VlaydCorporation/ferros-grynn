use std::collections::{
	BTreeMap,
	btree_map::{IntoIter, Iter, IterMut, Keys, Values},
};

use super::{Text, Value};

#[derive(Debug, Default, Clone, PartialOrd, PartialEq, Eq, Ord, Hash)]
pub struct Object(BTreeMap<Text, Value>);

impl Object {
	pub fn new() -> Self {
		Self::default()
	}

	pub fn len(&self) -> usize {
		self.0.len()
	}

	pub fn is_empty(&self) -> bool {
		self.0.is_empty()
	}

	pub fn get(&self, key: &str) -> Option<&Value> {
		self.0.get(key)
	}

	pub fn get_mut(&mut self, key: &str) -> Option<&mut Value> {
		self.0.get_mut(key)
	}

	pub fn contains_key(&self, key: &str) -> bool {
		self.0.contains_key(key)
	}

	pub fn insert<IntoText, IntoValue>(&mut self, key: IntoText, value: IntoValue) -> Option<Value>
	where
		IntoText: Into<Text>,
		IntoValue: Into<Value>,
	{
		self.0.insert(key.into(), value.into())
	}

	pub fn remove(&mut self, key: &str) -> Option<Value> {
		self.0.remove(key)
	}

	pub fn iter(&self) -> Iter<'_, Text, Value> {
		self.0.iter()
	}

	pub fn iter_mut(&mut self) -> IterMut<'_, Text, Value> {
		self.0.iter_mut()
	}

	pub fn keys(&self) -> Keys<'_, Text, Value> {
		self.0.keys()
	}

	pub fn values(&self) -> Values<'_, Text, Value> {
		self.0.values()
	}

	pub const fn as_map(&self) -> &BTreeMap<Text, Value> {
		&self.0
	}

	pub fn into_inner(self) -> BTreeMap<Text, Value> {
		self.0
	}
}

impl From<BTreeMap<Text, Value>> for Object {
	fn from(values: BTreeMap<Text, Value>) -> Self {
		Self(values)
	}
}

impl From<Object> for BTreeMap<Text, Value> {
	fn from(value: Object) -> Self {
		value.0
	}
}

impl<K, V> FromIterator<(K, V)> for Object
where
	K: Into<Text>,
	V: Into<Value>,
{
	fn from_iter<I: IntoIterator<Item = (K, V)>>(iter: I) -> Self {
		Self(iter.into_iter().map(|(key, value)| (key.into(), value.into())).collect())
	}
}

impl IntoIterator for Object {
	type Item = (Text, Value);
	type IntoIter = IntoIter<Text, Value>;

	fn into_iter(self) -> Self::IntoIter {
		self.0.into_iter()
	}
}

impl<'a> IntoIterator for &'a Object {
	type Item = (&'a Text, &'a Value);
	type IntoIter = Iter<'a, Text, Value>;

	fn into_iter(self) -> Self::IntoIter {
		self.iter()
	}
}

impl<'a> IntoIterator for &'a mut Object {
	type Item = (&'a Text, &'a mut Value);
	type IntoIter = IterMut<'a, Text, Value>;

	fn into_iter(self) -> Self::IntoIter {
		self.iter_mut()
	}
}

#[cfg(test)]
mod tests {
	use super::Object;

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
