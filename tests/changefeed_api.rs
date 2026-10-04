//! Public changefeed definitions, identities and cursor ownership on RAM.

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

#[test]
fn changefeed_configuration_and_cursor_need_no_gql() -> FerrosGrynnResult<()> {
	let store = Datastore::in_memory();
	let owned = common::configure(&store)?;
	drop(store);
	assert!(!owned.database_id().as_inner().is_nil());
	assert_eq!(owned.name().as_str(), "item");
	Ok(())
}

/// Whole-commit LIMIT, owned images and exact resume are available without GQL.
#[test]
fn changefeed_public_pull_reader_preserves_batches_and_owned_images() -> FerrosGrynnResult<()> {
	let store = Datastore::in_memory();
	let fixture = common::history_fixture(&store)?;
	let mut old = store.begin();
	let mut update = store.begin();
	update.set_base_property(&fixture.first, fixture.key, 2.into())?;
	update.set_base_property(&fixture.second, fixture.key, 3.into())?;
	update.remove_base_property(&fixture.first, PropertyKeyId::new(99))?;
	let updated = update.commit()?.versionstamp();
	let options =
		ChangefeedReadOptions::default().with_mode(ChangefeedReadMode::Full).with_limit(1);
	let mut tx = store.begin();
	let mut reader = tx.changes(fixture.feed, options)?;
	let Some(ChangefeedReadItem::Commit(first)) = reader.next_batch()? else {
		return Err(FerrosGrynnError::invalid_argument("initial batch missing"));
	};
	assert_eq!(first.entries().len(), 2);
	assert!(reader.next_batch()?.is_none());
	let cursor = reader
		.cursor()
		.ok_or_else(|| FerrosGrynnError::invalid_argument("batch cursor missing"))?;
	drop(reader);
	let encoded = cursor.encode();
	let options = ChangefeedReadOptions::new(ChangefeedSince::Cursor(ChangefeedCursor::decode(
		encoded.as_slice(),
	)?))
	.with_mode(ChangefeedReadMode::Full);
	let mut reader = tx.changes(fixture.feed, options)?;
	let Some(ChangefeedReadItem::Commit(second)) = reader.next_batch()? else {
		return Err(FerrosGrynnError::invalid_argument("update batch missing"));
	};
	assert_eq!(second.versionstamp(), updated);
	assert_eq!(second.entries().len(), 3);
	assert_eq!(second.entries()[2].before(), second.entries()[2].after());
	assert!(reader.next_batch()?.is_none());
	drop(reader);
	let mut reader = old.changes(fixture.feed, ChangefeedReadOptions::default())?;
	assert!(reader.next_batch()?.is_some());
	assert!(reader.next_batch()?.is_none());
	drop(reader);
	let mut timestamp = tx.changes(
		fixture.feed,
		ChangefeedReadOptions::new(ChangefeedSince::Timestamp(first.committed_at().clone())),
	)?;
	assert!(timestamp.cursor().is_none());
	assert_eq!(timestamp.next_batch()?, Some(ChangefeedReadItem::Commit(first.clone())));
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
	Ok(())
}

/// Historical FULL coverage and cursor bindings are checked at construction.
#[test]
fn changefeed_public_reader_rejects_wrong_cursor_and_missing_full_coverage() -> FerrosGrynnResult<()>
{
	let store = Datastore::in_memory();
	let fixture = common::history_fixture(&store)?;
	let info = store.begin().changefeed_info(fixture.feed)?;
	let cursor = ChangefeedCursor::new(
		info.database_id(),
		fixture.feed,
		ChangefeedReadMode::Full,
		0.into(),
	)?;
	let mut tx = store.begin();
	assert!(
		tx.changes(
			fixture.feed,
			ChangefeedReadOptions::new(ChangefeedSince::Cursor(cursor.clone()))
		)
		.is_err()
	);
	let foreign =
		ChangefeedCursor::new(Uuid::now_v7(), fixture.feed, ChangefeedReadMode::Full, 0.into())?;
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
			fixture.graph_feed,
			ChangefeedReadOptions::new(ChangefeedSince::Cursor(cursor))
				.with_mode(ChangefeedReadMode::Full)
		)
		.is_err()
	);
	let unknown = ChangefeedId::from_uuid(Uuid::now_v7())?;
	assert!(tx.changes(unknown, ChangefeedReadOptions::default()).is_err());
	let terminal =
		ChangefeedCursor::new(info.database_id(), unknown, ChangefeedReadMode::Net, 0.into())?;
	let mut expired =
		tx.changes(unknown, ChangefeedReadOptions::new(ChangefeedSince::Cursor(terminal)))?;
	assert!(
		matches!(expired.next_batch()?, Some(ChangefeedReadItem::Gap(gap)) if gap.reason() == ferros_grynn::changefeed::ChangefeedGapReason::FeedExpiredOrRetired && gap.from().is_none() && gap.through().is_none())
	);
	assert!(expired.next_batch()?.is_none());
	drop(expired);
	let mut configure = store.begin();
	configure.configure_changefeed(
		ChangefeedSource::Class(fixture.class),
		info.configuration().with_full(false),
	)?;
	configure.set_base_property(&fixture.first, fixture.key, 2.into())?;
	let net_stamp = configure.commit()?.versionstamp();
	let mut enable = store.begin();
	enable.configure_changefeed(
		ChangefeedSource::Class(fixture.class),
		info.configuration().with_full(true),
	)?;
	enable.set_base_property(&fixture.first, fixture.key, 3.into())?;
	enable.commit()?;
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
	Ok(())
}
