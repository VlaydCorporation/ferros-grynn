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

	/// Borrows the elements of this `PropertyDictionary` for iteration.
	pub fn iter(&self) -> impl ExactSizeIterator<Item = (PropertyKeyId, &Text)> {
		self.names.iter().enumerate().map(|(id, name)| (PropertyKeyId::new(id as u32), name))
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
}
