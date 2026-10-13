//! Actual LIVE publication, shared file reachability and bounded pinned replay.

use std::sync::Arc;

/// Query-owned checkpoints survive reopen with or without optional history.
fn graph_checkpoint_survives_reopen(mode: IoMode) -> FerrosGrynnResult<()> {
	use crate::live::{LiveGraphResult, LiveNext};
	for durable_history in [false, true] {
		let directory = TestDirectory::new();
		let mut options = LocalOptions::default();
		options.io_mode = mode;
		options.buffer_pool_pages = 16;
		let store = Datastore::create_local(&directory.0, options.clone())?;
		let mut seed = store.begin();
		let class = seed.define_class(ClassSchema::new("item", AtomKind::Node, [])?)?;
		let key = seed.intern_property("value")?;
		let record: RecordId = "item:a".parse()?;
		seed.create_atom(record.clone(), PropertyMap::from_iter([(key, Value::from(1))]))?;
		if durable_history {
			seed.configure_changefeed(
				ChangefeedSource::Class(class),
				ChangefeedConfig::from_nanoseconds(86_400_000_000_000)?,
			)?;
		}
		seed.commit()?;
		let query = LiveQuery::new(LiveSource::Class(class)).with_graph_result(
			LiveGraphResult::new(LiveExpr::field("id")?, LiveExpr::Literal(Value::None)),
		);
		let mut never_active = store.begin();
		let closed = never_active.register_live(query.clone(), LiveOptions::default())?;
		never_active.close_live(closed)?;
		never_active.commit()?;
		let mut subscription = store.subscribe_live(query, LiveOptions::default())?;
		let id = subscription.id();
		let original = subscription.cursor().clone();
		let initial = subscription
			.inner
			.take_graph_checkpoint()
			.ok_or_else(|| FerrosGrynnError::invalid_argument("initial graph absent"))?;
		assert_eq!(initial.versionstamp(), original.versionstamp());
		assert!(store.inner.current_root().live_catalog().graph_segments.contains_key(&id));
		let mut write = store.begin();
		write.set_base_property(&record, key, Value::from(2))?;
		let changed = write.commit()?.versionstamp();
		let LiveNext::Delivery(delivery) = subscription.next()? else {
			return Err(FerrosGrynnError::invalid_argument("graph delivery absent"));
		};
		let LivePayload::Graph(expected) = delivery.payload() else {
			return Err(FerrosGrynnError::invalid_argument("graph delivery faulted"));
		};
		let expected = Arc::clone(expected);
		drop(subscription);
		store.checkpoint(CheckpointMode::Restart)?;
		let mut generation = store.begin();
		generation.define_class(ClassSchema::new("other", AtomKind::Node, [])?)?;
		generation.commit()?;
		drop(store);
		let store = Datastore::open_local(&directory.0, options)?;
		assert_eq!(store.begin().live_info(closed)?.state(), crate::live::LiveState::Closed);
		let mut unrelated = store.begin();
		unrelated.create_atom("other:a".parse()?, PropertyMap::new())?;
		unrelated.commit()?;
		let root = store.inner.current_root();
		let recovered = root
			.live_catalog()
			.graph_checkpoints
			.get(&id)
			.ok_or_else(|| FerrosGrynnError::invalid_argument("recovered graph absent"))?;
		assert_eq!(recovered.versionstamp(), changed);
		assert_eq!(recovered.delta().after(), expected.delta().after());
		assert_eq!(recovered.delta().before(), expected.delta().before());
		assert!(
			!root.live_catalog().prepared.iter().any(|(owner, _)| *owner == id),
			"unrelated post-open DML must not create a graph delta"
		);
		if durable_history {
			let mut replay = store.resume_live(&original)?;
			let LiveNext::Delivery(delivery) = replay.next()? else {
				return Err(FerrosGrynnError::invalid_argument("graph replay absent"));
			};
			let LivePayload::Graph(batch) = delivery.payload() else {
				return Err(FerrosGrynnError::invalid_argument("graph replay GAP"));
			};
			assert_eq!(batch.versionstamp(), changed);
			assert_eq!(batch.delta().after(), expected.delta().after());
			replay.ack(delivery.token())?;
			assert!(matches!(replay.next()?, LiveNext::Idle));
		}
	}
	Ok(())
}

#[test]
fn live_graph_checkpoint_local_sync_reopen_history_and_generation() -> FerrosGrynnResult<()> {
	graph_checkpoint_survives_reopen(IoMode::Sync)
}

#[cfg(feature = "native-async-io")]
#[test]
fn live_graph_checkpoint_local_native_async_reopen_history_and_generation() -> FerrosGrynnResult<()>
{
	graph_checkpoint_survives_reopen(IoMode::NativeAsync)
}

/// Failed durable ACK never reapplies a resolved candidate or loses its token.
fn graph_sdk_ack_failure(mode: IoMode) -> FerrosGrynnResult<()> {
	use crate::{
		graph::{AtomRef, Frozen, GraphMut, GraphRead, Subgraph},
		live::{LiveGraphNext, LiveGraphResult},
		storage::wal::{CommitFailureGuard, CommitFailurePoint},
	};
	for point in [CommitFailurePoint::BeforeAppend, CommitFailurePoint::AfterAppend] {
		let directory = TestDirectory::new();
		let mut options = LocalOptions::default();
		options.io_mode = mode;
		options.buffer_pool_pages = 16;
		let store = Datastore::create_local(&directory.0, options.clone())?;
		let mut seed = store.begin();
		let class = seed.define_class(ClassSchema::new("item", AtomKind::Node, [])?)?;
		let key = seed.intern_property("score")?;
		let record: RecordId = "item:a".parse()?;
		seed.create_atom(record.clone(), PropertyMap::from_iter([(key, Value::from(1))]))?;
		seed.configure_changefeed(
			ChangefeedSource::Class(class),
			ChangefeedConfig::from_nanoseconds(86_400_000_000_000)?,
		)?;
		seed.commit()?;
		let query = LiveQuery::new(LiveSource::Class(class)).with_graph_result(
			LiveGraphResult::new(LiveExpr::field("id")?, LiveExpr::Literal(Value::None)),
		);
		let mut subscription = store
			.subscribe_live_graph::<Subgraph<Frozen>>(query, LiveOptions::default())?
			.into_mutable()?;
		let saved = subscription.cursor().clone();
		let atom = AtomRef::new(AtomKind::Node, record.clone());
		subscription.materialization()?.mutate_local([], |graph| {
			let id = graph
				.atom_id(&atom)?
				.ok_or_else(|| FerrosGrynnError::invalid_argument("local atom absent"))?;
			graph.node_properties_mut(id)?.insert(key, 99);
			Ok(())
		})?;
		let mut write = store.begin();
		write.set_base_property(&record, key, 2.into())?;
		let revision = write.commit()?.versionstamp();
		assert!(matches!(subscription.next()?, LiveGraphNext::Conflict(_)));
		let failure = CommitFailureGuard::set(point);
		assert!(subscription.keep_local().is_err());
		drop(failure);
		assert_eq!(subscription.revision(), Some(revision));
		assert_eq!(subscription.cursor(), &saved);
		assert!(subscription.applied_unconfirmed().is_some());
		assert!(subscription.conflict().is_none());
		subscription.materialization()?.mutate_local([], |graph| {
			let id = graph
				.atom_id(&atom)?
				.ok_or_else(|| FerrosGrynnError::invalid_argument("local atom absent"))?;
			graph.node_properties_mut(id)?.insert(key, 120);
			Ok(())
		})?;
		// Injected WAL I/O failure poisons the writer until ordinary reopen.
		assert!(subscription.next().is_err());
		assert!(subscription.applied_unconfirmed().is_some());
		subscription.materialization()?.with_graph(|graph| {
			let id = graph
				.atom_id(&atom)?
				.ok_or_else(|| FerrosGrynnError::invalid_argument("local atom absent"))?;
			let node = graph
				.node(id)?
				.ok_or_else(|| FerrosGrynnError::invalid_argument("local node absent"))?;
			assert_eq!(node.properties().get(key), Some(&Value::from(120)));
			Ok::<(), FerrosGrynnError>(())
		})??;
		drop(store);
		assert!(matches!(subscription.next()?, LiveGraphNext::Closed));
		assert!(subscription.applied_unconfirmed().is_some());
		let store = Datastore::open_local(&directory.0, options)?;
		let mut resumed = store.resume_live_graph::<Subgraph<Frozen>>(&saved)?;
		if point == CommitFailurePoint::BeforeAppend {
			assert!(
				matches!(resumed.next()?, LiveGraphNext::Updated { revision: received } if received == revision)
			);
		}
		assert_eq!(resumed.revision(), Some(revision));
		assert!(matches!(resumed.next()?, LiveGraphNext::Idle));
		resumed.close()?;
	}
	Ok(())
}

#[test]
fn live_graph_sdk_ack_failure_local_sync() -> FerrosGrynnResult<()> {
	graph_sdk_ack_failure(IoMode::Sync)
}

#[cfg(feature = "native-async-io")]
#[test]
fn live_graph_sdk_ack_failure_local_native_async() -> FerrosGrynnResult<()> {
	graph_sdk_ack_failure(IoMode::NativeAsync)
}

/// Source write-back and graph checkpoint recovery share one local WAL barrier.
fn graph_sdk_flush_local(mode: IoMode) -> FerrosGrynnResult<()> {
	use crate::{
		graph::{AtomRef, Frozen, GraphMut, GraphRead, Subgraph},
		live::{LiveGraphNext, LiveGraphResult},
		storage::wal::{CommitFailureGuard, CommitFailurePoint},
	};
	let directory = TestDirectory::new();
	let mut options = LocalOptions::default();
	options.io_mode = mode;
	options.buffer_pool_pages = 16;
	let store = Datastore::create_local(&directory.0, options.clone())?;
	let mut seed = store.begin();
	let class = seed.define_class(ClassSchema::new("item", AtomKind::Node, [])?)?;
	let key = seed.intern_property("score")?;
	let record: RecordId = "item:one".parse()?;
	seed.create_atom(record.clone(), PropertyMap::from_iter([(key, Value::from(1))]))?;
	seed.configure_changefeed(
		ChangefeedSource::Class(class),
		ChangefeedConfig::from_nanoseconds(86_400_000_000_000)?,
	)?;
	seed.commit()?;
	let query = LiveQuery::new(LiveSource::Class(class)).with_graph_result(LiveGraphResult::new(
		LiveExpr::field("id")?,
		LiveExpr::Literal(Value::None),
	));
	let mut subscription = store
		.subscribe_live_graph::<Subgraph<Frozen>>(query, LiveOptions::default())?
		.into_mutable()?;
	let saved = subscription.cursor().clone();
	let atom = AtomRef::new(AtomKind::Node, record.clone());
	subscription.materialization()?.mutate_local([], |graph| {
		let id = graph
			.atom_id(&atom)?
			.ok_or_else(|| FerrosGrynnError::invalid_argument("local atom absent"))?;
		graph.node_properties_mut(id)?.insert(key, 99);
		Ok(())
	})?;
	let old_stamp = store.inner.current_root().commit_ts;
	let failure = CommitFailureGuard::set(CommitFailurePoint::BeforeAppend);
	assert!(subscription.flush(store.begin()).is_err());
	drop(failure);
	assert_eq!(store.inner.current_root().commit_ts, old_stamp);
	assert_eq!(subscription.cursor(), &saved);
	subscription.materialization()?.with_graph(|graph| {
		let id = graph
			.atom_id(&atom)?
			.ok_or_else(|| FerrosGrynnError::invalid_argument("local atom absent"))?;
		assert_eq!(
			graph
				.node(id)?
				.ok_or_else(|| FerrosGrynnError::invalid_argument("local node absent"))?
				.properties()
				.get(key),
			Some(&Value::from(99))
		);
		Ok::<(), FerrosGrynnError>(())
	})??;
	drop(subscription);
	drop(store);
	let store = Datastore::open_local(&directory.0, options.clone())?;
	let mut subscription = store.resume_live_graph::<Subgraph<Frozen>>(&saved)?.into_mutable()?;
	subscription.materialization()?.mutate_local([], |graph| {
		let id = graph
			.atom_id(&atom)?
			.ok_or_else(|| FerrosGrynnError::invalid_argument("local atom absent"))?;
		graph.node_properties_mut(id)?.insert(key, 99);
		Ok(())
	})?;
	let committed = subscription.flush(store.begin())?;
	assert!(
		matches!(subscription.next()?, LiveGraphNext::Updated { revision } if revision == committed.versionstamp())
	);
	let saved = subscription.cursor().clone();
	drop(subscription);
	store.checkpoint(CheckpointMode::Restart)?;
	drop(store);
	let store = Datastore::open_local(&directory.0, options)?;
	assert_eq!(
		store
			.begin()
			.atom(&record)?
			.ok_or_else(|| FerrosGrynnError::invalid_argument("source absent"))?
			.properties()
			.get(key),
		Some(&Value::from(99))
	);
	let mut subscription = store.resume_live_graph::<Subgraph<Frozen>>(&saved)?;
	assert_eq!(subscription.revision(), Some(committed.versionstamp()));
	assert!(matches!(subscription.next()?, LiveGraphNext::Idle));
	subscription.close()?;
	Ok(())
}

#[test]
fn live_graph_flush_local_sync_commit_failure_and_reopen() -> FerrosGrynnResult<()> {
	graph_sdk_flush_local(IoMode::Sync)
}

#[cfg(feature = "native-async-io")]
#[test]
fn live_graph_flush_local_native_commit_failure_and_reopen() -> FerrosGrynnResult<()> {
	graph_sdk_flush_local(IoMode::NativeAsync)
}

/// The SDK restores immutable computed values across checkpoint and generation
/// replacement.
fn resume_survives_reopen(mode: IoMode) -> FerrosGrynnResult<()> {
	use crate::live::LiveNext;
	let directory = TestDirectory::new();
	let mut options = LocalOptions::default();
	options.io_mode = mode;
	options.buffer_pool_pages = 16;
	let (store, id, _aggregate, _binding, record, key, _feed) =
		fixture(&directory.0, options.clone())?;
	let saved = store.begin().live_info(id)?.cursor().clone();
	let mut update = store.begin();
	update.set_base_property(&record, key, 2.into())?;
	let first = update.commit()?.versionstamp();
	store.checkpoint(CheckpointMode::Restart)?;
	let mut generation = store.begin();
	generation.define_class(ClassSchema::new("other", AtomKind::Node, [])?)?;
	generation.set_base_property(&record, key, 3.into())?;
	let second = generation.commit()?.versionstamp();
	drop(store);
	let store = Datastore::open_local(&directory.0, options.clone())?;
	let mut resumed = store.resume_live(&saved)?;
	let mut tail = store.begin();
	tail.set_base_property(&record, key, 4.into())?;
	let third = tail.commit()?.versionstamp();
	for (ordinal, stamp) in [first, second, third].into_iter().enumerate() {
		let LiveNext::Delivery(delivery) = resumed.next()? else {
			return Err(FerrosGrynnError::invalid_argument("missing durable SDK history"));
		};
		let LivePayload::Batch(batch) = delivery.payload() else {
			return Err(FerrosGrynnError::invalid_argument("unexpected durable SDK GAP"));
		};
		assert_eq!(batch.versionstamp(), stamp);
		assert_eq!(batch.events()[0].before(), Some(&Value::from(1_i64 + ordinal as i64)));
		assert_eq!(batch.events()[0].after(), Some(&Value::from(2_i64 + ordinal as i64)));
		resumed.ack(delivery.token())?;
	}
	assert!(matches!(resumed.next()?, LiveNext::Idle));
	let confirmed = resumed.cursor().clone();
	drop(resumed);
	drop(store);
	let store = Datastore::open_local(&directory.0, options)?;
	let mut resumed = store.resume_live(&saved)?;
	assert_eq!(resumed.cursor(), &confirmed);
	assert!(matches!(resumed.next()?, LiveNext::Idle));
	Ok(())
}

#[test]
fn live_resume_local_sync_history_and_tail() -> FerrosGrynnResult<()> {
	resume_survives_reopen(IoMode::Sync)
}

/// New-incarnation history has several original clocks and no registration
/// event; its SDK reader survives local RAM clearing and then checkpoint/COW.
fn backfill_survives_reopen(mode: IoMode) -> FerrosGrynnResult<()> {
	use crate::live::{LiveNext, LiveSince};
	let directory = TestDirectory::new();
	let mut options = LocalOptions::default();
	options.io_mode = mode;
	options.buffer_pool_pages = 16;
	let (store, old, _, _, record, key, _) = fixture(&directory.0, options.clone())?;
	let original = store.begin().live_info(old)?;
	let floor = original.cursor().versionstamp();
	let mut stamps = Vec::new();
	for value in [2, 3] {
		let mut update = store.begin();
		update.set_base_property(&record, key, value.into())?;
		stamps.push(update.commit()?.versionstamp());
	}
	let mut subscription = store.subscribe_live(
		original.query().clone(),
		LiveOptions::default().with_since(LiveSince::Versionstamp(floor)),
	)?;
	let saved = subscription.cursor().clone();
	let id = subscription.id();
	assert_eq!(saved.versionstamp(), floor);
	assert!(store.inner.current_root().live_history().is_empty());
	for (ordinal, stamp) in stamps.iter().enumerate() {
		let LiveNext::Delivery(delivery) = subscription.next()? else {
			return Err(FerrosGrynnError::invalid_argument("local SDK backfill missing"));
		};
		let LivePayload::Batch(batch) = delivery.payload() else {
			return Err(FerrosGrynnError::invalid_argument("local SDK backfill GAP"));
		};
		assert_eq!(batch.versionstamp(), *stamp);
		assert_eq!(batch.events()[0].after(), Some(&Value::from(ordinal as i64 + 2)));
		// Let the final token remain unconfirmed for actual reopened replay.
		if ordinal == 0 {
			subscription.ack(delivery.token())?;
		}
	}
	drop(subscription);
	store.checkpoint(CheckpointMode::Restart)?;
	let mut generation = store.begin();
	generation.define_class(ClassSchema::new("generation", AtomKind::Node, [])?)?;
	generation.commit()?;
	drop(store);
	let store = Datastore::open_local(&directory.0, options)?;
	let mut resumed = store.resume_live(&saved)?;
	assert_eq!(resumed.id(), id);
	assert_eq!(resumed.cursor().versionstamp(), stamps[0]);
	let mut tail = store.begin();
	tail.set_base_property(&record, key, 4.into())?;
	let tail_stamp = tail.commit()?.versionstamp();
	for (ordinal, stamp) in [stamps[1], tail_stamp].into_iter().enumerate() {
		let LiveNext::Delivery(delivery) = resumed.next()? else {
			return Err(FerrosGrynnError::invalid_argument("reopened backfill or tail missing"));
		};
		let LivePayload::Batch(batch) = delivery.payload() else {
			return Err(FerrosGrynnError::invalid_argument("reopened backfill GAP"));
		};
		assert_eq!(batch.versionstamp(), stamp);
		assert_eq!(batch.events()[0].before(), Some(&Value::from(ordinal as i64 + 2)));
		assert_eq!(batch.events()[0].after(), Some(&Value::from(ordinal as i64 + 3)));
		resumed.ack(delivery.token())?;
	}
	assert!(matches!(resumed.next()?, LiveNext::Idle));
	Ok(())
}

#[test]
fn live_backfill_local_sync_registration_reopen_checkpoint_and_generation() -> FerrosGrynnResult<()>
{
	backfill_survives_reopen(IoMode::Sync)
}

#[cfg(feature = "native-async-io")]
#[test]
fn live_backfill_local_native_async_registration_reopen_checkpoint_and_generation()
-> FerrosGrynnResult<()> {
	backfill_survives_reopen(IoMode::NativeAsync)
}

/// Historical aggregate images keep their separate result identity and exact
/// source clocks across physical staging, checkpoint and reopened ACK.
fn aggregate_backfill_survives_reopen(mode: IoMode) -> FerrosGrynnResult<()> {
	use crate::live::{LiveIdentity, LiveNext, LiveSince};
	let directory = TestDirectory::new();
	let mut options = LocalOptions::default();
	options.io_mode = mode;
	options.buffer_pool_pages = 16;
	let (store, old, _, _, record, key, _) = fixture(&directory.0, options.clone())?;
	let original = store.begin().live_info(old)?;
	let query = LiveQuery::new(original.query().source().clone()).with_projection(
		LiveProjection::Value(LiveExpr::Call {
			name: "sum".into(),
			arguments: vec![LiveExpr::Field(crate::live::LivePath::new(vec![
				"properties".into(),
				"value".into(),
			])?)],
		}),
	);
	let floor = original.cursor().versionstamp();
	let mut stamps = Vec::new();
	for value in [2, 3] {
		let mut update = store.begin();
		update.set_base_property(&record, key, value.into())?;
		stamps.push(update.commit()?.versionstamp());
	}
	let subscription = store
		.subscribe_live(query, LiveOptions::default().with_since(LiveSince::Versionstamp(floor)))?;
	let saved = subscription.cursor().clone();
	drop(subscription);
	store.checkpoint(CheckpointMode::Restart)?;
	let mut generation = store.begin();
	generation.define_class(ClassSchema::new("generation", AtomKind::Node, [])?)?;
	generation.commit()?;
	drop(store);
	let store = Datastore::open_local(&directory.0, options.clone())?;
	let mut resumed = store.resume_live(&saved)?;
	for (ordinal, stamp) in stamps.into_iter().enumerate() {
		let LiveNext::Delivery(delivery) = resumed.next()? else {
			return Err(FerrosGrynnError::invalid_argument("local aggregate history missing"));
		};
		let LivePayload::Batch(batch) = delivery.payload() else {
			return Err(FerrosGrynnError::invalid_argument("local aggregate history GAP"));
		};
		assert_eq!(batch.versionstamp(), stamp);
		assert!(
			matches!(batch.events()[0].identity(), LiveIdentity::Aggregate(keys) if keys.is_empty())
		);
		assert_eq!(batch.events()[0].before(), Some(&Value::from(ordinal as i64 + 1)));
		assert_eq!(batch.events()[0].after(), Some(&Value::from(ordinal as i64 + 2)));
		resumed.ack(delivery.token())?;
	}
	assert!(matches!(resumed.next()?, LiveNext::Idle));
	let confirmed = resumed.cursor().clone();
	drop(resumed);
	drop(store);
	let store = Datastore::open_local(&directory.0, options)?;
	let mut resumed = store.resume_live(&saved)?;
	assert_eq!(resumed.cursor(), &confirmed);
	assert!(matches!(resumed.next()?, LiveNext::Idle));
	Ok(())
}

#[test]
fn live_backfill_aggregate_local_sync_reopen() -> FerrosGrynnResult<()> {
	aggregate_backfill_survives_reopen(IoMode::Sync)
}

/// NativeAsync exercises the real transport separately from sync mode.
#[test]
#[cfg(all(windows, feature = "native-async-io"))]
fn live_backfill_aggregate_local_native_async_reopen() -> FerrosGrynnResult<()> {
	aggregate_backfill_survives_reopen(IoMode::NativeAsync)
}

/// Linked-only mutations are captured as owned results, then read after
/// checkpoint/reopen without fetching current linked records.
fn fetch_backfill_survives_reopen(mode: IoMode) -> FerrosGrynnResult<()> {
	use crate::live::{LiveField, LiveIdentity, LiveNext, LivePath, LiveSince};
	let directory = TestDirectory::new();
	let mut options = LocalOptions::default();
	options.io_mode = mode;
	options.buffer_pool_pages = 16;
	let (store, _, _, _, record, key, _) = fixture(&directory.0, options.clone())?;
	let linked: RecordId = "item:b".parse()?;
	let mut link = store.begin();
	let manager = link.intern_property("manager")?;
	link.create_atom(linked.clone(), PropertyMap::from_iter([(key, 2.into())]))?;
	link.set_base_property(&record, manager, Value::from(linked.clone()))?;
	let floor = link.commit()?.versionstamp();
	let oracle = || -> FerrosGrynnResult<Value> {
		let result = store
			.execute("SELECT properties.manager AS boss FROM item:a FETCH boss;", &Object::new())?
			.into_iter()
			.next()
			.ok_or_else(|| FerrosGrynnError::invalid_argument("local FETCH oracle missing"))?
			.result?;
		let Value::Array(rows) = result.value() else {
			return Err(FerrosGrynnError::invalid_argument("local FETCH oracle is not rows"));
		};
		assert_eq!(rows.len(), 1);
		rows.iter()
			.next()
			.cloned()
			.ok_or_else(|| FerrosGrynnError::invalid_argument("local FETCH row missing"))
	};
	let mut expected = Vec::new();
	for value in [3, 4] {
		let old = oracle()?;
		let mut update = store.begin();
		update.set_base_property(&linked, key, value.into())?;
		let stamp = update.commit()?.versionstamp();
		expected.push((stamp, old, oracle()?));
	}
	let query = LiveQuery::new(LiveSource::Record(record.clone()))
		.with_projection(LiveProjection::Fields(vec![
			LiveField::new(LiveExpr::Field(LivePath::new(vec![
				"properties".into(),
				"manager".into(),
			])?))
			.with_alias(LivePath::field("boss")?),
		]))
		.with_fetch(vec![LivePath::field("boss")?]);
	let subscription = store
		.subscribe_live(query, LiveOptions::default().with_since(LiveSince::Versionstamp(floor)))?;
	let saved = subscription.cursor().clone();
	drop(subscription);
	store.checkpoint(CheckpointMode::Restart)?;
	drop(store);
	let store = Datastore::open_local(&directory.0, options)?;
	let mut resumed = store.resume_live(&saved)?;
	for (stamp, old, new) in expected {
		let LiveNext::Delivery(delivery) = resumed.next()? else {
			return Err(FerrosGrynnError::invalid_argument("local FETCH history missing"));
		};
		let LivePayload::Batch(batch) = delivery.payload() else {
			return Err(FerrosGrynnError::invalid_argument("local FETCH history GAP"));
		};
		assert_eq!(batch.versionstamp(), stamp);
		assert_eq!(batch.events().len(), 1);
		assert_eq!(batch.events()[0].identity(), &LiveIdentity::Record(record.clone()));
		assert_eq!(batch.events()[0].before(), Some(&old));
		assert_eq!(batch.events()[0].after(), Some(&new));
		resumed.ack(delivery.token())?;
	}
	assert!(matches!(resumed.next()?, LiveNext::Idle));
	Ok(())
}

#[test]
fn live_backfill_fetch_local_sync_reopen() -> FerrosGrynnResult<()> {
	fetch_backfill_survives_reopen(IoMode::Sync)
}

/// NativeAsync has a separate real-transport gate.
#[test]
#[cfg(all(windows, feature = "native-async-io"))]
fn live_backfill_fetch_local_native_async_reopen() -> FerrosGrynnResult<()> {
	fetch_backfill_survives_reopen(IoMode::NativeAsync)
}

/// Coverage failure survives publication/reopen even while source capture is
/// OFF.
#[test]
fn live_backfill_local_disabled_source_keeps_unproven_dependency_gap() -> FerrosGrynnResult<()> {
	use crate::live::{LiveNext, LiveSince};
	let directory = TestDirectory::new();
	let options = LocalOptions::default();
	let (store, old, _, _, record, key, feed) = fixture(&directory.0, options.clone())?;
	let info = store.begin().live_info(old)?;
	let floor = info.cursor().versionstamp();
	let mut update = store.begin();
	update.set_base_property(&record, key, 2.into())?;
	let lost = update.commit()?.versionstamp();
	let source = store.begin().changefeed_info(feed)?.source();
	let mut stop = store.begin();
	stop.drop_changefeed(source)?;
	stop.commit()?;
	let query = info
		.query()
		.clone()
		.with_fetch(vec![crate::live::LivePath::new(vec!["properties".into(), "manager".into()])?]);
	let subscription = store
		.subscribe_live(query, LiveOptions::default().with_since(LiveSince::Versionstamp(floor)))?;
	let saved = subscription.cursor().clone();
	drop(subscription);
	drop(store);
	let store = Datastore::open_local(&directory.0, options)?;
	let mut resumed = store.resume_live(&saved)?;
	let LiveNext::Delivery(delivery) = resumed.next()? else {
		return Err(FerrosGrynnError::invalid_argument("reopened coverage GAP missing"));
	};
	assert!(matches!(delivery.payload(), LivePayload::Gap(gap)
		if gap.reason() == crate::live::LiveGapReason::InsufficientCoverage
		&& gap.from().is_some_and(|from| from <= lost)
		&& gap.through().is_some_and(|through| through >= lost)));
	Ok(())
}

/// A catalog missing its final reference cannot silently report idle.
#[test]
fn live_resume_local_missing_reference_is_corruption() -> FerrosGrynnResult<()> {
	use crate::live::LiveNext;
	let directory = TestDirectory::new();
	let (store, id, _aggregate, _binding, record, key, _feed) =
		fixture(&directory.0, LocalOptions::default())?;
	let saved = store.begin().live_info(id)?.cursor().clone();
	let mut update = store.begin();
	update.set_base_property(&record, key, 2.into())?;
	update.commit()?;
	let mut root = (*store.inner.current_root()).clone();
	assert!(root.live_history().is_empty());
	Arc::make_mut(root.live_segments_mut()).remove(&id);
	*store.inner.current.write() = Arc::new(root);
	let mut resumed = store.resume_live(&saved)?;
	assert!(resumed.next().is_err());
	assert!(matches!(resumed.next()?, LiveNext::Closed));
	assert_eq!(resumed.cursor(), &saved);
	Ok(())
}

#[cfg(feature = "native-async-io")]
#[test]
fn live_resume_local_native_async_history_and_tail() -> FerrosGrynnResult<()> {
	resume_survives_reopen(IoMode::NativeAsync)
}

/// An uncertain ACK outcome never releases issued in-process delivery early.
#[test]
fn live_subscription_ack_wal_failure_preserves_delivery_and_recovers_watermark()
-> crate::gql_status::FerrosGrynnResult<()> {
	use crate::{
		live::{LiveNext, LiveOptions, LiveQuery, LiveSource},
		storage::wal::{CommitFailureGuard, CommitFailurePoint},
	};
	for point in [
		CommitFailurePoint::BeforeAppend,
		CommitFailurePoint::AfterAppend,
		CommitFailurePoint::AfterFlush,
		CommitFailurePoint::AfterSync,
	] {
		let directory = TestDirectory::new();
		let store = Datastore::create_local(&directory.0, LocalOptions::default())?;
		let mut setup = store.begin();
		let class = setup.define_class(ClassSchema::new("item", AtomKind::Node, [])?)?;
		setup.commit()?;
		let mut subscription = store
			.subscribe_live(LiveQuery::new(LiveSource::Class(class)), LiveOptions::default())?;
		let registered = subscription.cursor().clone();
		let mut update = store.begin();
		update.create_atom("item:a".parse()?, PropertyMap::new())?;
		update.commit()?;
		let LiveNext::Delivery(delivery) = subscription.next()? else {
			return Err(FerrosGrynnError::invalid_argument("missing ACK fixture delivery"));
		};
		let guard = CommitFailureGuard::set(point);
		let statistics = store.live_statistics()?;
		assert!(subscription.ack(delivery.token()).is_err());
		drop(guard);
		assert_eq!(subscription.cursor(), &registered);
		let LiveNext::Delivery(repeated) = subscription.next()? else {
			return Err(FerrosGrynnError::invalid_argument("failed ACK released delivery"));
		};
		assert_eq!(repeated.token(), delivery.token());
		let after = store.live_statistics()?;
		assert_eq!(
			after.acknowledgements, statistics.acknowledgements,
			"failed or uncertain ACK is not observed as successful"
		);
		assert_eq!(
			after.deliveries_issued, statistics.deliveries_issued,
			"repeat after failed ACK is not new issuance"
		);
		let id = subscription.id();
		drop(subscription);
		drop(store);
		let recovered = Datastore::open_local(&directory.0, LocalOptions::default())?;
		assert_eq!(
			recovered.live_statistics()?.acknowledgements,
			0,
			"process counters reset on reopen"
		);
		let expected = if point == CommitFailurePoint::BeforeAppend {
			&registered
		} else {
			delivery.token()
		};
		assert_eq!(recovered.begin().live_info(id)?.cursor(), expected);
	}
	Ok(())
}

use super::{GenerationFailureGuard, GenerationFailurePoint, TestDirectory};
use crate::{
	changefeed::{ChangefeedConfig, ChangefeedReadItem, ChangefeedReadOptions, ChangefeedSource},
	datastore::{CheckpointMode, ClassSchema, Datastore, IoMode, LocalOptions, StoreBackend},
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	graph::AtomKind,
	live::{
		LiveBindingOptions,
		LiveExpr,
		LiveId,
		LiveOptions,
		LivePayload,
		LiveProjection,
		LiveQuery,
		LiveSource,
	},
	storage::{HistorySegment, LocalChangefeedSnapshot, LocalStore},
	values::{Object, PropertyKeyId, PropertyMap, RecordId, Value},
};

/// Builds a source feed and two result shapes without an initial delivery.
fn fixture(
	path: &std::path::Path,
	options: LocalOptions,
) -> FerrosGrynnResult<(
	Datastore,
	LiveId,
	LiveId,
	crate::live::LiveBindingId,
	RecordId,
	PropertyKeyId,
	crate::changefeed::ChangefeedId,
)> {
	let store = Datastore::create_local(path, options)?;
	let mut setup = store.begin();
	let class = setup.define_class(ClassSchema::new("item", AtomKind::Node, [])?)?;
	let key = setup.intern_property("value")?;
	let record: RecordId = "item:a".parse()?;
	setup.create_atom(record.clone(), PropertyMap::from_iter([(key, Value::from(1))]))?;
	let feed = setup.configure_changefeed(
		ChangefeedSource::Class(class),
		ChangefeedConfig::from_nanoseconds(86_400_000_000_000)?,
	)?;
	let binding = setup.define_live_binding(
		LiveExpr::Literal(1.into()),
		Object::new(),
		LiveBindingOptions::default(),
	)?;
	let id = setup.register_live(
		LiveQuery::new(LiveSource::Class(class)).with_projection(LiveProjection::Value(
			LiveExpr::Field(crate::live::LivePath::new(vec!["properties".into(), "value".into()])?),
		)),
		LiveOptions::default(),
	)?;
	let aggregate = setup.register_live(
		LiveQuery::new(LiveSource::Class(class)).with_projection(LiveProjection::Value(
			LiveExpr::Call {
				name: "sum".into(),
				arguments: vec![
					LiveExpr::Field(crate::live::LivePath::new(vec![
						"properties".into(),
						"value".into(),
					])?)
					.binary(crate::live::LiveBinaryOperator::Add, LiveExpr::Binding(binding)),
				],
			},
		)),
		LiveOptions::default(),
	)?;
	setup.commit()?;
	assert!(store.inner.current_root().live_segments().is_empty());
	Ok((store, id, aggregate, binding, record, key, feed))
}

/// Resolves the actual transport and retains files/WAL independently of a root.
fn transport(store: &Datastore) -> FerrosGrynnResult<(&LocalStore, LocalChangefeedSnapshot)> {
	let StoreBackend::Local(local) = &store.inner.backend else {
		return Err(FerrosGrynnError::invalid_argument("local fixture required"));
	};
	let snapshot = local
		.capture_changefeed_snapshot(store.inner.current_root().commit_ts)?
		.ok_or_else(|| FerrosGrynnError::invalid_argument("missing pinned history snapshot"))?;
	Ok((local, snapshot))
}

/// Returns complete groups and verifies physical ceilings against stored
/// counts.
fn groups(store: &Datastore, id: LiveId) -> FerrosGrynnResult<Vec<Vec<u8>>> {
	let root = store.inner.current_root();
	let (local, snapshot) = transport(store)?;
	assert!(root.live_history().is_empty(), "local root does not retain RAM history");
	let mut output = Vec::new();
	if let Some(segments) = root.live_segments().get(&id) {
		for segment in segments.iter().copied() {
			let mut first = 1;
			let before = output.len();
			while first < segment.page_ceiling() {
				let (payload, info, next) = local
					.read_live_payload(&snapshot, segment, first, 32 * 1024 * 1024)?
					.ok_or_else(|| {
						FerrosGrynnError::invalid_argument("computed segment unavailable")
					})?;
				assert_eq!(info.id, id);
				output.push(payload_bytes(&payload, info)?);
				first = next;
			}
			assert_eq!(first, segment.page_ceiling());
			assert_eq!((output.len() - before) as u64, segment.commit_count());
		}
	}
	Ok(output)
}

/// Compares canonical result images including exact value variants and faults.
fn payload_bytes(
	payload: &LivePayload,
	info: crate::storage::live_history::LiveGroupInfo,
) -> FerrosGrynnResult<Vec<u8>> {
	crate::storage::live_history::encode(
		payload,
		info.id,
		info.version,
		info.stamp,
		info.time,
		32 * 1024 * 1024,
	)
}

/// Exercises DML, parameter-only catalog publication, rotation, generation
/// replacement, checkpoint and reopen under one selected production transport.
fn lifecycle(mode: IoMode) -> FerrosGrynnResult<()> {
	let directory = TestDirectory::new();
	let options = LocalOptions {
		io_mode: mode,
		..LocalOptions::default()
	};
	let rotation =
		super::super::changefeed::SegmentTargetGuard::set(3 * crate::storage::PAGE_SIZE as u64);
	let (store, id, aggregate, binding, record, key, feed) =
		fixture(&directory.0, options.clone())?;
	let mut update = store.begin();
	update.set_base_property(&record, key, Value::from(2))?;
	let first_stamp = update.commit()?.versionstamp();
	let old_root = store.inner.current_root();
	let old_segment: HistorySegment = *old_root
		.live_segments()
		.get(&id)
		.and_then(|segments| segments.first())
		.ok_or_else(|| FerrosGrynnError::invalid_argument("missing first LIVE segment"))?;
	let (local, pin) = transport(&store)?;
	let expected = groups(&store, id)?;
	let mut parameter = store.begin();
	parameter.replace_live_binding_value(binding, 4.into())?;
	parameter.commit()?;
	assert_eq!(groups(&store, id)?.len(), 1, "parameter does not affect an unrelated query");
	assert_eq!(groups(&store, aggregate)?.len(), 2, "aggregate parameter-only history is durable");
	let mut update = store.begin();
	update.set_base_property(&record, key, Value::from("x".repeat(33_000)))?;
	update.commit()?;
	assert_eq!(groups(&store, id)?.len(), 2);
	assert_eq!(store.inner.current_root().live_segments()[&id].len(), 2);
	assert_eq!(old_root.live_segments()[&id].len(), 1);
	let (owned, info, next) = local
		.read_live_payload(&pin, old_segment, 1, 32 * 1024 * 1024)?
		.ok_or_else(|| FerrosGrynnError::invalid_argument("old pinned group lost"))?;
	assert_eq!(payload_bytes(&owned, info)?, expected[0]);
	assert_eq!(info.stamp, first_stamp.get());
	assert_eq!(next, old_segment.page_ceiling());
	let mut schema = store.begin();
	schema.define_class(ClassSchema::new("later", AtomKind::Node, [])?)?;
	schema.commit()?;
	store.import(crate::datastore::ImportOptions::default(), |session| {
		session.apply_item(|tx| tx.create_atom("later:x".parse()?, PropertyMap::new()))?;
		Ok(())
	})?;
	let expected = groups(&store, id)?;
	let expected_aggregate = groups(&store, aggregate)?;
	let metadata = store.inner.current_root().live_catalog().histories[&id].clone();
	let mut reader = store.begin();
	let mut source = reader.changes(feed, ChangefeedReadOptions::default())?;
	while let Some(item) = source.next_batch()? {
		assert!(matches!(item, ChangefeedReadItem::Commit(_)));
	}
	drop(source);
	drop(reader);
	drop(pin);
	drop(old_root);
	store.checkpoint(CheckpointMode::Restart)?;
	drop(store);
	drop(rotation);
	let reopened = Datastore::open_local(&directory.0, options)?;
	assert_eq!(groups(&reopened, id)?, expected);
	assert_eq!(groups(&reopened, aggregate)?, expected_aggregate);
	assert_eq!(reopened.inner.current_root().live_catalog().histories[&id], metadata);
	Ok(())
}

#[test]
fn live_history_local_sync_publication_lifecycle() -> FerrosGrynnResult<()> {
	lifecycle(IoMode::Sync)
}

#[cfg(all(windows, feature = "native-async-io"))]
#[test]
fn live_history_local_native_async_publication_lifecycle() -> FerrosGrynnResult<()> {
	lifecycle(IoMode::NativeAsync)
}

/// Final import publication retains a concurrent close, computed results and
/// pinned earlier generations through the existing physical barrier.
fn import_lifecycle(mode: IoMode) -> FerrosGrynnResult<()> {
	let directory = TestDirectory::new();
	let mut options = LocalOptions::default();
	options.io_mode = mode;
	options.buffer_pool_pages = 16;
	let (store, stopped, aggregate, _binding, record, key, feed) =
		fixture(&directory.0, options.clone())?;
	let initial_stamp = store.inner.current_root().commit_ts;
	let mut first = store.begin();
	first.set_base_property(&record, key, 2.into())?;
	first.commit()?;
	let old = store.inner.current_root();
	let old_segment = old.live_segments()[&aggregate][0];
	let (local, pin) = transport(&store)?;
	let expected_old = groups(&store, aggregate)?;
	let mut close_stamp = 0;
	store.import(
		crate::datastore::ImportOptions {
			changefeed: crate::datastore::ImportChangefeedMode::On,
			segment_size: 1,
			..crate::datastore::ImportOptions::default()
		},
		|session| {
			session.apply_item(|tx| tx.set_base_property(&record, key, 5.into()))?;
			let mut close = store.begin();
			close.close_live(stopped)?;
			close_stamp = close.commit()?.commit_ts();
			session.apply_item(|tx| tx.set_base_property(&record, key, 7.into()))?;
			Ok(())
		},
	)?;
	let root = store.inner.current_root();
	assert_eq!(root.commit_ts, close_stamp + 1);
	assert_eq!(root.live_catalog().queries[&stopped].state, crate::live::LiveState::Closed);
	assert_eq!(root.live_catalog().queries[&stopped].published_at, close_stamp);
	assert_eq!(old.live_catalog().queries[&stopped].state, crate::live::LiveState::Active);
	assert_eq!(groups(&store, stopped)?.len(), 1, "closed subscription is not reactivated by END");
	let expected = groups(&store, aggregate)?;
	assert_eq!(expected.len(), 2);
	let LivePayload::Batch(computed) =
		crate::storage::live_history::decode(&expected[1], 32 * 1024 * 1024)?
	else {
		return Err(FerrosGrynnError::invalid_argument("missing final import batch"));
	};
	assert_eq!(computed.versionstamp().get(), root.commit_ts);
	assert_eq!(computed.events()[0].before(), Some(&Value::from(3)));
	assert_eq!(computed.events()[0].after(), Some(&Value::from(8)));
	let (payload, info, _next) =
		local
			.read_live_payload(&pin, old_segment, 1, 32 * 1024 * 1024)?
			.ok_or_else(|| FerrosGrynnError::invalid_argument("old import pin lost its group"))?;
	assert_eq!(payload_bytes(&payload, info)?, expected_old[0]);
	drop(pin);
	let mut reader = store.begin();
	let mut changes = reader.changes(feed, ChangefeedReadOptions::default())?;
	let mut stamps = Vec::new();
	while let Some(item) = changes.next_batch()? {
		if let ChangefeedReadItem::Commit(batch) = item {
			stamps.push(batch.versionstamp().get());
			if batch.versionstamp().get() == root.commit_ts {
				assert_eq!(batch.committed_at(), computed.committed_at());
			}
		}
	}
	assert_eq!(
		stamps,
		[initial_stamp, old.commit_ts, root.commit_ts],
		"source creation, update and END are the only source commits"
	);
	drop(changes);
	drop(reader);
	store.checkpoint(CheckpointMode::Restart)?;
	drop(root);
	drop(old);
	drop(store);
	let store = Datastore::open_local(&directory.0, options)?;
	assert_eq!(store.begin().live_info(stopped)?.state(), crate::live::LiveState::Closed);
	assert_eq!(groups(&store, aggregate)?, expected);
	drop(store);
	Ok(())
}

/// Local sync END uses the same history/generation protocol as ordinary commit.
#[test]
fn live_import_local_sync_lifecycle() -> FerrosGrynnResult<()> {
	import_lifecycle(IoMode::Sync)
}

/// NativeAsync is an independently selected actual local backend target.
#[test]
#[cfg(all(windows, feature = "native-async-io"))]
fn live_import_local_native_async_lifecycle() -> FerrosGrynnResult<()> {
	import_lifecycle(IoMode::NativeAsync)
}

/// A failed END generation keeps the independently committed close and
/// discards all accepted private primary/source/result data.
#[test]
fn live_import_local_failed_generation_keeps_close() -> FerrosGrynnResult<()> {
	for point in [
		GenerationFailurePoint::AfterBuild,
		GenerationFailurePoint::AfterFileSync,
		GenerationFailurePoint::AfterCatalogStage,
		GenerationFailurePoint::BeforeWalCommit,
	] {
		let directory = TestDirectory::new();
		let (store, stopped, aggregate, _, record, key, _) =
			fixture(&directory.0, LocalOptions::default())?;
		let mut guard = None;
		let mut closed = 0;
		let result = store.import(
			crate::datastore::ImportOptions {
				changefeed: crate::datastore::ImportChangefeedMode::On,
				segment_size: 1,
				..crate::datastore::ImportOptions::default()
			},
			|session| {
				session.apply_item(|tx| tx.set_base_property(&record, key, 7.into()))?;
				let mut close = store.begin();
				close.close_live(stopped)?;
				closed = close.commit()?.commit_ts();
				guard = Some(GenerationFailureGuard::set(point));
				Ok(())
			},
		);
		assert!(result.is_err(), "END must encounter the generation failure");
		drop(guard);
		assert_eq!(store.inner.current_root().commit_ts, closed);
		assert!(groups(&store, aggregate)?.is_empty());
		drop(store);
		let reopened = Datastore::open_local(&directory.0, LocalOptions::default())?;
		assert_eq!(reopened.inner.current_root().commit_ts, closed);
		assert_eq!(reopened.begin().live_info(stopped)?.state(), crate::live::LiveState::Closed);
		assert_eq!(
			reopened.begin().atom(&record)?.and_then(|atom| atom.properties().get(key).cloned()),
			Some(Value::from(1))
		);
		assert!(groups(&reopened, aggregate)?.is_empty());
	}
	Ok(())
}

/// END's uncertain WAL outcome recovers the fresh final stamp and all three
/// histories together, while the prior close is durable in either outcome.
#[test]
fn live_import_local_wal_barrier_recovers_final_stamp() -> FerrosGrynnResult<()> {
	use crate::storage::wal::{CommitFailureGuard, CommitFailurePoint};
	for point in [
		CommitFailurePoint::BeforeAppend,
		CommitFailurePoint::AfterAppend,
		CommitFailurePoint::AfterFlush,
		CommitFailurePoint::AfterSync,
	] {
		let directory = TestDirectory::new();
		let (store, stopped, aggregate, _, record, key, feed) =
			fixture(&directory.0, LocalOptions::default())?;
		let mut guard = None;
		let mut closed = 0;
		let result = store.import(
			crate::datastore::ImportOptions {
				changefeed: crate::datastore::ImportChangefeedMode::On,
				segment_size: 1,
				..crate::datastore::ImportOptions::default()
			},
			|session| {
				session.apply_item(|tx| tx.set_base_property(&record, key, 7.into()))?;
				let mut close = store.begin();
				close.close_live(stopped)?;
				closed = close.commit()?.commit_ts();
				guard = Some(CommitFailureGuard::set(point));
				Ok(())
			},
		);
		assert!(result.is_err(), "END must encounter the WAL failure");
		drop(guard);
		assert_eq!(store.inner.current_root().commit_ts, closed);
		drop(store);
		let reopened = Datastore::open_local(&directory.0, LocalOptions::default())?;
		let appended = !matches!(point, CommitFailurePoint::BeforeAppend);
		let final_stamp = closed + u64::from(appended);
		assert_eq!(reopened.inner.current_root().commit_ts, final_stamp);
		assert_eq!(reopened.begin().live_info(stopped)?.state(), crate::live::LiveState::Closed);
		assert!(groups(&reopened, stopped)?.is_empty());
		let expected = usize::from(appended);
		let results = groups(&reopened, aggregate)?;
		assert_eq!(results.len(), expected);
		if let Some(bytes) = results.first() {
			let LivePayload::Batch(batch) =
				crate::storage::live_history::decode(bytes, 8 * 1024 * 1024)?
			else {
				return Err(FerrosGrynnError::invalid_argument("missing recovered END batch"));
			};
			assert_eq!(batch.versionstamp().get(), final_stamp);
			assert_eq!(batch.events()[0].after(), Some(&Value::from(8)));
		}
		assert_eq!(
			reopened.begin().atom(&record)?.and_then(|atom| atom.properties().get(key).cloned()),
			Some(Value::from(if appended {
				7
			} else {
				1
			}))
		);
		let mut reader = reopened.begin();
		let mut source = reader.changes(feed, ChangefeedReadOptions::default())?;
		let mut count = 0;
		while let Some(item) = source.next_batch()? {
			assert!(matches!(item, ChangefeedReadItem::Commit(_)));
			count += 1;
		}
		assert_eq!(count, 1 + expected);
	}
	Ok(())
}

/// Failed primary publication leaves the old result catalog and images intact.
#[test]
fn live_history_local_failed_generation_is_atomic() -> FerrosGrynnResult<()> {
	for point in [
		GenerationFailurePoint::AfterBuild,
		GenerationFailurePoint::AfterFileSync,
		GenerationFailurePoint::AfterCatalogStage,
		GenerationFailurePoint::BeforeWalCommit,
	] {
		let directory = TestDirectory::new();
		let (store, id, _, _, record, key, _) = fixture(&directory.0, LocalOptions::default())?;
		let mut update = store.begin();
		update.set_base_property(&record, key, 2.into())?;
		update.commit()?;
		let expected = groups(&store, id)?;
		let stamp = store.inner.current_root().commit_ts;
		let guard = GenerationFailureGuard::set(point);
		let mut update = store.begin();
		update.set_base_property(&record, key, 3.into())?;
		assert!(update.commit().is_err());
		drop(guard);
		assert_eq!(store.inner.current_root().commit_ts, stamp);
		assert_eq!(groups(&store, id)?, expected);
		drop(store);
		let reopened = Datastore::open_local(&directory.0, LocalOptions::default())?;
		assert_eq!(groups(&reopened, id)?, expected);
	}
	Ok(())
}

/// A completed WAL append makes primary, source and computed results jointly
/// recoverable even when the caller observes an uncertain commit outcome.
#[test]
fn live_history_local_wal_barrier_recovers_computed_groups() -> FerrosGrynnResult<()> {
	use crate::storage::wal::{CommitFailureGuard, CommitFailurePoint};
	for point in [
		CommitFailurePoint::BeforeAppend,
		CommitFailurePoint::AfterAppend,
		CommitFailurePoint::AfterFlush,
		CommitFailurePoint::AfterSync,
	] {
		let directory = TestDirectory::new();
		let (store, id, aggregate, _, record, key, feed) =
			fixture(&directory.0, LocalOptions::default())?;
		let mut update = store.begin();
		update.set_base_property(&record, key, 2.into())?;
		update.commit()?;
		let old_stamp = store.inner.current_root().commit_ts;
		let statistics = store.live_statistics()?;
		let guard = CommitFailureGuard::set(point);
		let mut update = store.begin();
		update.set_base_property(&record, key, 3.into())?;
		assert!(update.commit().is_err());
		drop(guard);
		let failed = store.live_statistics()?;
		assert_eq!(failed.published_batches, statistics.published_batches);
		assert_eq!(failed.durable_history_groups, statistics.durable_history_groups);
		assert!(failed.query_evaluation_attempts > statistics.query_evaluation_attempts);
		drop(store);
		let reopened = Datastore::open_local(&directory.0, LocalOptions::default())?;
		assert_eq!(reopened.live_statistics()?.published_batches, 0);
		let appended = !matches!(point, CommitFailurePoint::BeforeAppend);
		assert_eq!(reopened.inner.current_root().commit_ts, old_stamp + u64::from(appended));
		let expected = 1 + usize::from(appended);
		assert_eq!(groups(&reopened, id)?.len(), expected);
		assert_eq!(groups(&reopened, aggregate)?.len(), expected);
		let mut reader = reopened.begin();
		let mut source = reader.changes(feed, ChangefeedReadOptions::default())?;
		let mut actual = 0;
		while let Some(item) = source.next_batch()? {
			assert!(matches!(item, ChangefeedReadItem::Commit(_)));
			actual += 1;
		}
		assert_eq!(actual, expected + 1, "source capture includes its setup commit");
	}
	Ok(())
}

/// A new identity and multiple backfilled clocks have one recovery outcome;
/// uncertainty cannot leave a published descriptor with partial history.
#[test]
fn live_backfill_local_wal_barrier_recovers_whole_registration() -> FerrosGrynnResult<()> {
	use crate::{
		live::{LiveNext, LiveSince},
		storage::wal::{CommitFailureGuard, CommitFailurePoint},
	};
	for point in [
		CommitFailurePoint::BeforeAppend,
		CommitFailurePoint::AfterAppend,
		CommitFailurePoint::AfterFlush,
		CommitFailurePoint::AfterSync,
	] {
		let directory = TestDirectory::new();
		let (store, old, _, _, record, key, _) = fixture(&directory.0, LocalOptions::default())?;
		let original = store.begin().live_info(old)?;
		let floor = original.cursor().versionstamp();
		let mut stamps = Vec::new();
		for value in [2, 3] {
			let mut update = store.begin();
			update.set_base_property(&record, key, value.into())?;
			stamps.push(update.commit()?.versionstamp());
		}
		let cutoff = store.inner.current_root().commit_ts;
		let mut registration = store.begin();
		let id = registration.register_live(
			original.query().clone(),
			LiveOptions::default().with_since(LiveSince::Versionstamp(floor)),
		)?;
		let guard = CommitFailureGuard::set(point);
		assert!(registration.commit().is_err());
		drop(guard);
		assert_eq!(store.inner.current_root().commit_ts, cutoff);
		assert!(store.begin().live_info(id).is_err());
		drop(store);
		let store = Datastore::open_local(&directory.0, LocalOptions::default())?;
		let appended = !matches!(point, CommitFailurePoint::BeforeAppend);
		assert_eq!(store.inner.current_root().commit_ts, cutoff + u64::from(appended));
		if !appended {
			assert!(store.begin().live_info(id).is_err());
			assert!(!store.inner.current_root().live_segments().contains_key(&id));
			continue;
		}
		let saved = store.begin().live_info(id)?.cursor().clone();
		assert_eq!(saved.versionstamp(), floor);
		let mut resumed = store.resume_live(&saved)?;
		for (ordinal, stamp) in stamps.into_iter().enumerate() {
			let LiveNext::Delivery(delivery) = resumed.next()? else {
				return Err(FerrosGrynnError::invalid_argument(
					"recovered registration history missing",
				));
			};
			let LivePayload::Batch(batch) = delivery.payload() else {
				return Err(FerrosGrynnError::invalid_argument(
					"recovered registration history GAP",
				));
			};
			assert_eq!(batch.versionstamp(), stamp);
			assert_eq!(batch.events()[0].after(), Some(&Value::from(ordinal as i64 + 2)));
			resumed.ack(delivery.token())?;
		}
		assert!(matches!(resumed.next()?, LiveNext::Idle));
	}
	Ok(())
}
