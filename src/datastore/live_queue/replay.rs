//! Lazy fixed-cutoff computed history; no transaction or primary read adapter.

use std::{ops::Bound, sync::Arc};

use crate::{
	datastore::{DatabaseRoot, LiveHistoryMetadata, StoreInner, root::LiveHistoryGroups},
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	live::{
		LiveCursor,
		LiveDelivery,
		LiveGap,
		LiveGapReason,
		LiveId,
		LiveOptions,
		LivePayload,
		LiveState,
		LiveVersion,
	},
	storage::{
		HistoryOwner,
		HistorySegment,
		LocalChangefeedSnapshot,
		live_history::{self, LiveGroupInfo},
	},
};

/// Small proven unavailable interval; gaps never imply events at every stamp.
#[derive(Clone, Copy)]
struct LostRange {
	/// Inclusive unavailable lower boundary.
	from: u64,
	/// Inclusive unavailable upper boundary.
	through: u64,
	/// Why exact computed images cannot be restored.
	reason: LiveGapReason,
}

/// At most one inspected group waits behind a coverage gap.
enum Item {
	/// Borrowed immutable RAM encoding; values are decoded only on selection.
	Memory(
		/// Immutable encoded LIVE group retained until this replay item is
		/// selected.
		Arc<[u8]>,
		/// Inspected commit and event metadata describing the encoded group.
		LiveGroupInfo,
	),
	/// A pinned complete physical group and its checked locator.
	Local(
		/// History segment containing the selected complete physical group.
		HistorySegment,
		/// First physical page of the selected history group.
		u64,
		/// Expected first page after the selected group, checked again during
		/// decoding.
		u64,
		/// Inspected commit and event metadata describing the physical group.
		LiveGroupInfo,
	),
	/// Optional file bytes are unavailable under trusted catalog bounds.
	Lost(
		/// Optional file bytes are unavailable under trusted catalog bounds.
		HistorySegment,
	),
}

/// Cursor for a single physical segment; counts include a purged prefix.
struct Walk {
	/// Current segment ordinal in the captured query's reference vector.
	segment: usize,
	/// Next complete group's first page.
	page: u64,
	/// Inspected groups, including an authoritative purged prefix.
	groups: u64,
	/// Previous group or completed segment's final stamp.
	last: u64,
	/// Previous checked commit time; equality is allowed.
	time: Option<i128>,
}

/// Owned history references and bounded pins, released at replay EOF/Drop.
pub(in crate::datastore) struct Replay {
	/// Exact query incarnation and version for every restored group.
	id: LiveId,
	/// Definition version checked independently of the query UUID.
	version: LiveVersion,
	/// Immutable query presentation, reconstructed from captured images only.
	output: crate::live::LiveOutputMode,
	/// Captured tail boundary; later primary writes cannot change old images.
	cutoff: u64,
	/// Trusted bounds prove whether a historical delivery can exist.
	has_work: bool,
	/// Authoritative final computed stamp must be inspected before EOF.
	expected_last: u64,
	/// Last emitted complete delivery, distinct from durable ACK.
	position: u64,
	/// Purged logical prefix remains authoritative when files are empty.
	floor: u64,
	/// Captured RAM history handles, without retaining the database root.
	memory: LiveHistoryGroups,
	/// Only this query's physical references are needed for replay.
	#[expect(
		clippy::rc_buffer,
		reason = "Shares the snapshot's existing COW reference vector without copying it"
	)]
	segments: Option<Arc<Vec<HistorySegment>>>,
	/// Existing WAL/file reachability pin, without graph binding scans.
	physical: Option<LocalChangefeedSnapshot>,
	/// Coverage gaps derived from capture epochs and the purge floor.
	lost: Vec<LostRange>,
	/// First coverage interval not yet emitted.
	lost_at: usize,
	/// One group may be inspected before an earlier GAP is emitted.
	pending: Option<Item>,
	/// Allocation-free metadata traversal position.
	walk: Walk,
	/// Hard materialization scratch ceiling.
	scratch: usize,
	/// Aggregate metadata/page work left, checked before every read.
	work: u64,
	/// Original work cap includes successful constructor metadata admission.
	initial_work: u64,
	/// Charged units already observed by a prior pull.
	reported_work: u64,
}

impl Replay {
	/// Prepares a new registration's narrow owned history before the barrier.
	/// The local backend may clear its RAM table after durable staging; this
	/// admitted handle therefore owns only this query's already-encoded groups.
	pub(super) fn registration(
		root: &DatabaseRoot,
		cursor: &LiveCursor,
		options: &LiveOptions,
	) -> FerrosGrynnResult<(Self, usize)> {
		let mut replay = Self::new(root, cursor, options, None)?;
		if !replay.has_work {
			return Ok((replay, 0));
		}
		let range = (
			Bound::Excluded((cursor.live_id(), cursor.versionstamp().get())),
			Bound::Included((cursor.live_id(), root.commit_ts)),
		);
		let mut count = 0_usize;
		let mut retained = std::mem::size_of::<Self>() + 512;
		for (_, bytes) in root.live_history().range(range) {
			replay.admit(1)?;
			count = count
				.checked_add(1)
				.ok_or_else(|| invalid("registration history count overflows"))?;
			retained = retained
				.checked_add(bytes.len())
				.ok_or_else(|| invalid("registration history bytes overflow"))?;
		}
		retained = retained
			.checked_add(crate::values::storage::tree_allocation_bytes(
				count,
				std::mem::size_of::<((LiveId, u64), Arc<[u8]>)>(),
			)?)
			.and_then(|bytes| {
				replay
					.lost
					.capacity()
					.checked_mul(std::mem::size_of::<LostRange>())
					.and_then(|lost| bytes.checked_add(lost))
			})
			.ok_or_else(|| invalid("registration history admission overflows"))?;
		if retained > options.scratch_bytes() {
			return Err(FerrosGrynnError::storage_decode_limit_exceeded(
				"LIVE registration replay",
				retained as u64,
				options.scratch_bytes() as u64,
			));
		}
		replay.admit(count as u64)?;
		let memory = root
			.live_history()
			.range(range)
			.map(|(key, bytes)| (*key, Arc::clone(bytes)))
			.collect();
		replay.memory = Arc::new(memory);
		Ok((replay, retained))
	}

	/// Captures only immutable history references, never the primary root.
	pub(in crate::datastore) fn new(
		root: &DatabaseRoot,
		cursor: &LiveCursor,
		options: &LiveOptions,
		physical: Option<LocalChangefeedSnapshot>,
	) -> FerrosGrynnResult<Self> {
		let definition = root
			.live_catalog()
			.queries
			.get(&cursor.live_id())
			.ok_or_else(|| invalid("LIVE replay definition is absent"))?;
		cursor.validate(root.database_uuid.into(), definition.id, definition.version)?;
		let cutoff = if matches!(definition.options.since(), crate::live::LiveSince::Timestamp(time)
			if time.as_inner().timestamp().as_nanosecond() > root.committed_at_ns)
		{
			cursor.versionstamp().get()
		} else if definition.state == LiveState::Closed {
			root.commit_ts.min(definition.published_at.saturating_sub(1))
		} else {
			root.commit_ts
		};
		let history = root.live_catalog().histories.get(&definition.id);
		if let Some(history) = history {
			history.validate(definition.starting_after, root.commit_ts)?;
			if history.version != definition.version || history.last_stamp > cutoff {
				return Err(invalid("LIVE replay history version or final bound differs"));
			}
		}
		let (lost, metadata_bytes, metadata_work) =
			coverage(history.map(Arc::as_ref), cursor.versionstamp().get(), cutoff, options)?;
		let has_work = !lost.is_empty()
			|| history.is_some_and(|history| history.last_stamp > cursor.versionstamp().get());
		let segments = root.live_segments().get(&definition.id).map(Arc::clone);
		if segments.as_ref().is_some_and(|segments| !segments.is_empty()) && physical.is_none() {
			return Err(invalid("LIVE replay physical bindings were not pinned"));
		}
		let mut walk = Walk {
			segment: 0,
			page: 1,
			groups: 0,
			last: 0,
			time: None,
		};
		let floor = history.map_or(0, |history| history.purged_through);
		if physical.is_none() {
			walk.last = cursor.versionstamp().get().max(floor);
		} else if let Some(history) = history
			&& history.retained_page != 0
		{
			walk.page = history.retained_page;
			walk.groups = history.retained_groups;
			walk.last = floor;
			walk.time = Some(history.purged_time_ns);
		}
		Ok(Self {
			id: definition.id,
			version: definition.version,
			output: definition.query.output(),
			cutoff,
			has_work,
			expected_last: history.map_or(0, |history| history.last_stamp),
			position: cursor.versionstamp().get(),
			floor,
			memory: Arc::clone(root.live_history()),
			segments,
			physical: physical.map(|pin| pin.observed_reader()),
			lost,
			lost_at: 0,
			pending: None,
			walk,
			scratch: options.scratch_bytes() - metadata_bytes,
			work: options.work() - metadata_work,
			initial_work: options.work(),
			reported_work: 0,
		})
	}

	/// Returns the fixed tail handshake boundary, without advancing ACK.
	pub(in crate::datastore) const fn cutoff(&self) -> u64 {
		self.cutoff
	}

	/// Empty proven history requires neither pins nor a historical head
	/// holdback.
	pub(in crate::datastore) const fn has_work(&self) -> bool {
		self.has_work
	}

	/// Checks aggregate work before metadata operations or physical page reads.
	fn admit(&mut self, units: u64) -> FerrosGrynnResult<()> {
		self.work = self.work.checked_sub(units).ok_or_else(|| {
			FerrosGrynnError::storage_decode_limit_exceeded("LIVE replay work", units, self.work)
		})?;
		Ok(())
	}

	/// Creates a whole-commit unavailable delivery from proven bounds.
	fn gap(
		&mut self,
		cursor: &LiveCursor,
		from: u64,
		through: u64,
		reason: LiveGapReason,
	) -> FerrosGrynnResult<LiveDelivery> {
		if from > through || through > self.cutoff || through <= self.position {
			return Err(invalid("LIVE replay GAP has invalid authoritative bounds"));
		}
		self.position = through;
		Ok(LiveDelivery {
			token: LiveCursor::new(cursor.database_id(), self.id, self.version, through.into())?,
			payload: Arc::new(LivePayload::Gap(LiveGap {
				id: self.id,
				reason,
				from: Some(from.into()),
				through: Some(through.into()),
			})),
		})
	}

	/// Pulls one saved payload outside commit/consumer locks. Coverage is
	/// emitted before any affected saved rows; no current FETCH is performed.
	pub(super) fn next(
		&mut self,
		store: &StoreInner,
		cursor: &LiveCursor,
	) -> FerrosGrynnResult<Option<LiveDelivery>> {
		let result = self.pull(store, cursor);
		let used = self.initial_work - self.work;
		store.live_observation.replay_work(used - self.reported_work);
		self.reported_work = used;
		result
	}

	/// Pulls saved whole-commit images with the unchanged bounded replay state
	/// machine.
	fn pull(
		&mut self,
		store: &StoreInner,
		cursor: &LiveCursor,
	) -> FerrosGrynnResult<Option<LiveDelivery>> {
		loop {
			if self.position >= self.cutoff {
				return Ok(None);
			}
			if self.pending.is_none() {
				self.pending = self.inspect_next(store)?;
			}
			let next_stamp = match &self.pending {
				Some(Item::Memory(_, info) | Item::Local(_, _, _, info)) => info.stamp,
				Some(Item::Lost(segment)) => {
					segment.bounds().0.max(self.position.saturating_add(1))
				}
				None => self.cutoff.saturating_add(1),
			};
			while let Some(lost) = self.lost.get(self.lost_at).copied() {
				if lost.through <= self.position {
					self.lost_at += 1;
					continue;
				}
				if lost.from > next_stamp {
					break;
				}
				self.lost_at += 1;
				let from = lost.from.max(self.position.saturating_add(1));
				return self.gap(cursor, from, lost.through, lost.reason).map(Some);
			}
			let Some(item) = self.pending.take() else {
				return Ok(None);
			};
			let (mut payload, info) = match item {
				Item::Memory(bytes, info) => {
					if info.stamp <= self.position || info.stamp <= self.floor {
						continue;
					}
					self.admit(1 + u64::from(info.events))?;
					(live_history::decode_delivery(&bytes, self.scratch)?, info)
				}
				Item::Local(segment, first, expected_next, info) => {
					if info.stamp <= self.position || info.stamp <= self.floor {
						continue;
					}
					let units = expected_next
						.checked_sub(first)
						.and_then(|pages| pages.checked_add(1 + u64::from(info.events)))
						.ok_or_else(|| invalid("LIVE replay work overflows"))?;
					self.admit(units)?;
					let pin = self
						.physical
						.as_ref()
						.ok_or_else(|| invalid("LIVE replay pin disappeared"))?;
					let Some((payload, actual, next)) =
						store.read_live_payload(pin, segment, first, self.scratch)?
					else {
						if self
							.segments
							.as_ref()
							.and_then(|segments| segments.get(self.walk.segment))
							== Some(&segment)
						{
							self.advance_segment(segment);
						}
						return self.segment_gap(cursor, segment).map(Some);
					};
					if actual != info || next != expected_next {
						return Err(invalid("LIVE replay locator changed in a pinned snapshot"));
					}
					(payload, info)
				}
				Item::Lost(segment) => {
					if segment.bounds().1 <= self.position || segment.bounds().1 <= self.floor {
						continue;
					}
					return self.segment_gap(cursor, segment).map(Some);
				}
			};
			if let LivePayload::Batch(batch) = &mut payload
				&& self.output != crate::live::LiveOutputMode::Full
			{
				let limits = crate::live::LiveBindingOptions::default()
					.with_budgets(self.scratch, self.work)?;
				let (_bytes, work) =
					crate::live::prepare_batch_presentation(batch, self.output, &limits)?;
				self.admit(work)?;
			}
			self.position = info.stamp;
			return Ok(Some(LiveDelivery {
				token: LiveCursor::new(
					cursor.database_id(),
					self.id,
					self.version,
					info.stamp.into(),
				)?,
				payload: Arc::new(payload),
			}));
		}
	}

	/// Optional bytes cannot erase their catalog's independently trusted
	/// bounds.
	fn segment_gap(
		&mut self,
		cursor: &LiveCursor,
		segment: HistorySegment,
	) -> FerrosGrynnResult<LiveDelivery> {
		let (first, last, _, _) = segment.bounds();
		self.gap(
			cursor,
			first.max(self.position.saturating_add(1)),
			last.min(self.cutoff),
			LiveGapReason::SegmentCorrupt,
		)
	}

	/// Inspects one group at a time, checking framing, ordering and counts.
	fn inspect_next(&mut self, store: &StoreInner) -> FerrosGrynnResult<Option<Item>> {
		if self.physical.is_none() {
			if self.walk.last >= self.cutoff {
				return self.checked_eof();
			}
			self.admit(1 + u64::from(usize::BITS - self.memory.len().leading_zeros()))?;
			let Some(((_, stamp), bytes)) = self
				.memory
				.range((
					Bound::Excluded((self.id, self.walk.last)),
					Bound::Included((self.id, self.cutoff)),
				))
				.next()
			else {
				return self.checked_eof();
			};
			let info = live_history::inspect(bytes, bytes.len() as u64)?;
			if info.id != self.id
				|| info.version != self.version
				|| info.stamp != *stamp
				|| info.stamp > self.expected_last
				|| self.walk.time.is_some_and(|time| info.time < time)
			{
				return Err(invalid("LIVE RAM history disagrees with its catalog key/order"));
			}
			self.walk.last = *stamp;
			self.walk.time = Some(info.time);
			return Ok(Some(Item::Memory(Arc::clone(bytes), info)));
		}
		loop {
			self.admit(1)?;
			let Some(segment) = self
				.segments
				.as_ref()
				.and_then(|segments| segments.get(self.walk.segment))
				.copied()
			else {
				return self.checked_eof();
			};
			if segment.owner() != HistoryOwner::Live(self.id) {
				return Err(invalid("LIVE segment has another authoritative owner"));
			}
			let (first_stamp, last_stamp, first_time, last_time) = segment.bounds();
			if last_stamp <= self.position || last_stamp <= self.floor {
				self.advance_segment(segment);
				continue;
			}
			if first_stamp > self.cutoff {
				return self.checked_eof();
			}
			if self.walk.page == 0 || self.walk.page >= segment.page_ceiling() {
				return Err(invalid("LIVE catalog replay page exceeds its segment"));
			}
			let pin = self
				.physical
				.as_ref()
				.ok_or_else(|| invalid("LIVE replay physical pin is missing"))?;
			let Some((info, next)) =
				store.inspect_live_group(pin, segment, self.walk.page, self.scratch)?
			else {
				self.advance_segment(segment);
				return Ok(Some(Item::Lost(segment)));
			};
			let valid = info.id == self.id
				&& info.version == self.version
				&& info.stamp > self.walk.last
				&& info.stamp <= self.cutoff
				&& info.stamp <= self.expected_last
				&& self.walk.time.is_none_or(|time| info.time >= time)
				&& (self.walk.groups != 0 || info.stamp == first_stamp && info.time == first_time)
				&& self.walk.groups < segment.commit_count()
				&& next > self.walk.page
				&& next <= segment.page_ceiling()
				&& (next != segment.page_ceiling()
					|| self.walk.groups + 1 == segment.commit_count()
						&& info.stamp == last_stamp
						&& info.time == last_time);
			if !valid {
				self.advance_segment(segment);
				return Ok(Some(Item::Lost(segment)));
			}
			let first = self.walk.page;
			self.walk.page = next;
			self.walk.groups += 1;
			self.walk.last = info.stamp;
			self.walk.time = Some(info.time);
			if next == segment.page_ceiling() {
				self.advance_segment(segment);
			}
			if info.stamp <= self.position || info.stamp <= self.floor {
				continue;
			}
			return Ok(Some(Item::Local(segment, first, next, info)));
		}
	}

	/// Exhausted optional bytes cannot erase an authoritative retained tail.
	fn checked_eof(&self) -> FerrosGrynnResult<Option<Item>> {
		if self.walk.last < self.expected_last {
			return Err(invalid("LIVE replay ended before its authoritative final stamp"));
		}
		Ok(None)
	}

	/// Moves to the next segment using only authoritative final bounds.
	fn advance_segment(&mut self, segment: HistorySegment) {
		self.walk.segment += 1;
		self.walk.page = 1;
		self.walk.groups = 0;
		self.walk.last = segment.bounds().1;
		self.walk.time = Some(segment.bounds().3);
	}
}

/// Computes bounded unavailable intervals without reading primary records.
fn coverage(
	history: Option<&LiveHistoryMetadata>,
	since: u64,
	cutoff: u64,
	options: &LiveOptions,
) -> FerrosGrynnResult<(Vec<LostRange>, usize, u64)> {
	let count = history
		.map_or(Some(1), |history| history.epochs.len().checked_add(2))
		.ok_or_else(|| invalid("LIVE coverage count overflows"))?;
	let bytes = count
		.checked_mul(std::mem::size_of::<LostRange>())
		.and_then(|bytes| bytes.checked_add(std::mem::size_of::<Replay>()))
		.ok_or_else(|| invalid("LIVE coverage metadata overflows"))?;
	if bytes > options.scratch_bytes() || count as u64 > options.work() {
		return Err(FerrosGrynnError::storage_decode_limit_exceeded(
			"LIVE replay coverage",
			bytes as u64,
			options.scratch_bytes() as u64,
		));
	}
	let mut result = Vec::new();
	result
		.try_reserve_exact(count)
		.map_err(|error| invalid("LIVE coverage allocation failed").with_technical_source(error))?;
	let Some(mut from) = since.checked_add(1).filter(|from| *from <= cutoff) else {
		return Ok((result, bytes, count as u64));
	};
	let Some(history) = history else {
		result.push(LostRange {
			from,
			through: cutoff,
			reason: missing_reason(options),
		});
		return Ok((result, bytes, count as u64));
	};
	if from <= history.purged_through {
		result.push(LostRange {
			from,
			through: history.purged_through.min(cutoff),
			reason: LiveGapReason::Retention,
		});
		from = history.purged_through.saturating_add(1);
	}
	let first = history
		.epochs
		.first()
		.ok_or_else(|| invalid("LIVE coverage epochs are absent"))?
		.started_at;
	if from < first && from <= cutoff {
		result.push(LostRange {
			from,
			through: cutoff.min(first - 1),
			reason: missing_reason(options),
		});
	}
	for (ordinal, epoch) in history.epochs.iter().enumerate() {
		if epoch.policy.is_some() {
			continue;
		}
		let start = from.max(epoch.started_at);
		let end =
			history.epochs.get(ordinal + 1).map_or(cutoff, |next| cutoff.min(next.started_at - 1));
		if start <= end {
			result.push(LostRange {
				from: start,
				through: end,
				reason: LiveGapReason::VolatileHistoryLost,
			});
		}
	}
	Ok((result, bytes, count as u64))
}

/// A new historical recipe lacks input coverage; an ordinary saved volatile
/// incarnation has lost its transient delivery history.
fn missing_reason(options: &LiveOptions) -> LiveGapReason {
	if matches!(options.since(), crate::live::LiveSince::Now) {
		LiveGapReason::VolatileHistoryLost
	} else {
		LiveGapReason::InsufficientCoverage
	}
}

/// Catalog inconsistency is distinct from optional historical file corruption.
fn invalid(reason: &str) -> FerrosGrynnError {
	FerrosGrynnError::invalid_storage_file(reason)
}

#[cfg(test)]
/// Regression tests for replay behavior.
mod tests {
	use super::*;
	use crate::{
		changefeed::{ChangefeedConfig, ChangefeedSource},
		datastore::{ClassSchema, Datastore},
		gql_status::formatting::DiagnosticValue,
		graph::AtomKind,
		live::{LiveNext, LiveQuery, LiveSource},
		values::PropertyMap,
	};

	/// An absent final RAM entry differs from optional corrupt segment bytes.
	#[test]
	fn live_replay_missing_ram_tail_is_corruption() {
		use crate::test_assertions::AssertResult as _;

		let store = Datastore::in_memory();
		let mut setup = store.begin();
		let class = setup
			.define_class(ClassSchema::new("item", AtomKind::Node, []).assert_ok())
			.assert_ok();
		setup
			.configure_changefeed(
				ChangefeedSource::Class(class),
				ChangefeedConfig::from_nanoseconds(86_400_000_000_000).assert_ok(),
			)
			.assert_ok();
		setup.commit().assert_ok();
		let initial = store
			.subscribe_live(LiveQuery::new(LiveSource::Class(class)), LiveOptions::default())
			.assert_ok();
		let saved = initial.cursor().clone();
		drop(initial);
		for record in ["item:a", "item:b"] {
			let mut update = store.begin();
			update.create_atom(record.parse().assert_ok(), PropertyMap::new()).assert_ok();
			update.commit().assert_ok();
		}
		let mut root = (*store.inner.current_root()).clone();
		let final_stamp = root.live_catalog().histories[&saved.live_id()].last_stamp;
		Arc::make_mut(root.live_history_mut()).remove(&(saved.live_id(), final_stamp));
		let mut replay = Replay::new(&root, &saved, &LiveOptions::default(), None).assert_ok();
		assert!(replay.next(&store.inner, &saved).assert_ok().is_some());
		assert!(replay.next(&store.inner, &saved).is_err());
	}

	/// A valid oversized historical control cannot advance ACK or retain
	/// permits.
	#[test]
	fn live_replay_resource_error_fuses_attachment_and_keeps_ack() {
		use crate::test_assertions::AssertResult as _;

		let store = Datastore::in_memory();
		let mut setup = store.begin();
		let class = setup
			.define_class(ClassSchema::new("item", AtomKind::Node, []).assert_ok())
			.assert_ok();
		setup
			.configure_changefeed(
				ChangefeedSource::Class(class),
				ChangefeedConfig::from_nanoseconds(86_400_000_000_000).assert_ok(),
			)
			.assert_ok();
		setup.commit().assert_ok();
		let initial = store
			.subscribe_live(LiveQuery::new(LiveSource::Class(class)), LiveOptions::default())
			.assert_ok();
		let saved = initial.cursor().clone();
		drop(initial);
		let mut update = store.begin();
		update.create_atom("item:a".parse().assert_ok(), PropertyMap::new()).assert_ok();
		let stamp = update.commit().assert_ok().versionstamp().get();
		let mut root = (*store.inner.current_root()).clone();
		let payload = LivePayload::Fault {
			diagnostic: FerrosGrynnError::live_evaluation_fault(
				saved.live_id().to_string(),
				"FG-G1F09",
				"fixture",
			)
			.with_extension("large", DiagnosticValue::String("x".repeat(48 * 1024))),
			gap: LiveGap {
				id: saved.live_id(),
				reason: LiveGapReason::EvaluationFault,
				from: Some(stamp.into()),
				through: Some(stamp.into()),
			},
		};
		let bytes = live_history::encode(
			&payload,
			saved.live_id(),
			saved.version(),
			stamp,
			root.committed_at_ns,
			8 * 1024 * 1024,
		)
		.assert_ok();
		assert!(live_history::decode(&bytes, 8 * 1024 * 1024).is_ok());
		Arc::make_mut(root.live_history_mut()).insert((saved.live_id(), stamp), bytes.into());
		*store.inner.current.write() = Arc::new(root);
		let mut resumed = store.resume_live(&saved).assert_ok();
		assert!(resumed.next().is_err());
		assert!(matches!(resumed.next().assert_ok(), LiveNext::Closed));
		assert_eq!(resumed.cursor(), &saved);
		assert_eq!(store.begin().live_info(saved.live_id()).assert_ok().cursor(), &saved);
		let replacement = store.resume_live(&saved).assert_ok();
		assert_eq!(
			replacement.cursor(),
			&saved,
			"failed replay released its consumer and head admission"
		);
	}
}
