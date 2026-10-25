//! Bounded historical values and explicit framing shared by Changefeed and
//! LIVE.

/// Shared checked segment headers and whole-commit continuation pages.
pub(in crate::storage) mod pages;

/// Owned qualified values shared by computed history and durable EVENT
/// contexts.
pub(in crate::storage) mod logical_values;

/// Stable identities shared by source images and computed LIVE results.
pub(in crate::storage) mod qualified;
pub(in crate::storage) mod values;

use crate::gql_status::{FerrosGrynnError, FerrosGrynnResult};

/// Hard payload cap shared with the existing frame-WAL transaction ceiling.
const MAX_COMMIT_BYTES: usize = 256 * 1024 * 1024;

/// Aggregate scratch admission before copying or growing a codec buffer.
pub(in crate::storage) struct CodecBudget {
	/// All currently retained input/output and temporary allocations.
	pub(in crate::storage) used: usize,
	/// Caller-provided aggregate cap, independent of persisted lengths.
	pub(in crate::storage) maximum: usize,
}

impl CodecBudget {
	/// Creates a bounded codec scope including its already held inputs.
	pub(in crate::storage) fn new(maximum: usize, retained: usize) -> FerrosGrynnResult<Self> {
		let mut budget = Self {
			used: 0,
			maximum,
		};
		budget.admit(retained)?;
		Ok(budget)
	}

	/// Returns the conservative admission high-water mark for observations.
	pub(in crate::storage) const fn admitted_bytes(&self) -> usize {
		self.used
	}

	/// Checks admission before allocation; no partial object escapes on
	/// failure.
	pub(in crate::storage) fn admit(&mut self, bytes: usize) -> FerrosGrynnResult<()> {
		let used = self
			.used
			.checked_add(bytes)
			.ok_or_else(|| invalid("changefeed codec scratch overflows"))?;
		if used > self.maximum {
			return Err(FerrosGrynnError::storage_decode_limit_exceeded(
				"changefeed codec scratch",
				used as u64,
				self.maximum as u64,
			));
		}
		self.used = used;
		Ok(())
	}
}

/// Checks the format's independent payload ceiling before reading allocations.
pub(in crate::storage) fn checked_payload(bytes: u64) -> FerrosGrynnResult<usize> {
	let bytes = usize::try_from(bytes)
		.map_err(|_error| invalid("changefeed payload length exceeds usize"))?;
	if bytes > MAX_COMMIT_BYTES {
		return Err(FerrosGrynnError::storage_decode_limit_exceeded(
			"changefeed commit bytes",
			bytes as u64,
			MAX_COMMIT_BYTES as u64,
		));
	}
	Ok(bytes)
}

/// A checked cursor into one length-bounded physical payload.
pub(in crate::storage) struct Decoder<'bytes> {
	/// Immutable complete payload.
	pub(in crate::storage) bytes: &'bytes [u8],
	/// Next unread offset; checked before advancing.
	pub(in crate::storage) position: usize,
}

impl<'bytes> Decoder<'bytes> {
	/// Starts decoding at the beginning of the declared payload.
	pub(in crate::storage) const fn new(bytes: &'bytes [u8]) -> Self {
		Self {
			bytes,
			position: 0,
		}
	}

	/// Takes an exact bounded slice without indexing a persisted length
	/// directly.
	pub(in crate::storage) fn take(&mut self, count: usize) -> FerrosGrynnResult<&'bytes [u8]> {
		let end = self
			.position
			.checked_add(count)
			.ok_or_else(|| invalid("changefeed field length overflows"))?;
		let bytes = self
			.bytes
			.get(self.position..end)
			.ok_or_else(|| invalid("changefeed payload is truncated"))?;
		self.position = end;
		Ok(bytes)
	}

	/// Reads one fixed unsigned integer from explicit little-endian bytes.
	pub(in crate::storage) fn u64(&mut self) -> FerrosGrynnResult<u64> {
		Ok(u64::from_le_bytes(
			self.take(8)?.try_into().map_err(|_error| invalid("changefeed u64 is truncated"))?,
		))
	}

	/// Reads one byte after validating the payload boundary.
	pub(in crate::storage) fn u8(&mut self) -> FerrosGrynnResult<u8> {
		Ok(self.take(1)?[0])
	}

	/// Reads one explicit little-endian count or dictionary slot.
	pub(in crate::storage) fn u32(&mut self) -> FerrosGrynnResult<u32> {
		Ok(u32::from_le_bytes(
			self.take(4)?.try_into().map_err(|_error| invalid("changefeed u32 is truncated"))?,
		))
	}

	/// Borrows one capped length-prefixed value without allocating from its
	/// length.
	pub(in crate::storage) fn blob(&mut self) -> FerrosGrynnResult<&'bytes [u8]> {
		let length = checked_payload(self.u64()?)?;
		self.take(length)
	}

	/// Rejects reserved bytes before interpreting following fields.
	pub(in crate::storage) fn zero(&mut self, count: usize) -> FerrosGrynnResult<()> {
		if self.take(count)?.iter().any(|byte| *byte != 0) {
			return Err(invalid("changefeed reserved bytes are nonzero"));
		}
		Ok(())
	}

	/// Rejects noncanonical trailing bytes instead of accepting a prefix
	/// silently.
	pub(in crate::storage) fn finish(self) -> FerrosGrynnResult<()> {
		if self.position != self.bytes.len() {
			return Err(invalid("changefeed payload has trailing bytes"));
		}
		Ok(())
	}
}

/// Checked amortized output growth, including its transient replacement buffer.
pub(in crate::storage) fn append(
	output: &mut Vec<u8>,
	bytes: &[u8],
	budget: &mut CodecBudget,
) -> FerrosGrynnResult<()> {
	let length = output
		.len()
		.checked_add(bytes.len())
		.ok_or_else(|| invalid("changefeed output length overflows"))?;
	checked_payload(length as u64)?;
	if length > output.capacity() {
		let old = output.capacity();
		let capacity = old.saturating_mul(2).max(64).max(length).min(MAX_COMMIT_BYTES);
		budget.admit(capacity)?;
		output
			.try_reserve_exact(capacity - output.len())
			.map_err(|_error| invalid("changefeed output allocation failed"))?;
		budget.used = budget
			.used
			.checked_sub(old)
			.ok_or_else(|| invalid("changefeed output reservation is inconsistent"))?;
	}
	output.extend_from_slice(bytes);
	Ok(())
}

/// Writes a self-delimiting byte field through the same aggregate admission.
pub(in crate::storage) fn append_blob(
	output: &mut Vec<u8>,
	bytes: &[u8],
	budget: &mut CodecBudget,
) -> FerrosGrynnResult<()> {
	append(output, &(bytes.len() as u64).to_le_bytes(), budget)?;
	append(output, bytes, budget)
}

/// Reports malformed physical bytes using the shared typed status mechanism.
pub(in crate::storage) fn invalid(detail: &str) -> FerrosGrynnError {
	FerrosGrynnError::invalid_storage_file(detail)
}
