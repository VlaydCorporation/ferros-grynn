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
pub struct Text(CompactString);

impl Text {
	pub fn new<Str: AsRef<str>>(value: Str) -> Self {
		Self(CompactString::new(value))
	}

	pub const fn as_inner(&self) -> &CompactString {
		&self.0
	}

	pub fn into_inner(self) -> CompactString {
		self.0
	}

	pub fn as_str(&self) -> &str {
		self.0.as_str()
	}

	pub fn into_string(self) -> String {
		self.0.into_string()
	}

	pub fn len(&self) -> usize {
		self.0.len()
	}

	pub fn is_empty(&self) -> bool {
		self.0.is_empty()
	}

	pub fn push(&mut self, value: char) {
		self.0.push(value);
	}

	pub fn push_str(&mut self, value: &str) {
		self.0.push_str(value);
	}

	pub fn clear(&mut self) {
		self.0.clear();
	}

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

	fn deref(&self) -> &Self::Target {
		self.as_str()
	}
}

impl AsRef<str> for Text {
	fn as_ref(&self) -> &str {
		self.as_str()
	}
}

impl Borrow<str> for Text {
	fn borrow(&self) -> &str {
		self.as_str()
	}
}

impl Display for Text {
	fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
		f.write_str(self.as_str())
	}
}

impl PartialEq<str> for Text {
	fn eq(&self, other: &str) -> bool {
		self.as_str() == other
	}
}

impl PartialEq<&str> for Text {
	fn eq(&self, other: &&str) -> bool {
		self.as_str() == *other
	}
}

impl PartialEq<Text> for str {
	fn eq(&self, other: &Text) -> bool {
		self == other.as_str()
	}
}

impl PartialEq<Text> for &str {
	fn eq(&self, other: &Text) -> bool {
		*self == other.as_str()
	}
}

impl PartialEq<String> for Text {
	fn eq(&self, other: &String) -> bool {
		self.as_str() == other
	}
}

impl PartialEq<Text> for String {
	fn eq(&self, other: &Text) -> bool {
		self == other.as_str()
	}
}

impl FromStr for Text {
	type Err = Infallible;

	fn from_str(value: &str) -> Result<Self, Self::Err> {
		Ok(Self::from(value))
	}
}

impl From<CompactString> for Text {
	fn from(value: CompactString) -> Self {
		Self(value)
	}
}

impl From<Text> for CompactString {
	fn from(value: Text) -> Self {
		value.0
	}
}

impl From<String> for Text {
	fn from(value: String) -> Self {
		Self(value.into())
	}
}

impl From<Text> for String {
	fn from(value: Text) -> Self {
		value.into_string()
	}
}

impl From<&str> for Text {
	fn from(value: &str) -> Self {
		Self(value.into())
	}
}

impl From<&String> for Text {
	fn from(value: &String) -> Self {
		Self::from(value.as_str())
	}
}

impl From<char> for Text {
	fn from(value: char) -> Self {
		let mut result = Self::default();
		result.push(value);
		result
	}
}

impl FromIterator<char> for Text {
	fn from_iter<I: IntoIterator<Item = char>>(iter: I) -> Self {
		let mut result = Self::default();
		result.extend(iter);
		result
	}
}

impl Extend<char> for Text {
	fn extend<I: IntoIterator<Item = char>>(&mut self, iter: I) {
		for value in iter {
			self.push(value);
		}
	}
}

#[cfg(test)]
mod tests {
	use compact_str::CompactString;

	use super::Text;
	use crate::gql_status::Domain;

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

	#[test]
	fn exposes_controlled_utf8_mutation() {
		let mut value = Text::from("граф");
		value.push('ы');
		value.push_str("!");
		assert_eq!(value.as_str(), "графы!");

		let error = value.truncate(1).unwrap_err();
		assert_eq!(error.status().code().to_string(), "FG-22F05");
		assert_eq!(error.status().domain(), Domain::Value);
		assert_eq!(
			error.status().message().as_deref(),
			Some("Byte index 1 is not a UTF-8 character boundary for a string of 11 bytes."),
		);
		value.truncate("граф".len()).unwrap();
		assert_eq!(value.as_str(), "граф");
		value.clear();
		assert!(value.is_empty());
	}

	#[test]
	fn works_as_a_borrowed_str_key() {
		let mut values = std::collections::BTreeMap::new();
		values.insert(Text::from("name"), 42);
		assert_eq!(values.get("name"), Some(&42));
	}
}
