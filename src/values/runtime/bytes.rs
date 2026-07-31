use std::slice::Iter;

#[derive(Debug, Default, Clone, PartialOrd, PartialEq, Eq, Ord, Hash)]
#[repr(transparent)]
pub struct Bytes(Vec<u8>);

impl Bytes {
	pub fn new(bytes: impl Into<Vec<u8>>) -> Self {
		Self(bytes.into())
	}

	pub fn len(&self) -> usize {
		self.0.len()
	}

	pub fn is_empty(&self) -> bool {
		self.0.is_empty()
	}

	pub fn as_slice(&self) -> &[u8] {
		&self.0
	}

	pub fn iter(&self) -> Iter<'_, u8> {
		self.0.iter()
	}

	pub fn into_vec(self) -> Vec<u8> {
		self.0
	}
}

impl From<Vec<u8>> for Bytes {
	fn from(value: Vec<u8>) -> Self {
		Self(value)
	}
}

impl From<&[u8]> for Bytes {
	fn from(value: &[u8]) -> Self {
		Self(value.to_vec())
	}
}

impl<const N: usize> From<[u8; N]> for Bytes {
	fn from(value: [u8; N]) -> Self {
		Self(value.into())
	}
}

impl From<Bytes> for Vec<u8> {
	fn from(value: Bytes) -> Self {
		value.0
	}
}

impl<'a> IntoIterator for &'a Bytes {
	type IntoIter = Iter<'a, u8>;
	type Item = &'a u8;

	fn into_iter(self) -> Self::IntoIter {
		self.iter()
	}
}

#[cfg(test)]
mod tests {
	use super::Bytes;

	#[test]
	fn exposes_only_byte_oriented_api() {
		let bytes = Bytes::from([1, 2, 3]);
		assert_eq!(bytes.as_slice(), &[1, 2, 3]);
		assert_eq!(bytes.len(), 3);
	}
}
