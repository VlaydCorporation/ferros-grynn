//! Whole-commit results and explicit loss-of-history records.

use super::{ChangeEntry, ChangefeedId, Versionstamp};
use crate::values::DateTime;

/// Complete changes for one source and one durable commit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChangeBatch {
	/// Retained source incarnation.
	pub(crate) source: ChangefeedId,
	/// Database-wide commit position.
	pub(crate) versionstamp: Versionstamp,
	/// Nondecreasing UTC commit time.
	pub(crate) committed_at: DateTime,
	/// All selected entries; the reader never splits this collection.
	pub(crate) entries: Vec<ChangeEntry>,
}

impl ChangeBatch {
	/// Returns the retained source incarnation.
	pub const fn source(&self) -> ChangefeedId {
		self.source
	}

	/// Returns the exact position shared by the whole commit.
	pub const fn versionstamp(&self) -> Versionstamp {
		self.versionstamp
	}

	/// Returns the persisted nondecreasing UTC commit time.
	pub const fn committed_at(&self) -> &DateTime {
		&self.committed_at
	}

	/// Returns all entries in canonical NET or execution FULL order.
	pub fn entries(&self) -> &[ChangeEntry] {
		&self.entries
	}
}

/// Explicit cause of an unavailable interval.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ChangefeedGapReason {
	/// Final DROP suppressed capture for this commit.
	SourceDropped,
	/// Import ran with changefeed capture disabled.
	ImportSuppressed,
	/// Retention removed an available prefix.
	Retention,
	/// A segment with trusted catalog bounds cannot be decoded.
	SegmentCorrupt,
	/// The retained source and all its history have expired.
	FeedExpiredOrRetired,
}

/// Loss of history with explicit optional trusted bounds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChangefeedGap {
	/// Retained source incarnation, even after retirement.
	pub(crate) source: ChangefeedId,
	/// Typed loss-of-history cause.
	pub(crate) reason: ChangefeedGapReason,
	/// First unavailable stamp, or None when no trustworthy bounds remain.
	pub(crate) from: Option<Versionstamp>,
	/// Last unavailable stamp, or None when no trustworthy bounds remain.
	pub(crate) through: Option<Versionstamp>,
}

impl ChangefeedGap {
	/// Returns the retained source incarnation.
	pub const fn source(&self) -> ChangefeedId {
		self.source
	}

	/// Returns the explicit cause of the missing history.
	pub const fn reason(&self) -> ChangefeedGapReason {
		self.reason
	}

	/// Returns the first trusted unavailable stamp.
	pub const fn from(&self) -> Option<Versionstamp> {
		self.from
	}

	/// Returns the final trusted unavailable stamp.
	pub const fn through(&self) -> Option<Versionstamp> {
		self.through
	}
}

/// One whole commit or an explicit gap, counted as one reader item.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum ChangefeedReadItem {
	/// Exact selected changes for a whole commit.
	Commit(ChangeBatch),
	/// Explicit unavailable history, followed by the available tail if any.
	Gap(ChangefeedGap),
}
