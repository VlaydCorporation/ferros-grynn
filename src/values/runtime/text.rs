use std::{
	borrow::Borrow,
	convert::Infallible,
	fmt::{self, Display, Formatter},
	ops::Deref,
	str::FromStr,
};

use compact_str::CompactString;

use crate::gql_status::{FerrosGrynnError, FerrosGrynnResult};

/// A UTF-8 string used by the Ferros-Grynn runtime.
///
/// The newtype keeps the storage strategy private while providing a stable
/// place for database-specific string behavior.
#[derive(Debug, Default, Clone, PartialOrd, PartialEq, Eq, Ord, Hash)]
#[repr(transparent)]
pub struct Text(
	/// A UTF-8 string used by the Ferros-Grynn runtime. The newtype keeps the
	/// storage strategy private while providing a stable place for
	/// database-specific string behavior.
	CompactString,
);

impl Text {
	/// Bounds a borrowed UTF-8 copy, including small heaps and wide-capacity
	/// metadata.
	pub(crate) fn estimated_copy_heap_bytes(length: usize) -> usize {
		if length <= size_of::<Self>() {
			0
		} else {
			// The extra word also covers compact_str's large 32-bit capacity
			// header.
			length.max(size_of::<Self>() + size_of::<usize>()).saturating_add(size_of::<usize>())
		}
	}

	/// Counts retained heap storage without charging the inline representation
	/// twice.
	pub(crate) fn estimated_heap_bytes(&self) -> usize {
		if self.0.is_heap_allocated() {
			self.0.capacity()
		} else {
			0
		}
	}

	/// Estimates the new clone's allocation independently of source spare
	/// capacity.
	pub(crate) fn estimated_clone_heap_bytes(&self) -> usize {
		if !self.0.is_heap_allocated() || self.0.len() <= size_of::<Self>() {
			0
		} else {
			// compact_str 0.10 rebuilds heap clones from length, with a minimum
			// capacity-word reserve; recheck this bound when upgrading it.
			self.0.len().max(size_of::<Self>() + size_of::<usize>())
		}
	}

	/// Constructs `Text` from the supplied initial state.
	pub fn new<Str: AsRef<str>>(value: Str) -> Self {
		Self(CompactString::new(value))
	}

	/// Borrows the wrapped inner representation.
	pub const fn as_inner(&self) -> &CompactString {
		&self.0
	}

	/// Returns the wrapped inner representation.
	pub fn into_inner(self) -> CompactString {
		self.0
	}

	/// Borrows str for `Text`.
	pub fn as_str(&self) -> &str {
		self.0.as_str()
	}

	/// Converts this value into string for `Text`.
	pub fn into_string(self) -> String {
		self.0.into_string()
	}

	/// Returns the logical element count of this `Text`.
	pub fn len(&self) -> usize {
		self.0.len()
	}

	/// Reports whether this `Text` contains no logical elements.
	pub fn is_empty(&self) -> bool {
		self.0.is_empty()
	}

	/// Appends one value to the collection.
	pub fn push(&mut self, value: char) {
		self.0.push(value);
	}

	/// Pushes str for `Text`.
	pub fn push_str(&mut self, value: &str) {
		self.0.push_str(value);
	}

	/// Removes all logical elements from the collection.
	pub fn clear(&mut self) {
		self.0.clear();
	}

	/// Returns truncate for `Text`.
	///
	/// # Errors
	///
	/// Returns `invalid_text_boundary` diagnostics.
	pub fn truncate(&mut self, new_len: usize) -> FerrosGrynnResult<()> {
		if !self.0.is_char_boundary(new_len) {
			return Err(FerrosGrynnError::invalid_text_boundary(new_len as u64, self.len() as u64));
		}
		self.0.truncate(new_len);
		Ok(())
	}
}

impl Deref for Text {
	type Target = str;

	/// Borrows the underlying value represented by this `Text`.
	fn deref(&self) -> &Self::Target {
		self.as_str()
	}
}

impl AsRef<str> for Text {
	/// Borrows the underlying representation without consuming this `Text`.
	fn as_ref(&self) -> &str {
		self.as_str()
	}
}

impl Borrow<str> for Text {
	/// Borrows the key or value represented by this `Text`.
	fn borrow(&self) -> &str {
		self.as_str()
	}
}

impl Display for Text {
	/// Formats `Text` using the requested formatter.
	fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
		f.write_str(self.as_str())
	}
}

impl PartialEq<str> for Text {
	/// Compares this `Text` with another value for equality.
	fn eq(&self, other: &str) -> bool {
		self.as_str() == other
	}
}

impl PartialEq<&str> for Text {
	/// Compares this `Text` with another value for equality.
	fn eq(&self, other: &&str) -> bool {
		self.as_str() == *other
	}
}

impl PartialEq<Text> for str {
	/// Compares this `str` with another value for equality.
	fn eq(&self, other: &Text) -> bool {
		self == other.as_str()
	}
}

impl PartialEq<Text> for &str {
	/// Compares this `&str` with another value for equality.
	fn eq(&self, other: &Text) -> bool {
		*self == other.as_str()
	}
}

impl PartialEq<String> for Text {
	/// Compares this `Text` with another value for equality.
	fn eq(&self, other: &String) -> bool {
		self.as_str() == other
	}
}

impl PartialEq<Text> for String {
	/// Compares this `String` with another value for equality.
	fn eq(&self, other: &Text) -> bool {
		self == other.as_str()
	}
}

impl FromStr for Text {
	type Err = Infallible;

	/// Constructs from str for `Text`.
	fn from_str(value: &str) -> Result<Self, Self::Err> {
		Ok(Self::from(value))
	}
}

impl From<CompactString> for Text {
	/// Converts `CompactString` into `Text`.
	fn from(value: CompactString) -> Self {
		Self(value)
	}
}

impl From<Text> for CompactString {
	/// Converts `Text` into `CompactString`.
	fn from(value: Text) -> Self {
		value.0
	}
}

impl From<String> for Text {
	/// Converts `String` into `Text`.
	fn from(value: String) -> Self {
		Self(value.into())
	}
}

impl From<Text> for String {
	/// Converts `Text` into `String`.
	fn from(value: Text) -> Self {
		value.into_string()
	}
}

impl From<&str> for Text {
	/// Converts `&str` into `Text`.
	fn from(value: &str) -> Self {
		Self(value.into())
	}
}

impl From<&String> for Text {
	/// Converts `&String` into `Text`.
	fn from(value: &String) -> Self {
		Self::from(value.as_str())
	}
}

impl From<char> for Text {
	/// Converts `char` into `Text`.
	fn from(value: char) -> Self {
		let mut result = Self::default();
		result.push(value);
		result
	}
}

impl FromIterator<char> for Text {
	/// Builds this `Text` from the supplied elements.
	fn from_iter<I: IntoIterator<Item = char>>(iter: I) -> Self {
		let mut result = Self::default();
		result.extend(iter);
		result
	}
}

impl Extend<char> for Text {
	/// Adds the supplied elements to this `Text`.
	fn extend<I: IntoIterator<Item = char>>(&mut self, iter: I) {
		for value in iter {
			self.push(value);
		}
	}
}

#[cfg(test)]
/// Regression tests for text behavior.
mod tests {
	use compact_str::CompactString;

	use super::Text;
	use crate::gql_status::Domain;
	#[cfg(test)]
	use crate::test_assertions::AssertResult as _;

	/// Heap-clone admission includes the small-heap minimum, not old spare
	/// capacity.
	#[test]
	fn clone_heap_estimate_matches_inline_small_and_overreserved_sources() {
		for length in [0, 24, 25, 31, 32, 128] {
			for overreserved in [false, true] {
				let source = "x".repeat(length);
				let value = if overreserved {
					let mut inner = CompactString::with_capacity(4096);
					inner.push_str(&source);
					Text::from(inner)
				} else {
					Text::from(source)
				};
				let cloned = value.clone();
				assert_eq!(cloned, value);
				assert_eq!(value.estimated_clone_heap_bytes(), cloned.estimated_heap_bytes());
			}
		}
		let static_value =
			Text::from(CompactString::const_new("a static string longer than the inline buffer"));
		assert_eq!(static_value.estimated_clone_heap_bytes(), 0);
		let cloned = static_value.clone();
		assert_eq!(cloned, static_value);
		assert_eq!(cloned.estimated_heap_bytes(), 0);
	}

	/// Bounds copied text without charging source spare or static storage.
	#[test]
	fn bounded_storage_copies_cover_static_and_spare_text() {
		use crate::values::{
			PropertyKeyId,
			PropertyMap,
			Value,
			storage::{EncodeContext, StorageValue, codec::CanonicalCodec},
		};

		let mut sources = vec![Text::from(CompactString::const_new(
			"a static string longer than the inline buffer",
		))];
		for length in [0, 24, 25, 31, 32, 128] {
			let mut inner = CompactString::with_capacity(4096);
			inner.push_str(&"x".repeat(length));
			sources.push(Text::from(inner));
			sources.push(Text::from("x".repeat(length)));
		}
		for value in sources {
			let clone = value.clone();
			let copy = if value.estimated_clone_heap_bytes() == 0 {
				0
			} else {
				Text::estimated_copy_heap_bytes(value.len())
			};
			assert!(copy >= clone.estimated_heap_bytes());
			let key = PropertyKeyId::new(0);
			let control = StorageValue::Object(vec![(key, StorageValue::String(clone))]);
			let expected = CanonicalCodec::encode(control.as_ref()).assert_ok();
			let maximum = size_of::<(PropertyKeyId, StorageValue)>() + copy + expected.len();
			let mut properties = PropertyMap::new();
			let value = Value::String(value);
			assert_eq!(
				StorageValue::prime_context_bounded(&value, &mut EncodeContext::default(), copy)
					.assert_ok(),
				0
			);
			if copy != 0 {
				assert!(
					StorageValue::prime_context_bounded(
						&value,
						&mut EncodeContext::default(),
						copy - 1
					)
					.is_err()
				);
			}
			properties.insert(key, value);
			let mut context = EncodeContext::default();
			assert!(
				StorageValue::encode_properties_bounded(&properties, &mut context, maximum - 1)
					.is_err()
			);
			let (actual, retained) =
				StorageValue::encode_properties_bounded(&properties, &mut context, maximum)
					.assert_ok();
			assert_eq!(actual, expected);
			assert_eq!(retained, 0);
		}
	}

	/// Verifies round trips through std and compact strings.
	#[test]
	fn round_trips_through_std_and_compact_strings() {
		assert_eq!(std::mem::size_of::<Text>(), std::mem::size_of::<CompactString>(),);

		let value = Text::from(String::from("Ferros-Grynn"));
		assert_eq!(value.as_str(), "Ferros-Grynn");

		let inner = value.clone().into_inner();
		assert_eq!(Text::from(inner.clone()), value);
		assert_eq!(CompactString::from(value.clone()), inner);
		assert_eq!(String::from(value), "Ferros-Grynn");
	}

	/// Verifies exposes controlled utf8 mutation.
	#[test]
	fn exposes_controlled_utf8_mutation() {
		let mut value = Text::from("граф");
		value.push('ы');
		value.push_str("!");
		assert_eq!(value.as_str(), "графы!");

		let error = value.truncate(1).assert_err();
		assert_eq!(error.status().code().to_string(), "FG-22F05");
		assert_eq!(error.status().domain(), Domain::Value);
		assert_eq!(
			error.status().message().as_deref(),
			Some("Byte index 1 is not a UTF-8 character boundary for a string of 11 bytes."),
		);
		value.truncate("граф".len()).assert_ok();
		assert_eq!(value.as_str(), "граф");
		value.clear();
		assert!(value.is_empty());
	}

	/// Verifies works as a borrowed str key.
	#[test]
	fn works_as_a_borrowed_str_key() {
		let mut values = std::collections::BTreeMap::new();
		values.insert(Text::from("name"), 42);
		assert_eq!(values.get("name"), Some(&42));
	}
}
