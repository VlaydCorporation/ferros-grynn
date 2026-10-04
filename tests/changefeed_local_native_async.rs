#![cfg(all(windows, feature = "native-async-io"))]
//! Changefeed metadata persistence on the native asynchronous local backend.

#[path = "common/changefeed.rs"]
mod common;

use std::path::PathBuf;

use ferros_grynn::{
	changefeed::{ChangefeedReadItem, ChangefeedReadMode, ChangefeedReadOptions},
	datastore::{CheckpointMode, Datastore, IoMode, LocalOptions},
	gql_status::FerrosGrynnResult,
	values::Uuid,
};

/// Releases the database directory after native handles.
struct Directory(PathBuf);
impl Drop for Directory {
	fn drop(&mut self) {
		drop(std::fs::remove_dir_all(&self.0));
	}
}

/// NativeAsync reads the same whole groups through its own production target.
#[test]
fn changefeed_native_async_reader_reopens_exact_history_without_gql() -> FerrosGrynnResult<()> {
	let directory = Directory(
		std::env::temp_dir()
			.join(format!("fgcf-n-{:016x}", Uuid::now_v7().as_inner().as_u128() as u64)),
	);
	let mut options = LocalOptions::default();
	options.io_mode = IoMode::NativeAsync;
	let store = Datastore::create_local(&directory.0, options.clone())?;
	let fixture = common::history_fixture(&store)?;
	let mut update = store.begin();
	update.set_base_property(&fixture.first, fixture.key, 2.into())?;
	let changed = update.commit()?.versionstamp();
	assert!(store.checkpoint(CheckpointMode::Restart)?);
	drop(store);
	let store = Datastore::open_local(&directory.0, options)?;
	let mut tx = store.begin();
	let mut reader = tx.changes(
		fixture.feed,
		ChangefeedReadOptions::default().with_mode(ChangefeedReadMode::Full),
	)?;
	assert!(
		matches!(reader.next_batch()?, Some(ChangefeedReadItem::Commit(batch)) if batch.entries().len() == 2)
	);
	let second = reader.next_batch()?;
	assert!(
		matches!(&second, Some(ChangefeedReadItem::Commit(batch)) if batch.versionstamp() == changed && batch.entries()[0].after().and_then(|image| image.properties().get("value")) == Some(&2.into()))
	);
	assert!(reader.next_batch()?.is_none());
	drop(reader);
	drop(tx);
	drop(store);
	assert!(
		matches!(second, Some(ChangefeedReadItem::Commit(batch)) if batch.entries()[0].before().and_then(|image| image.properties().get("value")) == Some(&1.into()))
	);
	Ok(())
}

/// Real edge-owned cross-level images survive WAL checkpoint and reopen.
#[test]
fn changefeed_native_async_qualified_routes_reopen_exact_images() -> FerrosGrynnResult<()> {
	use ferros_grynn::{
		changefeed::{ChangeGraphState, ChangefeedSince},
		gql_status::FerrosGrynnError,
		graph::AtomKind,
	};
	let directory = Directory(
		std::env::temp_dir()
			.join(format!("fgcf-q-n-{:016x}", Uuid::now_v7().as_inner().as_u128() as u64)),
	);
	let mut options = LocalOptions::default();
	options.io_mode = IoMode::NativeAsync;
	let store = Datastore::create_local(&directory.0, options.clone())?;
	let fixture = common::qualified_history_fixture(&store)?;
	let read_options = ChangefeedReadOptions::new(ChangefeedSince::Versionstamp(fixture.since))
		.with_mode(ChangefeedReadMode::Full);
	let mut tx = store.begin();
	let mut reader = tx.changes(fixture.feed, read_options.clone())?;
	let captured = reader
		.next_batch()?
		.ok_or_else(|| FerrosGrynnError::invalid_argument("qualified history absent"))?;
	assert!(reader.next_batch()?.is_none());
	drop(reader);
	drop(tx);
	let ChangefeedReadItem::Commit(batch) = &captured else {
		return Err(FerrosGrynnError::invalid_argument("qualified GAP unexpected"));
	};
	assert_eq!(batch.entries().len(), 1);
	let image = batch.entries()[0]
		.after()
		.ok_or_else(|| FerrosGrynnError::invalid_argument("qualified image absent"))?;
	let Some(ChangeGraphState::Edge {
		source,
		weight,
		..
	}) = image.graph_state()
	else {
		return Err(FerrosGrynnError::invalid_argument("qualified topology absent"));
	};
	assert_eq!(source[0].occurrence().atom().id(), &fixture.endpoint);
	assert_eq!(source[0].occurrence().graph().id(), fixture.child);
	assert_eq!(source[0].occurrence().atom().kind(), AtomKind::Edge);
	assert_eq!(source[0].owners()[0].graph().id(), fixture.parent);
	assert_eq!(source[0].owners()[0].atom().id(), &fixture.owner);
	assert_eq!(source[0].owners()[0].atom().kind(), AtomKind::Edge);
	assert_eq!(*weight, Some(0.into()));
	assert!(store.checkpoint(CheckpointMode::Restart)?);
	drop(store);
	let store = Datastore::open_local(&directory.0, options)?;
	let mut tx = store.begin();
	let mut reader = tx.changes(fixture.feed, read_options)?;
	assert_eq!(reader.next_batch()?, Some(captured.clone()));
	assert!(reader.next_batch()?.is_none());
	drop(reader);
	drop(tx);
	drop(store);
	assert_eq!(source[0].owners()[0].atom().id(), &fixture.owner);
	Ok(())
}

#[test]
fn changefeed_native_async_metadata_survives_checkpoint_reopen() -> FerrosGrynnResult<()> {
	let directory = Directory(
		std::env::temp_dir()
			.join(format!("fgcf-{:016x}", Uuid::now_v7().as_inner().as_u128() as u64)),
	);
	let mut options = LocalOptions::default();
	options.io_mode = IoMode::NativeAsync;
	options.buffer_pool_pages = 16;
	let store = Datastore::create_local(&directory.0, options.clone())?;
	let info = common::configure(&store)?;
	assert!(store.checkpoint(CheckpointMode::Restart)?);
	drop(store);
	let store = Datastore::open_local(&directory.0, options)?;
	assert_eq!(store.begin().changefeed_info(info.id())?, info);
	Ok(())
}
