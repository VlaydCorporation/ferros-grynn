//! Computed-result prefix TTL; source capture and LIVE share one purge job.

use std::{ops::Bound, sync::Arc};

use super::{MAX_REMOVALS, MAX_WORK, PurgeBudget};
use crate::{
	datastore::{ConflictKey, DatabaseRoot, LiveHistoryMetadata, StoreInner},
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	live::LiveId,
	storage::{HistoryOwner, HistorySegment, LocalChangefeedSnapshot, live_history},
};

/// A saved group belongs to exactly one captured durability epoch.
fn expired(
	history: &LiveHistoryMetadata,
	stamp: u64,
	time: i128,
	now: i128,
) -> FerrosGrynnResult<bool> {
	let ordinal = history.epochs.partition_point(|epoch| epoch.started_at <= stamp);
	let policy = ordinal
		.checked_sub(1)
		.and_then(|index| history.epochs.get(index))
		.and_then(|epoch| epoch.policy)
		.ok_or_else(|| invalid("LIVE retained group has no capture policy"))?;
	let cutoff = now
		.checked_sub(policy.retention_ns)
		.ok_or_else(|| invalid("LIVE retention time overflows"))?;
	Ok(time <= cutoff)
}

/// Full-segment expiration is proven conservatively across all policy epochs.
fn segment_deadline(history: &LiveHistoryMetadata, now: i128) -> FerrosGrynnResult<i128> {
	let ttl = history
		.epochs
		.iter()
		.filter_map(|epoch| epoch.policy.map(|policy| policy.retention_ns))
		.max()
		.ok_or_else(|| invalid("LIVE retention policy is absent"))?;
	now.checked_sub(ttl).ok_or_else(|| invalid("LIVE retention deadline overflows"))
}

/// Finds an expired prefix without allocating values or manufacturing jobs.
pub(super) fn due(
	store: &StoreInner,
	root: &DatabaseRoot,
	pin: Option<&LocalChangefeedSnapshot>,
	now: i128,
	work: &mut u64,
) -> FerrosGrynnResult<bool> {
	for (id, history) in &root.live_catalog().histories {
		*work = work
			.checked_add(1 + history.epochs.len() as u64)
			.ok_or_else(|| invalid("LIVE idle purge work overflows"))?;
		if *work > MAX_WORK {
			return Err(FerrosGrynnError::storage_decode_limit_exceeded(
				"LIVE idle purge work",
				*work,
				MAX_WORK,
			));
		}
		if store.maintenance.is_stopping() {
			return Ok(false);
		}
		if let Some(segment) =
			root.live_segments().get(id).and_then(|segments| segments.first()).copied()
		{
			check_owner(*id, segment)?;
			if segment.bounds().3 <= segment_deadline(history, now)? {
				return Ok(true);
			}
			if let Some(pin) = pin
				&& let Some((info, _)) = store.inspect_live_group(
					pin,
					segment,
					history.retained_page.max(1),
					32 * 1024,
				)?
				&& expired(history, info.stamp, info.time, now)?
			{
				return Ok(true);
			}
		} else if history.purged_through < root.commit_ts
			&& let Some(((_, stamp), bytes)) = root
				.live_history()
				.range((
					Bound::Excluded((*id, history.purged_through)),
					Bound::Included((*id, root.commit_ts)),
				))
				.next()
		{
			let info = live_history::inspect(bytes, bytes.len() as u64)?;
			if info.id != *id || info.stamp != *stamp || info.version != history.version {
				return Err(invalid("LIVE RAM purge key disagrees with its envelope"));
			}
			if expired(history, info.stamp, info.time, now)? {
				return Ok(true);
			}
		}
	}
	Ok(false)
}

/// Removes complete groups only; floors survive an empty physical reference
/// list.
pub(super) fn purge(
	store: &StoreInner,
	snapshot: &DatabaseRoot,
	next: &mut DatabaseRoot,
	pin: Option<&LocalChangefeedSnapshot>,
	now: i128,
	publication: u64,
	budget: &mut PurgeBudget,
) -> FerrosGrynnResult<()> {
	for (id, original) in &snapshot.live_catalog().histories {
		if budget.removed >= MAX_REMOVALS {
			break;
		}
		budget.step()?;
		let definition = snapshot
			.live_catalog()
			.queries
			.get(id)
			.ok_or_else(|| invalid("LIVE purge owner is absent"))?;
		let mut maximum_ttl = 0;
		for epoch in &original.epochs {
			budget.step()?;
			if let Some(policy) = epoch.policy {
				maximum_ttl = maximum_ttl.max(policy.retention_ns);
			}
		}
		original.validate(definition.starting_after, snapshot.commit_ts)?;
		if maximum_ttl == 0 {
			return Err(invalid("LIVE retention policy is absent"));
		}
		let segment_deadline = now
			.checked_sub(maximum_ttl)
			.ok_or_else(|| invalid("LIVE retention deadline overflows"))?;
		budget.bytes(
			std::mem::size_of::<LiveHistoryMetadata>()
				.checked_add(
					original
						.epochs
						.len()
						.checked_mul(std::mem::size_of::<crate::datastore::LiveCaptureEpoch>())
						.ok_or_else(|| invalid("LIVE purge epochs overflow"))?,
				)
				.ok_or_else(|| invalid("LIVE purge metadata overflows"))?,
		)?;
		let mut history = original.as_ref().clone();
		if let Some(segments) = snapshot.live_segments().get(id) {
			budget.bytes(
				segments
					.len()
					.checked_mul(std::mem::size_of::<HistorySegment>())
					.ok_or_else(|| invalid("LIVE purge references overflow"))?,
			)?;
			let mut removed = 0;
			for segment in segments.iter().copied() {
				if budget.removed >= MAX_REMOVALS {
					break;
				}
				budget.step()?;
				check_owner(*id, segment)?;
				if segment.bounds().3 <= segment_deadline {
					history.purged_through = segment.bounds().1;
					history.purged_time_ns = segment.bounds().3;
					history.retained_page = 0;
					history.retained_groups = 0;
					removed += 1;
					budget.removed += 1;
					continue;
				}
				if let Some(pin) = pin {
					partial(store, pin, *id, segment, &mut history, now, budget)?;
				}
				if history.retained_page == segment.page_ceiling() {
					history.retained_page = 0;
					history.retained_groups = 0;
					removed += 1;
					continue;
				}
				break;
			}
			if removed != 0 {
				let retained = segments[removed..].to_vec();
				let table = Arc::make_mut(next.live_segments_mut());
				if retained.is_empty() {
					table.remove(id);
				} else {
					table.insert(*id, Arc::new(retained));
				}
			}
		} else if history.purged_through < snapshot.commit_ts {
			for ((owner, stamp), bytes) in snapshot.live_history().range((
				Bound::Excluded((*id, history.purged_through)),
				Bound::Included((*id, snapshot.commit_ts)),
			)) {
				if budget.removed >= MAX_REMOVALS {
					break;
				}
				budget.step()?;
				let info = live_history::inspect(bytes, bytes.len() as u64)?;
				if info.id != *owner || info.version != history.version || info.stamp != *stamp {
					return Err(invalid("LIVE RAM purge key disagrees with its envelope"));
				}
				if !expired(&history, info.stamp, info.time, now)? {
					break;
				}
				Arc::make_mut(next.live_history_mut()).remove(&(*owner, *stamp));
				history.purged_through = info.stamp;
				history.purged_time_ns = info.time;
				budget.removed += 1;
			}
		}
		if history != **original {
			history.validate(definition.starting_after, snapshot.commit_ts)?;
			next.mark_version(ConflictKey::LiveCatalogRange, publication);
			Arc::make_mut(next.live_catalog_mut()).histories.insert(*id, Arc::new(history));
		}
	}
	Ok(())
}

/// Optional malformed bytes never create an authoritative partial purge floor.
fn partial(
	store: &StoreInner,
	pin: &LocalChangefeedSnapshot,
	id: LiveId,
	segment: HistorySegment,
	history: &mut LiveHistoryMetadata,
	now: i128,
	budget: &mut PurgeBudget,
) -> FerrosGrynnResult<()> {
	let mut page = history.retained_page.max(1);
	let mut groups = history.retained_groups;
	let mut previous = (history.purged_through, history.purged_time_ns);
	let (first, last, first_time, last_time) = segment.bounds();
	while page < segment.page_ceiling() && budget.removed < MAX_REMOVALS {
		budget.step()?;
		let Some((info, next)) =
			store.inspect_live_group(pin, segment, page, budget.maximum - budget.bytes)?
		else {
			break;
		};
		if info.id != id
			|| info.version != history.version
			|| info.stamp <= previous.0
			|| groups != 0 && info.time < previous.1
			|| groups == 0 && (info.stamp != first || info.time != first_time)
			|| groups >= segment.commit_count()
			|| next <= page
			|| next > segment.page_ceiling()
			|| next == segment.page_ceiling()
				&& (groups + 1 != segment.commit_count()
					|| info.stamp != last
					|| info.time != last_time)
		{
			break;
		}
		if !expired(history, info.stamp, info.time, now)? {
			break;
		}
		page = next;
		groups += 1;
		previous = (info.stamp, info.time);
		history.purged_through = info.stamp;
		history.purged_time_ns = info.time;
		history.retained_page = page;
		history.retained_groups = groups;
		budget.removed += 1;
	}
	Ok(())
}

/// Authoritative namespaces are never treated as optional file damage.
fn check_owner(id: LiveId, segment: HistorySegment) -> FerrosGrynnResult<()> {
	if segment.owner() != HistoryOwner::Live(id) {
		return Err(invalid("LIVE purge segment has another owner"));
	}
	Ok(())
}

/// Catalog inconsistency uses the common typed corruption status.
fn invalid(reason: &str) -> FerrosGrynnError {
	FerrosGrynnError::invalid_storage_file(reason)
}
