use rapidhash::{HashMapExt, RapidHashMap};

use super::{PropertyKeyId, Text};
use crate::gql_status::{FerrosGrynnError, FerrosGrynnResult};

/// Bidirectional runtime dictionary for graph property keys.
///
/// IDs are assigned densely in insertion order. Persistence code may rebuild
/// the dictionary in the same order, but the on-disk dictionary remains owned
/// by the storage layer.
#[derive(Debug, Default, Clone)]
pub struct PropertyDictionary {
	/// Ids associated with this property dictionary.
	ids: RapidHashMap<Text, PropertyKeyId>,
	/// Names associated with this property dictionary.
	names: Vec<Text>,
}

impl PropertyDictionary {
	/// Constructs `PropertyDictionary` from the supplied initial state.
	pub fn new() -> Self {
		Self::default()
	}

	/// Creates an empty collection with the requested initial capacity.
	pub fn with_capacity(capacity: usize) -> Self {
		Self {
			ids: RapidHashMap::with_capacity(capacity),
			names: Vec::with_capacity(capacity),
		}
	}

	/// Returns the logical element count of this `PropertyDictionary`.
	pub const fn len(&self) -> usize {
		self.names.len()
	}

	/// Reports whether this `PropertyDictionary` contains no logical elements.
	pub const fn is_empty(&self) -> bool {
		self.names.is_empty()
	}

	/// Looks up the requested entry without modifying the collection.
	pub fn get(&self, name: &str) -> Option<PropertyKeyId> {
		self.ids.get(name).copied()
	}

	/// Returns resolve for `PropertyDictionary`.
	pub fn resolve(&self, id: PropertyKeyId) -> Option<&Text> {
		self.names.get(id.get() as usize)
	}

	/// Returns intern for `PropertyDictionary`.
	///
	/// # Errors
	///
	/// Returns `property_dictionary_capacity_exceeded` diagnostics and
	/// propagates failures from called operations.
	pub fn intern<IntoText: Into<Text>>(
		&mut self,
		name: IntoText,
	) -> FerrosGrynnResult<PropertyKeyId> {
		let name = name.into();
		if let Some(id) = self.ids.get(name.as_str()) {
			return Ok(*id);
		}

		let raw = u32::try_from(self.names.len())
			.map_err(|_err| FerrosGrynnError::property_dictionary_capacity_exceeded())?;
		let id = PropertyKeyId::new(raw);
		self.names.push(name.clone());
		self.ids.insert(name, id);
		Ok(id)
	}

	/// Restores one exact append-only dictionary entry.
	pub(crate) fn restore(&mut self, id: PropertyKeyId, name: Text) -> FerrosGrynnResult<()> {
		if let Some(existing) = self.get(name.as_str()) {
			return if existing == id {
				Ok(())
			} else {
				Err(FerrosGrynnError::invalid_storage_file(
					"property name is bound to a different key",
				))
			};
		}
		let expected = u32::try_from(self.names.len())
			.map_err(|_error| FerrosGrynnError::property_dictionary_capacity_exceeded())?;
		if id.get() != expected {
			return Err(FerrosGrynnError::invalid_storage_file(
				"property dictionary key is not the next dense identity",
			));
		}
		self.names.push(name.clone());
		self.ids.insert(name, id);
		Ok(())
	}

	/// Bounds new clone backing, including reserved hash buckets but excluding
	/// the source vector's spare capacity.
	///
	/// # Errors
	/// Returns an overflow diagnostic when the reservation cannot fit usize.
	pub(crate) fn estimated_clone_heap_bytes(&self) -> FerrosGrynnResult<usize> {
		let overflow = || FerrosGrynnError::operation_overflow("property dictionary clone bytes");
		let capacity = self.ids.capacity();
		let buckets = if capacity == 0 {
			0
		} else {
			capacity
				.checked_add(1)
				.and_then(usize::checked_next_power_of_two)
				.ok_or_else(overflow)?
		};
		let map = if buckets == 0 {
			0
		} else {
			// std hashbrown 0.17 preserves buckets on clone. This append-only
			// map recovers them from capacity; recheck on toolchain upgrades.
			let align = align_of::<(Text, PropertyKeyId)>().max(16);
			let entries =
				buckets.checked_mul(size_of::<(Text, PropertyKeyId)>()).ok_or_else(overflow)?;
			let aligned = entries.checked_add(align - 1).ok_or_else(overflow)? & !(align - 1);
			// Current control groups need at most sixteen trailing bytes.
			aligned
				.checked_add(buckets)
				.and_then(|bytes| bytes.checked_add(16))
				.ok_or_else(overflow)?
		};
		let vector = self.names.len().checked_mul(size_of::<Text>()).ok_or_else(overflow)?;
		self.names.iter().try_fold(map.checked_add(vector).ok_or_else(overflow)?, |bytes, name| {
			name.estimated_clone_heap_bytes()
				.checked_mul(2)
				.and_then(|names| bytes.checked_add(names))
				.ok_or_else(overflow)
		})
	}

	/// Borrows the elements of this `PropertyDictionary` for iteration.
	pub fn iter(&self) -> impl ExactSizeIterator<Item = (PropertyKeyId, &Text)> {
		self.names.iter().enumerate().map(|(id, name)| (PropertyKeyId::new(id as u32), name))
	}
}

/// Operations shared with the engine and value codecs.
pub trait PropertyDictionaryInternal: Sized {
	/// Restores one exact append-only dictionary entry.
	///
	/// # Errors
	/// Propagates validation, resource-admission or callback errors from the
	/// operation.
	fn restore(&mut self, id: PropertyKeyId, name: Text) -> FerrosGrynnResult<()>;
	/// Bounds new clone backing, including reserved hash buckets but excluding
	/// the source vector's spare capacity.
	///
	/// # Errors
	/// Returns an overflow diagnostic when the reservation cannot fit usize.
	fn estimated_clone_heap_bytes(&self) -> FerrosGrynnResult<usize>;
}
impl PropertyDictionaryInternal for PropertyDictionary {
	fn restore(&mut self, id: PropertyKeyId, name: Text) -> FerrosGrynnResult<()> {
		<PropertyDictionary>::restore(self, id, name)
	}

	fn estimated_clone_heap_bytes(&self) -> FerrosGrynnResult<usize> {
		<PropertyDictionary>::estimated_clone_heap_bytes(self)
	}
}

#[cfg(test)]
/// Regression tests for property dictionary behavior.
mod tests {
	use super::PropertyDictionary;
	#[cfg(test)]
	use crate::test_assertions::AssertResult as _;

	/// Verifies assigns dense ids and resolves both directions.
	#[test]
	fn assigns_dense_ids_and_resolves_both_directions() {
		let mut dictionary = PropertyDictionary::new();
		let name = dictionary.intern("name").assert_ok();
		let age = dictionary.intern("age").assert_ok();

		assert_eq!(name.get(), 0);
		assert_eq!(age.get(), 1);
		assert_eq!(dictionary.get("name"), Some(name));
		assert_eq!(dictionary.resolve(age).map(|value| value.as_str()), Some("age"));
		assert_eq!(dictionary.resolve(2_u32.into()), None);
	}

	/// Verifies interning is idempotent and iteration uses id order.
	#[test]
	fn interning_is_idempotent_and_iteration_uses_id_order() {
		let mut dictionary = PropertyDictionary::new();
		let first = dictionary.intern("weight").assert_ok();
		let second = dictionary.intern(String::from("weight")).assert_ok();
		dictionary.intern("color").assert_ok();

		assert_eq!(first, second);
		assert_eq!(dictionary.len(), 2);
		assert_eq!(dictionary.iter().map(|(_, name)| name.as_str()).collect::<Vec<_>>(), [
			"weight", "color"
		],);
	}
	/// Hash clones preserve reservation even when empty; vector clones keep
	/// only their logical elements.
	#[test]
	fn clone_admission_keeps_reserved_map_but_releases_vector_spare() {
		assert_eq!(PropertyDictionary::new().estimated_clone_heap_bytes().assert_ok(), 0);
		let mut source = PropertyDictionary::with_capacity(128);
		let empty = source.clone();
		assert!(source.ids.capacity() >= 128);
		assert_eq!(empty.ids.capacity(), source.ids.capacity());
		assert_eq!(empty.names.capacity(), 0);
		let map = source.estimated_clone_heap_bytes().assert_ok();
		assert!(map > 128 * size_of::<(super::Text, super::PropertyKeyId)>());
		source.intern("p").assert_ok();
		let retained = source.estimated_clone_heap_bytes().assert_ok();
		assert_eq!(retained - map, size_of::<super::Text>());
		source.names.reserve(512);
		assert!(source.names.capacity() > source.names.len());
		assert_eq!(source.estimated_clone_heap_bytes().assert_ok(), retained);
		let cloned = source.clone();
		assert_eq!(cloned.ids.capacity(), source.ids.capacity());
		assert_eq!(cloned.names.capacity(), cloned.names.len());
		assert_eq!(cloned.get("p"), source.get("p"));
		assert_eq!(cloned.estimated_clone_heap_bytes().assert_ok(), retained);
	}

	/// The new clone owns two tight Text copies, not the source's extra
	/// capacity.
	#[test]
	fn clone_admission_tracks_owned_text_without_source_spare() {
		let mut inline = PropertyDictionary::with_capacity(4);
		inline.intern("p").assert_ok();
		let mut name = String::with_capacity(4096);
		name.push_str("property_with_a_heap_name_beyond_the_inline_text_buffer");
		let mut source = PropertyDictionary::with_capacity(4);
		source.intern(name).assert_ok();
		let cloned = source.clone();
		assert_eq!(cloned.ids.capacity(), inline.ids.capacity());
		assert_eq!(cloned.names.capacity(), 1);
		let new_text = cloned
			.ids
			.keys()
			.chain(cloned.names.iter())
			.map(super::Text::estimated_heap_bytes)
			.sum::<usize>();
		let source_text = source
			.ids
			.keys()
			.chain(source.names.iter())
			.map(super::Text::estimated_heap_bytes)
			.sum::<usize>();
		assert!(new_text > 0 && source_text > new_text);
		assert_eq!(
			source.estimated_clone_heap_bytes().assert_ok()
				- inline.estimated_clone_heap_bytes().assert_ok(),
			new_text
		);
		assert_eq!(
			source.estimated_clone_heap_bytes().assert_ok(),
			cloned.estimated_clone_heap_bytes().assert_ok()
		);
		assert_eq!(source.names, cloned.names);
	}
}
