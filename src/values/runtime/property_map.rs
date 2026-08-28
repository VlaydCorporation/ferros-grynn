use std::{
	cmp::Ordering,
	fmt::{self, Debug, Formatter},
	hash::{Hash, Hasher},
	iter::FusedIterator,
	mem::size_of,
	slice::Iter as SliceIter,
};

use thin_vec::ThinVec;

use super::Value;

type Entry = (PropertyKeyId, Value);

#[derive(Debug, Default, Clone, Copy, PartialOrd, PartialEq, Eq, Ord, Hash)]
#[repr(transparent)]
pub struct PropertyKeyId(u32);

impl PropertyKeyId {
	pub const fn new(value: u32) -> Self {
		Self(value)
	}

	pub const fn get(self) -> u32 {
		self.0
	}
}

impl fmt::Display for PropertyKeyId {
	fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
		fmt::Display::fmt(&self.0, f)
	}
}

impl From<u32> for PropertyKeyId {
	fn from(value: u32) -> Self {
		Self(value)
	}
}

impl From<PropertyKeyId> for u32 {
	fn from(value: PropertyKeyId) -> Self {
		value.0
	}
}

/// An atom property map kept in canonical key order.
///
/// `ThinVec` keeps an empty map allocation-free and makes `PropertyMap`
/// pointer-sized. A non-empty map uses one contiguous allocation containing
/// the vector header and entries. Sorted storage favors the common small-map,
/// full-row iteration, cloning, comparison, and canonical encoding paths.
#[derive(Default, Clone)]
#[repr(transparent)]
pub struct PropertyMap {
	entries: ThinVec<Entry>,
}

impl PropertyMap {
	pub fn new() -> Self {
		Self::default()
	}

	pub fn with_capacity(capacity: usize) -> Self {
		Self {
			entries: ThinVec::with_capacity(capacity),
		}
	}

	pub fn len(&self) -> usize {
		self.entries.len()
	}

	pub fn is_empty(&self) -> bool {
		self.entries.is_empty()
	}

	pub fn get(&self, key: PropertyKeyId) -> Option<&Value> {
		self.find(key).ok().map(|index| &self.entries[index].1)
	}

	pub fn get_mut(&mut self, key: PropertyKeyId) -> Option<&mut Value> {
		self.find(key).ok().map(|index| &mut self.entries[index].1)
	}

	pub fn contains_key(&self, key: PropertyKeyId) -> bool {
		self.find(key).is_ok()
	}

	pub fn insert<IntoValue: Into<Value>>(
		&mut self,
		key: PropertyKeyId,
		value: IntoValue,
	) -> Option<Value> {
		let value = value.into();
		match self.find(key) {
			Ok(index) => Some(std::mem::replace(&mut self.entries[index].1, value)),
			Err(index) => {
				self.entries.insert(index, (key, value));
				None
			}
		}
	}

	pub fn remove(&mut self, key: PropertyKeyId) -> Option<Value> {
		self.find(key).ok().map(|index| self.entries.remove(index).1)
	}

	pub fn clear(&mut self) {
		self.entries.clear();
	}

	pub fn reserve(&mut self, additional: usize) {
		self.entries.reserve(additional);
	}

	pub fn iter(&self) -> PropertyIter<'_> {
		PropertyIter {
			inner: self.entries.iter(),
		}
	}

	pub fn values(&self) -> impl DoubleEndedIterator<Item = &Value> + ExactSizeIterator {
		self.entries.iter().map(|(_, value)| value)
	}

	/// Estimates memory owned by the map allocation and nested value payloads.
	pub(crate) fn estimated_heap_bytes(&self) -> usize {
		self.entries.capacity().saturating_mul(size_of::<Entry>()).saturating_add(
			self.entries
				.iter()
				.map(|(_key, value)| value.estimated_heap_bytes())
				.fold(0_usize, usize::saturating_add),
		)
	}

	fn find(&self, key: PropertyKeyId) -> Result<usize, usize> {
		self.entries.binary_search_by_key(&key, |(key, _)| *key)
	}
}

impl Debug for PropertyMap {
	fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
		f.debug_map().entries(self.iter()).finish()
	}
}

const fn entry_ref(entry: &Entry) -> (PropertyKeyId, &Value) {
	(entry.0, &entry.1)
}

impl PartialEq for PropertyMap {
	fn eq(&self, other: &Self) -> bool {
		self.entries == other.entries
	}
}

impl Eq for PropertyMap {}

impl PartialOrd for PropertyMap {
	fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
		Some(self.cmp(other))
	}
}

impl Ord for PropertyMap {
	fn cmp(&self, other: &Self) -> Ordering {
		self.entries.cmp(&other.entries)
	}
}

impl Hash for PropertyMap {
	fn hash<H: Hasher>(&self, state: &mut H) {
		self.entries.hash(state);
	}
}

impl FromIterator<(PropertyKeyId, Value)> for PropertyMap {
	fn from_iter<I: IntoIterator<Item = (PropertyKeyId, Value)>>(iter: I) -> Self {
		let iter = iter.into_iter();
		let mut result = Self::with_capacity(iter.size_hint().0);
		for (key, value) in iter {
			result.insert(key, value);
		}
		result
	}
}

impl<'a> IntoIterator for &'a PropertyMap {
	type IntoIter = PropertyIter<'a>;
	type Item = (PropertyKeyId, &'a Value);

	fn into_iter(self) -> Self::IntoIter {
		self.iter()
	}
}

pub struct PropertyIter<'a> {
	inner: SliceIter<'a, Entry>,
}

impl<'a> Iterator for PropertyIter<'a> {
	type Item = (PropertyKeyId, &'a Value);

	fn next(&mut self) -> Option<Self::Item> {
		self.inner.next().map(entry_ref)
	}

	fn size_hint(&self) -> (usize, Option<usize>) {
		self.inner.size_hint()
	}
}

impl DoubleEndedIterator for PropertyIter<'_> {
	fn next_back(&mut self) -> Option<Self::Item> {
		self.inner.next_back().map(entry_ref)
	}
}

impl ExactSizeIterator for PropertyIter<'_> {
	fn len(&self) -> usize {
		self.inner.len()
	}
}

impl FusedIterator for PropertyIter<'_> {}

#[cfg(test)]
mod tests {
	use std::{
		collections::hash_map::DefaultHasher,
		hash::{Hash, Hasher},
		mem::size_of,
	};

	use super::{PropertyKeyId, PropertyMap};

	#[test]
	fn keeps_entries_sorted_and_replaces_existing_values() {
		let mut properties = PropertyMap::new();
		properties.insert(PropertyKeyId::new(7), 70_i64);
		properties.insert(PropertyKeyId::new(1), 10_i64);
		properties.insert(PropertyKeyId::new(4), 40_i64);

		assert_eq!(properties.iter().map(|(key, _)| key.get()).collect::<Vec<_>>(), [1, 4, 7],);
		assert_eq!(
			properties.insert(PropertyKeyId::new(4), 41_i64),
			Some(crate::values::Value::from(40_i64)),
		);
		assert_eq!(
			properties.get(PropertyKeyId::new(4)),
			Some(&crate::values::Value::from(41_i64))
		);
	}

	#[test]
	fn empty_map_is_pointer_sized_and_does_not_reserve_storage() {
		let properties = PropertyMap::new();
		assert_eq!(size_of::<PropertyMap>(), size_of::<usize>());
		assert!(properties.is_empty());
		assert_send_sync::<PropertyMap>();
	}

	#[test]
	fn removal_preserves_order_and_clear_preserves_validity() {
		let mut properties = (0..16)
			.map(|key| (PropertyKeyId::new(key), crate::values::Value::from(i64::from(key))))
			.collect::<PropertyMap>();
		assert_eq!(
			properties.remove(PropertyKeyId::new(8)),
			Some(crate::values::Value::from(8_i64)),
		);
		assert!(!properties.contains_key(PropertyKeyId::new(8)));
		assert!(properties.iter().map(|(key, _)| key).is_sorted());

		properties.clear();
		assert!(properties.is_empty());
		properties.insert(PropertyKeyId::new(1), 1_i64);
		assert_eq!(properties.len(), 1);
	}

	#[test]
	fn equality_order_and_hash_ignore_insertion_order() {
		let mut first = PropertyMap::new();
		let mut second = PropertyMap::new();
		for key in 0..16 {
			first.insert(PropertyKeyId::new(key), i64::from(key));
		}
		for key in (0..16).rev() {
			second.insert(PropertyKeyId::new(key), i64::from(key));
		}
		assert_eq!(first, second);
		assert_eq!(first.cmp(&second), std::cmp::Ordering::Equal);

		let mut first_hash = DefaultHasher::new();
		first.hash(&mut first_hash);
		let mut second_hash = DefaultHasher::new();
		second.hash(&mut second_hash);
		assert_eq!(first_hash.finish(), second_hash.finish());
	}

	#[test]
	fn clone_is_independent() {
		let mut properties = (0..16)
			.map(|key| (PropertyKeyId::new(key), crate::values::Value::from(i64::from(key))))
			.collect::<PropertyMap>();
		let clone = properties.clone();

		properties.remove(PropertyKeyId::new(8));
		assert_ne!(clone, properties);
		assert!(clone.contains_key(PropertyKeyId::new(8)));
	}

	#[test]
	fn from_iterator_uses_last_duplicate_value() {
		let properties = [
			(PropertyKeyId::new(2), crate::values::Value::from(20_i64)),
			(PropertyKeyId::new(1), crate::values::Value::from(10_i64)),
			(PropertyKeyId::new(2), crate::values::Value::from(21_i64)),
		]
		.into_iter()
		.collect::<PropertyMap>();

		assert_eq!(properties.len(), 2);
		assert_eq!(
			properties.get(PropertyKeyId::new(2)),
			Some(&crate::values::Value::from(21_i64))
		);
	}

	fn assert_send_sync<T: Send + Sync>() {}
}
