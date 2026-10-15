#![cfg(test)]
#![cfg(all(windows, feature = "native-async-io"))]
//! Changefeed metadata persistence on the native asynchronous local backend.

/// Assertions shared with the root unit-test harness.
#[path = "common/assertions.rs"]
pub mod test_assertions;

#[path = "common/changefeed.rs"]
mod common;

/// Qualified history setup used only by local backend targets.
#[path = "common/qualified_history.rs"]
mod qualified;

use std::path::PathBuf;

use ferros_grynn::{
	changefeed::{ChangefeedReadItem, ChangefeedReadMode, ChangefeedReadOptions},
	datastore::{CheckpointMode, Datastore, IoMode, LocalOptions},
	gql_status::FerrosGrynnResult,
	values::Uuid,
};

/// Releases the database directory after native handles.
struct Directory(
	/// Releases the database directory after native handles.
	PathBuf,
);
impl Drop for Directory {
	/// Runs cleanup for this `Directory` before its fields are released.
	fn drop(&mut self) {
		drop(std::fs::remove_dir_all(&self.0));
	}
}

/// NativeAsync reads the same whole groups through its own production target.
#[test]
fn changefeed_native_async_reader_reopens_exact_history_without_gql() {
	use crate::test_assertions::AssertResult as _;

	let directory = Directory(
		std::env::temp_dir()
			.join(format!("fgcf-n-{:016x}", Uuid::now_v7().as_inner().as_u128() as u64)),
	);
	let mut options = LocalOptions::default();
	options.io_mode = IoMode::NativeAsync;
	let store = Datastore::create_local(&directory.0, options).assert_ok();
	let (fixture, _source_metadata) = common::history_fixture(&store).assert_ok();
	let mut update = store.begin();
	update.set_base_property(&fixture.first, fixture.key, 2.into()).assert_ok();
	let changed = update.commit().assert_ok().versionstamp();
	assert!(store.checkpoint(CheckpointMode::Restart).assert_ok());
	drop(store);
	let store = Datastore::open_local(&directory.0, options).assert_ok();
	let mut tx = store.begin();
	let mut reader = tx
		.changes(fixture.feed, ChangefeedReadOptions::default().with_mode(ChangefeedReadMode::Full))
		.assert_ok();
	assert!(
		matches!(reader.next_batch().assert_ok(), Some(ChangefeedReadItem::Commit(batch)) if batch.entries().len() == 2)
	);
	let second = reader.next_batch().assert_ok();
	assert!(
		matches!(&second, Some(ChangefeedReadItem::Commit(batch)) if batch.versionstamp() == changed && batch.entries()[0].after().and_then(|image| image.properties().get("value")) == Some(&2.into()))
	);
	assert!(reader.next_batch().assert_ok().is_none());
	drop(reader);
	drop(tx);
	drop(store);
	assert!(
		matches!(second, Some(ChangefeedReadItem::Commit(batch)) if batch.entries()[0].before().and_then(|image| image.properties().get("value")) == Some(&1.into()))
	);
}

/// Real edge-owned cross-level images survive WAL checkpoint and reopen.
#[test]
fn changefeed_native_async_qualified_routes_reopen_exact_images() {
	use ferros_grynn::{
		changefeed::{ChangeGraphState, ChangefeedSince},
		gql_status::FerrosGrynnError,
		graph::AtomKind,
	};

	use crate::test_assertions::AssertResult as _;
	let directory = Directory(
		std::env::temp_dir()
			.join(format!("fgcf-q-n-{:016x}", Uuid::now_v7().as_inner().as_u128() as u64)),
	);
	let mut options = LocalOptions::default();
	options.io_mode = IoMode::NativeAsync;
	let store = Datastore::create_local(&directory.0, options).assert_ok();
	let fixture = qualified::qualified_history_fixture(&store).assert_ok();
	let read_options = ChangefeedReadOptions::new(ChangefeedSince::Versionstamp(fixture.since))
		.with_mode(ChangefeedReadMode::Full);
	let mut tx = store.begin();
	let mut reader = tx.changes(fixture.feed, read_options.clone()).assert_ok();
	let captured = reader
		.next_batch()
		.assert_ok()
		.ok_or_else(|| FerrosGrynnError::invalid_argument("qualified history absent"))
		.assert_ok();
	assert!(reader.next_batch().assert_ok().is_none());
	drop(reader);
	drop(tx);
	let ChangefeedReadItem::Commit(batch) = &captured else {
		return {
			let test_result: FerrosGrynnResult<()> =
				Err(FerrosGrynnError::invalid_argument("qualified GAP unexpected"));
			test_result.assert_ok();
		};
	};
	assert_eq!(batch.entries().len(), 1);
	let image = batch.entries()[0]
		.after()
		.ok_or_else(|| FerrosGrynnError::invalid_argument("qualified image absent"))
		.assert_ok();
	let Some(ChangeGraphState::Edge {
		source,
		weight,
		..
	}) = image.graph_state()
	else {
		return {
			let test_result: FerrosGrynnResult<()> =
				Err(FerrosGrynnError::invalid_argument("qualified topology absent"));
			test_result.assert_ok();
		};
	};
	assert_eq!(source[0].occurrence().atom().id(), &fixture.endpoint);
	assert_eq!(source[0].occurrence().graph().id(), fixture.child);
	assert_eq!(source[0].occurrence().atom().kind(), AtomKind::Edge);
	assert_eq!(source[0].owners()[0].graph().id(), fixture.parent);
	assert_eq!(source[0].owners()[0].atom().id(), &fixture.owner);
	assert_eq!(source[0].owners()[0].atom().kind(), AtomKind::Edge);
	assert_eq!(*weight, Some(0.into()));
	assert!(store.checkpoint(CheckpointMode::Restart).assert_ok());
	drop(store);
	let store = Datastore::open_local(&directory.0, options).assert_ok();
	let mut tx = store.begin();
	let mut reader = tx.changes(fixture.feed, read_options).assert_ok();
	assert_eq!(reader.next_batch().assert_ok(), Some(captured.clone()));
	assert!(reader.next_batch().assert_ok().is_none());
	drop(reader);
	drop(tx);
	drop(store);
	assert_eq!(source[0].owners()[0].atom().id(), &fixture.owner);
}

/// Verifies changefeed native async metadata survives checkpoint reopen.
#[test]
fn changefeed_native_async_metadata_survives_checkpoint_reopen() {
	use crate::test_assertions::AssertResult as _;

	let directory = Directory(
		std::env::temp_dir()
			.join(format!("fgcf-{:016x}", Uuid::now_v7().as_inner().as_u128() as u64)),
	);
	let mut options = LocalOptions::default();
	options.io_mode = IoMode::NativeAsync;
	options.buffer_pool_pages = 16;
	let store = Datastore::create_local(&directory.0, options).assert_ok();
	let info = common::configure(&store).assert_ok();
	assert!(store.checkpoint(CheckpointMode::Restart).assert_ok());
	drop(store);
	let store = Datastore::open_local(&directory.0, options).assert_ok();
	assert_eq!(store.begin().changefeed_info(info.id()).assert_ok(), info);
}
