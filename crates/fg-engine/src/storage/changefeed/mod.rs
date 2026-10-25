//! Explicit changefeed bytes and bounded page continuations.

/// Reversible representation diffs relative to complete after images.
mod diff;
/// Whole-commit NET/FULL envelopes and control gaps.
mod group;
/// Complete record and qualified occurrence images.
mod image;
#[cfg(test)]
mod tests;

pub(in crate::storage) use group::{
	decode as decode_group,
	encode as encode_group,
	inspect as inspect_group,
};

#[cfg(test)]
pub(in crate::storage) use super::history_codec::pages::{SegmentHeader, encode_pages};
pub(in crate::storage) use super::history_codec::{
	CodecBudget,
	pages::{CommitLocator, decode_header, decode_locator, decode_pages, first_payload},
};
use super::history_codec::{Decoder, append, append_blob, checked_payload, invalid, values};

/// Small shared descriptor tables; immutable snapshots retain older ceilings.
#[allow(
	clippy::redundant_pub_crate,
	reason = "Re-exported internally for MVCC snapshot ownership."
)]
pub type ChangefeedSegments = std::sync::Arc<
	std::collections::BTreeMap<
		crate::changefeed::ChangefeedId,
		std::sync::Arc<Vec<crate::storage::HistorySegment>>,
	>,
>;

/// Checked commit metadata for bounded seeking and FULL coverage preflight.
#[derive(Debug, Clone, Copy)]
#[allow(clippy::redundant_pub_crate, reason = "Internal storage/datastore reader boundary.")]
pub struct ChangefeedGroupInfo {
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
