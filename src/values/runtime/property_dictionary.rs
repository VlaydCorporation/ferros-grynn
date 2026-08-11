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
	ids: RapidHashMap<Text, PropertyKeyId>,
	names: Vec<Text>,
}

impl PropertyDictionary {
	pub fn new() -> Self {
		Self::default()
	}

	pub fn with_capacity(capacity: usize) -> Self {
		Self {
			ids: RapidHashMap::with_capacity(capacity),
			names: Vec::with_capacity(capacity),
		}
	}

	pub const fn len(&self) -> usize {
		self.names.len()
	}

	pub const fn is_empty(&self) -> bool {
		self.names.is_empty()
	}

	pub fn get(&self, name: &str) -> Option<PropertyKeyId> {
		self.ids.get(name).copied()
	}

	pub fn resolve(&self, id: PropertyKeyId) -> Option<&Text> {
		self.names.get(id.get() as usize)
	}

	pub fn intern<IntoText: Into<Text>>(&mut self, name: IntoText) -> FerrosGrynnResult<PropertyKeyId> {
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

	pub fn iter(&self) -> impl ExactSizeIterator<Item = (PropertyKeyId, &Text)> {
		self.names.iter().enumerate().map(|(id, name)| (PropertyKeyId::new(id as u32), name))
	}
}

#[cfg(test)]
mod tests {
	use super::PropertyDictionary;

	#[test]
	fn assigns_dense_ids_and_resolves_both_directions() {
		let mut dictionary = PropertyDictionary::new();
		let name = dictionary.intern("name").unwrap();
		let age = dictionary.intern("age").unwrap();

		assert_eq!(name.get(), 0);
		assert_eq!(age.get(), 1);
		assert_eq!(dictionary.get("name"), Some(name));
		assert_eq!(dictionary.resolve(age).map(|value| value.as_str()), Some("age"));
		assert_eq!(dictionary.resolve(2_u32.into()), None);
	}

	#[test]
	fn interning_is_idempotent_and_iteration_uses_id_order() {
		let mut dictionary = PropertyDictionary::new();
		let first = dictionary.intern("weight").unwrap();
		let second = dictionary.intern(String::from("weight")).unwrap();
		dictionary.intern("color").unwrap();

		assert_eq!(first, second);
		assert_eq!(dictionary.len(), 2);
		assert_eq!(dictionary.iter().map(|(_, name)| name.as_str()).collect::<Vec<_>>(), [
			"weight", "color"
		],);
	}
}
