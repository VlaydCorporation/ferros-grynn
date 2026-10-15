#![cfg(test)]

//! Public changefeed definitions, identities and cursor ownership on RAM.

/// Assertions shared with the root unit-test harness.
#[path = "common/assertions.rs"]
pub mod test_assertions;

#[path = "common/changefeed.rs"]
mod common;

use ferros_grynn::{
	changefeed::{
		ChangefeedCursor,
		ChangefeedId,
		ChangefeedReadItem,
		ChangefeedReadMode,
		ChangefeedReadOptions,
		ChangefeedSince,
		ChangefeedSource,
	},
	datastore::Datastore,
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	values::{PropertyKeyId, Uuid, Value},
};

/// Verifies changefeed configuration and cursor need no gql.
#[test]
fn changefeed_configuration_and_cursor_need_no_gql() {
	use crate::test_assertions::AssertResult as _;

	let store = Datastore::in_memory();
	let owned = common::configure(&store).assert_ok();
	drop(store);
	assert!(!owned.database_id().as_inner().is_nil());
	assert_eq!(owned.name().as_str(), "item");
}

/// Whole-commit LIMIT, owned images and exact resume are available without GQL.
#[test]
fn changefeed_public_pull_reader_preserves_batches_and_owned_images() {
	use crate::test_assertions::AssertResult as _;

	let store = Datastore::in_memory();
	let (fixture, (_class, _graph_feed, second)) = common::history_fixture(&store).assert_ok();
	let mut old = store.begin();
	let mut update = store.begin();
	update.set_base_property(&fixture.first, fixture.key, 2.into()).assert_ok();
	update.set_base_property(&second, fixture.key, 3.into()).assert_ok();
	update.remove_base_property(&fixture.first, PropertyKeyId::new(99)).assert_ok();
	let updated = update.commit().assert_ok().versionstamp();
	let options =
		ChangefeedReadOptions::default().with_mode(ChangefeedReadMode::Full).with_limit(1);
	let mut tx = store.begin();
	let mut reader = tx.changes(fixture.feed, options).assert_ok();
	let Some(ChangefeedReadItem::Commit(first)) = reader.next_batch().assert_ok() else {
		return {
			let test_result: FerrosGrynnResult<()> =
				Err(FerrosGrynnError::invalid_argument("initial batch missing"));
			test_result.assert_ok();
		};
	};
	assert_eq!(first.entries().len(), 2);
	assert!(reader.next_batch().assert_ok().is_none());
	let cursor = reader
		.cursor()
		.ok_or_else(|| FerrosGrynnError::invalid_argument("batch cursor missing"))
		.assert_ok();
	drop(reader);
	let encoded = cursor.encode();
	let options = ChangefeedReadOptions::new(ChangefeedSince::Cursor(
		ChangefeedCursor::decode(encoded.as_slice()).assert_ok(),
	))
	.with_mode(ChangefeedReadMode::Full);
	let mut reader = tx.changes(fixture.feed, options).assert_ok();
	let Some(ChangefeedReadItem::Commit(second)) = reader.next_batch().assert_ok() else {
		return {
			let test_result: FerrosGrynnResult<()> =
				Err(FerrosGrynnError::invalid_argument("update batch missing"));
			test_result.assert_ok();
		};
	};
	assert_eq!(second.versionstamp(), updated);
	assert_eq!(second.entries().len(), 3);
	assert_eq!(second.entries()[2].before(), second.entries()[2].after());
	assert!(reader.next_batch().assert_ok().is_none());
	drop(reader);
	let mut reader = old.changes(fixture.feed, ChangefeedReadOptions::default()).assert_ok();
	assert!(reader.next_batch().assert_ok().is_some());
	assert!(reader.next_batch().assert_ok().is_none());
	drop(reader);
	let mut timestamp = tx
		.changes(
			fixture.feed,
			ChangefeedReadOptions::new(ChangefeedSince::Timestamp(first.committed_at().clone())),
		)
		.assert_ok();
	assert!(timestamp.cursor().is_none());
	assert_eq!(timestamp.next_batch().assert_ok(), Some(ChangefeedReadItem::Commit(first)));
	drop(timestamp);
	drop(tx);
	drop(old);
	drop(store);
	assert_eq!(
		second.entries()[0].before().and_then(|image| image.properties().get("value")),
		Some(&Value::from(1))
	);
	assert_eq!(
		second.entries()[0].after().and_then(|image| image.properties().get("value")),
		Some(&Value::from(2))
	);
}

/// Historical FULL coverage and cursor bindings are checked at construction.
#[test]
fn changefeed_public_reader_rejects_wrong_cursor_and_missing_full_coverage() {
	use crate::test_assertions::AssertResult as _;

	let store = Datastore::in_memory();
	let (fixture, (class, graph_feed, _second)) = common::history_fixture(&store).assert_ok();
	let info = store.begin().changefeed_info(fixture.feed).assert_ok();
	let cursor =
		ChangefeedCursor::new(info.database_id(), fixture.feed, ChangefeedReadMode::Full, 0.into())
			.assert_ok();
	let mut tx = store.begin();
	assert!(
		tx.changes(
			fixture.feed,
			ChangefeedReadOptions::new(ChangefeedSince::Cursor(cursor.clone()))
		)
		.is_err()
	);
	let foreign =
		ChangefeedCursor::new(Uuid::now_v7(), fixture.feed, ChangefeedReadMode::Full, 0.into())
			.assert_ok();
	assert!(
		tx.changes(
			fixture.feed,
			ChangefeedReadOptions::new(ChangefeedSince::Cursor(foreign))
				.with_mode(ChangefeedReadMode::Full)
		)
		.is_err()
	);
	assert!(
		tx.changes(
			graph_feed,
			ChangefeedReadOptions::new(ChangefeedSince::Cursor(cursor))
				.with_mode(ChangefeedReadMode::Full)
		)
		.is_err()
	);
	let unknown = ChangefeedId::from_uuid(Uuid::now_v7()).assert_ok();
	assert!(tx.changes(unknown, ChangefeedReadOptions::default()).is_err());
	let terminal =
		ChangefeedCursor::new(info.database_id(), unknown, ChangefeedReadMode::Net, 0.into())
			.assert_ok();
	let mut expired = tx
		.changes(unknown, ChangefeedReadOptions::new(ChangefeedSince::Cursor(terminal)))
		.assert_ok();
	assert!(
		matches!(expired.next_batch().assert_ok(), Some(ChangefeedReadItem::Gap(gap)) if gap.reason() == ferros_grynn::changefeed::ChangefeedGapReason::FeedExpiredOrRetired && gap.from().is_none() && gap.through().is_none())
	);
	assert!(expired.next_batch().assert_ok().is_none());
	drop(expired);
	let mut configure = store.begin();
	configure
		.configure_changefeed(ChangefeedSource::Class(class), info.configuration().with_full(false))
		.assert_ok();
	configure.set_base_property(&fixture.first, fixture.key, 2.into()).assert_ok();
	let net_stamp = configure.commit().assert_ok().versionstamp();
	let mut enable = store.begin();
	enable
		.configure_changefeed(ChangefeedSource::Class(class), info.configuration().with_full(true))
		.assert_ok();
	enable.set_base_property(&fixture.first, fixture.key, 3.into()).assert_ok();
	enable.commit().assert_ok();
	let mut fresh = store.begin();
	assert!(
		fresh
			.changes(
				fixture.feed,
				ChangefeedReadOptions::default().with_mode(ChangefeedReadMode::Full)
			)
			.is_err()
	);
	assert!(
		fresh
			.changes(
				fixture.feed,
				ChangefeedReadOptions::new(ChangefeedSince::Versionstamp(net_stamp))
					.with_mode(ChangefeedReadMode::Full)
			)
			.is_ok()
	);
}
