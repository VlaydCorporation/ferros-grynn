//! Deterministic TTL, pinned history and ordinary public-reader regressions.

use std::{
	num::NonZeroUsize,
	sync::{Arc, atomic::AtomicBool},
	time::{Duration, Instant},
};

use super::TEST_TIME;
use crate::{
	changefeed::{
		ChangefeedConfig,
		ChangefeedCursor,
		ChangefeedGapReason,
		ChangefeedId,
		ChangefeedReadItem,
		ChangefeedReadMode,
		ChangefeedReadOptions,
		ChangefeedSince,
		ChangefeedSource,
	},
	datastore::{
		CheckpointMode,
		ClassSchema,
		Datastore,
		LocalOptions,
		MaintenanceJobKind,
		MaintenanceOptions,
		MaintenanceTarget,
	},
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	graph::AtomKind,
	values::{PropertyKeyId, PropertyMap, RecordId, Uuid},
};

/// One fixed day, shared with the test source policy.
const TTL: i128 = 86_400_000_000_000;

/// Restores the calling thread's clock even when a regression fails.
struct Clock(
	/// Previous injected timestamp restored when the test guard is dropped.
	Option<i128>,
);
impl Clock {
	/// Uses deterministic expiration without changing the worker thread clock.
	fn new(time: i128) -> Self {
		Self(TEST_TIME.replace(Some(time)))
	}
}
impl Drop for Clock {
	/// Runs cleanup for this `Clock` before its fields are released.
	fn drop(&mut self) {
		TEST_TIME.set(self.0);
	}
}

/// Releases files only after their datastore handles and transactions.
struct Directory(
	/// Releases files only after their datastore handles and transactions.
	std::path::PathBuf,
);
impl Directory {
	/// Creates a canonical unique name within the existing database-name cap.
	fn new() -> Self {
		Self(
			std::env::temp_dir()
				.join(format!("fgcf-p-{:016x}", Uuid::now_v7().as_inner().as_u128() as u64)),
		)
	}
}
impl Drop for Directory {
	/// Runs cleanup for this `Directory` before its fields are released.
	fn drop(&mut self) {
		drop(std::fs::remove_dir_all(&self.0));
	}
}

/// Publishes a record under a late-enabled final FULL policy.
fn fixture(store: &Datastore) -> FerrosGrynnResult<(ChangefeedId, RecordId, PropertyKeyId)> {
	let mut tx = store.begin();
	let class = tx.define_class(ClassSchema::new("item", AtomKind::Node, [])?)?;
	let key = tx.intern_property("value")?;
	let record: RecordId = "item:a".parse()?;
	let mut properties = PropertyMap::new();
	properties.insert(key, 1);
	tx.create_atom(record.clone(), properties)?;
	let feed = tx.configure_changefeed(
		ChangefeedSource::Class(class),
		ChangefeedConfig::from_nanoseconds(TTL)?.with_full(true),
	)?;
	tx.commit()?;
	Ok((feed, record, key))
}

/// Executes the same catalog-only publisher with a deterministic deadline.
fn purge(store: &Datastore, time: i128) -> FerrosGrynnResult<()> {
	let _clock = Clock::new(time);
	store.inner.run_foreground_maintenance(
		MaintenanceJobKind::PurgeChangefeed,
		MaintenanceTarget::Database,
		MaintenanceOptions::default(),
		None,
	)
}

/// The same TTL publisher retires computed groups while old replay pins stay
/// valid.
fn live_prefix_purge(store: &Datastore) -> FerrosGrynnResult<()> {
	use crate::{
		live::{
			LiveExpr,
			LiveGapReason,
			LiveNext,
			LiveOptions,
			LivePayload,
			LiveProjection,
			LiveQuery,
			LiveSource,
		},
		test_assertions::{AssertComparison as _, AssertCondition as _},
	};
	let (feed, record, key) = fixture(store)?;
	let class = store.begin().changefeed_info(feed)?.source();
	let ChangefeedSource::Class(class) = class else {
		return Err(FerrosGrynnError::invalid_argument("expected class source"));
	};
	let initial = store.subscribe_live(
		LiveQuery::new(LiveSource::Class(class)).with_projection(LiveProjection::Value(
			LiveExpr::Field(crate::live::LivePath::new(vec!["properties".into(), "value".into()])?),
		)),
		LiveOptions::default().with_queue(10, 1 << 20)?,
	)?;
	let saved = initial.cursor().clone();
	drop(initial);
	let mut update = store.begin();
	update.set_base_property(&record, key, 2.into())?;
	let first = update.commit()?.versionstamp();
	let first_time = store.inner.current_root().committed_at_ns;
	let mut update = store.begin();
	update.set_base_property(&record, key, 3.into())?;
	let second = update.commit()?.versionstamp();
	let second_time = store.inner.current_root().committed_at_ns;
	(second_time > first_time)
		.assert_true_with(|| "separate commits provide a partial TTL boundary".to_string());
	let mut pinned = store.resume_live(&saved)?;
	purge(store, first_time + TTL)?;
	let current = store.inner.current_root();
	(&(current.live_catalog().histories[&saved.live_id()].purged_through), &(first.get()))
		.assert_equal();
	let info = store.begin().live_info(saved.live_id())?;
	let history = info.history().ok_or_else(|| {
		FerrosGrynnError::invalid_argument("public partial purge metadata missing")
	})?;
	(&(history.purged_through()), &(first)).assert_equal();
	(&(history.last_stamp()), &(second)).assert_equal();
	(
		&(history.purged_at().map(|time| time.as_inner().timestamp().as_nanosecond())),
		&(Some(first_time)),
	)
		.assert_equal();
	let LiveNext::Delivery(old) = pinned.next()? else {
		return Err(FerrosGrynnError::invalid_argument("pinned replay disappeared"));
	};
	(&(old.token().versionstamp()), &(first)).assert_equal();
	(matches!(old.payload(), LivePayload::Batch(_))).assert_true();
	drop(pinned);
	let mut fresh = store.resume_live(&saved)?;
	let LiveNext::Delivery(gap) = fresh.next()? else {
		return Err(FerrosGrynnError::invalid_argument("missing retention GAP"));
	};
	(matches!(gap.payload(), LivePayload::Gap(gap) if gap.reason() == LiveGapReason::Retention && gap.through() == Some(first))).assert_true();
	fresh.ack(gap.token())?;
	let LiveNext::Delivery(tail) = fresh.next()? else {
		return Err(FerrosGrynnError::invalid_argument("missing retained tail"));
	};
	(&(tail.token().versionstamp()), &(second)).assert_equal();
	(matches!(tail.payload(), LivePayload::Batch(_))).assert_true();
	drop(fresh);
	purge(store, second_time + TTL)?;
	let current = store.inner.current_root();
	(&(current.live_catalog().histories[&saved.live_id()].purged_through), &(second.get()))
		.assert_equal();
	(!current.live_segments().contains_key(&saved.live_id())).assert_true();
	(!current.live_history().keys().any(|(id, _)| *id == saved.live_id())).assert_true();
	let info = store.begin().live_info(saved.live_id())?;
	let history = info.history().ok_or_else(|| {
		FerrosGrynnError::invalid_argument("public complete purge metadata missing")
	})?;
	(&(history.purged_through()), &(second)).assert_equal();
	(&(history.last_stamp()), &(second))
		.assert_equal_with(|| "purge does not lower the recorded watermark".to_string());
	(
		&(history.purged_at().map(|time| time.as_inner().timestamp().as_nanosecond())),
		&(Some(second_time)),
	)
		.assert_equal();
	let mut empty = store.resume_live(&saved)?;
	let LiveNext::Delivery(gap) = empty.next()? else {
		return Err(FerrosGrynnError::invalid_argument("empty catalog lost its floor"));
	};
	(matches!(gap.payload(), LivePayload::Gap(gap) if gap.reason() == LiveGapReason::Retention && gap.through() == Some(second))).assert_true();
	Ok(())
}

/// Verifies live retention ram prefix and pins.
#[test]
fn live_retention_ram_prefix_and_pins() -> FerrosGrynnResult<()> {
	live_prefix_purge(&Datastore::in_memory())
}

/// Verifies live retention local sync prefix and pins.
#[cfg(windows)]
#[test]
fn live_retention_local_sync_prefix_and_pins() -> FerrosGrynnResult<()> {
	let directory = Directory::new();
	live_prefix_purge(&Datastore::create_local(&directory.0, LocalOptions::default())?)
}

/// Shortened capture TTL can drain the final page through the partial path.
#[cfg(windows)]
#[test]
fn live_retention_local_ttl_change_drains_partial_segment_and_reopens() {
	use crate::{
		live::{LiveNext, LiveOptions, LivePayload, LiveQuery, LiveSource},
		test_assertions::AssertResult as _,
	};
	let directory = Directory::new();
	let store = Datastore::create_local(&directory.0, LocalOptions::default()).assert_ok();
	let (feed, record, key) = fixture(&store).assert_ok();
	let ChangefeedSource::Class(class) = store.begin().changefeed_info(feed).assert_ok().source()
	else {
		return {
			let test_result: FerrosGrynnResult<()> =
				Err(FerrosGrynnError::invalid_argument("expected class source"));
			test_result.assert_ok();
		};
	};
	let initial = store
		.subscribe_live(LiveQuery::new(LiveSource::Class(class)), LiveOptions::default())
		.assert_ok();
	let saved = initial.cursor().clone();
	drop(initial);
	let short = 60_000_000_000;
	let mut update = store.begin();
	update
		.configure_changefeed(
			ChangefeedSource::Class(class),
			ChangefeedConfig::from_nanoseconds(short).assert_ok(),
		)
		.assert_ok();
	update.set_base_property(&record, key, 2.into()).assert_ok();
	let stamp = update.commit().assert_ok().versionstamp();
	let time = store.inner.current_root().committed_at_ns;
	assert!(store.inner.current_root().live_segments().contains_key(&saved.live_id()));
	purge(&store, time + short).assert_ok();
	assert!(!store.inner.current_root().live_segments().contains_key(&saved.live_id()));
	assert_eq!(
		store.inner.current_root().live_catalog().histories[&saved.live_id()].retained_page,
		0
	);
	store.checkpoint(CheckpointMode::Restart).assert_ok();
	drop(store);
	let store = Datastore::open_local(&directory.0, LocalOptions::default()).assert_ok();
	let mut resumed = store.resume_live(&saved).assert_ok();
	let LiveNext::Delivery(gap) = resumed.next().assert_ok() else {
		return {
			let test_result: FerrosGrynnResult<()> =
				Err(FerrosGrynnError::invalid_argument("missing reopened purge floor"));
			test_result.assert_ok();
		};
	};
	assert!(matches!(gap.payload(), LivePayload::Gap(gap) if gap.through() == Some(stamp)));
}

/// Verifies live retention local native async prefix and pins.
#[cfg(all(windows, feature = "native-async-io"))]
#[test]
fn live_retention_local_native_async_prefix_and_pins() -> FerrosGrynnResult<()> {
	let directory = Directory::new();
	let defaults = LocalOptions::default();
	let options = LocalOptions {
		io_mode: crate::datastore::IoMode::NativeAsync,
		..defaults
	};
	live_prefix_purge(&Datastore::create_local(&directory.0, options)?)
}

/// Obtains one complete public batch rather than peeking into stored DTOs.
fn batch(
	tx: &mut crate::datastore::Transaction,
	feed: ChangefeedId,
) -> FerrosGrynnResult<crate::changefeed::ChangeBatch> {
	let mut reader =
		tx.changes(feed, ChangefeedReadOptions::default().with_mode(ChangefeedReadMode::Full))?;
	let Some(ChangefeedReadItem::Commit(batch)) = reader.next_batch()? else {
		return Err(FerrosGrynnError::invalid_argument("test history batch is absent"));
	};
	Ok(batch)
}

/// Verifies changefeed ram retention keeps floor then retires source with
/// terminal gap.
#[test]
fn changefeed_ram_retention_keeps_floor_then_retires_source_with_terminal_gap() {
	use crate::test_assertions::AssertResult as _;

	let store = Datastore::in_memory();
	let (feed, record, key) = fixture(&store).assert_ok();
	let original = batch(&mut store.begin(), feed).assert_ok();
	let mut old = store.begin();
	let info = old.changefeed_info(feed).assert_ok();
	let cursor = ChangefeedCursor::new(
		info.database_id(),
		feed,
		ChangefeedReadMode::Full,
		original.versionstamp(),
	)
	.assert_ok();
	let mut tx = store.begin();
	tx.set_base_property(&record, key, 2.into()).assert_ok();
	let changed = tx.commit().assert_ok().versionstamp();
	let time = original.committed_at().as_inner().timestamp().as_nanosecond();
	purge(&store, time + TTL).assert_ok();
	assert_eq!(
		store.begin().changefeed_info(feed).assert_ok().purged_through(),
		original.versionstamp()
	);
	let mut current = store.begin();
	let mut reader = current
		.changes(feed, ChangefeedReadOptions::default().with_mode(ChangefeedReadMode::Full))
		.assert_ok();
	assert!(
		matches!(reader.next_batch().assert_ok(), Some(ChangefeedReadItem::Gap(gap)) if gap.reason() == ChangefeedGapReason::Retention)
	);
	assert!(
		matches!(reader.next_batch().assert_ok(), Some(ChangefeedReadItem::Commit(batch)) if batch.versionstamp() == changed)
	);
	assert!(reader.next_batch().assert_ok().is_none());
	drop(reader);
	assert_eq!(batch(&mut old, feed).assert_ok(), original);
	let mut dropper = store.begin();
	dropper.drop_changefeed(info.source()).assert_ok();
	dropper.commit().assert_ok();
	let close_time = store.inner.current_root().changefeeds().definitions[&feed]
		.closed_time_ns
		.ok_or_else(|| FerrosGrynnError::invalid_argument("close time missing"))
		.assert_ok();
	purge(&store, close_time + TTL).assert_ok();
	assert!(store.begin().changefeed_info(feed).is_err());
	let mut reader = current
		.changes(
			feed,
			ChangefeedReadOptions::new(ChangefeedSince::Cursor(cursor))
				.with_mode(ChangefeedReadMode::Full),
		)
		.assert_ok();
	// The earlier transaction retains its original history despite retirement.
	assert!(reader.next_batch().assert_ok().is_some());
	drop(reader);
	let mut tx = store.begin();
	let expired = ChangefeedCursor::new(
		info.database_id(),
		feed,
		ChangefeedReadMode::Full,
		original.versionstamp(),
	)
	.assert_ok();
	let mut reader = tx
		.changes(
			feed,
			ChangefeedReadOptions::new(ChangefeedSince::Cursor(expired))
				.with_mode(ChangefeedReadMode::Full),
		)
		.assert_ok();
	assert!(
		matches!(reader.next_batch().assert_ok(), Some(ChangefeedReadItem::Gap(gap)) if gap.reason() == ChangefeedGapReason::FeedExpiredOrRetired && gap.from().is_none() && gap.through().is_none())
	);
	assert!(reader.next_batch().assert_ok().is_none());
	drop(reader);
	assert_eq!(
		tx.atom(&record).assert_ok().and_then(|atom| atom.properties().get(key).cloned()),
		Some(2.into())
	);
	assert_ne!(tx.configure_changefeed(info.source(), info.configuration()).assert_ok(), feed);
}

/// Verifies changefeed retention has bounded passes and no partial resource
/// publication.
#[test]
fn changefeed_retention_has_bounded_passes_and_no_partial_resource_publication() {
	use crate::test_assertions::AssertResult as _;

	let store = Datastore::in_memory();
	let (feed, record, key) = fixture(&store).assert_ok();
	for _ in 0..1028 {
		let mut tx = store.begin();
		tx.set_base_property(&record, key, 1.into()).assert_ok();
		tx.commit().assert_ok();
	}
	let snapshot = store.inner.current_root();
	let time = snapshot.committed_at_ns + TTL;
	let _clock = Clock::new(time);
	let small_defaults = MaintenanceOptions::default();
	let small = MaintenanceOptions {
		max_memory_bytes: NonZeroUsize::new(4096)
			.ok_or_else(|| FerrosGrynnError::invalid_argument("test budget is zero"))
			.assert_ok(),
		..small_defaults
	};
	assert!(
		store
			.inner
			.run_foreground_maintenance(
				MaintenanceJobKind::PurgeChangefeed,
				MaintenanceTarget::Database,
				small,
				None
			)
			.is_err()
	);
	assert_eq!(store.inner.current_root().commit_ts, snapshot.commit_ts);
	assert!(
		crate::storage::with_external_sort_control(
			MaintenanceOptions::default().max_memory_bytes,
			NonZeroUsize::MIN,
			Arc::new(AtomicBool::new(true)),
			|| store.inner.purge_changefeed_candidate(&snapshot, snapshot.commit_ts + 1)
		)
		.is_err()
	);
	assert_eq!(store.inner.current_root().change_history().len(), 1029);
	purge(&store, time).assert_ok();
	assert_eq!(store.inner.current_root().change_history().len(), 5);
	purge(&store, time).assert_ok();
	assert!(store.inner.current_root().change_history().is_empty());
	assert_eq!(
		store.begin().changefeed_info(feed).assert_ok().purged_through().get(),
		snapshot.commit_ts
	);
}

/// Verifies changefeed local partial purge preserves multi generation pins and
/// reopens floor.
#[test]
fn changefeed_local_partial_purge_preserves_multi_generation_pins_and_reopens_floor() {
	use crate::test_assertions::AssertResult as _;

	let directory = Directory::new();
	let store = Datastore::create_local(&directory.0, LocalOptions::default()).assert_ok();
	let (feed, record, key) = fixture(&store).assert_ok();
	let mut oldest = store.begin();
	let original = batch(&mut oldest, feed).assert_ok();
	let mut update = store.begin();
	update.set_base_property(&record, key, 2.into()).assert_ok();
	let second_stamp = update.commit().assert_ok().versionstamp();
	let mut ddl = store.begin();
	ddl.define_class(ClassSchema::new("other", AtomKind::Node, []).assert_ok()).assert_ok();
	ddl.commit().assert_ok();
	let root = store.inner.current_root();
	let second_time = root.changefeed_segments()[&feed][0].bounds().3;
	let first_time = original.committed_at().as_inner().timestamp().as_nanosecond();
	assert!(second_time > first_time);
	let feed_directory = directory.0.join("changefeed").join(feed.as_uuid().to_string());
	let path = std::fs::read_dir(&feed_directory)
		.map_err(|error| {
			FerrosGrynnError::storage_io_failure("test feed directory").with_technical_source(error)
		})
		.assert_ok()
		.next()
		.transpose()
		.map_err(|error| {
			FerrosGrynnError::storage_io_failure("test feed file").with_technical_source(error)
		})
		.assert_ok()
		.ok_or_else(|| FerrosGrynnError::invalid_argument("feed file missing"))
		.assert_ok()
		.path();
	purge(&store, first_time + TTL).assert_ok();
	let retained = store.inner.current_root();
	assert_eq!(
		retained.changefeeds().definitions[&feed].purged_through,
		original.versionstamp().get()
	);
	assert!(retained.changefeeds().definitions[&feed].retained_page > 1);
	let mut tx = store.begin();
	let mut reader = tx
		.changes(feed, ChangefeedReadOptions::default().with_mode(ChangefeedReadMode::Full))
		.assert_ok();
	assert!(matches!(reader.next_batch().assert_ok(), Some(ChangefeedReadItem::Gap(_))));
	assert!(
		matches!(reader.next_batch().assert_ok(), Some(ChangefeedReadItem::Commit(batch)) if batch.versionstamp() == second_stamp)
	);
	assert!(reader.next_batch().assert_ok().is_none());
	drop(reader);
	drop(tx);
	purge(&store, second_time + TTL).assert_ok();
	assert!(path.exists());
	assert_eq!(batch(&mut oldest, feed).assert_ok(), original);
	drop(oldest);
	store.inner.sweep_retired_changefeeds();
	assert!(!path.exists());
	let mut update = store.begin();
	update.set_base_property(&record, key, 3.into()).assert_ok();
	let tail = update.commit().assert_ok().versionstamp();
	assert!(store.checkpoint(CheckpointMode::Restart).assert_ok());
	drop(store);
	let reopened = Datastore::open_local(&directory.0, LocalOptions::default()).assert_ok();
	assert_eq!(reopened.begin().changefeed_info(feed).assert_ok().purged_through(), second_stamp);
	let mut tx = reopened.begin();
	let mut reader = tx
		.changes(feed, ChangefeedReadOptions::default().with_mode(ChangefeedReadMode::Full))
		.assert_ok();
	assert!(matches!(reader.next_batch().assert_ok(), Some(ChangefeedReadItem::Gap(_))));
	assert!(
		matches!(reader.next_batch().assert_ok(), Some(ChangefeedReadItem::Commit(batch)) if batch.versionstamp() == tail)
	);
	assert!(reader.next_batch().assert_ok().is_none());
	drop(reader);
	drop(tx);
	let info = reopened.begin().changefeed_info(feed).assert_ok();
	let old_cursor =
		ChangefeedCursor::new(info.database_id(), feed, ChangefeedReadMode::Full, tail).assert_ok();
	let mut oldest = reopened.begin();
	let mut dropper = reopened.begin();
	dropper.drop_changefeed(info.source()).assert_ok();
	dropper.commit().assert_ok();
	let close_time = reopened.inner.current_root().changefeeds().definitions[&feed]
		.closed_time_ns
		.ok_or_else(|| FerrosGrynnError::invalid_argument("local close time missing"))
		.assert_ok();
	purge(&reopened, close_time + TTL).assert_ok();
	assert!(reopened.begin().changefeed_info(feed).is_err());
	assert!(
		oldest
			.changes(feed, ChangefeedReadOptions::default())
			.assert_ok()
			.next_batch()
			.assert_ok()
			.is_some()
	);
	drop(oldest);
	reopened.inner.sweep_retired_changefeeds();
	assert!(reopened.checkpoint(CheckpointMode::Restart).assert_ok());
	drop(reopened);
	let expired = Datastore::open_local(&directory.0, LocalOptions::default()).assert_ok();
	let mut tx = expired.begin();
	let mut reader = tx
		.changes(
			feed,
			ChangefeedReadOptions::new(ChangefeedSince::Cursor(old_cursor))
				.with_mode(ChangefeedReadMode::Full),
		)
		.assert_ok();
	assert!(
		matches!(reader.next_batch().assert_ok(), Some(ChangefeedReadItem::Gap(gap)) if gap.reason() == ChangefeedGapReason::FeedExpiredOrRetired)
	);
	assert!(reader.next_batch().assert_ok().is_none());
}

/// Verifies changefeed idle worker expires history without new commits.
#[test]
fn changefeed_idle_worker_expires_history_without_new_commits() {
	use crate::test_assertions::AssertResult as _;

	let store = Datastore::in_memory();
	let (feed, _, _) = fixture(&store).assert_ok();
	let info = store.begin().changefeed_info(feed).assert_ok();
	let mut tx = store.begin();
	tx.configure_changefeed(
		info.source(),
		ChangefeedConfig::from_nanoseconds(20_000_000).assert_ok().with_full(true),
	)
	.assert_ok();
	tx.commit().assert_ok();
	let deadline = Instant::now() + Duration::from_secs(5);
	loop {
		if store.begin().changefeed_info(feed).assert_ok().purged_through().get() != 0 {
			break;
		}
		if Instant::now() >= deadline {
			return {
				let test_result: FerrosGrynnResult<()> =
					Err(FerrosGrynnError::invalid_argument("idle purge did not run"));
				test_result.assert_ok();
			};
		}
		std::thread::sleep(Duration::from_millis(10));
	}
	let job = store.purge_changefeeds(None).assert_ok();
	assert!(store.job(job).assert_ok().is_some());
}

/// A failed WAL barrier preserves the old floor and files until recovery proves
/// whether the complete catalog publication reached the durable log.
#[test]
fn changefeed_purge_wal_boundaries_recover_partial_and_whole_segment_floors() {
	use crate::{
		storage::{CommitFailureGuard, CommitFailurePoint},
		test_assertions::AssertResult as _,
	};
	for whole in [false, true] {
		for point in [
			CommitFailurePoint::BeforeAppend,
			CommitFailurePoint::AfterAppend,
			CommitFailurePoint::AfterFlush,
			CommitFailurePoint::AfterSync,
		] {
			let directory = Directory::new();
			let store = Datastore::create_local(&directory.0, LocalOptions::default()).assert_ok();
			let (feed, record, key) = fixture(&store).assert_ok();
			let original = batch(&mut store.begin(), feed).assert_ok();
			let first_time = original.committed_at().as_inner().timestamp().as_nanosecond();
			let mut tx = store.begin();
			tx.set_base_property(&record, key, 2.into()).assert_ok();
			let second = tx.commit().assert_ok().versionstamp();
			let root = store.inner.current_root();
			assert!(root.committed_at_ns > first_time);
			let deadline = if whole {
				root.committed_at_ns
			} else {
				first_time
			} + TTL;
			let failure = CommitFailureGuard::set(point);
			assert!(purge(&store, deadline).is_err());
			drop(failure);
			assert_eq!(store.inner.current_root().commit_ts, root.commit_ts);
			assert_eq!(store.begin().changefeed_info(feed).assert_ok().purged_through().get(), 0);
			let files = directory.0.join("changefeed").join(feed.as_uuid().to_string());
			assert_eq!(
				std::fs::read_dir(&files)
					.map_err(|error| {
						FerrosGrynnError::storage_io_failure("purge failure files")
							.with_technical_source(error)
					})
					.assert_ok()
					.count(),
				1
			);
			drop(store);
			let reopened = Datastore::open_local(&directory.0, LocalOptions::default()).assert_ok();
			let committed = point != CommitFailurePoint::BeforeAppend;
			let expected = if !committed {
				0
			} else if whole {
				second.get()
			} else {
				original.versionstamp().get()
			};
			assert_eq!(
				reopened.begin().changefeed_info(feed).assert_ok().purged_through().get(),
				expected
			);
			let mut tx = reopened.begin();
			assert_eq!(
				tx.atom(&record).assert_ok().and_then(|atom| atom.properties().get(key).cloned()),
				Some(2.into())
			);
			let mut reader = tx
				.changes(feed, ChangefeedReadOptions::default().with_mode(ChangefeedReadMode::Full))
				.assert_ok();
			if committed {
				assert!(
					matches!(reader.next_batch().assert_ok(), Some(ChangefeedReadItem::Gap(gap)) if gap.reason() == ChangefeedGapReason::Retention)
				);
			}
			if !committed {
				assert!(
					matches!(reader.next_batch().assert_ok(), Some(ChangefeedReadItem::Commit(batch)) if batch.versionstamp() == original.versionstamp())
				);
			}
			if !committed || !whole {
				assert!(
					matches!(reader.next_batch().assert_ok(), Some(ChangefeedReadItem::Commit(batch)) if batch.versionstamp() == second)
				);
			}
			assert!(reader.next_batch().assert_ok().is_none());
			if committed && whole {
				assert_eq!(
					std::fs::read_dir(&files)
						.map_err(|error| {
							FerrosGrynnError::storage_io_failure("recovered purge files")
								.with_technical_source(error)
						})
						.assert_ok()
						.count(),
					0
				);
			}
		}
	}
}
