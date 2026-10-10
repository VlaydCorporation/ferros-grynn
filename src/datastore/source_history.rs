//! Fixed-root NET/FULL history reading without an ordinary transaction.

use std::{ops::Bound, sync::Arc};

use super::{CapturedCommit, ChangefeedDefinition, DatabaseRoot, StoreInner};
use crate::{
	changefeed::{
		ChangeBatch,
		ChangefeedCursor,
		ChangefeedGap,
		ChangefeedGapReason,
		ChangefeedId,
		ChangefeedReadItem,
		ChangefeedReadMode,
		ChangefeedReadOptions,
		ChangefeedReadStatistics,
		ChangefeedSince,
		ReadBatches,
	},
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	storage::{ChangefeedGroupInfo, HistorySegment, LocalChangefeedSnapshot},
};

/// Bounded read work shared by seek, historical preflight and materialization.
struct WorkBudget {
	/// Work already performed in this reader.
	used: u64,
	/// Hard caller cap, checked before physical reads.
	maximum: u64,
}

impl WorkBudget {
	/// Admits metadata groups or physical pages before pulling them.
	fn admit(&mut self, additional: u64) -> FerrosGrynnResult<()> {
		let actual = self
			.used
			.checked_add(additional)
			.ok_or_else(|| FerrosGrynnError::operation_overflow("changefeed reader work"))?;
		if actual > self.maximum {
			return Err(FerrosGrynnError::storage_decode_limit_exceeded(
				"changefeed reader work",
				actual,
				self.maximum,
			));
		}
		self.used = actual;
		Ok(())
	}
}

/// Small allocation-free iterator position, cloned for metadata-only preflight.
#[derive(Clone)]
struct HistoryWalk {
	/// RAM key seek position, or previous checked physical commit stamp.
	last: u64,
	/// Previous physical UTC time for ordering validation.
	last_time: Option<i128>,
	/// Current catalog segment ordinal.
	segment: usize,
	/// Next complete group's first continuation page.
	page: u64,
	/// Successfully inspected complete groups in this segment.
	groups: u64,
}

impl Default for HistoryWalk {
	fn default() -> Self {
		Self {
			last: 0,
			last_time: None,
			segment: 0,
			page: 1,
			groups: 0,
		}
	}
}

/// A metadata-bearing whole commit or an unavailable trusted segment.
enum HistoryItem {
	/// RAM images are already immutable; copies retain their owned image Arcs.
	Memory(Arc<CapturedCommit>),
	/// Local images remain lazy until selection accepts this commit.
	Local {
		/// Catalog ordinal, retained after metadata advances to a later
		/// segment.
		ordinal: usize,
		/// Trusted file and stamp/time bounds from the pinned catalog.
		segment: HistorySegment,
		/// First continuation page of the selected complete commit.
		first: u64,
		/// Checked header used for coverage and ordering before image decoding.
		info: ChangefeedGroupInfo,
	},
	/// Complete unavailable interval, without guessing any images.
	Lost(HistorySegment),
}

/// Fixed-root history adapter shared by public reading and LIVE preparation.
pub(super) struct SourceReader<'store> {
	/// Backend access is borrowed only while this bounded reader executes.
	store: &'store StoreInner,
	/// Committed root; private mutations never manufacture published events.
	root: Arc<DatabaseRoot>,
	/// Retained physical bindings captured with this logical root at begin.
	physical: Option<LocalChangefeedSnapshot>,
	/// Exact source incarnation.
	source: ChangefeedId,
	/// Historical revision upper boundary, independent of private config
	/// changes.
	definition: Option<Arc<ChangefeedDefinition>>,
	/// Immutable selection and materialization limits.
	options: ChangefeedReadOptions,
	/// Allocation-free physical/RAM iteration state.
	walk: HistoryWalk,
	/// Aggregate work consumed by both coverage preflight and reading.
	work: WorkBudget,
	/// Payload-free observations, independent of snapshot pin sharing.
	statistics: ChangefeedReadStatistics,
	/// Whole-commit position last safely consumed; None before a timestamp
	/// seek.
	cursor: Option<ChangefeedCursor>,
	/// Lost prefix or terminal expiration to return before the available tail.
	pending: Option<ChangefeedGap>,
	/// Trusted clock of the last returned item, including retained-prefix GAP.
	last_delivery: Option<(u64, i128)>,
	/// Latest source boundary proven strictly older than timestamp SINCE.
	proven_before: u64,
	/// Batches and gaps delivered, excluding invisible empty NET batches.
	delivered: u64,
	/// EOF or first failure fuses this reader.
	done: bool,
}

impl<'store> SourceReader<'store> {
	/// Validates one source, position, physical pin and bounded read options.
	pub(super) fn new(
		store: &'store StoreInner,
		root: Arc<DatabaseRoot>,
		source: ChangefeedId,
		options: ChangefeedReadOptions,
		physical: Option<LocalChangefeedSnapshot>,
		expired: bool,
	) -> FerrosGrynnResult<Self> {
		if let ChangefeedSince::Cursor(cursor) = &options.since
			&& (cursor.database_id().as_inner() != &root.database_uuid
				|| cursor.source_id() != source
				|| cursor.mode() != options.mode)
		{
			return Err(FerrosGrynnError::invalid_argument(
				"changefeed cursor database, source or read mode mismatch",
			));
		}
		let definition = root.changefeeds().definitions.get(&source).cloned();
		if root.changefeed_segments().get(&source).is_some_and(|segments| !segments.is_empty())
			&& physical.is_none()
		{
			return Err(FerrosGrynnError::invalid_storage_file(
				"changefeed snapshot bindings are unavailable",
			));
		}
		let cursor = match &options.since {
			ChangefeedSince::Cursor(cursor) => {
				if cursor.source_id() != source {
					return Err(FerrosGrynnError::invalid_argument(
						"changefeed cursor source mismatch",
					));
				}
				Some(cursor.clone())
			}
			ChangefeedSince::Versionstamp(stamp) => Some(ChangefeedCursor::new(
				root.database_uuid.into(),
				source,
				options.mode,
				*stamp,
			)?),
			ChangefeedSince::Timestamp(_) => None,
		};
		if cursor.as_ref().is_some_and(|cursor| cursor.versionstamp().get() > root.commit_ts) {
			return Err(FerrosGrynnError::invalid_argument(
				"changefeed cursor is newer than the selected snapshot",
			));
		}
		let retained = std::mem::size_of::<Self>() + 512;
		if options.memory_bytes < retained {
			return Err(FerrosGrynnError::storage_decode_limit_exceeded(
				"changefeed reader scratch",
				retained as u64,
				options.memory_bytes as u64,
			));
		}
		let mut options = options;
		options.memory_bytes -= retained;
		let pending = if expired {
			Some(ChangefeedGap {
				source,
				reason: ChangefeedGapReason::FeedExpiredOrRetired,
				from: None,
				through: None,
			})
		} else if let Some(definition) = &definition {
			let lost = match &options.since {
				ChangefeedSince::Versionstamp(stamp) => stamp.get() < definition.purged_through,
				ChangefeedSince::Cursor(cursor) => {
					cursor.versionstamp().get() < definition.purged_through
				}
				ChangefeedSince::Timestamp(time) => {
					time.as_inner().timestamp().as_nanosecond() <= definition.purged_time_ns
				}
			};
			(definition.purged_through != 0 && lost).then_some(ChangefeedGap {
				source,
				reason: ChangefeedGapReason::Retention,
				from: None,
				through: Some(definition.purged_through.into()),
			})
		} else {
			None
		};
		let mut walk = HistoryWalk::default();
		if physical.is_none() {
			walk.last = cursor.as_ref().map_or(0, |cursor| cursor.versionstamp().get());
		} else if let Some(definition) = &definition
			&& definition.retained_page != 0
		{
			walk.page = definition.retained_page;
			walk.groups = definition.retained_groups;
			walk.last = definition.purged_through;
			walk.last_time = Some(definition.purged_time_ns);
		}
		let mut reader = Self {
			store,
			root,
			physical,
			source,
			definition,
			work: WorkBudget {
				used: 0,
				maximum: options.work,
			},
			options,
			statistics: ChangefeedReadStatistics {
				peak_scratch_bytes: retained as u64,
				..ChangefeedReadStatistics::default()
			},
			walk,
			cursor,
			pending,
			last_delivery: None,
			proven_before: 0,
			delivered: 0,
			done: false,
		};
		if reader.options.mode == ChangefeedReadMode::Full && reader.options.limit != 0 {
			reader.preflight_full()?;
		}
		Ok(reader)
	}

	/// Matches the public reader's early EOF before checking transaction age.
	pub(super) fn finished(&self) -> bool {
		self.done || self.delivered >= self.options.limit
	}

	/// The transaction adapter must fuse lifetime errors before delegation.
	pub(super) fn finish(&mut self) {
		self.done = true;
	}

	/// Gives commit preparation a trusted envelope clock without exposing it
	/// through the public source reader.
	pub(super) const fn delivery_clock(&self) -> Option<(u64, i128)> {
		self.last_delivery
	}

	/// Shares one remaining work allowance with the caller before each pull.
	pub(super) fn constrain_work(&mut self, remaining: u64) -> FerrosGrynnResult<()> {
		let maximum = self.work.used.checked_add(remaining).ok_or_else(|| {
			FerrosGrynnError::operation_overflow("shared changefeed work ceiling")
		})?;
		self.work.maximum = self.work.maximum.min(maximum);
		Ok(())
	}

	/// Resolves only a proven lower timestamp boundary. Missing source epochs
	/// cannot be skipped by choosing the first nonempty matching commit.
	pub(super) fn timestamp_floor(&mut self) -> FerrosGrynnResult<u64> {
		let ChangefeedSince::Timestamp(time) = &self.options.since else {
			return Err(FerrosGrynnError::invalid_argument(
				"timestamp floor requires timestamp SINCE",
			));
		};
		let requested = time.as_inner().timestamp().as_nanosecond();
		if requested > self.root.committed_at_ns {
			return Ok(self.root.commit_ts);
		}
		if let Some(definition) = &self.definition
			&& definition.purged_through != 0
			&& definition.purged_time_ns < requested
		{
			self.proven_before = definition.purged_through;
		}
		let mut walk = self.walk.clone();
		while let Some(item) = self.next_history(&mut walk)? {
			let (stamp, time) = match item {
				HistoryItem::Memory(commit) => (
					commit.net.versionstamp().get(),
					commit.net.committed_at().as_inner().timestamp().as_nanosecond(),
				),
				HistoryItem::Local {
					info,
					..
				} => (info.stamp, info.time),
				HistoryItem::Lost(segment) => (segment.bounds().1, segment.bounds().3),
			};
			if time >= requested {
				break;
			}
			self.proven_before = self.proven_before.max(stamp);
		}
		Ok(self.proven_before)
	}
}

impl SourceReader<'_> {
	/// Checks a group's exact historical position against the lower boundary.
	fn selected(&self, stamp: u64, time: i128) -> bool {
		if self.definition.as_ref().is_some_and(|entry| stamp <= entry.purged_through) {
			return false;
		}
		match &self.options.since {
			ChangefeedSince::Versionstamp(since) => stamp > since.get(),
			ChangefeedSince::Cursor(since) => stamp > since.versionstamp().get(),
			ChangefeedSince::Timestamp(since) => {
				time >= since.as_inner().timestamp().as_nanosecond()
			}
		}
	}

	/// Checks the entire requested FULL interval before returning any rows.
	fn preflight_full(&mut self) -> FerrosGrynnResult<()> {
		let mut walk = self.walk.clone();
		while let Some(item) = self.next_history(&mut walk)? {
			let (stamp, time, full, gap) = match item {
				HistoryItem::Memory(commit) => (
					commit.net.versionstamp().get(),
					commit.net.committed_at().as_inner().timestamp().as_nanosecond(),
					commit.full.is_some(),
					commit.gap.is_some(),
				),
				HistoryItem::Local {
					info,
					..
				} => (info.stamp, info.time, info.full, info.gap),
				HistoryItem::Lost(_) => continue,
			};
			if self.selected(stamp, time) && !full && !gap {
				return Err(FerrosGrynnError::query_feature_not_supported(
					"FULL changefeed history was not captured for the selected commit range",
				));
			}
		}
		Ok(())
	}

	/// Pulls bounded metadata, retaining no array of all history locators.
	fn next_history(&mut self, walk: &mut HistoryWalk) -> FerrosGrynnResult<Option<HistoryItem>> {
		let Some(physical) = &self.physical else {
			let next = self
				.root
				.change_history()
				.range((
					Bound::Excluded((self.source, walk.last)),
					Bound::Included((self.source, self.root.commit_ts)),
				))
				.next();
			let Some(((_, stamp), commit)) = next else {
				return self.checked_end(walk);
			};
			self.work.admit(1)?;
			self.statistics.groups_inspected = self.statistics.groups_inspected.saturating_add(1);
			walk.last = *stamp;
			return Ok(Some(HistoryItem::Memory(Arc::clone(commit))));
		};
		let Some(segments) = self.root.changefeed_segments().get(&self.source) else {
			return self.checked_end(walk);
		};
		loop {
			let Some(segment) = segments.get(walk.segment).copied() else {
				return self.checked_end(walk);
			};
			if segment.owner() != crate::storage::HistoryOwner::Changefeed(self.source) {
				return Err(FerrosGrynnError::invalid_storage_file(
					"source history segment owner disagrees with its catalog",
				));
			}
			let (first_stamp, last_stamp, first_time, last_time) = segment.bounds();
			if walk.page == 1
				|| walk.segment == 0
					&& walk.groups
						== self
							.definition
							.as_ref()
							.map_or(0, |definition| definition.retained_groups)
			{
				self.statistics.segment_visits = self.statistics.segment_visits.saturating_add(1);
			}
			if !self.selected(last_stamp, last_time) {
				self.work.admit(1)?;
				if matches!(&self.options.since, ChangefeedSince::Timestamp(time)
					if last_time < time.as_inner().timestamp().as_nanosecond())
				{
					self.proven_before = self.proven_before.max(last_stamp);
				}
				walk.advance_segment(last_stamp, last_time);
				continue;
			}
			self.work.admit(2)?;
			let info = self.store.inspect_changefeed_group(
				physical,
				segment,
				walk.page,
				self.options.memory_bytes,
			)?;
			let Some(info) = info else {
				walk.advance_segment(last_stamp, last_time);
				return Ok(Some(HistoryItem::Lost(segment)));
			};
			let valid = info.source == self.source
				&& info.stamp > walk.last
				&& walk.last_time.is_none_or(|time| info.time >= time)
				&& self
					.definition
					.as_ref()
					.is_some_and(|definition| info.revision <= definition.version.get())
				&& (walk.groups != 0 || info.stamp == first_stamp && info.time == first_time)
				&& walk.groups < segment.commit_count()
				&& (info.next_page != segment.page_ceiling()
					|| walk.groups + 1 == segment.commit_count()
						&& info.stamp == last_stamp
						&& info.time == last_time);
			if !valid {
				walk.advance_segment(last_stamp, last_time);
				return Ok(Some(HistoryItem::Lost(segment)));
			}
			let first = walk.page;
			self.statistics.groups_inspected = self.statistics.groups_inspected.saturating_add(1);
			let ordinal = walk.segment;
			walk.page = info.next_page;
			walk.groups += 1;
			walk.last = info.stamp;
			walk.last_time = Some(info.time);
			if walk.page == segment.page_ceiling() {
				walk.advance_segment(last_stamp, last_time);
			}
			return Ok(Some(HistoryItem::Local {
				ordinal,
				segment,
				first,
				info,
			}));
		}
	}

	/// EOF must reach the catalog's final source group, including empty NET
	/// groups. A matching current image cannot prove that a missing closed
	/// sequence of historical changes was captured.
	fn checked_end(&self, walk: &HistoryWalk) -> FerrosGrynnResult<Option<HistoryItem>> {
		if let Some(definition) = &self.definition
			&& (definition.last_stamp > self.root.commit_ts
				|| definition.purged_through > definition.last_stamp
				|| walk.last.max(definition.purged_through) < definition.last_stamp)
		{
			return Err(FerrosGrynnError::invalid_storage_file(
				"source history ended before its authoritative catalog boundary",
			));
		}
		Ok(None)
	}

	/// Creates a whole-commit cursor only after an item has been materialized.
	fn advance_cursor(&mut self, stamp: u64) -> FerrosGrynnResult<()> {
		let stamp =
			self.cursor.as_ref().map_or(stamp, |cursor| stamp.max(cursor.versionstamp().get()));
		self.cursor = Some(ChangefeedCursor::new(
			self.root.database_uuid.into(),
			self.source,
			self.options.mode,
			stamp.into(),
		)?);
		Ok(())
	}

	/// Copies the small entry vector after admission; immutable images stay
	/// owned.
	fn memory_batch(&mut self, commit: &CapturedCommit) -> FerrosGrynnResult<ChangeBatch> {
		let batch = match self.options.mode {
			ChangefeedReadMode::Net => &commit.net,
			ChangefeedReadMode::Full => commit.full.as_ref().ok_or_else(|| {
				FerrosGrynnError::query_feature_not_supported(
					"FULL changefeed history was not captured",
				)
			})?,
		};
		let required = batch
			.entries
			.len()
			.checked_mul(std::mem::size_of::<crate::changefeed::ChangeEntry>())
			.and_then(|bytes| bytes.checked_add(256))
			.ok_or_else(|| FerrosGrynnError::operation_overflow("changefeed batch bytes"))?;
		if required > self.options.memory_bytes {
			return Err(FerrosGrynnError::storage_decode_limit_exceeded(
				"changefeed reader scratch",
				required as u64,
				self.options.memory_bytes as u64,
			));
		}
		self.statistics.peak_scratch_bytes = self
			.statistics
			.peak_scratch_bytes
			.max((required + std::mem::size_of::<Self>() + 512) as u64);
		Ok(batch.clone())
	}

	/// Returns a corruption interval after clipping only provable stamp bounds.
	fn lost_segment(&mut self, segment: HistorySegment) -> FerrosGrynnResult<ChangefeedReadItem> {
		let (first, last, first_time, last_time) = segment.bounds();
		let from = match &self.options.since {
			ChangefeedSince::Timestamp(time)
				if time.as_inner().timestamp().as_nanosecond() > first_time =>
			{
				None
			}
			_ => Some(
				self.cursor
					.as_ref()
					.map_or(first, |cursor| {
						first.max(cursor.versionstamp().get().saturating_add(1))
					})
					.into(),
			),
		};
		self.advance_cursor(last)?;
		self.last_delivery = Some((last, last_time));
		Ok(ChangefeedReadItem::Gap(ChangefeedGap {
			source: self.source,
			reason: ChangefeedGapReason::SegmentCorrupt,
			from,
			through: Some(last.into()),
		}))
	}

	/// Materializes one selected item, skipping successful empty NET commits.
	fn pull(&mut self) -> FerrosGrynnResult<Option<ChangefeedReadItem>> {
		if self.done || self.delivered >= self.options.limit {
			return Ok(None);
		}
		if let Some(gap) = self.pending.take() {
			if let Some(through) = gap.through {
				self.advance_cursor(through.get())?;
				self.last_delivery = self
					.definition
					.as_ref()
					.map(|definition| (through.get(), definition.purged_time_ns));
			}
			if gap.reason == ChangefeedGapReason::FeedExpiredOrRetired {
				self.done = true;
			}
			self.delivered += 1;
			return Ok(Some(ChangefeedReadItem::Gap(gap)));
		}
		loop {
			let mut walk = self.walk.clone();
			let Some(item) = self.next_history(&mut walk)? else {
				self.done = true;
				if !matches!(&self.options.since, ChangefeedSince::Timestamp(time)
					if self.cursor.is_none() && time.as_inner().timestamp().as_nanosecond() > self.root.committed_at_ns)
				{
					self.advance_cursor(self.root.commit_ts)?;
				}
				return Ok(None);
			};
			self.walk = walk;
			let (stamp, time) = match &item {
				HistoryItem::Memory(commit) => (
					commit.net.versionstamp().get(),
					commit.net.committed_at().as_inner().timestamp().as_nanosecond(),
				),
				HistoryItem::Local {
					info,
					..
				} => (info.stamp, info.time),
				HistoryItem::Lost(segment) => {
					self.delivered += 1;
					return self.lost_segment(*segment).map(Some);
				}
			};
			if !self.selected(stamp, time) {
				if self.cursor.is_some() {
					self.advance_cursor(stamp)?;
				}
				continue;
			}
			let value = match item {
				HistoryItem::Memory(commit) => {
					if let Some(gap) = &commit.gap {
						ChangefeedReadItem::Gap(gap.clone())
					} else {
						ChangefeedReadItem::Commit(self.memory_batch(&commit)?)
					}
				}
				HistoryItem::Local {
					segment,
					first,
					info,
					ordinal,
				} => {
					if self.options.mode == ChangefeedReadMode::Net
						&& info.net_entries == 0
						&& !info.gap
					{
						self.advance_cursor(stamp)?;
						continue;
					}
					self.work.admit(u64::from(info.pages) + 3)?;
					let physical = self.physical.as_ref().ok_or_else(|| {
						FerrosGrynnError::invalid_storage_file(
							"changefeed snapshot has no physical binding",
						)
					})?;
					let Some((commit, next)) = self.store.read_changefeed_batch(
						physical,
						segment,
						first,
						self.options.memory_bytes,
					)?
					else {
						self.walk.segment = ordinal;
						self.walk.advance_segment(segment.bounds().1, segment.bounds().3);
						self.delivered += 1;
						return self.lost_segment(segment).map(Some);
					};
					if next != info.next_page {
						return Err(FerrosGrynnError::invalid_storage_file(
							"changefeed reader locator changed during pinned read",
						));
					}
					if let Some(gap) = commit.gap {
						ChangefeedReadItem::Gap(gap)
					} else {
						ChangefeedReadItem::Commit(match self.options.mode {
							ChangefeedReadMode::Net => commit.net,
							ChangefeedReadMode::Full => commit.full.ok_or_else(|| {
								FerrosGrynnError::query_feature_not_supported(
									"FULL changefeed history was not captured",
								)
							})?,
						})
					}
				}
				HistoryItem::Lost(_) => {
					return Err(FerrosGrynnError::invalid_storage_file(
						"changefeed segment unexpectedly lacks metadata",
					));
				}
			};
			self.advance_cursor(stamp)?;
			if matches!(&value, ChangefeedReadItem::Commit(batch) if batch.entries().is_empty()) {
				continue;
			}
			self.delivered += 1;
			self.last_delivery = Some((stamp, time));
			return Ok(Some(value));
		}
	}
}

impl HistoryWalk {
	/// Advances over a complete segment while preserving trusted order bounds.
	fn advance_segment(&mut self, stamp: u64, time: i128) {
		self.segment += 1;
		self.page = 1;
		self.groups = 0;
		self.last = stamp;
		self.last_time = Some(time);
	}
}

impl ReadBatches for SourceReader<'_> {
	fn next_batch(
		&mut self,
		memory_limit: Option<usize>,
	) -> FerrosGrynnResult<Option<ChangefeedReadItem>> {
		if let Some(limit) = memory_limit {
			self.options.memory_bytes = self.options.memory_bytes.min(limit);
		}
		let result = self.pull();
		match &result {
			Ok(Some(ChangefeedReadItem::Commit(batch))) => {
				self.statistics.batches = self.statistics.batches.saturating_add(1);
				self.statistics.entries =
					self.statistics.entries.saturating_add(batch.entries().len() as u64);
			}
			Ok(Some(ChangefeedReadItem::Gap(_))) => {
				self.statistics.gaps = self.statistics.gaps.saturating_add(1);
			}
			_ => {}
		}
		if result.is_err() {
			self.done = true;
		}
		result
	}

	fn cursor(&self) -> Option<ChangefeedCursor> {
		self.cursor.clone()
	}

	fn binding(&self) -> Option<crate::changefeed::ChangefeedReadBinding> {
		self.definition.as_ref().map(|definition| definition.read_binding(&self.root))
	}

	fn statistics(&self) -> ChangefeedReadStatistics {
		let mut statistics = self.statistics;
		statistics.work = self.work.used;
		if let Some(physical) = &self.physical {
			let (pages, scratch) = physical.read_observation();
			statistics.page_reads = pages;
			statistics.bytes_read = pages.saturating_mul(crate::storage::PAGE_SIZE as u64);
			statistics.peak_scratch_bytes = statistics
				.peak_scratch_bytes
				.max(scratch.saturating_add((std::mem::size_of::<Self>() + 512) as u64));
		}
		statistics
	}
}
