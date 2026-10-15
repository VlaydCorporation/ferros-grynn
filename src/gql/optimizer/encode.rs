//! Versioned canonical hashing for query, parameter, and plan identities.

use std::hash::{Hash, Hasher};

use rapidhash::v3::rapidhash_v3;

/// Byte collector with platform-independent integer encoding.
///
/// The standard [`Hash`] implementations remain responsible for semantic
/// structure. This writer removes the platform-dependent `usize` and native
/// endian behavior of general-purpose hashers, then hashes the resulting
/// versioned bytes with RapidHash V3.
#[derive(Debug, Default)]
pub struct CanonicalHasher {
	/// Bytes associated with this canonical hasher.
	bytes: Vec<u8>,
}

impl CanonicalHasher {
	/// Creates a writer with a version and a domain separator.
	pub(crate) fn versioned(version: u16, domain: &[u8]) -> Self {
		let mut writer = Self::default();
		writer.bytes.extend_from_slice(b"FGCP");
		writer.bytes.extend_from_slice(&version.to_le_bytes());
		writer.bytes.extend_from_slice(&(domain.len() as u64).to_le_bytes());
		writer.bytes.extend_from_slice(domain);
		writer
	}

	/// Returns the canonical bytes accumulated so far.
	#[cfg(test)]
	pub(crate) fn as_bytes(&self) -> &[u8] {
		&self.bytes
	}

	/// Performs the integer operation.
	fn integer(&mut self, tag: u8, bytes: &[u8]) {
		self.bytes.push(tag);
		self.bytes.extend_from_slice(bytes);
	}
}

impl Hasher for CanonicalHasher {
	/// Performs the finish operation for `CanonicalHasher`.
	fn finish(&self) -> u64 {
		rapidhash_v3(&self.bytes)
	}

	/// Performs the write operation for `CanonicalHasher`.
	fn write(&mut self, bytes: &[u8]) {
		self.bytes.push(0);
		self.bytes.extend_from_slice(&(bytes.len() as u64).to_le_bytes());
		self.bytes.extend_from_slice(bytes);
	}

	/// Writes u8 for `CanonicalHasher`.
	fn write_u8(&mut self, i: u8) {
		self.integer(1, &[i]);
	}

	/// Writes u16 for `CanonicalHasher`.
	fn write_u16(&mut self, i: u16) {
		self.integer(2, &i.to_le_bytes());
	}

	/// Writes u32 for `CanonicalHasher`.
	fn write_u32(&mut self, i: u32) {
		self.integer(3, &i.to_le_bytes());
	}

	/// Writes u64 for `CanonicalHasher`.
	fn write_u64(&mut self, i: u64) {
		self.integer(4, &i.to_le_bytes());
	}

	/// Writes u128 for `CanonicalHasher`.
	fn write_u128(&mut self, i: u128) {
		self.integer(5, &i.to_le_bytes());
	}

	/// Writes usize for `CanonicalHasher`.
	fn write_usize(&mut self, i: usize) {
		self.integer(6, &(i as u64).to_le_bytes());
	}

	/// Writes i8 for `CanonicalHasher`.
	fn write_i8(&mut self, i: i8) {
		self.integer(7, &i.to_le_bytes());
	}

	/// Writes i16 for `CanonicalHasher`.
	fn write_i16(&mut self, i: i16) {
		self.integer(8, &i.to_le_bytes());
	}

	/// Writes i32 for `CanonicalHasher`.
	fn write_i32(&mut self, i: i32) {
		self.integer(9, &i.to_le_bytes());
	}

	/// Writes i64 for `CanonicalHasher`.
	fn write_i64(&mut self, i: i64) {
		self.integer(10, &i.to_le_bytes());
	}

	/// Writes i128 for `CanonicalHasher`.
	fn write_i128(&mut self, i: i128) {
		self.integer(11, &i.to_le_bytes());
	}

	/// Writes isize for `CanonicalHasher`.
	fn write_isize(&mut self, i: isize) {
		self.integer(12, &(i as i64).to_le_bytes());
	}
}

/// Hashes one value with a versioned domain separator.
pub fn canonical_hash<T: Hash + ?Sized>(version: u16, domain: &[u8], value: &T) -> u64 {
	let mut writer = CanonicalHasher::versioned(version, domain);
	value.hash(&mut writer);
	writer.finish()
}
