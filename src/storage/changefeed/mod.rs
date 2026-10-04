//! Explicit changefeed bytes and bounded page continuations.

/// Reversible representation diffs relative to complete after images.
mod diff;
/// Whole-commit NET/FULL envelopes and control gaps.
mod group;
/// Complete record and qualified occurrence images.
mod image;
/// Segment headers and whole-commit continuation pages.
mod pages;
#[cfg(test)]
mod tests;
/// Historical dictionary namespaces and canonical runtime-value conversion.
mod values;

pub(in crate::storage) use group::{
	decode as decode_group,
	encode as encode_group,
	inspect as inspect_group,
};
pub(in crate::storage) use pages::{
	CommitLocator,
	SegmentHeader,
	decode_header,
	decode_locator,
	decode_pages,
	encode_header,
	encode_pages,
	first_payload,
	payload_pages,
};

use crate::gql_status::{FerrosGrynnError, FerrosGrynnResult};

/// An authoritative immutable-root reference to one append-only segment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(
	clippy::redundant_pub_crate,
	reason = "Re-exported internally for MVCC snapshot ownership."
)]
pub(crate) struct ChangefeedSegment {
	/// Physical identity allocated by the existing file catalog clock.
	pub(in crate::storage) file: crate::storage::types::DataFileId,
	/// Trusted snapshot bounds; disk headers must agree with these bytes.
	pub(in crate::storage) header: SegmentHeader,
}

/// Small shared descriptor tables; immutable snapshots retain older ceilings.
#[allow(
	clippy::redundant_pub_crate,
	reason = "Re-exported internally for MVCC snapshot ownership."
)]
pub(crate) type ChangefeedSegments = std::sync::Arc<
	std::collections::BTreeMap<
		crate::changefeed::ChangefeedId,
		std::sync::Arc<Vec<ChangefeedSegment>>,
	>,
>;

/// Checked commit metadata for bounded seeking and FULL coverage preflight.
#[derive(Debug, Clone, Copy)]
#[allow(clippy::redundant_pub_crate, reason = "Internal storage/datastore reader boundary.")]
pub(crate) struct ChangefeedGroupInfo {
	/// Exact source incarnation.
	pub(crate) source: crate::changefeed::ChangefeedId,
	/// Whole-commit position.
	pub(crate) stamp: u64,
	/// Nondecreasing UTC nanoseconds.
	pub(crate) time: i128,
	/// Historically captured operation mode.
	pub(crate) full: bool,
	/// Ordinary entries; zero FULL-only commits are skipped by NET readers.
	pub(crate) net_entries: u32,
	/// Suppression control rather than an unavailable capture mode.
	pub(crate) gap: bool,
	/// First page after this complete group.
	pub(crate) next_page: u64,
	/// Continuation pages needed to materialize this complete group.
	pub(crate) pages: u32,
	/// Historical validated source definition revision.
	pub(crate) revision: u32,
}

impl ChangefeedSegment {
	/// Returns independently trusted whole-segment stamp/time bounds.
	pub(crate) fn bounds(self) -> (u64, u64, i128, i128) {
		(
			self.header.first_stamp,
			self.header.last_stamp,
			self.header.first_time,
			self.header.last_time,
		)
	}

	/// Returns the exclusive page ceiling selected by the immutable root.
	pub(crate) fn page_ceiling(self) -> u64 {
		self.header.pages
	}

	/// Returns the expected complete group count for structural verification.
	pub(crate) fn commit_count(self) -> u64 {
		self.header.commits
	}
}

/// Hard payload cap shared with the existing frame-WAL transaction ceiling.
const MAX_COMMIT_BYTES: usize = 256 * 1024 * 1024;

/// Aggregate scratch admission before copying or growing a codec buffer.
pub(super) struct CodecBudget {
	/// All currently retained input/output and temporary allocations.
	used: usize,
	/// Caller-provided aggregate cap, independent of persisted lengths.
	maximum: usize,
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
fn checked_payload(bytes: u64) -> FerrosGrynnResult<usize> {
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
struct Decoder<'bytes> {
	/// Immutable complete payload.
	bytes: &'bytes [u8],
	/// Next unread offset; checked before advancing.
	position: usize,
}

impl<'bytes> Decoder<'bytes> {
	/// Starts decoding at the beginning of the declared payload.
	const fn new(bytes: &'bytes [u8]) -> Self {
		Self {
			bytes,
			position: 0,
		}
	}

	/// Takes an exact bounded slice without indexing a persisted length
	/// directly.
	fn take(&mut self, count: usize) -> FerrosGrynnResult<&'bytes [u8]> {
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
	fn u64(&mut self) -> FerrosGrynnResult<u64> {
		Ok(u64::from_le_bytes(
			self.take(8)?.try_into().map_err(|_error| invalid("changefeed u64 is truncated"))?,
		))
	}

	/// Reads one byte after validating the payload boundary.
	fn u8(&mut self) -> FerrosGrynnResult<u8> {
		Ok(self.take(1)?[0])
	}

	/// Reads one explicit little-endian count or dictionary slot.
	fn u32(&mut self) -> FerrosGrynnResult<u32> {
		Ok(u32::from_le_bytes(
			self.take(4)?.try_into().map_err(|_error| invalid("changefeed u32 is truncated"))?,
		))
	}

	/// Borrows one capped length-prefixed value without allocating from its
	/// length.
	fn blob(&mut self) -> FerrosGrynnResult<&'bytes [u8]> {
		let length = checked_payload(self.u64()?)?;
		self.take(length)
	}

	/// Rejects reserved bytes before interpreting following fields.
	fn zero(&mut self, count: usize) -> FerrosGrynnResult<()> {
		if self.take(count)?.iter().any(|byte| *byte != 0) {
			return Err(invalid("changefeed reserved bytes are nonzero"));
		}
		Ok(())
	}

	/// Rejects noncanonical trailing bytes instead of accepting a prefix
	/// silently.
	fn finish(self) -> FerrosGrynnResult<()> {
		if self.position != self.bytes.len() {
			return Err(invalid("changefeed payload has trailing bytes"));
		}
		Ok(())
	}
}

/// Checked amortized output growth, including its transient replacement buffer.
fn append(output: &mut Vec<u8>, bytes: &[u8], budget: &mut CodecBudget) -> FerrosGrynnResult<()> {
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
fn append_blob(
	output: &mut Vec<u8>,
	bytes: &[u8],
	budget: &mut CodecBudget,
) -> FerrosGrynnResult<()> {
	append(output, &(bytes.len() as u64).to_le_bytes(), budget)?;
	append(output, bytes, budget)
}

/// Reports malformed physical bytes using the shared typed status mechanism.
fn invalid(detail: &str) -> FerrosGrynnError {
	FerrosGrynnError::invalid_storage_file(detail)
}
