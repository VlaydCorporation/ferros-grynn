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
struct Clock(Option<i128>);
impl Clock {
	/// Uses deterministic expiration without changing the worker thread clock.
	fn new(time: i128) -> Self {
		Self(TEST_TIME.replace(Some(time)))
	}
}
impl Drop for Clock {
	fn drop(&mut self) {
		TEST_TIME.set(self.0);
	}
}

/// Releases files only after their datastore handles and transactions.
struct Directory(std::path::PathBuf);
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

#[test]
fn changefeed_ram_retention_keeps_floor_then_retires_source_with_terminal_gap()
-> FerrosGrynnResult<()> {
	let store = Datastore::in_memory();
	let (feed, record, key) = fixture(&store)?;
	let original = batch(&mut store.begin(), feed)?;
	let mut old = store.begin();
	let info = old.changefeed_info(feed)?;
	let cursor = ChangefeedCursor::new(
		info.database_id(),
		feed,
		ChangefeedReadMode::Full,
		original.versionstamp(),
	)?;
	let mut tx = store.begin();
	tx.set_base_property(&record, key, 2.into())?;
	let changed = tx.commit()?.versionstamp();
	let time = original.committed_at().as_inner().timestamp().as_nanosecond();
	purge(&store, time + TTL)?;
	assert_eq!(store.begin().changefeed_info(feed)?.purged_through(), original.versionstamp());
	let mut current = store.begin();
	let mut reader = current
		.changes(feed, ChangefeedReadOptions::default().with_mode(ChangefeedReadMode::Full))?;
	assert!(
		matches!(reader.next_batch()?, Some(ChangefeedReadItem::Gap(gap)) if gap.reason() == ChangefeedGapReason::Retention)
	);
	assert!(
		matches!(reader.next_batch()?, Some(ChangefeedReadItem::Commit(batch)) if batch.versionstamp() == changed)
	);
	assert!(reader.next_batch()?.is_none());
	drop(reader);
	assert_eq!(batch(&mut old, feed)?, original);
	let mut dropper = store.begin();
	dropper.drop_changefeed(info.source())?;
	dropper.commit()?;
	let close_time = store.inner.current_root().changefeeds().definitions[&feed]
		.closed_time_ns
		.ok_or_else(|| FerrosGrynnError::invalid_argument("close time missing"))?;
	purge(&store, close_time + TTL)?;
	assert!(store.begin().changefeed_info(feed).is_err());
	let mut reader = current.changes(
		feed,
		ChangefeedReadOptions::new(ChangefeedSince::Cursor(cursor))
			.with_mode(ChangefeedReadMode::Full),
	)?;
	// The earlier transaction retains its original history despite retirement.
	assert!(reader.next_batch()?.is_some());
	drop(reader);
	let mut tx = store.begin();
	let expired = ChangefeedCursor::new(
		info.database_id(),
		feed,
		ChangefeedReadMode::Full,
		original.versionstamp(),
	)?;
	let mut reader = tx.changes(
		feed,
		ChangefeedReadOptions::new(ChangefeedSince::Cursor(expired))
			.with_mode(ChangefeedReadMode::Full),
	)?;
	assert!(
		matches!(reader.next_batch()?, Some(ChangefeedReadItem::Gap(gap)) if gap.reason() == ChangefeedGapReason::FeedExpiredOrRetired && gap.from().is_none() && gap.through().is_none())
	);
	assert!(reader.next_batch()?.is_none());
	drop(reader);
	assert_eq!(
		tx.atom(&record)?.and_then(|atom| atom.properties().get(key).cloned()),
		Some(2.into())
	);
	assert_ne!(tx.configure_changefeed(info.source(), info.configuration())?, feed);
	Ok(())
}

#[test]
fn changefeed_retention_has_bounded_passes_and_no_partial_resource_publication()
-> FerrosGrynnResult<()> {
	let store = Datastore::in_memory();
	let (feed, record, key) = fixture(&store)?;
	for _ in 0..1028 {
		let mut tx = store.begin();
		tx.set_base_property(&record, key, 1.into())?;
		tx.commit()?;
	}
	let snapshot = store.inner.current_root();
	let time = snapshot.committed_at_ns + TTL;
	let _clock = Clock::new(time);
	let mut small = MaintenanceOptions::default();
	small.max_memory_bytes = NonZeroUsize::new(4096)
		.ok_or_else(|| FerrosGrynnError::invalid_argument("test budget is zero"))?;
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
	purge(&store, time)?;
	assert_eq!(store.inner.current_root().change_history().len(), 5);
	purge(&store, time)?;
	assert!(store.inner.current_root().change_history().is_empty());
	assert_eq!(store.begin().changefeed_info(feed)?.purged_through().get(), snapshot.commit_ts);
	Ok(())
}

#[test]
fn changefeed_local_partial_purge_preserves_multi_generation_pins_and_reopens_floor()
-> FerrosGrynnResult<()> {
	let directory = Directory::new();
	let store = Datastore::create_local(&directory.0, LocalOptions::default())?;
	let (feed, record, key) = fixture(&store)?;
	let mut oldest = store.begin();
	let original = batch(&mut oldest, feed)?;
	let mut update = store.begin();
	update.set_base_property(&record, key, 2.into())?;
	let second_stamp = update.commit()?.versionstamp();
	let mut ddl = store.begin();
	ddl.define_class(ClassSchema::new("other", AtomKind::Node, [])?)?;
	ddl.commit()?;
	let root = store.inner.current_root();
	let second_time = root.changefeed_segments()[&feed][0].bounds().3;
	let first_time = original.committed_at().as_inner().timestamp().as_nanosecond();
	assert!(second_time > first_time);
	let feed_directory = directory.0.join("changefeed").join(feed.as_uuid().to_string());
	let path = std::fs::read_dir(&feed_directory)
		.map_err(|error| {
			FerrosGrynnError::storage_io_failure("test feed directory").with_technical_source(error)
		})?
		.next()
		.transpose()
		.map_err(|error| {
			FerrosGrynnError::storage_io_failure("test feed file").with_technical_source(error)
		})?
		.ok_or_else(|| FerrosGrynnError::invalid_argument("feed file missing"))?
		.path();
	purge(&store, first_time + TTL)?;
	let retained = store.inner.current_root();
	assert_eq!(
		retained.changefeeds().definitions[&feed].purged_through,
		original.versionstamp().get()
	);
	assert!(retained.changefeeds().definitions[&feed].retained_page > 1);
	let mut tx = store.begin();
	let mut reader =
		tx.changes(feed, ChangefeedReadOptions::default().with_mode(ChangefeedReadMode::Full))?;
	assert!(matches!(reader.next_batch()?, Some(ChangefeedReadItem::Gap(_))));
	assert!(
		matches!(reader.next_batch()?, Some(ChangefeedReadItem::Commit(batch)) if batch.versionstamp() == second_stamp)
	);
	assert!(reader.next_batch()?.is_none());
	drop(reader);
	drop(tx);
	purge(&store, second_time + TTL)?;
	assert!(path.exists());
	assert_eq!(batch(&mut oldest, feed)?, original);
	drop(oldest);
	store.inner.sweep_retired_changefeeds();
	assert!(!path.exists());
	let mut update = store.begin();
	update.set_base_property(&record, key, 3.into())?;
	let tail = update.commit()?.versionstamp();
	assert!(store.checkpoint(CheckpointMode::Restart)?);
	drop(store);
	let reopened = Datastore::open_local(&directory.0, LocalOptions::default())?;
	assert_eq!(reopened.begin().changefeed_info(feed)?.purged_through(), second_stamp);
	let mut tx = reopened.begin();
	let mut reader =
		tx.changes(feed, ChangefeedReadOptions::default().with_mode(ChangefeedReadMode::Full))?;
	assert!(matches!(reader.next_batch()?, Some(ChangefeedReadItem::Gap(_))));
	assert!(
		matches!(reader.next_batch()?, Some(ChangefeedReadItem::Commit(batch)) if batch.versionstamp() == tail)
	);
	assert!(reader.next_batch()?.is_none());
	drop(reader);
	drop(tx);
	let info = reopened.begin().changefeed_info(feed)?;
	let old_cursor =
		ChangefeedCursor::new(info.database_id(), feed, ChangefeedReadMode::Full, tail)?;
	let mut oldest = reopened.begin();
	let mut dropper = reopened.begin();
	dropper.drop_changefeed(info.source())?;
	dropper.commit()?;
	let close_time = reopened.inner.current_root().changefeeds().definitions[&feed]
		.closed_time_ns
		.ok_or_else(|| FerrosGrynnError::invalid_argument("local close time missing"))?;
	purge(&reopened, close_time + TTL)?;
	assert!(reopened.begin().changefeed_info(feed).is_err());
	assert!(oldest.changes(feed, ChangefeedReadOptions::default())?.next_batch()?.is_some());
	drop(oldest);
	reopened.inner.sweep_retired_changefeeds();
	assert!(reopened.checkpoint(CheckpointMode::Restart)?);
	drop(reopened);
	let expired = Datastore::open_local(&directory.0, LocalOptions::default())?;
	let mut tx = expired.begin();
	let mut reader = tx.changes(
		feed,
		ChangefeedReadOptions::new(ChangefeedSince::Cursor(old_cursor))
			.with_mode(ChangefeedReadMode::Full),
	)?;
	assert!(
		matches!(reader.next_batch()?, Some(ChangefeedReadItem::Gap(gap)) if gap.reason() == ChangefeedGapReason::FeedExpiredOrRetired)
	);
	assert!(reader.next_batch()?.is_none());
	Ok(())
}

#[test]
fn changefeed_idle_worker_expires_history_without_new_commits() -> FerrosGrynnResult<()> {
	let store = Datastore::in_memory();
	let (feed, _, _) = fixture(&store)?;
	let info = store.begin().changefeed_info(feed)?;
	let mut tx = store.begin();
	tx.configure_changefeed(
		info.source(),
		ChangefeedConfig::from_nanoseconds(20_000_000)?.with_full(true),
	)?;
	tx.commit()?;
	let deadline = Instant::now() + Duration::from_secs(5);
	loop {
		if store.begin().changefeed_info(feed)?.purged_through().get() != 0 {
			break;
		}
		if Instant::now() >= deadline {
			return Err(FerrosGrynnError::invalid_argument("idle purge did not run"));
		}
		std::thread::sleep(Duration::from_millis(10));
	}
	let job = store.purge_changefeeds(None)?;
	assert!(store.job(job)?.is_some());
	Ok(())
}

/// A failed WAL barrier preserves the old floor and files until recovery proves
/// whether the complete catalog publication reached the durable log.
#[test]
fn changefeed_purge_wal_boundaries_recover_partial_and_whole_segment_floors()
-> FerrosGrynnResult<()> {
	use crate::storage::{CommitFailureGuard, CommitFailurePoint};
	for whole in [false, true] {
		for point in [
			CommitFailurePoint::BeforeAppend,
			CommitFailurePoint::AfterAppend,
			CommitFailurePoint::AfterFlush,
			CommitFailurePoint::AfterSync,
		] {
			let directory = Directory::new();
			let store = Datastore::create_local(&directory.0, LocalOptions::default())?;
			let (feed, record, key) = fixture(&store)?;
			let original = batch(&mut store.begin(), feed)?;
			let first_time = original.committed_at().as_inner().timestamp().as_nanosecond();
			let mut tx = store.begin();
			tx.set_base_property(&record, key, 2.into())?;
			let second = tx.commit()?.versionstamp();
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
			assert_eq!(store.begin().changefeed_info(feed)?.purged_through().get(), 0);
			let files = directory.0.join("changefeed").join(feed.as_uuid().to_string());
			assert_eq!(
				std::fs::read_dir(&files)
					.map_err(|error| {
						FerrosGrynnError::storage_io_failure("purge failure files")
							.with_technical_source(error)
					})?
					.count(),
				1
			);
			drop(store);
			let reopened = Datastore::open_local(&directory.0, LocalOptions::default())?;
			let committed = point != CommitFailurePoint::BeforeAppend;
			let expected = if !committed {
				0
			} else if whole {
				second.get()
			} else {
				original.versionstamp().get()
			};
			assert_eq!(reopened.begin().changefeed_info(feed)?.purged_through().get(), expected);
			let mut tx = reopened.begin();
			assert_eq!(
				tx.atom(&record)?.and_then(|atom| atom.properties().get(key).cloned()),
				Some(2.into())
			);
			let mut reader = tx.changes(
				feed,
				ChangefeedReadOptions::default().with_mode(ChangefeedReadMode::Full),
			)?;
			if committed {
				assert!(
					matches!(reader.next_batch()?, Some(ChangefeedReadItem::Gap(gap)) if gap.reason() == ChangefeedGapReason::Retention)
				);
			}
			if !committed {
				assert!(
					matches!(reader.next_batch()?, Some(ChangefeedReadItem::Commit(batch)) if batch.versionstamp() == original.versionstamp())
				);
			}
			if !committed || !whole {
				assert!(
					matches!(reader.next_batch()?, Some(ChangefeedReadItem::Commit(batch)) if batch.versionstamp() == second)
				);
			}
			assert!(reader.next_batch()?.is_none());
			if committed && whole {
				assert_eq!(
					std::fs::read_dir(&files)
						.map_err(|error| {
							FerrosGrynnError::storage_io_failure("recovered purge files")
								.with_technical_source(error)
						})?
						.count(),
					0
				);
			}
		}
	}
	Ok(())
}
