//! Checked queue admission and evaluation limits.

use std::time::Duration;

use super::LiveCursor;
use crate::{
	changefeed::Versionstamp,
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	values::DateTime,
};

/// Starting position; timestamp selection is inclusive, stamps exclusive.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum LiveSince {
	/// Start after the successful registration commit without initial rows.
	#[default]
	Now,
	/// Catch up after this complete commit; missing coverage is explicit.
	Versionstamp(Versionstamp),
	/// Catch up at or after the UTC timestamp, ordered by commit stamps.
	Timestamp(DateTime),
	/// Resume an existing incarnation from its confirmed watermark.
	Cursor(LiveCursor),
}

impl LiveSince {
	/// Checks a persisted exclusive floor without re-resolving timestamps.
	pub(crate) fn matches_start(&self, starting_after: u64, defined_at: u64) -> bool {
		starting_after <= defined_at
			&& match self {
				Self::Now => starting_after == defined_at,
				Self::Versionstamp(stamp) => starting_after == stamp.get(),
				Self::Timestamp(_) => true,
				Self::Cursor(cursor) => starting_after == cursor.versionstamp().get(),
			}
	}
}

/// Policy when a complete delivery cannot be admitted to a consumer queue.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum LiveOverflowPolicy {
	/// Report a lost interval explicitly in a reserved control slot.
	#[default]
	Gap,
	/// Terminate delivery with a diagnostic and confirmed watermark.
	Disconnect,
	/// Reserve capacity before publication, bounded by timeout and deadline.
	Block,
}

/// Validated per-subscription limits; queue accounting never splits a commit.
#[derive(Debug, Clone, PartialEq, Eq)]
#[must_use]
pub struct LiveOptions {
	/// Fixed start or resume position.
	since: LiveSince,
	/// Queue overflow policy.
	overflow: LiveOverflowPolicy,
	/// Event entries admitted before publication; controls have their own slot.
	queue_entries: usize,
	/// Retained queue byte ceiling, independent of entry count.
	queue_bytes: usize,
	/// Maximum evaluation scratch; specialized query limits still apply.
	scratch_bytes: usize,
	/// Maximum dependency and evaluation work units.
	work: u64,
	/// Capacity wait limit, additionally bounded by transaction deadline.
	block_timeout: Duration,
}

impl Default for LiveOptions {
	fn default() -> Self {
		Self {
			since: LiveSince::Now,
			overflow: LiveOverflowPolicy::Gap,
			queue_entries: 10_000,
			queue_bytes: 64 * 1024 * 1024,
			scratch_bytes: 256 * 1024 * 1024,
			work: 1_000_000,
			block_timeout: Duration::from_secs(30),
		}
	}
}

impl LiveOptions {
	/// Changes the explicit starting position.
	pub fn with_since(mut self, since: LiveSince) -> Self {
		self.since = since;
		self
	}

	/// Changes overflow behavior without changing queue limits.
	pub fn with_overflow(mut self, policy: LiveOverflowPolicy) -> Self {
		self.overflow = policy;
		self
	}

	/// Changes queue entry and byte limits, including whole-commit admission.
	///
	/// # Errors
	///
	/// Returns an invalid-argument status for zero or unaddressable limits.
	pub fn with_queue(mut self, entries: usize, bytes: usize) -> FerrosGrynnResult<Self> {
		if entries == 0
			|| bytes == 0
			|| entries > isize::MAX as usize
			|| bytes > isize::MAX as usize
		{
			return Err(FerrosGrynnError::invalid_argument(
				"LIVE queue limits must be positive and addressable",
			));
		}
		self.queue_entries = entries;
		self.queue_bytes = bytes;
		Ok(self)
	}

	/// Changes evaluation scratch and work budgets.
	///
	/// # Errors
	///
	/// Returns an invalid-argument status for zero or unaddressable scratch.
	pub fn with_budgets(mut self, scratch_bytes: usize, work: u64) -> FerrosGrynnResult<Self> {
		validate_evaluation_budgets(scratch_bytes, work)?;
		self.scratch_bytes = scratch_bytes;
		self.work = work;
		Ok(self)
	}

	/// Changes the maximum capacity wait; transaction deadline may shorten it.
	///
	/// # Errors
	///
	/// Returns an invalid-argument status for a zero or unrepresentable
	/// timeout.
	pub fn with_block_timeout(mut self, timeout: Duration) -> FerrosGrynnResult<Self> {
		if timeout.is_zero() || std::time::Instant::now().checked_add(timeout).is_none() {
			return Err(FerrosGrynnError::invalid_argument(
				"LIVE BLOCK timeout must be finite and positive",
			));
		}
		self.block_timeout = timeout;
		Ok(self)
	}

	/// Returns the selected starting boundary.
	pub const fn since(&self) -> &LiveSince {
		&self.since
	}

	/// Returns the queue policy.
	pub const fn overflow(&self) -> LiveOverflowPolicy {
		self.overflow
	}

	/// Returns the event entry cap.
	pub const fn queue_entries(&self) -> usize {
		self.queue_entries
	}

	/// Returns the retained queue byte cap.
	pub const fn queue_bytes(&self) -> usize {
		self.queue_bytes
	}

	/// Returns the scratch cap.
	pub const fn scratch_bytes(&self) -> usize {
		self.scratch_bytes
	}

	/// Returns the work cap.
	pub const fn work(&self) -> u64 {
		self.work
	}

	/// Returns the capacity timeout.
	pub const fn block_timeout(&self) -> Duration {
		self.block_timeout
	}
}

/// Validated limits for a reactive value without a consumer queue.
#[derive(Debug, Clone, PartialEq, Eq)]
#[must_use]
pub struct LiveBindingOptions {
	/// Maximum retained compiler and evaluation scratch.
	scratch_bytes: usize,
	/// Maximum dependency and evaluation work units.
	work: u64,
}

impl Default for LiveBindingOptions {
	fn default() -> Self {
		Self {
			scratch_bytes: 256 * 1024 * 1024,
			work: 1_000_000,
		}
	}
}

impl LiveBindingOptions {
	/// Changes the compiler, dependency and execution budgets.
	///
	/// # Errors
	///
	/// Returns invalid-argument status for zero or unaddressable budgets.
	pub fn with_budgets(mut self, scratch_bytes: usize, work: u64) -> FerrosGrynnResult<Self> {
		validate_evaluation_budgets(scratch_bytes, work)?;
		self.scratch_bytes = scratch_bytes;
		self.work = work;
		Ok(self)
	}

	/// Returns the aggregate evaluation scratch ceiling.
	pub const fn scratch_bytes(&self) -> usize {
		self.scratch_bytes
	}

	/// Returns the logical work ceiling.
	pub const fn work(&self) -> u64 {
		self.work
	}
}

/// Both subscriptions and bindings use the same addressable evaluation limits.
fn validate_evaluation_budgets(scratch_bytes: usize, work: u64) -> FerrosGrynnResult<()> {
	if scratch_bytes == 0 || scratch_bytes > isize::MAX as usize || work == 0 {
		return Err(FerrosGrynnError::invalid_argument(
			"LIVE evaluation budgets must be positive and addressable",
		));
	}
	Ok(())
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn resource_limits_reject_invalid_admission() {
		let options = LiveOptions::default();
		assert_eq!(options.queue_entries(), 10_000);
		assert_eq!(options.queue_bytes(), 64 * 1024 * 1024);
		assert_eq!(options.work(), 1_000_000);
		assert_eq!(options.overflow(), LiveOverflowPolicy::Gap);
		assert_eq!(options.since(), &LiveSince::Now);
		assert!(options.clone().with_queue(0, 1).is_err());
		assert!(options.clone().with_queue(1, 0).is_err());
		assert!(options.clone().with_queue(usize::MAX, 1).is_err());
		assert!(options.clone().with_budgets(0, 1).is_err());
		assert!(options.clone().with_budgets(1, 0).is_err());
		assert!(options.clone().with_budgets(usize::MAX, 1).is_err());
		assert!(options.with_block_timeout(Duration::ZERO).is_err());
		let binding = LiveBindingOptions::default();
		assert_eq!(binding.scratch_bytes(), 256 * 1024 * 1024);
		assert_eq!(binding.work(), 1_000_000);
		assert!(binding.clone().with_budgets(0, 1).is_err());
		assert!(binding.clone().with_budgets(1, 0).is_err());
		assert!(binding.with_budgets(usize::MAX, 1).is_err());
	}
}
