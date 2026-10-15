#![cfg(test)]

//! Public reactive registry persistence, run separately for each local backend.

#![cfg(windows)]
/// Assertions shared with the root unit-test harness.
#[path = "common/assertions.rs"]
pub mod test_assertions;

use ferros_grynn::{
	datastore::{CheckpointMode, ClassField, ClassSchema, Datastore, IoMode, LocalOptions},
	gql_status::FerrosGrynnResult,
	graph::AtomKind,
	live::{
		LiveBinaryOperator,
		LiveBindingOptions,
		LiveExpr,
		LiveOptions,
		LiveProjection,
		LiveQuery,
		LiveSource,
		LiveState,
	},
	values::{Object, PropertyMap, RecordId, Type, Uuid, Value},
};

/// Qualified duplicate SPLIT rows replay saved images after current data
/// empties.
fn split_results_survive_reopen(mode: IoMode) -> FerrosGrynnResult<()> {
	use ferros_grynn::{
		changefeed::{ChangefeedConfig, ChangefeedSource},
		gql_status::FerrosGrynnError,
		graph::GraphKind,
		live::{LiveIdentity, LiveIdentityPart, LiveNext, LivePath, LivePayload},
	};

	use crate::test_assertions::{AssertComparison as _, AssertCondition as _};
	let directory = std::env::temp_dir().join(format!(
		"fg-live-split-{:016x}",
		Uuid::now_v7().as_inner().as_u128() & u128::from(u64::MAX)
	));
	let mut options = LocalOptions::default();
	options.io_mode = mode;
	options.buffer_pool_pages = 16;
	let store = Datastore::create_local(&directory, options)?;
	let mut setup = store.begin();
	setup.define_class(ClassSchema::new("item", AtomKind::Node, [ClassField::new(
		"tags",
		Type::Array(Box::new(Type::Int)),
	)])?)?;
	setup.commit()?;
	let tags = store
		.property_dictionary()
		.get("tags")
		.ok_or_else(|| FerrosGrynnError::invalid_argument("SPLIT property missing"))?;
	let mut setup = store.begin();
	let record: RecordId = "item:a".parse()?;
	setup.create_atom(
		record.clone(),
		PropertyMap::from_iter([(tags, Value::from(vec![Value::from(7), Value::from(7)]))]),
	)?;
	let graph = setup.define_graph("split_scope", GraphKind::Graph)?;
	setup.attach_node(graph.id(), &record, None, None)?;
	setup.configure_changefeed(
		ChangefeedSource::Graph(graph.id()),
		ChangefeedConfig::new("P1D".parse()?)?,
	)?;
	setup.commit()?;
	let query = LiveQuery::new(LiveSource::GraphRecord {
		graph: graph.clone(),
		record: record.clone(),
	})
	.with_alias("p")?
	.with_split(vec![LiveExpr::Field(LivePath::new(vec!["p".into(), "tags".into()])?)])
	.with_projection(LiveProjection::Value(LiveExpr::Field(LivePath::new(vec![
		"p".into(),
		"tags".into(),
	])?)));
	let mut handle = store.subscribe_live(query.clone(), LiveOptions::default())?;
	let cursor = handle.cursor().clone();
	let mut update = store.begin();
	update.set_overlay(
		graph.id(),
		&record,
		tags,
		Value::from(vec![Value::from(8), Value::from(7), Value::from(7)]),
	)?;
	let stamp = update.commit()?.versionstamp();
	let LiveNext::Delivery(delivery) = handle.next()? else {
		return Err(FerrosGrynnError::invalid_argument("SPLIT delivery missing"));
	};
	let LivePayload::Batch(batch) = delivery.payload() else {
		return Err(FerrosGrynnError::invalid_argument("SPLIT delivery GAP"));
	};
	(&(batch.events().len()), &(2)).assert_equal();
	for event in batch.events() {
		let LiveIdentity::Composite(parts) = event.identity() else {
			return Err(FerrosGrynnError::invalid_argument("SPLIT qualified identity missing"));
		};
		(matches!(parts.as_slice(), [LiveIdentityPart::Occurrence(occurrence), LiveIdentityPart::Split(_)] if occurrence.graph().id() == graph.id() && occurrence.atom().id() == &record)).assert_true();
	}
	let expected = batch.events().to_vec();
	let token = delivery.token().clone();
	let mut update = store.begin();
	update.set_overlay(graph.id(), &record, tags, Value::from(Vec::<Value>::new()))?;
	let tail_stamp = update.commit()?.versionstamp();
	drop(handle);
	store.checkpoint(CheckpointMode::Restart)?;
	drop(store);
	let store = Datastore::open_local(&directory, options)?;
	(&(store.begin().live_info(cursor.live_id())?.query()), &(&query)).assert_equal();
	let mut handle = store.resume_live(&cursor)?;
	let LiveNext::Delivery(replay) = handle.next()? else {
		return Err(FerrosGrynnError::invalid_argument("SPLIT saved replay missing"));
	};
	(&(replay.token()), &(&token)).assert_equal();
	(matches!(replay.payload(), LivePayload::Batch(batch) if batch.versionstamp() == stamp && batch.events() == expected)).assert_true();
	handle.ack(replay.token())?;
	drop(handle);
	drop(store);
	let store = Datastore::open_local(&directory, options)?;
	(&(store.begin().live_info(cursor.live_id())?.cursor()), &(&token)).assert_equal();
	let mut handle = store.resume_live(&token)?;
	let LiveNext::Delivery(tail) = handle.next()? else {
		return Err(FerrosGrynnError::invalid_argument("SPLIT saved tail missing"));
	};
	(matches!(tail.payload(), LivePayload::Batch(batch) if batch.versionstamp() == tail_stamp && batch.events().len() == 3 && batch.events().iter().all(|event| event.before().is_some() && event.after().is_none()))).assert_true();
	handle.ack(tail.token())?;
	(matches!(handle.next()?, LiveNext::Idle)).assert_true();
	handle.close()?;
	drop(handle);
	drop(store);
	std::fs::remove_dir_all(&directory).map_err(|error| {
		FerrosGrynnError::invalid_argument("SPLIT fixture cleanup failed")
			.with_technical_source(error)
	})?;
	Ok(())
}

/// Verifies live window split local sync.
#[test]
fn live_window_split_local_sync() -> FerrosGrynnResult<()> {
	split_results_survive_reopen(IoMode::Sync)
}

/// Verifies live window split local native async.
#[cfg(feature = "native-async-io")]
#[test]
fn live_window_split_local_native_async() -> FerrosGrynnResult<()> {
	split_results_survive_reopen(IoMode::NativeAsync)
}

/// Seeds the changefeed-backed class and records used by the window replay
/// scenario.
///
/// # Errors
/// Propagates the existing schema, property lookup and transaction failures in
/// setup order.
fn seed_window_fixture(
	store: &Datastore,
) -> FerrosGrynnResult<(ferros_grynn::datastore::ClassId, ferros_grynn::values::PropertyKeyId)> {
	use ferros_grynn::{
		changefeed::{ChangefeedConfig, ChangefeedSource},
		gql_status::FerrosGrynnError,
	};
	let mut tx = store.begin();
	let class = tx.define_class(ClassSchema::new("item", AtomKind::Node, [ClassField::new(
		"age",
		Type::Int,
	)])?)?;
	tx.configure_changefeed(
		ChangefeedSource::Class(class),
		ChangefeedConfig::new("P1D".parse()?)?,
	)?;
	tx.commit()?;
	let age = store
		.property_dictionary()
		.get("age")
		.ok_or_else(|| FerrosGrynnError::invalid_argument("window property missing"))?;
	let mut tx = store.begin();
	for (id, score) in [("a", 1), ("b", 2), ("c", 3)] {
		tx.create_atom(
			format!("item:{id}").parse()?,
			PropertyMap::from_iter([(age, score.into())]),
		)?;
	}
	tx.commit()?;
	Ok((class, age))
}

/// Owned comparison/window descriptors and exact membership images survive
/// reopen.
fn window_results_survive_reopen(
	mode: IoMode,
	output: ferros_grynn::live::LiveOutputMode,
) -> FerrosGrynnResult<()> {
	use std::collections::BTreeMap;

	use ferros_grynn::{
		gql_status::FerrosGrynnError,
		live::{
			LiveIdentity,
			LiveNext,
			LiveNullsOrder,
			LiveOrder,
			LiveOrderDirection,
			LivePayload,
		},
	};

	use crate::test_assertions::{AssertComparison as _, AssertCondition as _};
	let directory = std::env::temp_dir().join(format!(
		"fg-live-window-{:016x}",
		Uuid::now_v7().as_inner().as_u128() & u128::from(u64::MAX)
	));
	let mut options = LocalOptions::default();
	options.io_mode = mode;
	options.buffer_pool_pages = 16;
	let store = Datastore::create_local(&directory, options)?;
	let (class, age) = seed_window_fixture(&store)?;
	let query = LiveQuery::new(LiveSource::Class(class))
		.with_output(output)
		.with_only(true)
		.with_order(vec![
			LiveOrder::new(LiveExpr::Cast {
				target: "string".into(),
				value: Box::new(LiveExpr::field("age")?),
			})
			.with_direction(LiveOrderDirection::Descending)
			.with_nulls(LiveNullsOrder::First)
			.with_collation(true)
			.with_numeric(true),
		])
		.with_parameters(Object::from_iter([("limit", Value::from(1_i64))]))
		.with_limit(LiveExpr::Parameter("limit".into()))
		.with_start_offset(LiveExpr::Literal(0.into()));
	let mut handle = store.subscribe_live(query.clone(), LiveOptions::default())?;
	let id = handle.id();
	let cursor = handle.cursor().clone();
	let b: RecordId = "item:b".parse()?;
	let mut tx = store.begin();
	tx.set_base_property(&b, age, 4.into())?;
	let stamp = tx.commit()?.versionstamp();
	let LiveNext::Delivery(delivery) = handle.next()? else {
		return Err(FerrosGrynnError::invalid_argument("window delivery missing"));
	};
	let LivePayload::Batch(batch) = delivery.payload() else {
		return Err(FerrosGrynnError::invalid_argument("window delivery fault"));
	};
	(&(batch.events().len()), &(2)).assert_equal();
	let expected_output = batch
		.events()
		.iter()
		.map(|event| (event.identity().clone(), event.output().cloned()))
		.collect::<BTreeMap<_, _>>();
	(
		&(expected_output.values().all(Option::is_some)),
		&(output != ferros_grynn::live::LiveOutputMode::Full),
	)
		.assert_equal();
	let expected = batch
		.events()
		.iter()
		.map(|event| (event.identity().clone(), (event.before().cloned(), event.after().cloned())))
		.collect::<BTreeMap<_, _>>();
	(expected[&LiveIdentity::Record(b.clone())].0.is_none()).assert_true();
	let mut tx = store.begin();
	tx.set_base_property(&b, age, 5.into())?;
	tx.commit()?;
	drop(handle);
	store.checkpoint(CheckpointMode::Restart)?;
	drop(store);
	let store = Datastore::open_local(&directory, options)?;
	(&(store.begin().live_info(id)?.query()), &(&query)).assert_equal();
	(&(store.live_statistics()?.published_batches), &(0))
		.assert_equal_with(|| "process counters reset on open".to_string());
	let mut handle = store.resume_live(&cursor)?;
	let LiveNext::Delivery(delivery) = handle.next()? else {
		return Err(FerrosGrynnError::invalid_argument("window replay missing"));
	};
	let LivePayload::Batch(batch) = delivery.payload() else {
		return Err(FerrosGrynnError::invalid_argument("window replay fault"));
	};
	let observed = store.live_statistics()?;
	(observed.history_page_reads > 0).assert_true();
	(observed.admitted_replay_work > 0).assert_true();
	let LiveNext::Delivery(repeated) = handle.next()? else {
		return Err(FerrosGrynnError::invalid_argument("window repeated replay missing"));
	};
	(&(repeated.token()), &(delivery.token())).assert_equal();
	(&(store.live_statistics()?.history_page_reads), &(observed.history_page_reads)).assert_equal();
	(&(store.live_statistics()?.deliveries_issued), &(observed.deliveries_issued)).assert_equal();
	(&(batch.versionstamp()), &(stamp)).assert_equal();
	for event in batch.events() {
		(&(event.before().cloned(), event.after().cloned()), &(expected[event.identity()]))
			.assert_equal();
		(&(event.output()), &(expected_output[event.identity()].as_ref())).assert_equal();
	}
	handle.ack(delivery.token())?;
	handle.close()?;
	drop(handle);
	drop(store);
	std::fs::remove_dir_all(&directory).map_err(|error| {
		FerrosGrynnError::invalid_argument("window fixture cleanup failed")
			.with_technical_source(error)
	})?;
	Ok(())
}

/// Verifies live window local sync.
#[test]
fn live_window_local_sync() -> FerrosGrynnResult<()> {
	window_results_survive_reopen(IoMode::Sync, ferros_grynn::live::LiveOutputMode::Full)
}

/// Verifies live window local native async.
#[cfg(feature = "native-async-io")]
#[test]
fn live_window_local_native_async() -> FerrosGrynnResult<()> {
	window_results_survive_reopen(IoMode::NativeAsync, ferros_grynn::live::LiveOutputMode::Full)
}

/// Verifies live patch window local sync.
#[test]
fn live_patch_window_local_sync() -> FerrosGrynnResult<()> {
	window_results_survive_reopen(IoMode::Sync, ferros_grynn::live::LiveOutputMode::Patch)
}

/// Verifies live patch window local native async.
#[cfg(feature = "native-async-io")]
#[test]
fn live_patch_window_local_native_async() -> FerrosGrynnResult<()> {
	window_results_survive_reopen(IoMode::NativeAsync, ferros_grynn::live::LiveOutputMode::Patch)
}

/// Grouped multi-class results resume from saved exact keys and owned images.
fn grouped_results_survive_reopen(mode: IoMode, constructors: bool) -> FerrosGrynnResult<()> {
	use ferros_grynn::{
		changefeed::{ChangefeedConfig, ChangefeedSource},
		gql_status::FerrosGrynnError,
		graph::GraphKind,
		live::{LiveIdentity, LiveNext, LivePayload},
	};

	use crate::test_assertions::{AssertComparison as _, AssertCondition as _};
	let directory = std::env::temp_dir().join(format!(
		"fg-live-group-{:016x}",
		Uuid::now_v7().as_inner().as_u128() & u128::from(u64::MAX)
	));
	let mut options = LocalOptions::default();
	options.io_mode = mode;
	options.buffer_pool_pages = 16;
	let store = Datastore::create_local(&directory, options)?;
	let mut tx = store.begin();
	let graph = tx.define_graph("scope", GraphKind::Graph)?;
	tx.configure_changefeed(
		ChangefeedSource::Graph(graph.id()),
		ChangefeedConfig::new("P1D".parse()?)?,
	)?;
	let wide = 9_007_199_254_740_992_i64;
	for class in ["left", "right"] {
		tx.define_class(ClassSchema::new(class, AtomKind::Node, [ClassField::new(
			"k",
			Type::Int,
		)])?)?;
	}
	tx.commit()?;
	let key = store
		.property_dictionary()
		.get("k")
		.ok_or_else(|| FerrosGrynnError::invalid_argument("group fixture property key missing"))?;
	let mut tx = store.begin();
	for class in ["left", "right"] {
		let record = format!("{class}:a").parse()?;
		tx.create_atom(record, PropertyMap::from_iter([(key, Value::from(wide))]))?;
		tx.attach_node(graph.id(), &format!("{class}:a").parse()?, None, None)?;
	}
	tx.commit()?;
	let count = || LiveExpr::Call {
		name: "count".into(),
		arguments: Vec::new(),
	};
	let expression = if constructors {
		LiveExpr::Object(vec![
			(
				"values".into(),
				LiveExpr::Array(vec![
					count(),
					LiveExpr::Cast {
						target: "int".into(),
						value: Box::new(count()),
					},
					LiveExpr::Index {
						base: Box::new(LiveExpr::Array(vec![count()])),
						index: Box::new(LiveExpr::Literal(0_i64.into())),
					},
					LiveExpr::Access {
						base: Box::new(LiveExpr::Object(vec![("n".into(), count())])),
						field: "n".into(),
						null_safe: false,
					},
				]),
			),
			("set".into(), LiveExpr::Set(vec![count(), count()])),
			("branch".into(), LiveExpr::Conditional {
				condition: Box::new(
					count().binary(LiveBinaryOperator::Greater, LiveExpr::Literal(0_i64.into())),
				),
				then_value: Box::new(count()),
				else_value: Box::new(
					count().binary(LiveBinaryOperator::Divide, LiveExpr::Literal(0_i64.into())),
				),
			}),
			("case".into(), LiveExpr::Case {
				operand: Some(Box::new(count())),
				branches: vec![(LiveExpr::Literal(1_i64.into()), count())],
				else_value: Some(Box::new(count())),
			}),
			("range".into(), LiveExpr::Range {
				start: Some(Box::new(LiveExpr::Literal(0_i64.into()))),
				end: Some(Box::new(count())),
				inclusive_end: true,
			}),
		])
	} else {
		count()
	};
	let image = |count: i64| -> FerrosGrynnResult<Value> {
		if !constructors {
			return Ok(count.into());
		}
		Ok(Value::Object(Object::from_iter([
			(
				"values",
				Value::Array(
					[count.into(), count.into(), count.into(), count.into()].into_iter().collect(),
				),
			),
			("set", Value::Set([count.into()].into_iter().collect())),
			("branch", count.into()),
			("case", count.into()),
			("range", Value::Range(ferros_grynn::values::Range::closed(0_i64, count)?)),
		])))
	};
	let query = LiveQuery::new(LiveSource::Graph(graph))
		.with_group_by(vec![LiveExpr::field("k")?])?
		.with_projection(LiveProjection::Value(expression));
	let handle = store.subscribe_live(query.clone(), LiveOptions::default())?;
	let id = handle.id();
	let saved = handle.cursor().clone();
	let record: RecordId = "right:a".parse()?;
	let mut tx = store.begin();
	tx.set_base_property(&record, key, (wide + 1).into())?;
	tx.commit()?;
	drop(handle);
	store.checkpoint(CheckpointMode::Restart)?;
	drop(store);
	let store = Datastore::open_local(&directory, options)?;
	(&(store.begin().live_info(id)?.query()), &(&query)).assert_equal();
	let mut handle = store.resume_live(&saved)?;
	let LiveNext::Delivery(delivery) = handle.next()? else {
		return Err(FerrosGrynnError::invalid_argument("group replay missing"));
	};
	let LivePayload::Batch(batch) = delivery.payload() else {
		return Err(FerrosGrynnError::invalid_argument("group replay gap"));
	};
	(&(batch.events().len()), &(2)).assert_equal();
	(&(batch.events()[0].identity()), &(&LiveIdentity::Aggregate(vec![Value::from(wide)])))
		.assert_equal();
	(&(batch.events()[0].before()), &(Some(&image(2)?))).assert_equal();
	(&(batch.events()[0].after()), &(Some(&image(1)?))).assert_equal();
	(&(batch.events()[1].identity()), &(&LiveIdentity::Aggregate(vec![Value::from(wide + 1)])))
		.assert_equal();
	(&(batch.events()[1].before()), &(None)).assert_equal();
	(&(batch.events()[1].after()), &(Some(&image(1)?))).assert_equal();
	handle.ack(delivery.token())?;
	let saved = handle.cursor().clone();
	let mut tx = store.begin();
	tx.delete_atom_cascade(&record, true).map_err(|error| {
		FerrosGrynnError::invalid_argument("group fixture delete preparation failed")
			.with_cause(error)
	})?;
	tx.commit().map_err(|error| {
		FerrosGrynnError::invalid_argument("group fixture delete publication failed")
			.with_cause(error)
	})?;
	drop(handle);
	drop(store);
	let store = Datastore::open_local(&directory, options)?;
	let mut handle = store.resume_live(&saved)?;
	let LiveNext::Delivery(delivery) = handle.next()? else {
		return Err(FerrosGrynnError::invalid_argument("group tail missing"));
	};
	(matches!(delivery.payload(), LivePayload::Batch(batch) if batch.events().len() == 1
		&& batch.events()[0].identity() == &LiveIdentity::Aggregate(vec![Value::from(wide + 1)])
		&& batch.events()[0].before() == Some(&image(1)?) && batch.events()[0].after().is_none()))
	.assert_true();
	handle.ack(delivery.token())?;
	handle.close()?;
	drop(handle);
	drop(store);
	std::fs::remove_dir_all(&directory).map_err(|error| {
		FerrosGrynnError::invalid_argument("group fixture cleanup failed")
			.with_technical_source(error)
	})?;
	Ok(())
}

/// Verifies live group results local sync.
#[test]
fn live_group_results_local_sync() -> FerrosGrynnResult<()> {
	grouped_results_survive_reopen(IoMode::Sync, false)
}

/// Verifies live group results local native async.
#[cfg(feature = "native-async-io")]
#[test]
fn live_group_results_local_native_async() -> FerrosGrynnResult<()> {
	grouped_results_survive_reopen(IoMode::NativeAsync, false)
}

/// Verifies live aggregate constructors local sync.
#[test]
fn live_aggregate_constructors_local_sync() -> FerrosGrynnResult<()> {
	grouped_results_survive_reopen(IoMode::Sync, true)
}

/// Verifies live aggregate constructors local native async.
#[cfg(feature = "native-async-io")]
#[test]
fn live_aggregate_constructors_local_native_async() -> FerrosGrynnResult<()> {
	grouped_results_survive_reopen(IoMode::NativeAsync, true)
}

/// Owned OMIT recipes and captured payloads resume independently of primary
/// rows.
fn omit_results_survive_reopen(mode: IoMode) -> FerrosGrynnResult<()> {
	use ferros_grynn::{
		changefeed::{ChangefeedConfig, ChangefeedSource},
		gql_status::FerrosGrynnError,
		live::{LiveIdentity, LiveNext, LivePath, LivePayload},
	};

	use crate::test_assertions::{AssertComparison as _, AssertCondition as _};
	let directory = std::env::temp_dir().join(format!(
		"fg-live-omit-{:016x}",
		Uuid::now_v7().as_inner().as_u128() & u128::from(u64::MAX)
	));
	let mut options = LocalOptions::default();
	options.io_mode = mode;
	options.buffer_pool_pages = 16;
	let store = Datastore::create_local(&directory, options)?;
	let mut tx = store.begin();
	let class = tx.define_class(ClassSchema::new("item", AtomKind::Node, [
		ClassField::new("k", Type::Int),
		ClassField::new("secret", Type::Int),
	])?)?;
	let record: RecordId = "item:a".parse()?;
	let wide = 9_007_199_254_740_993_i64;
	tx.configure_changefeed(
		ChangefeedSource::Class(class),
		ChangefeedConfig::new("P1D".parse()?)?,
	)?;
	tx.commit()?;
	let dictionary = store.property_dictionary();
	let key = dictionary
		.get("k")
		.ok_or_else(|| FerrosGrynnError::invalid_argument("OMIT fixture k missing"))?;
	let secret = dictionary
		.get("secret")
		.ok_or_else(|| FerrosGrynnError::invalid_argument("OMIT fixture secret missing"))?;
	let mut tx = store.begin();
	tx.create_atom(
		record.clone(),
		PropertyMap::from_iter([(key, wide.into()), (secret, 1.into())]),
	)?;
	tx.commit()?;
	let query = LiveQuery::new(LiveSource::Record(record.clone()))
		.with_omit(vec![LivePath::field("id")?, LivePath::field("secret")?]);
	let mut handle = store.subscribe_live(query.clone(), LiveOptions::default())?;
	let id = handle.id();
	let saved = handle.cursor().clone();
	let mut tx = store.begin();
	tx.set_base_property(&record, secret, 2.into())?;
	tx.commit()?;
	(matches!(handle.next()?, LiveNext::Idle)).assert_true();
	let mut tx = store.begin();
	tx.set_base_property(&record, key, (wide + 1).into())?;
	tx.commit()?;
	drop(handle);
	store.checkpoint(CheckpointMode::Restart)?;
	drop(store);
	let store = Datastore::open_local(&directory, options)?;
	(&(store.begin().live_info(id)?.query()), &(&query)).assert_equal();
	let mut handle = store.resume_live(&saved)?;
	let LiveNext::Delivery(delivery) = handle.next()? else {
		return Err(FerrosGrynnError::invalid_argument("OMIT replay missing"));
	};
	let LivePayload::Batch(batch) = delivery.payload() else {
		return Err(FerrosGrynnError::invalid_argument("OMIT replay gap"));
	};
	(&(batch.events().len()), &(1)).assert_equal();
	let event = &batch.events()[0];
	(&(event.identity()), &(&LiveIdentity::Record(record.clone()))).assert_equal();
	for (image, expected) in [(event.before(), wide), (event.after(), wide + 1)] {
		let object = image
			.and_then(Value::as_object)
			.ok_or_else(|| FerrosGrynnError::invalid_argument("OMIT replay image missing"))?;
		(&(object.get("k")), &(Some(&expected.into()))).assert_equal();
		(object.get("id").is_none() && object.get("secret").is_none()).assert_true();
	}
	let before_exit = event.after().cloned();
	handle.ack(delivery.token())?;
	let saved = handle.cursor().clone();
	let mut tx = store.begin();
	tx.delete_atom(&record)?;
	tx.commit()?;
	drop(handle);
	drop(store);
	let store = Datastore::open_local(&directory, options)?;
	let mut handle = store.resume_live(&saved)?;
	let LiveNext::Delivery(delivery) = handle.next()? else {
		return Err(FerrosGrynnError::invalid_argument("OMIT exit replay missing"));
	};
	(matches!(delivery.payload(), LivePayload::Batch(batch) if batch.events().len() == 1
		&& batch.events()[0].identity() == &LiveIdentity::Record(record)
		&& batch.events()[0].before() == before_exit.as_ref() && batch.events()[0].after().is_none()))
	.assert_true();
	handle.ack(delivery.token())?;
	handle.close()?;
	drop(handle);
	drop(store);
	std::fs::remove_dir_all(&directory).map_err(|error| {
		FerrosGrynnError::invalid_argument("OMIT fixture cleanup failed")
			.with_technical_source(error)
	})?;
	Ok(())
}

/// Verifies live omit local sync.
#[test]
fn live_omit_local_sync() -> FerrosGrynnResult<()> {
	omit_results_survive_reopen(IoMode::Sync)
}

/// Verifies live omit local native async.
#[cfg(feature = "native-async-io")]
#[test]
fn live_omit_local_native_async() -> FerrosGrynnResult<()> {
	omit_results_survive_reopen(IoMode::NativeAsync)
}

/// Text registration and frozen captures survive checkpoint and durable replay.
fn gql_registration_survives_reopen(mode: IoMode) -> FerrosGrynnResult<()> {
	use ferros_grynn::{
		changefeed::{ChangefeedConfig, ChangefeedSource},
		gql_status::FerrosGrynnError,
		live::{LiveId, LiveNext, LivePayload},
	};

	use crate::test_assertions::AssertComparison as _;
	let directory = std::env::temp_dir().join(format!(
		"fg-live-text-{:016x}",
		Uuid::now_v7().as_inner().as_u128() & u128::from(u64::MAX)
	));
	let mut options = LocalOptions::default();
	options.io_mode = mode;
	options.buffer_pool_pages = 16;
	let store = Datastore::create_local(&directory, options)?;
	let mut session = store.session();
	for result in session.execute(
		"DEFINE CLASS person TYPE NODE; DEFINE FIELD age ON CLASS person TYPE int; DEFINE PURE \
		 FUNCTION fn::capacity() -> int { RETURN 7; };",
		&Object::new(),
	)? {
		result.result?;
	}
	let mut capture = store.begin();
	let class = capture
		.class("person")?
		.ok_or_else(|| FerrosGrynnError::invalid_argument("text fixture CLASS is absent"))?
		.id()
		.ok_or_else(|| {
			FerrosGrynnError::invalid_argument("text fixture CLASS identity is absent")
		})?;
	capture.configure_changefeed(
		ChangefeedSource::Class(class),
		ChangefeedConfig::new("PT1H".parse()?)?,
	)?;
	capture.commit()?;
	let mut results = session.execute(
		"LET $minimum = 18; LIVE SELECT VALUE count() FROM person WHERE age >= $minimum BUFFER \
		 fn::capacity();",
		&Object::new(),
	)?;
	let result = results.pop().ok_or_else(|| {
		FerrosGrynnError::invalid_argument("text fixture registration result is absent")
	})?;
	for result in results {
		result.result?;
	}
	let outcome = result.result?;
	let Value::Uuid(uuid) = outcome.value() else {
		return Err(FerrosGrynnError::invalid_argument("text fixture LIVE UUID is absent"));
	};
	let id = LiveId::from_uuid(*uuid)?;
	let handle = session.take_live_subscription(id)?;
	let saved = handle.cursor().clone();
	for result in session
		.execute("ALTER LET $minimum VALUE 21; CREATE person:a SET age = 19;", &Object::new())?
	{
		result.result?;
	}
	drop(handle);
	drop(session);
	store.checkpoint(CheckpointMode::Restart)?;
	drop(store);
	let store = Datastore::open_local(&directory, options)?;
	let mut handle = store.resume_live(&saved)?;
	let LiveNext::Delivery(delivery) = handle.next()? else {
		return Err(FerrosGrynnError::invalid_argument("text fixture replay is absent"));
	};
	let LivePayload::Batch(batch) = delivery.payload() else {
		return Err(FerrosGrynnError::invalid_argument("text fixture unexpected GAP"));
	};
	(&(batch.events().len()), &(1)).assert_equal();
	(&(batch.events()[0].before()), &(Some(&0.into()))).assert_equal();
	(&(batch.events()[0].after()), &(Some(&1.into()))).assert_equal();
	handle.ack(delivery.token())?;
	drop(handle);
	drop(store);
	let store = Datastore::open_local(&directory, options)?;
	(&(store.begin().live_info(id)?.cursor()), &(delivery.token())).assert_equal();
	let mut session = store.session();
	let inputs = Object::from_iter([("sub", Value::from(id.as_uuid()))]);
	let mut info = session.execute("INFO FOR LIVE $sub;", &inputs)?;
	let row = info
		.pop()
		.ok_or_else(|| FerrosGrynnError::invalid_argument("text fixture INFO missing"))?
		.result?;
	let Value::Object(row) = row.value() else {
		return Err(FerrosGrynnError::invalid_argument("text fixture INFO row missing"));
	};
	(&(row.get("cursor")), &(Some(&Value::from(delivery.token().encode())))).assert_equal();
	for result in session.execute("KILL $sub;", &inputs)? {
		result.result?;
	}
	drop(session);
	store.checkpoint(CheckpointMode::Restart)?;
	drop(store);
	let store = Datastore::open_local(&directory, options)?;
	(&(store.begin().live_info(id)?.state()), &(LiveState::Closed)).assert_equal();
	drop(store);
	std::fs::remove_dir_all(&directory).map_err(|error| {
		FerrosGrynnError::invalid_argument("text fixture cleanup failed")
			.with_technical_source(error)
	})?;
	Ok(())
}

/// Verifies live gql registration local sync.
#[test]
fn live_gql_registration_local_sync() -> FerrosGrynnResult<()> {
	gql_registration_survives_reopen(IoMode::Sync)
}

/// Verifies live gql registration local native async.
#[cfg(feature = "native-async-io")]
#[test]
fn live_gql_registration_local_native_async() -> FerrosGrynnResult<()> {
	gql_registration_survives_reopen(IoMode::NativeAsync)
}

/// A retained text consumer confirms via GQL and survives ordinary WAL/rebuild.
fn gql_ack_survives_reopen(mode: IoMode) -> FerrosGrynnResult<()> {
	use ferros_grynn::{
		gql_status::FerrosGrynnError,
		live::{LiveId, LiveNext},
	};

	use crate::test_assertions::{AssertComparison as _, AssertCondition as _};
	let directory = std::env::temp_dir().join(format!(
		"fg-live-gack-{:016x}",
		Uuid::now_v7().as_inner().as_u128() & u128::from(u64::MAX)
	));
	let mut options = LocalOptions::default();
	options.io_mode = mode;
	options.buffer_pool_pages = 16;
	let store = Datastore::create_local(&directory, options)?;
	let mut session = store.session();
	for result in
		session.execute("DEFINE CLASS person TYPE NODE CHANGEFEED 1d FULL;", &Object::new())?
	{
		result.result?;
	}
	let result = session
		.execute("LIVE SELECT VALUE count() FROM person;", &Object::new())?
		.into_iter()
		.next()
		.ok_or_else(|| FerrosGrynnError::invalid_argument("ACK registration missing"))?
		.result?;
	let Value::Uuid(uuid) = result.value() else {
		return Err(FerrosGrynnError::invalid_argument("ACK UUID missing"));
	};
	let id = LiveId::from_uuid(*uuid)?;
	let before = store.begin().live_info(id)?.cursor().clone();
	for result in session.execute("CREATE person:a;", &Object::new())? {
		result.result?;
	}
	let LiveNext::Delivery(delivery) = session.next_live(id)? else {
		return Err(FerrosGrynnError::invalid_argument("ACK delivery missing"));
	};
	let inputs = Object::from_iter([
		("qid", Value::from(id.as_uuid())),
		("token", Value::from(delivery.token().encode())),
	]);
	for result in session.execute("BEGIN; ACK LIVE $qid CURSOR $token; CANCEL;", &inputs)? {
		result.result?;
	}
	(&(store.begin().live_info(id)?.cursor()), &(&before)).assert_equal();
	(matches!(session.next_live(id)?, LiveNext::Delivery(ref next) if next.token() == delivery.token())).assert_true();
	for result in session.execute(
		"ACK LIVE $qid CURSOR IF true THEN $token ELSE $token END; DEFINE CLASS other TYPE NODE;",
		&inputs,
	)? {
		result.result?;
	}
	(matches!(session.next_live(id)?, LiveNext::Idle)).assert_true();
	let handle = session.take_live_subscription(id)?;
	(&(handle.cursor()), &(delivery.token())).assert_equal();
	drop(handle);
	drop(session);
	store.checkpoint(CheckpointMode::Restart)?;
	drop(store);
	let store = Datastore::open_local(&directory, options)?;
	(&(store.begin().live_info(id)?.cursor()), &(delivery.token())).assert_equal();
	let mut tail = store.resume_live(delivery.token())?;
	(matches!(tail.next()?, LiveNext::Idle)).assert_true();
	drop(tail);
	drop(store);
	std::fs::remove_dir_all(&directory).map_err(|error| {
		FerrosGrynnError::invalid_argument("ACK fixture cleanup failed")
			.with_technical_source(error)
	})?;
	Ok(())
}

/// Verifies live gql ack local sync.
#[test]
fn live_gql_ack_local_sync() -> FerrosGrynnResult<()> {
	gql_ack_survives_reopen(IoMode::Sync)
}

/// Verifies live gql ack local native async.
#[cfg(feature = "native-async-io")]
#[test]
fn live_gql_ack_local_native_async() -> FerrosGrynnResult<()> {
	gql_ack_survives_reopen(IoMode::NativeAsync)
}

/// Text bindings remain durable identities after lexical session detach;
/// reopened queries retain their original parameter dependencies.
fn gql_binding_survives_reopen(mode: IoMode) -> FerrosGrynnResult<()> {
	use ferros_grynn::{
		gql_status::FerrosGrynnError,
		live::{LiveId, LiveNext, LivePayload},
	};

	use crate::test_assertions::{AssertComparison as _, AssertCondition as _};
	let directory = std::env::temp_dir().join(format!(
		"fg-live-bind-{:016x}",
		Uuid::now_v7().as_inner().as_u128() & u128::from(u64::MAX),
	));
	let mut options = LocalOptions::default();
	options.io_mode = mode;
	options.buffer_pool_pages = 16;
	let store = Datastore::create_local(&directory, options)?;
	let mut session = store.session();
	let mut output = session.execute(
		"DEFINE CLASS person TYPE NODE CHANGEFEED 1d FULL; DEFINE FIELD age ON CLASS person TYPE \
		 int; CREATE person:a SET age = 19; LIVE LET $minimum = 18; LET $sub = LIVE SELECT VALUE \
		 age FROM person WHERE age >= $minimum; RETURN $sub;",
		&Object::new(),
	)?;
	for item in &output {
		item.result.as_ref().map_err(Clone::clone)?;
	}
	let result = output
		.pop()
		.ok_or_else(|| FerrosGrynnError::invalid_argument("binding query missing"))?
		.result?;
	let Value::Uuid(uuid) = result.value() else {
		return Err(FerrosGrynnError::invalid_argument("binding query UUID missing"));
	};
	let id = LiveId::from_uuid(*uuid)?;
	let info = store.begin().live_info(id)?;
	let binding = info
		.bindings()
		.first()
		.ok_or_else(|| FerrosGrynnError::invalid_argument("binding dependency missing"))?
		.id();
	for result in session.execute(
		"ALTER LIVE LET $minimum VALUE 21; DEFINE CLASS generation TYPE NODE;",
		&Object::new(),
	)? {
		result.result?;
	}
	let LiveNext::Delivery(delivery) = session.next_live(id)? else {
		return Err(FerrosGrynnError::invalid_argument("binding exit delivery missing"));
	};
	(matches!(delivery.payload(), LivePayload::Batch(batch) if batch.events()[0].before() == Some(&19.into()) && batch.events()[0].after().is_none())).assert_true();
	for result in session.execute(
		"ACK LIVE $sub CURSOR $token;",
		&Object::from_iter([("token", Value::from(delivery.token().encode()))]),
	)? {
		result.result?;
	}
	let (duration_binding, zero) = exercise_live_deadline_binding(&store, &mut session)?;
	let (navigation_binding, navigation_record) =
		exercise_live_navigation_binding(&store, &mut session)?;
	let counts = session.execute(
		"LIVE LET $count = $initial; INFO FOR LIVE LET $count; TRAVERSE FROM [1, 1, 2] |>\x20
		 tv::limit($count); TRAVERSE FROM [1, 1, 2] |> tv::skip($count);",
		&Object::from_iter([("initial", Value::from(u64::MAX))]),
	)?;
	for result in &counts {
		result.result.as_ref().map_err(Clone::clone)?;
	}
	let count_uuid = counts[1]
		.result
		.as_ref()
		.map_err(Clone::clone)?
		.value()
		.as_object()
		.and_then(|info| info.get("id"))
		.and_then(Value::as_uuid)
		.ok_or_else(|| FerrosGrynnError::invalid_argument("count binding identity missing"))?;
	let count_binding = ferros_grynn::live::LiveBindingId::from_uuid(*count_uuid)?;
	(
		&(counts[2].result.as_ref().map_err(Clone::clone)?.value()),
		&(&Value::from(vec![Value::from(1), Value::from(1), Value::from(2)])),
	)
		.assert_equal();
	(
		&(counts[3].result.as_ref().map_err(Clone::clone)?.value()),
		&(&Value::from(Vec::<Value>::new())),
	)
		.assert_equal();
	for result in session.execute("ALTER LIVE LET $count VALUE 0 - 1;", &Object::new())? {
		result.result?;
	}
	for query in ["TRAVERSE FROM [] |> tv::limit($count);", "TRAVERSE FROM [] |> tv::skip($count);"]
	{
		(session.execute(query, &Object::new())?[0].result.is_err())
			.assert_true_with(|| query.to_string());
	}
	for result in session.execute(
		"ALTER LIVE LET $count VALUE $initial;",
		&Object::from_iter([("initial", Value::from(u64::MAX))]),
	)? {
		result.result?;
	}
	drop(session);
	store.checkpoint(CheckpointMode::Restart)?;
	drop(store);
	let store = Datastore::open_local(&directory, options)?;
	(&(store.begin().live_binding_value(binding)?), &(21.into())).assert_equal();
	(&(store.begin().live_binding_value(duration_binding)?), &(zero)).assert_equal();
	(&(store.begin().live_binding_value(count_binding)?), &(Value::from(u64::MAX))).assert_equal();
	(&(store.begin().live_binding_info(binding)?.revision().definition().get()), &(2))
		.assert_equal();
	let mut handle = store.resume_live(delivery.token())?;
	let mut session = store.session();
	(&(store.begin().live_binding_value(navigation_binding)?), &(Value::from(navigation_record)))
		.assert_equal();
	let navigation = session.execute(
		"LIVE LET $record = $restored; LET $read = | | -> any { RETURN $record.age; }; RETURN \
		 $read(); TRAVERSE FROM [0] |> tv::map(|$x| $record.age);",
		&Object::from_iter([("restored", store.begin().live_binding_value(navigation_binding)?)]),
	)?;
	for result in &navigation {
		result.result.as_ref().map_err(Clone::clone)?;
	}
	(&(navigation[2].result.as_ref().map_err(Clone::clone)?.value()), &(&Value::from(20)))
		.assert_equal();
	(
		&(navigation[3].result.as_ref().map_err(Clone::clone)?.value()),
		&(&Value::from(vec![Value::from(20)])),
	)
		.assert_equal();
	let counts = session.execute(
		"LIVE LET $count = $restored; TRAVERSE FROM [1, 1, 2] |> tv::limit($count); TRAVERSE FROM \
		 [1, 1, 2] |> tv::skip($count);",
		&Object::from_iter([("restored", store.begin().live_binding_value(count_binding)?)]),
	)?;
	for result in &counts {
		result.result.as_ref().map_err(Clone::clone)?;
	}
	(
		&(counts[1].result.as_ref().map_err(Clone::clone)?.value()),
		&(&Value::from(vec![Value::from(1), Value::from(1), Value::from(2)])),
	)
		.assert_equal();
	(
		&(counts[2].result.as_ref().map_err(Clone::clone)?.value()),
		&(&Value::from(Vec::<Value>::new())),
	)
		.assert_equal();
	let inputs = Object::from_iter([("minimum", Value::from(binding.as_uuid()))]);
	for result in
		session.execute("INFO FOR LIVE LET $minimum; ALTER LIVE LET $minimum VALUE 17;", &inputs)?
	{
		result.result?;
	}
	let LiveNext::Delivery(delivery) = handle.next()? else {
		return Err(FerrosGrynnError::invalid_argument("reopened binding entry delivery missing"));
	};
	(matches!(delivery.payload(), LivePayload::Batch(batch) if batch.events()[0].before().is_none() && batch.events()[0].after() == Some(&19.into()))).assert_true();
	handle.ack(delivery.token())?;
	drop(handle);
	for result in session.execute(
		"KILL $sub; REMOVE LIVE LET $minimum;",
		&Object::from_iter([
			("sub", Value::from(id.as_uuid())),
			("minimum", Value::from(binding.as_uuid())),
		]),
	)? {
		result.result?;
	}
	drop(session);
	store.checkpoint(CheckpointMode::Restart)?;
	drop(store);
	let store = Datastore::open_local(&directory, options)?;
	(&(store.begin().live_binding_info(binding)?.state()), &(LiveState::Closed)).assert_equal();
	drop(store);
	std::fs::remove_dir_all(&directory).map_err(|error| {
		FerrosGrynnError::invalid_argument("binding fixture cleanup failed")
			.with_technical_source(error)
	})?;
	Ok(())
}

/// Runs the original deadline-binding queries before the shared reopen
/// boundary.
///
/// # Errors
/// Returns the original query, catalog or fixture error at the same scenario
/// step.
///
/// # Panics
/// Panics if an original deadline or rejected-publication assertion fails.
fn exercise_live_deadline_binding(
	store: &Datastore,
	session: &mut ferros_grynn::datastore::GqlSession,
) -> FerrosGrynnResult<(ferros_grynn::live::LiveBindingId, Value)> {
	use ferros_grynn::gql_status::FerrosGrynnError;

	use crate::test_assertions::AssertCondition as _;
	let fixed_duration = Value::from("1s".parse::<ferros_grynn::values::Duration>()?);
	let duration_results = session.execute(
		"LIVE LET $deadline = $fixed; EXPLAIN SELECT VALUE age FROM person TIMEOUT $deadline; \
		 INFO FOR LIVE LET $deadline; SELECT VALUE age FROM person TIMEOUT $deadline;",
		&Object::from_iter([("fixed", fixed_duration)]),
	)?;
	for result in &duration_results {
		result.result.as_ref().map_err(Clone::clone)?;
	}
	let Some(Value::Uuid(duration_uuid)) = duration_results[2]
		.result
		.as_ref()
		.map_err(Clone::clone)?
		.value()
		.as_object()
		.and_then(|value| value.get("id"))
	else {
		return Err(FerrosGrynnError::invalid_argument("duration binding identity missing"));
	};
	let duration_binding = ferros_grynn::live::LiveBindingId::from_uuid(*duration_uuid)?;
	let mut setup = store.begin();
	let scope = setup.define_graph("deadline_scope", ferros_grynn::graph::GraphKind::Graph)?;
	setup.attach_node(scope.id(), &"person:a".parse()?, None, None)?;
	setup.commit()?;
	for result in session
		.execute("LET $scope = $graph;", &Object::from_iter([("graph", Value::from(scope))]))?
	{
		result.result?;
	}
	for result in session.execute(
		"TRAVERSE FROM MATCH (person AS n) IN GRAPH deadline_scope TIMEOUT $deadline RETURN n |> \
		 tv::count(); TRAVERSE FROM $scope |> tv::V() |> tv::as('n') |> tv::match(MATCH (person \
		 AS n) IN GRAPH deadline_scope TIMEOUT $deadline RETURN n) |> tv::count(); BUILD GRAPH \
		 deadline_copy FROM (MATCH (person AS n) IN GRAPH deadline_scope TIMEOUT $deadline RETURN \
		 n) NODES n;",
		&Object::new(),
	)? {
		result.result?;
	}
	for result in session.execute(
		"BEGIN IMPORT; CREATE person:import_timed SET age = 19 TIMEOUT $deadline; DELETE \
		 person:import_timed TIMEOUT $deadline; END IMPORT;",
		&Object::new(),
	)? {
		result.result?;
	}
	let zero = Value::from("0ms".parse::<ferros_grynn::values::Duration>()?);
	let rejected = session.execute(
		"ALTER LIVE LET $deadline VALUE $zero; CREATE person:timed_out SET age = 19 TIMEOUT \
		 $deadline;",
		&Object::from_iter([("zero", zero.clone())]),
	)?;
	(rejected[0].result.is_ok()).assert_true();
	(rejected[1].result.is_err()).assert_true();
	(store.begin().atom(&"person:timed_out".parse()?)?.is_none()).assert_true();
	let rejected_import = session.execute(
		"BEGIN IMPORT; CREATE person:import_timed_out SET age = 19 TIMEOUT $deadline; END IMPORT;",
		&Object::new(),
	)?;
	(rejected_import[0].result.is_err()).assert_true();
	(store.begin().atom(&"person:import_timed_out".parse()?)?.is_none()).assert_true();
	let rejected_explain = session.execute(
		"EXPLAIN ANALYZE CREATE person:explain_timed_out SET age = 19 TIMEOUT $deadline;",
		&Object::new(),
	)?;
	(rejected_explain[0].result.is_err()).assert_true();
	(store.begin().atom(&"person:explain_timed_out".parse()?)?.is_none()).assert_true();
	for source in [
		"TRAVERSE FROM MATCH (person AS n) IN GRAPH deadline_scope TIMEOUT $deadline RETURN n |> \
		 tv::count();",
		"TRAVERSE FROM $scope |> tv::V() |> tv::as('n') |> tv::match(MATCH (person AS n) IN GRAPH \
		 deadline_scope TIMEOUT $deadline RETURN n) |> tv::count();",
		"BUILD GRAPH deadline_rejected FROM (MATCH (person AS n) IN GRAPH deadline_scope TIMEOUT \
		 $deadline RETURN n) NODES n;",
	] {
		(session.execute(source, &Object::new())?[0].result.is_err())
			.assert_true_with(|| source.to_string());
	}
	(store.begin().named_graph("deadline_rejected")?.is_none()).assert_true();
	Ok((duration_binding, zero))
}

/// Runs the original navigation/closure and small-budget binding checks.
///
/// # Errors
/// Returns the original query, catalog or fixture error at the same scenario
/// step.
///
/// # Panics
/// Panics if an original snapshot, navigation or memory-bound assertion fails.
fn exercise_live_navigation_binding(
	store: &Datastore,
	session: &mut ferros_grynn::datastore::GqlSession,
) -> FerrosGrynnResult<(ferros_grynn::live::LiveBindingId, RecordId)> {
	use ferros_grynn::gql_status::FerrosGrynnError;

	use crate::test_assertions::{AssertComparison as _, AssertCondition as _};
	let navigation = session.execute(
		"DEFINE CLASS navigation TYPE NODE; DEFINE FIELD age ON CLASS navigation TYPE int; DEFINE \
		 FIELD payload ON CLASS navigation TYPE array<int>; CREATE navigation:a SET age = 19; \
		 LIVE LET $record = navigation:a; LET $read = | | -> any { RETURN $record.age; }; LET \
		 $large = | | -> any { RETURN $record.payload; }; INFO FOR LIVE LET $record;",
		&Object::new(),
	)?;
	for result in &navigation {
		result.result.as_ref().map_err(Clone::clone)?;
	}
	let Some(Value::Uuid(navigation_uuid)) = navigation
		.last()
		.and_then(|result| result.result.as_ref().ok())
		.and_then(|result| result.value().as_object())
		.and_then(|info| info.get("id"))
	else {
		return Err(FerrosGrynnError::invalid_argument("navigation binding identity missing"));
	};
	let navigation_binding = ferros_grynn::live::LiveBindingId::from_uuid(*navigation_uuid)?;
	let navigation_record = "navigation:a".parse::<RecordId>()?;
	let payload = store
		.property_dictionary()
		.get("payload")
		.ok_or_else(|| FerrosGrynnError::invalid_argument("navigation payload field missing"))?;
	let mut populate = store.begin();
	populate.set_base_property(
		&navigation_record,
		payload,
		Value::from(vec![Value::from(1); 20_000]),
	)?;
	populate.commit()?;
	for result in session.execute("BEGIN READ ONLY; RETURN $read();", &Object::new())? {
		(result.result.is_ok()).assert_true();
	}
	let mut update = store.begin();
	let age = store
		.property_dictionary()
		.get("age")
		.ok_or_else(|| FerrosGrynnError::invalid_argument("navigation field missing"))?;
	update.set_base_property(&navigation_record, age, 20.into())?;
	update.commit()?;
	for (query, expected) in [
		("RETURN $read();", Value::from(19)),
		("TRAVERSE FROM [0] |> tv::map(|$x| $record.age);", Value::from(vec![Value::from(19)])),
		("CANCEL; RETURN $read();", Value::from(20)),
	] {
		let results = session.execute(query, &Object::new())?;
		for result in &results {
			result.result.as_ref().map_err(Clone::clone)?;
		}
		(
			&(results
				.last()
				.and_then(|result| result.result.as_ref().ok())
				.map(|result| result.value())),
			&(Some(&expected)),
		)
			.assert_equal_with(|| query.to_string());
	}
	let mut navigation_options = ferros_grynn::gql::QueryExecutionOptions::default();
	navigation_options.barrier_memory_bytes = std::num::NonZeroUsize::new(4096)
		.ok_or_else(|| FerrosGrynnError::invalid_argument("navigation budget is zero"))?;
	let small = session.execute_with_options(
		"RETURN $read();",
		&Object::new(),
		navigation_options.clone(),
	)?;
	(&(small[0].result.as_ref().map_err(Clone::clone)?.value()), &(&Value::from(20)))
		.assert_equal();
	let bounded =
		session.execute_with_options("RETURN $large();", &Object::new(), navigation_options)?;
	(bounded[0].result.is_err()).assert_true();
	Ok((navigation_binding, navigation_record))
}

/// Verifies live gql binding local sync.
#[test]
fn live_gql_binding_local_sync() -> FerrosGrynnResult<()> {
	gql_binding_survives_reopen(IoMode::Sync)
}

/// SDK compound recipes, captured values and exact result history survive
/// reopen.
fn sdk_recipe_collections_survive_reopen(mode: IoMode) -> FerrosGrynnResult<()> {
	use ferros_grynn::{
		changefeed::{ChangefeedConfig, ChangefeedSource},
		gql_status::FerrosGrynnError,
		live::{LiveNext, LivePayload},
	};

	use crate::test_assertions::{AssertComparison as _, AssertCondition as _};
	let directory = std::env::temp_dir().join(format!(
		"fg-live-arr-{:016x}",
		Uuid::now_v7().as_inner().as_u128() & u128::from(u64::MAX)
	));
	let mut options = LocalOptions::default();
	options.io_mode = mode;
	options.buffer_pool_pages = 16;
	let store = Datastore::create_local(&directory, options)?;
	let mut tx = store.begin();
	let class = tx.define_class(ClassSchema::new("item", AtomKind::Node, [])?)?;
	tx.configure_changefeed(
		ChangefeedSource::Class(class),
		ChangefeedConfig::new("P1D".parse()?)?,
	)?;
	tx.create_atom("item:a".parse()?, PropertyMap::new())?;
	let input = tx.define_live_binding(
		LiveExpr::Literal(5.into()),
		Object::new(),
		LiveBindingOptions::default(),
	)?;
	let recipe = LiveExpr::Object(vec![
		("typed".into(), LiveExpr::CallWithOptions {
			name: "type::is".into(),
			type_argument: Some("int".into()),
			distinct: false,
			arguments: vec![(None, LiveExpr::Binding(input))],
		}),
		(
			"power".into(),
			LiveExpr::Binding(input).binary(LiveBinaryOperator::Power, LiveExpr::Literal(2.into())),
		),
		(
			"remainder".into(),
			LiveExpr::Binding(input)
				.binary(LiveBinaryOperator::Remainder, LiveExpr::Literal(2.into())),
		),
		("truthy".into(), LiveExpr::Binding(input).truthy()),
		(
			"elvis".into(),
			LiveExpr::Binding(input)
				.binary(LiveBinaryOperator::Elvis, LiveExpr::Parameter("fixed".into())),
		),
		(
			"contains".into(),
			LiveExpr::Array(vec![LiveExpr::Binding(input)]).binary(
				LiveBinaryOperator::ContainsAll,
				LiveExpr::Array(vec![LiveExpr::Binding(input)]),
			),
		),
		("range".into(), LiveExpr::Range {
			start: Some(Box::new(LiveExpr::Binding(input))),
			end: None,
			inclusive_end: false,
		}),
		("cast".into(), LiveExpr::Cast {
			target: "string".into(),
			value: Box::new(LiveExpr::Binding(input)),
		}),
		("case".into(), LiveExpr::Case {
			operand: Some(Box::new(LiveExpr::Binding(input))),
			branches: vec![
				(LiveExpr::Literal(0.into()), LiveExpr::Parameter("fixed".into())),
				(
					LiveExpr::Literal(11.into()),
					LiveExpr::Negate(Box::new(LiveExpr::Binding(input))),
				),
			],
			else_value: Some(Box::new(LiveExpr::Binding(input))),
		}),
		(
			"unique".into(),
			LiveExpr::Set(vec![
				LiveExpr::Binding(input),
				LiveExpr::Parameter("fixed".into()),
				LiveExpr::Binding(input),
			]),
		),
		("conditional".into(), LiveExpr::Conditional {
			condition: Box::new(LiveExpr::Binding(input)),
			then_value: Box::new(LiveExpr::Parameter("fixed".into())),
			else_value: Box::new(LiveExpr::Literal(0.into())),
		}),
		("last".into(), LiveExpr::Binding(input)),
		("last".into(), LiveExpr::Negate(Box::new(LiveExpr::Binding(input)))),
		("selected".into(), LiveExpr::Index {
			base: Box::new(LiveExpr::Access {
				base: Box::new(LiveExpr::Object(vec![(
					"values".into(),
					LiveExpr::Array(vec![LiveExpr::Binding(input)]),
				)])),
				field: "values".into(),
				null_safe: false,
			}),
			index: Box::new(LiveExpr::Literal(0.into())),
		}),
		("missing".into(), LiveExpr::Access {
			base: Box::new(LiveExpr::Literal(Value::None)),
			field: "value".into(),
			null_safe: true,
		}),
		(
			"parts".into(),
			LiveExpr::Array(vec![
				LiveExpr::Binding(input),
				LiveExpr::Positive(Box::new(LiveExpr::Literal(9_007_199_254_740_993_i64.into()))),
				LiveExpr::Parameter("fixed".into()),
			]),
		),
	]);
	let composite = tx.define_live_binding(
		recipe.clone(),
		Object::from_iter([("fixed", Value::from(7))]),
		LiveBindingOptions::default(),
	)?;
	tx.commit()?;
	let expected = |number: i64| -> FerrosGrynnResult<Value> {
		Ok(Value::Object(Object::from_iter([
			("typed", Value::Bool(true)),
			("power", Value::from(number * number)),
			("remainder", Value::from(number % 2)),
			("truthy", Value::Bool(number != 0)),
			(
				"elvis",
				Value::from(if number == 0 {
					7
				} else {
					number
				}),
			),
			("contains", Value::Bool(true)),
			(
				"range",
				Value::Range(ferros_grynn::values::Range::new(
					std::ops::Bound::Included(number.into()),
					std::ops::Bound::Unbounded,
				)?),
			),
			(
				"case",
				Value::from(match number {
					0 => 7,
					11 => -11,
					_ => number,
				}),
			),
			(
				"unique",
				Value::Set(ferros_grynn::values::Set::from(vec![
					Value::from(number),
					Value::from(7),
				])),
			),
			(
				"conditional",
				Value::from(if number == 0 {
					0
				} else {
					7
				}),
			),
			("last", Value::from(-number)),
			("cast", Value::from(number.to_string())),
			("selected", Value::from(number)),
			("missing", Value::None),
			(
				"parts",
				Value::from(vec![
					Value::from(number),
					Value::from(9_007_199_254_740_993_i64),
					Value::from(7),
				]),
			),
		])))
	};
	(&(store.begin().live_binding_value(composite)?), &(expected(5)?)).assert_equal();
	let mut handle = store.subscribe_live(
		LiveQuery::new(LiveSource::Class(class))
			.with_projection(LiveProjection::Value(LiveExpr::Binding(composite))),
		LiveOptions::default(),
	)?;
	let initial = handle.cursor().clone();
	let mut update = store.begin();
	update.replace_live_binding_value(input, 11.into())?;
	update.commit()?;
	let LiveNext::Delivery(delivery) = handle.next()? else {
		return Err(FerrosGrynnError::invalid_argument("SDK recipe delivery missing"));
	};
	(matches!(delivery.payload(), LivePayload::Batch(batch) if batch.events().len() == 1 && batch.events()[0].before() == Some(&expected(5)?) && batch.events()[0].after() == Some(&expected(11)?))).assert_true();
	drop(handle);
	store.checkpoint(CheckpointMode::Restart)?;
	drop(store);
	let store = Datastore::open_local(&directory, options)?;
	(&(store.begin().live_binding_value(composite)?), &(expected(11)?)).assert_equal();
	(&(store.begin().live_binding_info(composite)?.expression()), &(&recipe)).assert_equal();
	let mut handle = store.resume_live(&initial)?;
	let LiveNext::Delivery(replayed) = handle.next()? else {
		return Err(FerrosGrynnError::invalid_argument("SDK recipe replay missing"));
	};
	(&(replayed.token()), &(delivery.token())).assert_equal();
	(matches!(replayed.payload(), LivePayload::Batch(batch) if batch.events().len() == 1 && batch.events()[0].before() == Some(&expected(5)?) && batch.events()[0].after() == Some(&expected(11)?))).assert_true();
	handle.ack(replayed.token())?;
	let mut update = store.begin();
	update.replace_live_binding_value(input, 0.into())?;
	update.commit()?;
	let LiveNext::Delivery(next) = handle.next()? else {
		return Err(FerrosGrynnError::invalid_argument("SDK reopened recipe delivery missing"));
	};
	(matches!(next.payload(), LivePayload::Batch(batch) if batch.events()[0].before() == Some(&expected(11)?) && batch.events()[0].after() == Some(&expected(0)?))).assert_true();
	handle.ack(next.token())?;
	drop(handle);
	drop(store);
	std::fs::remove_dir_all(&directory).map_err(|error| {
		FerrosGrynnError::invalid_argument("SDK recipe fixture cleanup failed")
			.with_technical_source(error)
	})?;
	Ok(())
}

/// Verifies live recipe sdk local sync.
#[test]
fn live_recipe_sdk_local_sync() -> FerrosGrynnResult<()> {
	sdk_recipe_collections_survive_reopen(IoMode::Sync)
}

/// Graph policy and qualified record images survive independently of current
/// primary values; saved-result replay and ACK use the same owned SDK handles.
fn graph_record_survives_reopen(mode: IoMode) -> FerrosGrynnResult<()> {
	use ferros_grynn::{
		changefeed::{ChangefeedConfig, ChangefeedSource},
		gql_status::FerrosGrynnError,
		graph::GraphKind,
		live::{LiveIdentity, LiveNext, LivePayload},
	};

	use crate::test_assertions::{AssertComparison as _, AssertCondition as _};
	let directory = std::env::temp_dir().join(format!(
		"fg-live-single-{:016x}",
		Uuid::now_v7().as_inner().as_u128() & u128::from(u64::MAX)
	));
	let mut options = LocalOptions::default();
	options.io_mode = mode;
	options.buffer_pool_pages = 16;
	let store = Datastore::create_local(&directory, options)?;
	let mut setup = store.begin();
	setup.define_class(ClassSchema::new("item", AtomKind::Node, [
		ferros_grynn::datastore::ClassField::new("score", ferros_grynn::values::Type::Int),
	])?)?;
	setup.commit()?;
	let score = store.property_dictionary().get("score").ok_or_else(|| {
		FerrosGrynnError::invalid_argument("graph record score dictionary key missing")
	})?;
	let mut setup = store.begin();
	let record: RecordId = "item:a".parse()?;
	setup.create_atom(record.clone(), PropertyMap::from_iter([(score, 1.into())]))?;
	let graph = setup.define_graph("single", GraphKind::Graph)?;
	let other = setup.define_graph("other", GraphKind::Graph)?;
	setup.attach_node(graph.id(), &record, None, None)?;
	setup.configure_changefeed(
		ChangefeedSource::Graph(graph.id()),
		ChangefeedConfig::new("P1D".parse()?)?,
	)?;
	let initial_value = 9_007_199_254_740_993_i64;
	setup.set_overlay(graph.id(), &record, score, initial_value.into())?;
	setup.commit()?;
	let query = LiveQuery::new(LiveSource::GraphRecord {
		graph: graph.clone(),
		record: record.clone(),
	})
	.with_projection(LiveProjection::Value(LiveExpr::field("score")?));
	let mut handle = store.subscribe_live(query.clone(), LiveOptions::default())?;
	let cursor = handle.cursor().clone();
	let mut session = store.session();
	let results = session.execute(
		"LET $scope = $graph; LET $sub = LIVE SELECT VALUE score FROM item:a IN GRAPH $scope; \
		 RETURN $sub;",
		&Object::from_iter([("graph", Value::from(graph.clone()))]),
	)?;
	for result in &results {
		result.result.as_ref().map_err(Clone::clone)?;
	}
	let id = results
		.last()
		.and_then(|result| result.result.as_ref().ok())
		.and_then(|result| result.value().as_uuid())
		.ok_or_else(|| FerrosGrynnError::invalid_argument("frozen scope identity missing"))?;
	let mut frozen = session.take_live_subscription(ferros_grynn::live::LiveId::from_uuid(*id)?)?;
	let frozen_cursor = frozen.cursor().clone();
	for result in session.execute(
		"ALTER LET $scope VALUE $replacement;",
		&Object::from_iter([("replacement", Value::from(other))]),
	)? {
		result.result?;
	}
	drop(session);
	let mut update = store.begin();
	update.set_overlay(graph.id(), &record, score, (initial_value + 1).into())?;
	update.commit()?;
	let LiveNext::Delivery(delivery) = handle.next()? else {
		return Err(FerrosGrynnError::invalid_argument("graph record delivery missing"));
	};
	let LiveNext::Delivery(frozen_delivery) = frozen.next()? else {
		return Err(FerrosGrynnError::invalid_argument("frozen scope delivery missing"));
	};
	drop(frozen);
	drop(handle);
	store.checkpoint(CheckpointMode::Restart)?;
	drop(store);
	let store = Datastore::open_local(&directory, options)?;
	(&(store.begin().live_info(cursor.live_id())?.query()), &(&query)).assert_equal();
	let frozen_info = store.begin().live_info(frozen_cursor.live_id())?;
	(&(frozen_info.query().source()), &(query.source())).assert_equal();
	(&(frozen_info.query().projection()), &(query.projection())).assert_equal();
	(&(frozen_info.query().parameters().get("scope")), &(Some(&Value::from(graph.clone()))))
		.assert_equal();
	let mut update = store.begin();
	update.set_overlay(graph.id(), &record, score, (initial_value + 2).into())?;
	update.commit()?;
	let mut handle = store.resume_live(&cursor)?;
	let LiveNext::Delivery(replay) = handle.next()? else {
		return Err(FerrosGrynnError::invalid_argument("graph record replay missing"));
	};
	(&(replay.token()), &(delivery.token())).assert_equal();
	let LivePayload::Batch(batch) = replay.payload() else {
		return Err(FerrosGrynnError::invalid_argument("graph record replay GAP"));
	};
	(&(batch.events().len()), &(1)).assert_equal();
	(&(batch.events()[0].before()), &(Some(&initial_value.into()))).assert_equal();
	(&(batch.events()[0].after()), &(Some(&(initial_value + 1).into()))).assert_equal();
	(matches!(batch.events()[0].identity(), LiveIdentity::Occurrence(occurrence)
		if occurrence.graph().id() == graph.id() && occurrence.atom().id() == &record))
	.assert_true();
	handle.ack(replay.token())?;
	let LiveNext::Delivery(tail) = handle.next()? else {
		return Err(FerrosGrynnError::invalid_argument("graph record tail missing"));
	};
	(matches!(tail.payload(), LivePayload::Batch(batch)
		if batch.events()[0].before() == Some(&(initial_value + 1).into()) && batch.events()[0].after() == Some(&(initial_value + 2).into()))).assert_true();
	handle.ack(tail.token())?;
	handle.close()?;
	drop(handle);
	let mut frozen = store.resume_live(&frozen_cursor)?;
	let LiveNext::Delivery(replay) = frozen.next()? else {
		return Err(FerrosGrynnError::invalid_argument("frozen scope replay missing"));
	};
	(&(replay.token()), &(frozen_delivery.token())).assert_equal();
	let LivePayload::Batch(batch) = replay.payload() else {
		return Err(FerrosGrynnError::invalid_argument("frozen scope replay GAP"));
	};
	(&(batch.events().len()), &(1)).assert_equal();
	(&(batch.events()[0].before()), &(Some(&initial_value.into()))).assert_equal();
	(&(batch.events()[0].after()), &(Some(&(initial_value + 1).into()))).assert_equal();
	(matches!(batch.events()[0].identity(), LiveIdentity::Occurrence(occurrence)
		if occurrence.graph().id() == graph.id() && occurrence.atom().id() == &record))
	.assert_true();
	frozen.ack(replay.token())?;
	let LiveNext::Delivery(tail) = frozen.next()? else {
		return Err(FerrosGrynnError::invalid_argument("frozen scope tail missing"));
	};
	(matches!(tail.payload(), LivePayload::Batch(batch)
		if batch.events()[0].before() == Some(&(initial_value + 1).into()) && batch.events()[0].after() == Some(&(initial_value + 2).into()))).assert_true();
	frozen.ack(tail.token())?;
	frozen.close()?;
	drop(frozen);
	drop(store);
	std::fs::remove_dir_all(&directory).map_err(|error| {
		FerrosGrynnError::invalid_argument("graph record cleanup failed")
			.with_technical_source(error)
	})?;
	Ok(())
}

/// Verifies live graph record local sync.
#[test]
fn live_graph_record_local_sync() -> FerrosGrynnResult<()> {
	graph_record_survives_reopen(IoMode::Sync)
}

/// Expression scopes preserve exact qualified saved results and durable name
/// anchors independently of the current graph-effective primary values.
fn scope_expression_survives_reopen(mode: IoMode) -> FerrosGrynnResult<()> {
	use ferros_grynn::{
		changefeed::{ChangefeedConfig, ChangefeedSource},
		gql_status::FerrosGrynnError,
		graph::GraphKind,
		live::{LiveIdentity, LiveNext, LivePayload},
	};

	use crate::test_assertions::{AssertComparison as _, AssertCondition as _};
	let directory = std::env::temp_dir().join(format!(
		"fg-live-scope-{:016x}",
		Uuid::now_v7().as_inner().as_u128() & u128::from(u64::MAX)
	));
	let mut options = LocalOptions::default();
	options.io_mode = mode;
	options.buffer_pool_pages = 16;
	let store = Datastore::create_local(&directory, options)?;
	let mut setup = store.begin();
	let class = setup.define_class(ClassSchema::new("item", AtomKind::Node, [
		ferros_grynn::datastore::ClassField::new("score", ferros_grynn::values::Type::Int),
	])?)?;
	setup.define_class(ClassSchema::new("scope_unused", AtomKind::Node, [
		ferros_grynn::datastore::ClassField::new("score", ferros_grynn::values::Type::Int),
	])?)?;
	setup.commit()?;
	let score = store
		.property_dictionary()
		.get("score")
		.ok_or_else(|| FerrosGrynnError::invalid_argument("scope score dictionary key missing"))?;
	let mut setup = store.begin();
	let record: RecordId = "item:a".parse()?;
	setup.create_atom(record.clone(), PropertyMap::from_iter([(score, 1.into())]))?;
	let first = setup.define_graph("scope_a", GraphKind::Graph)?;
	let second = setup.define_graph("scope_b", GraphKind::Graph)?;
	for (graph, value) in [(&first, 10), (&second, 20)] {
		setup.attach_node(graph.id(), &record, None, None)?;
		setup.set_overlay(graph.id(), &record, score, value.into())?;
		setup.configure_changefeed(
			ChangefeedSource::Graph(graph.id()),
			ChangefeedConfig::new("P1D".parse()?)?,
		)?;
	}
	let scope = setup.define_live_binding(
		LiveExpr::Literal("scope_a".into()),
		Object::new(),
		LiveBindingOptions::default(),
	)?;
	setup.commit()?;
	let score_expression = LiveExpr::field("score")?;
	let queries = [
		LiveSource::GraphExpr(Box::new(LiveExpr::Binding(scope))),
		LiveSource::GraphClassExpr {
			graph: Box::new(LiveExpr::Binding(scope)),
			class,
		},
		LiveSource::GraphRecordExpr {
			graph: Box::new(LiveExpr::Binding(scope)),
			record: record.clone(),
		},
	]
	.map(|source| {
		LiveQuery::new(source).with_projection(LiveProjection::Value(score_expression.clone()))
	});
	let mut readers = Vec::new();
	let mut cursors = Vec::new();
	for query in &queries {
		let handle = store.subscribe_live(query.clone(), LiveOptions::default())?;
		cursors.push(handle.cursor().clone());
		readers.push(handle);
	}
	let mut change = store.begin();
	change.replace_live_binding_value(scope, "scope_b".into())?;
	change.set_overlay(second.id(), &record, score, 21.into())?;
	change.commit()?;
	let mut deliveries = Vec::new();
	for reader in &mut readers {
		let LiveNext::Delivery(delivery) = reader.next()? else {
			return Err(FerrosGrynnError::invalid_argument("scope saved result missing"));
		};
		deliveries.push(delivery);
	}
	drop(readers);
	store.checkpoint(CheckpointMode::Restart)?;
	drop(store);
	let store = Datastore::open_local(&directory, options)?;
	let mut change = store.begin();
	change.set_overlay(second.id(), &record, score, 22.into())?;
	change.commit()?;
	for ((cursor, saved), query) in cursors.iter().zip(&deliveries).zip(&queries) {
		(&(store.begin().live_info(cursor.live_id())?.query()), &(query)).assert_equal();
		let mut reader = store.resume_live(cursor)?;
		let LiveNext::Delivery(replay) = reader.next()? else {
			return Err(FerrosGrynnError::invalid_argument("scope replay missing"));
		};
		(&(replay.token()), &(saved.token())).assert_equal();
		let LivePayload::Batch(batch) = replay.payload() else {
			return Err(FerrosGrynnError::invalid_argument("scope replay GAP"));
		};
		let LivePayload::Batch(saved_batch) = saved.payload() else {
			return Err(FerrosGrynnError::invalid_argument("scope saved payload is not a batch"));
		};
		(&(batch), &(saved_batch)).assert_equal();
		(&(batch.events().len()), &(2)).assert_equal();
		for event in batch.events() {
			let LiveIdentity::Occurrence(occurrence) = event.identity() else {
				return Err(FerrosGrynnError::invalid_argument("scope replay identity missing"));
			};
			(&(occurrence.atom().id()), &(&record)).assert_equal();
			if occurrence.graph().id() == first.id() {
				(&(event.before()), &(Some(&Value::from(10)))).assert_equal();
				(&(event.after()), &(None)).assert_equal();
			} else {
				(&(occurrence.graph().id()), &(second.id())).assert_equal();
				(&(event.before()), &(None)).assert_equal();
				(&(event.after()), &(Some(&Value::from(21)))).assert_equal();
			}
		}
		reader.ack(replay.token())?;
		let LiveNext::Delivery(tail) = reader.next()? else {
			return Err(FerrosGrynnError::invalid_argument("scope tail missing"));
		};
		(matches!(tail.payload(), LivePayload::Batch(batch) if batch.events().len() == 1
			&& batch.events()[0].before() == Some(&Value::from(21)) && batch.events()[0].after() == Some(&Value::from(22)))).assert_true();
		reader.ack(tail.token())?;
	}
	let mut recreate = store.begin();
	recreate.remove_graph("scope_b")?;
	let replacement = recreate.define_graph("scope_b", GraphKind::Graph)?;
	recreate.attach_node(replacement.id(), &record, None, None)?;
	recreate.set_overlay(replacement.id(), &record, score, 99.into())?;
	recreate.commit()?;
	store.checkpoint(CheckpointMode::Restart)?;
	drop(store);
	let store = Datastore::open_local(&directory, options)?;
	for cursor in &cursors {
		(&(store.begin().live_info(cursor.live_id())?.state()), &(LiveState::Paused))
			.assert_equal();
	}
	drop(store);
	std::fs::remove_dir_all(&directory).map_err(|error| {
		FerrosGrynnError::invalid_argument("scope expression cleanup failed")
			.with_technical_source(error)
	})?;
	Ok(())
}

/// Verifies live scope expression local sync.
#[test]
fn live_scope_expression_local_sync() -> FerrosGrynnResult<()> {
	scope_expression_survives_reopen(IoMode::Sync)
}

/// Defines the JOIN fixture overlays and constructs the query with independent
/// computed scopes.
///
/// # Errors
/// Propagates the existing schema, graph, binding and query-construction
/// failures in setup order.
fn prepare_join_scope_query(
	store: &Datastore,
) -> FerrosGrynnResult<(
	ferros_grynn::values::PropertyKeyId,
	RecordId,
	ferros_grynn::graph::GraphRef,
	ferros_grynn::graph::GraphRef,
	ferros_grynn::live::LiveBindingId,
	LiveQuery,
)> {
	use ferros_grynn::{
		changefeed::{ChangefeedConfig, ChangefeedSource},
		gql_status::FerrosGrynnError,
		graph::GraphKind,
		live::{LiveJoin, LiveJoinKind, LivePath},
	};
	let mut setup = store.begin();
	setup.define_class(ClassSchema::new("item", AtomKind::Node, [ClassField::new(
		"score",
		Type::Int,
	)])?)?;
	setup.define_class(ClassSchema::new("other", AtomKind::Node, [
		ClassField::new("padding", Type::String),
		ClassField::new("score", Type::Int),
	])?)?;
	setup.commit()?;
	let score = store
		.property_dictionary()
		.get("score")
		.ok_or_else(|| FerrosGrynnError::invalid_argument("JOIN fixture score property missing"))?;
	let mut setup = store.begin();
	let record: RecordId = "item:a".parse()?;
	setup.create_atom(record.clone(), PropertyMap::from_iter([(score, 1.into())]))?;
	let first = setup.define_graph("join_a", GraphKind::Graph)?;
	let second = setup.define_graph("join_b", GraphKind::Graph)?;
	let third = setup.define_graph("join_c", GraphKind::Graph)?;
	for (graph, value) in [(&first, 10), (&second, 20), (&third, 30)] {
		setup.attach_node(graph.id(), &record, None, None)?;
		setup.set_overlay(graph.id(), &record, score, value.into())?;
		setup.configure_changefeed(
			ChangefeedSource::Graph(graph.id()),
			ChangefeedConfig::new("P1D".parse()?)?,
		)?;
	}
	let left_scope = setup.define_live_binding(
		LiveExpr::Literal("join_a".into()),
		Object::new(),
		LiveBindingOptions::default(),
	)?;
	let right_scope = setup.define_live_binding(
		LiveExpr::Literal("join_b".into()),
		Object::new(),
		LiveBindingOptions::default(),
	)?;
	setup.commit()?;
	let field = |alias: &str| -> FerrosGrynnResult<LiveExpr> {
		Ok(LiveExpr::Field(LivePath::new(vec![alias.into(), "score".into()])?))
	};
	let query = LiveQuery::new(LiveSource::GraphRecord {
		graph: first,
		record: record.clone(),
	})
	.with_alias("p")?
	.with_joins(vec![
		LiveJoin::new(
			LiveSource::GraphExpr(Box::new(LiveExpr::Binding(left_scope))),
			"a",
			LiveJoinKind::Cross,
			None,
		)?,
		LiveJoin::new(
			LiveSource::GraphRecordExpr {
				graph: Box::new(LiveExpr::Binding(right_scope)),
				record: record.clone(),
			},
			"b",
			LiveJoinKind::Cross,
			None,
		)?,
	])
	.with_projection(LiveProjection::Value(LiveExpr::Array(vec![field("a")?, field("b")?])));
	Ok((score, record, second, third, left_scope, query))
}

/// Changes the fixture's left JOIN scope and retains the exact delivery used
/// for replay checks.
///
/// # Errors
/// Propagates the existing binding replacement and commit failures, then checks
/// delivery presence.
fn capture_join_scope_change(
	store: &Datastore,
	handle: &mut ferros_grynn::live::LiveSubscription,
	left_scope: ferros_grynn::live::LiveBindingId,
) -> FerrosGrynnResult<ferros_grynn::live::LiveDelivery> {
	use ferros_grynn::{gql_status::FerrosGrynnError, live::LiveNext};
	let mut change = store.begin();
	change.replace_live_binding_value(left_scope, "join_c".into())?;
	change.commit()?;
	let LiveNext::Delivery(saved) = handle.next()? else {
		return Err(FerrosGrynnError::invalid_argument("JOIN saved delivery missing"));
	};
	Ok(saved)
}

/// Publishes the class-generation change and scoped overlay update before JOIN
/// replay.
///
/// # Errors
/// Propagates the original class definition, overlay update and commit failures
/// in order.
fn publish_join_generation(
	store: &Datastore,
	third: &ferros_grynn::graph::GraphRef,
	record: &RecordId,
	score: ferros_grynn::values::PropertyKeyId,
) -> FerrosGrynnResult<()> {
	let mut change = store.begin();
	change.define_class(ClassSchema::new("join_generation", AtomKind::Node, [
		ClassField::new("score", Type::Int),
	])?)?;
	change.set_overlay(third.id(), record, score, 31.into())?;
	change.commit()?;
	Ok(())
}

/// Independent computed JOIN scopes retain their owners through replay and
/// generation replacement; recreating an unchanged name faults the query.
fn join_scopes_survive_reopen(mode: IoMode) -> FerrosGrynnResult<()> {
	use ferros_grynn::{
		gql_status::FerrosGrynnError,
		graph::GraphKind,
		live::{LiveNext, LivePayload},
	};

	use crate::test_assertions::AssertComparison as _;
	let directory = std::env::temp_dir().join(format!(
		"fg-live-join-{:016x}",
		Uuid::now_v7().as_inner().as_u128() & u128::from(u64::MAX)
	));
	let mut options = LocalOptions::default();
	options.io_mode = mode;
	options.buffer_pool_pages = 16;
	let store = Datastore::create_local(&directory, options)?;
	let (score, record, second, third, left_scope, query) = prepare_join_scope_query(&store)?;
	let mut handle = store.subscribe_live(query.clone(), LiveOptions::default())?;
	let cursor = handle.cursor().clone();
	let id = handle.id();
	let saved = capture_join_scope_change(&store, &mut handle, left_scope)?;
	drop(handle);
	store.checkpoint(CheckpointMode::Restart)?;
	drop(store);
	let store = Datastore::open_local(&directory, options)?;
	(&(store.begin().live_info(id)?.query()), &(&query)).assert_equal();
	publish_join_generation(&store, &third, &record, score)?;
	let mut handle = store.resume_live(&cursor)?;
	let LiveNext::Delivery(replay) = handle.next()? else {
		return Err(FerrosGrynnError::invalid_argument("JOIN replay delivery missing"));
	};
	(&(replay.token()), &(saved.token())).assert_equal();
	let (LivePayload::Batch(replayed), LivePayload::Batch(original)) =
		(replay.payload(), saved.payload())
	else {
		return Err(FerrosGrynnError::invalid_argument("JOIN saved replay is not a batch"));
	};
	(&(replayed), &(original)).assert_equal();
	handle.ack(replay.token())?;
	let LiveNext::Delivery(tail) = handle.next()? else {
		return Err(FerrosGrynnError::invalid_argument("JOIN current delivery missing"));
	};
	let LivePayload::Batch(batch) = tail.payload() else {
		return Err(FerrosGrynnError::invalid_argument("JOIN current payload is not a batch"));
	};
	(&(batch.events().len()), &(1)).assert_equal();
	(&(batch.events()[0].before()), &(Some(&Value::from(vec![30.into(), 20.into()]))))
		.assert_equal();
	(&(batch.events()[0].after()), &(Some(&Value::from(vec![31.into(), 20.into()]))))
		.assert_equal();
	handle.ack(tail.token())?;
	let confirmed = handle.cursor().clone();
	drop(handle);
	store.checkpoint(CheckpointMode::Restart)?;
	drop(store);
	let store = Datastore::open_local(&directory, options)?;
	(&(store.begin().live_info(id)?.cursor()), &(&confirmed)).assert_equal();
	let mut replace = store.begin();
	replace.remove_graph("join_b")?;
	let replacement = replace.define_graph("join_b", GraphKind::Graph)?;
	replace.attach_node(replacement.id(), &record, None, None)?;
	replace.set_overlay(replacement.id(), &record, score, 99.into())?;
	replace.commit()?;
	(&(replacement.id()), &(second.id())).assert_different();
	(&(store.begin().live_info(id)?.state()), &(LiveState::Paused)).assert_equal();
	store.checkpoint(CheckpointMode::Restart)?;
	drop(store);
	let store = Datastore::open_local(&directory, options)?;
	(&(store.begin().live_info(id)?.state()), &(LiveState::Paused)).assert_equal();
	drop(store);
	std::fs::remove_dir_all(&directory).map_err(|error| {
		FerrosGrynnError::invalid_argument("JOIN fixture cleanup failed")
			.with_technical_source(error)
	})?;
	Ok(())
}

/// Verifies live join scopes local sync.
#[test]
fn live_join_scopes_local_sync() -> FerrosGrynnResult<()> {
	join_scopes_survive_reopen(IoMode::Sync)
}

/// Verifies live join scopes local native async.
#[cfg(feature = "native-async-io")]
#[test]
fn live_join_scopes_local_native_async() -> FerrosGrynnResult<()> {
	join_scopes_survive_reopen(IoMode::NativeAsync)
}

/// Verifies live scope expression local native async.
#[cfg(feature = "native-async-io")]
#[test]
fn live_scope_expression_local_native_async() -> FerrosGrynnResult<()> {
	scope_expression_survives_reopen(IoMode::NativeAsync)
}

/// Verifies live graph record local native async.
#[cfg(feature = "native-async-io")]
#[test]
fn live_graph_record_local_native_async() -> FerrosGrynnResult<()> {
	graph_record_survives_reopen(IoMode::NativeAsync)
}

/// Verifies live recipe sdk local native async.
#[cfg(feature = "native-async-io")]
#[test]
fn live_recipe_sdk_local_native_async() -> FerrosGrynnResult<()> {
	sdk_recipe_collections_survive_reopen(IoMode::NativeAsync)
}

/// Verifies live gql binding local native async.
#[cfg(feature = "native-async-io")]
#[test]
fn live_gql_binding_local_native_async() -> FerrosGrynnResult<()> {
	gql_binding_survives_reopen(IoMode::NativeAsync)
}

/// Physical search paths resolve reactive operands in the same snapshot as
/// their exact fallback and validate explicit EF before pulling rows.
fn gql_search_snapshot_inputs(mode: IoMode) -> FerrosGrynnResult<()> {
	use ferros_grynn::{datastore::GqlSession, gql_status::FerrosGrynnError, values::Vector};

	use crate::test_assertions::{AssertComparison as _, AssertCondition as _};

	let directory = std::env::temp_dir().join(format!(
		"fg-live-find-{:016x}",
		Uuid::now_v7().as_inner().as_u128() & u128::from(u64::MAX)
	));
	let mut options = LocalOptions::default();
	options.io_mode = mode;
	options.buffer_pool_pages = 16;
	let store = Datastore::create_local(&directory, options)?;
	let mut session = store.session();
	let execute = |session: &mut GqlSession, text: &str, parameters: &Object| {
		let mut value = Value::None;
		for result in session.execute(text, parameters)? {
			value = result.result?.value().clone();
		}
		Ok::<_, ferros_grynn::gql_status::FerrosGrynnError>(value)
	};
	execute(
		&mut session,
		"DEFINE ANALYZER words TOKENIZERS blank FILTERS lowercase; DEFINE CLASS document TYPE \
		 NODE; DEFINE FIELD title ON CLASS document TYPE string; DEFINE FIELD embedding ON CLASS \
		 document TYPE vector<f32>;",
		&Object::new(),
	)?;
	for (name, title, vector) in [
		("a", "Rust graph", vec![1.0_f32, 0.0]),
		("b", "Rust storage", vec![0.0_f32, 1.0]),
		("c", "Graph only", vec![-1.0_f32, 0.0]),
	] {
		execute(
			&mut session,
			&format!("CREATE document:{name} SET title = $title, embedding = $vector;"),
			&Object::from_iter([
				("title", Value::from(title)),
				("vector", Value::Vector(Vector::F32(vector))),
			]),
		)?;
	}
	execute(
		&mut session,
		"DEFINE INDEX by_title ON CLASS document FIELDS title FULLTEXT ANALYZER words SCOPE BASE; \
		 DEFINE INDEX by_embedding ON CLASS document FIELDS embedding HNSW DIMENSION 2 TYPE F32 \
		 DIST EUCLIDEAN SCOPE BASE; LIVE LET $needle = 'rust'; LIVE LET $k = 1; LIVE LET $ef = 4; \
		 LIVE LET $query = $vector;",
		&Object::from_iter([("vector", Value::Vector(Vector::F32(vec![1.0, 0.0])))]),
	)?;
	let scan = "SELECT VALUE id FROM document WITH NOINDEX WHERE embedding <| $k |> $query;";
	let indexed = "SELECT VALUE id FROM document WITH INDEX by_embedding WHERE embedding <| $k, \
	               EF $ef |> $query;";
	for (needle, vector, first) in
		[("rust", vec![1.0, 0.0], "document:a"), ("graph", vec![-1.0, 0.0], "document:c")]
	{
		execute(
			&mut session,
			"ALTER LIVE LET $needle VALUE $text; ALTER LIVE LET $query VALUE $vector;",
			&Object::from_iter([
				("text", Value::from(needle)),
				("vector", Value::Vector(Vector::F32(vector))),
			]),
		)?;
		let fulltext =
			"SELECT VALUE id FROM document WITH NOINDEX WHERE title @@ $needle ORDER BY id;";
		let oracle = execute(&mut session, fulltext, &Object::new())?;
		(
			&(execute(
				&mut session,
				&fulltext.replace("WITH NOINDEX", "WITH INDEX by_title"),
				&Object::new(),
			)?),
			&(oracle),
		)
			.assert_equal();
		let expected = Value::from(vec![Value::from(first.parse::<RecordId>()?)]);
		(&(execute(&mut session, scan, &Object::new())?), &(expected)).assert_equal();
		(&(execute(&mut session, indexed, &Object::new())?), &(expected)).assert_equal();
	}
	execute(&mut session, "ALTER LIVE LET $ef VALUE 0;", &Object::new())?;
	let invalid = execute(&mut session, indexed, &Object::new())
		.err()
		.ok_or_else(|| FerrosGrynnError::invalid_argument("search fixture accepted zero EF"))?;
	(invalid.to_string().contains("positive integer")).assert_true_with(|| format!("{invalid}"));
	execute(
		&mut session,
		"ALTER LIVE LET $ef VALUE 1; ALTER LIVE LET $k VALUE 2;",
		&Object::new(),
	)?;
	let invalid = execute(&mut session, indexed, &Object::new())
		.err()
		.ok_or_else(|| FerrosGrynnError::invalid_argument("search fixture accepted EF below K"))?;
	(invalid.to_string().contains("EF must be at least K"))
		.assert_true_with(|| format!("{invalid}"));
	drop(session);
	drop(store);
	std::fs::remove_dir_all(&directory).map_err(|error| {
		FerrosGrynnError::invalid_argument("search fixture cleanup failed")
			.with_technical_source(error)
	})?;
	Ok(())
}

/// Verifies live gql search local sync.
#[test]
fn live_gql_search_local_sync() -> FerrosGrynnResult<()> {
	gql_search_snapshot_inputs(IoMode::Sync)
}

/// Verifies live gql search local native async.
#[cfg(feature = "native-async-io")]
#[test]
fn live_gql_search_local_native_async() -> FerrosGrynnResult<()> {
	gql_search_snapshot_inputs(IoMode::NativeAsync)
}

/// Exercises actual catalog publication, checkpoint, generation rebuild and
/// reopen.
fn registry_survives_reopen(mode: IoMode) -> FerrosGrynnResult<()> {
	use crate::test_assertions::AssertComparison as _;

	let directory = std::env::temp_dir().join(format!(
		"fg-live-{:016x}",
		Uuid::now_v7().as_inner().as_u128() & u128::from(u64::MAX)
	));
	let mut options = LocalOptions::default();
	options.io_mode = mode;
	options.buffer_pool_pages = 16;
	let store = Datastore::create_local(&directory, options)?;
	let mut tx = store.begin();
	let class = tx.define_class(ClassSchema::new("person", AtomKind::Node, [])?)?;
	let query = LiveQuery::new(LiveSource::Class(class));
	let id = tx.register_live(query.clone(), LiveOptions::default())?;
	let registration = tx.commit()?.versionstamp();
	store.checkpoint(CheckpointMode::Restart)?;
	drop(store);
	let store = Datastore::open_local(&directory, options)?;
	let info = store.begin().live_info(id)?;
	(&(info.state()), &(LiveState::Active)).assert_equal();
	(&(info.defined_at()), &(registration)).assert_equal();
	(&(info.query()), &(&query)).assert_equal();
	let mut schema = store.begin();
	schema.define_class(ClassSchema::new("other", AtomKind::Node, [])?)?;
	schema.commit()?;
	let mut replace = store.begin();
	let next = replace.reregister_live(id, query, LiveOptions::default())?;
	replace.commit()?;
	drop(store);
	let store = Datastore::open_local(&directory, options)?;
	(&(store.begin().live_info(id)?.state()), &(LiveState::Closed)).assert_equal();
	(&(store.begin().live_info(next)?.state()), &(LiveState::Active)).assert_equal();
	drop(store);
	std::fs::remove_dir_all(&directory).map_err(|error| {
		ferros_grynn::gql_status::FerrosGrynnError::invalid_argument(
			"test directory cleanup failed",
		)
		.with_technical_source(error)
	})?;
	Ok(())
}

/// Verifies live registry local sync.
#[test]
fn live_registry_local_sync() -> FerrosGrynnResult<()> {
	registry_survives_reopen(IoMode::Sync)
}

/// Public pull/ACK publication survives both checkpoint and generation rebuild.
fn subscription_ack_survives_reopen(mode: IoMode) -> FerrosGrynnResult<()> {
	use ferros_grynn::{gql_status::FerrosGrynnError, live::LiveNext};

	use crate::test_assertions::{AssertComparison as _, AssertCondition as _};
	let directory = std::env::temp_dir().join(format!(
		"fg-live-ack-{:016x}",
		Uuid::now_v7().as_inner().as_u128() & u128::from(u64::MAX)
	));
	let mut options = LocalOptions::default();
	options.io_mode = mode;
	options.buffer_pool_pages = 16;
	let store = Datastore::create_local(&directory, options)?;
	let mut setup = store.begin();
	let class = setup.define_class(ClassSchema::new("person", AtomKind::Node, [])?)?;
	setup.commit()?;
	let mut subscription =
		store.subscribe_live(LiveQuery::new(LiveSource::Class(class)), LiveOptions::default())?;
	let id = subscription.id();
	let mut insert = store.begin();
	insert.create_atom("person:a".parse()?, PropertyMap::new())?;
	let stamp = insert.commit()?.versionstamp();
	let LiveNext::Delivery(delivery) = subscription.next()? else {
		return Err(FerrosGrynnError::invalid_argument("missing local SDK delivery"));
	};
	(&(delivery.token().versionstamp()), &(stamp)).assert_equal();
	subscription.ack(delivery.token())?;
	(matches!(subscription.next()?, LiveNext::Idle)).assert_true();
	store.checkpoint(CheckpointMode::Restart)?;
	let mut generation = store.begin();
	generation.define_class(ClassSchema::new("other", AtomKind::Node, [])?)?;
	generation.commit()?;
	drop(subscription);
	drop(store);
	let store = Datastore::open_local(&directory, options)?;
	(&(store.begin().live_info(id)?.cursor()), &(delivery.token())).assert_equal();
	let mut close = store.begin();
	close.close_live(id)?;
	close.commit()?;
	drop(store);
	let store = Datastore::open_local(&directory, options)?;
	(&(store.begin().live_info(id)?.state()), &(LiveState::Closed)).assert_equal();
	(&(store.begin().live_info(id)?.cursor()), &(delivery.token())).assert_equal();
	drop(store);
	std::fs::remove_dir_all(&directory).map_err(|error| {
		FerrosGrynnError::invalid_argument("local SDK fixture cleanup failed")
			.with_technical_source(error)
	})?;
	Ok(())
}

/// Verifies live subscription ack local sync.
#[test]
fn live_subscription_ack_local_sync() -> FerrosGrynnResult<()> {
	subscription_ack_survives_reopen(IoMode::Sync)
}

/// Verifies live subscription ack local native async.
#[cfg(feature = "native-async-io")]
#[test]
fn live_subscription_ack_local_native_async() -> FerrosGrynnResult<()> {
	subscription_ack_survives_reopen(IoMode::NativeAsync)
}

/// Verifies live registry local native async.
#[cfg(feature = "native-async-io")]
#[test]
fn live_registry_local_native_async() -> FerrosGrynnResult<()> {
	registry_survives_reopen(IoMode::NativeAsync)
}

/// Exercises exact values, final-state reevaluation and durable fault heads.
fn bindings_survive_reopen(mode: IoMode) -> FerrosGrynnResult<()> {
	use crate::test_assertions::{AssertComparison as _, AssertCondition as _};

	let directory = std::env::temp_dir().join(format!(
		"fg-live-b-{:016x}",
		Uuid::now_v7().as_inner().as_u128() & u128::from(u64::MAX)
	));
	let mut options = LocalOptions::default();
	options.io_mode = mode;
	options.buffer_pool_pages = 16;
	let store = Datastore::create_local(&directory, options)?;
	let mut tx = store.begin();
	let class = tx.define_class(ClassSchema::new("person", AtomKind::Node, [])?)?;
	let record: RecordId = "person:a".parse()?;
	tx.create_atom(record.clone(), PropertyMap::new())?;
	let count = tx.define_live_binding(
		LiveExpr::Select(Box::new(LiveQuery::new(LiveSource::Class(class)).with_projection(
			LiveProjection::Value(LiveExpr::Call {
				name: "count".into(),
				arguments: Vec::new(),
			}),
		))),
		Object::new(),
		LiveBindingOptions::default(),
	)?;
	let record_count = tx.define_live_binding(
		LiveExpr::Select(Box::new(
			LiveQuery::new(LiveSource::Record(record.clone())).with_projection(
				LiveProjection::Value(LiveExpr::Call {
					name: "count".into(),
					arguments: Vec::new(),
				}),
			),
		)),
		Object::new(),
		LiveBindingOptions::default(),
	)?;
	let inverse = tx.define_live_binding(
		LiveExpr::Literal(Value::from(10_i64))
			.binary(LiveBinaryOperator::Divide, LiveExpr::Binding(count)),
		Object::new(),
		LiveBindingOptions::default(),
	)?;
	let exact = tx.define_live_binding(
		LiveExpr::Literal(Value::from(9_007_199_254_740_993_i64)),
		Object::new(),
		LiveBindingOptions::default(),
	)?;
	tx.commit()?;
	store.checkpoint(CheckpointMode::Restart)?;
	drop(store);
	let store = Datastore::open_local(&directory, options)?;
	(&(store.begin().live_binding_value(exact)?), &(Value::from(9_007_199_254_740_993_i64)))
		.assert_equal();
	(&(store.begin().live_binding_value(record_count)?), &(Value::from(1_i64))).assert_equal();
	let mut tx = store.begin();
	tx.delete_atom(&record)?;
	tx.commit()?;
	drop(store);
	let store = Datastore::open_local(&directory, options)?;
	let info = store.begin().live_binding_info(inverse)?;
	(&(info.state()), &(LiveState::Paused)).assert_equal();
	let diagnostic = info.diagnostic().ok_or_else(|| {
		ferros_grynn::gql_status::FerrosGrynnError::invalid_argument(
			"test expected a durable binding fault",
		)
	})?;
	(&(diagnostic.status().code().to_string()), &("FG-G1F09")).assert_equal();
	(store.begin().atom(&record)?.is_none()).assert_true();
	(&(store.begin().live_binding_value(count)?), &(Value::from(0_i64))).assert_equal();
	(&(store.begin().live_binding_value(record_count)?), &(Value::from(0_i64))).assert_equal();
	let mut tx = store.begin();
	// Schema publication builds a new generation and must preserve bindings.
	tx.define_class(ClassSchema::new("other", AtomKind::Node, [])?)?;
	tx.replace_live_binding_value(inverse, Value::from(3_i64))?;
	tx.commit()?;
	store.checkpoint(CheckpointMode::Restart)?;
	drop(store);
	let store = Datastore::open_local(&directory, options)?;
	(&(store.begin().live_binding_value(inverse)?), &(Value::from(3_i64))).assert_equal();
	(&(store.begin().live_binding_info(inverse)?.revision().definition().get()), &(2))
		.assert_equal();
	(&(store.begin().live_binding_value(exact)?), &(Value::from(9_007_199_254_740_993_i64)))
		.assert_equal();
	drop(store);
	std::fs::remove_dir_all(&directory).map_err(|error| {
		ferros_grynn::gql_status::FerrosGrynnError::invalid_argument(
			"test directory cleanup failed",
		)
		.with_technical_source(error)
	})?;
	Ok(())
}

/// Verifies live bindings local sync.
#[test]
fn live_bindings_local_sync() -> FerrosGrynnResult<()> {
	bindings_survive_reopen(IoMode::Sync)
}

/// Verifies live bindings local native async.
#[cfg(feature = "native-async-io")]
#[test]
fn live_bindings_local_native_async() -> FerrosGrynnResult<()> {
	bindings_survive_reopen(IoMode::NativeAsync)
}

/// Concrete PURE/source captures and query faults survive WAL and checkpoint.
fn query_captures_survive_reopen(mode: IoMode) -> FerrosGrynnResult<()> {
	use crate::test_assertions::{AssertComparison as _, AssertCondition as _};

	let directory = std::env::temp_dir().join(format!(
		"fg-live-q-{:016x}",
		Uuid::now_v7().as_inner().as_u128() & u128::from(u64::MAX)
	));
	let mut options = LocalOptions::default();
	options.io_mode = mode;
	options.buffer_pool_pages = 16;
	let store = Datastore::create_local(&directory, options)?;
	let mut tx = store.begin();
	let class = tx.define_class(ClassSchema::new("person", AtomKind::Node, [])?)?;
	tx.define_class(ClassSchema::new("policy", AtomKind::Node, [])?)?;
	tx.create_atom("person:a".parse()?, PropertyMap::new())?;
	let record: RecordId = "policy:a".parse()?;
	tx.create_atom(record.clone(), PropertyMap::new())?;
	// Existing function DDL supplies the fixture; LIVE uses only Rust builders.
	tx.execute(
		"DEFINE PURE FUNCTION fn::inverse(n: any) -> number { RETURN 10 / $n; };",
		&Object::new(),
	)?;
	let query = LiveQuery::new(LiveSource::Class(class)).with_projection(LiveProjection::Value(
		LiveExpr::Call {
			name: "fn::inverse".into(),
			arguments: vec![LiveExpr::Select(Box::new(
				LiveQuery::new(LiveSource::Record(record.clone())).with_projection(
					LiveProjection::Value(LiveExpr::Call {
						name: "count".into(),
						arguments: Vec::new(),
					}),
				),
			))],
		},
	));
	let id = tx.register_live(query.clone(), LiveOptions::default())?;
	tx.commit()?;
	store.checkpoint(CheckpointMode::Restart)?;
	drop(store);
	let store = Datastore::open_local(&directory, options)?;
	let mut old = store.begin();
	(&(old.live_info(id)?.state()), &(LiveState::Active)).assert_equal();
	(&(old.live_info(id)?.query()), &(&query)).assert_equal();
	let mut tx = store.begin();
	tx.delete_atom(&record)?;
	tx.commit()?;
	(store.begin().atom(&record)?.is_none()).assert_true();
	(&(old.live_info(id)?.state()), &(LiveState::Active)).assert_equal();
	drop(old);
	drop(store);
	let store = Datastore::open_local(&directory, options)?;
	let info = store.begin().live_info(id)?;
	(&(info.state()), &(LiveState::Paused)).assert_equal();
	(
		&(info.diagnostic().map(|error| error.status().code().to_string())),
		&(Some("FG-G1F09".into())),
	)
		.assert_equal();
	let mut tx = store.begin();
	tx.define_class(ClassSchema::new("generation", AtomKind::Node, [])?)?;
	tx.commit()?;
	store.checkpoint(CheckpointMode::Restart)?;
	drop(store);
	let store = Datastore::open_local(&directory, options)?;
	(&(store.begin().live_info(id)?.state()), &(LiveState::Paused)).assert_equal();
	drop(store);
	std::fs::remove_dir_all(&directory).map_err(|error| {
		ferros_grynn::gql_status::FerrosGrynnError::invalid_argument(
			"test directory cleanup failed",
		)
		.with_technical_source(error)
	})?;
	Ok(())
}

/// Verifies live query captures local sync.
#[test]
fn live_query_captures_local_sync() -> FerrosGrynnResult<()> {
	query_captures_survive_reopen(IoMode::Sync)
}

/// Verifies live query captures local native async.
#[cfg(feature = "native-async-io")]
#[test]
fn live_query_captures_local_native_async() -> FerrosGrynnResult<()> {
	query_captures_survive_reopen(IoMode::NativeAsync)
}

/// Deferred statistics after reopen do not force old queries through
/// registration.
fn registration_only_after_reopen(mode: IoMode) -> FerrosGrynnResult<()> {
	use ferros_grynn::gql_status::FerrosGrynnError;

	use crate::test_assertions::AssertComparison as _;
	let directory = std::env::temp_dir().join(format!(
		"fg-lreg-{:016x}",
		Uuid::now_v7().as_inner().as_u128() & u128::from(u64::MAX)
	));
	let mut options = LocalOptions::default();
	options.io_mode = mode;
	let store = Datastore::create_local(&directory, options)?;
	let mut setup = store.begin();
	let class =
		setup.define_class(ClassSchema::new("item", AtomKind::Node, [ClassField::new(
			"value",
			Type::Int,
		)])?)?;
	setup.commit()?;
	let key = store
		.property_dictionary()
		.get("value")
		.ok_or_else(|| FerrosGrynnError::invalid_argument("registration property missing"))?;
	let mut setup = store.begin();
	setup.create_atom("item:a".parse()?, PropertyMap::from_iter([(key, 1.into())]))?;
	setup.commit()?;
	let first =
		store.subscribe_live(LiveQuery::new(LiveSource::Class(class)), LiveOptions::default())?;
	let first_id = first.id();
	drop(first);
	store.checkpoint(CheckpointMode::Restart)?;
	drop(store);
	let store = Datastore::open_local(&directory, options)?;
	let attempts = store.live_statistics()?.query_evaluation_attempts;
	let second =
		store.subscribe_live(LiveQuery::new(LiveSource::Class(class)), LiveOptions::default())?;
	(&(store.live_statistics()?.query_evaluation_attempts - attempts), &(1)).assert_equal();
	(&(store.begin().live_info(first_id)?.state()), &(LiveState::Active)).assert_equal();
	let mut write = store.begin();
	write.set_base_property(&"item:a".parse()?, key, 2.into())?;
	write.commit()?;
	(
		&(store
			.begin()
			.atom(&"item:a".parse()?)?
			.and_then(|atom| atom.properties().get(key).cloned())),
		&(Some(2.into())),
	)
		.assert_equal();
	drop(second);
	drop(store);
	std::fs::remove_dir_all(&directory).map_err(|error| {
		FerrosGrynnError::invalid_argument("registration fixture cleanup failed")
			.with_technical_source(error)
	})?;
	Ok(())
}

/// Verifies live registration only local sync.
#[test]
fn live_registration_only_local_sync() -> FerrosGrynnResult<()> {
	registration_only_after_reopen(IoMode::Sync)
}

/// Verifies live registration only local native async.
#[cfg(feature = "native-async-io")]
#[test]
fn live_registration_only_local_native_async() -> FerrosGrynnResult<()> {
	registration_only_after_reopen(IoMode::NativeAsync)
}
