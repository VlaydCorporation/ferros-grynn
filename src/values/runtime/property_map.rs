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

/// Type used to represent entry.
type Entry = (PropertyKeyId, Value);

/// Interned property-name identifier used by compact property maps.
#[derive(Debug, Default, Clone, Copy, PartialOrd, PartialEq, Eq, Ord, Hash)]
#[repr(transparent)]
pub struct PropertyKeyId(
	/// Numeric identifier assigned by the property dictionary.
	u32,
);

impl PropertyKeyId {
	/// Constructs `PropertyKeyId` from the supplied initial state.
	pub const fn new(value: u32) -> Self {
		Self(value)
	}

	/// Returns the numeric identifier represented by this PropertyKeyId.
	pub const fn get(self) -> u32 {
		self.0
	}
}

impl fmt::Display for PropertyKeyId {
	/// Formats `PropertyKeyId` using the requested formatter.
	fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
		fmt::Display::fmt(&self.0, f)
	}
}

impl From<u32> for PropertyKeyId {
	/// Converts `u32` into `PropertyKeyId`.
	fn from(value: u32) -> Self {
		Self(value)
	}
}

impl From<PropertyKeyId> for u32 {
	/// Converts `PropertyKeyId` into `u32`.
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
	/// Entries associated with this property map.
	entries: ThinVec<Entry>,
}

impl PropertyMap {
	/// Constructs `PropertyMap` from the supplied initial state.
	pub fn new() -> Self {
		Self::default()
	}

	/// Creates an empty collection with the requested initial capacity.
	pub fn with_capacity(capacity: usize) -> Self {
		Self {
			entries: ThinVec::with_capacity(capacity),
		}
	}

	/// Returns the logical element count of this `PropertyMap`.
	pub fn len(&self) -> usize {
		self.entries.len()
	}

	/// Reports whether this `PropertyMap` contains no logical elements.
	pub fn is_empty(&self) -> bool {
		self.entries.is_empty()
	}

	/// Looks up the requested entry without modifying the collection.
	pub fn get(&self, key: PropertyKeyId) -> Option<&Value> {
		self.find(key).ok().map(|index| &self.entries[index].1)
	}

	/// Borrows the requested entry mutably when it exists.
	pub fn get_mut(&mut self, key: PropertyKeyId) -> Option<&mut Value> {
		self.find(key).ok().map(|index| &mut self.entries[index].1)
	}

	/// Tests membership of key for `PropertyMap`.
	pub fn contains_key(&self, key: PropertyKeyId) -> bool {
		self.find(key).is_ok()
	}

	/// Inserts a key and value using the collection's replacement rules.
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

	/// Removes the requested entry and returns it when present.
	pub fn remove(&mut self, key: PropertyKeyId) -> Option<Value> {
		self.find(key).ok().map(|index| self.entries.remove(index).1)
	}

	/// Removes all logical elements from the collection.
	pub fn clear(&mut self) {
		self.entries.clear();
	}

	/// Returns reserve for `PropertyMap`.
	pub fn reserve(&mut self, additional: usize) {
		self.entries.reserve(additional);
	}

	/// Borrows the elements of this `PropertyMap` for iteration.
	pub fn iter(&self) -> PropertyIter<'_> {
		PropertyIter {
			inner: self.entries.iter(),
		}
	}

	/// Returns values for `PropertyMap`.
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

	/// Finds property map.
	fn find(&self, key: PropertyKeyId) -> Result<usize, usize> {
		self.entries.binary_search_by_key(&key, |(key, _)| *key)
	}
}

impl Debug for PropertyMap {
	/// Formats `PropertyMap` using the requested formatter.
	fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
		f.debug_map().entries(self.iter()).finish()
	}
}

/// Returns the entry ref result.
const fn entry_ref(entry: &Entry) -> (PropertyKeyId, &Value) {
	(entry.0, &entry.1)
}

impl PartialEq for PropertyMap {
	/// Compares this `PropertyMap` with another value for equality.
	fn eq(&self, other: &Self) -> bool {
		self.entries == other.entries
	}
}

impl Eq for PropertyMap {}

impl PartialOrd for PropertyMap {
	/// Compares this `PropertyMap` with another value when they are ordered.
	fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
		Some(self.cmp(other))
	}
}

impl Ord for PropertyMap {
	/// Compares this `PropertyMap` with another value using its ordering
	/// contract.
	fn cmp(&self, other: &Self) -> Ordering {
		self.entries.cmp(&other.entries)
	}
}

impl Hash for PropertyMap {
	/// Feeds this `PropertyMap` into the supplied hasher.
	fn hash<H: Hasher>(&self, state: &mut H) {
		self.entries.hash(state);
	}
}

impl FromIterator<(PropertyKeyId, Value)> for PropertyMap {
	/// Builds this `PropertyMap` from the supplied elements.
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

	/// Iterates over borrowed elements without consuming the collection.
	fn into_iter(self) -> Self::IntoIter {
		self.iter()
	}
}

/// Iterator over interned property keys and their borrowed runtime values.
pub struct PropertyIter<'a> {
	/// Inner associated with this property iter.
	inner: SliceIter<'a, Entry>,
}

impl<'a> Iterator for PropertyIter<'a> {
	type Item = (PropertyKeyId, &'a Value);

	/// Advances this `PropertyIter<'a>` and returns its next element, if any.
	fn next(&mut self) -> Option<Self::Item> {
		self.inner.next().map(entry_ref)
	}

	/// Returns lower and optional upper bounds on the remaining element count.
	fn size_hint(&self) -> (usize, Option<usize>) {
		self.inner.size_hint()
	}
}

impl DoubleEndedIterator for PropertyIter<'_> {
	/// Advances this `PropertyIter<'_>` from the back and returns its next
	/// element, if any.
	fn next_back(&mut self) -> Option<Self::Item> {
		self.inner.next_back().map(entry_ref)
	}
}

impl ExactSizeIterator for PropertyIter<'_> {
	/// Returns the logical element count of this `PropertyIter<'_>`.
	fn len(&self) -> usize {
		self.inner.len()
	}
}

impl FusedIterator for PropertyIter<'_> {}

#[cfg(test)]
/// Regression tests for property map behavior.
mod tests {
	use std::{
		collections::hash_map::DefaultHasher,
		hash::{Hash, Hasher},
		mem::size_of,
	};

	use super::{PropertyKeyId, PropertyMap};

	/// Verifies keeps entries sorted and replaces existing values.
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

	/// Verifies empty map is pointer sized and does not reserve storage.
	#[test]
	fn empty_map_is_pointer_sized_and_does_not_reserve_storage() {
		let properties = PropertyMap::new();
		assert_eq!(size_of::<PropertyMap>(), size_of::<usize>());
		assert!(properties.is_empty());
		assert_send_sync::<PropertyMap>();
	}

	/// Verifies removal preserves order and clear preserves validity.
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

	/// Verifies equality order and hash ignore insertion order.
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

	/// Verifies clone is independent.
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

	/// Verifies from iterator uses last duplicate value.
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

	/// Returns assert send sync for this module.
	fn assert_send_sync<T: Send + Sync>() {}
}
