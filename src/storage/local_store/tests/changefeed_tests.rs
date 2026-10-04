//! Durable feed publication through real local WAL, catalog and reopen paths.

use super::{GenerationFailureGuard, GenerationFailurePoint, TestDirectory};
use crate::{
	changefeed::{
		ChangefeedConfig,
		ChangefeedGapReason,
		ChangefeedId,
		ChangefeedReadItem,
		ChangefeedReadMode,
		ChangefeedReadOptions,
		ChangefeedSince,
		ChangefeedSource,
	},
	datastore::{
		CapturedCommit,
		CheckpointMode,
		ClassSchema,
		Datastore,
		ImportChangefeedMode,
		ImportOptions,
		ImportViolationPolicy,
		LocalOptions,
		StoreBackend,
	},
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	graph::{AtomKind, GraphKind},
	storage::{ChangefeedSegment, LocalChangefeedSnapshot, LocalStore},
	values::{PropertyKeyId, PropertyMap, RecordId, Value},
};

/// Seeds both global and graph-local FULL sources after ordinary initial DML.
fn fixture(
	path: &std::path::Path,
) -> FerrosGrynnResult<(Datastore, RecordId, PropertyKeyId, ChangefeedId, ChangefeedId)> {
	let store = Datastore::create_local(path, LocalOptions::default())?;
	let mut seed = store.begin();
	let class = seed.define_class(ClassSchema::new("item", AtomKind::Node, [])?)?;
	let graph = seed.define_graph("scope", GraphKind::Graph)?;
	let key = seed.intern_property("value")?;
	let record = "item:a".parse::<RecordId>()?;
	let mut properties = PropertyMap::new();
	properties.insert(key, 1);
	seed.create_atom(record.clone(), properties)?;
	seed.attach_node(graph.id(), &record, None, None)?;
	seed.commit()?;
	let mut configure = store.begin();
	let policy = ChangefeedConfig::from_nanoseconds(86_400_000_000_000)?.with_full(true);
	let class_feed = configure.configure_changefeed(ChangefeedSource::Class(class), policy)?;
	let graph_feed = configure.configure_changefeed(ChangefeedSource::Graph(graph.id()), policy)?;
	configure.commit()?;
	Ok((store, record, key, class_feed, graph_feed))
}

/// Counts actual metadata/payload reads while independently sharing snapshot
/// pins.
#[test]
fn changefeed_local_reader_observations_count_real_page_requests_and_reset() -> FerrosGrynnResult<()>
{
	let directory = TestDirectory::new();
	let (store, record, key, feed, _) = fixture(&directory.0)?;
	let mut write = store.begin();
	write.set_base_property(&record, key, 2.into())?;
	write.commit()?;
	let published = store.changefeed_statistics()?;
	assert_eq!(published.published_groups, 2);
	assert_eq!(published.net_entries, 2);
	assert_eq!(published.full_entries, 2);
	assert_eq!(published.retained_segments, 2);
	assert!(published.retained_feed_bytes >= 4 * crate::storage::PAGE_SIZE as u64);
	let mut tx = store.begin();
	let options = ChangefeedReadOptions::default().with_mode(ChangefeedReadMode::Full);
	let mut reader = tx.changes(feed, options.clone())?;
	assert_eq!(reader.statistics().page_reads, 2, "FULL header-only preflight");
	assert!(reader.next_batch()?.is_some());
	assert!(reader.next_batch()?.is_none());
	let statistics = reader.statistics();
	assert_eq!(statistics.batches, 1);
	assert_eq!(statistics.entries, 1);
	assert_eq!(statistics.groups_inspected, 2);
	assert_eq!(statistics.segment_visits, 2);
	assert_eq!(statistics.page_reads, 7);
	assert_eq!(statistics.bytes_read, 7 * crate::storage::PAGE_SIZE as u64);
	assert!(statistics.peak_scratch_bytes >= crate::storage::PAGE_SIZE as u64);
	drop(reader);
	let mut second = tx.changes(feed, options)?;
	assert_eq!(second.statistics().page_reads, 2, "old pulls must not enter the new reader");
	while second.next_batch()?.is_some() {}
	assert_eq!(second.statistics(), statistics);
	Ok(())
}

/// Resolves the actual local owner without a fallback to RAM history.
fn local(store: &Datastore) -> FerrosGrynnResult<&LocalStore> {
	match &store.inner.backend {
		StoreBackend::Local(local) => Ok(local),
		StoreBackend::InMemory => Err(FerrosGrynnError::invalid_argument("local fixture expected")),
	}
}

/// Captures the same file/WAL pins that a transaction reader will retain.
fn snapshot(store: &Datastore) -> FerrosGrynnResult<LocalChangefeedSnapshot> {
	local(store)?
		.capture_changefeed_snapshot(store.inner.current_root().commit_ts)?
		.ok_or_else(|| FerrosGrynnError::invalid_argument("fixture physical snapshot absent"))
}

/// Reads every complete group, checking catalog counts and exact page ceilings.
fn groups(store: &Datastore, feed: ChangefeedId) -> FerrosGrynnResult<Vec<CapturedCommit>> {
	let root = store.inner.current_root();
	let snapshot = snapshot(store)?;
	let mut groups = Vec::new();
	if let Some(segments) = root.changefeed_segments().get(&feed) {
		for segment in segments.iter().copied() {
			let mut first = 1;
			let start = groups.len();
			while first < segment.header.pages {
				let (group, next) = local(store)?.read_changefeed_group(
					&snapshot,
					segment,
					first,
					256 * 1024 * 1024,
				)?;
				first = next;
				groups.push(group);
			}
			assert_eq!(first, segment.header.pages);
			assert_eq!((groups.len() - start) as u64, segment.header.commits);
		}
	}
	assert!(root.change_history().is_empty(), "local snapshots must use durable groups");
	Ok(groups)
}

/// Gets the first authoritative descriptor without indexing an absent fixture.
fn first_segment(store: &Datastore, feed: ChangefeedId) -> FerrosGrynnResult<ChangefeedSegment> {
	store
		.inner
		.current_root()
		.changefeed_segments()
		.get(&feed)
		.and_then(|segments| segments.first())
		.copied()
		.ok_or_else(|| FerrosGrynnError::invalid_argument("fixture segment absent"))
}

#[test]
fn changefeed_local_append_full_only_pins_checkpoint_and_reopen() -> FerrosGrynnResult<()> {
	let directory = TestDirectory::new();
	let (store, record, key, class_feed, graph_feed) = fixture(&directory.0)?;
	let mut update = store.begin();
	update.set_base_property(&record, key, Value::from(2))?;
	let first_stamp = update.commit()?.commit_ts();
	let old_segment = first_segment(&store, class_feed)?;
	let old = snapshot(&store)?;
	let mut old_transaction = store.begin();
	let mut update = store.begin();
	update.set_base_property(&record, key, Value::from(3))?;
	update.commit()?;
	let mut noop = store.begin();
	noop.remove_base_property(&record, PropertyKeyId::new(999))?;
	let noop_stamp = noop.commit()?.commit_ts();
	assert_eq!(first_segment(&store, class_feed)?.file, old_segment.file);
	assert_eq!(first_segment(&store, class_feed)?.header.commits, 3);
	store.checkpoint(CheckpointMode::Passive)?;
	let (historical, ceiling) =
		local(&store)?.read_changefeed_group(&old, old_segment, 1, 256 * 1024 * 1024)?;
	assert_eq!(historical.net.versionstamp().get(), first_stamp);
	assert_eq!(ceiling, old_segment.header.pages);
	assert_eq!(
		historical.net.entries()[0].after().and_then(|image| image.properties().get("value")),
		Some(&Value::from(2))
	);
	let class_groups = groups(&store, class_feed)?;
	assert_eq!(class_groups.len(), 3);
	assert_eq!(class_groups[2].net.versionstamp().get(), noop_stamp);
	assert!(class_groups[2].net.entries().is_empty());
	assert_eq!(class_groups[2].full.as_ref().map(|full| full.entries().len()), Some(1));
	assert_eq!(groups(&store, graph_feed)?.len(), 2);
	let mut old_reader = old_transaction.changes(class_feed, ChangefeedReadOptions::default())?;
	assert_eq!(old_reader.next_batch()?, Some(ChangefeedReadItem::Commit(historical.net.clone())));
	assert!(old_reader.next_batch()?.is_none());
	drop(old_reader);
	drop(old_transaction);
	let mut transaction = store.begin();
	let mut reader =
		transaction.changes(class_feed, ChangefeedReadOptions::default().with_limit(1))?;
	assert_eq!(reader.next_batch()?, Some(ChangefeedReadItem::Commit(class_groups[0].net.clone())));
	let cursor = reader
		.cursor()
		.ok_or_else(|| FerrosGrynnError::invalid_argument("missing local cursor"))?;
	assert!(reader.next_batch()?.is_none());
	drop(reader);
	let mut resumed = transaction
		.changes(class_feed, ChangefeedReadOptions::new(ChangefeedSince::Cursor(cursor)))?;
	assert_eq!(
		resumed.next_batch()?,
		Some(ChangefeedReadItem::Commit(class_groups[1].net.clone()))
	);
	assert!(resumed.next_batch()?.is_none());
	assert_eq!(resumed.cursor().map(|cursor| cursor.versionstamp().get()), Some(noop_stamp));
	drop(resumed);
	drop(transaction);
	// A later full generation must keep every existing feed file registration.
	let mut ddl = store.begin();
	ddl.define_class(ClassSchema::new("unrelated", AtomKind::Node, [])?)?;
	ddl.commit()?;
	assert_eq!(groups(&store, class_feed)?.len(), 3);
	drop(old);
	store.checkpoint(CheckpointMode::Restart)?;
	drop(store);
	let reopened = Datastore::open_local(&directory.0, LocalOptions::default())?;
	let restored = groups(&reopened, class_feed)?;
	assert_eq!(restored.len(), 3);
	assert_eq!(restored[0].net, class_groups[0].net);
	assert_eq!(restored[2].full, class_groups[2].full);
	let mut transaction = reopened.begin();
	let mut full = transaction.changes(
		class_feed,
		ChangefeedReadOptions::default().with_mode(ChangefeedReadMode::Full),
	)?;
	for group in &restored {
		assert_eq!(full.next_batch()?, group.full.clone().map(ChangefeedReadItem::Commit));
	}
	assert!(full.next_batch()?.is_none());
	assert_eq!(groups(&reopened, graph_feed)?.len(), 2);
	Ok(())
}

#[test]
fn changefeed_local_import_on_off_is_atomic_and_survives_later_generation() -> FerrosGrynnResult<()>
{
	for mode in [ImportChangefeedMode::On, ImportChangefeedMode::Off] {
		let directory = TestDirectory::new();
		let (store, record, key, feed, _graph_feed) = fixture(&directory.0)?;
		let report = store.import(
			ImportOptions {
				changefeed: mode,
				segment_size: 1,
				on_violation: ImportViolationPolicy::Skip,
				reject_limit: Some(1),
				..ImportOptions::default()
			},
			|session| {
				session.apply_item(|tx| tx.set_base_property(&record, key, Value::from(2)))?;
				session.apply_item(|tx| tx.create_atom(record.clone(), PropertyMap::new()))?;
				session.apply_item(|tx| tx.set_base_property(&record, key, Value::from(3)))?;
				session
					.apply_item(|tx| tx.remove_base_property(&record, PropertyKeyId::new(999)))?;
				Ok(())
			},
		)?;
		assert_eq!(report.applied_items(), 3);
		assert_eq!(report.skipped_items(), 1);
		let captured = groups(&store, feed)?;
		assert_eq!(captured.len(), 1);
		if mode == ImportChangefeedMode::On {
			assert_eq!(captured[0].full.as_ref().map(|full| full.entries().len()), Some(3));
			assert_eq!(
				captured[0].net.entries()[0]
					.before()
					.and_then(|image| image.properties().get("value")),
				Some(&Value::from(1))
			);
			assert_eq!(
				captured[0].net.entries()[0]
					.after()
					.and_then(|image| image.properties().get("value")),
				Some(&Value::from(3))
			);
		} else {
			assert_eq!(
				captured[0].gap.as_ref().map(|gap| gap.reason()),
				Some(ChangefeedGapReason::ImportSuppressed)
			);
		}
		let mut ddl = store.begin();
		ddl.define_class(ClassSchema::new("later", AtomKind::Node, [])?)?;
		ddl.commit()?;
		drop(store);
		let reopened = Datastore::open_local(&directory.0, LocalOptions::default())?;
		assert_eq!(groups(&reopened, feed)?[0].net, captured[0].net);
		assert_eq!(groups(&reopened, feed)?[0].gap, captured[0].gap);
	}
	Ok(())
}

#[test]
fn changefeed_local_failed_publication_preserves_old_bounds_and_primary() -> FerrosGrynnResult<()> {
	for point in [
		GenerationFailurePoint::AfterBuild,
		GenerationFailurePoint::AfterFileSync,
		GenerationFailurePoint::AfterCatalogStage,
		GenerationFailurePoint::BeforeWalCommit,
	] {
		let directory = TestDirectory::new();
		let (store, record, key, feed, _graph_feed) = fixture(&directory.0)?;
		let mut update = store.begin();
		update.set_base_property(&record, key, Value::from(2))?;
		update.commit()?;
		let previous = first_segment(&store, feed)?;
		let stamp = store.inner.current_root().commit_ts;
		let failure = GenerationFailureGuard::set(point);
		let mut update = store.begin();
		update.set_base_property(&record, key, Value::from(3))?;
		assert!(update.commit().is_err());
		drop(failure);
		assert_eq!(store.inner.current_root().commit_ts, stamp);
		assert_eq!(first_segment(&store, feed)?, previous);
		drop(store);
		let reopened = Datastore::open_local(&directory.0, LocalOptions::default())?;
		let restored = groups(&reopened, feed)?;
		assert_eq!(restored.len(), 1);
		assert_eq!(
			restored[0].net.entries()[0].after().and_then(|image| image.properties().get("value")),
			Some(&Value::from(2))
		);
	}
	Ok(())
}

/// Corrupt optional segments preserve independent catalog bounds for a GAP;
/// unrelated primary records and other feeds remain readable after reopen.
#[test]
fn changefeed_local_missing_or_malformed_segment_preserves_trusted_bounds() -> FerrosGrynnResult<()>
{
	for missing in [false, true] {
		let directory = TestDirectory::new();
		let (store, record, key, feed, graph_feed) = fixture(&directory.0)?;
		let mut update = store.begin();
		update.set_base_property(&record, key, Value::from(2))?;
		update.commit()?;
		let expected = first_segment(&store, feed)?;
		store.checkpoint(CheckpointMode::Restart)?;
		let path = directory.0.join(format!(
			"changefeed/{}/{}.fcf",
			feed.as_uuid(),
			expected.header.segment
		));
		drop(store);
		let mutation = if missing {
			std::fs::remove_file(&path)
		} else {
			std::fs::write(&path, [1])
		};
		mutation.map_err(|error| {
			FerrosGrynnError::storage_io_failure("inject feed file corruption")
				.with_technical_source(error)
		})?;
		let reopened = Datastore::open_local(&directory.0, LocalOptions::default())?;
		assert_eq!(first_segment(&reopened, feed)?, expected);
		let snapshot = snapshot(&reopened)?;
		assert!(
			local(&reopened)?
				.read_changefeed_group(&snapshot, expected, 1, 256 * 1024 * 1024)
				.is_err()
		);
		assert_eq!(groups(&reopened, graph_feed)?.len(), 1);
		let mut transaction = reopened.begin();
		let mut reader = transaction
			.changes(feed, ChangefeedReadOptions::default().with_mode(ChangefeedReadMode::Full))?;
		assert!(
			matches!(reader.next_batch()?, Some(ChangefeedReadItem::Gap(gap)) if gap.reason() == ChangefeedGapReason::SegmentCorrupt && gap.from().map(|stamp| stamp.get()) == Some(expected.header.first_stamp) && gap.through().map(|stamp| stamp.get()) == Some(expected.header.last_stamp))
		);
		assert!(reader.next_batch()?.is_none());
		drop(reader);
		let mut limited = transaction
			.changes(feed, ChangefeedReadOptions::default().with_budgets(4_000, 100)?)?;
		assert!(limited.next_batch().is_err());
		drop(limited);
		drop(transaction);
		let mut unrelated = reopened.begin();
		unrelated.define_class(ClassSchema::new("unrelated", AtomKind::Node, [])?)?;
		unrelated.commit()?;
		assert_eq!(first_segment(&reopened, feed)?, expected);
	}
	Ok(())
}

/// Historical capture flags, private changes and metadata budgets use durable
/// groups, independently of the source's latest FULL configuration.
#[test]
fn changefeed_local_reader_checks_full_coverage_before_pull_and_keeps_private_reads_committed()
-> FerrosGrynnResult<()> {
	let directory = TestDirectory::new();
	let (store, record, key, feed, _graph_feed) = fixture(&directory.0)?;
	let info = store.begin().changefeed_info(feed)?;
	let mut net = store.begin();
	net.configure_changefeed(
		info.source(),
		ChangefeedConfig::from_nanoseconds(86_400_000_000_000)?,
	)?;
	net.set_base_property(&record, key, 2.into())?;
	let net_stamp = net.commit()?.versionstamp();
	let mut full = store.begin();
	full.configure_changefeed(info.source(), info.configuration())?;
	full.set_base_property(&record, key, 3.into())?;
	let full_stamp = full.commit()?.versionstamp();
	let mut tx = store.begin();
	assert!(
		tx.changes(
			feed,
			ChangefeedReadOptions::default().with_mode(ChangefeedReadMode::Full).with_limit(1)
		)
		.is_err()
	);
	tx.set_base_property(&record, key, 4.into())?;
	let mut reader = tx.changes(
		feed,
		ChangefeedReadOptions::new(ChangefeedSince::Versionstamp(net_stamp))
			.with_mode(ChangefeedReadMode::Full),
	)?;
	assert!(
		matches!(reader.next_batch()?, Some(ChangefeedReadItem::Commit(batch)) if batch.versionstamp() == full_stamp && batch.entries()[0].after().and_then(|image| image.properties().get("value")) == Some(&Value::from(3)))
	);
	assert!(reader.next_batch()?.is_none());
	drop(reader);
	assert!(
		tx.changes(
			feed,
			ChangefeedReadOptions::default()
				.with_mode(ChangefeedReadMode::Full)
				.with_budgets(256 * 1024 * 1024, 1)?
		)
		.is_err()
	);
	drop(tx);
	Ok(())
}

/// Valid first-page metadata must not make a corrupt payload skip a healthy
/// tail.
#[test]
fn changefeed_local_corrupt_last_payload_yields_gap_then_next_segment() -> FerrosGrynnResult<()> {
	use std::io::{Seek, SeekFrom, Write};

	use crate::storage::{local_store::changefeed::SegmentTargetGuard, page::PAGE_SIZE};
	let _rotation = SegmentTargetGuard::set(4 * PAGE_SIZE as u64);
	let directory = TestDirectory::new();
	let (store, record, key, feed, _graph_feed) = fixture(&directory.0)?;
	let mut first = store.begin();
	first.set_base_property(
		&record,
		key,
		Value::Bytes(crate::values::Bytes::from(vec![7; 20_000])),
	)?;
	first.commit()?;
	let corrupt = first_segment(&store, feed)?;
	assert!(corrupt.header.pages > 2);
	let mut second = store.begin();
	second.set_base_property(&record, key, 3.into())?;
	let tail = second.commit()?.versionstamp();
	assert_eq!(
		store.inner.current_root().changefeed_segments().get(&feed).map(|segments| segments.len()),
		Some(2)
	);
	store.checkpoint(CheckpointMode::Restart)?;
	let path =
		directory.0.join(format!("changefeed/{}/{}.fcf", feed.as_uuid(), corrupt.header.segment));
	drop(store);
	let failure = |error| {
		FerrosGrynnError::storage_io_failure("inject continuation corruption")
			.with_technical_source(error)
	};
	let mut file = std::fs::OpenOptions::new().write(true).open(path).map_err(failure)?;
	file.seek(SeekFrom::Start(2 * PAGE_SIZE as u64 + 100)).map_err(failure)?;
	file.write_all(&[99]).map_err(failure)?;
	file.sync_all().map_err(failure)?;
	drop(file);
	let reopened = Datastore::open_local(&directory.0, LocalOptions::default())?;
	let mut tx = reopened.begin();
	let mut reader =
		tx.changes(feed, ChangefeedReadOptions::default().with_mode(ChangefeedReadMode::Full))?;
	assert!(
		matches!(reader.next_batch()?, Some(ChangefeedReadItem::Gap(gap)) if gap.reason() == ChangefeedGapReason::SegmentCorrupt && gap.through().map(|stamp| stamp.get()) == Some(corrupt.header.last_stamp))
	);
	assert!(
		matches!(reader.next_batch()?, Some(ChangefeedReadItem::Commit(batch)) if batch.versionstamp() == tail && batch.entries()[0].after().and_then(|image| image.properties().get("value")) == Some(&Value::from(3)))
	);
	assert!(reader.next_batch()?.is_none());
	Ok(())
}

/// Repeated known failures creating the first segments leave no growing set
/// of unreferenced feed files in a running database.
#[test]
fn changefeed_local_first_segment_known_failures_remove_pending_files() -> FerrosGrynnResult<()> {
	let directory = TestDirectory::new();
	let (store, record, key, feed, _graph_feed) = fixture(&directory.0)?;
	for point in [
		GenerationFailurePoint::AfterFileSync,
		GenerationFailurePoint::AfterCatalogStage,
		GenerationFailurePoint::BeforeWalCommit,
	] {
		for _retry in 0..2 {
			let failure = GenerationFailureGuard::set(point);
			let mut update = store.begin();
			update.set_base_property(&record, key, Value::from(2))?;
			assert!(update.commit().is_err());
			drop(failure);
			assert!(groups(&store, feed)?.is_empty());
			assert_eq!(segment_file_count(&directory.0)?, 0);
		}
	}
	Ok(())
}

/// Counts real files, including unreferenced UUIDs absent from catalog state.
fn segment_file_count(root: &std::path::Path) -> FerrosGrynnResult<usize> {
	let directory = root.join("changefeed");
	if !directory.exists() {
		return Ok(0);
	}
	let failure = |error| {
		FerrosGrynnError::storage_io_failure("inspect fixture feed files")
			.with_technical_source(error)
	};
	let mut count = 0;
	for directory in std::fs::read_dir(directory).map_err(failure)? {
		let directory = directory.map_err(failure)?;
		if !directory.file_type().map_err(failure)?.is_dir() {
			continue;
		}
		for file in std::fs::read_dir(directory.path()).map_err(failure)? {
			if file.map_err(failure)?.file_type().map_err(failure)?.is_file() {
				count += 1;
			}
		}
	}
	Ok(count)
}

/// A source with no segments must still obey the recovered publication clock.
#[test]
fn changefeed_local_recovery_rejects_future_source_clocks_without_segments() -> FerrosGrynnResult<()>
{
	let directory = TestDirectory::new();
	let (store, _record, _key, _feed, _graph_feed) = fixture(&directory.0)?;
	let stamp = store.inner.current_root().commit_ts;
	let mut catalog = local(&store)?.state.lock()._database_catalog.clone();
	assert!(catalog.changefeeds.iter().all(|entry| entry.segments.is_empty()));
	super::super::changefeed::validate_recovered(&catalog, stamp)?;
	catalog.changefeeds[0].published_at = stamp + 1;
	catalog.changefeeds[0].epochs[0].started_at = (stamp + 1).into();
	drop(catalog.encode()?);
	assert!(super::super::changefeed::validate_recovered(&catalog, stamp).is_err());
	Ok(())
}

/// Files remain recoverable when transport reports an error after writing a
/// complete commit. Quarantine and feed pages share the primary/catalog result.
#[test]
fn changefeed_local_uncertain_wal_import_recovers_feed_primary_and_quarantine()
-> FerrosGrynnResult<()> {
	use crate::storage::wal::{CommitFailureGuard, CommitFailurePoint};
	for point in [
		CommitFailurePoint::BeforeAppend,
		CommitFailurePoint::AfterAppend,
		CommitFailurePoint::AfterFlush,
		CommitFailurePoint::AfterSync,
	] {
		let directory = TestDirectory::new();
		let (store, record, key, feed, _graph_feed) = fixture(&directory.0)?;
		let old_stamp = store.inner.current_root().commit_ts;
		let failure = CommitFailureGuard::set(point);
		let result = store.import(
			ImportOptions {
				changefeed: ImportChangefeedMode::On,
				segment_size: 1,
				on_violation: ImportViolationPolicy::Quarantine,
				reject_limit: Some(1),
				..ImportOptions::default()
			},
			|session| {
				session.apply_item_with_payload(Value::from("accepted"), |tx| {
					tx.set_base_property(&record, key, Value::from(2))
				})?;
				session.apply_item_with_payload(Value::from("duplicate"), |tx| {
					tx.create_atom(record.clone(), PropertyMap::new())
				})?;
				Ok(())
			},
		);
		assert!(result.is_err());
		drop(failure);
		assert_eq!(store.inner.current_root().commit_ts, old_stamp);
		let pending_files = segment_file_count(&directory.0)?;
		let pending_generations = std::fs::read_dir(directory.0.join("atoms"))
			.map_err(|error| {
				FerrosGrynnError::storage_io_failure("inspect pending fixture generations")
					.with_technical_source(error)
			})?
			.count();
		for _retry in 0..2 {
			let mut retry = store.begin();
			retry.set_base_property(&record, key, Value::from(3))?;
			assert!(retry.commit().is_err());
			assert_eq!(segment_file_count(&directory.0)?, pending_files);
			assert_eq!(
				std::fs::read_dir(directory.0.join("atoms"))
					.map_err(|error| FerrosGrynnError::storage_io_failure(
						"inspect retry fixture generations"
					)
					.with_technical_source(error))?
					.count(),
				pending_generations
			);
		}
		drop(store);
		let reopened = Datastore::open_local(&directory.0, LocalOptions::default())?;
		let recovered = groups(&reopened, feed)?;
		if point == CommitFailurePoint::BeforeAppend {
			assert_eq!(reopened.inner.current_root().commit_ts, old_stamp);
			assert!(recovered.is_empty());
			assert_eq!(segment_file_count(&directory.0)?, 0);
		} else {
			assert_eq!(reopened.inner.current_root().commit_ts, old_stamp + 1);
			assert_eq!(recovered.len(), 1);
			assert_eq!(recovered[0].full.as_ref().map(|full| full.entries().len()), Some(1));
			let mut read = reopened.begin();
			assert_eq!(
				read.atom(&record)?.as_ref().and_then(|atom| atom.properties().get(key)),
				Some(&Value::from(2))
			);
			let state = local(&reopened)?.state.lock();
			let quarantine = state
				.file_catalog
				.files
				.iter()
				.find(|file| {
					file.file_id.kind()
						== Some(crate::storage::types::DataFileKind::ImportQuarantine)
				})
				.ok_or_else(|| FerrosGrynnError::invalid_argument("recovered quarantine absent"))?;
			let id = quarantine
				.path
				.as_str()
				.strip_prefix("admin/quarantine/")
				.and_then(|name| name.strip_suffix(".fgq"))
				.ok_or_else(|| {
					FerrosGrynnError::invalid_argument("quarantine fixture path invalid")
				})?
				.parse::<crate::values::Uuid>()
				.map_err(|error| FerrosGrynnError::invalid_argument(error.to_string()))?;
			drop(state);
			let mut rejected = local(&reopened)?.open_quarantine(id)?;
			assert_eq!(
				rejected.next_record()?.map(|item| item.payload),
				Some(Value::from("duplicate"))
			);
		}
	}
	Ok(())
}

/// A feed-only no-op uses the same uncertain durability barrier as ordinary
/// primary writes, preserving unpublished files until reopen determines
/// reachability.
#[test]
fn changefeed_local_uncertain_wal_full_noop_recovers_one_complete_commit() -> FerrosGrynnResult<()>
{
	use crate::storage::wal::{CommitFailureGuard, CommitFailurePoint};
	for point in [
		CommitFailurePoint::BeforeAppend,
		CommitFailurePoint::AfterAppend,
		CommitFailurePoint::AfterFlush,
		CommitFailurePoint::AfterSync,
	] {
		let directory = TestDirectory::new();
		let (store, record, key, feed, graph_feed) = fixture(&directory.0)?;
		let old_stamp = store.inner.current_root().commit_ts;
		let ChangefeedSource::Graph(graph) = store.begin().changefeed_info(graph_feed)?.source()
		else {
			return Err(FerrosGrynnError::invalid_argument("graph source absent"));
		};
		let guard = CommitFailureGuard::set(point);
		let mut tx = store.begin();
		tx.set_base_property(&record, key, 1.into())?;
		tx.set_occurrence_label(graph, &record, None)?;
		assert!(tx.commit().is_err());
		drop(guard);
		assert_eq!(store.inner.current_root().commit_ts, old_stamp);
		let pending = segment_file_count(&directory.0)?;
		let mut retry = store.begin();
		retry.set_base_property(&record, key, 1.into())?;
		assert!(retry.commit().is_err());
		assert_eq!(segment_file_count(&directory.0)?, pending);
		drop(store);
		let store = Datastore::open_local(&directory.0, LocalOptions::default())?;
		let recovered_stamp = store.inner.current_root().commit_ts;
		for id in [feed, graph_feed] {
			let recovered = groups(&store, id)?;
			if point == CommitFailurePoint::BeforeAppend {
				assert_eq!(recovered_stamp, old_stamp);
				assert!(recovered.is_empty());
				assert_eq!(segment_file_count(&directory.0)?, 0);
			} else {
				assert_eq!(recovered_stamp, old_stamp + 1);
				assert_eq!(recovered.len(), 1);
				assert!(recovered[0].net.entries().is_empty());
				let full = recovered[0]
					.full
					.as_ref()
					.ok_or_else(|| FerrosGrynnError::invalid_argument("recovered FULL absent"))?;
				assert_eq!(full.entries().len(), 1);
				assert_eq!(full.versionstamp().get(), recovered_stamp);
				assert_eq!(recovered[0].net.versionstamp().get(), recovered_stamp);
				for image in [full.entries()[0].before(), full.entries()[0].after()] {
					let image = image.ok_or_else(|| {
						FerrosGrynnError::invalid_argument("recovered no-op image absent")
					})?;
					assert!(
						matches!(image.identity(), crate::changefeed::ChangeIdentity::Record(atom) if atom.id() == &record)
							|| matches!(image.identity(), crate::changefeed::ChangeIdentity::Occurrence(atom) if atom.atom().id() == &record && atom.graph().id() == graph)
					);
					assert_eq!(image.properties().get("value"), Some(&Value::from(1)));
				}
				assert_eq!(full.entries()[0].before(), full.entries()[0].after());
			}
		}
		assert_eq!(
			store.begin().atom(&record)?.as_ref().and_then(|atom| atom.properties().get(key)),
			Some(&Value::from(1))
		);
		// A pre-append failure can leave no WAL-only pages to backfill.
		let _checkpoint_progress = store.checkpoint(CheckpointMode::Restart)?;
		drop(store);
		let store = Datastore::open_local(&directory.0, LocalOptions::default())?;
		assert_eq!(store.inner.current_root().commit_ts, recovered_stamp);
		assert_eq!(
			groups(&store, feed)?.len(),
			usize::from(point != CommitFailurePoint::BeforeAppend)
		);
	}
	Ok(())
}
