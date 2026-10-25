use std::slice::Iter;

/// Owned binary value exposed through a borrowed byte slice.
#[derive(Debug, Default, Clone, PartialOrd, PartialEq, Eq, Ord, Hash)]
#[repr(transparent)]
pub struct Bytes(
	/// Owned binary bytes.
	Vec<u8>,
);

impl Bytes {
	/// Constructs `Bytes` from the supplied initial state.
	pub fn new<IntoVec: Into<Vec<u8>>>(bytes: IntoVec) -> Self {
		Self(bytes.into())
	}

	/// Returns the logical element count of this `Bytes`.
	pub fn len(&self) -> usize {
		self.0.len()
	}

	/// Returns the retained allocation size, including spare byte capacity.
	pub(crate) fn estimated_heap_bytes(&self) -> usize {
		self.0.capacity()
	}

	/// Reports whether this `Bytes` contains no logical elements.
	pub fn is_empty(&self) -> bool {
		self.0.is_empty()
	}

	/// Borrows the elements as a contiguous slice.
	pub fn as_slice(&self) -> &[u8] {
		&self.0
	}

	/// Borrows the elements of this `Bytes` for iteration.
	pub fn iter(&self) -> Iter<'_, u8> {
		self.0.iter()
	}

	/// Consumes the collection and returns its owned element vector.
	pub fn into_vec(self) -> Vec<u8> {
		self.0
	}
}

impl From<Vec<u8>> for Bytes {
	/// Converts `Vec<u8>` into `Bytes`.
	fn from(value: Vec<u8>) -> Self {
		Self(value)
	}
}

impl From<&[u8]> for Bytes {
	/// Converts `&[u8]` into `Bytes`.
	fn from(value: &[u8]) -> Self {
		Self(value.to_vec())
	}
}

impl<const N: usize> From<[u8; N]> for Bytes {
	/// Converts `[u8; N]` into `Bytes`.
	fn from(value: [u8; N]) -> Self {
		Self(value.into())
	}
}

impl From<Bytes> for Vec<u8> {
	/// Converts `Bytes` into `Vec<u8>`.
	fn from(value: Bytes) -> Self {
		value.0
	}
}

impl<'a> IntoIterator for &'a Bytes {
	type IntoIter = Iter<'a, u8>;
	type Item = &'a u8;

	/// Iterates over borrowed elements without consuming the collection.
	fn into_iter(self) -> Self::IntoIter {
		self.iter()
	}
}

#[cfg(test)]
/// Regression tests for bytes behavior.
mod tests {
	use super::Bytes;

	/// Verifies exposes only byte oriented api.
	#[test]
	fn exposes_only_byte_oriented_api() {
		let bytes = Bytes::from([1, 2, 3]);
		assert_eq!(bytes.as_slice(), &[1, 2, 3]);
		assert_eq!(bytes.len(), 3);
	}
}
