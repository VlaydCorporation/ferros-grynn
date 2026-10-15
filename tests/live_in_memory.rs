#![cfg(test)]

//! Public snapshot registry ownership and rollback without query text.

/// Assertions shared with the root unit-test harness.
#[path = "common/assertions.rs"]
pub mod test_assertions;

/// Proven empty durable replay does not reserve a maximum historical head.
#[test]
fn live_resume_ram_empty_history_admits_multiple_consumers() {
	use ferros_grynn::{
		changefeed::{ChangefeedConfig, ChangefeedSource},
		live::{LiveExpr, LiveNext},
		values::PropertyMap,
	};

	use crate::test_assertions::AssertResult as _;
	let store = Datastore::in_memory();
	let mut setup = store.begin();
	let class =
		setup.define_class(ClassSchema::new("person", AtomKind::Node, []).assert_ok()).assert_ok();
	setup
		.configure_changefeed(
			ChangefeedSource::Class(class),
			ChangefeedConfig::new("P1D".parse().assert_ok()).assert_ok(),
		)
		.assert_ok();
	setup.commit().assert_ok();
	let mut cursors = Vec::new();
	for _ in 0..8 {
		let subscription = store
			.subscribe_live(
				LiveQuery::new(LiveSource::Class(class))
					.with_filter(LiveExpr::Literal(false.into())),
				LiveOptions::default(),
			)
			.assert_ok();
		cursors.push(subscription.cursor().clone());
	}
	let mut update = store.begin();
	update.create_atom("person:a".parse().assert_ok(), PropertyMap::new()).assert_ok();
	update.commit().assert_ok();
	let mut handles = cursors
		.iter()
		.map(|cursor| store.resume_live(cursor))
		.collect::<FerrosGrynnResult<Vec<_>>>()
		.assert_ok();
	for handle in &mut handles {
		assert!(matches!(handle.next().assert_ok(), LiveNext::Idle));
	}
}

/// Saved computed images precede a concurrent tail and use durable ACK on
/// resume.
#[test]
fn live_resume_ram_history_ack_and_tail() {
	use ferros_grynn::{
		changefeed::{ChangefeedConfig, ChangefeedSource},
		datastore::ClassField,
		gql_status::FerrosGrynnError,
		live::{LiveCursor, LiveExpr, LiveNext, LivePayload, LiveProjection},
		values::{PropertyMap, RecordId, Type, Value},
	};

	use crate::test_assertions::AssertResult as _;
	let store = Datastore::in_memory();
	let mut setup = store.begin();
	let class = setup
		.define_class(
			ClassSchema::new("person", AtomKind::Node, [ClassField::new("age", Type::Int)])
				.assert_ok(),
		)
		.assert_ok();
	setup
		.configure_changefeed(
			ChangefeedSource::Class(class),
			ChangefeedConfig::new("P1D".parse().assert_ok()).assert_ok(),
		)
		.assert_ok();
	setup.commit().assert_ok();
	let key = store
		.property_dictionary()
		.get("age")
		.ok_or_else(|| FerrosGrynnError::invalid_argument("missing age"))
		.assert_ok();
	let record: RecordId = "person:a".parse().assert_ok();
	let mut setup = store.begin();
	setup.create_atom(record.clone(), PropertyMap::from_iter([(key, 18.into())])).assert_ok();
	setup.commit().assert_ok();
	let query = LiveQuery::new(LiveSource::Class(class))
		.with_projection(LiveProjection::Value(LiveExpr::field("age").assert_ok()));
	let initial = store.subscribe_live(query, LiveOptions::default()).assert_ok();
	let saved = initial.cursor().clone();
	drop(initial);
	let mut stamps = Vec::new();
	for age in [19_i64, 20] {
		let mut update = store.begin();
		update.set_base_property(&record, key, age.into()).assert_ok();
		stamps.push(update.commit().assert_ok().versionstamp());
	}
	let mut resumed = store.resume_live(&saved).assert_ok();
	let mut tail = store.begin();
	tail.set_base_property(&record, key, 21.into()).assert_ok();
	stamps.push(tail.commit().assert_ok().versionstamp());
	for (ordinal, stamp) in stamps.into_iter().enumerate() {
		let LiveNext::Delivery(delivery) = resumed.next().assert_ok() else {
			return {
				let test_result: FerrosGrynnResult<()> =
					Err(FerrosGrynnError::invalid_argument("missing replay/tail delivery"));
				test_result.assert_ok();
			};
		};
		let LivePayload::Batch(batch) = delivery.payload() else {
			return {
				let test_result: FerrosGrynnResult<()> =
					Err(FerrosGrynnError::invalid_argument("unexpected history GAP"));
				test_result.assert_ok();
			};
		};
		assert_eq!(batch.versionstamp(), stamp);
		assert_eq!(batch.events()[0].before(), Some(&Value::from(18_i64 + ordinal as i64)));
		assert_eq!(batch.events()[0].after(), Some(&Value::from(19_i64 + ordinal as i64)));
		let LiveNext::Delivery(repeated) = resumed.next().assert_ok() else {
			return {
				let test_result: FerrosGrynnResult<()> =
					Err(FerrosGrynnError::invalid_argument("missing unconfirmed replay"));
				test_result.assert_ok();
			};
		};
		assert_eq!(repeated.token(), delivery.token());
		resumed.ack(delivery.token()).assert_ok();
	}
	assert!(matches!(resumed.next().assert_ok(), LiveNext::Idle));
	let confirmed = resumed.cursor().clone();
	drop(resumed);
	let forged = LiveCursor::new(
		saved.database_id(),
		saved.live_id(),
		saved.version(),
		(confirmed.versionstamp().get() + 1).into(),
	)
	.assert_ok();
	assert!(store.resume_live(&forged).is_err());
	let mut resumed = store.resume_live(&saved).assert_ok();
	assert_eq!(resumed.cursor(), &confirmed, "older client cursor cannot regress durable ACK");
	assert!(
		matches!(resumed.next().assert_ok(), LiveNext::Idle),
		"ACK-only global stamps do not imply a GAP"
	);
}

/// Detach loses a volatile prefix explicitly; closed incarnations can drain
/// saved history.
#[test]
fn live_resume_ram_volatile_gap_and_closed_history() {
	use ferros_grynn::{
		changefeed::{ChangefeedConfig, ChangefeedSource},
		gql_status::FerrosGrynnError,
		live::{LiveGapReason, LiveNext, LivePayload},
		values::PropertyMap,
	};

	use crate::test_assertions::AssertResult as _;
	let store = Datastore::in_memory();
	let mut setup = store.begin();
	let class =
		setup.define_class(ClassSchema::new("person", AtomKind::Node, []).assert_ok()).assert_ok();
	setup.commit().assert_ok();
	let initial = store
		.subscribe_live(LiveQuery::new(LiveSource::Class(class)), LiveOptions::default())
		.assert_ok();
	let saved = initial.cursor().clone();
	drop(initial);
	let mut insert = store.begin();
	insert.create_atom("person:a".parse().assert_ok(), PropertyMap::new()).assert_ok();
	let lost = insert.commit().assert_ok().versionstamp();
	let mut resumed = store.resume_live(&saved).assert_ok();
	let LiveNext::Delivery(gap) = resumed.next().assert_ok() else {
		return {
			let test_result: FerrosGrynnResult<()> =
				Err(FerrosGrynnError::invalid_argument("missing volatile GAP"));
			test_result.assert_ok();
		};
	};
	assert!(
		matches!(gap.payload(), LivePayload::Gap(gap) if gap.reason() == LiveGapReason::VolatileHistoryLost && gap.through() == Some(lost))
	);
	resumed.ack(gap.token()).assert_ok();
	assert!(matches!(resumed.next().assert_ok(), LiveNext::Idle));
	drop(resumed);
	let mut enable = store.begin();
	enable
		.configure_changefeed(
			ChangefeedSource::Class(class),
			ChangefeedConfig::new("P1D".parse().assert_ok()).assert_ok(),
		)
		.assert_ok();
	enable.commit().assert_ok();
	let cursor = store.begin().live_info(saved.live_id()).assert_ok().cursor().clone();
	let mut insert = store.begin();
	insert.create_atom("person:b".parse().assert_ok(), PropertyMap::new()).assert_ok();
	let retained = insert.commit().assert_ok().versionstamp();
	let mut close = store.begin();
	close.close_live(saved.live_id()).assert_ok();
	close.commit().assert_ok();
	let mut resumed = store.resume_live(&cursor).assert_ok();
	let LiveNext::Delivery(gap) = resumed.next().assert_ok() else {
		return {
			let test_result: FerrosGrynnResult<()> =
				Err(FerrosGrynnError::invalid_argument("missing pre-capture GAP"));
			test_result.assert_ok();
		};
	};
	assert!(matches!(gap.payload(), LivePayload::Gap(_)));
	resumed.ack(gap.token()).assert_ok();
	let LiveNext::Delivery(delivery) = resumed.next().assert_ok() else {
		return {
			let test_result: FerrosGrynnResult<()> =
				Err(FerrosGrynnError::invalid_argument("missing closed history"));
			test_result.assert_ok();
		};
	};
	assert_eq!(delivery.token().versionstamp(), retained);
	assert!(matches!(delivery.payload(), LivePayload::Batch(_)));
	resumed.ack(delivery.token()).assert_ok();
	assert!(matches!(resumed.next().assert_ok(), LiveNext::Closed));
}

/// Cancelled BLOCK admits no partial fanout and releases every permit.
#[test]
fn live_subscription_ram_block_cancellation_deadline_and_import() {
	use std::{
		sync::mpsc,
		thread,
		time::{Duration, Instant},
	};

	use ferros_grynn::{
		datastore::{CommitOptions, ImportOptions},
		gql::QueryCancellation,
		gql_status::FerrosGrynnError,
		live::{LiveNext, LiveOverflowPolicy},
		values::{PropertyMap, RecordId},
	};

	use crate::test_assertions::AssertResult as _;
	let store = Datastore::in_memory();
	let mut setup = store.begin();
	let class =
		setup.define_class(ClassSchema::new("person", AtomKind::Node, []).assert_ok()).assert_ok();
	setup.commit().assert_ok();
	let query = LiveQuery::new(LiveSource::Class(class));
	let options = LiveOptions::default().with_queue(1, 1 << 20).assert_ok();
	let mut gap = store.subscribe_live(query.clone(), options.clone()).assert_ok();
	let mut blocked =
		store.subscribe_live(query, options.with_overflow(LiveOverflowPolicy::Block)).assert_ok();
	let first: RecordId = "person:a".parse().assert_ok();
	let rejected: RecordId = "person:b".parse().assert_ok();
	let mut tx = store.begin();
	tx.create_atom(first, PropertyMap::new()).assert_ok();
	tx.commit().assert_ok();
	let LiveNext::Delivery(first_gap) = gap.next().assert_ok() else {
		return {
			let test_result: ferros_grynn::gql_status::FerrosGrynnResult<()> =
				Err(FerrosGrynnError::invalid_argument("missing first delivery"));
			test_result.assert_ok();
		};
	};
	let LiveNext::Delivery(first_block) = blocked.next().assert_ok() else {
		return {
			let test_result: ferros_grynn::gql_status::FerrosGrynnResult<()> =
				Err(FerrosGrynnError::invalid_argument("missing BLOCK delivery"));
			test_result.assert_ok();
		};
	};
	let cancellation = QueryCancellation::default();
	let mut controls = CommitOptions::default();
	controls.cancellation = Some(cancellation.clone());
	let (send, receive) = mpsc::channel();
	thread::scope(|scope| -> ferros_grynn::gql_status::FerrosGrynnResult<()> {
		let rejected = rejected.clone();
		let store = &store;
		scope.spawn(move || {
			let result = (|| {
				let mut tx = store.begin();
				tx.create_atom(rejected, PropertyMap::new())?;
				tx.commit_with_options(&controls)
			})();
			let _sent = send.send(result);
		});
		assert!(matches!(
			receive.recv_timeout(Duration::from_millis(75)),
			Err(mpsc::RecvTimeoutError::Timeout)
		));
		cancellation.cancel();
		let result = receive.recv_timeout(Duration::from_secs(2)).map_err(|error| {
			FerrosGrynnError::invalid_argument("BLOCK cancellation did not finish")
				.with_technical_source(error)
		})?;
		assert!(result.is_err());
		Ok(())
	})
	.assert_ok();
	assert!(store.begin().atom(&rejected).assert_ok().is_none());
	let LiveNext::Delivery(repeated) = gap.next().assert_ok() else {
		return {
			let test_result: ferros_grynn::gql_status::FerrosGrynnResult<()> =
				Err(FerrosGrynnError::invalid_argument("partial fanout replaced issued delivery"));
			test_result.assert_ok();
		};
	};
	assert_eq!(repeated.token(), first_gap.token());
	let mut deadline = CommitOptions::default();
	deadline.deadline = Instant::now().checked_add(Duration::from_millis(40));
	let mut tx = store.begin();
	tx.create_atom(rejected.clone(), PropertyMap::new()).assert_ok();
	assert!(tx.commit_with_options(&deadline).is_err());
	assert!(store.begin().atom(&rejected).assert_ok().is_none());
	deadline.deadline = Instant::now().checked_add(Duration::from_millis(40));
	assert!(
		store
			.import_with_commit_options(ImportOptions::default(), &deadline, |session| {
				session.apply_item(|tx| tx.create_atom(rejected.clone(), PropertyMap::new()))?;
				Ok(())
			})
			.is_err()
	);
	assert!(store.begin().atom(&rejected).assert_ok().is_none());
	gap.ack(first_gap.token()).assert_ok();
	blocked.ack(first_block.token()).assert_ok();
	let mut tx = store.begin();
	tx.create_atom(rejected, PropertyMap::new()).assert_ok();
	tx.commit().assert_ok();
	assert!(matches!(gap.next().assert_ok(), LiveNext::Delivery(_)));
	assert!(matches!(blocked.next().assert_ok(), LiveNext::Delivery(_)));
}

/// Public delivery/ACK ownership, replacement and shutdown without query text.
#[test]
fn live_subscription_ram_whole_commit_ack_and_shutdown() {
	use ferros_grynn::{
		gql_status::FerrosGrynnError,
		live::{LiveCursor, LiveExpr, LiveNext, LivePayload, LiveProjection},
		values::{PropertyMap, RecordId, Value},
	};

	use crate::test_assertions::AssertResult as _;
	let store = Datastore::in_memory();
	let mut setup = store.begin();
	let class = setup
		.define_class(
			ClassSchema::new("person", AtomKind::Node, [ferros_grynn::datastore::ClassField::new(
				"age",
				ferros_grynn::values::Type::Int,
			)])
			.assert_ok(),
		)
		.assert_ok();
	setup.commit().assert_ok();
	let key = store
		.property_dictionary()
		.get("age")
		.ok_or_else(|| FerrosGrynnError::invalid_argument("missing age property"))
		.assert_ok();
	let mut setup = store.begin();
	let first: RecordId = "person:a".parse().assert_ok();
	let second: RecordId = "person:b".parse().assert_ok();
	setup.create_atom(first.clone(), PropertyMap::from_iter([(key, 18.into())])).assert_ok();
	setup.create_atom(second.clone(), PropertyMap::from_iter([(key, 19.into())])).assert_ok();
	setup.commit().assert_ok();
	let query = LiveQuery::new(LiveSource::Class(class))
		.with_projection(LiveProjection::Value(LiveExpr::field("age").assert_ok()));
	let mut subscription = store.subscribe_live(query.clone(), LiveOptions::default()).assert_ok();
	assert!(matches!(subscription.next().assert_ok(), LiveNext::Idle));
	let registered = subscription.cursor().clone();
	let mut old = store.begin();
	let mut update = store.begin();
	update.set_base_property(&first, key, 20.into()).assert_ok();
	update.set_base_property(&second, key, 21.into()).assert_ok();
	let stamp = update.commit().assert_ok().versionstamp();
	let LiveNext::Delivery(delivery) = subscription.next().assert_ok() else {
		return {
			let test_result: ferros_grynn::gql_status::FerrosGrynnResult<()> =
				Err(FerrosGrynnError::invalid_argument("missing SDK delivery"));
			test_result.assert_ok();
		};
	};
	let LivePayload::Batch(batch) = delivery.payload() else {
		return {
			let test_result: ferros_grynn::gql_status::FerrosGrynnResult<()> =
				Err(FerrosGrynnError::invalid_argument("missing SDK batch"));
			test_result.assert_ok();
		};
	};
	assert_eq!(batch.versionstamp(), stamp);
	assert_eq!(batch.events().len(), 2);
	assert_eq!(batch.events()[0].before(), Some(&Value::from(18_i64)));
	assert_eq!(batch.events()[0].after(), Some(&Value::from(20_i64)));
	assert_eq!(subscription.cursor(), &registered);
	let fabricated = LiveCursor::new(
		registered.database_id(),
		registered.live_id(),
		registered.version(),
		(stamp.get() + 1).into(),
	)
	.assert_ok();
	assert!(subscription.ack(&fabricated).is_err());
	let LiveNext::Delivery(repeated) = subscription.next().assert_ok() else {
		return {
			let test_result: ferros_grynn::gql_status::FerrosGrynnResult<()> =
				Err(FerrosGrynnError::invalid_argument("missing repeated SDK delivery"));
			test_result.assert_ok();
		};
	};
	assert_eq!(repeated.token(), delivery.token());
	subscription.ack(delivery.token()).assert_ok();
	subscription.ack(delivery.token()).assert_ok();
	assert_eq!(subscription.cursor(), delivery.token());
	assert_eq!(store.begin().live_info(subscription.id()).assert_ok().cursor(), delivery.token());
	assert_eq!(old.live_info(subscription.id()).assert_ok().cursor(), &registered);
	assert!(matches!(subscription.next().assert_ok(), LiveNext::Idle));
	let mut replacement =
		store.reregister_live(subscription.id(), query, LiveOptions::default()).assert_ok();
	assert_ne!(replacement.id(), subscription.id());
	assert!(matches!(subscription.next().assert_ok(), LiveNext::Closed));
	assert!(matches!(replacement.next().assert_ok(), LiveNext::Idle));
	drop(old);
	drop(store);
	assert!(matches!(replacement.next().assert_ok(), LiveNext::Closed));
	assert_eq!(
		batch.events()[1].after(),
		Some(&Value::from(21_i64)),
		"owned payload survives datastore shutdown"
	);
}

/// Oversized whole commits pause capture while committing primary data.
#[test]
fn live_subscription_ram_oversized_commit_faults_without_partial_delivery() {
	use ferros_grynn::{
		gql_status::FerrosGrynnError,
		live::{LiveNext, LivePayload},
		values::{PropertyMap, RecordId},
	};

	use crate::test_assertions::AssertResult as _;
	let store = Datastore::in_memory();
	let mut setup = store.begin();
	let class =
		setup.define_class(ClassSchema::new("person", AtomKind::Node, []).assert_ok()).assert_ok();
	setup.commit().assert_ok();
	let mut subscription = store
		.subscribe_live(
			LiveQuery::new(LiveSource::Class(class)),
			LiveOptions::default().with_queue(1, 1 << 20).assert_ok(),
		)
		.assert_ok();
	let mut update = store.begin();
	let first: RecordId = "person:a".parse().assert_ok();
	let second: RecordId = "person:b".parse().assert_ok();
	update.create_atom(first.clone(), PropertyMap::new()).assert_ok();
	update.create_atom(second.clone(), PropertyMap::new()).assert_ok();
	update.commit().assert_ok();
	assert!(store.begin().atom(&first).assert_ok().is_some());
	assert!(store.begin().atom(&second).assert_ok().is_some());
	assert_eq!(store.begin().live_info(subscription.id()).assert_ok().state(), LiveState::Paused);
	let LiveNext::Delivery(delivery) = subscription.next().assert_ok() else {
		return {
			let test_result: ferros_grynn::gql_status::FerrosGrynnResult<()> =
				Err(FerrosGrynnError::invalid_argument("missing SDK resource fault"));
			test_result.assert_ok();
		};
	};
	assert!(matches!(delivery.payload(), LivePayload::Fault { .. }));
	subscription.ack(delivery.token()).assert_ok();
	assert!(matches!(subscription.next().assert_ok(), LiveNext::Idle));
}

use ferros_grynn::{
	datastore::{ClassSchema, Datastore},
	gql_status::FerrosGrynnResult,
	graph::AtomKind,
	live::{LiveOptions, LiveQuery, LiveSource, LiveState},
};

/// Metadata and frozen recipes remain owned after their transaction ends.
#[test]
fn live_registry_ram_owned_info_and_incarnations() {
	use crate::test_assertions::AssertResult as _;

	let store = Datastore::in_memory();
	let mut tx = store.begin();
	let class =
		tx.define_class(ClassSchema::new("person", AtomKind::Node, []).assert_ok()).assert_ok();
	let query = LiveQuery::new(LiveSource::Class(class));
	let id = tx.register_live(query.clone(), LiveOptions::default()).assert_ok();
	let stamp = tx.commit().assert_ok().versionstamp();
	let info = store.begin().live_info(id).assert_ok();
	let mut old = store.begin();
	let mut replace = store.begin();
	let next = replace.reregister_live(id, query, LiveOptions::default()).assert_ok();
	replace.commit().assert_ok();
	assert_eq!(old.live_info(id).assert_ok().state(), LiveState::Active);
	assert_eq!(info.defined_at(), stamp);
	assert_eq!(info.cursor().versionstamp(), stamp);
	assert_eq!(store.begin().live_info(id).assert_ok().state(), LiveState::Closed);
	assert_eq!(store.begin().live_info(next).assert_ok().state(), LiveState::Active);
	let mut aborted = store.begin();
	aborted.close_live(next).assert_ok();
	drop(aborted);
	assert_eq!(store.begin().live_info(next).assert_ok().state(), LiveState::Active);
}

/// Independent SDK identities remain reactive while ordinary inputs stay
/// frozen.
#[test]
fn live_bindings_ram_owned_values_and_revisions() {
	use ferros_grynn::{
		live::{LiveBindingOptions, LiveExpr},
		values::{Object, Value},
	};

	use crate::test_assertions::AssertResult as _;
	let store = Datastore::in_memory();
	let mut tx = store.begin();
	let mut parameters = Object::new();
	parameters.insert("fixed", Value::from(18_i64));
	let first = tx
		.define_live_binding(
			LiveExpr::Parameter("fixed".into()),
			parameters,
			LiveBindingOptions::default(),
		)
		.assert_ok();
	let second = tx
		.define_live_binding(LiveExpr::Binding(first), Object::new(), LiveBindingOptions::default())
		.assert_ok();
	tx.commit().assert_ok();
	let owned = store.begin().live_binding_info(second).assert_ok();
	let mut old = store.begin();
	let mut tx = store.begin();
	tx.replace_live_binding_value(first, Value::from(21_i64)).assert_ok();
	let stamp = tx.commit().assert_ok().versionstamp();
	assert_eq!(store.begin().live_binding_value(second).assert_ok(), Value::from(21_i64));
	assert_eq!(store.begin().live_binding_info(second).assert_ok().revision().value(), stamp);
	assert_eq!(old.live_binding_value(second).assert_ok(), Value::from(18_i64));
	assert_eq!(owned.value(), &Value::from(18_i64));
}

/// Delivery disconnection never substitutes for explicit durable capture stop.
#[test]
fn live_close_after_disconnect_stops_capture() {
	use ferros_grynn::{
		live::{LiveNext, LiveOverflowPolicy, LivePayload},
		values::PropertyMap,
	};

	use crate::test_assertions::AssertResult as _;
	let store = Datastore::in_memory();
	let mut setup = store.begin();
	let class =
		setup.define_class(ClassSchema::new("item", AtomKind::Node, []).assert_ok()).assert_ok();
	setup.commit().assert_ok();
	let mut handle = store
		.subscribe_live(
			LiveQuery::new(LiveSource::Class(class)),
			LiveOptions::default()
				.with_overflow(LiveOverflowPolicy::Disconnect)
				.with_queue(1, 64 * 1024 * 1024)
				.assert_ok(),
		)
		.assert_ok();
	let id = handle.id();
	let mut write = store.begin();
	write.create_atom("item:a".parse().assert_ok(), PropertyMap::new()).assert_ok();
	write.commit().assert_ok();
	let mut write = store.begin();
	write.create_atom("item:b".parse().assert_ok(), PropertyMap::new()).assert_ok();
	write.commit().assert_ok();
	let LiveNext::Delivery(delivery) = handle.next().assert_ok() else {
		return {
			let test_result: FerrosGrynnResult<()> =
				Err(ferros_grynn::gql_status::FerrosGrynnError::invalid_argument(
					"disconnect control absent",
				));
			test_result.assert_ok();
		};
	};
	assert!(matches!(delivery.payload(), LivePayload::Gap(_)));
	handle.ack(delivery.token()).assert_ok();
	assert!(matches!(handle.next().assert_ok(), LiveNext::Closed));
	assert_eq!(store.begin().live_info(id).assert_ok().state(), LiveState::Active);
	handle.close().assert_ok();
	assert_eq!(store.begin().live_info(id).assert_ok().state(), LiveState::Closed);
	let attempts = store.live_statistics().assert_ok().query_evaluation_attempts;
	let mut write = store.begin();
	write.create_atom("item:c".parse().assert_ok(), PropertyMap::new()).assert_ok();
	write.commit().assert_ok();
	assert_eq!(store.live_statistics().assert_ok().query_evaluation_attempts, attempts);
	drop(store);
	handle.close().assert_ok();
}

/// INFO reports current dependency revisions; saved batches own evaluated
/// revisions.
#[test]
fn live_info_binding_revisions_are_snapshot_dependencies() {
	use ferros_grynn::{
		live::{LiveBindingOptions, LiveExpr, LiveProjection},
		values::{Object, PropertyMap},
	};

	use crate::test_assertions::AssertResult as _;
	let store = Datastore::in_memory();
	let mut setup = store.begin();
	let class =
		setup.define_class(ClassSchema::new("item", AtomKind::Node, []).assert_ok()).assert_ok();
	setup.create_atom("item:a".parse().assert_ok(), PropertyMap::new()).assert_ok();
	let binding = setup
		.define_live_binding(
			LiveExpr::Literal(1.into()),
			Object::new(),
			LiveBindingOptions::default(),
		)
		.assert_ok();
	setup.commit().assert_ok();
	let mut handle = store
		.subscribe_live(
			LiveQuery::new(LiveSource::Class(class))
				.with_projection(LiveProjection::Value(LiveExpr::Binding(binding))),
			LiveOptions::default(),
		)
		.assert_ok();
	let id = handle.id();
	let old = store.begin().live_info(id).assert_ok().bindings()[0];
	handle.close().assert_ok();
	let mut replace = store.begin();
	replace.replace_live_binding_value(binding, 2.into()).assert_ok();
	replace.commit().assert_ok();
	let mut read = store.begin();
	let info = read.live_info(id).assert_ok();
	let current = read.live_binding_info(binding).assert_ok().revision();
	assert_eq!(info.state(), LiveState::Closed);
	assert_eq!(info.bindings().len(), 1);
	assert_eq!(info.bindings()[0], current);
	assert_ne!(info.bindings()[0].definition(), old.definition());
}
