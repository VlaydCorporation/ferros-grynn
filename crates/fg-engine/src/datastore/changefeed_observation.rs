//! Constant-size process observations; catalog bounds remain the durability
//! authority.

use std::sync::atomic::{AtomicU64, Ordering};

use super::{DatabaseRoot, Datastore};
use crate::{
	changefeed::ChangefeedStatistics,
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
};

/// Capture and publication counts shared by datastore handles, with bounded
/// cardinality.
#[derive(Default)]
pub(super) struct Counters {
	/// Scheduled operation replay attempts, including eventual failed commits.
	operations: AtomicU64,
	/// Successfully published NET entries.
	net: AtomicU64,
	/// Successfully published FULL entries.
	full: AtomicU64,
	/// Successfully published source/commit groups.
	groups: AtomicU64,
	/// Successfully published suppression groups.
	gaps: AtomicU64,
	/// Conservative retained capture/spool admission peak.
	scratch: AtomicU64,
}

/// Constant-size summary retained across the existing WAL publication barrier.
#[derive(Default)]
pub(super) struct Publication {
	/// NET entries for this publication only.
	net: u64,
	/// FULL entries for this publication only.
	full: u64,
	/// Groups for the current durable commit stamp.
	groups: u64,
	/// Suppression GAP groups for the current stamp.
	gaps: u64,
}

impl Publication {
	/// Observes only current-stamp groups, without scanning or cloning old
	/// images.
	pub(super) fn capture(root: &DatabaseRoot) -> Self {
		let mut observation = Self::default();
		for definition in root.changefeeds().definitions.values() {
			if definition.last_stamp != root.commit_ts {
				continue;
			}
			if let Some(group) = root.change_history().get(&(definition.id, root.commit_ts)) {
				observation.net = observation.net.saturating_add(group.net.entries().len() as u64);
				observation.full = observation.full.saturating_add(
					group.full.as_ref().map_or(0, |batch| batch.entries().len() as u64),
				);
				observation.groups = observation.groups.saturating_add(1);
				observation.gaps = observation.gaps.saturating_add(u64::from(group.gap.is_some()));
			}
		}
		observation
	}
}

/// Saturating diagnostics never change commit results or delivery semantics.
pub(super) fn add(counter: &AtomicU64, value: u64) {
	let _previous = counter.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
		Some(current.saturating_add(value))
	});
}

impl Counters {
	/// Records scheduled logical operations before capture replay.
	pub(super) fn operations(&self, operations: u64) {
		add(&self.operations, operations);
	}

	/// Records a conservative scratch peak after successful capture/spool
	/// admission.
	pub(super) fn scratch(&self, bytes: u64) {
		self.scratch.fetch_max(bytes, Ordering::Relaxed);
	}

	/// Records complete groups only after successful primary/feed/catalog
	/// persistence.
	pub(super) fn published(&self, publication: Publication) {
		add(&self.net, publication.net);
		add(&self.full, publication.full);
		add(&self.groups, publication.groups);
		add(&self.gaps, publication.gaps);
	}
}

impl Datastore {
	/// Returns payload-free process counters and retained physical history
	/// gauges.
	///
	/// The backlog check reads bounded metadata through the existing history
	/// pins. Process counters reset at open; retained gauges come from the
	/// immutable root.
	///
	/// # Errors
	///
	/// Returns metadata work limits, catalog corruption or snapshot/revision
	/// errors.
	pub fn changefeed_statistics(&self) -> FerrosGrynnResult<ChangefeedStatistics> {
		let root = self.inner.current_root();
		let mut segments = 0_u64;
		let mut bytes = 0_u64;
		for source in root.changefeed_segments().values() {
			for segment in source.iter() {
				segments = segments.saturating_add(1);
				if segments > 1_000_000 {
					return Err(FerrosGrynnError::storage_decode_limit_exceeded(
						"changefeed observation work",
						segments,
						1_000_000,
					));
				}
				bytes = bytes.saturating_add(
					segment.page_ceiling().saturating_mul(crate::storage::PAGE_SIZE as u64),
				);
			}
		}
		let counters = &self.inner.changefeed_observation;
		Ok(ChangefeedStatistics {
			capture_operations: counters.operations.load(Ordering::Relaxed),
			net_entries: counters.net.load(Ordering::Relaxed),
			full_entries: counters.full.load(Ordering::Relaxed),
			published_groups: counters.groups.load(Ordering::Relaxed),
			published_gaps: counters.gaps.load(Ordering::Relaxed),
			peak_capture_scratch_bytes: counters.scratch.load(Ordering::Relaxed),
			retained_sources: root.changefeeds().definitions.len() as u64,
			retained_segments: segments,
			retained_feed_bytes: bytes,
			purge_backlog: self.inner.has_changefeed_retention_work()?,
		})
	}
}
