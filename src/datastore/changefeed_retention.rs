//! Logical TTL authority; physical unlink follows durable catalog publication.

use std::{ops::Bound, sync::Arc};

use super::{ChangefeedDefinition, ConflictKey, DatabaseRoot, StoreInner};
use crate::{
	changefeed::ChangefeedState,
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	storage::{ChangefeedGroupInfo, ChangefeedSegment, LocalChangefeedSnapshot},
};

/// Maximum removed groups/segments in one worker pass; later passes continue.
const MAX_REMOVALS: usize = 1024;
/// Metadata and page inspection cap, independent of payload allocation limits.
const MAX_WORK: u64 = 1_000_000;

#[cfg(test)]
thread_local! {
	/// Deterministic deadlines in tests; the worker still observes its own clock.
	pub(super) static TEST_TIME: std::cell::Cell<Option<i128>> = const { std::cell::Cell::new(None) };
}

/// Reads a nondecreasing deadline clock without altering the commit clock.
fn observed_time(root: &DatabaseRoot) -> i128 {
	#[cfg(test)]
	if let Some(time) = TEST_TIME.get() {
		return root.committed_at_ns.max(time);
	}
	root.committed_at_ns.max(jiff::Timestamp::now().as_nanosecond())
}

/// Checked pass accounting, shared by metadata scans and COW publication.
struct PurgeBudget {
	/// Scratch already admitted, including replacement metadata.
	bytes: usize,
	/// Existing maintenance memory cap.
	maximum: usize,
	/// Metadata/page operations already performed.
	work: u64,
	/// Removed whole groups or segments in this pass.
	removed: usize,
}

impl PurgeBudget {
	/// Checks all retained scratch before allocation or copying.
	fn bytes(&mut self, additional: usize) -> FerrosGrynnResult<()> {
		let actual = self
			.bytes
			.checked_add(additional)
			.ok_or_else(|| FerrosGrynnError::operation_overflow("changefeed purge scratch"))?;
		if actual > self.maximum {
			return Err(FerrosGrynnError::storage_decode_limit_exceeded(
				"changefeed purge scratch",
				actual as u64,
				self.maximum as u64,
			));
		}
		self.bytes = actual;
		Ok(())
	}

	/// Checks cancellation and bounded work before inspecting a group.
	fn step(&mut self) -> FerrosGrynnResult<()> {
		crate::storage::check_external_sort_cancelled()?;
		self.work = self
			.work
			.checked_add(1)
			.ok_or_else(|| FerrosGrynnError::operation_overflow("changefeed purge work"))?;
		if self.work > MAX_WORK {
			return Err(FerrosGrynnError::storage_decode_limit_exceeded(
				"changefeed purge work",
				self.work,
				MAX_WORK,
			));
		}
		Ok(())
	}
}

impl StoreInner {
	/// Pins a matching logical/physical root before metadata-only TTL work.
	fn retention_snapshot(
		&self,
		root: &DatabaseRoot,
	) -> FerrosGrynnResult<Option<LocalChangefeedSnapshot>> {
		if root.changefeed_segments().is_empty() {
			return Ok(None);
		}
		let _commit = self.commit.lock();
		if self.current_root().commit_ts != root.commit_ts {
			return Err(FerrosGrynnError::maintenance_revision_race("changefeed retention"));
		}
		self.capture_changefeed_snapshot(root.commit_ts)?
			.map(Some)
			.ok_or_else(|| FerrosGrynnError::maintenance_revision_race("changefeed snapshot pin"))
	}

	/// Checks the first live group without creating empty durable TTL jobs.
	pub(super) fn has_changefeed_retention_work(&self) -> FerrosGrynnResult<bool> {
		if self.ensure_write_allowed(0).is_err() {
			return Ok(false);
		}
		let root = self.current_root();
		if root.changefeeds().definitions.is_empty() {
			return Ok(false);
		}
		let pin = self.retention_snapshot(&root)?;
		let now = observed_time(&root);
		let mut work = 0_u64;
		for entry in root.changefeeds().definitions.values() {
			work += 1;
			if work > MAX_WORK {
				return Err(FerrosGrynnError::storage_decode_limit_exceeded(
					"changefeed idle purge work",
					work,
					MAX_WORK,
				));
			}
			if self.maintenance.is_stopping() {
				return Ok(false);
			}
			let cutoff = cutoff(entry, now)?;
			if entry.closed_time_ns.is_some_and(|time| time <= cutoff) {
				return Ok(true);
			}
			if let Some(segments) = root.changefeed_segments().get(&entry.id)
				&& let Some(segment) = segments.first().copied()
			{
				if segment.bounds().3 <= cutoff {
					return Ok(true);
				}
				if let Some(pin) = &pin
					&& self
						.inspect_changefeed_group(
							pin,
							segment,
							entry.retained_page.max(1),
							32 * 1024,
						)?
						.is_some_and(|info| info.time <= cutoff)
				{
					return Ok(true);
				}
			} else if root
				.change_history()
				.range((
					Bound::Excluded((entry.id, entry.purged_through)),
					Bound::Included((entry.id, root.commit_ts)),
				))
				.next()
				.is_some_and(|(_, group)| {
					group.net.committed_at().as_inner().timestamp().as_nanosecond() <= cutoff
				}) {
				return Ok(true);
			}
		}
		Ok(false)
	}

	/// Creates one bounded immutable candidate, leaving the old root intact.
	pub(super) fn purge_changefeed_candidate(
		&self,
		snapshot: &DatabaseRoot,
		publication: u64,
	) -> FerrosGrynnResult<DatabaseRoot> {
		let pin = self.retention_snapshot(snapshot)?;
		let now = observed_time(snapshot);
		let mut budget = PurgeBudget {
			bytes: 32 * 1024,
			maximum: crate::storage::external_sort_run_budget(),
			work: 0,
			removed: 0,
		};
		budget.bytes(0)?;
		for (count, width) in [
			(snapshot.changefeeds().definitions.len(), 128),
			(snapshot.changefeed_segments().len(), 128),
			(snapshot.change_history().len(), 128),
			// Version entries contain only fixed-size Copy identities and stamps.
			(snapshot.versions.len(), std::mem::size_of::<(ConflictKey, u64)>()),
			(
				snapshot.graphs.len(),
				std::mem::size_of::<(crate::graph::GraphId, Arc<super::GraphRoot>)>(),
			),
		] {
			budget.bytes(crate::values::storage::tree_allocation_bytes(count, width)?)?;
		}
		crate::storage::check_external_sort_cancelled()?;
		let mut next = snapshot.clone();
		for original in snapshot.changefeeds().definitions.values() {
			if budget.removed >= MAX_REMOVALS {
				break;
			}
			budget.step()?;
			let cutoff = cutoff(original, now)?;
			budget.bytes(
				std::mem::size_of::<ChangefeedDefinition>()
					.checked_add(original.name.len())
					.ok_or_else(|| FerrosGrynnError::operation_overflow("purge source name"))?
					.checked_add(
						original
							.epochs
							.len()
							.checked_mul(std::mem::size_of::<
								crate::changefeed::ChangefeedCaptureEpoch,
							>())
							.ok_or_else(|| {
								FerrosGrynnError::operation_overflow("purge capture epochs")
							})?,
					)
					.ok_or_else(|| FerrosGrynnError::operation_overflow("purge source metadata"))?,
			)?;
			let mut entry = original.as_ref().clone();
			if let Some(segments) = snapshot.changefeed_segments().get(&entry.id) {
				budget.bytes(
					segments
						.len()
						.checked_mul(std::mem::size_of::<ChangefeedSegment>())
						.ok_or_else(|| {
							FerrosGrynnError::operation_overflow("purge segment metadata")
						})?,
				)?;
				let mut removed = 0;
				for segment in segments.iter().copied() {
					if budget.removed >= MAX_REMOVALS {
						break;
					}
					budget.step()?;
					let (_, last, _, last_time) = segment.bounds();
					if last_time <= cutoff {
						entry.purged_through = last;
						entry.purged_time_ns = last_time;
						entry.retained_page = 0;
						entry.retained_groups = 0;
						removed += 1;
						budget.removed += 1;
						continue;
					}
					if let Some(pin) = &pin {
						self.purge_partial_segment(pin, segment, &mut entry, cutoff, &mut budget)?;
					}
					break;
				}
				if removed != 0 {
					let retained = segments[removed..].to_vec();
					let table = Arc::make_mut(next.changefeed_segments_mut());
					if retained.is_empty() {
						table.remove(&entry.id);
					} else {
						table.insert(entry.id, Arc::new(retained));
					}
				}
			} else {
				for ((feed, stamp), group) in snapshot.change_history().range((
					Bound::Excluded((entry.id, entry.purged_through)),
					Bound::Included((entry.id, snapshot.commit_ts)),
				)) {
					if budget.removed >= MAX_REMOVALS {
						break;
					}
					budget.step()?;
					let time = group.net.committed_at().as_inner().timestamp().as_nanosecond();
					if time > cutoff {
						break;
					}
					Arc::make_mut(next.change_history_mut()).remove(&(*feed, *stamp));
					entry.purged_through = *stamp;
					entry.purged_time_ns = time;
					budget.removed += 1;
				}
			}
			let empty = !next
				.changefeed_segments()
				.get(&entry.id)
				.is_some_and(|segments| !segments.is_empty())
				&& next
					.change_history()
					.range((Bound::Included((entry.id, 0)), Bound::Included((entry.id, u64::MAX))))
					.next()
					.is_none();
			let retire = entry.state != ChangefeedState::Active
				&& empty
				&& entry.closed_time_ns.is_some_and(|time| time <= cutoff);
			if retire || entry != **original {
				next.mark_version(ConflictKey::ChangefeedCatalogRange, publication);
				next.mark_version(ConflictKey::ChangefeedDefinition(entry.source), publication);
				let catalog = Arc::make_mut(next.changefeeds_mut());
				if retire {
					catalog.definitions.remove(&entry.id);
				} else {
					catalog.definitions.insert(entry.id, Arc::new(entry));
				}
			}
		}
		Ok(next)
	}

	/// Advances an authoritative prefix only across complete checked metadata.
	fn purge_partial_segment(
		&self,
		pin: &LocalChangefeedSnapshot,
		segment: ChangefeedSegment,
		entry: &mut ChangefeedDefinition,
		cutoff: i128,
		budget: &mut PurgeBudget,
	) -> FerrosGrynnResult<()> {
		let mut page = entry.retained_page.max(1);
		let mut groups = entry.retained_groups;
		let (first, last, first_time, last_time) = segment.bounds();
		let mut previous = (entry.purged_through, entry.purged_time_ns);
		while page < segment.page_ceiling() && budget.removed < MAX_REMOVALS {
			budget.step()?;
			let Some(info) =
				self.inspect_changefeed_group(pin, segment, page, budget.maximum - budget.bytes)?
			else {
				break;
			};
			if !valid_group(info, entry, previous, groups)
				|| groups == 0 && (info.stamp != first || info.time != first_time)
				|| info.next_page == segment.page_ceiling()
					&& (groups + 1 != segment.commit_count()
						|| info.stamp != last
						|| info.time != last_time)
			{
				break;
			}
			if info.time > cutoff {
				break;
			}
			page = info.next_page;
			groups += 1;
			previous = (info.stamp, info.time);
			entry.purged_through = info.stamp;
			entry.purged_time_ns = info.time;
			entry.retained_page = page;
			entry.retained_groups = groups;
			budget.removed += 1;
		}
		Ok(())
	}
}

#[cfg(test)]
mod tests;

/// Uses the selected source's fixed TTL without reversing the durable clock.
fn cutoff(entry: &ChangefeedDefinition, now: i128) -> FerrosGrynnResult<i128> {
	let retention = entry.config.retention().fixed_nanoseconds().ok_or_else(|| {
		FerrosGrynnError::invalid_storage_file("retained changefeed TTL is not fixed")
	})?;
	now.checked_sub(retention)
		.ok_or_else(|| FerrosGrynnError::operation_overflow("changefeed retention deadline"))
}

/// Keeps optional corruption from manufacturing a trusted partial purge floor.
fn valid_group(
	info: ChangefeedGroupInfo,
	entry: &ChangefeedDefinition,
	previous: (u64, i128),
	groups: u64,
) -> bool {
	info.source == entry.id
		&& info.revision <= entry.version.get()
		&& info.stamp > previous.0
		&& (groups == 0 || info.time >= previous.1)
}
