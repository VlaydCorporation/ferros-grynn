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
pub(crate) struct CanonicalHasher {
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

	fn integer(&mut self, tag: u8, bytes: &[u8]) {
		self.bytes.push(tag);
		self.bytes.extend_from_slice(bytes);
	}
}

impl Hasher for CanonicalHasher {
	fn finish(&self) -> u64 {
		rapidhash_v3(&self.bytes)
	}

	fn write(&mut self, bytes: &[u8]) {
		self.bytes.push(0);
		self.bytes.extend_from_slice(&(bytes.len() as u64).to_le_bytes());
		self.bytes.extend_from_slice(bytes);
	}

	fn write_u8(&mut self, value: u8) {
		self.integer(1, &[value]);
	}

	fn write_u16(&mut self, value: u16) {
		self.integer(2, &value.to_le_bytes());
	}

	fn write_u32(&mut self, value: u32) {
		self.integer(3, &value.to_le_bytes());
	}

	fn write_u64(&mut self, value: u64) {
		self.integer(4, &value.to_le_bytes());
	}

	fn write_u128(&mut self, value: u128) {
		self.integer(5, &value.to_le_bytes());
	}

	fn write_usize(&mut self, value: usize) {
		self.integer(6, &(value as u64).to_le_bytes());
	}

	fn write_i8(&mut self, value: i8) {
		self.integer(7, &value.to_le_bytes());
	}

	fn write_i16(&mut self, value: i16) {
		self.integer(8, &value.to_le_bytes());
	}

	fn write_i32(&mut self, value: i32) {
		self.integer(9, &value.to_le_bytes());
	}

	fn write_i64(&mut self, value: i64) {
		self.integer(10, &value.to_le_bytes());
	}

	fn write_i128(&mut self, value: i128) {
		self.integer(11, &value.to_le_bytes());
	}

	fn write_isize(&mut self, value: isize) {
		self.integer(12, &(value as i64).to_le_bytes());
	}
}

/// Hashes one value with a versioned domain separator.
pub(crate) fn canonical_hash<T: Hash + ?Sized>(version: u16, domain: &[u8], value: &T) -> u64 {
	let mut writer = CanonicalHasher::versioned(version, domain);
	value.hash(&mut writer);
	writer.finish()
}
