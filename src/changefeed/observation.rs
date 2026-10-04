//! Owned logical bindings and payload-free reader observations.

use super::{ChangefeedConfig, ChangefeedId, ChangefeedSource, ChangefeedState, Versionstamp};
use crate::{datastore::SchemaVersion, values::Uuid};

/// Allocation-free committed source metadata; private policy edits are
/// excluded.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub struct ChangefeedReadBinding {
	/// Durable database origin used by resume validation.
	pub database: Uuid,
	/// Exact retained source incarnation.
	pub id: ChangefeedId,
	/// Logical class or graph scope.
	pub source: ChangefeedSource,
	/// Committed definition revision.
	pub revision: SchemaVersion,
	/// Latest committed capture policy, independent of historical FULL
	/// coverage.
	pub configuration: ChangefeedConfig,
	/// Committed source lifecycle.
	pub state: ChangefeedState,
	/// Fixed upper boundary for this read.
	pub snapshot: Versionstamp,
	/// Latest retained history position, or zero.
	pub last: Versionstamp,
	/// Unavailable prefix floor, or zero.
	pub purged_through: Versionstamp,
}

/// Cumulative observations for one reader, including historical FULL preflight.
///
/// Counters saturate on overflow and never affect result semantics. They
/// contain no images, property values, file names or physical identities.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct ChangefeedReadStatistics {
	/// Successfully returned whole commits, excluding GAPs and empty NET
	/// commits.
	pub batches: u64,
	/// Entries in successfully returned commits.
	pub entries: u64,
	/// Successfully returned explicit loss intervals.
	pub gaps: u64,
	/// Inspected whole-commit headers, including preflight and skipped commits.
	pub groups_inspected: u64,
	/// Logical page read attempts through WAL/home/cache; these are not disk
	/// misses.
	pub page_reads: u64,
	/// Requested logical page bytes; zero for RAM history.
	pub bytes_read: u64,
	/// Segment entries visited, including preflight and skipped segments.
	pub segment_visits: u64,
	/// Admitted metadata/group/page work units.
	pub work: u64,
	/// Conservative admitted scratch high-water mark in bytes, not measured
	/// RSS.
	pub peak_scratch_bytes: u64,
}

/// Process counters since datastore open plus current retained-history gauges.
///
/// Observations are diagnostic and not a durability or delivery
/// acknowledgement.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct ChangefeedStatistics {
	/// Successful logical operations scheduled for capture replay, including
	/// retries.
	pub capture_operations: u64,
	/// NET entries in successfully published feed groups.
	pub net_entries: u64,
	/// FULL entries in successfully published feed groups; multiple targets may
	/// occur.
	pub full_entries: u64,
	/// Whole source/commit groups successfully published in this process.
	pub published_groups: u64,
	/// Suppression GAP groups successfully published in this process.
	pub published_gaps: u64,
	/// Conservative admitted capture/spool scratch peak; not measured RSS.
	pub peak_capture_scratch_bytes: u64,
	/// Retained active/stopped/retired source definitions in the current root.
	pub retained_sources: u64,
	/// Retained physical segments in the current root; zero on RAM.
	pub retained_segments: u64,
	/// Physical page footprint, including headers and an expired partial
	/// prefix.
	pub retained_feed_bytes: u64,
	/// At least one TTL source/group currently needs bounded maintenance work.
	pub purge_backlog: bool,
}
