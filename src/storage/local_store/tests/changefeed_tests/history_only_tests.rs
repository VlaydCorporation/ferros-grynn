//! FULL no-op capture retains data files and the ordinary commit barrier.

use std::sync::Arc;

use super::*;
use crate::{
	datastore::IoMode,
	storage::{catalog::FileCatalogEntry, types::DataFileKind},
	test_assertions::{AssertOption as _, AssertResult as _},
};

/// History growth must not replace any unrelated physical identity.
fn data_files(store: &Datastore) -> Vec<FileCatalogEntry> {
	local(store)
		.assert_ok()
		.state
		.lock()
		.file_catalog
		.files
		.iter()
		.filter(|entry| entry.file_id.kind() != Some(DataFileKind::Changefeed))
		.cloned()
		.collect()
}

/// Class and graph FULL groups carry the unchanged target at the new stamp.
fn assert_noop_groups(store: &Datastore, feeds: [ChangefeedId; 2], first: u64, count: usize) {
	for feed in feeds {
		let captured = groups(store, feed).assert_ok();
		assert_eq!(captured.len(), count);
		for (offset, group) in captured.iter().enumerate() {
			let stamp = first + u64::try_from(offset).assert_ok();
			assert_eq!(group.net.versionstamp().get(), stamp);
			assert!(group.net.entries().is_empty());
			let full = group.full.as_ref().assert_some();
			assert_eq!(full.versionstamp().get(), stamp);
			assert_eq!(full.entries().len(), 1);
			let entry = &full.entries()[0];
			assert_eq!(entry.before(), entry.after());
			assert_eq!(
				entry.after().assert_some().properties().get("value"),
				Some(&Value::from(1))
			);
		}
	}
}

/// Successful absence intents retain the source generation through repeated
/// capture.
#[test]
fn full_history_only_sync_retains_files_and_reopens() {
	full_history_only(IoMode::Sync);
}

/// The actual selected native transport survives create and cold recovery.
#[cfg(feature = "native-async-io")]
#[test]
fn full_history_only_native_async_retains_files_and_reopens() {
	full_history_only(IoMode::NativeAsync);
}

/// Exercises physical retention independently of successful FULL capture.
fn full_history_only(io_mode: IoMode) {
	let directory = TestDirectory::new();
	let options = LocalOptions {
		io_mode,
		..LocalOptions::default()
	};
	let (store, record, key, class_feed, graph_feed) =
		fixture_with_options(&directory.0, options).assert_ok();
	assert_eq!(store.local_storage_status().assert_some().options().io_mode, io_mode);
	drop(store);
	let store = Datastore::open_local(&directory.0, options).assert_ok();
	assert_eq!(store.local_storage_status().assert_some().options().io_mode, io_mode);
	let source = store.inner.current_root();
	assert!(source.statistics.auxiliary_deferred());
	let graph = source.schema.named_graph("scope").assert_some().reference().id();
	let global = source.resolve_atom(&record).assert_some();
	let owner = source.membership(graph, global).assert_some();
	let local = local(&store).assert_ok();
	let pin = local.capture_query_snapshot(source.commit_ts).assert_ok().assert_some();
	let files = data_files(&store);
	let directories = super::super::generation_directory_paths(&directory.0);
	let boundary = local.wal.snapshot().visible_lsn();
	for count in 1..=2 {
		let mut noop = store.begin();
		noop.remove_base_property(&record, PropertyKeyId::new(999)).assert_ok();
		noop.set_occurrence_label(graph, &record, None).assert_ok();
		let stamp = noop.commit().assert_ok().commit_ts();
		assert_eq!(stamp, source.commit_ts + u64::try_from(count).assert_ok());
		assert_noop_groups(&store, [class_feed, graph_feed], source.commit_ts + 1, count);
		assert_eq!(data_files(&store), files, "FULL_HISTORY_ONLY_REBUILT_DATA_FILES");
		assert_eq!(super::super::generation_directory_paths(&directory.0), directories);
		assert!(store.inner.current_root().statistics.auxiliary_deferred());
	}
	assert!(local.wal.snapshot().visible_lsn() > boundary);
	store.checkpoint(CheckpointMode::Passive).assert_ok();
	// This is the first read through this retained pin, not a cache-cold claim.
	let context = crate::datastore::index_context(&source.schema, &source.dictionary).assert_ok();
	let physical = local
		.physical_traversal_fields(&pin, graph, owner, &context.decoder())
		.assert_ok()
		.assert_some();
	assert_eq!(physical.base_properties.get(key), Some(&Value::from(1)));
	assert!(physical.overlay.is_none());
	drop(pin);
	drop(source);
	let first_stamp = store.inner.current_root().commit_ts - 1;
	drop(store);
	let store = Datastore::open_local(&directory.0, options).assert_ok();
	assert_eq!(store.local_storage_status().assert_some().options().io_mode, io_mode);
	assert_noop_groups(&store, [class_feed, graph_feed], first_stamp, 2);
	assert_eq!(data_files(&store), files);
	let mut update = store.begin();
	update.set_base_property(&record, key, 2.into()).assert_ok();
	update.commit().assert_ok();
	assert_eq!(data_files(&store), files);
	assert_eq!(
		store.begin().atom(&record).assert_ok().assert_some().properties().get(key),
		Some(&Value::from(2))
	);
}

/// Known pre-publication failures retain both FULL sources and the source root.
#[test]
fn full_history_only_known_failures_publish_nothing() {
	for io_mode in [IoMode::Sync, IoMode::NativeAsync] {
		if io_mode == IoMode::NativeAsync && !cfg!(feature = "native-async-io") {
			continue;
		}
		for point in [
			GenerationFailurePoint::AfterBuild,
			GenerationFailurePoint::AfterFileSync,
			GenerationFailurePoint::AfterCatalogStage,
			GenerationFailurePoint::BeforeWalCommit,
		] {
			let directory = TestDirectory::new();
			let options = LocalOptions {
				io_mode,
				..LocalOptions::default()
			};
			let (store, record, _key, class_feed, graph_feed) =
				fixture_with_options(&directory.0, options).assert_ok();
			drop(store);
			let store = Datastore::open_local(&directory.0, options).assert_ok();
			let before = store.inner.current_root();
			let graph = before.schema.named_graph("scope").assert_some().reference().id();
			let files = local(&store).assert_ok().state.lock().file_catalog.files.clone();
			let boundary = local(&store).assert_ok().wal.snapshot().visible_lsn();
			let failure = GenerationFailureGuard::set(point);
			let mut noop = store.begin();
			noop.remove_base_property(&record, PropertyKeyId::new(999)).assert_ok();
			noop.set_occurrence_label(graph, &record, None).assert_ok();
			assert!(noop.commit().is_err());
			drop(failure);
			assert!(Arc::ptr_eq(&store.inner.current_root(), &before));
			assert_eq!(local(&store).assert_ok().state.lock().file_catalog.files, files);
			assert_eq!(local(&store).assert_ok().wal.snapshot().visible_lsn(), boundary);
			assert_noop_groups(&store, [class_feed, graph_feed], before.commit_ts + 1, 0);
		}
	}
}

/// Empty and NET-only absence commits do not reach any durable publisher.
#[test]
fn full_history_only_empty_and_net_noop_keep_wal_boundary() {
	for io_mode in [IoMode::Sync, IoMode::NativeAsync] {
		if io_mode == IoMode::NativeAsync && !cfg!(feature = "native-async-io") {
			continue;
		}
		let directory = TestDirectory::new();
		let options = LocalOptions {
			io_mode,
			..LocalOptions::default()
		};
		let (store, record, _key, class_feed, graph_feed) =
			fixture_with_options(&directory.0, options).assert_ok();
		let graph =
			store.inner.current_root().schema.named_graph("scope").assert_some().reference().id();
		let mut configure = store.begin();
		let policy = ChangefeedConfig::from_nanoseconds(86_400_000_000_000).assert_ok();
		configure
			.configure_changefeed(
				ChangefeedSource::Class(
					store
						.inner
						.current_root()
						.schema
						.class("item")
						.assert_some()
						.id()
						.assert_some(),
				),
				policy,
			)
			.assert_ok();
		configure.configure_changefeed(ChangefeedSource::Graph(graph), policy).assert_ok();
		configure.commit().assert_ok();
		let before = store.inner.current_root();
		let boundary = local(&store).assert_ok().wal.snapshot().visible_lsn();
		assert_eq!(store.begin().commit().assert_ok().commit_ts(), before.commit_ts);
		let mut noop = store.begin();
		noop.remove_base_property(&record, PropertyKeyId::new(999)).assert_ok();
		noop.set_occurrence_label(graph, &record, None).assert_ok();
		assert_eq!(noop.commit().assert_ok().commit_ts(), before.commit_ts);
		assert!(Arc::ptr_eq(&store.inner.current_root(), &before));
		assert_eq!(local(&store).assert_ok().wal.snapshot().visible_lsn(), boundary);
		assert_noop_groups(&store, [class_feed, graph_feed], before.commit_ts + 1, 0);
	}
}

/// A mutation-free FULL capture still resolves uncertain WAL outcomes at
/// reopen.
#[test]
fn full_history_only_uncertain_wal_recovers_one_complete_commit() {
	use crate::storage::wal::{CommitFailureGuard, CommitFailurePoint};
	for io_mode in [IoMode::Sync, IoMode::NativeAsync] {
		if io_mode == IoMode::NativeAsync && !cfg!(feature = "native-async-io") {
			continue;
		}
		for point in [
			CommitFailurePoint::BeforeAppend,
			CommitFailurePoint::AfterAppend,
			CommitFailurePoint::AfterFlush,
			CommitFailurePoint::AfterSync,
		] {
			let directory = TestDirectory::new();
			let options = LocalOptions {
				io_mode,
				..LocalOptions::default()
			};
			let (store, record, _key, class_feed, graph_feed) =
				fixture_with_options(&directory.0, options).assert_ok();
			drop(store);
			let store = Datastore::open_local(&directory.0, options).assert_ok();
			let before = store.inner.current_root();
			let stamp = before.commit_ts;
			let graph = before.schema.named_graph("scope").assert_some().reference().id();
			let files = data_files(&store);
			let failure = CommitFailureGuard::set(point);
			let mut noop = store.begin();
			noop.remove_base_property(&record, PropertyKeyId::new(999)).assert_ok();
			noop.set_occurrence_label(graph, &record, None).assert_ok();
			assert!(noop.commit().is_err());
			drop(failure);
			assert!(Arc::ptr_eq(&store.inner.current_root(), &before));
			let pending = segment_file_count(&directory.0).assert_ok();
			let mut retry = store.begin();
			retry.remove_base_property(&record, PropertyKeyId::new(999)).assert_ok();
			assert!(retry.commit().is_err());
			assert_eq!(segment_file_count(&directory.0).assert_ok(), pending);
			drop(before);
			drop(store);
			let store = Datastore::open_local(&directory.0, options).assert_ok();
			assert_eq!(store.local_storage_status().assert_some().options().io_mode, io_mode);
			let count = usize::from(point != CommitFailurePoint::BeforeAppend);
			assert_eq!(
				store.inner.current_root().commit_ts,
				stamp + u64::try_from(count).assert_ok()
			);
			assert_noop_groups(&store, [class_feed, graph_feed], stamp + 1, count);
			assert_eq!(data_files(&store), files);
			store.checkpoint(CheckpointMode::Restart).assert_ok();
			drop(store);
			let store = Datastore::open_local(&directory.0, options).assert_ok();
			assert_noop_groups(&store, [class_feed, graph_feed], stamp + 1, count);
			assert_eq!(data_files(&store), files);
		}
	}
}

/// An intent-only FULL writer remains blocked by the ordinary import lease.
#[test]
fn full_history_only_noop_cannot_bypass_import_lease() {
	for io_mode in [IoMode::Sync, IoMode::NativeAsync] {
		if io_mode == IoMode::NativeAsync && !cfg!(feature = "native-async-io") {
			continue;
		}
		let directory = TestDirectory::new();
		let options = LocalOptions {
			io_mode,
			..LocalOptions::default()
		};
		let (store, record, _key, class_feed, graph_feed) =
			fixture_with_options(&directory.0, options).assert_ok();
		let graph =
			store.inner.current_root().schema.named_graph("scope").assert_some().reference().id();
		let mut noop = store.begin();
		noop.remove_base_property(&record, PropertyKeyId::new(999)).assert_ok();
		noop.set_occurrence_label(graph, &record, None).assert_ok();
		store
			.import(
				ImportOptions {
					changefeed: ImportChangefeedMode::On,
					..ImportOptions::default()
				},
				|_session| {
					let before = store.inner.current_root();
					let boundary = local(&store).assert_ok().wal.snapshot().visible_lsn();
					assert!(noop.commit().is_err());
					assert!(Arc::ptr_eq(&store.inner.current_root(), &before));
					assert_eq!(local(&store).assert_ok().wal.snapshot().visible_lsn(), boundary);
					assert_noop_groups(&store, [class_feed, graph_feed], before.commit_ts + 1, 0);
					Ok(())
				},
			)
			.assert_ok();
	}
}
