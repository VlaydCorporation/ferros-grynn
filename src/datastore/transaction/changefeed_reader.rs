//! Fixed-snapshot selection, metadata preflight and whole-commit pull reading.

use std::{ops::Bound, sync::Arc};

use super::Transaction;
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
		ChangefeedReader,
		ChangefeedSelector,
		ChangefeedSince,
		ReadBatches,
	},
	datastore::{CapturedCommit, ChangefeedDefinition, ConflictKey, DatabaseRoot},
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	storage::{ChangefeedGroupInfo, ChangefeedSegment, LocalChangefeedSnapshot},
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
		segment: ChangefeedSegment,
		/// First continuation page of the selected complete commit.
		first: u64,
		/// Checked header used for coverage and ordering before image decoding.
		info: ChangefeedGroupInfo,
	},
	/// Complete unavailable interval, without guessing any images.
	Lost(ChangefeedSegment),
}

/// Private adapter borrowing a live transaction while pinning its fixed root.
struct SnapshotReader<'transaction> {
	/// Borrow prevents transaction mutation or commit while reading.
	transaction: &'transaction Transaction,
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
	/// Batches and gaps delivered, excluding invisible empty NET batches.
	delivered: u64,
	/// EOF or first failure fuses this reader.
	done: bool,
}

impl Transaction {
	/// Opens a bounded fixed-snapshot history reader without executing GQL.
	///
	/// FULL coverage is checked across the selected history before any item is
	/// returned. Returned images and cursors outlive the reader and
	/// transaction. Private mutations are excluded; this borrow prevents
	/// writes while pulling.
	///
	/// # Errors
	///
	/// Returns snapshot expiry, missing/ambiguous source, wrong cursor origin,
	/// source or mode, unavailable FULL coverage, or resource errors.
	///
	/// # Examples
	///
	/// ```
	/// # fn main() -> ferros_grynn::gql_status::FerrosGrynnResult<()> {
	/// use ferros_grynn::{
	/// 	changefeed::{
	/// 		ChangefeedConfig,
	/// 		ChangefeedReadItem,
	/// 		ChangefeedReadOptions,
	/// 		ChangefeedSince,
	/// 		ChangefeedSource,
	/// 	},
	/// 	datastore::{ClassSchema, Datastore},
	/// 	graph::AtomKind,
	/// 	values::PropertyMap,
	/// };
	/// let store = Datastore::in_memory();
	/// let mut tx = store.begin();
	/// let class = tx.define_class(ClassSchema::new("item", AtomKind::Node, [])?)?;
	/// let retention = "P1D".parse::<ferros_grynn::values::Duration>().map_err(|error| {
	/// 	ferros_grynn::gql_status::FerrosGrynnError::invalid_argument(error.to_string())
	/// })?;
	/// let feed =
	/// 	tx.configure_changefeed(ChangefeedSource::Class(class), ChangefeedConfig::new(retention)?)?;
	/// tx.create_atom("item:a".parse()?, PropertyMap::new())?;
	/// tx.commit()?;
	/// let mut tx = store.begin();
	/// let mut reader = tx.changes(feed, ChangefeedReadOptions::default().with_limit(1))?;
	/// let binding = reader.binding().ok_or_else(|| {
	/// 	ferros_grynn::gql_status::FerrosGrynnError::invalid_argument("missing binding")
	/// })?;
	/// assert_eq!(binding.id, feed);
	/// let Some(ChangefeedReadItem::Commit(batch)) = reader.next_batch()? else {
	/// 	return Err(ferros_grynn::gql_status::FerrosGrynnError::invalid_argument(
	/// 		"missing initial batch",
	/// 	));
	/// };
	/// let cursor = reader.cursor().ok_or_else(|| {
	/// 	ferros_grynn::gql_status::FerrosGrynnError::invalid_argument("missing cursor")
	/// })?;
	/// let observed = reader.statistics();
	/// assert_eq!(observed.batches, 1);
	/// assert_eq!(observed.entries, 1);
	/// drop(reader);
	/// drop(tx);
	/// assert_eq!(batch.entries().len(), 1);
	/// assert_eq!(store.changefeed_statistics()?.published_groups, 1);
	/// let mut tx = store.begin();
	/// let mut resumed =
	/// 	tx.changes(feed, ChangefeedReadOptions::new(ChangefeedSince::Cursor(cursor)))?;
	/// assert!(resumed.next_batch()?.is_none());
	/// # Ok(())
	/// # }
	/// ```
	///
	/// A reader cannot outlive the transaction whose snapshot it borrows:
	///
	/// ```compile_fail,E0597
	/// use ferros_grynn::{
	/// 	changefeed::{ChangefeedId, ChangefeedReadOptions},
	/// 	datastore::Datastore,
	/// 	gql_status::FerrosGrynnResult,
	/// };
	/// fn invalid_reader_lifetime(store: &Datastore, feed: ChangefeedId) -> FerrosGrynnResult<()> {
	/// 	let mut reader;
	/// 	{
	/// 		let mut tx = store.begin();
	/// 		reader = tx.changes(feed, ChangefeedReadOptions::default())?;
	/// 	}
	/// 	let _batch = reader.next_batch()?;
	/// 	Ok(())
	/// }
	/// ```
	#[allow(
		clippy::tabs_in_doc_comments,
		reason = "nightly rustfmt uses the required hard tabs in Rustdoc examples"
	)]
	pub fn changes<Selector: Into<ChangefeedSelector>>(
		&mut self,
		selector: Selector,
		options: ChangefeedReadOptions,
	) -> FerrosGrynnResult<ChangefeedReader<'_>> {
		self.prepare_read()?;
		self.record_read(ConflictKey::ChangefeedCatalogRange);
		let selector = selector.into();
		let root = Arc::clone(&self.snapshot);
		if let ChangefeedSince::Cursor(cursor) = &options.since
			&& (cursor.database_id().as_inner() != &root.database_uuid
				|| cursor.mode() != options.mode)
		{
			return Err(FerrosGrynnError::invalid_argument(
				"changefeed cursor database or read mode mismatch",
			));
		}
		let selected = match (&selector, &options.since) {
			(ChangefeedSelector::Id(id), ChangefeedSince::Cursor(cursor))
				if cursor.source_id() == *id
					&& !self.working.changefeeds().definitions.contains_key(id) =>
			{
				None
			}
			_ => Some(Arc::clone(self.resolve_changefeed(&selector)?)),
		};
		let source = match (&selected, &selector) {
			(Some(definition), _) => definition.id,
			(None, ChangefeedSelector::Id(id)) => *id,
			_ => {
				return Err(FerrosGrynnError::invalid_argument(
					"changefeed source could not be resolved",
				));
			}
		};
		let definition = root.changefeeds().definitions.get(&source).cloned();
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
		let retained = std::mem::size_of::<SnapshotReader<'_>>() + 512;
		if options.memory_bytes < retained {
			return Err(FerrosGrynnError::storage_decode_limit_exceeded(
				"changefeed reader scratch",
				retained as u64,
				options.memory_bytes as u64,
			));
		}
		let mut options = options;
		options.memory_bytes -= retained;
		let pending = if selected.is_none() {
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
		let physical =
			if root.changefeed_segments().get(&source).is_some_and(|segments| !segments.is_empty())
			{
				self.changefeed_snapshot
					.as_ref()
					.filter(|(stamp, _)| *stamp == root.commit_ts)
					.ok_or_else(|| {
						FerrosGrynnError::invalid_storage_file(
							"changefeed snapshot bindings were not pinned",
						)
					})?
					.1
					.clone()?
					.ok_or_else(|| {
						FerrosGrynnError::invalid_storage_file(
							"changefeed snapshot bindings are unavailable",
						)
					})?
					.observed_reader()
					.into()
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
		let mut reader = SnapshotReader {
			transaction: self,
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
			delivered: 0,
			done: false,
		};
		if reader.options.mode == ChangefeedReadMode::Full && reader.options.limit != 0 {
			reader.preflight_full()?;
		}
		Ok(ChangefeedReader::new(Box::new(reader)))
	}
}

impl SnapshotReader<'_> {
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
				return Ok(None);
			};
			self.work.admit(1)?;
			self.statistics.groups_inspected = self.statistics.groups_inspected.saturating_add(1);
			walk.last = *stamp;
			return Ok(Some(HistoryItem::Memory(Arc::clone(commit))));
		};
		let Some(segments) = self.root.changefeed_segments().get(&self.source) else {
			return Ok(None);
		};
		loop {
			let Some(segment) = segments.get(walk.segment).copied() else {
				return Ok(None);
			};
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
				walk.advance_segment(last_stamp, last_time);
				continue;
			}
			self.work.admit(2)?;
			let info = self.transaction.store.inner.inspect_changefeed_group(
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
	fn lost_segment(
		&mut self,
		segment: ChangefeedSegment,
	) -> FerrosGrynnResult<ChangefeedReadItem> {
		let (first, last, first_time, _) = segment.bounds();
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
		self.transaction.ensure_live()?;
		if let Some(gap) = self.pending.take() {
			if let Some(through) = gap.through {
				self.advance_cursor(through.get())?;
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
					let Some((commit, next)) = self.transaction.store.inner.read_changefeed_batch(
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

impl ReadBatches for SnapshotReader<'_> {
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

#[cfg(test)]
mod tests;
