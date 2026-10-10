//! Historical LIVE preparation over captured images and checked input coverage.

use std::{path::PathBuf, sync::Arc};

use crate::{
	changefeed::{
		ChangefeedGapReason,
		ChangefeedReadItem,
		ChangefeedReadOptions,
		ChangefeedSince,
		ReadBatches,
	},
	datastore::{
		BindingAdmission,
		DatabaseRoot,
		LiveCaptureEpoch,
		LiveCapturePolicy,
		LiveDefinition,
		LiveHistoryMetadata,
		StoreInner,
		source_history::SourceReader,
	},
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	live::{LiveBatch, LiveBindingOptions, LiveGap, LiveGapReason, LivePayload, LiveSince},
	storage::LocalChangefeedSnapshot,
};

/// Complete-domain reconstruction from a proven empty creation anchor.
pub(super) mod domain;
/// Conservative row-local eligibility and image-backed ordinary SELECT.
pub(super) mod rows;

/// Materializes bounded new-incarnation history before the publication lock.
/// Unsupported historical input coverage remains explicit; current tail
/// evaluation still uses the complete normal candidate.
pub(super) fn prepare(
	store: &StoreInner,
	before: &Arc<DatabaseRoot>,
	next: &mut DatabaseRoot,
	physical: Option<LocalChangefeedSnapshot>,
	scratch_root: PathBuf,
) -> FerrosGrynnResult<()> {
	let limits = LiveBindingOptions::default();
	let mut shared = BindingAdmission::new(&limits);
	shared.admit(
		next.live_catalog()
			.queries
			.len()
			.checked_mul(64)
			.ok_or_else(|| FerrosGrynnError::operation_overflow("LIVE historical registrations"))?,
		next.live_catalog().queries.len() as u64,
	)?;
	let ids = next
		.live_catalog()
		.queries
		.values()
		.filter(|definition| {
			definition.defined_at == next.commit_ts
				&& !matches!(definition.options.since(), LiveSince::Now)
		})
		.map(|definition| definition.id)
		.collect::<Vec<_>>();
	for id in ids {
		let definition = next.live_catalog().queries.get(&id).cloned().ok_or_else(|| {
			FerrosGrynnError::invalid_storage_file("historical registration disappeared")
		})?;
		let remainder = shared.remaining_options()?;
		let available = remainder.scratch_bytes().min(definition.options.scratch_bytes());
		let limits = LiveBindingOptions::default()
			.with_budgets(available, remainder.work().min(definition.options.work()))?;
		let mut budget = BindingAdmission::new(&limits);
		let source = definition.capture_source;
		let feed = before.changefeeds().for_source(source);
		let mut floor = match definition.options.since() {
			LiveSince::Versionstamp(stamp) => stamp.get(),
			LiveSince::Timestamp(time)
				if time.as_inner().timestamp().as_nanosecond() > next.committed_at_ns =>
			{
				next.commit_ts
			}
			LiveSince::Timestamp(_) => 0,
			_ => {
				return Err(FerrosGrynnError::invalid_argument(
					"historical registration requires stamp or timestamp",
				));
			}
		};
		if matches!(definition.options.since(), LiveSince::Versionstamp(_))
			&& floor > before.commit_ts
		{
			return Err(FerrosGrynnError::invalid_argument(
				"LIVE SINCE is newer than the historical snapshot",
			));
		}
		let mut envelopes = Vec::new();
		let mut restored = false;
		if let Some(feed) = feed {
			let since = match definition.options.since() {
				LiveSince::Versionstamp(stamp) => ChangefeedSince::Versionstamp(*stamp),
				LiveSince::Timestamp(time) => ChangefeedSince::Timestamp(time.clone()),
				_ => {
					return Err(FerrosGrynnError::invalid_argument(
						"historical selection is malformed",
					));
				}
			};
			let read_limits = budget.remaining_options()?;
			let mut reader = SourceReader::new(
				store,
				Arc::clone(before),
				feed.id,
				ChangefeedReadOptions::new(since)
					.with_budgets(read_limits.scratch_bytes(), read_limits.work())?,
				physical.clone(),
				false,
			)?;
			if matches!(definition.options.since(), LiveSince::Timestamp(_))
				&& floor != next.commit_ts
			{
				floor = reader.timestamp_floor()?;
			}
			let mut read_work = reader.statistics().work;
			budget.admit(0, read_work)?;
			let class = definition.source_class.and_then(|class| before.schema.class_by_id(class));
			restored = class.as_ref().is_some_and(|class| {
				class.published_at <= floor
					|| class.version() == crate::datastore::SchemaVersion::INITIAL
			}) && definition.functions.is_empty()
				&& class
					.as_ref()
					.map(|class| rows::eligible(&definition.query, class, &mut budget))
					.transpose()?
					.unwrap_or(false);
			// Coverage is preflighted even when this source has no selected
			// changes. Unsupported dependencies leave the whole earlier
			// interval unavailable; final capture OFF cannot
			// discard that fact.
			if restored {
				loop {
					let read_limits = budget.remaining_options()?;
					reader.constrain_work(read_limits.work())?;
					let resident = std::mem::size_of::<SourceReader<'_>>() + 512;
					let read_scratch =
						read_limits.scratch_bytes().checked_sub(resident).ok_or_else(|| {
							FerrosGrynnError::traversal_resource_limit_exceeded(
								"historical source reader",
								read_limits.scratch_bytes() as u64,
							)
						})?;
					let item = reader.next_batch(Some(read_scratch))?;
					let used = reader.statistics().work;
					budget.admit(
						0,
						used.checked_sub(read_work).ok_or_else(|| {
							FerrosGrynnError::operation_overflow("historical source work delta")
						})?,
					)?;
					read_work = used;
					let Some(item) = item else {
						break;
					};
					let (stamp, time) = reader.delivery_clock().ok_or_else(|| {
						FerrosGrynnError::invalid_storage_file(
							"historical source lacks trusted commit bounds",
						)
					})?;
					let retained = usize::try_from(reader.statistics().peak_scratch_bytes)
						.map_err(|error| {
							FerrosGrynnError::operation_overflow("historical source scratch")
								.with_technical_source(error)
						})?;
					project(
						&definition,
						before,
						item,
						stamp,
						time,
						retained,
						scratch_root.clone(),
						&mut envelopes,
						&mut budget,
					)?;
				}
			} else if class
				.as_ref()
				.map(|class| domain::eligible(&definition.query, class, &mut budget))
				.transpose()?
				.unwrap_or(false)
			{
				drop(reader);
				restored = domain::prepare(
					store,
					before,
					&definition,
					feed,
					floor,
					physical.clone(),
					scratch_root.clone(),
					&mut envelopes,
					&mut budget,
				)?;
			}
		} else if let LiveSince::Timestamp(time) = definition.options.since()
			&& time.as_inner().timestamp().as_nanosecond() > before.committed_at_ns
		{
			// Monotonic root time proves this boundary, including a future
			// subscription that must keep waiting for its timestamp.
			floor = if time.as_inner().timestamp().as_nanosecond() > next.committed_at_ns {
				next.commit_ts
			} else {
				before.commit_ts
			};
		}
		budget.admit(
			next.live_catalog()
				.queries
				.len()
				.checked_mul(256)
				.ok_or_else(|| FerrosGrynnError::operation_overflow("historical registry COW"))?,
			1,
		)?;
		let entry =
			Arc::make_mut(next.live_catalog_mut()).queries.get_mut(&id).ok_or_else(|| {
				FerrosGrynnError::invalid_storage_file("historical registration is absent")
			})?;
		let entry = Arc::make_mut(entry);
		entry.starting_after = floor;
		entry.acknowledged = floor;
		if restored && let Some(feed) = feed {
			install(next, &definition, feed, floor, before.commit_ts, envelopes, &mut budget)?;
		}
		let remaining = budget.remaining_options()?;
		shared.admit(available - remaining.scratch_bytes(), limits.work() - remaining.work())?;
	}
	Ok(())
}

/// Holds the returned source image bank apart from SQL scratch. Temporary
/// projection allocations die after this group; only encoded envelopes remain.
fn project(
	definition: &LiveDefinition,
	base: &DatabaseRoot,
	item: ChangefeedReadItem,
	stamp: u64,
	time: i128,
	source_bytes: usize,
	scratch_root: PathBuf,
	envelopes: &mut Vec<(u64, Arc<[u8]>)>,
	outer: &mut BindingAdmission,
) -> FerrosGrynnResult<()> {
	let remaining = outer.remaining_options()?;
	let scratch = remaining.scratch_bytes().checked_sub(source_bytes).ok_or_else(|| {
		FerrosGrynnError::traversal_resource_limit_exceeded(
			"historical source and result scratch",
			remaining.scratch_bytes() as u64,
		)
	})?;
	let limits = LiveBindingOptions::default().with_budgets(scratch, remaining.work())?;
	let mut temporary = BindingAdmission::new(&limits);
	let mut payload = match item {
		ChangefeedReadItem::Commit(batch) => {
			let old = rows::evaluate(
				base,
				definition,
				&batch,
				rows::ImageSelection::BeforeChanges,
				scratch_root.clone(),
				&mut temporary,
			)?;
			let new = rows::evaluate(
				base,
				definition,
				&batch,
				rows::ImageSelection::AfterChanges,
				scratch_root,
				&mut temporary,
			)?;
			let events = super::live_result::net_changes(old.rows, new.rows, &mut temporary)?;
			if events.is_empty() {
				outer.admit(0, limits.work() - temporary.remaining_options()?.work())?;
				return Ok(());
			}
			LivePayload::Batch(LiveBatch {
				id: definition.id,
				version: definition.version,
				stamp: batch.versionstamp(),
				committed_at: batch.committed_at().clone(),
				events,
				bindings: Vec::new(),
			})
		}
		ChangefeedReadItem::Gap(gap) => LivePayload::Gap(LiveGap {
			id: definition.id,
			reason: match gap.reason {
				ChangefeedGapReason::ImportSuppressed => LiveGapReason::ImportSuppressed,
				ChangefeedGapReason::Retention => LiveGapReason::Retention,
				ChangefeedGapReason::SegmentCorrupt => LiveGapReason::SegmentCorrupt,
				_ => LiveGapReason::InsufficientCoverage,
			},
			from: gap.from,
			through: gap.through,
		}),
	};
	encode(definition, &mut payload, stamp, time, envelopes, &mut temporary)?;
	let bytes = envelopes
		.last()
		.ok_or_else(|| FerrosGrynnError::invalid_storage_file("historical encoding is absent"))?
		.1
		.len()
		.checked_add(128)
		.ok_or_else(|| FerrosGrynnError::operation_overflow("historical retained envelope"))?;
	outer.admit(bytes, limits.work() - temporary.remaining_options()?.work())
}

/// Installs proven epochs and groups with one COW admission and linear growth.
fn install(
	next: &mut DatabaseRoot,
	definition: &LiveDefinition,
	feed: &crate::datastore::ChangefeedDefinition,
	floor: u64,
	cutoff: u64,
	envelopes: Vec<(u64, Arc<[u8]>)>,
	budget: &mut BindingAdmission,
) -> FerrosGrynnResult<()> {
	budget.admit(
		feed.epochs
			.len()
			.checked_mul(std::mem::size_of::<LiveCaptureEpoch>())
			.ok_or_else(|| FerrosGrynnError::operation_overflow("historical capture epochs"))?,
		feed.epochs.len() as u64,
	)?;
	let mut epochs = Vec::<LiveCaptureEpoch>::new();
	epochs.try_reserve_exact(feed.epochs.len()).map_err(|error| {
		FerrosGrynnError::traversal_resource_limit_exceeded(
			"historical capture epochs",
			feed.epochs.len() as u64,
		)
		.with_technical_source(error)
	})?;
	for epoch in &feed.epochs {
		let policy = epoch
			.configuration()
			.map(|config| LiveCapturePolicy::from_source(feed.id, epoch.revision(), config))
			.transpose()?;
		let start = epoch.started_at().get().max(floor.saturating_add(1));
		if start > cutoff {
			continue;
		}
		if let Some(last) = epochs.last_mut()
			&& last.started_at == start
		{
			last.policy = policy;
		} else if epochs.last().is_none_or(|last| last.policy != policy)
			&& (policy.is_some() || !epochs.is_empty())
		{
			epochs.push(LiveCaptureEpoch {
				started_at: start,
				policy,
			});
		}
	}
	let first = epochs.iter().position(|epoch| epoch.policy.is_some()).unwrap_or(epochs.len());
	epochs.drain(..first);
	if epochs.is_empty() {
		return Ok(());
	}
	let mut history = LiveHistoryMetadata {
		version: definition.version,
		last_stamp: 0,
		purged_through: 0,
		purged_time_ns: 0,
		retained_page: 0,
		retained_groups: 0,
		epochs,
	};
	let original = if Arc::strong_count(next.live_history()) > 1 {
		next.live_history().len()
	} else {
		0
	};
	budget.admit(
		crate::values::storage::tree_allocation_bytes(
			original
				.checked_add(envelopes.len())
				.ok_or_else(|| FerrosGrynnError::operation_overflow("historical result count"))?,
			std::mem::size_of::<((crate::live::LiveId, u64), Arc<[u8]>)>(),
		)?,
		envelopes.len() as u64,
	)?;
	for (stamp, bytes) in envelopes {
		let at = history.epochs.partition_point(|epoch| epoch.started_at <= stamp);
		budget.admit(0, u64::from(usize::BITS - history.epochs.len().leading_zeros()) + 1)?;
		if at == 0 || history.epochs[at - 1].policy.is_none() {
			continue;
		}
		Arc::make_mut(next.live_history_mut()).insert((definition.id, stamp), bytes);
		history.last_stamp = stamp;
	}
	Arc::make_mut(next.live_catalog_mut()).histories.insert(definition.id, Arc::new(history));
	Ok(())
}

/// Admits the encoded Arc, temporary copy and queue head before ownership.
fn encode(
	definition: &LiveDefinition,
	payload: &mut LivePayload,
	stamp: u64,
	time: i128,
	output: &mut Vec<(u64, Arc<[u8]>)>,
	budget: &mut BindingAdmission,
) -> FerrosGrynnResult<()> {
	if let LivePayload::Batch(batch) = payload {
		let limits = budget.remaining_options()?;
		let (bytes, work) =
			crate::live::prepare_batch_presentation(batch, definition.query.output(), &limits)?;
		budget.admit(bytes, work)?;
	}
	if !crate::datastore::live_queue::Footprint::fits_empty(payload, &definition.options)? {
		return Err(FerrosGrynnError::traversal_resource_limit_exceeded(
			"historical LIVE whole commit exceeds an empty queue",
			definition.options.queue_bytes() as u64,
		));
	}
	let bytes = crate::storage::live_history::encode(
		payload,
		definition.id,
		definition.version,
		stamp,
		time,
		budget.remaining_options()?.scratch_bytes(),
	)?;
	budget.admit(
		bytes
			.len()
			.checked_mul(2)
			.and_then(|bytes| bytes.checked_add(128))
			.ok_or_else(|| FerrosGrynnError::operation_overflow("historical envelope bytes"))?,
		1,
	)?;
	output.try_reserve_exact(1).map_err(|error| {
		FerrosGrynnError::traversal_resource_limit_exceeded(
			"historical LIVE groups",
			output.len() as u64,
		)
		.with_technical_source(error)
	})?;
	output.push((stamp, Arc::from(bytes)));
	Ok(())
}

#[cfg(test)]
mod tests;
