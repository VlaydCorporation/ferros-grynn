//! Historical projection evidence independent of current primary values.

use std::sync::Arc;

use super::rows;
use crate::{
	changefeed::{ChangefeedConfig, ChangefeedSource},
	datastore::{BindingAdmission, ClassSchema, Datastore},
	gql_status::FerrosGrynnResult,
	graph::AtomKind,
	live::{
		LiveBinaryOperator,
		LiveBindingOptions,
		LiveExpr,
		LiveOptions,
		LivePath,
		LiveProjection,
		LiveQuery,
		LiveSince,
		LiveSource,
	},
	values::{Object, PropertyMap, RecordId, Value},
};

/// Historical mode limits are validated before publishing a new incarnation.
#[test]
fn live_patch_history_admission_rejects_unrenderable_commit() {
	use crate::{
		datastore::ClassField,
		live::LiveOutputMode,
		test_assertions::AssertResult as _,
		values::Type,
	};
	let store = Datastore::in_memory();
	let mut setup = store.begin();
	let class = setup
		.define_class(
			ClassSchema::new("item", AtomKind::Node, [ClassField::new("text", Type::String)])
				.assert_ok(),
		)
		.assert_ok();
	let text = setup.intern_property("text").assert_ok();
	setup
		.configure_changefeed(
			ChangefeedSource::Class(class),
			ChangefeedConfig::new("P1D".parse().assert_ok()).assert_ok(),
		)
		.assert_ok();
	let floor = setup.commit().assert_ok().versionstamp();
	let record: RecordId = "item:a".parse().assert_ok();
	let mut create = store.begin();
	create
		.create_atom(
			record.clone(),
			PropertyMap::from_iter([(text, Value::from("x".repeat(16 * 1024)))]),
		)
		.assert_ok();
	create.commit().assert_ok();
	let mut remove = store.begin();
	remove.delete_atom(&record).assert_ok();
	remove.commit().assert_ok();
	let query = LiveQuery::new(LiveSource::Class(class)).with_output(LiveOutputMode::Patch);
	let options = LiveOptions::default().with_budgets(256 * 1024 * 1024, 4096).assert_ok();
	let mut now = store.subscribe_live(query.clone(), options.clone()).assert_ok();
	now.close().assert_ok();
	let before = store.inner.current_root().commit_ts;
	assert!(
		store.subscribe_live(query, options.with_since(LiveSince::Versionstamp(floor))).is_err()
	);
	assert_eq!(
		store.inner.current_root().commit_ts,
		before,
		"rejected historical registration is unpublished"
	);
}

/// SPLIT windows need complete captured domains, including unchanged members.
#[test]
fn live_window_split_history_matches_whole_domain() {
	use crate::{
		datastore::ClassField,
		live::{LiveIdentity, LiveNext, LiveOrder, LivePayload},
		test_assertions::AssertResult as _,
		values::Type,
	};
	let store = Datastore::in_memory();
	let mut setup = store.begin();
	let class = setup
		.define_class(
			ClassSchema::new("item", AtomKind::Node, [ClassField::new(
				"tags",
				Type::Array(Box::new(Type::Int)),
			)])
			.assert_ok(),
		)
		.assert_ok();
	let tags = setup.intern_property("tags").assert_ok();
	setup
		.configure_changefeed(
			ChangefeedSource::Class(class),
			ChangefeedConfig::new("P1D".parse().assert_ok()).assert_ok(),
		)
		.assert_ok();
	let a: RecordId = "item:a".parse().assert_ok();
	setup
		.create_atom(
			a.clone(),
			PropertyMap::from_iter([(tags, Value::from(vec![Value::from(7), Value::from(7)]))]),
		)
		.assert_ok();
	setup
		.create_atom(
			"item:b".parse().assert_ok(),
			PropertyMap::from_iter([(tags, Value::from(vec![Value::from(9)]))]),
		)
		.assert_ok();
	let floor = setup.commit().assert_ok().versionstamp();
	let query = LiveQuery::new(LiveSource::Class(class))
		.with_split(vec![LiveExpr::field("tags").assert_ok()])
		.with_projection(LiveProjection::Value(LiveExpr::field("tags").assert_ok()))
		.with_order(vec![LiveOrder::new(LiveExpr::field("tags").assert_ok())])
		.with_limit(LiveExpr::Literal(2.into()));
	let root = store.inner.current_root();
	let schema = root
		.schema
		.class_by_id(class)
		.ok_or_else(|| {
			crate::gql_status::FerrosGrynnError::invalid_argument("SPLIT history class missing")
		})
		.assert_ok();
	let mut budget = BindingAdmission::new(&LiveBindingOptions::default());
	assert!(!rows::eligible(&query, &schema, &mut budget).assert_ok());
	drop(root);
	let mut active = store.subscribe_live(query.clone(), LiveOptions::default()).assert_ok();
	let mut expected = Vec::new();
	for values in [vec![Value::from(8), Value::from(7), Value::from(7)], Vec::new()] {
		let mut tx = store.begin();
		tx.set_base_property(&a, tags, Value::from(values)).assert_ok();
		let stamp = tx.commit().assert_ok().versionstamp();
		let LiveNext::Delivery(delivery) = active.next().assert_ok() else {
			return {
				let test_result: FerrosGrynnResult<()> =
					Err(crate::gql_status::FerrosGrynnError::invalid_argument(
						"SPLIT domain delivery missing",
					));
				test_result.assert_ok();
			};
		};
		let LivePayload::Batch(batch) = delivery.payload() else {
			return {
				let test_result: FerrosGrynnResult<()> =
					Err(crate::gql_status::FerrosGrynnError::invalid_argument(
						"SPLIT domain unexpected GAP",
					));
				test_result.assert_ok();
			};
		};
		assert_eq!(batch.versionstamp(), stamp);
		assert!(
			batch
				.events()
				.iter()
				.all(|event| matches!(event.identity(), LiveIdentity::Composite(_)))
		);
		expected.push((stamp, batch.events().to_vec()));
		active.ack(delivery.token()).assert_ok();
	}
	let mut replay = store
		.subscribe_live(query, LiveOptions::default().with_since(LiveSince::Versionstamp(floor)))
		.assert_ok();
	for (stamp, events) in expected {
		let LiveNext::Delivery(delivery) = replay.next().assert_ok() else {
			return {
				let test_result: FerrosGrynnResult<()> =
					Err(crate::gql_status::FerrosGrynnError::invalid_argument(
						"SPLIT domain replay missing",
					));
				test_result.assert_ok();
			};
		};
		assert!(
			matches!(delivery.payload(), LivePayload::Batch(batch) if batch.versionstamp() == stamp && batch.events() == events)
		);
		replay.ack(delivery.token()).assert_ok();
	}
	assert!(matches!(replay.next().assert_ok(), LiveNext::Idle));
	active.close().assert_ok();
	replay.close().assert_ok();
}

/// A late capture policy cannot reconstruct preexisting SPLIT window members.
#[test]
fn live_window_split_missing_creation_anchor_is_gap() {
	use crate::{
		datastore::ClassField,
		live::{LiveGapReason, LiveNext, LivePayload},
		test_assertions::AssertResult as _,
		values::Type,
	};
	let store = Datastore::in_memory();
	let mut setup = store.begin();
	let class = setup
		.define_class(
			ClassSchema::new("item", AtomKind::Node, [ClassField::new(
				"tags",
				Type::Array(Box::new(Type::Int)),
			)])
			.assert_ok(),
		)
		.assert_ok();
	let tags = setup.intern_property("tags").assert_ok();
	setup
		.create_atom(
			"item:old".parse().assert_ok(),
			PropertyMap::from_iter([(tags, Value::from(vec![Value::from(7)]))]),
		)
		.assert_ok();
	let floor = setup.commit().assert_ok().versionstamp();
	let mut tx = store.begin();
	tx.configure_changefeed(
		ChangefeedSource::Class(class),
		ChangefeedConfig::new("P1D".parse().assert_ok()).assert_ok(),
	)
	.assert_ok();
	tx.commit().assert_ok();
	let mut tx = store.begin();
	tx.create_atom(
		"item:new".parse().assert_ok(),
		PropertyMap::from_iter([(tags, Value::from(vec![Value::from(8)]))]),
	)
	.assert_ok();
	tx.commit().assert_ok();
	let query = LiveQuery::new(LiveSource::Class(class))
		.with_split(vec![LiveExpr::field("tags").assert_ok()]);
	let mut replay = store
		.subscribe_live(query, LiveOptions::default().with_since(LiveSince::Versionstamp(floor)))
		.assert_ok();
	let LiveNext::Delivery(delivery) = replay.next().assert_ok() else {
		return {
			let test_result: FerrosGrynnResult<()> = Err(
				crate::gql_status::FerrosGrynnError::invalid_argument("SPLIT anchor GAP missing"),
			);
			test_result.assert_ok();
		};
	};
	assert!(
		matches!(delivery.payload(), LivePayload::Gap(gap) if gap.reason() == LiveGapReason::InsufficientCoverage)
	);
	replay.ack(delivery.token()).assert_ok();
	replay.close().assert_ok();
}

/// WHERE transitions and frozen inputs use saved images even after later DML.
#[test]
fn live_backfill_row_images_use_exact_select_and_never_current_primary() {
	use crate::test_assertions::AssertResult as _;

	let store = Datastore::in_memory();
	let mut setup = store.begin();
	let class = setup
		.define_class(
			ClassSchema::new("person", AtomKind::Node, [crate::datastore::ClassField::new(
				"age",
				crate::values::Type::Int,
			)])
			.assert_ok(),
		)
		.assert_ok();
	let key = setup.intern_property("age").assert_ok();
	let record: RecordId = "person:a".parse().assert_ok();
	setup
		.configure_changefeed(
			ChangefeedSource::Class(class),
			ChangefeedConfig::new("P1D".parse().assert_ok()).assert_ok(),
		)
		.assert_ok();
	setup.create_atom(record.clone(), PropertyMap::from_iter([(key, Value::from(17))])).assert_ok();
	let query = LiveQuery::new(LiveSource::Class(class))
		.with_parameters(Object::from_iter([("minimum", Value::from(18))]))
		.with_filter(
			LiveExpr::field("age")
				.assert_ok()
				.binary(LiveBinaryOperator::GreaterOrEqual, LiveExpr::Parameter("minimum".into())),
		)
		.with_projection(LiveProjection::Value(LiveExpr::field("age").assert_ok()));
	let id = setup.register_live(query.clone(), LiveOptions::default()).assert_ok();
	setup.commit().assert_ok();
	let feed = store
		.inner
		.current_root()
		.changefeeds()
		.definitions
		.values()
		.next()
		.ok_or_else(|| crate::gql_status::FerrosGrynnError::invalid_argument("fixture feed"))
		.assert_ok()
		.id;
	let mut update = store.begin();
	update.set_base_property(&record, key, 19.into()).assert_ok();
	let stamp = update.commit().assert_ok().versionstamp().get();
	let saved = store.inner.current_root().change_history()[&(feed, stamp)].clone();
	let mut update = store.begin();
	update.set_base_property(&record, key, 99.into()).assert_ok();
	update.commit().assert_ok();
	let root = store.inner.current_root();
	let definition = &root.live_catalog().queries[&id];
	let class = root
		.schema
		.class_by_id(class)
		.ok_or_else(|| crate::gql_status::FerrosGrynnError::invalid_argument("fixture class"))
		.assert_ok();
	let mut budget = BindingAdmission::new(&LiveBindingOptions::default());
	assert!(rows::eligible(&query, &class, &mut budget).assert_ok());
	let before = rows::evaluate(
		&root,
		definition,
		&saved.net,
		rows::ImageSelection::BeforeChanges,
		std::env::temp_dir(),
		&mut budget,
	)
	.assert_ok();
	let after = rows::evaluate(
		&root,
		definition,
		&saved.net,
		rows::ImageSelection::AfterChanges,
		std::env::temp_dir(),
		&mut budget,
	)
	.assert_ok();
	assert!(before.rows.is_empty());
	assert_eq!(after.rows.values().collect::<Vec<_>>(), vec![&Value::from(19)]);
	let mut malformed = saved.net.clone();
	Arc::make_mut(
		malformed.entries[0]
			.after
			.as_mut()
			.ok_or_else(|| crate::gql_status::FerrosGrynnError::invalid_argument("fixture image"))
			.assert_ok(),
	)
	.label = Some("foreign".into());
	assert!(
		rows::evaluate(
			&root,
			definition,
			&malformed,
			rows::ImageSelection::AfterChanges,
			std::env::temp_dir(),
			&mut budget
		)
		.is_err()
	);
	let mut tiny =
		BindingAdmission::new(&LiveBindingOptions::default().with_budgets(1, 1_000).assert_ok());
	assert!(
		rows::evaluate(
			&root,
			definition,
			&saved.net,
			rows::ImageSelection::AfterChanges,
			std::env::temp_dir(),
			&mut tiny
		)
		.is_err()
	);
}

/// Optional class qualification is safe; foreign navigation needs coverage.
#[test]
fn live_backfill_eligibility_distinguishes_properties_from_record_navigation() {
	use crate::test_assertions::AssertResult as _;

	let class = ClassSchema::new("person", AtomKind::Node, []).assert_ok();
	let mut setup = Datastore::in_memory().begin();
	let id = setup.define_class(class.clone()).assert_ok();
	for (path, eligible) in [
		(vec!["age"], true),
		(vec!["PERSON", "age"], true),
		(vec!["properties", "manager"], true),
		(vec!["person", "properties", "age"], true),
		(vec!["manager", "age"], false),
		(vec!["properties", "manager", "age"], false),
		(vec!["P", "age"], true),
		(vec!["p", "properties", "age"], true),
		(vec!["p", "manager", "age"], false),
		(vec!["p", "properties", "manager", "age"], false),
	] {
		let query = LiveQuery::new(LiveSource::Class(id))
			.with_alias("p")
			.assert_ok()
			.with_projection(LiveProjection::Value(LiveExpr::Field(
				LivePath::new(path.into_iter().map(Into::into).collect()).assert_ok(),
			)));
		assert_eq!(
			rows::eligible(
				&query,
				&class,
				&mut BindingAdmission::new(&LiveBindingOptions::default())
			)
			.assert_ok(),
			eligible
		);
	}
	let class = ClassSchema::new("person", AtomKind::Node, [crate::datastore::ClassField::new(
		"person",
		crate::values::Type::Object,
	)])
	.assert_ok();
	let query = LiveQuery::new(LiveSource::Class(id)).with_projection(LiveProjection::Value(
		LiveExpr::Field(LivePath::new(vec!["person".into(), "age".into()]).assert_ok()),
	));
	assert!(
		!rows::eligible(&query, &class, &mut BindingAdmission::new(&LiveBindingOptions::default()))
			.assert_ok()
	);
	assert!(
		rows::eligible(
			&query.with_alias("person").assert_ok(),
			&class,
			&mut BindingAdmission::new(&LiveBindingOptions::default())
		)
		.assert_ok()
	);
}

/// A qualifier does not require a creation anchor for exact changed-row replay.
#[test]
fn live_backfill_source_alias_keeps_changed_row_coverage() {
	use crate::{gql_status::FerrosGrynnError, test_assertions::AssertResult as _};

	let store = Datastore::in_memory();
	let mut setup = store.begin();
	let class =
		setup.define_class(ClassSchema::new("item", AtomKind::Node, []).assert_ok()).assert_ok();
	let key = setup.intern_property("value").assert_ok();
	let record: RecordId = "item:a".parse().assert_ok();
	setup.create_atom(record.clone(), PropertyMap::from_iter([(key, Value::from(1))])).assert_ok();
	setup.commit().assert_ok();
	let mut capture = store.begin();
	capture
		.configure_changefeed(
			ChangefeedSource::Class(class),
			ChangefeedConfig::new("P1D".parse().assert_ok()).assert_ok(),
		)
		.assert_ok();
	let floor = capture.commit().assert_ok().versionstamp();
	let mut update = store.begin();
	update.set_base_property(&record, key, 2.into()).assert_ok();
	let stamp = update.commit().assert_ok().versionstamp();
	let query = LiveQuery::new(LiveSource::Class(class))
		.with_alias("p")
		.assert_ok()
		.with_projection(LiveProjection::Value(LiveExpr::Field(
			LivePath::new(vec!["P".into(), "properties".into(), "value".into()]).assert_ok(),
		)));
	let mut subscription = store
		.subscribe_live(query, LiveOptions::default().with_since(LiveSince::Versionstamp(floor)))
		.assert_ok();
	let crate::live::LiveNext::Delivery(delivery) = subscription.next().assert_ok() else {
		return {
			let test_result: FerrosGrynnResult<()> =
				Err(FerrosGrynnError::invalid_argument("qualified historical delivery missing"));
			test_result.assert_ok();
		};
	};
	let crate::live::LivePayload::Batch(batch) = delivery.payload() else {
		return {
			let test_result: FerrosGrynnResult<()> =
				Err(FerrosGrynnError::invalid_argument("qualifier lost changed-row coverage"));
			test_result.assert_ok();
		};
	};
	assert_eq!(batch.versionstamp(), stamp);
	assert_eq!(batch.events().len(), 1);
	assert_eq!(batch.events()[0].before(), Some(&Value::from(1)));
	assert_eq!(batch.events()[0].after(), Some(&Value::from(2)));
	subscription.ack(delivery.token()).assert_ok();
	assert!(matches!(subscription.next().assert_ok(), crate::live::LiveNext::Idle));
}

/// Invalid historical floors cannot publish a volatile RAM definition either.
#[test]
fn live_backfill_future_floor_rejects_without_changing_ram_root() {
	use crate::test_assertions::AssertResult as _;

	let store = Datastore::in_memory();
	let mut setup = store.begin();
	let class =
		setup.define_class(ClassSchema::new("person", AtomKind::Node, []).assert_ok()).assert_ok();
	setup.commit().assert_ok();
	let old = store.inner.current_root();
	let mut tx = store.begin();
	tx.register_live(
		LiveQuery::new(LiveSource::Class(class)),
		LiveOptions::default().with_since(LiveSince::Versionstamp((old.commit_ts + 100).into())),
	)
	.assert_ok();
	assert!(tx.commit().is_err());
	assert!(Arc::ptr_eq(&old, &store.inner.current_root()));
}

/// A new incarnation stores original clocks and resumes after its historical
/// ACK floor, then continues with the ordinary live tail.
#[test]
fn live_backfill_transaction_registration_catches_up_whole_commits() {
	use crate::test_assertions::AssertResult as _;

	let store = Datastore::in_memory();
	let mut setup = store.begin();
	let class =
		setup.define_class(ClassSchema::new("item", AtomKind::Node, []).assert_ok()).assert_ok();
	let key = setup.intern_property("value").assert_ok();
	let record: RecordId = "item:a".parse().assert_ok();
	setup
		.configure_changefeed(
			ChangefeedSource::Class(class),
			ChangefeedConfig::new("P1D".parse().assert_ok()).assert_ok(),
		)
		.assert_ok();
	setup.create_atom(record.clone(), PropertyMap::from_iter([(key, Value::from(1))])).assert_ok();
	let starting = setup.commit().assert_ok().versionstamp();
	let mut stamps = Vec::new();
	for value in [2, 3] {
		let mut update = store.begin();
		update.set_base_property(&record, key, Value::from(value)).assert_ok();
		stamps.push(update.commit().assert_ok().versionstamp());
	}
	let mut registration = store.begin();
	let query = LiveQuery::new(LiveSource::Class(class)).with_projection(LiveProjection::Value(
		LiveExpr::Field(LivePath::new(vec!["properties".into(), "value".into()]).assert_ok()),
	));
	let id = registration
		.register_live(query, LiveOptions::default().with_since(LiveSince::Versionstamp(starting)))
		.assert_ok();
	let registered = registration.commit().assert_ok().versionstamp();
	let saved = store.begin().live_info(id).assert_ok().cursor().clone();
	assert_eq!(saved.versionstamp(), starting);
	assert!(registered > saved.versionstamp());
	let mut subscription = store.resume_live(&saved).assert_ok();
	let mut tail = store.begin();
	tail.set_base_property(&record, key, 4.into()).assert_ok();
	stamps.push(tail.commit().assert_ok().versionstamp());
	for (ordinal, stamp) in stamps.into_iter().enumerate() {
		let crate::live::LiveNext::Delivery(delivery) = subscription.next().assert_ok() else {
			return {
				let test_result: FerrosGrynnResult<()> =
					Err(crate::gql_status::FerrosGrynnError::invalid_argument(
						"missing historical delivery",
					));
				test_result.assert_ok();
			};
		};
		let crate::live::LivePayload::Batch(batch) = delivery.payload() else {
			return {
				let test_result: FerrosGrynnResult<()> =
					Err(crate::gql_status::FerrosGrynnError::invalid_argument(
						"unexpected historical GAP",
					));
				test_result.assert_ok();
			};
		};
		assert_eq!(batch.versionstamp(), stamp);
		assert_eq!(batch.events()[0].before(), Some(&Value::from(ordinal as i64 + 1)));
		assert_eq!(batch.events()[0].after(), Some(&Value::from(ordinal as i64 + 2)));
		subscription.ack(delivery.token()).assert_ok();
	}
	assert!(matches!(subscription.next().assert_ok(), crate::live::LiveNext::Idle));
}

/// Common source fixture has no active LIVE evaluation while history grows.
fn source_fixture() -> FerrosGrynnResult<(
	Datastore,
	crate::datastore::ClassId,
	RecordId,
	crate::values::PropertyKeyId,
	crate::changefeed::Versionstamp,
)> {
	let store = Datastore::in_memory();
	let mut setup = store.begin();
	let class = setup.define_class(ClassSchema::new("item", AtomKind::Node, [])?)?;
	let key = setup.intern_property("value")?;
	let record: RecordId = "item:a".parse()?;
	setup.configure_changefeed(
		ChangefeedSource::Class(class),
		ChangefeedConfig::new("P1D".parse()?)?,
	)?;
	setup.create_atom(record.clone(), PropertyMap::from_iter([(key, 1.into())]))?;
	let floor = setup.commit()?.versionstamp();
	Ok((store, class, record, key, floor))
}

/// Every saved image precedes a concurrently appended tail; ACK is whole
/// commit.
#[test]
fn live_backfill_sdk_registration_catches_up_and_preserves_issued_delivery() {
	use crate::test_assertions::AssertResult as _;

	let (store, class, record, key, floor) = source_fixture().assert_ok();
	let mut stamps = Vec::new();
	for value in [2, 3] {
		let mut update = store.begin();
		update.set_base_property(&record, key, value.into()).assert_ok();
		stamps.push(update.commit().assert_ok().versionstamp());
	}
	let query = LiveQuery::new(LiveSource::Class(class)).with_projection(LiveProjection::Value(
		LiveExpr::Field(LivePath::new(vec!["properties".into(), "value".into()]).assert_ok()),
	));
	let mut subscription = store
		.subscribe_live(query, LiveOptions::default().with_since(LiveSince::Versionstamp(floor)))
		.assert_ok();
	assert_eq!(subscription.cursor().versionstamp(), floor);
	let mut tail = store.begin();
	tail.set_base_property(&record, key, 4.into()).assert_ok();
	stamps.push(tail.commit().assert_ok().versionstamp());
	for (ordinal, stamp) in stamps.into_iter().enumerate() {
		let crate::live::LiveNext::Delivery(delivery) = subscription.next().assert_ok() else {
			return {
				let test_result: FerrosGrynnResult<()> = Err(
					crate::gql_status::FerrosGrynnError::invalid_argument("SDK catch-up missing"),
				);
				test_result.assert_ok();
			};
		};
		let crate::live::LivePayload::Batch(batch) = delivery.payload() else {
			return {
				let test_result: FerrosGrynnResult<()> =
					Err(crate::gql_status::FerrosGrynnError::invalid_argument("SDK catch-up GAP"));
				test_result.assert_ok();
			};
		};
		assert_eq!(batch.versionstamp(), stamp);
		assert_eq!(batch.events()[0].before(), Some(&Value::from(ordinal as i64 + 1)));
		assert_eq!(batch.events()[0].after(), Some(&Value::from(ordinal as i64 + 2)));
		let crate::live::LiveNext::Delivery(repeated) = subscription.next().assert_ok() else {
			return {
				let test_result: FerrosGrynnResult<()> = Err(
					crate::gql_status::FerrosGrynnError::invalid_argument("SDK repeat missing"),
				);
				test_result.assert_ok();
			};
		};
		assert_eq!(repeated.token(), delivery.token());
		subscription.ack(delivery.token()).assert_ok();
	}
	assert!(matches!(subscription.next().assert_ok(), crate::live::LiveNext::Idle));
}

/// An OFF interval cannot disappear when timestamp selection finds a later
/// batch.
#[test]
fn live_backfill_timestamp_keeps_capture_disabled_prefix() {
	use crate::test_assertions::AssertResult as _;

	let (store, class, record, key, _) = source_fixture().assert_ok();
	let mut stop = store.begin();
	stop.drop_changefeed(ChangefeedSource::Class(class)).assert_ok();
	let stopped = stop.commit().assert_ok().versionstamp();
	let mut missing = store.begin();
	missing.set_base_property(&record, key, 2.into()).assert_ok();
	let unavailable = missing.commit().assert_ok().versionstamp();
	let requested = crate::values::DateTime::from(
		jiff::Timestamp::from_nanosecond(store.inner.current_root().committed_at_ns)
			.assert_ok()
			.to_zoned(jiff::tz::TimeZone::UTC),
	);
	let mut enable = store.begin();
	enable
		.configure_changefeed(
			ChangefeedSource::Class(class),
			ChangefeedConfig::new("P1D".parse().assert_ok()).assert_ok(),
		)
		.assert_ok();
	enable.commit().assert_ok();
	let mut update = store.begin();
	update.set_base_property(&record, key, 3.into()).assert_ok();
	let available = update.commit().assert_ok().versionstamp();
	let mut subscription = store
		.subscribe_live(
			LiveQuery::new(LiveSource::Class(class)),
			LiveOptions::default().with_since(LiveSince::Timestamp(requested)),
		)
		.assert_ok();
	let crate::live::LiveNext::Delivery(delivery) = subscription.next().assert_ok() else {
		return {
			let test_result: FerrosGrynnResult<()> =
				Err(crate::gql_status::FerrosGrynnError::invalid_argument("timestamp GAP missing"));
			test_result.assert_ok();
		};
	};
	let crate::live::LivePayload::Gap(gap) = delivery.payload() else {
		return {
			let test_result: FerrosGrynnResult<()> = Err(
				crate::gql_status::FerrosGrynnError::invalid_argument("timestamp prefix erased"),
			);
			test_result.assert_ok();
		};
	};
	assert!(gap.from().is_some_and(|from| from <= stopped));
	assert!(gap.through().is_some_and(|through| through >= unavailable && through < available));
	subscription.ack(delivery.token()).assert_ok();
	let crate::live::LiveNext::Delivery(delivery) = subscription.next().assert_ok() else {
		return {
			let test_result: FerrosGrynnResult<()> = Err(
				crate::gql_status::FerrosGrynnError::invalid_argument("timestamp tail missing"),
			);
			test_result.assert_ok();
		};
	};
	assert!(matches!(delivery.payload(), crate::live::LivePayload::Batch(batch)
		if batch.versionstamp() == available));
}

/// Unproven external dependencies require GAP even with no source batches and
/// when the final capture policy is OFF.
#[test]
fn live_backfill_dependency_preflight_covers_empty_and_disabled_sources() {
	use crate::test_assertions::AssertResult as _;

	for stopped in [false, true] {
		let (store, class, record, key, floor) = source_fixture().assert_ok();
		let mut update = store.begin();
		if stopped {
			update.set_base_property(&record, key, 2.into()).assert_ok();
			update.commit().assert_ok();
			let mut stop = store.begin();
			stop.drop_changefeed(ChangefeedSource::Class(class)).assert_ok();
			stop.commit().assert_ok();
		} else {
			update
				.define_class(ClassSchema::new("unrelated", AtomKind::Node, []).assert_ok())
				.assert_ok();
			update.commit().assert_ok();
		}
		let cutoff = store.inner.current_root().commit_ts;
		let query = LiveQuery::new(LiveSource::Class(class))
			.with_projection(LiveProjection::Fields(vec![
				crate::live::LiveField::new(LiveExpr::Literal(Value::from(
					"foreign:missing".parse::<RecordId>().assert_ok(),
				)))
				.with_alias(LivePath::field("manager").assert_ok()),
			]))
			.with_fetch(vec![LivePath::field("manager").assert_ok()]);
		let mut subscription = store
			.subscribe_live(
				query,
				LiveOptions::default().with_since(LiveSince::Versionstamp(floor)),
			)
			.assert_ok();
		let crate::live::LiveNext::Delivery(delivery) = subscription.next().assert_ok() else {
			return {
				let test_result: FerrosGrynnResult<()> = Err(
					crate::gql_status::FerrosGrynnError::invalid_argument("dependency GAP missing"),
				);
				test_result.assert_ok();
			};
		};
		assert!(matches!(delivery.payload(), crate::live::LivePayload::Gap(gap)
			if gap.reason() == crate::live::LiveGapReason::InsufficientCoverage
			&& gap.from() == Some((floor.get() + 1).into())
			&& gap.through().is_some_and(|through| through.get() >= cutoff)));
	}
}

/// Many small commits require linear retained capacity, with reusable SQL
/// scratch.
#[test]
fn live_backfill_many_groups_fit_small_linear_budget() {
	use crate::test_assertions::AssertResult as _;

	let (store, class, record, key, floor) = source_fixture().assert_ok();
	for value in 2..=513 {
		let mut update = store.begin();
		update.set_base_property(&record, key, value.into()).assert_ok();
		update.commit().assert_ok();
	}
	let mut subscription = store
		.subscribe_live(
			LiveQuery::new(LiveSource::Class(class)),
			LiveOptions::default()
				.with_budgets(8 * 1024 * 1024, 1_000_000)
				.assert_ok()
				.with_queue(10, 1024 * 1024)
				.assert_ok()
				.with_since(LiveSince::Versionstamp(floor)),
		)
		.assert_ok();
	for _ in 0..512 {
		let crate::live::LiveNext::Delivery(delivery) = subscription.next().assert_ok() else {
			return {
				let test_result: FerrosGrynnResult<()> = Err(
					crate::gql_status::FerrosGrynnError::invalid_argument("small history missing"),
				);
				test_result.assert_ok();
			};
		};
		assert!(matches!(delivery.payload(), crate::live::LivePayload::Batch(_)));
		subscription.ack(delivery.token()).assert_ok();
	}
	assert!(matches!(subscription.next().assert_ok(), crate::live::LiveNext::Idle));
}

/// A future timestamp waits across ordinary DML, then starts on its exact
/// boundary.
#[test]
fn live_backfill_future_timestamp_suppresses_early_tail() {
	use crate::test_assertions::AssertResult as _;

	let (store, class, record, key, _) = source_fixture().assert_ok();
	let future = store.inner.current_root().committed_at_ns + 86_400_000_000_000;
	let since = crate::values::DateTime::from(
		jiff::Timestamp::from_nanosecond(future).assert_ok().to_zoned(jiff::tz::TimeZone::UTC),
	);
	let mut subscription = store
		.subscribe_live(
			LiveQuery::new(LiveSource::Class(class)),
			LiveOptions::default().with_since(LiveSince::Timestamp(since)),
		)
		.assert_ok();
	let mut early = store.begin();
	early.set_base_property(&record, key, 2.into()).assert_ok();
	early.commit().assert_ok();
	assert!(matches!(subscription.next().assert_ok(), crate::live::LiveNext::Idle));
	{
		let mut root = store.inner.current.write();
		Arc::make_mut(&mut root).committed_at_ns = future;
	}
	let mut boundary = store.begin();
	boundary.set_base_property(&record, key, 3.into()).assert_ok();
	let stamp = boundary.commit().assert_ok().versionstamp();
	let crate::live::LiveNext::Delivery(delivery) = subscription.next().assert_ok() else {
		return {
			let test_result: FerrosGrynnResult<()> = Err(
				crate::gql_status::FerrosGrynnError::invalid_argument("timestamp boundary missing"),
			);
			test_result.assert_ok();
		};
	};
	assert!(matches!(delivery.payload(), crate::live::LivePayload::Batch(batch)
		if batch.versionstamp() == stamp));
}

/// SINCE includes net DML in its registration commit; NOW still has no initial
/// set.
#[test]
fn live_backfill_registration_commit_dml_is_not_skipped() {
	use crate::test_assertions::AssertResult as _;

	let (store, class, record, key, floor) = source_fixture().assert_ok();
	let mut registration = store.begin();
	let id = registration
		.register_live(
			LiveQuery::new(LiveSource::Class(class)),
			LiveOptions::default().with_since(LiveSince::Versionstamp(floor)),
		)
		.assert_ok();
	registration.set_base_property(&record, key, 2.into()).assert_ok();
	let stamp = registration.commit().assert_ok().versionstamp();
	let cursor = store.begin().live_info(id).assert_ok().cursor().clone();
	let mut subscription = store.resume_live(&cursor).assert_ok();
	let crate::live::LiveNext::Delivery(delivery) = subscription.next().assert_ok() else {
		return {
			let test_result: FerrosGrynnResult<()> = Err(
				crate::gql_status::FerrosGrynnError::invalid_argument("registration DML missing"),
			);
			test_result.assert_ok();
		};
	};
	assert!(matches!(delivery.payload(), crate::live::LivePayload::Batch(batch)
		if batch.versionstamp() == stamp && batch.events()[0].before().is_some()));
}

/// Before a future timestamp, a volatile incarnation has no lost interval to
/// replay merely because unrelated commits advanced the global clock.
#[test]
fn live_backfill_volatile_future_timestamp_resume_is_idle() {
	use crate::test_assertions::AssertResult as _;

	let (store, class, record, key, _) = source_fixture().assert_ok();
	let mut stop = store.begin();
	stop.drop_changefeed(ChangefeedSource::Class(class)).assert_ok();
	stop.commit().assert_ok();
	let future = store.inner.current_root().committed_at_ns + 86_400_000_000_000;
	let since = crate::values::DateTime::from(
		jiff::Timestamp::from_nanosecond(future).assert_ok().to_zoned(jiff::tz::TimeZone::UTC),
	);
	let subscription = store
		.subscribe_live(
			LiveQuery::new(LiveSource::Class(class)),
			LiveOptions::default().with_since(LiveSince::Timestamp(since)),
		)
		.assert_ok();
	let saved = subscription.cursor().clone();
	drop(subscription);
	let mut early = store.begin();
	early.set_base_property(&record, key, 2.into()).assert_ok();
	early.commit().assert_ok();
	let mut resumed = store.resume_live(&saved).assert_ok();
	assert!(matches!(resumed.next().assert_ok(), crate::live::LiveNext::Idle));
	assert_eq!(resumed.cursor(), &saved);
}

/// COUNT and SUM include unchanged members, preserve one aggregate identity,
/// and restore the empty-domain result through the ordinary SELECT executor.
#[test]
fn live_backfill_aggregate_complete_domain_includes_unchanged_rows_and_empty_domain() {
	use crate::test_assertions::AssertResult as _;

	let store = Datastore::in_memory();
	let mut setup = store.begin();
	let class = setup
		.define_class(
			ClassSchema::new("item", AtomKind::Node, [crate::datastore::ClassField::new(
				"value",
				crate::values::Type::Int,
			)])
			.assert_ok(),
		)
		.assert_ok();
	setup
		.configure_changefeed(
			ChangefeedSource::Class(class),
			ChangefeedConfig::new("P1D".parse().assert_ok()).assert_ok(),
		)
		.assert_ok();
	let key = setup.intern_property("value").assert_ok();
	let a: RecordId = "item:a".parse().assert_ok();
	let b: RecordId = "item:b".parse().assert_ok();
	let c: RecordId = "item:c".parse().assert_ok();
	setup.create_atom(a.clone(), PropertyMap::from_iter([(key, 2.into())])).assert_ok();
	setup.create_atom(b.clone(), PropertyMap::from_iter([(key, 7.into())])).assert_ok();
	let floor = setup.commit().assert_ok().versionstamp();
	let mut update = store.begin();
	update.set_base_property(&a, key, 3.into()).assert_ok();
	let first = update.commit().assert_ok().versionstamp();
	let mut insert = store.begin();
	insert.create_atom(c.clone(), PropertyMap::from_iter([(key, 5.into())])).assert_ok();
	let second = insert.commit().assert_ok().versionstamp();
	let mut delete = store.begin();
	delete.delete_atom(&b).assert_ok();
	let third = delete.commit().assert_ok().versionstamp();
	let mut empty = store.begin();
	empty.delete_atom(&a).assert_ok();
	empty.delete_atom(&c).assert_ok();
	let fourth = empty.commit().assert_ok().versionstamp();
	for (name, arguments, expected) in [
		("count", Vec::new(), vec![(second, 2, 3), (third, 3, 2), (fourth, 2, 0)]),
		("sum", vec![LiveExpr::field("value").assert_ok()], vec![
			(first, 9, 10),
			(second, 10, 15),
			(third, 15, 8),
			(fourth, 8, 0),
		]),
	] {
		let query = LiveQuery::new(LiveSource::Class(class)).with_projection(
			LiveProjection::Value(LiveExpr::Call {
				name: name.into(),
				arguments,
			}),
		);
		let mut subscription = store
			.subscribe_live(
				query,
				LiveOptions::default().with_since(LiveSince::Versionstamp(floor)),
			)
			.assert_ok();
		for (stamp, before, after) in expected {
			let crate::live::LiveNext::Delivery(delivery) = subscription.next().assert_ok() else {
				return {
					let test_result: FerrosGrynnResult<()> =
						Err(crate::gql_status::FerrosGrynnError::invalid_argument(
							"aggregate history missing",
						));
					test_result.assert_ok();
				};
			};
			let crate::live::LivePayload::Batch(batch) = delivery.payload() else {
				return {
					let test_result: FerrosGrynnResult<()> =
						Err(crate::gql_status::FerrosGrynnError::invalid_argument(
							"aggregate history GAP",
						));
					test_result.assert_ok();
				};
			};
			assert_eq!(batch.versionstamp(), stamp);
			assert_eq!(batch.events().len(), 1);
			assert!(
				matches!(batch.events()[0].identity(), crate::live::LiveIdentity::Aggregate(keys) if keys.is_empty())
			);
			assert_eq!(batch.events()[0].before(), Some(&Value::from(before)));
			assert_eq!(batch.events()[0].after(), Some(&Value::from(after)));
			subscription.ack(delivery.token()).assert_ok();
		}
		assert!(matches!(subscription.next().assert_ok(), crate::live::LiveNext::Idle));
	}
}

/// An aggregate cannot reconstruct unknown pre-capture members from primary.
#[test]
fn live_backfill_aggregate_missing_creation_anchor_is_explicit_gap() {
	use crate::test_assertions::AssertResult as _;

	let store = Datastore::in_memory();
	let mut setup = store.begin();
	let class =
		setup.define_class(ClassSchema::new("item", AtomKind::Node, []).assert_ok()).assert_ok();
	setup.create_atom("item:old".parse().assert_ok(), PropertyMap::new()).assert_ok();
	let floor = setup.commit().assert_ok().versionstamp();
	let mut enable = store.begin();
	enable
		.configure_changefeed(
			ChangefeedSource::Class(class),
			ChangefeedConfig::new("P1D".parse().assert_ok()).assert_ok(),
		)
		.assert_ok();
	enable.commit().assert_ok();
	let mut insert = store.begin();
	insert.create_atom("item:new".parse().assert_ok(), PropertyMap::new()).assert_ok();
	insert.commit().assert_ok();
	let query = LiveQuery::new(LiveSource::Class(class)).with_projection(LiveProjection::Value(
		LiveExpr::Call {
			name: "count".into(),
			arguments: Vec::new(),
		},
	));
	let mut subscription = store
		.subscribe_live(query, LiveOptions::default().with_since(LiveSince::Versionstamp(floor)))
		.assert_ok();
	let crate::live::LiveNext::Delivery(delivery) = subscription.next().assert_ok() else {
		return {
			let test_result: FerrosGrynnResult<()> =
				Err(crate::gql_status::FerrosGrynnError::invalid_argument(
					"aggregate anchor GAP missing",
				));
			test_result.assert_ok();
		};
	};
	assert!(matches!(delivery.payload(), crate::live::LivePayload::Gap(gap)
		if gap.reason() == crate::live::LiveGapReason::InsufficientCoverage));
}

/// Equal cutoff images do not excuse a missing returning-to-original tail.
#[test]
fn live_backfill_aggregate_missing_closed_tail_is_corruption() {
	use crate::test_assertions::AssertResult as _;

	let (store, class, record, key, floor) = source_fixture().assert_ok();
	let mut update = store.begin();
	update.set_base_property(&record, key, 2.into()).assert_ok();
	let changed = update.commit().assert_ok().versionstamp().get();
	let mut restore = store.begin();
	restore.set_base_property(&record, key, 1.into()).assert_ok();
	let restored = restore.commit().assert_ok().versionstamp().get();
	{
		let mut current = store.inner.current.write();
		let root = Arc::make_mut(&mut current);
		let feed = root
			.changefeeds()
			.for_source(ChangefeedSource::Class(class))
			.ok_or_else(|| crate::gql_status::FerrosGrynnError::invalid_argument("fixture source"))
			.assert_ok()
			.id;
		Arc::make_mut(root.change_history_mut()).remove(&(feed, changed));
		Arc::make_mut(root.change_history_mut()).remove(&(feed, restored));
	}
	let cutoff = store.inner.current_root().commit_ts;
	let query = LiveQuery::new(LiveSource::Class(class)).with_projection(LiveProjection::Value(
		LiveExpr::Call {
			name: "sum".into(),
			arguments: vec![LiveExpr::field("value").assert_ok()],
		},
	));
	assert!(
		store
			.subscribe_live(
				query,
				LiveOptions::default().with_since(LiveSince::Versionstamp(floor))
			)
			.is_err()
	);
	assert_eq!(store.inner.current_root().commit_ts, cutoff);
	assert!(store.inner.current_root().live_catalog().queries.is_empty());
}

/// Sequential insertion contexts share peak scratch; unchanged members and
/// dictionary width must not multiply their retained charge.
#[test]
fn live_backfill_aggregate_dictionary_scratch_is_peak_not_cumulative() {
	use crate::test_assertions::AssertResult as _;

	let store = Datastore::in_memory();
	let mut setup = store.begin();
	let class = setup
		.define_class(
			ClassSchema::new("item", AtomKind::Node, [crate::datastore::ClassField::new(
				"value",
				crate::values::Type::Int,
			)])
			.assert_ok(),
		)
		.assert_ok();
	let key = setup.intern_property("value").assert_ok();
	for ordinal in 0..63 {
		setup.intern_property(&format!("unused_{ordinal}")).assert_ok();
	}
	setup
		.configure_changefeed(
			ChangefeedSource::Class(class),
			ChangefeedConfig::new("P1D".parse().assert_ok()).assert_ok(),
		)
		.assert_ok();
	for ordinal in 0..512 {
		setup
			.create_atom(
				format!("item:r{ordinal:04}").parse().assert_ok(),
				PropertyMap::from_iter([(key, 1.into())]),
			)
			.assert_ok();
	}
	let floor = setup.commit().assert_ok().versionstamp();
	let mut update = store.begin();
	update.set_base_property(&"item:r0000".parse().assert_ok(), key, 2.into()).assert_ok();
	let stamp = update.commit().assert_ok().versionstamp();
	for name in ["count", "sum"] {
		let query = LiveQuery::new(LiveSource::Class(class)).with_projection(
			LiveProjection::Value(LiveExpr::Call {
				name: name.into(),
				arguments: if name == "sum" {
					vec![LiveExpr::field("value").assert_ok()]
				} else {
					Vec::new()
				},
			}),
		);
		let mut subscription = store
			.subscribe_live(
				query.clone(),
				LiveOptions::default()
					.with_budgets(8 * 1024 * 1024, 1_000_000)
					.assert_ok()
					.with_since(LiveSince::Versionstamp(floor)),
			)
			.assert_ok();
		if name == "sum" {
			let crate::live::LiveNext::Delivery(delivery) = subscription.next().assert_ok() else {
				return {
					let test_result: FerrosGrynnResult<()> =
						Err(crate::gql_status::FerrosGrynnError::invalid_argument(
							"bounded SUM missing",
						));
					test_result.assert_ok();
				};
			};
			let crate::live::LivePayload::Batch(batch) = delivery.payload() else {
				return {
					let test_result: FerrosGrynnResult<()> = Err(
						crate::gql_status::FerrosGrynnError::invalid_argument("bounded SUM GAP"),
					);
					test_result.assert_ok();
				};
			};
			assert_eq!(batch.versionstamp(), stamp);
			assert_eq!(batch.events()[0].before(), Some(&Value::from(512)));
			assert_eq!(batch.events()[0].after(), Some(&Value::from(513)));
			subscription.ack(delivery.token()).assert_ok();
		}
		assert!(matches!(subscription.next().assert_ok(), crate::live::LiveNext::Idle));
		let cutoff = store.inner.current_root().commit_ts;
		let registered = store.inner.current_root().live_catalog().queries.len();
		assert!(
			store
				.subscribe_live(
					query,
					LiveOptions::default()
						.with_budgets(64 * 1024, 1_000_000)
						.assert_ok()
						.with_since(LiveSince::Versionstamp(floor)),
				)
				.is_err()
		);
		assert_eq!(store.inner.current_root().commit_ts, cutoff);
		assert_eq!(store.inner.current_root().live_catalog().queries.len(), registered);
	}
}

/// Linked-member-only changes, negative reads and recreated records use each
/// historical complete domain, checked against ordinary SELECT before/after.
#[test]
fn live_backfill_omit_fetch_linked_members_matches_select_and_negative_reads() {
	use crate::{
		datastore::ClassField,
		live::LiveField,
		test_assertions::AssertResult as _,
		values::Type,
	};
	let store = Datastore::in_memory();
	let mut setup = store.begin();
	let class = setup
		.define_class(
			ClassSchema::new("person", AtomKind::Node, [
				ClassField::new("age", Type::Int),
				ClassField::new("manager", Type::Option(Box::new(Type::Record))),
			])
			.assert_ok(),
		)
		.assert_ok();
	let age = setup.intern_property("age").assert_ok();
	let manager = setup.intern_property("manager").assert_ok();
	let alice: RecordId = "person:alice".parse().assert_ok();
	let bob: RecordId = "person:bob".parse().assert_ok();
	let carol: RecordId = "person:carol".parse().assert_ok();
	setup
		.configure_changefeed(
			ChangefeedSource::Class(class),
			ChangefeedConfig::new("P1D".parse().assert_ok()).assert_ok(),
		)
		.assert_ok();
	setup
		.create_atom(
			alice.clone(),
			PropertyMap::from_iter([(age, 19.into()), (manager, Value::from(bob.clone()))]),
		)
		.assert_ok();
	setup.create_atom(bob.clone(), PropertyMap::from_iter([(age, 40.into())])).assert_ok();
	let floor = setup.commit().assert_ok().versionstamp();
	let query = LiveQuery::new(LiveSource::Class(class))
		.with_filter(
			LiveExpr::field("age")
				.assert_ok()
				.binary(LiveBinaryOperator::Less, LiveExpr::Literal(30.into())),
		)
		.with_projection(LiveProjection::Fields(vec![
			LiveField::new(LiveExpr::field("manager").assert_ok())
				.with_alias(LivePath::field("boss").assert_ok()),
			LiveField::new(LiveExpr::field("age").assert_ok())
				.with_alias(LivePath::field("secret").assert_ok()),
		]))
		.with_omit(vec![LivePath::field("secret").assert_ok()])
		.with_fetch(vec![LivePath::field("boss").assert_ok()]);
	let oracle = || -> FerrosGrynnResult<Value> {
		let result = store
			.execute(
				"SELECT manager AS boss, age AS secret FROM person WHERE age < 30 OMIT secret \
				 FETCH boss;",
				&Object::new(),
			)?
			.into_iter()
			.next()
			.ok_or_else(|| {
				crate::gql_status::FerrosGrynnError::invalid_argument("SELECT oracle missing")
			})?
			.result?;
		let Value::Array(rows) = result.value() else {
			return Err(crate::gql_status::FerrosGrynnError::invalid_argument(
				"SELECT oracle is not rows",
			));
		};
		assert_eq!(rows.len(), 1);
		rows.iter().next().cloned().ok_or_else(|| {
			crate::gql_status::FerrosGrynnError::invalid_argument("SELECT row missing")
		})
	};
	let mut expected = Vec::new();
	for operation in 0..5 {
		let old = oracle().assert_ok();
		let mut update = store.begin();
		match operation {
			0 => {
				update.set_base_property(&bob, age, 41.into()).assert_ok();
			}
			1 => {
				update.delete_atom(&bob).assert_ok();
			}
			2 => {
				update
					.create_atom(bob.clone(), PropertyMap::from_iter([(age, 42.into())]))
					.assert_ok();
			}
			3 => {
				update.set_base_property(&alice, age, 20.into()).assert_ok();
				update.set_base_property(&bob, age, 43.into()).assert_ok();
			}
			_ => {
				update
					.create_atom(carol.clone(), PropertyMap::from_iter([(age, 44.into())]))
					.assert_ok();
				update.set_base_property(&alice, manager, Value::from(carol.clone())).assert_ok();
			}
		}
		let stamp = update.commit().assert_ok().versionstamp();
		expected.push((stamp, old, oracle().assert_ok()));
	}
	for source in [LiveSource::Class(class), LiveSource::Record(alice.clone())] {
		let selected = LiveQuery::new(source)
			.with_filter(
				query
					.filter()
					.cloned()
					.ok_or_else(|| {
						crate::gql_status::FerrosGrynnError::invalid_argument(
							"fixture filter missing",
						)
					})
					.assert_ok(),
			)
			.with_projection(query.projection().clone())
			.with_omit(query.omit().to_vec())
			.with_fetch(query.fetch().to_vec());
		let mut subscription = store
			.subscribe_live(
				selected,
				LiveOptions::default().with_since(LiveSince::Versionstamp(floor)),
			)
			.assert_ok();
		for (stamp, old, new) in &expected {
			let crate::live::LiveNext::Delivery(delivery) = subscription.next().assert_ok() else {
				return {
					let test_result: FerrosGrynnResult<()> =
						Err(crate::gql_status::FerrosGrynnError::invalid_argument(
							"historical FETCH missing",
						));
					test_result.assert_ok();
				};
			};
			let crate::live::LivePayload::Batch(batch) = delivery.payload() else {
				return {
					let test_result: FerrosGrynnResult<()> =
						Err(crate::gql_status::FerrosGrynnError::invalid_argument(
							"historical FETCH GAP",
						));
					test_result.assert_ok();
				};
			};
			assert_eq!(batch.versionstamp(), *stamp);
			assert_eq!(batch.events().len(), 1);
			assert_eq!(
				batch.events()[0].identity(),
				&crate::live::LiveIdentity::Record(alice.clone())
			);
			assert_eq!(batch.events()[0].before(), Some(old));
			assert_eq!(batch.events()[0].after(), Some(new));
			subscription.ack(delivery.token()).assert_ok();
		}
		assert!(matches!(subscription.next().assert_ok(), crate::live::LiveNext::Idle));
	}
}

/// Aggregate membership follows linked-record reads and suppresses unchanged
/// aggregate values, rather than merely changed source rows.
#[test]
fn live_backfill_navigation_aggregate_matches_select() {
	use crate::{
		datastore::ClassField,
		live::LiveIdentity,
		test_assertions::AssertResult as _,
		values::Type,
	};
	let store = Datastore::in_memory();
	let mut setup = store.begin();
	let class = setup
		.define_class(
			ClassSchema::new("person", AtomKind::Node, [
				ClassField::new("age", Type::Int),
				ClassField::new("manager", Type::Option(Box::new(Type::Record))),
			])
			.assert_ok(),
		)
		.assert_ok();
	let age = setup.intern_property("age").assert_ok();
	let manager = setup.intern_property("manager").assert_ok();
	let alice: RecordId = "person:alice".parse().assert_ok();
	let bob: RecordId = "person:bob".parse().assert_ok();
	setup
		.configure_changefeed(
			ChangefeedSource::Class(class),
			ChangefeedConfig::new("P1D".parse().assert_ok()).assert_ok(),
		)
		.assert_ok();
	setup
		.create_atom(
			alice,
			PropertyMap::from_iter([(age, 19.into()), (manager, Value::from(bob.clone()))]),
		)
		.assert_ok();
	setup.create_atom(bob.clone(), PropertyMap::from_iter([(age, 40.into())])).assert_ok();
	let floor = setup.commit().assert_ok().versionstamp();
	let oracle = || -> FerrosGrynnResult<Value> {
		let response = store.execute(
			"SELECT VALUE count() FROM person WHERE age < 30 AND manager.age = 41;",
			&Object::new(),
		)?;
		let result = response
			.into_iter()
			.next()
			.ok_or_else(|| {
				crate::gql_status::FerrosGrynnError::invalid_argument("aggregate oracle missing")
			})?
			.result?;
		let Value::Array(rows) = result.value() else {
			return Err(crate::gql_status::FerrosGrynnError::invalid_argument(
				"aggregate oracle is not rows",
			));
		};
		assert_eq!(rows.len(), 1);
		rows.iter().next().cloned().ok_or_else(|| {
			crate::gql_status::FerrosGrynnError::invalid_argument("aggregate row missing")
		})
	};
	let mut expected = Vec::new();
	for operation in 0..4 {
		let old = oracle().assert_ok();
		let mut update = store.begin();
		match operation {
			0 => {
				update.set_base_property(&bob, age, 41.into()).assert_ok();
			}
			1 => {
				update.set_base_property(&bob, age, 42.into()).assert_ok();
			}
			2 => {
				update.set_base_property(&bob, age, 40.into()).assert_ok();
			}
			_ => {
				update.set_base_property(&bob, age, 41.into()).assert_ok();
			}
		}
		let stamp = update.commit().assert_ok().versionstamp();
		let new = oracle().assert_ok();
		if old != new {
			expected.push((stamp, old, new));
		}
	}
	assert_eq!(expected.len(), 3);
	let query = LiveQuery::new(LiveSource::Class(class))
		.with_filter(
			LiveExpr::field("age")
				.assert_ok()
				.binary(LiveBinaryOperator::Less, LiveExpr::Literal(30.into()))
				.binary(
					LiveBinaryOperator::And,
					LiveExpr::Field(
						LivePath::new(vec!["manager".into(), "age".into()]).assert_ok(),
					)
					.binary(LiveBinaryOperator::Equal, LiveExpr::Literal(41.into())),
				),
		)
		.with_projection(LiveProjection::Value(LiveExpr::Call {
			name: "count".into(),
			arguments: Vec::new(),
		}));
	let mut subscription = store
		.subscribe_live(query, LiveOptions::default().with_since(LiveSince::Versionstamp(floor)))
		.assert_ok();
	for (stamp, old, new) in expected {
		let crate::live::LiveNext::Delivery(delivery) = subscription.next().assert_ok() else {
			return {
				let test_result: FerrosGrynnResult<()> =
					Err(crate::gql_status::FerrosGrynnError::invalid_argument(
						"historical navigation missing",
					));
				test_result.assert_ok();
			};
		};
		let crate::live::LivePayload::Batch(batch) = delivery.payload() else {
			return {
				let test_result: FerrosGrynnResult<()> =
					Err(crate::gql_status::FerrosGrynnError::invalid_argument(
						"historical navigation GAP",
					));
				test_result.assert_ok();
			};
		};
		assert_eq!(batch.versionstamp(), stamp);
		assert_eq!(batch.events().len(), 1);
		assert!(
			matches!(batch.events()[0].identity(), LiveIdentity::Aggregate(keys) if keys.is_empty())
		);
		assert_eq!(batch.events()[0].before(), Some(&old));
		assert_eq!(batch.events()[0].after(), Some(&new));
		subscription.ack(delivery.token()).assert_ok();
	}
	assert!(matches!(subscription.next().assert_ok(), crate::live::LiveNext::Idle));
}
