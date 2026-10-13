//! Deterministic control races between optimistic import preparations.

use super::*;
use crate::{
	changefeed::{ChangefeedConfig, ChangefeedSource},
	datastore::{ClassField, ClassSchema, ConflictKey, IsolationLevel},
	graph::{AtomKind, GraphKind},
	live::{LiveExpr, LiveOptions, LivePayload, LiveProjection, LiveQuery, LiveSource, LiveState},
	values::{PropertyMap, RecordId, Type},
};

/// A completed preparation must remain retryable without losing its accepted
/// spool or retaining envelopes from an unpublished attempt.
#[test]
fn live_import_preparation_retry_keeps_spool_and_latest_controls() -> FerrosGrynnResult<()> {
	let store = Datastore::in_memory();
	let mut setup = store.begin();
	let class =
		setup.define_class(ClassSchema::new("item", AtomKind::Node, [ClassField::new(
			"value",
			Type::Int,
		)])?)?;
	let key = setup.intern_property("value")?;
	let record: RecordId = "item:a".parse()?;
	setup.create_atom(record.clone(), PropertyMap::from_iter([(key, 1.into())]))?;
	let second: RecordId = "item:b".parse()?;
	setup.create_atom(second.clone(), PropertyMap::from_iter([(key, 2.into())]))?;
	let graph = setup.define_graph("scope", GraphKind::Metagraph)?;
	setup.attach_node(graph.id(), &record, None, None)?;
	setup.commit()?;
	let mut setup = store.begin();
	let feed = setup.configure_changefeed(
		ChangefeedSource::Class(class),
		ChangefeedConfig::from_nanoseconds(86_400_000_000_000)?.with_full(true),
	)?;
	let query = LiveQuery::new(LiveSource::Record(record.clone()))
		.with_projection(LiveProjection::Value(LiveExpr::field("value")?));
	let live = setup.register_live(query.clone(), LiveOptions::default())?;
	let stopped = setup.register_live(query, LiveOptions::default())?;
	setup.commit()?;
	let base = store.inner.current_root();
	let owner = store.inner.next_transaction.fetch_add(1, Ordering::Relaxed);
	let lease = ImportLease::acquire(store.inner.clone(), owner)?;
	let options = ImportOptions {
		changefeed: ImportChangefeedMode::On,
		segment_size: 1,
		..ImportOptions::default()
	};
	let provisional = base.commit_ts + 1;
	let tx = Transaction::new_import(
		store.clone(),
		owner,
		TransactionOptions::new(IsolationLevel::Serializable, TransactionAccess::ReadWrite),
		Arc::clone(&base),
		options.indexes,
		options.constraints,
		options.on_violation,
		None,
		options.changefeed,
		options.events,
		provisional,
	);
	let mut session =
		ImportSession::new(tx, options, provisional, store.inner.import_scratch_root());
	session.apply_item(|tx| tx.set_base_property(&record, key, 3.into()))?;
	session.apply_item(|tx| {
		tx.set_base_property(&record, key, 5.into())?;
		tx.attach_node(graph.id(), &second, None, None)
	})?;
	let (tx, _report, changed, quarantine) = session.finish()?;
	assert!(changed);
	assert!(quarantine.is_none());
	let (mut tx, _indexes) = tx.finish_import(provisional, options.indexes, false)?;
	tx.prepare_import_publication(&base, 256 * 1024 * 1024)?;
	let mut close = store.begin();
	close.close_live(stopped)?;
	let close_stamp = close.commit()?.commit_ts();
	assert_eq!(close_stamp, provisional);
	let observed = store.inner.current_root();
	tx.prepare_import_publication(&observed, 256 * 1024 * 1024)?;
	let mut root = tx.into_import_root();
	assert_eq!(root.commit_ts, close_stamp + 1);
	assert_eq!(root.version(ConflictKey::LiveDefinition(stopped)), close_stamp);
	assert_eq!(root.graphs[&graph.id()].tensor_revision, root.commit_ts);
	assert!(!root.change_history().contains_key(&(feed, provisional)));
	assert!(!root.live_history().contains_key(&(live, provisional)));
	let source = root
		.change_history()
		.get(&(feed, root.commit_ts))
		.ok_or_else(|| FerrosGrynnError::invalid_argument("retry lost accepted source history"))?;
	assert_eq!(source.full.as_ref().map(|batch| batch.entries().len()), Some(2));
	let bytes = root
		.live_history()
		.get(&(live, root.commit_ts))
		.ok_or_else(|| FerrosGrynnError::invalid_argument("retry lost computed history"))?;
	let LivePayload::Batch(batch) = crate::storage::live_history::decode(bytes, 8 * 1024 * 1024)?
	else {
		return Err(FerrosGrynnError::invalid_argument("retry did not produce a batch"));
	};
	assert_eq!(batch.events()[0].before(), Some(&Value::from(1)));
	assert_eq!(batch.events()[0].after(), Some(&Value::from(5)));
	assert_eq!(root.live_catalog().queries[&stopped].state, LiveState::Closed);
	let _commit = store.inner.commit.lock();
	assert!(Arc::ptr_eq(&observed, &store.inner.current_root()));
	store.inner.persist_import_root(&mut root, None)?;
	*store.inner.current.write() = Arc::new(root);
	drop(_commit);
	drop(lease);
	assert_eq!(store.begin().live_info(stopped)?.state(), LiveState::Closed);
	Ok(())
}

/// Models a repeated UTC time at the preparation boundary without relying on
/// operating-system clock resolution or sleeping during a test.
fn repeated_time_publication(store: &Datastore, import: bool) -> FerrosGrynnResult<()> {
	let mut setup = store.begin();
	let class =
		setup.define_class(ClassSchema::new("item", AtomKind::Node, [ClassField::new(
			"value",
			Type::Int,
		)])?)?;
	let key = setup.intern_property("value")?;
	let record: RecordId = "item:a".parse()?;
	setup.create_atom(record.clone(), PropertyMap::from_iter([(key, 1.into())]))?;
	setup.configure_changefeed(
		ChangefeedSource::Class(class),
		ChangefeedConfig::from_nanoseconds(86_400_000_000_000)?,
	)?;
	let live = setup.register_live(
		LiveQuery::new(LiveSource::Record(record.clone()))
			.with_projection(LiveProjection::Value(LiveExpr::field("value")?)),
		LiveOptions::default(),
	)?;
	setup.commit()?;
	let previous = store.inner.current_root();
	let mut root = (*previous).clone();
	root.commit_ts = previous.commit_ts + 1;
	let atom = root
		.resolve_atom(&record)
		.ok_or_else(|| FerrosGrynnError::invalid_argument("missing clock fixture atom"))?;
	root.set_base_property(atom, key, 2.into())?;
	let frozen = root.committed_at_ns;
	assert_eq!(frozen, previous.committed_at_ns, "preparation accepts a repeated time");
	crate::datastore::transaction::prepare_live_publication(
		&previous,
		&mut root,
		store.inner.query_scratch_root(),
		crate::datastore::transaction::LiveHistoryCapture::Enabled,
		crate::datastore::transaction::LiveQueryTargets::All,
		&store.inner.live_observation,
	)?;
	let bytes = Arc::clone(
		root.live_history()
			.get(&(live, root.commit_ts))
			.ok_or_else(|| FerrosGrynnError::invalid_argument("missing repeated-time envelope"))?,
	);
	if import {
		store.inner.persist_import_root(&mut root, None)?;
	} else {
		store.inner.persist_root(&mut root, Some(&previous), None, false, false)?;
	}
	assert_eq!(
		root.committed_at_ns, frozen,
		"persistence must not refresh the prepared commit clock"
	);
	let info = crate::storage::live_history::inspect(&bytes, bytes.len() as u64)?;
	assert_eq!(info.time, root.committed_at_ns);
	Ok(())
}

/// The ordinary barrier preserves a prepared time even if the clock advances
/// after encoding and the preceding commit has the same time.
#[test]
fn live_publication_repeated_time_keeps_normal_clock() -> FerrosGrynnResult<()> {
	repeated_time_publication(&Datastore::in_memory(), false)
}

/// Import END preserves the same immutable clock contract.
#[test]
fn live_import_repeated_time_keeps_final_clock() -> FerrosGrynnResult<()> {
	repeated_time_publication(&Datastore::in_memory(), true)
}

/// Local publication must accept the frozen time rather than reject its
/// already encoded computed history as malformed.
#[cfg(windows)]
fn repeated_time_local(mode: crate::datastore::IoMode) -> FerrosGrynnResult<()> {
	for import in [false, true] {
		let path = std::env::temp_dir().join(format!(
			"fgdb-{:016x}",
			Uuid::now_v7().as_inner().as_u128() & u128::from(u64::MAX)
		));
		let mut options = crate::datastore::LocalOptions::default();
		options.io_mode = mode;
		let store = Datastore::create_local(&path, options)?;
		let result = repeated_time_publication(&store, import);
		drop(store);
		std::fs::remove_dir_all(&path).map_err(|source| {
			FerrosGrynnError::storage_io_failure("remove live clock fixture")
				.with_technical_source(source)
		})?;
		result?;
	}
	Ok(())
}

/// Sync independently exercises both actual local barriers with equal times.
#[cfg(windows)]
#[test]
fn live_import_repeated_time_keeps_local_sync_clock() -> FerrosGrynnResult<()> {
	repeated_time_local(crate::datastore::IoMode::Sync)
}

/// NativeAsync is selected as a distinct supported backend target.
#[cfg(all(windows, feature = "native-async-io"))]
#[test]
fn live_import_repeated_time_keeps_local_native_clock() -> FerrosGrynnResult<()> {
	repeated_time_local(crate::datastore::IoMode::NativeAsync)
}

/// Resident unchanged revisions need no scratch copy, but a real control
/// rebase still fails before COW when the admitted budget cannot hold it.
#[test]
fn live_import_revision_admission_only_charges_required_rebase() -> FerrosGrynnResult<()> {
	let store = Datastore::in_memory();
	let mut setup = store.begin();
	let class = setup.define_class(ClassSchema::new("item", AtomKind::Node, [])?)?;
	let live =
		setup.register_live(LiveQuery::new(LiveSource::Class(class)), LiveOptions::default())?;
	setup.commit()?;
	let base = store.inner.current_root();
	let mut candidate = (*base).clone();
	candidate.commit_ts = base.commit_ts + 1;
	let revisions = Arc::clone(&candidate.versions);
	candidate.rebase_import_controls(&base, &base, 1, 1)?;
	assert!(Arc::ptr_eq(&candidate.versions, &revisions), "unchanged revisions stay shared");
	let mut close = store.begin();
	close.close_live(live)?;
	close.commit()?;
	let current = store.inner.current_root();
	assert!(candidate.rebase_import_controls(&base, &current, 1, 1).is_err());
	assert!(Arc::ptr_eq(&candidate.versions, &revisions), "rejected admission precedes COW");
	assert_eq!(current.live_catalog().queries[&live].state, LiveState::Closed);
	Ok(())
}
