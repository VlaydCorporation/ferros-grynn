//! Independent snapshot/clock and bounded-reader contract checks.

use std::sync::Arc;

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
	datastore::{ClassSchema, Datastore},
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	graph::AtomKind,
	values::{DateTime, PropertyKeyId, PropertyMap, RecordId, Uuid},
};

/// Publishes an empty configured source for controlled subsequent operations.
fn fixture() -> FerrosGrynnResult<(Datastore, ChangefeedId, RecordId, PropertyKeyId)> {
	let store = Datastore::in_memory();
	let mut setup = store.begin();
	let class = setup.define_class(ClassSchema::new("item", AtomKind::Node, [])?)?;
	let key = setup.intern_property("value")?;
	let id = setup.configure_changefeed(
		ChangefeedSource::Class(class),
		ChangefeedConfig::from_nanoseconds(86_400_000_000_000)?.with_full(true),
	)?;
	setup.commit()?;
	Ok((store, id, "item:a".parse()?, key))
}

/// Observes committed policy and independent counters through the standalone
/// SDK.
#[test]
fn changefeed_reader_observations_are_owned_committed_and_independent() {
	use crate::test_assertions::AssertResult as _;

	let (store, id, record, key) = fixture().assert_ok();
	two_commits(&store, &record, key).assert_ok();
	let before = store.changefeed_statistics().assert_ok();
	assert_eq!(before.published_groups, 2);
	assert_eq!(before.net_entries, 2);
	assert_eq!(before.full_entries, 2);
	assert!(before.capture_operations >= 2);
	assert!(before.peak_capture_scratch_bytes > 0);
	let mut tx = store.begin();
	let source = tx.changefeed_info(id).assert_ok().source();
	tx.configure_changefeed(
		source,
		ChangefeedConfig::from_nanoseconds(86_400_000_000_000).assert_ok(),
	)
	.assert_ok();
	let options = ChangefeedReadOptions::default().with_mode(ChangefeedReadMode::Full);
	let mut first = tx.changes(id, options.clone()).assert_ok();
	let binding = first
		.binding()
		.ok_or_else(|| FerrosGrynnError::invalid_argument("read binding absent"))
		.assert_ok();
	assert!(binding.configuration.full(), "private policy cannot change committed binding");
	assert_eq!(first.statistics().groups_inspected, 2);
	while first.next_batch().assert_ok().is_some() {}
	let statistics = first.statistics();
	assert_eq!(statistics.batches, 2);
	assert_eq!(statistics.entries, 2);
	assert_eq!(statistics.groups_inspected, 4);
	assert_eq!(statistics.page_reads, 0);
	assert_eq!(statistics.bytes_read, 0);
	assert!(statistics.peak_scratch_bytes > 0);
	drop(first);
	let mut second = tx.changes(id, options).assert_ok();
	assert_eq!(second.statistics().batches, 0);
	while second.next_batch().assert_ok().is_some() {}
	assert_eq!(second.statistics(), statistics);
	drop(second);
	drop(tx);
	assert_eq!(store.changefeed_statistics().assert_ok(), before);
}

/// Creates two complete commits under an exact, nondecreasing simulated clock.
fn two_commits(
	store: &Datastore,
	record: &RecordId,
	key: PropertyKeyId,
) -> FerrosGrynnResult<(u64, u64)> {
	let mut first = store.begin();
	first.create_atom(record.clone(), PropertyMap::new())?;
	let first = first.commit()?.commit_ts;
	let mut second = store.begin();
	second.set_base_property(record, key, 2.into())?;
	let second = second.commit()?.commit_ts;
	Ok((first, second))
}

/// Verifies changefeed reader same timestamps large stamps and zero full net.
#[test]
fn changefeed_reader_same_timestamps_large_stamps_and_zero_full_net() {
	use crate::test_assertions::AssertResult as _;

	let (store, id, record, key) = fixture().assert_ok();
	let time = jiff::Timestamp::now().as_nanosecond() + 86_400_000_000_000;
	{
		let mut root = store.inner.current.write();
		let root = Arc::make_mut(&mut root);
		root.commit_ts = (1_u64 << 53) + 17;
		root.committed_at_ns = time;
	}
	let (first, second) = two_commits(&store, &record, key).assert_ok();
	assert!(first > 1_u64 << 53);
	let timestamp = DateTime::new(
		jiff::Timestamp::from_nanosecond(time)
			.map_err(|error| FerrosGrynnError::invalid_argument(error.to_string()))
			.assert_ok()
			.to_zoned(jiff::tz::TimeZone::UTC),
	);
	let mut tx = store.begin();
	let mut reader = tx
		.changes(id, ChangefeedReadOptions::new(ChangefeedSince::Timestamp(timestamp)))
		.assert_ok();
	for stamp in [first, second] {
		assert!(
			matches!(reader.next_batch().assert_ok(), Some(ChangefeedReadItem::Commit(batch)) if batch.versionstamp().get() == stamp && batch.committed_at().as_inner().timestamp().as_nanosecond() == time)
		);
	}
	assert!(reader.next_batch().assert_ok().is_none());
	drop(reader);
	let mut noop = store.begin();
	noop.set_base_property(&record, key, 2.into()).assert_ok();
	let noop = noop.commit().assert_ok().versionstamp();
	let mut fresh = store.begin();
	let since = ChangefeedSince::Versionstamp(second.into());
	let mut net = fresh.changes(id, ChangefeedReadOptions::new(since.clone())).assert_ok();
	assert!(net.next_batch().assert_ok().is_none());
	assert_eq!(net.cursor().map(|cursor| cursor.versionstamp()), Some(noop));
	drop(net);
	let mut full = fresh
		.changes(id, ChangefeedReadOptions::new(since).with_mode(ChangefeedReadMode::Full))
		.assert_ok();
	assert!(
		matches!(full.next_batch().assert_ok(), Some(ChangefeedReadItem::Commit(batch)) if batch.versionstamp() == noop && batch.entries().len() == 1 && batch.entries()[0].before() == batch.entries()[0].after())
	);
}

/// Verifies changefeed reader retention gap tail and timestamp floor are exact.
#[test]
fn changefeed_reader_retention_gap_tail_and_timestamp_floor_are_exact() {
	use crate::test_assertions::AssertResult as _;

	let (store, id, record, key) = fixture().assert_ok();
	let (first, second) = two_commits(&store, &record, key).assert_ok();
	let purged_time;
	{
		let mut root = store.inner.current.write();
		let root = Arc::make_mut(&mut root);
		purged_time = root
			.change_history()
			.get(&(id, first))
			.ok_or_else(|| FerrosGrynnError::invalid_argument("missing purge fixture"))
			.assert_ok()
			.net
			.committed_at()
			.as_inner()
			.timestamp()
			.as_nanosecond();
		Arc::make_mut(root.change_history_mut()).remove(&(id, first));
		let definition = Arc::make_mut(root.changefeeds_mut())
			.definitions
			.get_mut(&id)
			.ok_or_else(|| FerrosGrynnError::invalid_argument("missing feed fixture"))
			.assert_ok();
		let definition = Arc::make_mut(definition);
		definition.purged_through = first;
		definition.purged_time_ns = purged_time;
	}
	let mut tx = store.begin();
	let mut reader = tx.changes(id, ChangefeedReadOptions::default().with_limit(1)).assert_ok();
	assert!(
		matches!(reader.next_batch().assert_ok(), Some(ChangefeedReadItem::Gap(gap)) if gap.reason() == ChangefeedGapReason::Retention && gap.from().is_none() && gap.through().map(|stamp| stamp.get()) == Some(first))
	);
	let cursor = reader
		.cursor()
		.ok_or_else(|| FerrosGrynnError::invalid_argument("missing retention cursor"))
		.assert_ok();
	assert!(reader.next_batch().assert_ok().is_none());
	drop(reader);
	let mut resumed =
		tx.changes(id, ChangefeedReadOptions::new(ChangefeedSince::Cursor(cursor))).assert_ok();
	assert!(
		matches!(resumed.next_batch().assert_ok(), Some(ChangefeedReadItem::Commit(batch)) if batch.versionstamp().get() == second)
	);
	drop(resumed);
	let timestamp = DateTime::new(
		jiff::Timestamp::from_nanosecond(purged_time + 1)
			.map_err(|error| FerrosGrynnError::invalid_argument(error.to_string()))
			.assert_ok()
			.to_zoned(jiff::tz::TimeZone::UTC),
	);
	let mut reader = tx
		.changes(id, ChangefeedReadOptions::new(ChangefeedSince::Timestamp(timestamp)))
		.assert_ok();
	assert!(!matches!(reader.next_batch().assert_ok(), Some(ChangefeedReadItem::Gap(_))));
}

/// Verifies changefeed reader budgets and future cursors are errors not gaps.
#[test]
fn changefeed_reader_budgets_and_future_cursors_are_errors_not_gaps() {
	use crate::test_assertions::AssertResult as _;

	let (store, id, record, key) = fixture().assert_ok();
	let (_, second) = two_commits(&store, &record, key).assert_ok();
	let mut tx = store.begin();
	let info = tx.changefeed_info(id).assert_ok();
	let future =
		ChangefeedCursor::new(info.database_id(), id, ChangefeedReadMode::Net, (second + 1).into())
			.assert_ok();
	assert!(tx.changes(id, ChangefeedReadOptions::new(ChangefeedSince::Cursor(future))).is_err());
	let budget_error = tx
		.changes(id, ChangefeedReadOptions::default().with_budgets(8, 100).assert_ok())
		.err()
		.ok_or_else(|| FerrosGrynnError::invalid_argument("reader accepted undersized budget"))
		.assert_ok();
	assert!(
		matches!(budget_error.status().status(), crate::gql_status::Status::StorageDecodeLimitExceeded { actual, maximum: 8, .. } if *actual > 8)
	);
	assert!(
		tx.changes(
			id,
			ChangefeedReadOptions::default()
				.with_mode(ChangefeedReadMode::Full)
				.with_budgets(1_000_000, 1)
				.assert_ok()
		)
		.is_err()
	);
	let mut limited = tx
		.changes(id, ChangefeedReadOptions::default().with_budgets(1_000_000, 1).assert_ok())
		.assert_ok();
	assert!(limited.next_batch().assert_ok().is_some());
	let cursor = limited.cursor();
	assert!(limited.next_batch().is_err());
	assert_eq!(limited.cursor(), cursor);
	assert!(limited.next_batch().assert_ok().is_none());
	drop(limited);
	assert!(ChangefeedReadOptions::default().with_budgets(0, 1).is_err());
	let unknown = ChangefeedId::from_uuid(Uuid::now_v7()).assert_ok();
	assert!(tx.changes(unknown, ChangefeedReadOptions::default()).is_err());
}

/// A transaction lifetime failure is returned once without advancing position.
#[test]
fn changefeed_reader_transaction_failure_is_fused() {
	use crate::{changefeed::ReadBatches, test_assertions::AssertResult as _};
	let (store, id, record, key) = fixture().assert_ok();
	two_commits(&store, &record, key).assert_ok();
	let mut tx = store.begin();
	let history = crate::datastore::source_history::SourceReader::new(
		&store.inner,
		Arc::clone(&tx.snapshot),
		id,
		ChangefeedReadOptions::default(),
		None,
		false,
	)
	.assert_ok();
	tx.gql_aborted = true;
	let mut reader = super::SnapshotReader {
		transaction: &tx,
		history,
	};
	let cursor = reader.cursor();
	assert!(reader.next_batch(None).is_err());
	assert_eq!(reader.cursor(), cursor);
	assert!(reader.next_batch(None).assert_ok().is_none());
}

/// A stamp-only cursor cannot preserve a time boundary that has not been met.
#[test]
fn changefeed_reader_timestamp_seek_never_exports_an_earlier_stamp_cursor() {
	use crate::test_assertions::AssertResult as _;

	let (store, id, record, key) = fixture().assert_ok();
	two_commits(&store, &record, key).assert_ok();
	let future_time = store.inner.current_root().committed_at_ns + 86_400_000_000_000;
	let timestamp = DateTime::new(
		jiff::Timestamp::from_nanosecond(future_time)
			.map_err(|error| FerrosGrynnError::invalid_argument(error.to_string()))
			.assert_ok()
			.to_zoned(jiff::tz::TimeZone::UTC),
	);
	let since = ChangefeedSince::Timestamp(timestamp);
	let mut tx = store.begin();
	let mut future = tx.changes(id, ChangefeedReadOptions::new(since.clone())).assert_ok();
	assert!(future.next_batch().assert_ok().is_none());
	assert!(future.cursor().is_none());
	drop(future);
	let mut limited = tx
		.changes(
			id,
			ChangefeedReadOptions::new(since.clone()).with_budgets(1_000_000, 1).assert_ok(),
		)
		.assert_ok();
	assert!(limited.next_batch().is_err());
	assert!(limited.cursor().is_none());
	drop(limited);
	let mut update = store.begin();
	update.set_base_property(&record, key, 3.into()).assert_ok();
	update.commit().assert_ok();
	let mut tx = store.begin();
	let mut resumed = tx.changes(id, ChangefeedReadOptions::new(since)).assert_ok();
	assert!(resumed.next_batch().assert_ok().is_none());
	assert!(resumed.cursor().is_none());
}
