//! Text registration exercises the same SDK and whole-commit consumer state.

use crate::{
	changefeed::{ChangefeedConfig, ChangefeedSource},
	datastore::{ClassField, ClassSchema, Datastore, GqlSession},
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	graph::AtomKind,
	live::{LiveId, LiveIdentity, LiveNext, LiveOutputMode, LivePayload, LiveProjection},
	values::{Object, PropertyMap, RecordId, Type, Value},
};

/// Array positions retain bag copies and emit exact net changes after reorder.
#[test]
fn live_window_split_gql_net_delivery() -> FerrosGrynnResult<()> {
	use crate::live::LiveIdentityPart;
	let store = Datastore::in_memory();
	let mut session = store.session();
	execute_value(
		&mut session,
		"DEFINE CLASS item TYPE NODE SCHEMALESS; CREATE item:a SET tags = [7, 7];",
		&Object::new(),
	)?;
	let id = identity(execute_value(
		&mut session,
		"LIVE SELECT VALUE properties.tags FROM item SPLIT properties.tags;",
		&Object::new(),
	)?)?;
	assert_eq!(store.begin().live_info(id)?.query().split().len(), 1);
	let mut handle = session.take_live_subscription(id)?;
	execute_value(&mut session, "UPDATE item:a SET tags = [8, 7, 7];", &Object::new())?;
	let LiveNext::Delivery(delivery) = handle.next()? else {
		return Err(FerrosGrynnError::invalid_argument("SPLIT delivery absent"));
	};
	let LivePayload::Batch(batch) = delivery.payload() else {
		return Err(FerrosGrynnError::invalid_argument("SPLIT delivery faulted"));
	};
	assert_eq!(batch.events().len(), 2);
	for (event, position, before, after) in [
		(&batch.events()[0], 0, Some(Value::from(7)), Some(Value::from(8))),
		(&batch.events()[1], 2, None, Some(Value::from(7))),
	] {
		assert!(
			matches!(event.identity(),LiveIdentity::Composite(parts) if parts.last()==Some(&LiveIdentityPart::Split(position)))
		);
		assert_eq!(event.before(), before.as_ref());
		assert_eq!(event.after(), after.as_ref());
	}
	handle.ack(delivery.token())?;
	execute_value(&mut session, "UPDATE item:a SET tags = [];", &Object::new())?;
	let LiveNext::Delivery(delivery) = handle.next()? else {
		return Err(FerrosGrynnError::invalid_argument("SPLIT leave absent"));
	};
	let LivePayload::Batch(batch) = delivery.payload() else {
		return Err(FerrosGrynnError::invalid_argument("SPLIT leave faulted"));
	};
	assert_eq!(batch.events().len(), 3);
	assert!(batch.events().iter().all(|event| event.after().is_none()));
	handle.ack(delivery.token())?;
	handle.close()?;
	Ok(())
}

/// Window operands retain frozen lexical values and tracked binding identities.
#[test]
fn live_window_gql_frozen_and_reactive_bounds() -> FerrosGrynnResult<()> {
	let store = Datastore::in_memory();
	let mut setup = store.begin();
	setup.define_class(ClassSchema::new("item", AtomKind::Node, [ClassField::new(
		"age",
		Type::Int,
	)])?)?;
	let age = setup.intern_property("age")?;
	for (id, value) in [("a", 1), ("b", 2), ("c", 3)] {
		setup.create_atom(
			format!("item:{id}").parse()?,
			PropertyMap::from_iter([(age, value.into())]),
		)?;
	}
	setup.commit()?;
	let mut session = store.session();
	execute_value(
		&mut session,
		"LET $fixed = 1; LIVE LET $take = 1; LIVE LET $rank = 1; LIVE LET $skip = 0;",
		&Object::new(),
	)?;
	let frozen = identity(execute_value(
		&mut session,
		"LIVE SELECT VALUE age FROM item ORDER BY age LIMIT $fixed;",
		&Object::new(),
	)?)?;
	let tracked = identity(execute_value(
		&mut session,
		"LIVE SELECT VALUE age FROM item ORDER BY age * $rank LIMIT $take START $skip;",
		&Object::new(),
	)?)?;
	let mut frozen = session.take_live_subscription(frozen)?;
	let mut tracked = session.take_live_subscription(tracked)?;
	execute_value(&mut session, "ALTER LET $fixed VALUE 3;", &Object::new())?;
	assert!(matches!(frozen.next()?, LiveNext::Idle));
	execute_value(
		&mut session,
		"BEGIN; ALTER LIVE LET $rank VALUE -1; ALTER LIVE LET $take VALUE 2; COMMIT;",
		&Object::new(),
	)?;
	let LiveNext::Delivery(delivery) = tracked.next()? else {
		return Err(FerrosGrynnError::invalid_argument("reactive window missing"));
	};
	let LivePayload::Batch(batch) = delivery.payload() else {
		return Err(FerrosGrynnError::invalid_argument("reactive window fault"));
	};
	assert_eq!(batch.events().len(), 3);
	assert_eq!(batch.events()[0].identity(), &LiveIdentity::Record("item:a".parse()?));
	assert_eq!(batch.events()[0].before(), Some(&1.into()));
	assert!(batch.events()[0].after().is_none());
	for (event, value) in batch.events()[1..].iter().zip([2, 3]) {
		assert!(event.before().is_none());
		assert_eq!(event.after(), Some(&value.into()));
	}
	tracked.ack(delivery.token())?;
	assert!(matches!(frozen.next()?, LiveNext::Idle));
	execute_value(&mut session, "ALTER LIVE LET $skip VALUE 1;", &Object::new())?;
	let LiveNext::Delivery(delivery) = tracked.next()? else {
		return Err(FerrosGrynnError::invalid_argument("reactive offset missing"));
	};
	assert!(matches!(delivery.payload(), LivePayload::Batch(batch) if batch.events().len() == 2));
	tracked.ack(delivery.token())?;
	let recipe = store.begin().live_info(tracked.id())?.query().clone();
	execute_value(
		&mut session,
		"BEGIN; ALTER LIVE LET $take VALUE -1; CREATE item:new SET age = 5; COMMIT;",
		&Object::new(),
	)?;
	assert!(
		store.begin().atom(&"item:new".parse()?)?.is_some(),
		"source DML commits despite invalid LIVE bound"
	);
	assert_eq!(store.begin().live_info(tracked.id())?.state(), crate::live::LiveState::Paused);
	let LiveNext::Delivery(fault) = tracked.next()? else {
		return Err(FerrosGrynnError::invalid_argument("reactive window fault missing"));
	};
	assert!(matches!(fault.payload(), LivePayload::Fault { .. }));
	tracked.ack(fault.token())?;
	execute_value(&mut session, "ALTER LIVE LET $take VALUE 1;", &Object::new())?;
	let mut recovered =
		store.reregister_live(tracked.id(), recipe, crate::live::LiveOptions::default())?;
	assert!(matches!(recovered.next()?, LiveNext::Idle));
	assert_eq!(store.begin().live_info(recovered.id())?.state(), crate::live::LiveState::Active);
	for sql in [
		"LIVE SELECT * FROM item ORDER BY RAND();",
		"LIVE SELECT * FROM item START AFTER $cursor;",
		"LIVE SELECT * FROM item LIMIT -1;",
		"LIVE SELECT * FROM item LIMIT 0.5;",
	] {
		assert!(
			session.execute(sql, &Object::new())?.into_iter().any(|result| result.result.is_err()),
			"{sql}"
		);
	}
	Ok(())
}

/// ONLY preserves reactive windows and faults without rejecting source DML.
#[test]
fn live_window_only_gql_reactive_limit() -> FerrosGrynnResult<()> {
	let store = Datastore::in_memory();
	let mut setup = store.begin();
	setup.define_class(ClassSchema::new("item", AtomKind::Node, [ClassField::new(
		"age",
		Type::Int,
	)])?)?;
	let age = setup.intern_property("age")?;
	for (id, value) in [("a", 1), ("b", 2)] {
		setup.create_atom(
			format!("item:{id}").parse()?,
			PropertyMap::from_iter([(age, value.into())]),
		)?;
	}
	setup.commit()?;
	let mut session = store.session();
	let Err(error) =
		execute_value(&mut session, "LIVE SELECT ONLY VALUE age FROM item;", &Object::new())
	else {
		return Err(FerrosGrynnError::invalid_argument("GQL ONLY registration accepted two rows"));
	};
	assert_eq!(error.status().code().to_string(), "FG-22F22");
	execute_value(&mut session, "LIVE LET $take = 1;", &Object::new())?;
	let id = identity(execute_value(
		&mut session,
		"LIVE SELECT ONLY VALUE age FROM item ORDER BY age LIMIT $take;",
		&Object::new(),
	)?)?;
	let mut handle = session.take_live_subscription(id)?;
	assert!(store.begin().live_info(id)?.query().only());
	for (limit, before, after) in [(0, Some(Value::from(1)), None), (1, None, Some(Value::from(1)))]
	{
		execute_value(
			&mut session,
			&format!("ALTER LIVE LET $take VALUE {limit};"),
			&Object::new(),
		)?;
		let LiveNext::Delivery(delivery) = handle.next()? else {
			return Err(FerrosGrynnError::invalid_argument("ONLY window delivery absent"));
		};
		let LivePayload::Batch(batch) = delivery.payload() else {
			return Err(FerrosGrynnError::invalid_argument("ONLY window unexpectedly faulted"));
		};
		assert_eq!(batch.events().len(), 1);
		assert_eq!(batch.events()[0].identity(), &LiveIdentity::Record("item:a".parse()?));
		assert_eq!(batch.events()[0].before(), before.as_ref());
		assert_eq!(batch.events()[0].after(), after.as_ref());
		handle.ack(delivery.token())?;
	}
	execute_value(
		&mut session,
		"BEGIN; ALTER LIVE LET $take VALUE 2; UPDATE item:b SET age = 3; COMMIT;",
		&Object::new(),
	)?;
	assert_eq!(
		execute_value(&mut session, "SELECT ONLY VALUE age FROM item:b;", &Object::new())?,
		Value::from(3)
	);
	assert_eq!(store.begin().live_info(id)?.state(), crate::live::LiveState::Paused);
	let LiveNext::Delivery(delivery) = handle.next()? else {
		return Err(FerrosGrynnError::invalid_argument("ONLY GQL fault absent"));
	};
	assert!(matches!(delivery.payload(), LivePayload::Fault { diagnostic, .. }
		if diagnostic.status().code().to_string() == "FG-G1F09"
			&& diagnostic.cause().is_some_and(|cause|
				cause.status().code().to_string() == "FG-22F22")));
	Ok(())
}

/// A single base atom in two graphs retains separate qualified membership,
/// overlays and missing external FETCH dependencies through the shared
/// executor.
#[test]
fn live_graph_record_gql_and_sdk_scope() -> FerrosGrynnResult<()> {
	use crate::{
		graph::GraphKind,
		live::{LiveExpr, LiveField, LiveOptions, LivePath, LiveQuery, LiveSource},
	};
	let store = Datastore::in_memory();
	let mut setup = store.begin();
	setup.define_class(ClassSchema::new("person", AtomKind::Node, [
		ClassField::new("age", Type::Int),
		ClassField::new("manager", Type::Option(Box::new(Type::Record))),
	])?)?;
	let age = setup.intern_property("age")?;
	let manager = setup.intern_property("manager")?;
	let record: RecordId = "person:a".parse()?;
	let boss: RecordId = "person:boss".parse()?;
	setup.create_atom(
		record.clone(),
		PropertyMap::from_iter([(age, 18.into()), (manager, Value::from(boss.clone()))]),
	)?;
	let first = setup.define_graph("single_one", GraphKind::Graph)?;
	let second = setup.define_graph("single_two", GraphKind::Graph)?;
	for (graph, value) in [(&first, 11), (&second, 22)] {
		setup.attach_node(graph.id(), &record, None, None)?;
		setup.set_overlay(graph.id(), &record, age, value.into())?;
	}
	setup.commit()?;
	let mut sdk = store.subscribe_live(
		LiveQuery::new(LiveSource::GraphRecord {
			graph: first.clone(),
			record: record.clone(),
		})
		.with_projection(LiveProjection::Fields(vec![
			LiveField::new(LiveExpr::field("age")?),
			LiveField::new(LiveExpr::field("manager")?),
		]))
		.with_fetch(vec![LivePath::field("manager")?]),
		LiveOptions::default(),
	)?;
	let mut session = store.session();
	let id = identity(execute_value(
		&mut session,
		"LIVE SELECT age, manager FROM person:a IN GRAPH single_two FETCH manager;",
		&Object::new(),
	)?)?;
	let mut text = session.take_live_subscription(id)?;
	let mut update = store.begin();
	update.set_base_property(&record, age, 19.into())?;
	update.commit()?;
	assert!(matches!(sdk.next()?, LiveNext::Idle));
	assert!(matches!(text.next()?, LiveNext::Idle));
	let mut update = store.begin();
	update.create_atom(boss.clone(), PropertyMap::from_iter([(age, 40.into())]))?;
	update.commit()?;
	for (handle, graph, expected_age) in [(&mut sdk, &first, 11), (&mut text, &second, 22)] {
		let LiveNext::Delivery(delivery) = handle.next()? else {
			return Err(FerrosGrynnError::invalid_argument("scoped FETCH delivery missing"));
		};
		let LivePayload::Batch(batch) = delivery.payload() else {
			return Err(FerrosGrynnError::invalid_argument("scoped FETCH batch missing"));
		};
		assert_eq!(batch.events().len(), 1);
		let event = &batch.events()[0];
		assert!(matches!(event.identity(), LiveIdentity::Occurrence(occurrence)
			if occurrence.graph().id() == graph.id() && occurrence.atom().id() == &record));
		let oracle = execute_value(
			&mut session,
			&format!("SELECT age, manager FROM person:a IN GRAPH {} FETCH manager;", graph.name()),
			&Object::new(),
		)?;
		let rows = oracle
			.as_array()
			.ok_or_else(|| FerrosGrynnError::invalid_argument("oracle rows missing"))?;
		assert_eq!(event.after(), rows.get(0));
		let value = event
			.after()
			.and_then(Value::as_object)
			.ok_or_else(|| FerrosGrynnError::invalid_argument("scoped image missing"))?;
		assert_eq!(value.get("age"), Some(&expected_age.into()));
		assert!(
			matches!(value.get("manager"), Some(Value::Object(boss)) if boss.get("age") == Some(&40.into()))
		);
		handle.ack(delivery.token())?;
	}
	let mut update = store.begin();
	update.detach_occurrence(first.id(), &record, false)?;
	update.commit()?;
	let LiveNext::Delivery(delivery) = sdk.next()? else {
		return Err(FerrosGrynnError::invalid_argument("membership exit missing"));
	};
	assert!(matches!(delivery.payload(), LivePayload::Batch(batch)
		if batch.events().len() == 1 && batch.events()[0].before().is_some() && batch.events()[0].after().is_none()));
	sdk.ack(delivery.token())?;
	assert!(matches!(text.next()?, LiveNext::Idle));
	let mut update = store.begin();
	update.attach_node(first.id(), &record, None, None)?;
	update.commit()?;
	let LiveNext::Delivery(delivery) = sdk.next()? else {
		return Err(FerrosGrynnError::invalid_argument("membership entry missing"));
	};
	assert!(matches!(delivery.payload(), LivePayload::Batch(batch)
		if batch.events()[0].before().is_none() && batch.events()[0].after().and_then(Value::as_object).and_then(|row| row.get("age")) == Some(&19.into())));
	sdk.ack(delivery.token())?;
	assert!(matches!(text.next()?, LiveNext::Idle));
	Ok(())
}

/// OMIT hides payload fields without losing identity, and precedes external
/// FETCH.
#[test]
fn live_omit_gql_sdk_net_and_fetch_order() -> FerrosGrynnResult<()> {
	use crate::live::{LiveOptions, LivePath, LiveQuery, LiveSource};
	let store = Datastore::in_memory();
	let mut session = store.session();
	execute_value(
		&mut session,
		"DEFINE CLASS person TYPE NODE; DEFINE FIELD age ON CLASS person TYPE int; DEFINE FIELD \
		 manager ON CLASS person TYPE option<record>; DEFINE FIELD info ON CLASS person TYPE \
		 object; CREATE person:boss SET age = 40; CREATE person:a SET age = 18, manager = \
		 person:boss, info = {secret: 1, public: 2};",
		&Object::new(),
	)?;
	let select = "SELECT * FROM person:a OMIT id, info.secret, absent FETCH manager;";
	let mut previous = execute_value(&mut session, select, &Object::new())?
		.as_array()
		.and_then(|rows| rows.get(0))
		.cloned()
		.ok_or_else(|| FerrosGrynnError::invalid_argument("OMIT oracle missing"))?;
	let id = identity(execute_value(&mut session, &format!("LIVE {select}"), &Object::new())?)?;
	let mut text = session.take_live_subscription(id)?;
	let record: RecordId = "person:a".parse()?;
	let mut sdk = store.subscribe_live(
		LiveQuery::new(LiveSource::Record(record.clone()))
			.with_omit(vec![
				LivePath::field("id")?,
				LivePath::new(vec!["info".into(), "secret".into()])?,
				LivePath::field("absent")?,
			])
			.with_fetch(vec![LivePath::field("manager")?]),
		LiveOptions::default(),
	)?;
	let suppressed = identity(execute_value(
		&mut session,
		"LIVE SELECT * FROM person:a OMIT manager, id, info.secret FETCH manager;",
		&Object::new(),
	)?)?;
	let mut suppressed = session.take_live_subscription(suppressed)?;
	execute_value(
		&mut session,
		"UPDATE person:a SET info = {secret: 99, public: 2};",
		&Object::new(),
	)?;
	assert!(matches!(text.next()?, LiveNext::Idle));
	assert!(matches!(sdk.next()?, LiveNext::Idle));
	assert!(matches!(suppressed.next()?, LiveNext::Idle));
	for mutation in ["UPDATE person:a SET age = 19;", "UPDATE person:boss SET age = 41;"] {
		execute_value(&mut session, mutation, &Object::new())?;
		let oracle = execute_value(&mut session, select, &Object::new())?;
		let after = oracle
			.as_array()
			.and_then(|rows| rows.get(0))
			.ok_or_else(|| FerrosGrynnError::invalid_argument("OMIT after missing"))?;
		for handle in [&mut text, &mut sdk] {
			let LiveNext::Delivery(delivery) = handle.next()? else {
				return Err(FerrosGrynnError::invalid_argument("OMIT delivery missing"));
			};
			let LivePayload::Batch(batch) = delivery.payload() else {
				return Err(FerrosGrynnError::invalid_argument("OMIT batch missing"));
			};
			assert_eq!(batch.events().len(), 1);
			assert_eq!(batch.events()[0].identity(), &LiveIdentity::Record(record.clone()));
			assert_eq!(batch.events()[0].before(), Some(&previous));
			assert_eq!(batch.events()[0].after(), Some(after));
			assert!(after.as_object().is_some_and(|object| object.get("id").is_none()));
			handle.ack(delivery.token())?;
		}
		previous = after.clone();
		if mutation.contains("person:a") {
			let LiveNext::Delivery(delivery) = suppressed.next()? else {
				return Err(FerrosGrynnError::invalid_argument(
					"suppressed FETCH row update missing",
				));
			};
			suppressed.ack(delivery.token())?;
		} else {
			assert!(matches!(suppressed.next()?, LiveNext::Idle));
		}
	}
	for handle in [&mut text, &mut sdk, &mut suppressed] {
		handle.close()?;
	}
	Ok(())
}

/// Compound aggregate values retain one identity and apply data and
/// reactive-parameter changes together through ordinary SELECT.
#[test]
fn live_aggregate_constructors_gql_net_results() -> FerrosGrynnResult<()> {
	let store = Datastore::in_memory();
	let mut session = store.session();
	execute_value(
		&mut session,
		"DEFINE CLASS person TYPE NODE; DEFINE FIELD age ON CLASS person TYPE int; CREATE \
		 person:a SET age = 18; CREATE person:b SET age = 20; LIVE LET $threshold = 1;",
		&Object::new(),
	)?;
	let select = "SELECT VALUE {values: [count(), <int>sum(age)], chosen: IF count() > $threshold \
	              THEN sum(age) ELSE 0 END, cases: CASE count() WHEN 2 THEN sum(age) ELSE count() \
	              END, bounds: count()..=<int>sum(age)} FROM person GROUP ALL;";
	let before = execute_value(&mut session, select, &Object::new())?;
	let id = identity(execute_value(&mut session, &format!("LIVE {select}"), &Object::new())?)?;
	let mut handle = session.take_live_subscription(id)?;
	let mut previous = before
		.as_array()
		.and_then(|rows| rows.get(0))
		.cloned()
		.ok_or_else(|| FerrosGrynnError::invalid_argument("aggregate before missing"))?;
	for mutation in [
		"BEGIN; UPDATE person:b SET age = 21; ALTER LIVE LET $threshold VALUE 5; COMMIT;",
		"ALTER LIVE LET $threshold VALUE 0;",
	] {
		execute_value(&mut session, mutation, &Object::new())?;
		let oracle = execute_value(&mut session, select, &Object::new())?;
		let after = oracle
			.as_array()
			.and_then(|rows| rows.get(0))
			.ok_or_else(|| FerrosGrynnError::invalid_argument("aggregate oracle missing"))?;
		let LiveNext::Delivery(delivery) = handle.next()? else {
			return Err(FerrosGrynnError::invalid_argument("aggregate compound delivery missing"));
		};
		let LivePayload::Batch(batch) = delivery.payload() else {
			return Err(FerrosGrynnError::invalid_argument("aggregate compound batch missing"));
		};
		assert_eq!(batch.events().len(), 1);
		assert_eq!(batch.events()[0].identity(), &LiveIdentity::Aggregate(Vec::new()));
		assert_eq!(batch.events()[0].before(), Some(&previous));
		assert_eq!(batch.events()[0].after(), Some(after));
		previous = after.clone();
		handle.ack(delivery.token())?;
	}
	execute_value(
		&mut session,
		"BEGIN; ALTER LIVE LET $threshold VALUE 10; CANCEL;",
		&Object::new(),
	)?;
	assert!(matches!(handle.next()?, LiveNext::Idle));
	handle.close()?;
	Ok(())
}

/// Group keys are result identities; frozen and reactive captures participate
/// in the same ordinary SELECT net diff.
#[test]
fn live_group_gql_frozen_reactive_and_sdk() -> FerrosGrynnResult<()> {
	use crate::live::{LiveExpr, LiveField, LiveOptions, LivePath, LiveQuery, LiveSource};
	let store = Datastore::in_memory();
	let mut session = store.session();
	execute_value(
		&mut session,
		"DEFINE CLASS item TYPE NODE CHANGEFEED 1d; DEFINE FIELD k ON CLASS item TYPE int; CREATE \
		 item:a SET k = 1; CREATE item:b SET k = 1; CREATE item:c SET k = 2; LIVE LET $offset = \
		 0; LET $fixed = 10;",
		&Object::new(),
	)?;
	let dynamic = identity(execute_value(
		&mut session,
		"LIVE SELECT k + $offset AS key, count() AS n FROM item GROUP BY k + $offset;",
		&Object::new(),
	)?)?;
	let frozen = identity(execute_value(
		&mut session,
		"LIVE SELECT k + $fixed AS key, count() AS n FROM item GROUP BY k + $fixed;",
		&Object::new(),
	)?)?;
	let class = store
		.begin()
		.class("item")?
		.and_then(|class| class.id())
		.ok_or_else(|| FerrosGrynnError::invalid_argument("group fixture class missing"))?;
	let mut sdk = store.subscribe_live(
		LiveQuery::new(LiveSource::Class(class))
			.with_group_by(vec![LiveExpr::field("k")?])?
			.with_projection(LiveProjection::Fields(vec![
				LiveField::new(LiveExpr::field("k")?).with_alias(LivePath::field("key")?),
				LiveField::new(LiveExpr::Call {
					name: "count".into(),
					arguments: Vec::new(),
				})
				.with_alias(LivePath::field("n")?),
			])),
		LiveOptions::default(),
	)?;
	assert!(LiveQuery::new(LiveSource::Class(class)).with_group_by(Vec::new()).is_err());
	let mut dynamic_handle = session.take_live_subscription(dynamic)?;
	let mut frozen_handle = session.take_live_subscription(frozen)?;
	execute_value(
		&mut session,
		"BEGIN; UPDATE item:a SET k = 2; ALTER LIVE LET $offset VALUE 1; COMMIT;",
		&Object::new(),
	)?;
	for (handle, expected, oracle) in [
		(&mut dynamic_handle, 2, "SELECT k + 1 AS key, count() AS n FROM item GROUP BY k + 1;"),
		(&mut frozen_handle, 2, "SELECT k + 10 AS key, count() AS n FROM item GROUP BY k + 10;"),
		(&mut sdk, 2, "SELECT k AS key, count() AS n FROM item GROUP BY k;"),
	] {
		let LiveNext::Delivery(delivery) = handle.next()? else {
			return Err(FerrosGrynnError::invalid_argument("group delivery missing"));
		};
		let LivePayload::Batch(batch) = delivery.payload() else {
			return Err(FerrosGrynnError::invalid_argument("group batch missing"));
		};
		assert_eq!(batch.events().len(), expected);
		let values = execute_value(&mut session, oracle, &Object::new())?;
		let values = values
			.as_array()
			.ok_or_else(|| FerrosGrynnError::invalid_argument("group oracle missing"))?;
		for event in batch.events() {
			let LiveIdentity::Aggregate(keys) = event.identity() else {
				return Err(FerrosGrynnError::invalid_argument("group identity missing"));
			};
			assert_eq!(keys.len(), 1);
			let after = values
				.iter()
				.find(|value| value.as_object().and_then(|row| row.get("key")) == keys.first());
			assert_eq!(event.after(), after);
		}
		handle.ack(delivery.token())?;
	}
	execute_value(&mut session, "ALTER LIVE LET $offset VALUE 2;", &Object::new())?;
	let LiveNext::Delivery(delivery) = dynamic_handle.next()? else {
		return Err(FerrosGrynnError::invalid_argument("group key-only delivery missing"));
	};
	assert!(matches!(delivery.payload(), LivePayload::Batch(batch) if batch.events().len() == 3));
	dynamic_handle.ack(delivery.token())?;
	assert!(matches!(frozen_handle.next()?, LiveNext::Idle));
	assert!(matches!(sdk.next()?, LiveNext::Idle));
	execute_value(&mut session, "ALTER LET $fixed VALUE 100;", &Object::new())?;
	assert!(matches!(frozen_handle.next()?, LiveNext::Idle));
	execute_value(&mut session, "BEGIN; ALTER LIVE LET $offset VALUE 9; CANCEL;", &Object::new())?;
	assert!(matches!(dynamic_handle.next()?, LiveNext::Idle));
	for source in [
		"LIVE SELECT * FROM item GROUP ALL;",
		"LIVE SELECT k, count() FROM item GROUP BY k + 1;",
		"LIVE SELECT VALUE count() FROM item GROUP BY time::now();",
		"LIVE SELECT VALUE count() FROM item GROUP BY count();",
	] {
		assert!(execute_value(&mut session, source, &Object::new()).is_err(), "{source}");
	}
	Ok(())
}

/// Propagates every statement-local failure before using the final value.
fn execute_value(
	session: &mut GqlSession,
	text: &str,
	parameters: &Object,
) -> FerrosGrynnResult<Value> {
	let mut value = Value::None;
	for result in session.execute(text, parameters)? {
		value = result.result?.value().clone();
	}
	Ok(value)
}

/// UUID registration values use the existing runtime envelope unchanged.
fn identity(value: Value) -> FerrosGrynnResult<LiveId> {
	let Value::Uuid(uuid) = value else {
		return Err(FerrosGrynnError::invalid_argument("fixture LIVE UUID missing"));
	};
	LiveId::from_uuid(uuid)
}

/// Parameter-only commits update result membership; ordinary captures stay
/// frozen.
#[test]
fn live_gql_binding_chain_replacement_and_closure_identity() -> FerrosGrynnResult<()> {
	let store = Datastore::in_memory();
	let mut session = store.session();
	execute_value(
		&mut session,
		"DEFINE CLASS person TYPE NODE; DEFINE FIELD age ON CLASS person TYPE int; CREATE \
		 person:a SET age = 19; CREATE person:b SET age = 23; LET $fixed = 18; LIVE LET $min = \
		 $fixed; LIVE LET $next = $min + 1; LET $frozen = $min; LET $read = | | -> any { RETURN \
		 $min; };",
		&Object::new(),
	)?;
	let id = identity(execute_value(
		&mut session,
		"LET $sub = LIVE SELECT VALUE age FROM person WHERE age >= $min; RETURN $sub;",
		&Object::new(),
	)?)?;
	let info = store.begin().live_info(id)?;
	assert_eq!(info.bindings().len(), 1);
	assert!(!info.query().parameters().contains_key("min"));
	let binding = info.bindings()[0].id();
	let mut subscription = session.take_live_subscription(id)?;
	execute_value(&mut session, "ALTER LIVE LET $min VALUE 21;", &Object::new())?;
	assert_eq!(execute_value(&mut session, "RETURN $next;", &Object::new())?, 22.into());
	assert_eq!(execute_value(&mut session, "RETURN $read();", &Object::new())?, 21.into());
	assert_eq!(execute_value(&mut session, "RETURN $frozen;", &Object::new())?, 18.into());
	let LiveNext::Delivery(delivery) = subscription.next()? else {
		return Err(FerrosGrynnError::invalid_argument("reactive membership delivery missing"));
	};
	let LivePayload::Batch(batch) = delivery.payload() else {
		return Err(FerrosGrynnError::invalid_argument("reactive membership batch missing"));
	};
	assert_eq!(batch.events().len(), 1);
	assert_eq!(batch.events()[0].before(), Some(&19.into()));
	assert_eq!(batch.events()[0].after(), None);
	subscription.ack(delivery.token())?;
	execute_value(
		&mut session,
		"ALTER LIVE LET $min VALUE (SELECT VALUE count() FROM person); CREATE person:c SET age = \
		 30;",
		&Object::new(),
	)?;
	assert_eq!(execute_value(&mut session, "RETURN $min;", &Object::new())?, 3.into());
	assert_eq!(execute_value(&mut session, "RETURN $next;", &Object::new())?, 4.into());
	assert_eq!(store.begin().live_binding_info(binding)?.revision().definition().get(), 3);
	let plain = Object::from_iter([("ordinary_uuid", Value::from(binding.as_uuid()))]);
	assert_eq!(
		execute_value(&mut session, "RETURN $ordinary_uuid;", &plain)?,
		Value::from(binding.as_uuid())
	);
	assert_eq!(
		execute_value(&mut session, "IF true { LET $min = 99; RETURN $min; };", &Object::new())?,
		99.into()
	);
	assert_eq!(execute_value(&mut session, "RETURN $read();", &Object::new())?, 3.into());
	Ok(())
}

/// Failed cycles, TRY and CANCEL preserve registry and lexical savepoints.
#[test]
fn live_gql_binding_cycles_savepoints_and_removal() -> FerrosGrynnResult<()> {
	let store = Datastore::in_memory();
	let mut session = store.session();
	execute_value(
		&mut session,
		"LIVE LET $first = 1; LIVE LET $second = $first + 1;",
		&Object::new(),
	)?;
	assert_eq!(
		execute_value(
			&mut session,
			"TRY { ALTER LIVE LET $first VALUE $second; } CATCH |$e| {}; RETURN $first;",
			&Object::new()
		)?,
		1.into()
	);
	assert_eq!(
		execute_value(
			&mut session,
			"TRY { ALTER LIVE LET $first VALUE 10; THROW 'rollback'; } CATCH |$e| {}; RETURN \
			 $second;",
			&Object::new()
		)?,
		2.into()
	);
	let count = store.inner.current_root().live_catalog().bindings.len();
	execute_value(
		&mut session,
		"TRY { LIVE LET $lost = 7; THROW 'rollback'; } CATCH |$e| {};",
		&Object::new(),
	)?;
	assert_eq!(store.inner.current_root().live_catalog().bindings.len(), count);
	assert!(execute_value(&mut session, "RETURN $lost;", &Object::new()).is_err());
	execute_value(&mut session, "BEGIN; LIVE LET $private = 8; CANCEL;", &Object::new())?;
	assert_eq!(store.inner.current_root().live_catalog().bindings.len(), count);
	assert!(execute_value(&mut session, "RETURN $private;", &Object::new()).is_err());
	assert!(execute_value(&mut session, "REMOVE LIVE LET $first;", &Object::new()).is_err());
	assert!(execute_value(&mut session, "ALTER LET $first VALUE 99;", &Object::new()).is_err());
	execute_value(
		&mut session,
		"REMOVE LIVE LET $second; REMOVE LIVE LET $first;",
		&Object::new(),
	)?;
	let Value::Object(info) =
		execute_value(&mut session, "INFO FOR LIVE LET $first;", &Object::new())?
	else {
		return Err(FerrosGrynnError::invalid_argument("binding metadata missing"));
	};
	assert_eq!(info.get("state"), Some(&"closed".into()));
	assert!(execute_value(&mut session, "RETURN $first;", &Object::new()).is_err());
	Ok(())
}

/// Snapshot reads stay bounded and immutable reactive inputs cannot mutate a
/// frame.
#[test]
fn live_gql_binding_value_admission_and_readonly() -> FerrosGrynnResult<()> {
	let store = Datastore::in_memory();
	let mut session = store.session();
	assert_eq!(
		execute_value(&mut session, "RETURN <string>'abc';", &Object::new())?,
		"\"abc\"".into()
	);
	let inputs = Object::from_iter([("payload", Value::from("x".repeat(128 * 1024)))]);
	execute_value(
		&mut session,
		"LIVE LET $large = $payload; LIVE LET $items = $array;",
		&Object::from_iter([
			(
				"payload",
				inputs
					.get("payload")
					.cloned()
					.ok_or_else(|| FerrosGrynnError::invalid_argument("payload missing"))?,
			),
			("array", Value::from(vec![Value::from(1)])),
		]),
	)?;
	let result = session.execute_with_options(
		"RETURN $large;",
		&Object::new(),
		crate::gql::QueryExecutionOptions {
			barrier_memory_bytes: std::num::NonZeroUsize::new(4096)
				.ok_or_else(|| FerrosGrynnError::invalid_argument("fixture budget is zero"))?,
			..crate::gql::QueryExecutionOptions::default()
		},
	)?;
	assert!(result[0].result.is_err());
	for text in ["RETURN string::len($large);", "RETURN <string>$large;"] {
		let result = session.execute_with_options(
			text,
			&Object::new(),
			crate::gql::QueryExecutionOptions {
				barrier_memory_bytes: std::num::NonZeroUsize::new(192 * 1024)
					.ok_or_else(|| FerrosGrynnError::invalid_argument("fixture budget is zero"))?,
				..crate::gql::QueryExecutionOptions::default()
			},
		)?;
		assert!(result[0].result.is_err(), "{text}");
	}
	let result = session.execute_with_options(
		"RETURN <string>$large;",
		&Object::new(),
		crate::gql::QueryExecutionOptions {
			barrier_memory_bytes: std::num::NonZeroUsize::new(320 * 1024)
				.ok_or_else(|| FerrosGrynnError::invalid_argument("fixture budget is zero"))?,
			..crate::gql::QueryExecutionOptions::default()
		},
	)?;
	assert!(result[0].result.is_err());
	execute_value(
		&mut session,
		"LIVE LET $nested = $payload;",
		&Object::from_iter([(
			"payload",
			Value::from(vec![
				inputs
					.get("payload")
					.cloned()
					.ok_or_else(|| FerrosGrynnError::invalid_argument("payload missing"))?,
			]),
		)]),
	)?;
	let result = session.execute_with_options(
		"RETURN $nested[0];",
		&Object::new(),
		crate::gql::QueryExecutionOptions {
			barrier_memory_bytes: std::num::NonZeroUsize::new(320 * 1024)
				.ok_or_else(|| FerrosGrynnError::invalid_argument("fixture budget is zero"))?,
			..crate::gql::QueryExecutionOptions::default()
		},
	)?;
	assert_eq!(
		result[0].result.as_ref().map_err(Clone::clone)?.value(),
		inputs
			.get("payload")
			.ok_or_else(|| FerrosGrynnError::invalid_argument("payload missing"))?
	);
	assert_eq!(
		execute_value(&mut session, "RETURN $large;", &Object::new())?,
		inputs
			.get("payload")
			.cloned()
			.ok_or_else(|| FerrosGrynnError::invalid_argument("payload missing"))?
	);
	assert!(execute_value(&mut session, "array::push($items, 2);", &Object::new()).is_err());
	assert!(
		execute_value(&mut session, "BEGIN READ ONLY; LIVE LET $forbidden = 1;", &Object::new())
			.is_err()
	);
	assert!(
		!store
			.inner
			.current_root()
			.live_catalog()
			.bindings
			.values()
			.any(|entry| entry.value.as_ref() == &Value::from(99))
	);
	let mut transaction = store.begin();
	for result in transaction.execute("LIVE LET $direct = 99; RETURN $direct;", &Object::new())? {
		result.result?;
	}
	transaction.commit()?;
	Ok(())
}

/// A fault preserves its typed cause while an older snapshot retains the
/// preceding successful binding value.
#[test]
fn live_gql_binding_paused_diagnostic_and_old_snapshot() -> FerrosGrynnResult<()> {
	let store = Datastore::in_memory();
	let mut session = store.session();
	execute_value(
		&mut session,
		"DEFINE CLASS person TYPE NODE; CREATE person:a; LIVE LET $count = (SELECT VALUE count() \
		 FROM person); LIVE LET $inverse = 10 / $count; BEGIN;",
		&Object::new(),
	)?;
	let mut write = store.begin();
	write.delete_atom(&"person:a".parse()?)?;
	write.commit()?;
	assert!(store.begin().atom(&"person:a".parse()?)?.is_none());
	assert_eq!(execute_value(&mut session, "RETURN $inverse;", &Object::new())?, 10.into());
	execute_value(&mut session, "COMMIT;", &Object::new())?;
	let error = execute_value(&mut session, "RETURN $inverse;", &Object::new())
		.err()
		.ok_or_else(|| FerrosGrynnError::invalid_argument("binding fault missing"))?;
	assert_eq!(error.status().code().to_string(), "FG-G1F09");
	assert!(error.cause().is_some());
	Ok(())
}

/// Catalog copy admission uses the statement limit and leaves a rejected
/// removal entirely private.
#[test]
fn live_gql_binding_remove_catalog_budget() -> FerrosGrynnResult<()> {
	let store = Datastore::in_memory();
	let mut session = store.session();
	let statements =
		(0..256).map(|index| format!("LIVE LET $binding{index} = {index};")).collect::<String>();
	execute_value(&mut session, &statements, &Object::new())?;
	let stamp = store.inner.current_root().commit_ts;
	let result = session.execute_with_options(
		"REMOVE LIVE LET $binding0;",
		&Object::new(),
		crate::gql::QueryExecutionOptions {
			barrier_memory_bytes: std::num::NonZeroUsize::new(4096)
				.ok_or_else(|| FerrosGrynnError::invalid_argument("fixture budget is zero"))?,
			..crate::gql::QueryExecutionOptions::default()
		},
	)?;
	assert!(result[0].result.is_err());
	assert_eq!(store.inner.current_root().commit_ts, stamp);
	assert_eq!(execute_value(&mut session, "RETURN $binding0;", &Object::new())?, 0.into());
	Ok(())
}

/// SINCE and BUFFER resolve a reactive identity once at registration and
/// retain their options independently of subsequent binding changes.
#[test]
fn live_gql_binding_registration_options() -> FerrosGrynnResult<()> {
	let store = Datastore::in_memory();
	let mut session = store.session();
	execute_value(
		&mut session,
		"DEFINE CLASS person TYPE NODE CHANGEFEED 1d FULL;",
		&Object::new(),
	)?;
	let floor = store.inner.current_root().commit_ts;
	execute_value(
		&mut session,
		"CREATE person:a; LIVE LET $capacity = 5; LIVE LET $start = $stamp;",
		&Object::from_iter([("stamp", Value::from(floor))]),
	)?;
	let id = identity(execute_value(
		&mut session,
		"LIVE SELECT VALUE count() FROM person SINCE $start BUFFER $capacity + 1;",
		&Object::new(),
	)?)?;
	assert_eq!(store.begin().live_info(id)?.options().queue_entries(), 6);
	let LiveNext::Delivery(delivery) = session.next_live(id)? else {
		return Err(FerrosGrynnError::invalid_argument("reactive SINCE delivery missing"));
	};
	assert!(
		matches!(delivery.payload(), LivePayload::Batch(batch) if batch.events()[0].after() == Some(&1.into()))
	);
	execute_value(&mut session, "ALTER LIVE LET $capacity VALUE 9;", &Object::new())?;
	assert_eq!(store.begin().live_info(id)?.options().queue_entries(), 6);
	Ok(())
}

/// Pagination inputs resolve their identity in the invocation snapshot,
/// including nested and derived SELECT routes.
#[test]
fn live_gql_binding_select_pagination_inputs() -> FerrosGrynnResult<()> {
	let store = Datastore::in_memory();
	let mut session = store.session();
	execute_value(
		&mut session,
		"DEFINE CLASS person TYPE NODE; DEFINE FIELD age ON CLASS person TYPE int; CREATE \
		 person:a SET age = 10; CREATE person:b SET age = 20; CREATE person:c SET age = 30; LIVE \
		 LET $size = 1; LIVE LET $skip = 0; LET $read = | | -> any { RETURN $size; };",
		&Object::new(),
	)?;
	let inputs = [
		"SELECT VALUE age FROM person ORDER BY age LIMIT $size START $skip;",
		"SELECT VALUE age FROM person ORDER BY age LIMIT $read() START $skip;",
		"SELECT VALUE p.age FROM (SELECT age FROM person ORDER BY age LIMIT $size START $skip) AS \
		 p;",
	];
	for text in inputs {
		assert_eq!(
			execute_value(&mut session, text, &Object::new())?,
			Value::from(vec![Value::from(10)]),
			"{text}"
		);
	}
	let nested = "SELECT VALUE p.age IN (SELECT VALUE q.age FROM person AS q ORDER BY q.age LIMIT \
	              $size START $skip) FROM person AS p ORDER BY p.age;";
	assert_eq!(
		execute_value(&mut session, nested, &Object::new())?,
		Value::from(vec![Value::from(true), Value::from(false), Value::from(false)])
	);
	execute_value(
		&mut session,
		"ALTER LIVE LET $size VALUE 2; ALTER LIVE LET $skip VALUE 1;",
		&Object::new(),
	)?;
	for text in inputs {
		assert_eq!(
			execute_value(&mut session, text, &Object::new())?,
			Value::from(vec![Value::from(20), Value::from(30)]),
			"{text}"
		);
	}
	assert_eq!(
		execute_value(&mut session, nested, &Object::new())?,
		Value::from(vec![Value::from(false), Value::from(true), Value::from(true)])
	);
	execute_value(&mut session, "ALTER LIVE LET $size VALUE 0 - 1;", &Object::new())?;
	assert!(execute_value(&mut session, inputs[0], &Object::new()).is_err());
	execute_value(
		&mut session,
		"ALTER LIVE LET $size VALUE 1; ALTER LIVE LET $skip VALUE 'bad';",
		&Object::new(),
	)?;
	assert!(execute_value(&mut session, inputs[0], &Object::new()).is_err());
	Ok(())
}

/// Every MATCH selector uses the shared invocation snapshot, including
/// connector keys, variable paths, hyperedge endpoints and containment.
#[test]
fn live_gql_binding_match_selector_inputs() -> FerrosGrynnResult<()> {
	use crate::graph::{EdgeTopology, GraphKind};

	let store = Datastore::in_memory();
	let mut setup = store.begin();
	for (class, kind) in [
		("person", AtomKind::Node),
		("module", AtomKind::Node),
		("document", AtomKind::Node),
		("link", AtomKind::Edge),
	] {
		setup.define_class(ClassSchema::new(class, kind, [])?)?;
	}
	let root = setup.define_graph("reactive_match", GraphKind::Metagraph)?;
	let child = setup.define_graph("reactive_child", GraphKind::Graph)?;
	let people = ["person:a", "person:b", "person:c"]
		.map(str::parse::<RecordId>)
		.into_iter()
		.collect::<FerrosGrynnResult<Vec<_>>>()?;
	let mut local = Vec::new();
	for person in &people {
		setup.create_atom(person.clone(), PropertyMap::new())?;
		local.push(setup.attach_node(root.id(), person, None, None)?);
	}
	for (name, topology) in [
		("link:ab", EdgeTopology::directed([local[0]], [local[1]])),
		("link:ac", EdgeTopology::directed([local[0]], [local[2]])),
		("link:fan", EdgeTopology::directed([local[0], local[1]], [local[2]])),
	] {
		let record: RecordId = name.parse()?;
		setup.create_atom(record.clone(), PropertyMap::new())?;
		setup.attach_edge(root.id(), &record, None, topology, None, None)?;
	}
	let owner: RecordId = "module:owner".parse()?;
	let document: RecordId = "document:leaf".parse()?;
	setup.create_atom(owner.clone(), PropertyMap::new())?;
	setup.create_atom(document.clone(), PropertyMap::new())?;
	setup.attach_node(root.id(), &owner, None, Some(child.id()))?;
	setup.attach_node(child.id(), &document, None, None)?;
	setup.commit()?;
	let mut session = store.session();
	execute_value(
		&mut session,
		"LIVE LET $start = 'a'; LIVE LET $target = 'b'; LIVE LET $connector = 'ab'; LIVE LET $doc \
		 = 'leaf'; LIVE LET $owner = 'owner';",
		&Object::new(),
	)?;
	let inputs = [
		"MATCH (person:$target AS b) IN GRAPH reactive_match RETURN b;",
		"MATCH (person:$start)-[link:$connector]->(person:$target AS b) IN GRAPH reactive_match \
		 RETURN b;",
		"MATCH (person:$start)-[link*1 SHORTEST]->(person:$target AS b) IN GRAPH reactive_match \
		 RETURN b;",
	];
	for (index, target, connector) in [(1, "b", "ab"), (2, "c", "ac")] {
		execute_value(
			&mut session,
			&format!(
				"ALTER LIVE LET $target VALUE '{target}'; ALTER LIVE LET $connector VALUE \
				 '{connector}';"
			),
			&Object::new(),
		)?;
		for input in inputs {
			let value = execute_value(&mut session, input, &Object::new())?;
			let rows = value.as_array().ok_or_else(|| {
				FerrosGrynnError::invalid_argument("MATCH fixture did not return rows")
			})?;
			assert_eq!(rows.len(), 1, "{input}");
			assert_eq!(
				rows.get(0)
					.and_then(Value::as_object)
					.and_then(|row| row.get("b"))
					.and_then(Value::as_atom_occurrence)
					.map(|atom| atom.atom().id()),
				Some(&people[index]),
				"{input}"
			);
		}
	}
	execute_value(
		&mut session,
		"ALTER LIVE LET $start VALUE 'b'; ALTER LIVE LET $connector VALUE 'fan';",
		&Object::new(),
	)?;
	for input in [
		"MATCH HYPER [link:$connector] CONTAINS HEAD [(person:$start)], TAIL [(person:$target)] \
		 IN GRAPH reactive_match RETURN 1 AS found;",
		"MATCH (document:$doc) WITHIN <(module:$owner)> IN GRAPH reactive_match RETURN 1 AS found;",
	] {
		assert_eq!(
			execute_value(&mut session, input, &Object::new())?,
			Value::from(vec![Value::Object(Object::from_iter([("found", 1)]))]),
			"{input}"
		);
	}
	Ok(())
}

/// Clause depth uses lexical snapshot values without replacing explicit
/// segment bounds, including embedded MATCH and EXPLAIN.
#[test]
fn live_gql_binding_match_max_depth() -> FerrosGrynnResult<()> {
	use crate::graph::{EdgeTopology, GraphKind};

	let store = Datastore::in_memory();
	let mut setup = store.begin();
	for (class, kind) in
		[("person", AtomKind::Node), ("policy", AtomKind::Node), ("link", AtomKind::Edge)]
	{
		setup.define_class(ClassSchema::new(class, kind, [])?)?;
	}
	let graph = setup.define_graph("depths", GraphKind::Graph)?;
	let mut local = Vec::new();
	for id in ["person:a", "person:b", "person:c"] {
		let record = id.parse::<RecordId>()?;
		setup.create_atom(record.clone(), PropertyMap::new())?;
		local.push(setup.attach_node(graph.id(), &record, None, None)?);
	}
	for (id, source, target) in [("link:ab", 0, 1), ("link:bc", 1, 2)] {
		let record = id.parse::<RecordId>()?;
		setup.create_atom(record.clone(), PropertyMap::new())?;
		setup.attach_edge(
			graph.id(),
			&record,
			None,
			EdgeTopology::directed([local[source]], [local[target]]),
			None,
			None,
		)?;
	}
	setup.create_atom("policy:one".parse()?, PropertyMap::new())?;
	setup.commit()?;
	let mut session = store.session();
	execute_value(
		&mut session,
		"LET $fixed = 1; LIVE LET $depth = (SELECT VALUE count() FROM policy);",
		&Object::new(),
	)?;
	let query = "MATCH path = (person:a)-[link*1..]->(person AS b) IN GRAPH depths MAXDEPTH \
	             $depth RETURN path::length(path) AS length ORDER BY length;";
	let expected = |lengths: &[i32]| {
		Value::from(
			lengths
				.iter()
				.map(|length| Value::Object(Object::from_iter([("length", *length)])))
				.collect::<Vec<_>>(),
		)
	};
	assert_eq!(execute_value(&mut session, query, &Object::new())?, expected(&[1]));
	execute_value(&mut session, "BEGIN READ ONLY;", &Object::new())?;
	let mut writer = store.begin();
	writer.create_atom("policy:two".parse()?, PropertyMap::new())?;
	writer.commit()?;
	assert_eq!(execute_value(&mut session, query, &Object::new())?, expected(&[1]));
	execute_value(&mut session, "CANCEL;", &Object::new())?;
	assert_eq!(execute_value(&mut session, query, &Object::new())?, expected(&[1, 2]));
	assert_eq!(
		execute_value(&mut session, &query.replace("$depth", "$fixed"), &Object::new())?,
		expected(&[1])
	);
	execute_value(&mut session, "ALTER LET $fixed VALUE 2;", &Object::new())?;
	assert_eq!(
		execute_value(&mut session, &query.replace("$depth", "$fixed"), &Object::new())?,
		expected(&[1, 2])
	);
	let params = Object::from_iter([("graph", Value::from(graph))]);
	let embedded = "TRAVERSE FROM $graph |> tv::V() |> tv::as('a') |> tv::match(MATCH (person:a \
	                AS a)-[link*1..]->(person AS b) IN GRAPH depths MAXDEPTH $depth RETURN b) |> \
	                tv::select('b') |> tv::count();";
	assert_eq!(execute_value(&mut session, embedded, &params)?, Value::from(vec![Value::from(2)]));
	let mut hashes = Vec::new();
	for depth in [1, 2] {
		execute_value(
			&mut session,
			&format!("ALTER LIVE LET $depth VALUE {depth};"),
			&Object::new(),
		)?;
		let plan = execute_value(
			&mut session,
			&format!("EXPLAIN FULL FORMAT JSON {query}"),
			&Object::new(),
		)?;
		let plan = plan
			.as_object()
			.ok_or_else(|| FerrosGrynnError::invalid_argument("MATCH EXPLAIN fixture missing"))?;
		let nodes = plan
			.get("nodes")
			.and_then(Value::as_array)
			.ok_or_else(|| FerrosGrynnError::invalid_argument("MATCH EXPLAIN nodes missing"))?;
		let expand = nodes
			.iter()
			.filter_map(Value::as_object)
			.find(|node| node.get("operator") == Some(&Value::from("PathEnumerate")))
			.ok_or_else(|| {
				FerrosGrynnError::invalid_argument("MATCH EXPLAIN path operator missing")
			})?;
		assert_eq!(expand.get("maximum_depth"), Some(&Value::from(depth)));
		hashes.push(plan.get("plan_hash").cloned());
		assert_eq!(
			execute_value(&mut session, embedded, &params)?,
			Value::from(vec![Value::from(depth)])
		);
	}
	assert_ne!(hashes[0], hashes[1]);
	let explained = execute_value(
		&mut session,
		"EXPLAIN FULL FORMAT JSON BUILD GRAPH warned FROM (MATCH (person:a)-[link*1..]->(person \
		 AS b) IN GRAPH depths MAXDEPTH $depth RETURN b) NODES b;",
		&Object::new(),
	)?;
	let warnings = explained
		.as_object()
		.and_then(|plan| plan.get("warnings"))
		.and_then(Value::as_array)
		.ok_or_else(|| FerrosGrynnError::invalid_argument("embedded MATCH warnings missing"))?;
	assert!(
		warnings
			.iter()
			.filter_map(Value::as_str)
			.any(|warning| warning.contains("MATCH MAXDEPTH is bound at invocation"))
	);
	execute_value(&mut session, "ALTER LIVE LET $depth VALUE 0;", &Object::new())?;
	let explicit = query.replace("*1..", "*2");
	assert_eq!(execute_value(&mut session, &explicit, &Object::new())?, expected(&[2]));
	assert_eq!(
		execute_value(&mut session, &query.replace("*1..", "*0.."), &Object::new())?,
		expected(&[0])
	);
	assert!(execute_value(&mut session, query, &Object::new()).is_err());
	assert!(
		execute_value(&mut session, &format!("EXPLAIN FULL FORMAT JSON {query}"), &Object::new())
			.is_err()
	);
	for value in ["-1", "1.5", "31", "'bad'"] {
		execute_value(
			&mut session,
			&format!("ALTER LIVE LET $depth VALUE {value};"),
			&Object::new(),
		)?;
		for query in [
			query,
			explicit.as_str(),
			"MATCH (person:missing) IN GRAPH depths MAXDEPTH $depth RETURN 1 AS found;",
		] {
			assert!(
				execute_value(&mut session, query, &Object::new()).is_err(),
				"{value}: {query}"
			);
		}
	}
	Ok(())
}

/// Embedded MATCH keeps its own typed duration and caller snapshot.
#[test]
fn duration_embedded_match_uses_live_values() -> FerrosGrynnResult<()> {
	use crate::graph::{GraphKind, GraphRead, GraphRefReader};

	let store = Datastore::in_memory();
	let mut setup = store.begin();
	setup.define_class(ClassSchema::new("person", AtomKind::Node, [])?)?;
	let graph = setup.define_graph("duration_scope", GraphKind::Graph)?;
	for record in ["person:a", "person:b"] {
		let record = record.parse::<RecordId>()?;
		setup.create_atom(record.clone(), PropertyMap::new())?;
		setup.attach_node(graph.id(), &record, None, None)?;
	}
	setup.commit()?;
	let inputs = Object::from_iter([
		("second", Value::from("1s".parse::<crate::values::Duration>()?)),
		("zero", Value::from("0ms".parse::<crate::values::Duration>()?)),
		("graph", Value::from(graph.clone())),
	]);
	let mut session = store.session();
	execute_value(
		&mut session,
		"LIVE LET $limit = $zero; LET $fixed = $zero; LET $scope = $graph;",
		&inputs,
	)?;
	let queries = |timeout: &str, target: &str| {
		[
			format!(
				"TRAVERSE FROM MATCH (person AS n) IN GRAPH duration_scope TIMEOUT {timeout} \
				 RETURN n |> tv::count();"
			),
			format!(
				"TRAVERSE FROM $scope |> tv::V() |> tv::as('n') |> tv::match(MATCH (person AS n) \
				 IN GRAPH duration_scope TIMEOUT {timeout} RETURN n) |> tv::count();"
			),
			format!(
				"BUILD GRAPH {target} FROM (MATCH (person AS n) IN GRAPH duration_scope TIMEOUT \
				 {timeout} RETURN n) NODES n;"
			),
			format!(
				"BUILD GRAPH {target}_graph FROM (MATCH (person AS n) IN GRAPH duration_scope \
				 TIMEOUT {timeout} RETURN GRAPH {{ nodes: n, edges: [] }});"
			),
		]
	};
	for timeout in ["$limit", "$fixed", "0ms"] {
		for source in queries(timeout, "duration_rejected") {
			assert!(execute_value(&mut session, &source, &Object::new()).is_err(), "{source}");
		}
	}
	assert!(store.begin().named_graph("duration_rejected")?.is_none());
	assert!(store.begin().named_graph("duration_rejected_graph")?.is_none());
	execute_value(
		&mut session,
		"ALTER LIVE LET $limit VALUE $second; ALTER LET $fixed VALUE $second;",
		&inputs,
	)?;
	for (index, timeout) in ["$limit", "$fixed", "1s"].into_iter().enumerate() {
		let queries = queries(timeout, &format!("duration_copy_{index}"));
		for source in &queries[..2] {
			assert_eq!(
				execute_value(&mut session, source, &Object::new())?,
				Value::from(vec![Value::from(2)])
			);
		}
		for source in &queries[2..] {
			let result = execute_value(&mut session, source, &Object::new())?;
			let reference = result.as_graph_ref().ok_or_else(|| {
				FerrosGrynnError::invalid_argument("timed BUILD GRAPH reference missing")
			})?;
			assert_eq!(store.begin().read_graph(reference)?.node_ids()?.count(), 2);
		}
	}
	execute_value(&mut session, "ALTER LIVE LET $limit VALUE $zero;", &inputs)?;
	for source in queries("$limit", "duration_planned") {
		for prefix in ["EXPLAIN", "EXPLAIN FULL"] {
			execute_value(&mut session, &format!("{prefix} {source}"), &Object::new())?;
		}
		assert!(
			execute_value(&mut session, &format!("EXPLAIN ANALYZE {source}"), &Object::new())
				.is_err()
		);
	}
	assert!(store.begin().named_graph("duration_planned")?.is_none());
	assert!(store.begin().named_graph("duration_planned_graph")?.is_none());
	for source in queries("$limit", "duration_shadow") {
		execute_value(
			&mut session,
			&format!("IF true {{ LET $limit = $second; {source} }};"),
			&inputs,
		)?;
	}
	let info = execute_value(&mut session, "INFO FOR LIVE LET $limit;", &Object::new())?;
	let uuid = info.as_object().and_then(|info| info.get("id")).ok_or_else(|| {
		FerrosGrynnError::invalid_argument("test duration binding identity missing")
	})?;
	for source in queries("$ordinary", "duration_uuid") {
		assert!(
			execute_value(&mut session, &source, &Object::from_iter([("ordinary", uuid.clone())]))
				.is_err(),
			"{source}"
		);
	}
	for duration in ["P1M", "-PT1S", "PT20000000000S"] {
		let invalid = Object::from_iter([(
			"invalid",
			Value::from(duration.parse::<crate::values::Duration>()?),
		)]);
		execute_value(&mut session, "ALTER LIVE LET $limit VALUE $invalid;", &invalid)?;
		for source in queries("$limit", "duration_invalid") {
			assert!(execute_value(&mut session, &source, &Object::new()).is_err(), "{source}");
		}
	}
	for source in queries("18446744073709549568s", "duration_overflow") {
		assert!(execute_value(&mut session, &source, &Object::new()).is_err(), "{source}");
	}
	for source in queries("1s", "duration_parent") {
		let results = session.execute_with_options(
			&source,
			&Object::new(),
			crate::gql::QueryExecutionOptions {
				statement_timeout: Some(std::time::Duration::ZERO),
				..crate::gql::QueryExecutionOptions::default()
			},
		)?;
		assert!(results[0].result.is_err(), "{source}");
	}
	for name in ["duration_uuid", "duration_invalid", "duration_overflow", "duration_parent"] {
		assert!(store.begin().named_graph(name)?.is_none(), "{name}");
	}
	Ok(())
}

/// Cancellation by the final MATCH visitor cannot return a successful partial
/// result.
#[test]
fn duration_embedded_match_checks_final_visitor() -> FerrosGrynnResult<()> {
	use super::super::{
		QueryRead,
		TransactionRead,
		initialize_slots,
		visit_match_with_environment,
	};
	use crate::{
		gql::{
			ExecutionControl,
			QueryCancellation,
			QueryExecutionOptions,
			analysis::analyze,
			ir::{LogicalBuildGraphSource, LogicalStatement},
		},
		graph::GraphKind,
	};
	let store = Datastore::in_memory();
	let mut setup = store.begin();
	setup.define_class(ClassSchema::new("person", AtomKind::Node, [])?)?;
	let graph = setup.define_graph("callback_scope", GraphKind::Graph)?;
	for record in ["person:a", "person:b"] {
		let record = record.parse::<RecordId>()?;
		setup.create_atom(record.clone(), PropertyMap::new())?;
		setup.attach_node(graph.id(), &record, None, None)?;
	}
	setup.commit()?;
	let mut transaction = store.begin();
	let snapshot = transaction.query_snapshot()?;
	let read = TransactionRead {
		transaction: &mut transaction,
		snapshot,
		metadata_only: false,
	};
	let source = "BUILD GRAPH callback_copy FROM (MATCH (person AS n) IN GRAPH callback_scope \
	              TIMEOUT 1s RETURN n) NODES n;";
	let program = crate::gql::parse(source, crate::syn::ParserSettings::default())?;
	let logical = analyze(&program, source, &read, &Object::new())?;
	let LogicalStatement::BuildGraph(build) = &logical.statements[0] else {
		return Err(FerrosGrynnError::invalid_argument("test BUILD GRAPH plan missing"));
	};
	let LogicalBuildGraphSource::Match(plan) = &build.source else {
		return Err(FerrosGrynnError::invalid_argument("test embedded MATCH plan missing"));
	};
	let slots = initialize_slots(&logical, &Object::new());
	let cancellation = QueryCancellation::default();
	let control = ExecutionControl::new(
		&QueryExecutionOptions {
			cancellation: Some(cancellation.clone()),
			..QueryExecutionOptions::default()
		},
		None,
	);
	let mut calls = 0;
	let result = visit_match_with_environment(
		&read,
		read.snapshot()?,
		plan,
		&slots,
		source,
		&control,
		&Object::new(),
		&mut |_value| {
			calls += 1;
			if calls == 2 {
				cancellation.cancel();
			}
			Ok(())
		},
	);
	assert_eq!(calls, 2);
	assert!(result.is_err());
	Ok(())
}

/// Ordinary scalar closures retain the resolver after reading a reactive
/// record.
#[test]
fn live_gql_reactive_record_navigation_uses_snapshot_resolver() -> FerrosGrynnResult<()> {
	let store = Datastore::in_memory();
	let mut session = store.session();
	execute_value(
		&mut session,
		"DEFINE CLASS person TYPE NODE; DEFINE FIELD age ON CLASS person TYPE int; CREATE \
		 person:a SET age = 19; CREATE person:b SET age = 23; LIVE LET $record = person:a; LET \
		 $read = | | -> any { RETURN $record.age; };",
		&Object::new(),
	)?;
	for (mutation, record, age) in [
		("RETURN NONE;", "person:a", 19),
		("UPDATE person:a SET age = 20;", "person:a", 20),
		("ALTER LIVE LET $record VALUE person:b;", "person:b", 23),
	] {
		execute_value(&mut session, mutation, &Object::new())?;
		let oracle = execute_value(
			&mut session,
			&format!("SELECT VALUE age FROM {record};"),
			&Object::new(),
		)?;
		assert_eq!(oracle, Value::from(vec![Value::from(age)]));
		for source in [
			"TRAVERSE FROM [0] |> tv::map(|$x| $record.age);",
			"TRAVERSE FROM [$record.age] |> tv::map(|$x| $x);",
		] {
			assert_eq!(execute_value(&mut session, source, &Object::new())?, oracle, "{source}");
		}
		assert_eq!(execute_value(&mut session, "RETURN $read();", &Object::new())?, age.into());
	}
	assert_eq!(
		execute_value(&mut session, "RETURN $record.missing;", &Object::new())?,
		Value::None
	);
	assert_eq!(
		execute_value(
			&mut session,
			"IF true { LET $record = {age: 7}; RETURN [$record.age, $read()]; };",
			&Object::new(),
		)?,
		Value::from(vec![Value::from(7), Value::from(23)])
	);
	let info = execute_value(&mut session, "INFO FOR LIVE LET $record;", &Object::new())?;
	let id = info
		.as_object()
		.and_then(|info| info.get("id"))
		.ok_or_else(|| FerrosGrynnError::invalid_argument("record binding identity missing"))?;
	assert!(
		execute_value(
			&mut session,
			"LET $ordinary = $input; RETURN $ordinary.age;",
			&Object::from_iter([("input", id.clone())]),
		)
		.is_err()
	);
	Ok(())
}

/// Reactive record property reads use the statement snapshot and register
/// their base record dependency independently from the binding identity.
#[test]
fn live_gql_reactive_record_navigation_snapshot_and_ssi() -> FerrosGrynnResult<()> {
	for source in ["RETURN $read();", "TRAVERSE FROM [0] |> tv::map(|$x| $record.age);"] {
		for isolation in ["SNAPSHOT", "READ COMMITTED", "SERIALIZABLE"] {
			let store = Datastore::in_memory();
			let mut session = store.session();
			execute_value(
				&mut session,
				"DEFINE CLASS person TYPE NODE; DEFINE FIELD age ON CLASS person TYPE int; CREATE \
				 person:a SET age = 19; LIVE LET $record = person:a; LET $read = | | -> any { \
				 RETURN $record.age; };",
				&Object::new(),
			)?;
			execute_value(
				&mut session,
				&format!("BEGIN ISOLATION LEVEL {isolation};"),
				&Object::new(),
			)?;
			let initial = execute_value(&mut session, source, &Object::new())?;
			let mut writer = store.begin();
			let age = writer.intern_property("age")?;
			writer.set_base_property(&"person:a".parse()?, age, 20.into())?;
			writer.commit()?;
			let expected = if source.starts_with("RETURN") {
				Value::from(20)
			} else {
				Value::from(vec![Value::from(20)])
			};
			assert_eq!(
				execute_value(&mut session, source, &Object::new())?,
				if isolation == "READ COMMITTED" {
					expected.clone()
				} else {
					initial
				},
				"{source} {isolation}"
			);
			if isolation == "SERIALIZABLE" {
				let commit = session
					.execute("CREATE person:written SET age = 0; COMMIT;", &Object::new())?;
				assert!(commit[0].result.is_ok());
				assert!(commit[1].result.is_err());
				assert!(store.begin().atom(&"person:written".parse()?)?.is_none());
			} else {
				execute_value(&mut session, "CANCEL;", &Object::new())?;
			}
			assert_eq!(execute_value(&mut session, source, &Object::new())?, expected);
		}
	}
	Ok(())
}

/// A small reactive record reference cannot bypass the statement budget when
/// resolving a large owned property.
#[test]
fn live_gql_reactive_record_navigation_property_admission() -> FerrosGrynnResult<()> {
	use super::super::QueryRead;

	let store = Datastore::in_memory();
	let mut setup = store.begin();
	setup.define_class(ClassSchema::new("person", AtomKind::Node, [
		ClassField::new("payload", Type::Array(Box::new(Type::Int))),
		ClassField::new("age", Type::Int),
	])?)?;
	let payload = setup.intern_property("payload")?;
	let age = setup.intern_property("age")?;
	setup.create_atom(
		"person:a".parse()?,
		PropertyMap::from_iter([
			(payload, Value::from(vec![Value::from(1); 20_000])),
			(age, 19.into()),
		]),
	)?;
	let record = "person:a".parse::<RecordId>()?;
	let graph = setup.define_graph("property_scope", crate::graph::GraphKind::Graph)?;
	let fallback = setup.define_graph("property_fallback", crate::graph::GraphKind::Graph)?;
	setup.attach_node(graph.id(), &record, Some("label".repeat(2048).into()), None)?;
	setup.hide_property(graph.id(), &record, age)?;
	setup.set_overlay(graph.id(), &record, payload, Value::from(vec![Value::from(7)]))?;
	setup.commit()?;
	let mut session = store.session();
	execute_value(
		&mut session,
		"LIVE LET $record = person:a; LET $read = | | -> any { RETURN $record.payload; };",
		&Object::new(),
	)?;
	let options = crate::gql::QueryExecutionOptions {
		barrier_memory_bytes: std::num::NonZeroUsize::new(4096)
			.ok_or_else(|| FerrosGrynnError::invalid_argument("navigation budget is zero"))?,
		..crate::gql::QueryExecutionOptions::default()
	};
	for query in [
		"RETURN $read();",
		"TRAVERSE FROM [0] |> tv::map(|$x| $record.payload);",
		"TRAVERSE FROM [$record.payload] |> tv::limit(1);",
	] {
		let result = session.execute_with_options(query, &Object::new(), options.clone())?;
		assert!(result[0].result.is_err(), "{query}");
	}
	let result =
		session.execute_with_options("RETURN $record.age;", &Object::new(), options.clone())?;
	assert_eq!(result[0].result.as_ref().map_err(Clone::clone)?.value(), &Value::from(19));
	let root = store.inner.current_root();
	let control = crate::gql::ExecutionControl::new(&options, None);
	let observe = |_key| {};
	let work = |_amount| Ok(());
	let read = crate::gql::execution::root_read::RootRead::new(
		&root,
		&control,
		&observe,
		&work,
		std::env::temp_dir(),
	);
	let snapshot = read.snapshot()?;
	assert_eq!(read.record_field(snapshot, &record, None, "age", Some(&control))?, Value::from(19));
	assert_eq!(
		read.record_field(snapshot, &record, Some(&graph), "age", Some(&control))?,
		Value::None
	);
	assert_eq!(
		read.record_field(snapshot, &record, Some(&graph), "payload", Some(&control))?,
		Value::from(vec![Value::from(7)])
	);
	assert_eq!(
		read.record_field(snapshot, &record, Some(&fallback), "age", Some(&control))?,
		Value::from(19)
	);
	assert_eq!(
		read.record_field(snapshot, &record, Some(&graph), "id", Some(&control))?,
		Value::from(record.clone())
	);
	assert!(read.record_field(snapshot, &record, Some(&graph), "label", Some(&control)).is_err());
	assert!(read.record_field(snapshot, &record, None, "payload", Some(&control)).is_err());
	Ok(())
}

/// Invalid reactive counts are rejected even when the source has no rows.
#[test]
fn live_gql_traversal_counts_reject_invalid_values() -> FerrosGrynnResult<()> {
	let overflow = rust_decimal::Decimal::from(u64::MAX) + rust_decimal::Decimal::ONE;
	for value in [
		Value::from(-1),
		Value::from(1.5),
		Value::from(overflow),
		Value::from("2"),
		Value::from(true),
		Value::None,
		Value::Null,
		Value::from(Object::new()),
		Value::from(vec![Value::from(1)]),
	] {
		let store = Datastore::in_memory();
		let mut session = store.session();
		let parameters = Object::from_iter([("input", value)]);
		execute_value(&mut session, "LIVE LET $count = $input;", &parameters)?;
		for query in [
			"TRAVERSE FROM [1, 2] |> tv::limit($count);",
			"TRAVERSE FROM [1, 2] |> tv::skip($count);",
			"TRAVERSE FROM [] |> tv::limit($count);",
			"TRAVERSE FROM [] |> tv::skip($count);",
		] {
			assert!(execute_value(&mut session, query, &Object::new()).is_err(), "{query}");
		}
	}
	for value in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
		let store = Datastore::in_memory();
		let mut session = store.session();
		let parameters = Object::from_iter([("count", Value::from(value))]);
		for query in
			["TRAVERSE FROM [] |> tv::limit($count);", "TRAVERSE FROM [] |> tv::skip($count);"]
		{
			assert!(execute_value(&mut session, query, &parameters).is_err(), "{query}");
		}
	}
	Ok(())
}

/// Ordinary and reactive counts preserve bulk multiplicity and exact numbers
/// throughout the unsigned range.
#[test]
fn live_gql_traversal_counts_match_slice_oracle() -> FerrosGrynnResult<()> {
	let source = [1, 1, 1, 2];
	for (value, boundary) in [
		(Value::from(0), 0),
		(Value::from(1), 1),
		(Value::from(2), 2),
		(Value::from(3.0), 3),
		(Value::from(rust_decimal::Decimal::from(3)), 3),
		(Value::from(4), 4),
		(Value::from(9_007_199_254_740_993_u64), 4),
		(Value::from(9_223_372_036_854_775_808_u64), 4),
		(Value::from(u64::MAX), 4),
	] {
		let store = Datastore::in_memory();
		let mut session = store.session();
		let parameters = Object::from_iter([("input", value)]);
		execute_value(&mut session, "LET $fixed = $input; LIVE LET $count = $input;", &parameters)?;
		for binding in ["fixed", "count"] {
			for (step, expected) in [("limit", &source[..boundary]), ("skip", &source[boundary..])]
			{
				let query = format!("TRAVERSE FROM [1, 1, 1, 2] |> tv::{step}(${binding});");
				assert_eq!(
					execute_value(&mut session, &query, &Object::new())?,
					Value::from(expected.iter().copied().map(Value::from).collect::<Vec<_>>()),
					"{query}"
				);
			}
		}
	}
	Ok(())
}

/// Count preparation reads the transaction's binding version and records an
/// SSI dependency independently of the source values.
#[test]
fn live_gql_traversal_counts_snapshot_and_ssi() -> FerrosGrynnResult<()> {
	for step in ["limit", "skip"] {
		for isolation in ["SNAPSHOT", "READ COMMITTED", "SERIALIZABLE"] {
			let store = Datastore::in_memory();
			let mut session = store.session();
			let info = execute_value(
				&mut session,
				"DEFINE CLASS person TYPE NODE; LIVE LET $count = 1; INFO FOR LIVE LET $count;",
				&Object::new(),
			)?;
			let uuid = info
				.as_object()
				.and_then(|info| info.get("id"))
				.and_then(Value::as_uuid)
				.ok_or_else(|| FerrosGrynnError::invalid_argument("count binding UUID missing"))?;
			let binding = crate::live::LiveBindingId::from_uuid(*uuid)?;
			execute_value(
				&mut session,
				&format!("BEGIN ISOLATION LEVEL {isolation};"),
				&Object::new(),
			)?;
			let query = format!("TRAVERSE FROM [1, 1, 2] |> tv::{step}($count);");
			let initial = execute_value(&mut session, &query, &Object::new())?;
			let mut writer = store.begin();
			writer.replace_live_binding_value(binding, 2.into())?;
			writer.commit()?;
			let expected = if step == "limit" {
				Value::from(vec![Value::from(1), Value::from(1)])
			} else {
				Value::from(vec![Value::from(2)])
			};
			assert_eq!(
				execute_value(&mut session, &query, &Object::new())?,
				if isolation == "READ COMMITTED" {
					expected.clone()
				} else {
					initial
				},
				"{step} {isolation}"
			);
			if isolation == "SERIALIZABLE" {
				let commit = session.execute("CREATE person:written; COMMIT;", &Object::new())?;
				assert!(commit[0].result.is_ok());
				assert!(commit[1].result.is_err());
				assert!(store.begin().atom(&"person:written".parse()?)?.is_none());
			} else {
				execute_value(&mut session, "CANCEL;", &Object::new())?;
			}
			assert_eq!(execute_value(&mut session, &query, &Object::new())?, expected);
		}
	}
	Ok(())
}

/// An ordinary graph parameter freezes the qualified LIVE domain at
/// registration rather than following subsequent lexical replacement.
#[test]
fn live_gql_frozen_graph_scope_parameter() -> FerrosGrynnResult<()> {
	use crate::graph::GraphKind;
	let store = Datastore::in_memory();
	let mut setup = store.begin();
	setup.define_class(ClassSchema::new("person", AtomKind::Node, [ClassField::new(
		"age",
		Type::Int,
	)])?)?;
	let age = setup.intern_property("age")?;
	let record = "person:a".parse::<RecordId>()?;
	setup.create_atom(record.clone(), PropertyMap::from_iter([(age, 19.into())]))?;
	let first = setup.define_graph("frozen_scope_one", GraphKind::Graph)?;
	let second = setup.define_graph("frozen_scope_two", GraphKind::Graph)?;
	for (graph, value) in [(&first, 11), (&second, 22)] {
		setup.attach_node(graph.id(), &record, None, None)?;
		setup.set_overlay(graph.id(), &record, age, value.into())?;
	}
	setup.commit()?;
	let parameters = Object::from_iter([
		("first", Value::from(first.clone())),
		("second", Value::from(second.clone())),
	]);
	let mut session = store.session();
	let id = identity(execute_value(
		&mut session,
		"LET $scope = $first; LET $sub = LIVE SELECT VALUE age FROM person IN GRAPH $scope; \
		 RETURN $sub;",
		&parameters,
	)?)?;
	assert!(matches!(store.begin().live_info(id)?.query().source(),
		crate::live::LiveSource::GraphClass { graph, .. } if graph.id() == first.id()));
	let mut handle = session.take_live_subscription(id)?;
	execute_value(&mut session, "ALTER LET $scope VALUE $second;", &parameters)?;
	let mut update = store.begin();
	update.set_overlay(first.id(), &record, age, 13.into())?;
	update.set_overlay(second.id(), &record, age, 24.into())?;
	update.commit()?;
	let LiveNext::Delivery(delivery) = handle.next()? else {
		return Err(FerrosGrynnError::invalid_argument("frozen graph delivery missing"));
	};
	let LivePayload::Batch(batch) = delivery.payload() else {
		return Err(FerrosGrynnError::invalid_argument("frozen graph batch missing"));
	};
	assert_eq!(batch.events().len(), 1);
	let event = &batch.events()[0];
	assert!(matches!(event.identity(), LiveIdentity::Occurrence(occurrence)
		if occurrence.graph().id() == first.id() && occurrence.atom().id() == &record));
	assert_eq!(event.before(), Some(&Value::from(11)));
	assert_eq!(event.after(), Some(&Value::from(13)));
	handle.ack(delivery.token())?;
	let replacement = identity(execute_value(
		&mut session,
		"LIVE SELECT VALUE age FROM person IN GRAPH $scope;",
		&Object::new(),
	)?)?;
	let mut replacement = session.take_live_subscription(replacement)?;
	let mut update = store.begin();
	update.set_overlay(first.id(), &record, age, 15.into())?;
	update.set_overlay(second.id(), &record, age, 26.into())?;
	update.commit()?;
	for (handle, graph, value) in [(&mut handle, &first, 15), (&mut replacement, &second, 26)] {
		let LiveNext::Delivery(delivery) = handle.next()? else {
			return Err(FerrosGrynnError::invalid_argument("fresh graph delivery missing"));
		};
		let LivePayload::Batch(batch) = delivery.payload() else {
			return Err(FerrosGrynnError::invalid_argument("fresh graph batch missing"));
		};
		assert_eq!(batch.events().len(), 1);
		assert!(matches!(batch.events()[0].identity(), LiveIdentity::Occurrence(occurrence)
			if occurrence.graph().id() == graph.id() && occurrence.atom().id() == &record));
		assert_eq!(batch.events()[0].after(), Some(&Value::from(value)));
		handle.ack(delivery.token())?;
	}
	for (query, parameters) in [
		(
			"LIVE SELECT VALUE age FROM person:a IN GRAPH $first;",
			Object::from_iter([("first", Value::from(first.clone()))]),
		),
		(
			"LIVE SELECT VALUE age FROM person:a IN GRAPH $named;",
			Object::from_iter([("named", Value::from("frozen_scope_one"))]),
		),
		(
			"IF true { LET $scope = $first; LET $inner = LIVE SELECT VALUE age FROM person:a IN \
			 GRAPH $scope; RETURN $inner; };",
			Object::from_iter([("first", Value::from(first.clone()))]),
		),
	] {
		let id = identity(execute_value(&mut session, query, &parameters)?)?;
		assert!(
			matches!(store.begin().live_info(id)?.query().source(),
			crate::live::LiveSource::GraphRecord { graph, record: selected }
			if graph.id() == first.id() && selected == &record),
			"{query}"
		);
	}
	let pending = identity(execute_value(
		&mut session,
		"BEGIN; LET $rolled = LIVE SELECT VALUE age FROM person IN GRAPH $first; RETURN $rolled;",
		&parameters,
	)?)?;
	execute_value(&mut session, "CANCEL;", &Object::new())?;
	assert!(store.begin().live_info(pending).is_err());
	for value in [Value::from(1), Value::None, Value::from(crate::values::Uuid::now_v7())] {
		assert!(
			execute_value(
				&mut session,
				"LIVE SELECT VALUE age FROM person IN GRAPH $wrong;",
				&Object::from_iter([("wrong", value)]),
			)
			.is_err()
		);
	}
	execute_value(&mut session, "LIVE LET $tracked = $first;", &parameters)?;
	let tracked = identity(execute_value(
		&mut session,
		"LIVE SELECT VALUE age FROM person IN GRAPH $tracked;",
		&Object::new(),
	)?)?;
	assert!(matches!(
		store.begin().live_info(tracked)?.query().source(),
		crate::live::LiveSource::GraphClassExpr { .. }
	));
	session.take_live_subscription(tracked)?.close()?;
	let mut remove = store.begin();
	remove.remove_graph("frozen_scope_one")?;
	remove.commit()?;
	assert!(
		execute_value(
			&mut session,
			"LIVE SELECT VALUE age FROM person IN GRAPH $first;",
			&parameters,
		)
		.is_err()
	);
	Ok(())
}

/// Root and nested graph recipes follow identities through a binding chain.
#[test]
fn live_gql_scope_expression_binding_chain() -> FerrosGrynnResult<()> {
	use crate::graph::GraphKind;
	let store = Datastore::in_memory();
	let mut setup = store.begin();
	setup.define_class(ClassSchema::new("person", AtomKind::Node, [ClassField::new(
		"age",
		Type::Int,
	)])?)?;
	let age = setup.intern_property("age")?;
	let record: RecordId = "person:a".parse()?;
	setup.create_atom(record.clone(), PropertyMap::from_iter([(age, 19.into())]))?;
	let first = setup.define_graph("tracked_scope_a", GraphKind::Graph)?;
	let second = setup.define_graph("tracked_scope_b", GraphKind::Graph)?;
	setup.attach_node(first.id(), &record, None, None)?;
	setup.set_overlay(first.id(), &record, age, 11.into())?;
	setup.commit()?;
	let parameters = Object::from_iter([
		("first", Value::from(first.clone())),
		("second", Value::from(second.clone())),
	]);
	let mut session = store.session();
	let id = identity(execute_value(
		&mut session,
		"LIVE LET $scope = $first; LIVE LET $chain = $scope; LIVE LET $size = (SELECT VALUE \
		 count() FROM person IN GRAPH $scope); LET $sub = LIVE SELECT VALUE age FROM person:a IN \
		 GRAPH $chain; RETURN $sub;",
		&parameters,
	)?)?;
	let mut handle = session.take_live_subscription(id)?;
	assert_eq!(execute_value(&mut session, "RETURN $size;", &Object::new())?, Value::from(1));
	assert!(execute_value(&mut session, "REMOVE LIVE LET $scope;", &Object::new()).is_err());
	execute_value(&mut session, "ALTER LIVE LET $scope VALUE $second;", &parameters)?;
	assert_eq!(execute_value(&mut session, "RETURN $size;", &Object::new())?, Value::from(0));
	let LiveNext::Delivery(delivery) = handle.next()? else {
		return Err(FerrosGrynnError::invalid_argument("scope leave missing"));
	};
	let LivePayload::Batch(batch) = delivery.payload() else {
		return Err(FerrosGrynnError::invalid_argument("scope leave batch missing"));
	};
	assert_eq!(batch.events().len(), 1);
	assert_eq!(batch.events()[0].before(), Some(&Value::from(11)));
	assert_eq!(batch.events()[0].after(), None);
	handle.ack(delivery.token())?;
	let mut attach = store.begin();
	attach.attach_node(second.id(), &record, None, None)?;
	attach.set_overlay(second.id(), &record, age, 22.into())?;
	attach.commit()?;
	assert_eq!(execute_value(&mut session, "RETURN $size;", &Object::new())?, Value::from(1));
	let LiveNext::Delivery(delivery) = handle.next()? else {
		return Err(FerrosGrynnError::invalid_argument("scope enter missing"));
	};
	let LivePayload::Batch(batch) = delivery.payload() else {
		return Err(FerrosGrynnError::invalid_argument("scope enter batch missing"));
	};
	let LiveIdentity::Occurrence(occurrence) = batch.events()[0].identity() else {
		return Err(FerrosGrynnError::invalid_argument("scope enter identity missing"));
	};
	assert_eq!(occurrence.graph().id(), second.id());
	assert_eq!(batch.events()[0].before(), None);
	assert_eq!(batch.events()[0].after(), Some(&Value::from(22)));
	handle.ack(delivery.token())?;
	handle.close()?;
	Ok(())
}

/// Computed scope functions retain concrete versions and external record reads.
#[test]
fn live_gql_scope_expression_pure_data_dependency() -> FerrosGrynnResult<()> {
	use crate::graph::GraphKind;
	let store = Datastore::in_memory();
	let mut setup = store.begin();
	setup.define_class(ClassSchema::new("person", AtomKind::Node, [ClassField::new(
		"age",
		Type::Int,
	)])?)?;
	setup.define_class(ClassSchema::new("policy", AtomKind::Node, [ClassField::new(
		"name",
		Type::String,
	)])?)?;
	let age = setup.intern_property("age")?;
	let name = setup.intern_property("name")?;
	let record: RecordId = "person:a".parse()?;
	let policy: RecordId = "policy:config".parse()?;
	setup.create_atom(record.clone(), PropertyMap::from_iter([(age, 1.into())]))?;
	setup.create_atom(policy.clone(), PropertyMap::from_iter([(name, "computed_a".into())]))?;
	let first = setup.define_graph("computed_a", GraphKind::Graph)?;
	let second = setup.define_graph("computed_b", GraphKind::Graph)?;
	for (graph, value) in [(&first, 11), (&second, 22)] {
		setup.attach_node(graph.id(), &record, None, None)?;
		setup.set_overlay(graph.id(), &record, age, value.into())?;
	}
	setup.commit()?;
	let mut session = store.session();
	let id = identity(execute_value(
		&mut session,
		"DEFINE PURE FUNCTION fn::scope(name: any) -> any { RETURN $name; }; LET $sub = LIVE \
		 SELECT VALUE age FROM person IN GRAPH fn::scope(policy:config.name); RETURN $sub;",
		&Object::new(),
	)?)?;
	let registered = store.inner.current_root();
	assert_eq!(registered.live_catalog().queries[&id].functions.len(), 1);
	let mut handle = session.take_live_subscription(id)?;
	let mut change = store.begin();
	change.set_base_property(&policy, name, "computed_b".into())?;
	change.commit()?;
	let LiveNext::Delivery(delivery) = handle.next()? else {
		return Err(FerrosGrynnError::invalid_argument("computed scope delivery missing"));
	};
	let LivePayload::Batch(batch) = delivery.payload() else {
		return Err(FerrosGrynnError::invalid_argument("computed scope batch missing"));
	};
	assert_eq!(batch.events().len(), 2);
	for event in batch.events() {
		let LiveIdentity::Occurrence(occurrence) = event.identity() else {
			return Err(FerrosGrynnError::invalid_argument("computed scope identity missing"));
		};
		if occurrence.graph().id() == first.id() {
			assert_eq!(event.before(), Some(&Value::from(11)));
			assert_eq!(event.after(), None);
		} else {
			assert_eq!(occurrence.graph().id(), second.id());
			assert_eq!(event.before(), None);
			assert_eq!(event.after(), Some(&Value::from(22)));
		}
	}
	handle.ack(delivery.token())?;
	handle.close()?;
	Ok(())
}

/// Resolving a frozen graph name observes the graph catalog in addition to
/// the successful graph identity before staging the registration.
#[test]
fn live_gql_frozen_graph_scope_catalog_ssi() -> FerrosGrynnResult<()> {
	use crate::graph::GraphKind;
	let store = Datastore::in_memory();
	let mut setup = store.begin();
	setup.define_class(ClassSchema::new("item", AtomKind::Node, [])?)?;
	setup.define_graph("named_scope", GraphKind::Graph)?;
	setup.commit()?;
	let mut session = store.session();
	let id = identity(execute_value(
		&mut session,
		"BEGIN ISOLATION LEVEL SERIALIZABLE; LET $sub = LIVE SELECT * FROM item IN GRAPH $scope; \
		 RETURN $sub;",
		&Object::from_iter([("scope", Value::from("named_scope"))]),
	)?)?;
	let mut concurrent = store.begin();
	concurrent.define_graph("other_scope", GraphKind::Graph)?;
	concurrent.commit()?;
	assert!(execute_value(&mut session, "COMMIT;", &Object::new()).is_err());
	assert!(store.begin().live_info(id).is_err());
	assert!(session.take_live_subscription(id).is_err());
	Ok(())
}

/// MATCH resolves GraphRef captures before planning and preserves the same
/// qualified domain in nested consumers and graph assembly.
#[test]
fn live_gql_binding_match_graph() -> FerrosGrynnResult<()> {
	use crate::graph::{GraphKind, GraphRead, GraphRefReader};

	let store = Datastore::in_memory();
	let mut setup = store.begin();
	setup.define_class(ClassSchema::new("person", AtomKind::Node, [])?)?;
	setup.define_class(ClassSchema::new("policy", AtomKind::Node, [])?)?;
	setup.create_atom("policy:one".parse()?, PropertyMap::new())?;
	setup.define_class(ClassSchema::new("link", AtomKind::Edge, [])?)?;
	let first = setup.define_graph("scope_one", GraphKind::Graph)?;
	let second = setup.define_graph("scope_two", GraphKind::Graph)?;
	for (graph, record) in [(first.id(), "person:a"), (second.id(), "person:b")] {
		let record = record.parse::<RecordId>()?;
		setup.create_atom(record.clone(), PropertyMap::new())?;
		setup.attach_node(graph, &record, None, None)?;
	}
	setup.commit()?;
	let params = Object::from_iter([
		("first", Value::from(first.clone())),
		("second", Value::from(second.clone())),
	]);
	let mut session = store.session();
	let query = "MATCH (person AS n) IN GRAPH $scope RETURN n;";
	let assert_domain = |value: &Value, graph: &crate::graph::GraphRef, record: &str| {
		let rows = value
			.as_array()
			.ok_or_else(|| FerrosGrynnError::invalid_argument("MATCH rows missing"))?;
		assert_eq!(rows.len(), 1);
		let atom = rows
			.get(0)
			.and_then(Value::as_object)
			.and_then(|row| row.get("n"))
			.and_then(Value::as_atom_occurrence)
			.ok_or_else(|| FerrosGrynnError::invalid_argument("MATCH occurrence missing"))?;
		assert_eq!(atom.graph().id(), graph.id());
		assert_eq!(atom.atom().id().to_string(), record);
		Ok::<(), FerrosGrynnError>(())
	};
	assert_domain(
		&execute_value(
			&mut session,
			"LET $fixed = $first; MATCH (person AS n) IN GRAPH $fixed RETURN n;",
			&params,
		)?,
		&first,
		"person:a",
	)?;
	assert_domain(
		&execute_value(
			&mut session,
			"ALTER LET $fixed VALUE $second; MATCH (person AS n) IN GRAPH $fixed RETURN n;",
			&params,
		)?,
		&second,
		"person:b",
	)?;
	execute_value(
		&mut session,
		"LIVE LET $scope = IF (SELECT VALUE count() FROM policy) > 1 THEN $second ELSE $first END;",
		&params,
	)?;
	assert_domain(&execute_value(&mut session, query, &Object::new())?, &first, "person:a")?;
	execute_value(&mut session, "BEGIN READ ONLY;", &Object::new())?;
	let mut writer = store.begin();
	writer.create_atom("policy:two".parse()?, PropertyMap::new())?;
	writer.commit()?;
	assert_domain(&execute_value(&mut session, query, &Object::new())?, &first, "person:a")?;
	execute_value(&mut session, "CANCEL;", &Object::new())?;
	assert_domain(&execute_value(&mut session, query, &Object::new())?, &second, "person:b")?;
	let embedded = "TRAVERSE FROM $scope |> tv::V() |> tv::as('a') |> tv::match(MATCH (person AS \
	                a) IN GRAPH $scope RETURN a) |> tv::count();";
	assert_eq!(
		execute_value(&mut session, embedded, &Object::new())?,
		Value::from(vec![Value::from(1)])
	);
	let mut hashes = Vec::new();
	for (parameter, graph) in [("first", &first), ("second", &second)] {
		execute_value(
			&mut session,
			&format!("ALTER LIVE LET $scope VALUE ${parameter};"),
			&params,
		)?;
		let explained = execute_value(
			&mut session,
			&format!("EXPLAIN FULL FORMAT JSON {query}"),
			&Object::new(),
		)?;
		let explained = explained
			.as_object()
			.ok_or_else(|| FerrosGrynnError::invalid_argument("MATCH EXPLAIN missing"))?;
		let nodes = explained
			.get("nodes")
			.and_then(Value::as_array)
			.ok_or_else(|| FerrosGrynnError::invalid_argument("MATCH EXPLAIN nodes missing"))?;
		for node in nodes.iter().filter_map(Value::as_object) {
			assert_eq!(node.get("graph_id"), Some(&Value::from(graph.id().get())));
			assert_eq!(node.get("graph_binding"), Some(&Value::from("resolved")));
		}
		hashes.push(explained.get("plan_hash").cloned());
	}
	assert_ne!(hashes[0], hashes[1]);
	let explained = execute_value(
		&mut session,
		"EXPLAIN FULL FORMAT JSON BUILD GRAPH warned_scope FROM (MATCH (person AS n) IN GRAPH \
		 $scope RETURN n) NODES n;",
		&Object::new(),
	)?;
	assert!(
		explained
			.as_object()
			.and_then(|plan| plan.get("warnings"))
			.and_then(Value::as_array)
			.is_some_and(|warnings| warnings
				.iter()
				.filter_map(Value::as_str)
				.any(|warning| warning.contains("MATCH GRAPH is bound at invocation")))
	);
	let built = execute_value(
		&mut session,
		"MATCH (person AS n) IN GRAPH $scope RETURN GRAPH { nodes: n, edges: [] };",
		&Object::new(),
	)?;
	let Value::Graph(built) = built else {
		return Err(FerrosGrynnError::invalid_argument("MATCH graph result missing"));
	};
	assert_eq!(built.node_ids()?.count(), 1);
	let built = execute_value(
		&mut session,
		"BUILD GRAPH selected_scope FROM (MATCH (person AS n) IN GRAPH $scope RETURN n) NODES n;",
		&Object::new(),
	)?;
	let reference = built
		.as_graph_ref()
		.ok_or_else(|| FerrosGrynnError::invalid_argument("BUILD graph reference missing"))?;
	assert_eq!(store.begin().read_graph(reference)?.node_ids()?.count(), 1);
	let explained = execute_value(
		&mut session,
		"EXPLAIN FULL FORMAT JSON BUILD GRAPH dynamic_tensor FROM (MATCH path = (person AS \
		 a)-[link*0..2 ALL SHORTEST]->(person AS b) IN GRAPH $scope RETURN path) NODES path;",
		&Object::new(),
	)?;
	let tensor = explained
		.as_object()
		.and_then(|plan| plan.get("nodes"))
		.and_then(Value::as_array)
		.and_then(|nodes| {
			nodes
				.iter()
				.filter_map(Value::as_object)
				.find(|node| node.get("operator") == Some(&Value::from("TensorAllShortest")))
		})
		.ok_or_else(|| FerrosGrynnError::invalid_argument("deferred tensor plan missing"))?;
	assert_eq!(tensor.get("format"), Some(&Value::from("DYNAMIC")));
	assert_eq!(tensor.get("scope"), Some(&Value::from("DYNAMIC")));
	assert_eq!(tensor.get("graph_binding"), Some(&Value::from("deferred")));
	assert_eq!(tensor.get("graph_id"), Some(&Value::None));
	assert!(!tensor.contains_key("index_id"));
	let degree = execute_value(
		&mut session,
		"TRAVERSE FROM MATCH (person AS n) IN GRAPH $scope RETURN n |> tv::degree();",
		&Object::new(),
	)?;
	assert!(degree.as_array().is_some_and(|rows| rows.len() == 1));
	let degree_atom = degree
		.as_array()
		.and_then(|rows| rows.get(0))
		.and_then(Value::as_object)
		.and_then(|report| report.get("values"))
		.and_then(Value::as_array)
		.and_then(|rows| rows.get(0))
		.and_then(Value::as_object)
		.and_then(|row| row.get("atom"))
		.and_then(Value::as_atom_occurrence)
		.ok_or_else(|| FerrosGrynnError::invalid_argument("degree occurrence missing"))?;
	assert_eq!(degree_atom.graph().id(), second.id());
	for value in ["'scope_two'", "1", "NONE"] {
		execute_value(
			&mut session,
			&format!("ALTER LIVE LET $scope VALUE {value};"),
			&Object::new(),
		)?;
		assert!(execute_value(&mut session, query, &Object::new()).is_err());
		assert!(
			execute_value(
				&mut session,
				"MATCH (person:missing) IN GRAPH $scope RETURN 1;",
				&Object::new()
			)
			.is_err()
		);
		assert!(
			execute_value(
				&mut session,
				&format!("EXPLAIN FULL FORMAT JSON {query}"),
				&Object::new()
			)
			.is_err()
		);
	}
	let mut removal = store.begin();
	removal.remove_graph(first.name())?;
	let replacement = removal.define_graph(first.name().as_str(), GraphKind::Graph)?;
	assert_ne!(replacement.id(), first.id());
	removal.commit()?;
	assert!(
		execute_value(&mut session, "MATCH (person:missing) IN GRAPH $first RETURN 1;", &params)
			.is_err()
	);
	Ok(())
}

/// Search options resolve fixed snapshot values without coercing dynamic
/// query types or relaxing the PURE contract of executable metrics.
#[test]
fn live_gql_binding_search_inputs() -> FerrosGrynnResult<()> {
	use crate::values::Vector;

	let store = Datastore::in_memory();
	let mut session = store.session();
	execute_value(
		&mut session,
		"DEFINE ANALYZER words TOKENIZERS blank FILTERS lowercase; DEFINE CLASS document TYPE \
		 NODE; DEFINE FIELD title ON CLASS document TYPE string; DEFINE FIELD embedding ON CLASS \
		 document TYPE vector<f32>; DEFINE INDEX by_title ON CLASS document FIELDS title FULLTEXT \
		 ANALYZER words SCOPE BASE;",
		&Object::new(),
	)?;
	for (id, title, vector) in [
		("a", "Rust graph", vec![1.0_f32, 0.0]),
		("b", "Rust storage", vec![0.0_f32, 1.0]),
		("c", "Graph only", vec![-1.0_f32, 0.0]),
	] {
		execute_value(
			&mut session,
			&format!("CREATE document:{id} SET title = $title, embedding = $vector;"),
			&Object::from_iter([
				("title", Value::from(title)),
				("vector", Value::Vector(Vector::F32(vector))),
			]),
		)?;
	}
	execute_value(
		&mut session,
		"DEFINE INDEX by_embedding ON CLASS document FIELDS embedding HNSW DIMENSION 2 TYPE F32 \
		 DIST EUCLIDEAN SCOPE BASE; LIVE LET $needle = 'rust'; LIVE LET $k = 1; LIVE LET $ef = 4; \
		 LIVE LET $exponent = 2; LIVE LET $query = $input;",
		&Object::from_iter([("input", Value::Vector(Vector::F32(vec![1.0, 0.0])))]),
	)?;
	for (needle, names) in
		[("rust", ["document:a", "document:b"]), ("graph", ["document:a", "document:c"])]
	{
		execute_value(
			&mut session,
			&format!("ALTER LIVE LET $needle VALUE '{needle}';"),
			&Object::new(),
		)?;
		let expected = Value::from(
			names
				.into_iter()
				.map(|id| id.parse::<RecordId>().map(Value::from))
				.collect::<FerrosGrynnResult<Vec<_>>>()?,
		);
		for access in ["WITH NOINDEX", ""] {
			assert_eq!(
				execute_value(
					&mut session,
					&format!(
						"SELECT VALUE id FROM document {access} WHERE title @@ $needle ORDER BY \
						 id;"
					),
					&Object::new(),
				)?,
				expected
			);
		}
	}
	let queries = [
		"SELECT VALUE id FROM document WITH NOINDEX WHERE embedding <| $k |> $query;",
		"SELECT VALUE id FROM document WITH NOINDEX WHERE embedding <| $k, MINKOWSKI($exponent) \
		 |> $query;",
		"SELECT VALUE id FROM document WHERE embedding <| $k |> $query;",
	];
	for (vector, k, names) in [
		(vec![1.0, 0.0], 1, vec!["document:a"]),
		(vec![-1.0, 0.0], 2, vec!["document:c", "document:b"]),
	] {
		execute_value(
			&mut session,
			"ALTER LIVE LET $query VALUE $input; ALTER LIVE LET $k VALUE $count;",
			&Object::from_iter([
				("input", Value::Vector(Vector::F32(vector))),
				("count", Value::from(k)),
			]),
		)?;
		let expected = Value::from(
			names
				.into_iter()
				.map(|id| id.parse::<RecordId>().map(Value::from))
				.collect::<FerrosGrynnResult<Vec<_>>>()?,
		);
		for query in queries {
			assert_eq!(execute_value(&mut session, query, &Object::new())?, expected, "{query}");
		}
	}
	execute_value(&mut session, "ALTER LIVE LET $exponent VALUE 1;", &Object::new())?;
	assert_eq!(
		execute_value(&mut session, queries[1], &Object::new())?,
		Value::from(vec![
			Value::from("document:c".parse::<RecordId>()?),
			Value::from("document:a".parse::<RecordId>()?)
		])
	);
	execute_value(&mut session, "ALTER LIVE LET $exponent VALUE 0;", &Object::new())?;
	assert!(execute_value(&mut session, queries[1], &Object::new()).is_err());
	execute_value(&mut session, "ALTER LIVE LET $k VALUE 0;", &Object::new())?;
	assert!(execute_value(&mut session, queries[0], &Object::new()).is_err());
	execute_value(
		&mut session,
		"ALTER LIVE LET $k VALUE 1; ALTER LIVE LET $query VALUE $input; ALTER LIVE LET $needle \
		 VALUE 7;",
		&Object::from_iter([("input", Value::Vector(Vector::F64(vec![1.0, 0.0])))]),
	)?;
	let wrong_kind =
		execute_value(&mut session, queries[0], &Object::new()).err().ok_or_else(|| {
			FerrosGrynnError::invalid_argument("KNN accepted a different element kind")
		})?;
	assert!(wrong_kind.to_string().contains("element types must match"), "{wrong_kind}");
	assert!(
		execute_value(
			&mut session,
			"SELECT id FROM document WHERE title @@ $needle;",
			&Object::new()
		)
		.is_err()
	);
	Ok(())
}

/// An empty source still admits query tokenization before any row pull;
/// payload size alone does not cover token/filter expansion or work.
#[test]
fn live_gql_binding_search_query_admission() -> FerrosGrynnResult<()> {
	use std::num::NonZeroUsize;

	use crate::gql::QueryExecutionOptions;

	let store = Datastore::in_memory();
	let mut session = store.session();
	execute_value(
		&mut session,
		"DEFINE ANALYZER words TOKENIZERS blank FILTERS lowercase; DEFINE CLASS document TYPE \
		 NODE; DEFINE FIELD title ON CLASS document TYPE string; DEFINE INDEX by_title ON CLASS \
		 document FIELDS title FULLTEXT ANALYZER words SCOPE BASE; LIVE LET $needle = $input;",
		&Object::from_iter([("input", Value::from("rust ".repeat(4096)))]),
	)?;
	for options in [
		QueryExecutionOptions {
			barrier_memory_bytes: NonZeroUsize::new(128 * 1024).ok_or_else(|| {
				FerrosGrynnError::invalid_argument("search fixture scratch is zero")
			})?,
			..QueryExecutionOptions::default()
		},
		QueryExecutionOptions {
			max_loop_iterations: 128,
			..QueryExecutionOptions::default()
		},
	] {
		let result = session.execute_with_options(
			"SELECT VALUE id FROM document WITH NOINDEX WHERE title @@ $needle;",
			&Object::new(),
			options,
		)?;
		let failure = result
			.into_iter()
			.next()
			.ok_or_else(|| {
				FerrosGrynnError::invalid_argument("search fixture missing statement result")
			})?
			.result
			.err()
			.ok_or_else(|| FerrosGrynnError::invalid_argument("search query bypassed admission"))?;
		assert!(failure.to_string().contains("LIVE LET value"), "{failure}");
	}
	execute_value(
		&mut session,
		"DEFINE PURE FUNCTION fn::whole(value: string) -> array<object> { RETURN [{term: $value, \
		 start: 0, end: string::len($value)}]; }; DEFINE ANALYZER whole FUNCTION fn::whole \
		 FILTERS uppercase; DEFINE CLASS fragment TYPE NODE; DEFINE FIELD title ON CLASS fragment \
		 TYPE string; DEFINE INDEX by_title ON CLASS fragment FIELDS title FULLTEXT ANALYZER \
		 whole SCOPE BASE; LIVE LET $term = 'word';",
		&Object::new(),
	)?;
	let udf_query = "SELECT VALUE id FROM fragment WITH NOINDEX WHERE title @@ $term;";
	assert_eq!(
		execute_value(&mut session, udf_query, &Object::new())?,
		Value::from(Vec::<Value>::new())
	);
	execute_value(
		&mut session,
		"ALTER LIVE LET $term VALUE $input;",
		&Object::from_iter([("input", Value::from("rust".repeat(512)))]),
	)?;
	let results =
		session.execute_with_options(udf_query, &Object::new(), QueryExecutionOptions {
			max_loop_iterations: 128,
			..QueryExecutionOptions::default()
		})?;
	let failure = results
		.into_iter()
		.next()
		.ok_or_else(|| FerrosGrynnError::invalid_argument("search UDF fixture result missing"))?
		.result
		.err()
		.ok_or_else(|| FerrosGrynnError::invalid_argument("search UDF bypassed query admission"))?;
	assert!(failure.to_string().contains("LIVE LET value work"), "{failure}");
	Ok(())
}

/// Composite recipes preserve ordinary numeric semantics and frozen captures.
#[test]
fn live_recipe_gql_collections_and_unary() -> FerrosGrynnResult<()> {
	let store = Datastore::in_memory();
	let mut session = store.session();
	execute_value(
		&mut session,
		"DEFINE CLASS person TYPE NODE; DEFINE FIELD age ON CLASS person TYPE int; CREATE \
		 person:a SET age = 19; LET $fixed = 7; LIVE LET $amount = $input; LIVE LET $vector = \
		 [+$amount, -$amount, $fixed]; LIVE LET $object = {items: $vector, negative: -$amount};",
		&Object::from_iter([("input", Value::from(9_007_199_254_740_993_i64))]),
	)?;
	let oracle = "SELECT VALUE {age: age, items: $object, positive: +age} FROM person;";
	assert_eq!(
		execute_value(&mut session, "RETURN $vector;", &Object::new())?,
		Value::from(vec![
			Value::from(9_007_199_254_740_993_i64),
			Value::from(-9_007_199_254_740_993_i64),
			Value::from(7),
		])
	);
	let before = execute_value(&mut session, oracle, &Object::new())?;
	let id = identity(execute_value(
		&mut session,
		"LIVE SELECT VALUE {age: age, items: $object, positive: +age} FROM person;",
		&Object::new(),
	)?)?;
	execute_value(
		&mut session,
		"ALTER LET $fixed VALUE 99; ALTER LIVE LET $amount VALUE 5;",
		&Object::new(),
	)?;
	let object = Value::Object(Object::from_iter([
		("items", Value::from(vec![Value::from(5), Value::from(-5), Value::from(7)])),
		("negative", Value::from(-5)),
	]));
	assert_eq!(execute_value(&mut session, "RETURN $object;", &Object::new())?, object);
	let after = execute_value(&mut session, oracle, &Object::new())?;
	let LiveNext::Delivery(delivery) = session.next_live(id)? else {
		return Err(FerrosGrynnError::invalid_argument("composite recipe delivery missing"));
	};
	let LivePayload::Batch(batch) = delivery.payload() else {
		return Err(FerrosGrynnError::invalid_argument("composite recipe batch missing"));
	};
	assert_eq!(batch.events().len(), 1);
	assert_eq!(batch.events()[0].before(), before.as_array().and_then(|rows| rows.get(0)));
	assert_eq!(batch.events()[0].after(), after.as_array().and_then(|rows| rows.get(0)));
	assert!(
		execute_value(&mut session, "ALTER LIVE LET $amount VALUE $object;", &Object::new())
			.is_err()
	);
	assert_eq!(execute_value(&mut session, "RETURN $amount;", &Object::new())?, Value::from(5));
	assert!(execute_value(&mut session, "LIVE LET $bad = +'text';", &Object::new()).is_err());
	let mut rejected = store.begin();
	let error = rejected
		.define_live_binding(
			crate::live::LiveExpr::Array(vec![
				crate::live::LiveExpr::Literal(Value::from(1));
				4096
			]),
			Object::new(),
			crate::live::LiveBindingOptions::default().with_budgets(16 * 1024 * 1024, 128)?,
		)
		.err()
		.ok_or_else(|| FerrosGrynnError::invalid_argument("recipe work cap was ignored"))?;
	assert_eq!(error.status().condition(), crate::gql_status::Condition::ProgramLimitExceeded);
	Ok(())
}

/// Set recipes evaluate every input and retain ordinary equality, frozen
/// captures and reactive dependencies before duplicate elimination.
#[test]
fn live_recipe_gql_set() -> FerrosGrynnResult<()> {
	let store = Datastore::in_memory();
	let mut session = store.session();
	execute_value(
		&mut session,
		"DEFINE CLASS person TYPE NODE; DEFINE FIELD age ON CLASS person TYPE int; CREATE \
		 person:a SET age = 19; LET $fixed = 7; LIVE LET $amount = $input; LIVE LET $unique = \
		 set[$amount, $fixed, $amount];",
		&Object::from_iter([("input", Value::from(9_007_199_254_740_993_i64))]),
	)?;
	assert_eq!(
		execute_value(&mut session, "RETURN $unique;", &Object::new())?,
		Value::Set(crate::values::Set::from(vec![
			Value::from(7),
			Value::from(9_007_199_254_740_993_i64),
		]))
	);
	assert_eq!(
		execute_value(&mut session, "LIVE LET $empty = set[]; RETURN $empty;", &Object::new())?,
		Value::Set(crate::values::Set::new())
	);
	let oracle = "SELECT VALUE set[age, $unique, age] FROM person;";
	let before = execute_value(&mut session, oracle, &Object::new())?;
	let id = identity(execute_value(&mut session, &format!("LIVE {oracle}"), &Object::new())?)?;
	execute_value(&mut session, "ALTER LET $fixed VALUE 99;", &Object::new())?;
	execute_value(
		&mut session,
		"BEGIN; ALTER LIVE LET $amount VALUE 7; UPDATE person:a SET age = 23; COMMIT;",
		&Object::new(),
	)?;
	assert_eq!(
		execute_value(&mut session, "RETURN $unique;", &Object::new())?,
		Value::Set(crate::values::Set::from(vec![Value::from(7)]))
	);
	let after = execute_value(&mut session, oracle, &Object::new())?;
	let LiveNext::Delivery(delivery) = session.next_live(id)? else {
		return Err(FerrosGrynnError::invalid_argument("set recipe delivery missing"));
	};
	let LivePayload::Batch(batch) = delivery.payload() else {
		return Err(FerrosGrynnError::invalid_argument("set recipe batch missing"));
	};
	assert_eq!(batch.events().len(), 1);
	assert_eq!(batch.events()[0].before(), before.as_array().and_then(|rows| rows.get(0)));
	assert_eq!(batch.events()[0].after(), after.as_array().and_then(|rows| rows.get(0)));
	for rejected in
		["ALTER LIVE LET $amount VALUE set[$unique, $unique];", "LIVE LET $bad = set[1, 1 / 0, 1];"]
	{
		let count = store.inner.current_root().live_catalog().bindings.len();
		assert!(execute_value(&mut session, rejected, &Object::new()).is_err(), "{rejected}");
		assert_eq!(store.inner.current_root().live_catalog().bindings.len(), count);
	}
	assert_eq!(execute_value(&mut session, "RETURN $amount;", &Object::new())?, 7.into());
	let mut rejected = store.begin();
	let error = rejected
		.define_live_binding(
			crate::live::LiveExpr::Set(vec![crate::live::LiveExpr::Literal(1.into()); 4096]),
			Object::new(),
			crate::live::LiveBindingOptions::default().with_budgets(16 * 1024 * 1024, 128)?,
		)
		.err()
		.ok_or_else(|| FerrosGrynnError::invalid_argument("set work cap was ignored"))?;
	assert_eq!(error.status().condition(), crate::gql_status::Condition::ProgramLimitExceeded);
	Ok(())
}

/// IF recipes retain truthiness, short-circuit evaluation and all capture
/// dependencies while sharing ordinary SELECT results.
#[test]
fn live_recipe_gql_conditional() -> FerrosGrynnResult<()> {
	let store = Datastore::in_memory();
	let mut session = store.session();
	execute_value(
		&mut session,
		"DEFINE CLASS person TYPE NODE; DEFINE FIELD age ON CLASS person TYPE int; CREATE \
		 person:a SET age = 19; DEFINE IMPURE FUNCTION fn::write() -> int { RETURN 1; }; LET \
		 $fixed = 9007199254740993; LIVE LET $pick = true; LIVE LET $left = 18; LIVE LET $right = \
		 21; LIVE LET $chosen = IF $pick THEN $left ELSE $right END; LIVE LET $safe = IF 1 THEN \
		 $fixed ELSE 1 / 0 END;",
		&Object::new(),
	)?;
	assert_eq!(
		execute_value(&mut session, "RETURN $safe;", &Object::new())?,
		Value::from(9_007_199_254_740_993_i64)
	);
	let oracle = "SELECT VALUE IF age >= $chosen THEN $fixed ELSE 0 END FROM person;";
	let before = execute_value(&mut session, oracle, &Object::new())?;
	let id = identity(execute_value(&mut session, &format!("LIVE {oracle}"), &Object::new())?)?;
	execute_value(&mut session, "ALTER LIVE LET $pick VALUE false;", &Object::new())?;
	assert_eq!(execute_value(&mut session, "RETURN $chosen;", &Object::new())?, 21.into());
	let after = execute_value(&mut session, oracle, &Object::new())?;
	let LiveNext::Delivery(delivery) = session.next_live(id)? else {
		return Err(FerrosGrynnError::invalid_argument("conditional delivery missing"));
	};
	let LivePayload::Batch(batch) = delivery.payload() else {
		return Err(FerrosGrynnError::invalid_argument("conditional batch missing"));
	};
	assert_eq!(batch.events()[0].before(), before.as_array().and_then(|rows| rows.get(0)));
	assert_eq!(batch.events()[0].after(), after.as_array().and_then(|rows| rows.get(0)));
	execute_value(
		&mut session,
		"ACK LIVE $qid CURSOR $token;",
		&ack_parameters(id, delivery.token()),
	)?;
	execute_value(
		&mut session,
		"ALTER LET $fixed VALUE 7; ALTER LIVE LET $pick VALUE true;",
		&Object::new(),
	)?;
	let LiveNext::Delivery(delivery) = session.next_live(id)? else {
		return Err(FerrosGrynnError::invalid_argument("conditional frozen capture missing"));
	};
	assert!(
		matches!(delivery.payload(), LivePayload::Batch(batch) if batch.events()[0].after() == Some(&Value::from(9_007_199_254_740_993_i64)))
	);
	assert_eq!(
		execute_value(&mut session, oracle, &Object::new())?,
		Value::from(vec![Value::from(7)])
	);
	for rejected in [
		"ALTER LIVE LET $right VALUE IF false THEN $chosen ELSE 21 END;",
		"LIVE LET $bad = IF true THEN 1 ELSE fn::write() END;",
	] {
		assert!(execute_value(&mut session, rejected, &Object::new()).is_err(), "{rejected}");
	}
	assert_eq!(execute_value(&mut session, "RETURN $right;", &Object::new())?, 21.into());
	execute_value(
		&mut session,
		"LIVE LET $from_record = IF $pick THEN $reference.age ELSE 0 END;",
		&Object::from_iter([("reference", Value::Record(Box::new("person:a".parse()?)))]),
	)?;
	for (statements, expected) in [
		("UPDATE person:a SET age = 20;", 20),
		("ALTER LIVE LET $pick VALUE false; UPDATE person:a SET age = 23;", 0),
		("ALTER LIVE LET $pick VALUE true;", 23),
	] {
		execute_value(&mut session, statements, &Object::new())?;
		assert_eq!(
			execute_value(&mut session, "RETURN $from_record;", &Object::new())?,
			Value::from(expected)
		);
	}
	let mut rejected = store.begin();
	let error = rejected
		.define_live_binding(
			crate::live::LiveExpr::Conditional {
				condition: Box::new(crate::live::LiveExpr::Literal(true.into())),
				then_value: Box::new(crate::live::LiveExpr::Literal(1.into())),
				else_value: Box::new(crate::live::LiveExpr::Array(
					vec![crate::live::LiveExpr::Literal(1.into()); 4096],
				)),
			},
			Object::new(),
			crate::live::LiveBindingOptions::default().with_budgets(16 * 1024 * 1024, 128)?,
		)
		.err()
		.ok_or_else(|| FerrosGrynnError::invalid_argument("inactive branch budget ignored"))?;
	assert_eq!(error.status().condition(), crate::gql_status::Condition::ProgramLimitExceeded);
	Ok(())
}

/// CASE recipes share first-match, exact simple comparison and inactive-branch
/// validation with ordinary SELECT while preserving every capture.
#[test]
fn live_recipe_gql_case() -> FerrosGrynnResult<()> {
	let store = Datastore::in_memory();
	let mut session = store.session();
	execute_value(
		&mut session,
		"DEFINE CLASS person TYPE NODE; DEFINE FIELD age ON CLASS person TYPE int; CREATE \
		 person:a SET age = 19; DEFINE IMPURE FUNCTION fn::write() -> int { RETURN 1; }; LET \
		 $fixed = 9007199254740993; LIVE LET $selector = 2; LIVE LET $chosen = CASE $selector \
		 WHEN 1 THEN 18 WHEN 2 THEN 21 ELSE 99 END; LIVE LET $first = CASE WHEN true THEN $fixed \
		 WHEN 1 / 0 THEN 99 END; LIVE LET $missing = CASE 3 WHEN 2 THEN 10 END; LIVE LET $exact = \
		 CASE $fixed WHEN 9007199254740992 THEN 0 WHEN 9007199254740993 THEN 1 END;",
		&Object::new(),
	)?;
	assert_eq!(
		execute_value(&mut session, "RETURN $first;", &Object::new())?,
		Value::from(9_007_199_254_740_993_i64)
	);
	assert_eq!(execute_value(&mut session, "RETURN $missing;", &Object::new())?, Value::None);
	assert_eq!(execute_value(&mut session, "RETURN $exact;", &Object::new())?, 1.into());
	let oracle = "SELECT VALUE CASE WHEN age >= $chosen THEN $fixed ELSE 0 END FROM person;";
	let before = execute_value(&mut session, oracle, &Object::new())?;
	let id = identity(execute_value(&mut session, &format!("LIVE {oracle}"), &Object::new())?)?;
	execute_value(&mut session, "ALTER LIVE LET $selector VALUE 1;", &Object::new())?;
	let after = execute_value(&mut session, oracle, &Object::new())?;
	let LiveNext::Delivery(delivery) = session.next_live(id)? else {
		return Err(FerrosGrynnError::invalid_argument("CASE recipe delivery missing"));
	};
	let LivePayload::Batch(batch) = delivery.payload() else {
		return Err(FerrosGrynnError::invalid_argument("CASE recipe batch missing"));
	};
	assert_eq!(batch.events().len(), 1);
	assert_eq!(batch.events()[0].before(), before.as_array().and_then(|rows| rows.get(0)));
	assert_eq!(batch.events()[0].after(), after.as_array().and_then(|rows| rows.get(0)));
	execute_value(
		&mut session,
		"ACK LIVE $qid CURSOR $token;",
		&ack_parameters(id, delivery.token()),
	)?;
	execute_value(
		&mut session,
		"ALTER LET $fixed VALUE 7; ALTER LIVE LET $selector VALUE 2;",
		&Object::new(),
	)?;
	assert_eq!(
		execute_value(&mut session, "RETURN $first;", &Object::new())?,
		Value::from(9_007_199_254_740_993_i64)
	);
	for rejected in [
		"ALTER LIVE LET $selector VALUE CASE WHEN false THEN $chosen ELSE 2 END;",
		"LIVE LET $bad = CASE WHEN true THEN 1 ELSE fn::write() END;",
	] {
		let count = store.inner.current_root().live_catalog().bindings.len();
		assert!(execute_value(&mut session, rejected, &Object::new()).is_err(), "{rejected}");
		assert_eq!(store.inner.current_root().live_catalog().bindings.len(), count);
	}
	assert_eq!(execute_value(&mut session, "RETURN $selector;", &Object::new())?, 2.into());
	for (count, expected) in [
		(0, crate::gql_status::Condition::DataException),
		(4096, crate::gql_status::Condition::ProgramLimitExceeded),
	] {
		let mut rejected = store.begin();
		let error = rejected
			.define_live_binding(
				crate::live::LiveExpr::Case {
					operand: None,
					branches: vec![
						(
							crate::live::LiveExpr::Literal(false.into()),
							crate::live::LiveExpr::Literal(1.into())
						);
						count
					],
					else_value: None,
				},
				Object::new(),
				crate::live::LiveBindingOptions::default().with_budgets(16 * 1024 * 1024, 128)?,
			)
			.err()
			.ok_or_else(|| {
				FerrosGrynnError::invalid_argument("CASE branch admission was ignored")
			})?;
		assert_eq!(error.status().condition(), expected);
	}
	Ok(())
}

/// Typed and named calls retain frozen/reactive captures, signatures and PURE
/// versions through the shared analyzer.
#[test]
fn live_recipe_gql_call_modifiers() -> FerrosGrynnResult<()> {
	let store = Datastore::in_memory();
	let mut session = store.session();
	execute_value(
		&mut session,
		"DEFINE PURE FUNCTION fn::pair(a: int, b: int) -> int { RETURN $a * 10 + $b; }; DEFINE \
		 IMPURE FUNCTION fn::write(a: int) -> int { RETURN $a; }; LET $fixed = 9007199254740993; \
		 LIVE LET $input = 2; LIVE LET $typed = type::is<int>($input); LIVE LET $named = \
		 fn::pair(b = <int>$input, a = 1); LIVE LET $converted = \
		 type::to<int>('9007199254740993'); LIVE LET $frozen = fn::pair(b = 1, a = $fixed);",
		&Object::new(),
	)?;
	for (index, source) in [
		"type::is<int>($input)",
		"fn::pair(b = <int>$input, a = 1)",
		"type::to<int>('9007199254740993')",
	]
	.into_iter()
	.enumerate()
	{
		let expected = execute_value(&mut session, &format!("RETURN {source};"), &Object::new())?;
		assert_eq!(
			execute_value(
				&mut session,
				&format!("LIVE LET $check{index} = {source}; RETURN $check{index};"),
				&Object::new()
			)?,
			expected
		);
	}
	assert_eq!(execute_value(&mut session, "RETURN $named;", &Object::new())?, 12.into());
	assert_eq!(
		execute_value(&mut session, "RETURN $converted;", &Object::new())?,
		9_007_199_254_740_993_i64.into()
	);
	execute_value(
		&mut session,
		"ALTER LET $fixed VALUE 7; ALTER LIVE LET $input VALUE 3;",
		&Object::new(),
	)?;
	assert_eq!(execute_value(&mut session, "RETURN $named;", &Object::new())?, 13.into());
	assert_eq!(
		execute_value(&mut session, "RETURN $frozen;", &Object::new())?,
		90_071_992_547_409_931_i64.into()
	);
	for rejected in [
		"LIVE LET $bad = fn::pair(c = 1, b = 2);",
		"LIVE LET $bad = fn::pair(a = 1, A = 2);",
		"LIVE LET $bad = fn::pair(a = 1, 2);",
		"LIVE LET $bad = type::is<int>('a', 'b');",
		"LIVE LET $bad = fn::write(a = 1);",
		"LIVE LET $bad = count(DISTINCT);",
		"ALTER LIVE LET $input VALUE fn::pair(b = <int>$named, a = 1);",
	] {
		let root = store.inner.current_root().commit_ts;
		assert!(execute_value(&mut session, rejected, &Object::new()).is_err(), "{rejected}");
		assert_eq!(store.inner.current_root().commit_ts, root);
	}
	execute_value(&mut session, "BEGIN; ALTER LIVE LET $input VALUE 9; CANCEL;", &Object::new())?;
	assert_eq!(execute_value(&mut session, "RETURN $named;", &Object::new())?, 13.into());
	let mut tx = store.begin();
	let oversized = crate::live::LiveExpr::CallWithOptions {
		name: "type::is".into(),
		type_argument: Some("int".into()),
		distinct: false,
		arguments: vec![(None, crate::live::LiveExpr::Literal(1.into())); 4096],
	};
	let error = tx
		.define_live_binding(
			oversized,
			Object::new(),
			crate::live::LiveBindingOptions::default().with_budgets(16 * 1024 * 1024, 128)?,
		)
		.err()
		.ok_or_else(|| FerrosGrynnError::invalid_argument("call work cap ignored"))?;
	assert_eq!(error.status().condition(), crate::gql_status::Condition::ProgramLimitExceeded);
	Ok(())
}

/// Signature metadata is bounded before ordinary function ranking, source
/// rendering and compiler-owned parameter types can be allocated.
#[test]
fn live_recipe_call_parameter_type_admission() -> FerrosGrynnResult<()> {
	let store = Datastore::in_memory();
	let mut ty = Type::Int;
	for _ in 0..65 {
		ty = Type::Option(Box::new(ty));
	}
	let mut setup = store.begin();
	setup.define_function(crate::datastore::SchemaFunction::new(
		"fn",
		"deep",
		vec![crate::datastore::SchemaFunctionParameter::new("x", ty)?],
		Type::Int,
		crate::datastore::FunctionEffect::Pure,
		false,
		None,
		"{ RETURN 1; }",
	)?)?;
	setup.define_function(crate::datastore::SchemaFunction::new(
		"fn",
		"bridge",
		Vec::new(),
		Type::Int,
		crate::datastore::FunctionEffect::Pure,
		false,
		None,
		"{ RETURN fn::deep(x = NONE); }",
	)?)?;
	setup.commit()?;
	let mut tx = store.begin();
	let error = tx
		.define_live_binding(
			crate::live::LiveExpr::CallWithOptions {
				name: "fn::deep".into(),
				type_argument: None,
				distinct: false,
				arguments: vec![(Some("x".into()), crate::live::LiveExpr::Literal(Value::None))],
			},
			Object::new(),
			crate::live::LiveBindingOptions::default(),
		)
		.err()
		.ok_or_else(|| FerrosGrynnError::invalid_argument("deep signature was not bounded"))?;
	assert_eq!(error.status().condition(), crate::gql_status::Condition::ProgramLimitExceeded);
	let error = tx
		.define_live_binding(
			crate::live::LiveExpr::Call {
				name: "fn::bridge".into(),
				arguments: Vec::new(),
			},
			Object::new(),
			crate::live::LiveBindingOptions::default(),
		)
		.err()
		.ok_or_else(|| {
			FerrosGrynnError::invalid_argument("transitive signature was not bounded")
		})?;
	assert_eq!(error.status().condition(), crate::gql_status::Condition::ProgramLimitExceeded);
	let mut session = store.session();
	let error = execute_value(&mut session, "LIVE LET $deep = fn::deep(x = NONE);", &Object::new())
		.err()
		.ok_or_else(|| FerrosGrynnError::invalid_argument("text deep signature was not bounded"))?;
	assert_eq!(error.status().condition(), crate::gql_status::Condition::ProgramLimitExceeded);
	Ok(())
}

/// Small valid signatures use raw metadata admission within the existing
/// compiler budget rather than receiving the source multiplier twice.
#[test]
fn live_recipe_call_signature_budget() -> FerrosGrynnResult<()> {
	let store = Datastore::in_memory();
	let mut setup = store.begin();
	setup.define_function(crate::datastore::SchemaFunction::new(
		"fn",
		"small",
		(0..32)
			.map(|index| {
				crate::datastore::SchemaFunctionParameter::new(format!("a{index}"), Type::Int)
			})
			.collect::<FerrosGrynnResult<_>>()?,
		Type::Int,
		crate::datastore::FunctionEffect::Pure,
		false,
		None,
		"{ RETURN $a0; }",
	)?)?;
	setup.commit()?;
	for named in [false, true] {
		let values = (0..32)
			.map(|index| crate::live::LiveExpr::Literal(Value::from(index)))
			.collect::<Vec<_>>();
		let expression = if named {
			crate::live::LiveExpr::CallWithOptions {
				name: "fn::small".into(),
				type_argument: None,
				distinct: false,
				arguments: values
					.into_iter()
					.enumerate()
					.map(|(index, value)| (Some(format!("a{index}").into()), value))
					.collect(),
			}
		} else {
			crate::live::LiveExpr::Call {
				name: "fn::small".into(),
				arguments: values,
			}
		};
		let mut tx = store.begin();
		let id = tx.define_live_binding(
			expression,
			Object::new(),
			crate::live::LiveBindingOptions::default().with_budgets(16 * 1024 * 1024, 1_000_000)?,
		)?;
		assert_eq!(tx.live_binding_value(id)?, Value::from(0));
	}
	Ok(())
}

/// Programmatic recipes enforce the same named-tail rule as GQL syntax.
#[test]
fn live_recipe_call_named_order() -> FerrosGrynnResult<()> {
	let store = Datastore::in_memory();
	let mut session = store.session();
	execute_value(
		&mut session,
		"DEFINE PURE FUNCTION fn::pair(a: int, b: int) -> int { RETURN $a * 10 + $b; };",
		&Object::new(),
	)?;
	let mut tx = store.begin();
	assert!(
		tx.define_live_binding(
			crate::live::LiveExpr::CallWithOptions {
				name: "fn::pair".into(),
				type_argument: None,
				distinct: false,
				arguments: vec![
					(Some("b".into()), crate::live::LiveExpr::Literal(2.into())),
					(None, crate::live::LiveExpr::Literal(1.into()))
				],
			},
			Object::new(),
			crate::live::LiveBindingOptions::default()
		)
		.is_err()
	);
	Ok(())
}

/// DISTINCT aggregate history reconstructs complete captured domains, including
/// duplicates and sentinels, without reading the current primary values.
#[test]
fn live_recipe_gql_distinct_history() -> FerrosGrynnResult<()> {
	let store = Datastore::in_memory();
	let mut setup = store.begin();
	let class =
		setup.define_class(ClassSchema::new("item", AtomKind::Node, [ClassField::new(
			"age",
			Type::Any,
		)])?)?;
	setup.configure_changefeed(
		ChangefeedSource::Class(class),
		ChangefeedConfig::new("P1D".parse()?)?,
	)?;
	setup.commit()?;
	let mut session = store.session();
	execute_value(
		&mut session,
		"CREATE item:a SET age = 9007199254740993; CREATE item:b SET age = 9007199254740993; \
		 CREATE item:c SET age = NONE; CREATE item:d SET age = NULL;",
		&Object::new(),
	)?;
	let oracle = "SELECT {count: count(DISTINCT age), sum: sum(DISTINCT age)} FROM item;";
	let before = execute_value(&mut session, oracle, &Object::new())?;
	let floor = store.begin().commit()?.versionstamp();
	let id = identity(execute_value(&mut session, &format!("LIVE {oracle}"), &Object::new())?)?;
	execute_value(&mut session, "UPDATE item:b SET age = 9007199254740992;", &Object::new())?;
	let after = execute_value(&mut session, oracle, &Object::new())?;
	let LiveNext::Delivery(current) = session.next_live(id)? else {
		return Err(FerrosGrynnError::invalid_argument("distinct LIVE delivery missing"));
	};
	let LivePayload::Batch(batch) = current.payload() else {
		return Err(FerrosGrynnError::invalid_argument("distinct LIVE fault"));
	};
	assert_eq!(batch.events().len(), 1);
	assert!(
		matches!(batch.events()[0].identity(), LiveIdentity::Aggregate(keys) if keys.is_empty())
	);
	assert_eq!(batch.events()[0].before(), before.as_array().and_then(|rows| rows.get(0)));
	assert_eq!(batch.events()[0].after(), after.as_array().and_then(|rows| rows.get(0)));
	execute_value(&mut session, "UPDATE item:b SET age = 99;", &Object::new())?;
	let history = identity(execute_value(
		&mut session,
		&format!(
			"LIVE {} SINCE $floor;",
			oracle.trim_end_matches(';').replace("count(", "COUNT(").replace("sum(", "SUM(")
		),
		&Object::from_iter([("floor", Value::from(floor.get()))]),
	)?)?;
	let LiveNext::Delivery(replayed) = session.next_live(history)? else {
		return Err(FerrosGrynnError::invalid_argument("distinct historical delivery missing"));
	};
	let LivePayload::Batch(replayed) = replayed.payload() else {
		return Err(FerrosGrynnError::invalid_argument("distinct history coverage missing"));
	};
	assert_eq!(replayed.versionstamp(), batch.versionstamp());
	assert_eq!(replayed.events(), batch.events());
	Ok(())
}

/// Scalar operator recipes preserve ordinary arithmetic, quantifiers and
/// truthiness.
#[test]
fn live_recipe_gql_scalar_operators() -> FerrosGrynnResult<()> {
	let store = Datastore::in_memory();
	let mut session = store.session();
	for (index, source) in [
		"2 ** 3 ** 2",
		"9007199254740993 % 2",
		"9007199254740993 IS 9007199254740992",
		"NONE IS NULL",
		"1 NOT IS 2",
		"3 NOT IN [1, 2]",
		"[1, 2] ALLIN set[1, 2, 3]",
		"[4, 2] ANYIN set[1, 2, 3]",
		"[4, 5] NONEIN set[1, 2, 3]",
		"[1, 2] ?= 2",
		"[2, 2] *= 2",
		"[] ALLIN []",
		"[] ANYIN []",
		"set[] NONEIN []",
		"[] ?= 1",
		"set[] *= 1",
		"'ferros-grynn' CONTAINS 'grynn'",
		"{a: 1} CONTAINS 'a'",
		"[1, 2, 3] CONTAINSNOT 4",
		"[1, 2, 3] CONTAINSANY [0, 2]",
		"[1, 2, 3] CONTAINSALL [1, 3]",
		"[] CONTAINSALL []",
		"set[] CONTAINSANY []",
		"0 ?: 7",
		"0 ?? 7",
		"NONE ?: 7",
		"NULL ?: 7",
		"1 ?: (1 / 0)",
		"1 ?? (1 / 0)",
		"false AND (1 / 0)",
		"true OR (1 / 0)",
		"!!NONE",
		"!!NULL",
		"!!0",
		"!!1",
		"!![]",
		"!!set[]",
		"!!{}",
		"!!''",
	]
	.into_iter()
	.enumerate()
	{
		let expected = execute_value(&mut session, &format!("RETURN {source};"), &Object::new())?;
		let result = execute_value(
			&mut session,
			&format!("LIVE LET $operator{index} = {source}; RETURN $operator{index};"),
			&Object::new(),
		)?;
		assert_eq!(result, expected, "{source}");
	}
	for source in ["2 ** 63", "1 % 0", "<int>'-9223372036854775808' % -1", "[1, 2] ALLIN 3"] {
		let ordinary = execute_value(&mut session, &format!("RETURN {source};"), &Object::new())
			.err()
			.ok_or_else(|| {
				FerrosGrynnError::invalid_argument(format!(
					"invalid ordinary operator succeeded: {source}"
				))
			})?;
		let count = store.inner.current_root().live_catalog().bindings.len();
		let error =
			execute_value(&mut session, &format!("LIVE LET $bad = {source};"), &Object::new())
				.err()
				.ok_or_else(|| {
					FerrosGrynnError::invalid_argument("invalid LIVE operator succeeded")
				})?;
		assert_eq!(error.status().condition(), ordinary.status().condition(), "{source}");
		assert_eq!(store.inner.current_root().live_catalog().bindings.len(), count);
	}
	let count = store.inner.current_root().live_catalog().bindings.len();
	let error = execute_value(
		&mut session,
		&format!("LIVE LET $deep = {}true;", "!!".repeat(33)),
		&Object::new(),
	)
	.err()
	.ok_or_else(|| FerrosGrynnError::invalid_argument("synthetic truthy depth ignored"))?;
	assert_eq!(error.status().condition(), crate::gql_status::Condition::ProgramLimitExceeded);
	assert_eq!(store.inner.current_root().live_catalog().bindings.len(), count);
	execute_value(
		&mut session,
		"DEFINE CLASS item TYPE NODE; DEFINE FIELD age ON CLASS item TYPE int; CREATE item:a SET \
		 age = 19; LET $fixed = 7; LIVE LET $policy = [19]; LIVE LET $allowed = $policy CONTAINS \
		 19; LIVE LET $frozen = 0 ?: $fixed;",
		&Object::new(),
	)?;
	let oracle = "SELECT VALUE {power: age ** 2, remainder: age % 2, truth: !!age, allowed: \
	              $policy CONTAINS age, frozen: $frozen} FROM item WHERE [age] ALLIN $policy;";
	let before = execute_value(&mut session, oracle, &Object::new())?;
	let id = identity(execute_value(&mut session, &format!("LIVE {oracle}"), &Object::new())?)?;
	execute_value(
		&mut session,
		"BEGIN; ALTER LIVE LET $policy VALUE [21]; UPDATE item:a SET age = 21; COMMIT; ALTER LET \
		 $fixed VALUE 99;",
		&Object::new(),
	)?;
	let after = execute_value(&mut session, oracle, &Object::new())?;
	let LiveNext::Delivery(delivery) = session.next_live(id)? else {
		return Err(FerrosGrynnError::invalid_argument("operator delivery missing"));
	};
	let LivePayload::Batch(batch) = delivery.payload() else {
		return Err(FerrosGrynnError::invalid_argument("operator batch missing"));
	};
	assert_eq!(batch.events().len(), 1);
	assert_eq!(batch.events()[0].before(), before.as_array().and_then(|rows| rows.get(0)));
	assert_eq!(batch.events()[0].after(), after.as_array().and_then(|rows| rows.get(0)));
	execute_value(
		&mut session,
		"ACK LIVE $qid CURSOR $token;",
		&ack_parameters(id, delivery.token()),
	)?;
	assert!(
		execute_value(
			&mut session,
			"ALTER LIVE LET $policy VALUE $policy ?: $allowed;",
			&Object::new()
		)
		.is_err()
	);
	execute_value(&mut session, "BEGIN; ALTER LIVE LET $policy VALUE []; CANCEL;", &Object::new())?;
	assert!(matches!(session.next_live(id)?, LiveNext::Idle));
	Ok(())
}

/// Spatial recipes retain exact boundary and eager member validation semantics.
#[test]
fn live_recipe_gql_spatial_operators() -> FerrosGrynnResult<()> {
	let store = Datastore::in_memory();
	let mut session = store.session();
	let point = "<geometry>{type: 'Point', coordinates: [1, 1]}";
	let boundary = "<geometry>{type: 'Point', coordinates: [0, 1]}";
	let polygon = "<geometry>{type: 'Polygon', coordinates: [[[0,0], [2,0], [2,2], [0,2], [0,0]]]}";
	for (index, source) in [
		format!("{point} INSIDE {polygon}"),
		format!("{boundary} INSIDE {polygon}"),
		format!("{boundary} NOTINSIDE {polygon}"),
		format!("{boundary} OUTSIDE {polygon}"),
		format!("{boundary} INTERSECTS {polygon}"),
		format!("[{point}, {boundary}] ALLINSIDE {polygon}"),
		format!("[{point}, {boundary}] ANYINSIDE {polygon}"),
		format!("[{boundary}] NONEINSIDE {polygon}"),
		format!("[] ALLINSIDE {polygon}"),
		format!("[] ANYINSIDE {polygon}"),
		format!("set[] NONEINSIDE {polygon}"),
		format!("NONE OUTSIDE {polygon}"),
		format!("NULL NOTINSIDE {polygon}"),
	]
	.into_iter()
	.enumerate()
	{
		let expected = execute_value(&mut session, &format!("RETURN {source};"), &Object::new())?;
		let result = execute_value(
			&mut session,
			&format!("LIVE LET $spatial{index} = {source}; RETURN $spatial{index};"),
			&Object::new(),
		)?;
		assert_eq!(result, expected, "{source}");
	}
	for source in
		[format!("[{point}, 1] ANYINSIDE {polygon}"), format!("[NONE, 1] NONEINSIDE {polygon}")]
	{
		let ordinary = execute_value(&mut session, &format!("RETURN {source};"), &Object::new())
			.err()
			.ok_or_else(|| {
				FerrosGrynnError::invalid_argument("invalid ordinary spatial member succeeded")
			})?;
		let count = store.inner.current_root().live_catalog().bindings.len();
		let error =
			execute_value(&mut session, &format!("LIVE LET $bad = {source};"), &Object::new())
				.err()
				.ok_or_else(|| {
					FerrosGrynnError::invalid_argument("invalid LIVE spatial member succeeded")
				})?;
		assert_eq!(error.status().condition(), ordinary.status().condition());
		assert_eq!(store.inner.current_root().live_catalog().bindings.len(), count);
	}
	Ok(())
}

/// Range recipes preserve exact bounds, optional endpoints and captures.
#[test]
fn live_recipe_gql_range() -> FerrosGrynnResult<()> {
	let store = Datastore::in_memory();
	let mut session = store.session();
	execute_value(
		&mut session,
		"DEFINE CLASS person TYPE NODE; DEFINE FIELD age ON CLASS person TYPE int; CREATE \
		 person:a SET age = 19; CREATE person:b SET age = 22; LET $fixed = 23; LIVE LET $lower = \
		 18; LIVE LET $range = $lower..$fixed; LIVE LET $frozen = $fixed..;",
		&Object::new(),
	)?;
	for (index, source) in [
		"1..3",
		"1..=3",
		"..3",
		"..=3",
		"1..",
		"1..=",
		"..",
		"..=",
		"9007199254740993..=9007199254740994",
		"1..=1.0",
		"'a'..='z'",
	]
	.into_iter()
	.enumerate()
	{
		let expected = execute_value(&mut session, &format!("RETURN {source};"), &Object::new())?;
		execute_value(
			&mut session,
			&format!("LIVE LET $sample{index} = {source};"),
			&Object::new(),
		)?;
		assert_eq!(
			execute_value(&mut session, &format!("RETURN $sample{index};"), &Object::new())?,
			expected
		);
	}
	let oracle = "SELECT VALUE {bounds: $lower..=$fixed, open: age.., left: ..age} FROM person \
	              WHERE age IN $range;";
	let before = execute_value(&mut session, oracle, &Object::new())?;
	let id = identity(execute_value(&mut session, &format!("LIVE {oracle}"), &Object::new())?)?;
	execute_value(
		&mut session,
		"BEGIN; ALTER LIVE LET $lower VALUE 20; UPDATE person:a SET age = 21; COMMIT;",
		&Object::new(),
	)?;
	let after = execute_value(&mut session, oracle, &Object::new())?;
	let LiveNext::Delivery(delivery) = session.next_live(id)? else {
		return Err(FerrosGrynnError::invalid_argument("Range delivery missing"));
	};
	let LivePayload::Batch(batch) = delivery.payload() else {
		return Err(FerrosGrynnError::invalid_argument("Range batch missing"));
	};
	assert_eq!(batch.events().len(), 2);
	for (index, event) in batch.events().iter().enumerate() {
		assert_eq!(event.before(), before.as_array().and_then(|rows| rows.get(index)));
		assert_eq!(event.after(), after.as_array().and_then(|rows| rows.get(index)));
	}
	execute_value(
		&mut session,
		"ACK LIVE $qid CURSOR $token;",
		&ack_parameters(id, delivery.token()),
	)?;
	let frozen = execute_value(&mut session, "RETURN $frozen;", &Object::new())?;
	execute_value(&mut session, "ALTER LET $fixed VALUE 99;", &Object::new())?;
	assert_eq!(execute_value(&mut session, "RETURN $frozen;", &Object::new())?, frozen);
	assert!(matches!(session.next_live(id)?, LiveNext::Idle));
	assert!(
		execute_value(&mut session, "ALTER LIVE LET $lower VALUE $range..;", &Object::new())
			.is_err()
	);
	execute_value(&mut session, "BEGIN; ALTER LIVE LET $lower VALUE 18; CANCEL;", &Object::new())?;
	assert_eq!(execute_value(&mut session, "RETURN $lower;", &Object::new())?, 20.into());
	for source in ["3..1", "1..'x'"] {
		let ordinary = execute_value(&mut session, &format!("RETURN {source};"), &Object::new())
			.err()
			.ok_or_else(|| {
				FerrosGrynnError::invalid_argument("invalid ordinary Range succeeded")
			})?;
		let count = store.inner.current_root().live_catalog().bindings.len();
		let error =
			execute_value(&mut session, &format!("LIVE LET $bad = {source};"), &Object::new())
				.err()
				.ok_or_else(|| {
					FerrosGrynnError::invalid_argument("invalid LIVE Range succeeded")
				})?;
		assert_eq!(error.status().condition(), ordinary.status().condition());
		assert_eq!(store.inner.current_root().live_catalog().bindings.len(), count);
	}
	Ok(())
}

/// CAST preserves ordinary exact conversion, captures and failures while
/// admitting symbolic target names before recursive type analysis.
#[test]
fn live_recipe_gql_cast() -> FerrosGrynnResult<()> {
	let store = Datastore::in_memory();
	let mut session = store.session();
	execute_value(
		&mut session,
		"DEFINE CLASS person TYPE NODE; DEFINE FIELD age ON CLASS person TYPE int; CREATE \
		 person:a SET age = 19; LET $fixed = '9007199254740993'; LIVE LET $text = '21'; LIVE LET \
		 $whole = <int>$text; LIVE LET $frozen = <int>$fixed; LIVE LET $decimal = <decimal>'1.25';",
		&Object::new(),
	)?;
	assert_eq!(
		execute_value(&mut session, "RETURN $frozen;", &Object::new())?,
		9_007_199_254_740_993_i64.into()
	);
	assert_eq!(
		execute_value(&mut session, "RETURN $decimal;", &Object::new())?,
		execute_value(&mut session, "RETURN <decimal>'1.25';", &Object::new())?
	);
	let oracle =
		"SELECT VALUE {age: <string>age, fixed: <int>$fixed} FROM person WHERE age >= $whole;";
	let before = execute_value(&mut session, oracle, &Object::new())?;
	let id = identity(execute_value(&mut session, &format!("LIVE {oracle}"), &Object::new())?)?;
	execute_value(
		&mut session,
		"BEGIN; ALTER LIVE LET $text VALUE '18'; UPDATE person:a SET age = 20; COMMIT;",
		&Object::new(),
	)?;
	let after = execute_value(&mut session, oracle, &Object::new())?;
	let LiveNext::Delivery(delivery) = session.next_live(id)? else {
		return Err(FerrosGrynnError::invalid_argument("CAST delivery missing"));
	};
	let LivePayload::Batch(batch) = delivery.payload() else {
		return Err(FerrosGrynnError::invalid_argument("CAST batch missing"));
	};
	assert_eq!(batch.events().len(), 1);
	assert_eq!(batch.events()[0].before(), before.as_array().and_then(|rows| rows.get(0)));
	assert_eq!(batch.events()[0].after(), after.as_array().and_then(|rows| rows.get(0)));
	execute_value(
		&mut session,
		"ACK LIVE $qid CURSOR $token;",
		&ack_parameters(id, delivery.token()),
	)?;
	execute_value(&mut session, "ALTER LET $fixed VALUE '7';", &Object::new())?;
	assert_eq!(
		execute_value(&mut session, "RETURN $frozen;", &Object::new())?,
		9_007_199_254_740_993_i64.into()
	);
	for target in ["unknown", "option<int>", "vector<f64>", "array<int>"] {
		let ordinary = execute_value(&mut session, &format!("RETURN <{target}>1;"), &Object::new())
			.err()
			.ok_or_else(|| {
				FerrosGrynnError::invalid_argument("ordinary conversion unexpectedly succeeded")
			})?;
		let count = store.inner.current_root().live_catalog().bindings.len();
		let error =
			execute_value(&mut session, &format!("LIVE LET $bad = <{target}>1;"), &Object::new())
				.err()
				.ok_or_else(|| {
					FerrosGrynnError::invalid_argument("LIVE conversion unexpectedly succeeded")
				})?;
		assert_eq!(error.status().condition(), ordinary.status().condition());
		assert_eq!(store.inner.current_root().live_catalog().bindings.len(), count);
	}
	assert!(
		execute_value(&mut session, "ALTER LIVE LET $text VALUE <string>$whole;", &Object::new())
			.is_err()
	);
	assert!(execute_value(&mut session, "LIVE LET $bad = <int>'x';", &Object::new()).is_err());
	assert_eq!(execute_value(&mut session, "RETURN $whole;", &Object::new())?, 18.into());
	for (target, scratch, work, expected) in [
		(String::new(), 1024 * 1024, 1_000_000, crate::gql_status::Condition::DataException),
		(
			"option<int".to_owned(),
			1024 * 1024,
			1_000_000,
			crate::gql_status::Condition::DataException,
		),
		(
			format!("{}int{}", "option<".repeat(65), ">".repeat(65)),
			1024 * 1024,
			1_000_000,
			crate::gql_status::Condition::ProgramLimitExceeded,
		),
		(
			"x".repeat(128 * 1024),
			4096,
			1_000_000,
			crate::gql_status::Condition::ProgramLimitExceeded,
		),
		("int".to_owned(), 1024 * 1024, 3, crate::gql_status::Condition::ProgramLimitExceeded),
	] {
		let mut rejected = store.begin();
		let error = rejected
			.define_live_binding(
				crate::live::LiveExpr::Cast {
					target: target.into(),
					value: Box::new(crate::live::LiveExpr::Literal(1.into())),
				},
				Object::new(),
				crate::live::LiveBindingOptions::default().with_budgets(scratch, work)?,
			)
			.err()
			.ok_or_else(|| FerrosGrynnError::invalid_argument("CAST target limit ignored"))?;
		assert_eq!(error.status().condition(), expected, "scratch={scratch}, work={work}: {error}");
	}
	Ok(())
}

/// Failed conversion diagnostics spend the same renderer pool as successful
/// string casts, including type::to and reactive ordinary evaluation.
#[test]
fn live_recipe_cast_failure_render_bounded() -> FerrosGrynnResult<()> {
	use crate::gql::{
		ExecutionControl,
		QueryExecutionOptions,
		ir::{Builtin, TypedExpr, TypedExprKind},
	};
	let payload = Value::Object(Object::from_iter([("escaped", Value::from("\0".repeat(20_000)))]));
	let span = crate::gql_status::TextSpan::new(0, 0);
	let options = QueryExecutionOptions {
		barrier_memory_bytes: std::num::NonZeroUsize::new(64 * 1024)
			.ok_or_else(|| FerrosGrynnError::invalid_argument("CAST fixture scratch is zero"))?,
		..QueryExecutionOptions::default()
	};
	for target in [
		Type::String,
		Type::Int,
		Type::Float,
		Type::Decimal,
		Type::Number,
		Type::Option(Box::new(Type::Int)),
		Type::Vector(crate::values::VectorKind::F64),
	] {
		for builtin in [false, true] {
			let control = ExecutionControl::new(&options, None);
			control.admit_reactive_work(1)?;
			let operand = TypedExpr {
				kind: TypedExprKind::Constant(payload.clone()),
				ty: Type::Object,
				span,
			};
			let kind = if builtin {
				TypedExprKind::Builtin {
					function: Builtin::TypeTo(target.clone()),
					arguments: vec![operand],
				}
			} else {
				TypedExprKind::Cast {
					target: target.clone(),
					expr: Box::new(operand),
				}
			};
			let expression = TypedExpr {
				kind,
				ty: target.clone(),
				span,
			};
			let error =
				crate::gql::execution::eval::eval(&expression, None, &[], "", None, Some(&control))
					.err()
					.ok_or_else(|| {
						FerrosGrynnError::invalid_argument("unadmitted CAST renderer succeeded")
					})?;
			assert_eq!(
				error.status().condition(),
				crate::gql_status::Condition::ProgramLimitExceeded,
				"target={target}, builtin={builtin}: {error}"
			);
		}
	}
	for target in [Type::Any, Type::Bool, Type::Object] {
		let control = ExecutionControl::new(&options, None);
		control.admit_reactive_work(1)?;
		let expression = TypedExpr {
			kind: TypedExprKind::Cast {
				target: target.clone(),
				expr: Box::new(TypedExpr {
					kind: TypedExprKind::Constant(payload.clone()),
					ty: Type::Object,
					span,
				}),
			},
			ty: target,
			span,
		};
		assert!(
			crate::gql::execution::eval::eval(&expression, None, &[], "", None, Some(&control))
				.is_ok()
		);
	}
	let store = Datastore::in_memory();
	let root = store.inner.current_root();
	let control = ExecutionControl::new(&options, None);
	let observe = |_key| {};
	let work = |_work| Ok(());
	let read = crate::gql::execution::root_read::RootRead::new(
		&root,
		&control,
		&observe,
		&work,
		std::path::PathBuf::new(),
	);
	assert!(!control.has_reactive_copies());
	let error = crate::gql::execution::eval::cast(
		payload.clone(),
		&Type::Int,
		crate::gql::execution::QueryRead::bounded_fields(&read),
		Some(&control),
	)
	.err()
	.ok_or_else(|| FerrosGrynnError::invalid_argument("RootRead CAST discarded bounded fields"))?;
	assert_eq!(error.status().condition(), crate::gql_status::Condition::ProgramLimitExceeded);
	let repeated = "k".repeat(20_000);
	let objects = vec![
		Value::Object(Object::from_iter([(repeated.as_str(), Value::from(1))])),
		Value::Object(Object::from_iter([(repeated.as_str(), Value::from(2))])),
	];
	for value in [Value::from(objects.clone()), Value::Set(crate::values::Set::from(objects))] {
		let control = ExecutionControl::new(&options, None);
		control.admit_reactive_value(&value)?;
		let error = crate::gql::execution::eval::cast(value, &Type::Object, None, Some(&control))
			.err()
			.ok_or_else(|| {
				FerrosGrynnError::invalid_argument("duplicate-key diagnostic escaped its cap")
			})?;
		assert_eq!(error.status().condition(), crate::gql_status::Condition::ProgramLimitExceeded);
	}
	let control = ExecutionControl::new(&options, None);
	control.admit_reactive_work(1)?;
	let members = vec![
		Value::Object(Object::from_iter([("a", 1)])),
		Value::Object(Object::from_iter([("b", 2)])),
	];
	assert_eq!(
		crate::gql::execution::eval::cast(
			Value::from(members),
			&Type::Object,
			None,
			Some(&control)
		)?,
		Value::Object(Object::from_iter([("a", 1), ("b", 2)]))
	);
	let mut rejected = store.begin();
	let error = rejected
		.define_live_binding(
			crate::live::LiveExpr::Cast {
				target: "int".into(),
				value: Box::new(crate::live::LiveExpr::Parameter("payload".into())),
			},
			Object::from_iter([("payload", payload)]),
			crate::live::LiveBindingOptions::default().with_budgets(256 * 1024, 1_000_000)?,
		)
		.err()
		.ok_or_else(|| FerrosGrynnError::invalid_argument("SDK CAST renderer budget ignored"))?;
	assert_eq!(
		error.status().condition(),
		crate::gql_status::Condition::ProgramLimitExceeded,
		"{error}"
	);
	rejected.commit()?;
	assert!(store.inner.current_root().live_catalog().bindings.is_empty());
	Ok(())
}

/// Reactive scalar quantifiers share the bounded root comparison work guard.
#[test]
fn live_recipe_binary_reactive_work_bounded() -> FerrosGrynnResult<()> {
	use crate::gql::{
		ExecutionControl,
		QueryExecutionOptions,
		ir::{TypedExpr, TypedExprKind},
	};
	let span = crate::gql_status::TextSpan::new(0, 0);
	let options = QueryExecutionOptions {
		barrier_memory_bytes: std::num::NonZeroUsize::new(16 * 1024 * 1024)
			.ok_or_else(|| FerrosGrynnError::invalid_argument("binary fixture scratch is zero"))?,
		max_loop_iterations: 1000,
		..QueryExecutionOptions::default()
	};
	let values = Value::from((0_i64..100).map(Value::from).collect::<Vec<_>>());
	let expression = TypedExpr {
		kind: TypedExprKind::Binary {
			left: Box::new(TypedExpr {
				kind: TypedExprKind::Constant(values.clone()),
				ty: Type::Array(Box::new(Type::Int)),
				span,
			}),
			op: crate::syn::BinaryOperator::AllIn,
			right: Box::new(TypedExpr {
				kind: TypedExprKind::Constant(values),
				ty: Type::Array(Box::new(Type::Int)),
				span,
			}),
		},
		ty: Type::Bool,
		span,
	};
	let control = ExecutionControl::new(&options, None);
	control.admit_reactive_work(1)?;
	let error = crate::gql::execution::eval::eval(&expression, None, &[], "", None, Some(&control))
		.err()
		.ok_or_else(|| FerrosGrynnError::invalid_argument("reactive binary work ignored"))?;
	assert_eq!(error.status().condition(), crate::gql_status::Condition::ProgramLimitExceeded);
	Ok(())
}

/// Invalid range diagnostics are admitted before formatting escaped bounds.
#[test]
fn live_recipe_range_failure_render_bounded() -> FerrosGrynnResult<()> {
	use crate::gql::{
		ExecutionControl,
		QueryExecutionOptions,
		ir::{TypedExpr, TypedExprKind},
	};
	let span = crate::gql_status::TextSpan::new(0, 0);
	let options = QueryExecutionOptions {
		barrier_memory_bytes: std::num::NonZeroUsize::new(64 * 1024)
			.ok_or_else(|| FerrosGrynnError::invalid_argument("Range fixture scratch is zero"))?,
		..QueryExecutionOptions::default()
	};
	let control = ExecutionControl::new(&options, None);
	control.admit_reactive_work(1)?;
	let expression = TypedExpr {
		kind: TypedExprKind::Range {
			start: Some(Box::new(TypedExpr {
				kind: TypedExprKind::Constant(Value::from(format!("z{}", "\0".repeat(20_000)))),
				ty: Type::String,
				span,
			})),
			end: Some(Box::new(TypedExpr {
				kind: TypedExprKind::Constant(Value::from("a")),
				ty: Type::String,
				span,
			})),
			inclusive_end: true,
		},
		ty: Type::Range(Box::new(Type::String)),
		span,
	};
	let error = crate::gql::execution::eval::eval(&expression, None, &[], "", None, Some(&control))
		.err()
		.ok_or_else(|| FerrosGrynnError::invalid_argument("invalid Range succeeded"))?;
	assert_eq!(error.status().condition(), crate::gql_status::Condition::ProgramLimitExceeded);
	let store = Datastore::in_memory();
	let root = store.inner.current_root();
	let control = ExecutionControl::new(&options, None);
	let observe = |_key| {};
	let work = |_work| Ok(());
	let read = crate::gql::execution::root_read::RootRead::new(
		&root,
		&control,
		&observe,
		&work,
		std::path::PathBuf::new(),
	);
	assert!(!control.has_reactive_copies());
	let error = crate::gql::execution::eval::eval(
		&expression,
		None,
		&[],
		"",
		crate::gql::execution::QueryRead::bounded_fields(&read),
		Some(&control),
	)
	.err()
	.ok_or_else(|| FerrosGrynnError::invalid_argument("RootRead invalid Range succeeded"))?;
	assert_eq!(error.status().condition(), crate::gql_status::Condition::ProgramLimitExceeded);
	let control = ExecutionControl::new(&options, None);
	control.admit_reactive_work(1)?;
	let TypedExprKind::Range {
		start,
		..
	} = expression.kind
	else {
		return Err(FerrosGrynnError::invalid_argument("Range fixture kind changed"));
	};
	let open = TypedExpr {
		kind: TypedExprKind::Range {
			start,
			end: None,
			inclusive_end: false,
		},
		ty: Type::Range(Box::new(Type::String)),
		span,
	};
	assert!(matches!(
		crate::gql::execution::eval::eval(&open, None, &[], "", None, Some(&control))?,
		Value::Range(_)
	));
	let mut rejected = store.begin();
	let error = rejected
		.define_live_binding(
			crate::live::LiveExpr::Range {
				start: Some(Box::new(crate::live::LiveExpr::Literal(Value::from(format!(
					"z{}",
					"\0".repeat(20_000)
				))))),
				end: Some(Box::new(crate::live::LiveExpr::Literal("a".into()))),
				inclusive_end: true,
			},
			Object::new(),
			crate::live::LiveBindingOptions::default().with_budgets(256 * 1024, 1_000_000)?,
		)
		.err()
		.ok_or_else(|| FerrosGrynnError::invalid_argument("SDK Range scratch ignored"))?;
	assert_eq!(error.status().condition(), crate::gql_status::Condition::ProgramLimitExceeded);
	rejected.commit()?;
	assert!(store.inner.current_root().live_catalog().bindings.is_empty());
	Ok(())
}

/// Shallow nullable results must not grow inferred type depth with branch
/// count; the same compiler reduction owns CASE and collection item types.
#[test]
fn live_recipe_nullable_inference_stays_bounded() -> FerrosGrynnResult<()> {
	let store = Datastore::in_memory();
	let root = store.inner.current_root();
	let catalog = crate::gql::root_catalog::RootCatalog(&root);
	let optional = Type::Option(Box::new(Type::Int));
	let mut cases = String::from("CASE WHEN true THEN 1");
	let mut elements = String::from("1");
	for _ in 0..96 {
		cases.push_str(" WHEN false THEN 1 WHEN false THEN NONE WHEN false THEN NULL");
		elements.push_str(", 1, NONE, NULL");
	}
	cases.push_str(" END");
	for (expression, expected) in [
		(cases.clone(), optional.clone()),
		(
			"CASE WHEN true THEN NONE WHEN false THEN CASE WHEN true THEN 1 ELSE NONE END END"
				.to_owned(),
			optional.clone(),
		),
		(format!("[{elements}]"), Type::Array(Box::new(optional.clone()))),
		(format!("set[{elements}]"), Type::Set(Box::new(optional))),
	] {
		let source = format!("RETURN {expression};");
		let syntax = crate::gql::parse(&source, crate::gql::ParserSettings::default())?;
		let typed = crate::gql::analysis::analyze(&syntax, &source, &catalog, &Object::new())?;
		let Some(crate::gql::ir::LogicalStatement::Return {
			value,
		}) = typed.statements.first()
		else {
			return Err(FerrosGrynnError::invalid_argument("nullable inference return missing"));
		};
		assert_eq!(value.ty, expected);
	}
	let mut session = store.session();
	assert_eq!(
		execute_value(
			&mut session,
			&format!("LIVE LET $nullable = {cases}; RETURN $nullable;"),
			&Object::new()
		)?,
		1.into()
	);
	execute_value(
		&mut session,
		"DEFINE PURE FUNCTION fn::nullable($value: option<int>) -> option<int> { RETURN CASE WHEN \
		 true THEN $value WHEN false THEN 1 END; }; LET $pass = |$value: option<int>| -> \
		 option<int> { RETURN CASE WHEN true THEN $value WHEN false THEN 1 END; };",
		&Object::new(),
	)?;
	for (expression, expected) in [
		(
			"fn::nullable(CASE WHEN true THEN 1 WHEN false THEN NONE WHEN false THEN 1 END)",
			1.into(),
		),
		("fn::nullable(CASE WHEN true THEN NONE ELSE 1 END)", Value::None),
		("$pass(CASE WHEN true THEN 1 WHEN false THEN NONE WHEN false THEN 1 END)", 1.into()),
		("$pass(CASE WHEN true THEN NONE ELSE 1 END)", Value::None),
	] {
		assert_eq!(
			execute_value(&mut session, &format!("RETURN {expression};"), &Object::new())?,
			expected
		);
	}
	assert_eq!(
		execute_value(
			&mut session,
			"LIVE LET $called = fn::nullable(CASE WHEN true THEN 1 WHEN false THEN NONE WHEN \
			 false THEN 1 END); RETURN $called;",
			&Object::new(),
		)?,
		1.into()
	);
	Ok(())
}

/// Field/index recipes retain alias resolution, frozen captures and exact
/// reads.
#[test]
fn live_recipe_gql_access_and_index() -> FerrosGrynnResult<()> {
	let store = Datastore::in_memory();
	let mut session = store.session();
	execute_value(
		&mut session,
		"DEFINE CLASS person TYPE NODE; DEFINE FIELD age ON CLASS person TYPE int; DEFINE FIELD \
		 details ON CLASS person TYPE object; CREATE person:a SET age = 19, details = {name: \
		 'Ada'}; LET $fixed = {limit: 7}; LIVE LET $policy = {values: [18, 21], missing: NONE}; \
		 LIVE LET $index = 0; LIVE LET $minimum = $policy.values[$index]; LIVE LET $optional = \
		 $policy.missing?.value; LIVE LET $frozen = $fixed.limit;",
		&Object::new(),
	)?;
	assert_eq!(execute_value(&mut session, "RETURN $minimum;", &Object::new())?, 18.into());
	assert_eq!(execute_value(&mut session, "RETURN $optional;", &Object::new())?, Value::None);
	let oracle = "SELECT VALUE {age: P.age, name: P.details?.name, chosen: \
	              $policy.values[$index], fixed: $frozen} FROM person AS p;";
	let before = execute_value(&mut session, oracle, &Object::new())?;
	let id = identity(execute_value(
		&mut session,
		"LIVE SELECT VALUE {age: P.age, name: P.details?.name, chosen: $policy.values[$index], \
		 fixed: $frozen} FROM person AS p;",
		&Object::new(),
	)?)?;
	execute_value(
		&mut session,
		"ALTER LET $fixed VALUE {limit: 99}; ALTER LIVE LET $index VALUE 1;",
		&Object::new(),
	)?;
	assert_eq!(execute_value(&mut session, "RETURN $frozen;", &Object::new())?, 7.into());
	assert_eq!(execute_value(&mut session, "RETURN $minimum;", &Object::new())?, 21.into());
	let after = execute_value(&mut session, oracle, &Object::new())?;
	let LiveNext::Delivery(delivery) = session.next_live(id)? else {
		return Err(FerrosGrynnError::invalid_argument("access recipe delivery missing"));
	};
	let LivePayload::Batch(batch) = delivery.payload() else {
		return Err(FerrosGrynnError::invalid_argument("access recipe batch missing"));
	};
	assert_eq!(batch.events()[0].before(), before.as_array().and_then(|rows| rows.get(0)));
	assert_eq!(batch.events()[0].after(), after.as_array().and_then(|rows| rows.get(0)));
	assert!(
		execute_value(
			&mut session,
			"ALTER LIVE LET $policy VALUE {values: [$minimum]};",
			&Object::new(),
		)
		.is_err()
	);
	assert_eq!(execute_value(&mut session, "RETURN $minimum;", &Object::new())?, 21.into());
	for expression in ["18.value", "[1]['wrong']", "NONE.value"] {
		let count = store.inner.current_root().live_catalog().bindings.len();
		assert!(
			execute_value(&mut session, &format!("LIVE LET $bad = {expression};"), &Object::new(),)
				.is_err(),
			"{expression}"
		);
		assert_eq!(store.inner.current_root().live_catalog().bindings.len(), count);
	}
	let mut rejected = store.begin();
	let error = rejected
		.define_live_binding(
			crate::live::LiveExpr::Access {
				base: Box::new(crate::live::LiveExpr::Literal(Value::None)),
				field: "x".repeat(1024 * 1024).into(),
				null_safe: true,
			},
			Object::new(),
			crate::live::LiveBindingOptions::default().with_budgets(128 * 1024, 1_000_000)?,
		)
		.err()
		.ok_or_else(|| FerrosGrynnError::invalid_argument("access field cap ignored"))?;
	assert_eq!(error.status().condition(), crate::gql_status::Condition::ProgramLimitExceeded);
	Ok(())
}

/// Reactive cursor selection reads the statement root without persisting
/// cursors.
#[test]
fn live_gql_binding_start_after_selection() -> FerrosGrynnResult<()> {
	let store = Datastore::in_memory();
	let mut session = store.session();
	execute_value(
		&mut session,
		"DEFINE CLASS item TYPE NODE; DEFINE FIELD rank ON CLASS item TYPE int; CREATE item:a SET \
		 rank = 1; CREATE item:b SET rank = 2; CREATE item:c SET rank = 3; CREATE item:d SET rank \
		 = 4; LIVE LET $pick = true;",
		&Object::new(),
	)?;
	let page = "SELECT VALUE rank FROM item ORDER BY rank LIMIT 1";
	let first = session
		.execute(&format!("{page};"), &Object::new())?
		.into_iter()
		.next()
		.ok_or_else(|| FerrosGrynnError::invalid_argument("first page missing"))?;
	assert_eq!(first.result?.value(), &Value::from(vec![Value::from(1)]));
	let first_cursor = first
		.continuation
		.ok_or_else(|| FerrosGrynnError::invalid_argument("first cursor missing"))?;
	let mut parameters = Object::from_iter([("first", Value::from(first_cursor))]);
	let second = session
		.execute(&format!("{page} START AFTER $first;"), &parameters)?
		.into_iter()
		.next()
		.ok_or_else(|| FerrosGrynnError::invalid_argument("second page missing"))?;
	assert_eq!(second.result?.value(), &Value::from(vec![Value::from(2)]));
	parameters.insert(
		"second",
		Value::from(
			second
				.continuation
				.ok_or_else(|| FerrosGrynnError::invalid_argument("second cursor missing"))?,
		),
	);
	let selection = format!("{page} START AFTER CASE WHEN $pick THEN $first ELSE $second END;");
	assert_eq!(execute_value(&mut session, &selection, &parameters)?, Value::from(vec![2.into()]));
	execute_value(&mut session, "ALTER LIVE LET $pick VALUE false;", &Object::new())?;
	assert_eq!(execute_value(&mut session, &selection, &parameters)?, Value::from(vec![3.into()]));
	let count = store.inner.current_root().live_catalog().bindings.len();
	assert!(execute_value(&mut session, "LIVE LET $bad = $first;", &parameters).is_err());
	assert_eq!(store.inner.current_root().live_catalog().bindings.len(), count);
	Ok(())
}

/// Record-valued bindings preserve negative/external dependency tracking.
#[test]
fn live_recipe_access_external_record() -> FerrosGrynnResult<()> {
	let store = Datastore::in_memory();
	let mut session = store.session();
	execute_value(
		&mut session,
		"DEFINE CLASS person TYPE NODE CHANGEFEED 1d; CREATE person:a; DEFINE CLASS policy TYPE \
		 NODE; DEFINE FIELD age ON CLASS policy TYPE int; CREATE policy:a SET age = 18; LIVE LET \
		 $record = policy:a; LIVE LET $minimum = $record.age;",
		&Object::new(),
	)?;
	let id = identity(execute_value(
		&mut session,
		"LIVE SELECT VALUE $minimum FROM person;",
		&Object::new(),
	)?)?;
	let floor = store.begin().commit()?.versionstamp();
	execute_value(&mut session, "UPDATE policy:a SET age = 21;", &Object::new())?;
	let LiveNext::Delivery(delivery) = session.next_live(id)? else {
		return Err(FerrosGrynnError::invalid_argument("external access delivery missing"));
	};
	assert!(matches!(delivery.payload(), LivePayload::Batch(batch)
		if batch.events()[0].before() == Some(&18.into()) && batch.events()[0].after() == Some(&21.into())));
	let history = identity(execute_value(
		&mut session,
		"LET $external = policy:a; LIVE SELECT VALUE $external.age FROM person SINCE $floor;",
		&Object::from_iter([("floor", Value::from(floor.get()))]),
	)?)?;
	let LiveNext::Delivery(delivery) = session.next_live(history)? else {
		return Err(FerrosGrynnError::invalid_argument("external coverage GAP missing"));
	};
	assert!(matches!(delivery.payload(), LivePayload::Gap(gap)
		if gap.reason() == crate::live::LiveGapReason::InsufficientCoverage));
	Ok(())
}

/// Compound projections reconstructed from captured rows never read today's
/// values.
#[test]
fn live_recipe_historical_compounds() -> FerrosGrynnResult<()> {
	let store = Datastore::in_memory();
	let mut session = store.session();
	execute_value(
		&mut session,
		"DEFINE CLASS item TYPE NODE CHANGEFEED 1d; DEFINE FIELD age ON CLASS item TYPE int; \
		 CREATE item:a SET age = 17;",
		&Object::new(),
	)?;
	let floor = store.begin().commit()?.versionstamp();
	execute_value(
		&mut session,
		"UPDATE item:a SET age = 19; UPDATE item:a SET age = 99;",
		&Object::new(),
	)?;
	let id = identity(execute_value(
		&mut session,
		"LIVE SELECT VALUE {values: [age, -age, +age], first: [age][0], nested: {value: \
		 age}.value, unique: set[age, age, -age], selected: IF age >= 18 THEN age ELSE -age END, \
		 case: CASE age WHEN 17 THEN -age WHEN 19 THEN age ELSE 0 END, missing_case: CASE age \
		 WHEN 19 THEN age END, cast: <string>age, range: age.., through: ..=age, power: age ** 2, \
		 remainder: age % 2, truthy: !!age, quantifier: [age] ALLIN [age, 99]} FROM item SINCE \
		 $floor;",
		&Object::from_iter([("floor", Value::from(floor.get()))]),
	)?)?;
	let LiveNext::Delivery(delivery) = session.next_live(id)? else {
		return Err(FerrosGrynnError::invalid_argument("historical compound delivery missing"));
	};
	let LivePayload::Batch(batch) = delivery.payload() else {
		return Err(FerrosGrynnError::invalid_argument("historical compound coverage missing"));
	};
	let expected = |age: i64| -> FerrosGrynnResult<Value> {
		Ok(Value::Object(Object::from_iter([
			("power", Value::from(age * age)),
			("remainder", Value::from(age % 2)),
			("truthy", Value::Bool(true)),
			("quantifier", Value::Bool(true)),
			("values", Value::from(vec![Value::from(age), Value::from(-age), Value::from(age)])),
			("first", Value::from(age)),
			("nested", Value::from(age)),
			("cast", Value::from(age.to_string())),
			(
				"range",
				Value::Range(crate::values::Range::new(
					std::ops::Bound::Included(age.into()),
					std::ops::Bound::Unbounded,
				)?),
			),
			(
				"through",
				Value::Range(crate::values::Range::new(
					std::ops::Bound::Unbounded,
					std::ops::Bound::Included(age.into()),
				)?),
			),
			(
				"case",
				Value::from(if age == 17 {
					-age
				} else {
					age
				}),
			),
			(
				"missing_case",
				if age == 19 {
					Value::from(age)
				} else {
					Value::None
				},
			),
			(
				"unique",
				Value::Set(crate::values::Set::from(vec![Value::from(age), Value::from(-age)])),
			),
			(
				"selected",
				Value::from(if age >= 18 {
					age
				} else {
					-age
				}),
			),
		])))
	};
	assert_eq!(batch.events().len(), 1);
	assert_eq!(batch.events()[0].before(), Some(&expected(17)?));
	assert_eq!(batch.events()[0].after(), Some(&expected(19)?));
	Ok(())
}

/// Scalar history inputs read current binding values, retain their snapshot
/// through a transaction, and never enable primary-record navigation.
#[test]
fn live_gql_binding_changefeed_inputs() -> FerrosGrynnResult<()> {
	let store = Datastore::in_memory();
	let mut setup = store.begin();
	let class = setup.define_class(ClassSchema::new("item", AtomKind::Node, [])?)?;
	setup.configure_changefeed(
		ChangefeedSource::Class(class),
		ChangefeedConfig::new("P1D".parse()?)?,
	)?;
	let floor = setup.commit()?.versionstamp();
	let feed = store.begin().changefeed_info(ChangefeedSource::Class(class))?.id();
	let mut stamps = Vec::new();
	for record in ["item:a", "item:b"] {
		let mut write = store.begin();
		write.create_atom(record.parse()?, PropertyMap::new())?;
		stamps.push(write.commit()?.versionstamp());
	}
	let mut session = store.session();
	execute_value(
		&mut session,
		"LIVE LET $feed = $identity; LIVE LET $since = $floor; LIVE LET $take = 1;",
		&Object::from_iter([
			("identity", Value::from(feed.as_uuid())),
			("floor", Value::from(floor.get())),
		]),
	)?;
	let inputs = [
		"SHOW CHANGES FOR CLASS item SINCE $since LIMIT $take;",
		"SHOW CHANGES FOR CHANGEFEED $feed SINCE $since LIMIT $take;",
	];
	for (position, expected) in [(floor, stamps[0]), (stamps[0], stamps[1])] {
		execute_value(
			&mut session,
			"ALTER LIVE LET $since VALUE $position;",
			&Object::from_iter([("position", Value::from(position.get()))]),
		)?;
		for input in inputs {
			let value = execute_value(&mut session, input, &Object::new())?;
			assert_eq!(value.as_array().map(crate::values::Array::len), Some(1));
			assert_eq!(
				value
					.as_array()
					.and_then(|rows| rows.get(0))
					.and_then(Value::as_object)
					.and_then(|row| row.get("versionstamp")),
				Some(&Value::from(expected.get()))
			);
		}
	}
	let info = execute_value(&mut session, "INFO FOR CHANGEFEED $feed;", &Object::new())?;
	assert_eq!(info.as_object().and_then(|row| row.get("id")), Some(&Value::from(feed.as_uuid())));
	let binding = execute_value(&mut session, "INFO FOR LIVE LET $since;", &Object::new())?;
	let binding = binding
		.as_object()
		.and_then(|row| row.get("id"))
		.ok_or_else(|| {
			FerrosGrynnError::invalid_argument("history fixture binding identity missing")
		})?
		.clone();
	execute_value(&mut session, "BEGIN;", &Object::new())?;
	let before = execute_value(&mut session, inputs[0], &Object::new())?;
	let mut other = store.session();
	execute_value(
		&mut other,
		"ALTER LIVE LET $binding VALUE $position;",
		&Object::from_iter([("binding", binding), ("position", Value::from(stamps[1].get()))]),
	)?;
	assert_eq!(execute_value(&mut session, inputs[0], &Object::new())?, before);
	execute_value(&mut session, "CANCEL;", &Object::new())?;
	assert_eq!(
		execute_value(&mut session, inputs[0], &Object::new())?,
		Value::from(Vec::<Value>::new())
	);
	assert!(
		execute_value(&mut session, "SHOW CHANGES FOR CLASS item SINCE item:a.id;", &Object::new())
			.is_err()
	);
	for (value, options, message) in [
		(
			Value::from(vec![Value::from(0); 4096]),
			crate::gql::QueryExecutionOptions {
				max_loop_iterations: 128,
				..crate::gql::QueryExecutionOptions::default()
			},
			"changefeed operand work",
		),
		(
			Value::Bytes(vec![0_u8; 256 * 1024].into()),
			crate::gql::QueryExecutionOptions {
				barrier_memory_bytes: std::num::NonZeroUsize::new(128 * 1024).ok_or_else(|| {
					FerrosGrynnError::invalid_argument("history fixture scratch is zero")
				})?,
				..crate::gql::QueryExecutionOptions::default()
			},
			"changefeed operand scratch",
		),
	] {
		execute_value(
			&mut session,
			"ALTER LIVE LET $since VALUE $input;",
			&Object::from_iter([("input", value)]),
		)?;
		let results = session.execute_with_options(inputs[0], &Object::new(), options)?;
		let failure = results
			.into_iter()
			.next()
			.ok_or_else(|| FerrosGrynnError::invalid_argument("history fixture result missing"))?
			.result
			.err()
			.ok_or_else(|| FerrosGrynnError::invalid_argument("history input bypassed budget"))?;
		assert!(failure.to_string().contains(message), "{failure}");
	}
	Ok(())
}

/// ACK inputs own the fixed token while the session retains its consumer.
fn ack_parameters(id: LiveId, cursor: &crate::live::LiveCursor) -> Object {
	Object::from_iter([("qid", Value::from(id.as_uuid())), ("token", Value::from(cursor.encode()))])
}

/// UUID and cursor validity do not replace delivery issuance by the session.
#[test]
fn live_gql_ack_requires_issued_session_delivery_and_confirms_whole_commit() -> FerrosGrynnResult<()>
{
	use crate::{
		changefeed::Versionstamp,
		live::{LiveCursor, LiveVersion},
		values::Uuid,
	};
	let store = Datastore::in_memory();
	let mut session = store.session();
	execute_value(&mut session, "DEFINE CLASS person TYPE NODE;", &Object::new())?;
	let id = identity(execute_value(&mut session, "LIVE SELECT * FROM person;", &Object::new())?)?;
	execute_value(&mut session, "CREATE person:a;", &Object::new())?;
	let info = store.begin().live_info(id)?;
	let unissued = LiveCursor::new(
		info.cursor().database_id(),
		id,
		info.version(),
		store.begin().commit()?.versionstamp(),
	)?;
	assert!(
		execute_value(&mut session, "ACK LIVE $qid CURSOR $token;", &ack_parameters(id, &unissued))
			.is_err()
	);
	let LiveNext::Delivery(delivery) = session.next_live(id)? else {
		return Err(FerrosGrynnError::invalid_argument("session ACK delivery missing"));
	};
	let token = delivery.token().clone();
	for wrong in [
		LiveCursor::new(Uuid::now_v7(), id, token.version(), token.versionstamp())?,
		LiveCursor::new(
			token.database_id(),
			LiveId::from_uuid(Uuid::now_v7())?,
			token.version(),
			token.versionstamp(),
		)?,
		LiveCursor::new(
			token.database_id(),
			id,
			LiveVersion::new(token.version().get() + 1)?,
			token.versionstamp(),
		)?,
		LiveCursor::new(token.database_id(), id, token.version(), Versionstamp::new(u64::MAX))?,
	] {
		assert!(
			execute_value(
				&mut session,
				"ACK LIVE $qid CURSOR $token;",
				&ack_parameters(id, &wrong)
			)
			.is_err()
		);
	}
	execute_value(&mut session, "ACK LIVE $qid CURSOR $token;", &ack_parameters(id, &token))?;
	assert_eq!(store.begin().live_info(id)?.cursor(), &token);
	assert!(matches!(session.next_live(id)?, LiveNext::Idle));
	let stamp = store.begin().commit()?.versionstamp();
	execute_value(&mut session, "ACK LIVE $qid CURSOR $token;", &ack_parameters(id, &token))?;
	assert_eq!(store.begin().commit()?.versionstamp(), stamp);
	let handle = session.take_live_subscription(id)?;
	assert_eq!(handle.cursor(), &token);
	assert!(session.next_live(id).is_err());
	assert!(
		execute_value(&mut session, "ACK LIVE $qid CURSOR $token;", &ack_parameters(id, &token))
			.is_err()
	);
	Ok(())
}

/// CANCEL/TRY/extraction preserve queue payloads until a valid owning COMMIT.
#[test]
fn live_gql_ack_rollback_and_transferred_authority() -> FerrosGrynnResult<()> {
	let store = Datastore::in_memory();
	let mut session = store.session();
	execute_value(&mut session, "DEFINE CLASS person TYPE NODE;", &Object::new())?;
	let id = identity(execute_value(&mut session, "LIVE SELECT * FROM person;", &Object::new())?)?;
	execute_value(&mut session, "CREATE person:a;", &Object::new())?;
	let LiveNext::Delivery(delivery) = session.next_live(id)? else {
		return Err(FerrosGrynnError::invalid_argument("rollback ACK delivery missing"));
	};
	let token = delivery.token().clone();
	let inputs = ack_parameters(id, &token);
	let before = store.begin().live_info(id)?.cursor().clone();
	for text in [
		"BEGIN; ACK LIVE $qid CURSOR $token; CANCEL;",
		"TRY { ACK LIVE $qid CURSOR $token; THROW 'undo'; } CATCH |$e| {};",
	] {
		execute_value(&mut session, text, &inputs)?;
		assert_eq!(store.begin().live_info(id)?.cursor(), &before);
		assert!(
			matches!(session.next_live(id)?, LiveNext::Delivery(ref next) if next.token() == &token)
		);
	}
	execute_value(&mut session, "BEGIN; ACK LIVE $qid CURSOR $token;", &inputs)?;
	let mut handle = session.take_live_subscription(id)?;
	assert!(execute_value(&mut session, "COMMIT;", &Object::new()).is_err());
	assert_eq!(store.begin().live_info(id)?.cursor(), &before);
	assert_eq!(handle.cursor(), &before);
	assert!(matches!(handle.next()?, LiveNext::Delivery(ref next) if next.token() == &token));
	handle.ack(&token)?;
	assert!(matches!(handle.next()?, LiveNext::Idle));
	Ok(())
}

/// Pure and repeated control ACKs bypass a lease without admitting primary DML.
#[test]
fn live_gql_ack_during_import_keeps_items_and_rejects_mixed_writes() -> FerrosGrynnResult<()> {
	use crate::datastore::ImportOptions;
	let store = Datastore::in_memory();
	let mut session = store.session();
	execute_value(
		&mut session,
		"DEFINE CLASS person TYPE NODE CHANGEFEED 1d FULL;",
		&Object::new(),
	)?;
	let id = identity(execute_value(&mut session, "LIVE SELECT * FROM person;", &Object::new())?)?;
	execute_value(&mut session, "CREATE person:seed;", &Object::new())?;
	let LiveNext::Delivery(delivery) = session.next_live(id)? else {
		return Err(FerrosGrynnError::invalid_argument("import ACK delivery missing"));
	};
	let token = delivery.token().clone();
	let inputs = ack_parameters(id, &token);
	let report = store.import(
		ImportOptions {
			segment_size: 1,
			..ImportOptions::default()
		},
		|importer| {
			importer.apply_item(|tx| tx.create_atom("person:a".parse()?, PropertyMap::new()))?;
			execute_value(&mut session, "BEGIN; ACK LIVE $qid CURSOR $token; CANCEL;", &inputs)?;
			execute_value(
				&mut session,
				"TRY { ACK LIVE $qid CURSOR $token; THROW 'undo'; } CATCH |$e| {};",
				&inputs,
			)?;
			execute_value(
				&mut session,
				"ACK LIVE $qid CURSOR $token; ACK LIVE $qid CURSOR $token;",
				&inputs,
			)?;
			assert_eq!(store.begin().live_info(id)?.cursor(), &token);
			assert!(
				execute_value(
					&mut session,
					"IF true { ACK LIVE $qid CURSOR $token; CREATE person:forbidden; };",
					&inputs
				)
				.is_err()
			);
			importer.apply_item(|tx| tx.create_atom("person:b".parse()?, PropertyMap::new()))?;
			Ok(())
		},
	)?;
	assert_eq!(report.applied_items(), 2);
	assert_eq!(store.begin().live_info(id)?.cursor(), &token);
	let Value::Array(rows) = execute_value(&mut session, "SELECT * FROM person;", &Object::new())?
	else {
		return Err(FerrosGrynnError::invalid_argument("ACK import rows missing"));
	};
	assert_eq!(rows.len(), 3);
	Ok(())
}

/// Cursor navigation reads the fixed root; expression copies spend admitted
/// scratch.
#[test]
fn live_gql_ack_cursor_expression_reads_and_resource_rejection() -> FerrosGrynnResult<()> {
	let store = Datastore::in_memory();
	let mut session = store.session();
	execute_value(
		&mut session,
		"DEFINE CLASS person TYPE NODE; DEFINE CLASS token_store TYPE NODE; DEFINE FIELD cursor \
		 ON CLASS token_store TYPE bytes;",
		&Object::new(),
	)?;
	let id = identity(execute_value(&mut session, "LIVE SELECT * FROM person;", &Object::new())?)?;
	execute_value(&mut session, "CREATE person:a;", &Object::new())?;
	let LiveNext::Delivery(delivery) = session.next_live(id)? else {
		return Err(FerrosGrynnError::invalid_argument("expression ACK delivery missing"));
	};
	let before = store.begin().live_info(id)?.cursor().clone();
	let mut parameters = ack_parameters(id, delivery.token());
	parameters.insert("reference", Value::from("token_store:one".parse::<RecordId>()?));
	parameters.insert("padding", Value::Bytes(vec![0_u8; 64 * 1024].into()));
	let options = crate::gql::QueryExecutionOptions {
		barrier_memory_bytes: std::num::NonZeroUsize::new(16 * 1024)
			.ok_or_else(|| FerrosGrynnError::invalid_argument("fixture scratch is zero"))?,
		..crate::gql::QueryExecutionOptions::default()
	};
	let mut results = session.execute_with_options(
		"ACK LIVE $qid CURSOR IF true THEN $padding ELSE $token END;",
		&parameters,
		options,
	)?;
	let error = results
		.pop()
		.ok_or_else(|| FerrosGrynnError::invalid_argument("expression rejection missing"))?
		.result
		.err()
		.ok_or_else(|| FerrosGrynnError::invalid_argument("oversized cursor admitted"))?;
	assert!(error.to_string().contains("scratch"), "{error}");
	assert_eq!(store.begin().live_info(id)?.cursor(), &before);
	assert!(
		matches!(session.next_live(id)?, LiveNext::Delivery(ref next) if next.token() == delivery.token())
	);
	execute_value(
		&mut session,
		"CREATE token_store:one SET cursor = $token; ACK LIVE $qid CURSOR $reference.cursor;",
		&parameters,
	)?;
	assert_eq!(store.begin().live_info(id)?.cursor(), delivery.token());
	assert!(matches!(session.next_live(id)?, LiveNext::Idle));
	Ok(())
}

/// Typed effects admit nested ACK calls only in implicit or explicit write
/// transactions.
#[test]
fn live_gql_ack_nested_effects_and_cursor_expressions() -> FerrosGrynnResult<()> {
	let store = Datastore::in_memory();
	let mut session = store.session();
	execute_value(&mut session, "DEFINE CLASS person TYPE NODE;", &Object::new())?;
	execute_value(
		&mut session,
		"DEFINE IMPURE FUNCTION fn::confirm(qid: any, token: bytes) -> any { ACK LIVE $qid CURSOR \
		 IF true THEN $token ELSE $token END; RETURN NONE; };",
		&Object::new(),
	)?;
	let id = identity(execute_value(&mut session, "LIVE SELECT * FROM person;", &Object::new())?)?;
	for (index, text) in [
		"BEGIN; RETURN fn::confirm($qid, $token); COMMIT;",
		"RETURN fn::confirm($qid, $token);",
		"LET $confirm = |$q: any, $c: bytes| -> any { ACK LIVE $q CURSOR $c; RETURN NONE; }; \
		 RETURN $confirm($qid, $token);",
	]
	.into_iter()
	.enumerate()
	{
		execute_value(&mut session, &format!("CREATE person:p{index};"), &Object::new())?;
		let LiveNext::Delivery(delivery) = session.next_live(id)? else {
			return Err(FerrosGrynnError::invalid_argument("nested ACK delivery missing"));
		};
		let parameters = ack_parameters(id, delivery.token());
		execute_value(&mut session, "BEGIN READ ONLY;", &Object::new())?;
		assert!(
			execute_value(&mut session, "RETURN fn::confirm($qid, $token);", &parameters).is_err()
		);
		execute_value(&mut session, text, &parameters)?;
		assert_eq!(store.begin().live_info(id)?.cursor(), delivery.token());
		assert!(matches!(session.next_live(id)?, LiveNext::Idle));
	}
	Ok(())
}

/// Declared STABLE rejects mutating callbacks; declared IMPURE admits them.
#[test]
fn live_gql_ack_stable_callbacks_cannot_exceed_declared_effect() -> FerrosGrynnResult<()> {
	let store = Datastore::in_memory();
	let mut session = store.session();
	execute_value(
		&mut session,
		"DEFINE CLASS person TYPE NODE; DEFINE STABLE FUNCTION fn::indirect(cb: any, qid: any, \
		 token: bytes) -> any { RETURN $cb($qid, $token); }; DEFINE IMPURE FUNCTION \
		 fn::write_indirect(cb: any, qid: any, token: bytes) -> any { RETURN $cb($qid, $token); };",
		&Object::new(),
	)?;
	let id = identity(execute_value(&mut session, "LIVE SELECT * FROM person;", &Object::new())?)?;
	execute_value(
		&mut session,
		"LET $confirm = |$q: any, $c: bytes| -> any { ACK LIVE $q CURSOR $c; CREATE \
		 person:forbidden; RETURN NONE; }; CREATE person:a;",
		&Object::new(),
	)?;
	let LiveNext::Delivery(delivery) = session.next_live(id)? else {
		return Err(FerrosGrynnError::invalid_argument("effect ceiling delivery missing"));
	};
	let before = store.begin().live_info(id)?.cursor().clone();
	let parameters = ack_parameters(id, delivery.token());
	for prefix in ["", "BEGIN; "] {
		assert!(
			execute_value(
				&mut session,
				&format!("{prefix}RETURN fn::indirect($confirm, $qid, $token);"),
				&parameters
			)
			.is_err()
		);
		assert_eq!(store.begin().live_info(id)?.cursor(), &before);
		assert!(
			matches!(session.next_live(id)?, LiveNext::Delivery(ref next) if next.token() == delivery.token())
		);
		assert_eq!(
			execute_value(&mut session, "SELECT VALUE count() FROM person;", &Object::new())?,
			Value::from(vec![Value::from(1)])
		);
	}
	execute_value(&mut session, "RETURN fn::write_indirect($confirm, $qid, $token);", &parameters)?;
	assert_eq!(store.begin().live_info(id)?.cursor(), delivery.token());
	assert_eq!(
		execute_value(&mut session, "SELECT VALUE count() FROM person;", &Object::new())?,
		Value::from(vec![Value::from(2)])
	);
	Ok(())
}

/// Replacing a lexical value changes later registrations, never prior captures.
#[test]
fn live_gql_alter_let_keeps_registered_captures() -> FerrosGrynnResult<()> {
	let store = Datastore::in_memory();
	let mut session = store.session();
	execute_value(
		&mut session,
		"DEFINE CLASS person TYPE NODE; DEFINE FIELD age ON CLASS person TYPE int;",
		&Object::new(),
	)?;
	let first = identity(execute_value(
		&mut session,
		"LET $min = 18; LIVE SELECT VALUE age FROM person WHERE age >= $min;",
		&Object::new(),
	)?)?;
	let stamp = store.begin().commit()?.versionstamp();
	assert_eq!(
		execute_value(&mut session, "ALTER LET $min VALUE 21; RETURN $min;", &Object::new())?,
		21.into()
	);
	assert_eq!(store.begin().commit()?.versionstamp(), stamp);
	let second = identity(execute_value(
		&mut session,
		"LIVE SELECT VALUE age FROM person WHERE age >= $min;",
		&Object::new(),
	)?)?;
	assert_eq!(
		store.begin().live_info(first)?.query().parameters().get("min"),
		Some(&Value::from(18))
	);
	assert_eq!(
		store.begin().live_info(second)?.query().parameters().get("min"),
		Some(&Value::from(21))
	);
	let mut old = session.take_live_subscription(first)?;
	let mut new = session.take_live_subscription(second)?;
	execute_value(&mut session, "CREATE person:a SET age = 19;", &Object::new())?;
	let LiveNext::Delivery(delivery) = old.next()? else {
		return Err(FerrosGrynnError::invalid_argument("frozen capture delivery missing"));
	};
	let LivePayload::Batch(batch) = delivery.payload() else {
		return Err(FerrosGrynnError::invalid_argument("frozen capture batch missing"));
	};
	assert_eq!(batch.events()[0].after(), Some(&Value::from(19)));
	assert!(matches!(new.next()?, LiveNext::Idle));
	Ok(())
}

/// Lexical updates share existing TRY/scopes and preserve immutable inputs.
#[test]
fn live_gql_alter_let_rollback_scopes_and_immutable_inputs() -> FerrosGrynnResult<()> {
	let store = Datastore::in_memory();
	let mut session = store.session();
	execute_value(&mut session, "LET $value = 10;", &Object::new())?;
	assert_eq!(
		execute_value(
			&mut session,
			"TRY { ALTER LET $value VALUE 20; THROW 'undo'; } CATCH |$error| { }; RETURN $value;",
			&Object::new()
		)?,
		10.into()
	);
	assert_eq!(
		execute_value(
			&mut session,
			"IF true { LET $value = 1; ALTER LET $value VALUE 2; RETURN $value; };",
			&Object::new()
		)?,
		2.into()
	);
	assert_eq!(execute_value(&mut session, "RETURN $value;", &Object::new())?, 10.into());
	assert_eq!(
		execute_value(
			&mut session,
			"FOR $v IN [1,2] { ALTER LET $value VALUE $value + $v; }; RETURN $value;",
			&Object::new()
		)?,
		13.into()
	);
	for (source, inputs) in [
		("ALTER LET $input VALUE 2;", Object::from_iter([("input", Value::from(1))])),
		("ALTER LET $missing VALUE 2;", Object::new()),
		("ALTER LET $value VALUE 'wrong type';", Object::new()),
	] {
		assert!(session.execute(source, &inputs)?.iter().any(|result| result.result.is_err()));
	}
	assert_eq!(execute_value(&mut session, "RETURN $value;", &Object::new())?, 13.into());
	execute_value(
		&mut session,
		"BEGIN READ ONLY; LET $private = 40; ALTER LET $private VALUE 41;",
		&Object::new(),
	)?;
	assert_eq!(execute_value(&mut session, "RETURN $private;", &Object::new())?, 41.into());
	execute_value(&mut session, "CANCEL;", &Object::new())?;
	assert!(session.execute("RETURN $private;", &Object::new())?[0].result.is_err());
	execute_value(
		&mut session,
		"DEFINE PURE FUNCTION fn::local() -> int { LET $x = 1; ALTER LET $x VALUE 2; RETURN $x; };",
		&Object::new(),
	)?;
	assert_eq!(execute_value(&mut session, "RETURN fn::local();", &Object::new())?, 2.into());
	Ok(())
}

/// Dynamic type checks precede mutation in both normal and PURE execution.
#[test]
fn live_gql_alter_let_rejects_dynamic_values_without_replacing_slot() -> FerrosGrynnResult<()> {
	let store = Datastore::in_memory();
	let mut session = store.session();
	let parameters = Object::from_iter([(
		"obj",
		Value::Object(Object::from_iter([("value", Value::from("wrong"))])),
	)]);
	for initializer in ["math::pow(2, 3)", "1"] {
		let text = format!(
			"IF true {{ LET $n = {initializer}; TRY {{ ALTER LET $n VALUE $obj.value; }} CATCH \
			 |$e| {{ RETURN $n; }}; THROW 'type check was skipped'; }};"
		);
		let expected = if initializer == "1" {
			Value::from(1)
		} else {
			Value::from(8.0)
		};
		assert_eq!(execute_value(&mut session, &text, &parameters)?, expected);
	}
	execute_value(
		&mut session,
		"DEFINE PURE FUNCTION fn::checked($obj: object) -> int { LET $n = 1; TRY { ALTER LET $n \
		 VALUE $obj.value; } CATCH |$e| {}; RETURN $n; };",
		&Object::new(),
	)?;
	assert_eq!(execute_value(&mut session, "RETURN fn::checked($obj);", &parameters)?, 1.into());
	Ok(())
}

/// An annotation survives successful calls, NONE values, shadows and CANCEL.
#[test]
fn live_gql_alter_let_retains_annotation_across_session_and_transaction_calls()
-> FerrosGrynnResult<()> {
	let store = Datastore::in_memory();
	let mut session = store.session();
	assert_eq!(
		execute_value(
			&mut session,
			"IF true { LET $x: option<int> = 1; ALTER LET $x VALUE NONE; RETURN $x; };",
			&Object::new()
		)?,
		Value::None
	);
	execute_value(&mut session, "LET $x: option<int> = 1;", &Object::new())?;
	assert_eq!(
		execute_value(&mut session, "ALTER LET $x VALUE NONE; RETURN $x;", &Object::new())?,
		Value::None
	);
	assert_eq!(
		execute_value(&mut session, "ALTER LET $x VALUE 2; RETURN $x;", &Object::new())?,
		2.into()
	);
	assert_eq!(
		execute_value(
			&mut session,
			"LET $y: option<int> = $x; ALTER LET $x VALUE $y; ALTER LET $x VALUE $x; RETURN $x;",
			&Object::new()
		)?,
		2.into()
	);
	execute_value(
		&mut session,
		"DEFINE PURE FUNCTION fn::optional() -> option<int> { LET $n: option<int> = 1; LET $copy: \
		 option<int> = $n; ALTER LET $n VALUE $copy; ALTER LET $n VALUE $n; ALTER LET $n VALUE \
		 NONE; RETURN $n; };",
		&Object::new(),
	)?;
	assert_eq!(execute_value(&mut session, "RETURN fn::optional();", &Object::new())?, Value::None);
	assert_eq!(
		execute_value(
			&mut session,
			"LET $target: option<number> = 0; LET $source: option<int> = 1; ALTER LET $target \
			 VALUE $source; RETURN $target;",
			&Object::new()
		)?,
		1.into()
	);
	assert_eq!(
		execute_value(
			&mut session,
			"ALTER LET $source VALUE NONE; ALTER LET $target VALUE $source; RETURN $target;",
			&Object::new()
		)?,
		Value::None
	);
	execute_value(
		&mut session,
		"BEGIN READ ONLY; LET $x = 'shadow'; ALTER LET $x VALUE 'local';",
		&Object::new(),
	)?;
	assert_eq!(execute_value(&mut session, "RETURN $x;", &Object::new())?, "local".into());
	execute_value(&mut session, "CANCEL; ALTER LET $x VALUE NONE;", &Object::new())?;
	assert_eq!(execute_value(&mut session, "RETURN $x;", &Object::new())?, Value::None);
	let mut tx = store.begin();
	for source in ["LET $x: option<int> = 1;", "ALTER LET $x VALUE NONE;", "ALTER LET $x VALUE 3;"]
	{
		for result in tx.execute(source, &Object::new())? {
			result.result?;
		}
	}
	let results = tx.execute("RETURN $x;", &Object::new())?;
	let mut value = Value::None;
	for result in results {
		value = result.result?.value().clone();
	}
	assert_eq!(value, Value::from(3));
	Ok(())
}

/// A dynamic container cannot bypass a declared member contract.
#[test]
fn live_gql_alter_let_checks_container_members() -> FerrosGrynnResult<()> {
	let store = Datastore::in_memory();
	let mut session = store.session();
	let parameters = Object::from_iter([(
		"obj",
		Value::Object(Object::from_iter([(
			"value",
			Value::from(vec![Value::from(2), Value::from("wrong")]),
		)])),
	)]);
	execute_value(&mut session, "LET $xs: array<int> = [1];", &Object::new())?;
	assert_eq!(
		execute_value(
			&mut session,
			"TRY { ALTER LET $xs VALUE $obj.value; } CATCH |$e| {}; RETURN $xs;",
			&parameters
		)?,
		Value::from(vec![Value::from(1)])
	);
	assert!(
		execute_value(&mut session, "LET $bad: array<int> = $obj.value;", &parameters).is_err()
	);
	assert!(execute_value(&mut session, "RETURN $bad;", &Object::new()).is_err());
	assert_eq!(
		execute_value(&mut session, "ALTER LET $xs VALUE [2,3]; RETURN $xs;", &Object::new())?,
		Value::from(vec![Value::from(2), Value::from(3)])
	);
	Ok(())
}

/// INFO uses the executing snapshot; close is transactional and repeatable.
#[test]
fn live_gql_info_and_kill_follow_commit_cancel_and_try() -> FerrosGrynnResult<()> {
	use crate::live::LiveState;
	let store = Datastore::in_memory();
	let mut session = store.session();
	execute_value(&mut session, "DEFINE CLASS person TYPE NODE;", &Object::new())?;
	let id = identity(execute_value(
		&mut session,
		"LET $sub = LIVE SELECT * FROM person; RETURN $sub;",
		&Object::new(),
	)?)?;
	let info = execute_value(&mut session, "INFO FOR LIVE $sub;", &Object::new())?;
	let Value::Object(row) = info else {
		return Err(FerrosGrynnError::invalid_argument("LIVE INFO fixture row missing"));
	};
	assert_eq!(row.get("id"), Some(&Value::from(id.as_uuid())));
	assert_eq!(row.get("state"), Some(&Value::from("active")));
	let metadata = store.begin().live_info(id)?;
	assert_eq!(row.get("starting_after"), Some(&Value::from(metadata.starting_after().get())));
	assert_eq!(row.get("history"), Some(&Value::None));
	assert_eq!(row.get("read_strategy"), Some(&Value::from("conservative_rescan")));
	let reactive = execute_value(
		&mut session,
		"LIVE LET $current = $sub; INFO FOR LIVE $current;",
		&Object::new(),
	)?;
	let Value::Object(reactive) = reactive else {
		return Err(FerrosGrynnError::invalid_argument("reactive INFO identity row"));
	};
	assert_eq!(reactive.get("id"), Some(&Value::from(id.as_uuid())));
	assert_eq!(
		row.get("cursor"),
		Some(&Value::from(store.begin().live_info(id)?.cursor().encode()))
	);
	let mut handle = session.take_live_subscription(id)?;
	execute_value(&mut session, "BEGIN; KILL $sub; CANCEL;", &Object::new())?;
	assert_eq!(store.begin().live_info(id)?.state(), LiveState::Active);
	assert!(matches!(handle.next()?, LiveNext::Idle));
	execute_value(
		&mut session,
		"TRY { KILL $sub; THROW 'undo'; } CATCH |$error| { };",
		&Object::new(),
	)?;
	assert_eq!(store.begin().live_info(id)?.state(), LiveState::Active);
	let readonly = session.execute("BEGIN READ ONLY; KILL $sub;", &Object::new())?;
	assert!(readonly.iter().any(|result| result.result.is_err()));
	assert_eq!(store.begin().live_info(id)?.state(), LiveState::Active);
	execute_value(&mut session, "BEGIN; KILL $sub; COMMIT; KILL $sub;", &Object::new())?;
	assert_eq!(store.begin().live_info(id)?.state(), LiveState::Closed);
	Ok(())
}

/// A private close suppresses attachment; registration identity remains owned.
#[test]
fn live_gql_pending_kill_never_exposes_a_consumer() -> FerrosGrynnResult<()> {
	let store = Datastore::in_memory();
	let mut session = store.session();
	execute_value(&mut session, "DEFINE CLASS person TYPE NODE;", &Object::new())?;
	let id = identity(execute_value(
		&mut session,
		"BEGIN; LET $sub = LIVE SELECT * FROM person; KILL $sub; RETURN $sub;",
		&Object::new(),
	)?)?;
	execute_value(&mut session, "COMMIT;", &Object::new())?;
	assert_eq!(store.begin().live_info(id)?.state(), crate::live::LiveState::Closed);
	assert!(session.take_live_subscription(id).is_err());
	Ok(())
}

/// Routing must refresh ReadCommitted before distinguishing LIVE from jobs.
#[test]
fn live_gql_kill_refreshes_read_committed_and_info_keeps_old_snapshot() -> FerrosGrynnResult<()> {
	use crate::live::{LiveOptions, LiveQuery, LiveSource, LiveState};
	let store = Datastore::in_memory();
	let mut setup = store.begin();
	let class = setup.define_class(ClassSchema::new("person", AtomKind::Node, [])?)?;
	setup.commit()?;
	let mut latest = store.session();
	execute_value(&mut latest, "BEGIN ISOLATION LEVEL READ COMMITTED;", &Object::new())?;
	let handle =
		store.subscribe_live(LiveQuery::new(LiveSource::Class(class)), LiveOptions::default())?;
	let id = handle.id();
	let bindings = Object::from_iter([("sub", Value::from(id.as_uuid()))]);
	let mut old = store.session();
	execute_value(&mut old, "BEGIN READ ONLY;", &Object::new())?;
	execute_value(&mut latest, "KILL $sub; COMMIT;", &bindings)?;
	assert_eq!(store.begin().live_info(id)?.state(), LiveState::Closed);
	let Value::Object(row) = execute_value(&mut old, "INFO FOR LIVE $sub;", &bindings)? else {
		return Err(FerrosGrynnError::invalid_argument("snapshot LIVE INFO fixture missing"));
	};
	assert_eq!(row.get("state"), Some(&Value::from("active")));
	execute_value(&mut old, "CANCEL;", &Object::new())?;
	Ok(())
}

/// Tiny caller caps reject before the metadata snapshot is copied.
#[test]
fn live_gql_info_rejects_small_cap_without_changing_registry() -> FerrosGrynnResult<()> {
	let store = Datastore::in_memory();
	let mut session = store.session();
	execute_value(&mut session, "DEFINE CLASS person TYPE NODE;", &Object::new())?;
	let id = identity(execute_value(&mut session, "LIVE SELECT * FROM person;", &Object::new())?)?;
	let info = store.begin().live_info(id)?;
	let result = session.execute_with_options(
		"INFO FOR LIVE $sub;",
		&Object::from_iter([("sub", Value::from(id.as_uuid()))]),
		crate::gql::QueryExecutionOptions {
			barrier_memory_bytes: std::num::NonZeroUsize::new(1)
				.ok_or_else(|| FerrosGrynnError::invalid_argument("fixture memory cap missing"))?,
			..crate::gql::QueryExecutionOptions::default()
		},
	)?;
	assert!(result[0].result.is_err());
	assert_eq!(store.begin().live_info(id)?.cursor(), info.cursor());
	assert_eq!(store.begin().live_info(id)?.state(), info.state());
	Ok(())
}

/// Control publication may advance the clock while an importer owns data
/// writes.
#[test]
fn live_gql_kill_during_import_preserves_leased_items() -> FerrosGrynnResult<()> {
	use crate::{datastore::ImportOptions, live::LiveState};
	let store = Datastore::in_memory();
	let mut session = store.session();
	execute_value(&mut session, "DEFINE CLASS person TYPE NODE;", &Object::new())?;
	let id = identity(execute_value(
		&mut session,
		"LET $sub = LIVE SELECT * FROM person; RETURN $sub;",
		&Object::new(),
	)?)?;
	let report = store.import(
		ImportOptions {
			segment_size: 1,
			..ImportOptions::default()
		},
		|importer| {
			importer.apply_item(|tx| tx.create_atom("person:a".parse()?, PropertyMap::new()))?;
			execute_value(&mut session, "BEGIN; KILL $sub; CANCEL;", &Object::new())?;
			execute_value(
				&mut session,
				"TRY { KILL $sub; THROW 'undo'; } CATCH |$error| { };",
				&Object::new(),
			)?;
			assert_eq!(store.begin().live_info(id)?.state(), LiveState::Active);
			execute_value(&mut session, "KILL $sub;", &Object::new())?;
			assert_eq!(store.begin().live_info(id)?.state(), LiveState::Closed);
			importer.apply_item(|tx| tx.create_atom("person:b".parse()?, PropertyMap::new()))?;
			Ok(())
		},
	)?;
	assert_eq!(report.applied_items(), 2);
	assert_eq!(store.begin().live_info(id)?.state(), LiveState::Closed);
	let Value::Array(rows) = execute_value(&mut session, "SELECT * FROM person;", &Object::new())?
	else {
		return Err(FerrosGrynnError::invalid_argument("import fixture rows missing"));
	};
	assert_eq!(rows.len(), 2);
	Ok(())
}

/// Option calls execute the checked plans after temporary compiler teardown.
#[test]
fn live_gql_pure_options_survive_private_compiler_scope() -> FerrosGrynnResult<()> {
	let store = Datastore::in_memory();
	let mut session = store.session();
	execute_value(
		&mut session,
		"DEFINE CLASS person TYPE NODE; DEFINE FIELD age ON CLASS person TYPE int; DEFINE PURE \
		 FUNCTION fn::capacity() -> int { RETURN 7; };",
		&Object::new(),
	)?;
	let id = identity(execute_value(
		&mut session,
		"LIVE SELECT VALUE age FROM person BUFFER fn::capacity();",
		&Object::new(),
	)?)?;
	let mut handle = session.take_live_subscription(id)?;
	assert!(matches!(handle.next()?, LiveNext::Idle));
	assert_eq!(execute_value(&mut session, "RETURN fn::capacity();", &Object::new())?, 7.into());
	execute_value(&mut session, "CREATE person:a SET age = 19;", &Object::new())?;
	let LiveNext::Delivery(delivery) = handle.next()? else {
		return Err(FerrosGrynnError::invalid_argument("PURE options LIVE delivery missing"));
	};
	assert!(
		matches!(delivery.payload(), LivePayload::Batch(batch) if batch.events()[0].after() == Some(&19.into()))
	);
	Ok(())
}

/// Execute parameters are captured once; extracting a handle releases its
/// dependency on the session, with no initial data delivery.
#[test]
fn live_gql_frozen_parameters_and_owned_session_handle() -> FerrosGrynnResult<()> {
	let store = Datastore::in_memory();
	let mut setup = store.begin();
	setup.define_class(ClassSchema::new("person", AtomKind::Node, [ClassField::new(
		"age",
		Type::Int,
	)])?)?;
	let age = setup.intern_property("age")?;
	let record: RecordId = "person:a".parse()?;
	setup.create_atom(record.clone(), PropertyMap::from_iter([(age, 17.into())]))?;
	setup.commit()?;
	let mut session = store.session();
	let id = identity(execute_value(
		&mut session,
		"LET $sub = LIVE SELECT VALUE age FROM person WHERE age >= $minimum ON OVERFLOW GAP \
		 BUFFER 7; RETURN $sub;",
		&Object::from_iter([("minimum", Value::from(18))]),
	)?)?;
	let mut handle = session.take_live_subscription(id)?;
	assert!(matches!(handle.next()?, LiveNext::Idle));
	assert!(session.take_live_subscription(id).is_err());
	execute_value(
		&mut session,
		"RETURN $minimum;",
		&Object::from_iter([("minimum", Value::from(21))]),
	)?;
	drop(session);
	let mut write = store.begin();
	write.set_base_property(&record, age, 19.into())?;
	let stamp = write.commit()?.versionstamp();
	let LiveNext::Delivery(delivery) = handle.next()? else {
		return Err(FerrosGrynnError::invalid_argument("GQL LIVE delivery missing"));
	};
	let LivePayload::Batch(batch) = delivery.payload() else {
		return Err(FerrosGrynnError::invalid_argument("unexpected GQL LIVE gap"));
	};
	assert_eq!(batch.versionstamp(), stamp);
	assert_eq!(batch.events().len(), 1);
	assert_eq!(batch.events()[0].identity(), &LiveIdentity::Record(record));
	assert_eq!(batch.events()[0].before(), None);
	assert_eq!(batch.events()[0].after(), Some(&19.into()));
	handle.ack(delivery.token())?;
	assert!(matches!(handle.next()?, LiveNext::Idle));
	Ok(())
}

/// Analysis of an enclosing block precedes LET execution; capture must use
/// the newly initialized lexical slot instead of analysis-time parameters.
#[test]
fn live_gql_block_let_capture_uses_registration_time_slot() -> FerrosGrynnResult<()> {
	let store = Datastore::in_memory();
	let mut setup = store.begin();
	setup.define_class(ClassSchema::new("person", AtomKind::Node, [ClassField::new(
		"age",
		Type::Int,
	)])?)?;
	let age = setup.intern_property("age")?;
	setup.commit()?;
	let mut session = store.session();
	let id = identity(execute_value(
		&mut session,
		"IF true { LET $threshold = 18; LET $sub = LIVE SELECT VALUE age FROM person WHERE age >= \
		 $threshold; RETURN $sub; };",
		&Object::new(),
	)?)?;
	let mut handle = session.take_live_subscription(id)?;
	let mut write = store.begin();
	write.create_atom("person:a".parse()?, PropertyMap::from_iter([(age, 19.into())]))?;
	write.commit()?;
	let LiveNext::Delivery(delivery) = handle.next()? else {
		return Err(FerrosGrynnError::invalid_argument("block LIVE missing"));
	};
	assert!(
		matches!(delivery.payload(), LivePayload::Batch(batch) if batch.events()[0].after() == Some(&19.into()))
	);
	Ok(())
}

/// CANCEL and TRY restore logical registration and its session journal;
/// explicit COMMIT exposes exactly the surviving registration.
#[test]
fn live_gql_cancel_try_and_explicit_commit_activation() -> FerrosGrynnResult<()> {
	let store = Datastore::in_memory();
	let mut setup = store.begin();
	setup.define_class(ClassSchema::new("person", AtomKind::Node, [])?)?;
	setup.commit()?;
	let mut session = store.session();
	let id = identity(execute_value(
		&mut session,
		"BEGIN; LET $sub = LIVE SELECT * FROM person; RETURN $sub;",
		&Object::new(),
	)?)?;
	assert!(session.take_live_subscription(id).is_err());
	execute_value(&mut session, "CANCEL;", &Object::new())?;
	assert!(store.begin().live_info(id).is_err());
	let count = store.inner.current_root().live_catalog().queries.len();
	execute_value(
		&mut session,
		"TRY { LET $sub = LIVE SELECT * FROM person; THROW 'rollback'; } CATCH |$error| { RETURN \
		 NONE; };",
		&Object::new(),
	)?;
	assert_eq!(store.inner.current_root().live_catalog().queries.len(), count);
	let id = identity(execute_value(
		&mut session,
		"BEGIN; LET $sub = LIVE SELECT * FROM person; RETURN $sub;",
		&Object::new(),
	)?)?;
	execute_value(&mut session, "COMMIT;", &Object::new())?;
	let mut handle = session.take_live_subscription(id)?;
	assert!(matches!(handle.next()?, LiveNext::Idle));
	let mut write = store.begin();
	write.create_atom("person:a".parse()?, PropertyMap::new())?;
	write.commit()?;
	assert!(matches!(handle.next()?, LiveNext::Delivery(_)));
	Ok(())
}

/// GQL SINCE restores the same independent aggregate result identity and
/// exact saved values as programmatic registration.
#[test]
fn live_gql_since_count_replays_whole_commits() -> FerrosGrynnResult<()> {
	let store = Datastore::in_memory();
	let mut setup = store.begin();
	let class = setup.define_class(ClassSchema::new("person", AtomKind::Node, [])?)?;
	setup.configure_changefeed(
		ChangefeedSource::Class(class),
		ChangefeedConfig::new("P1D".parse()?)?,
	)?;
	let floor = setup.commit()?.versionstamp();
	let mut stamps = Vec::new();
	for name in ["person:a", "person:b"] {
		let mut write = store.begin();
		write.create_atom(name.parse()?, PropertyMap::new())?;
		stamps.push(write.commit()?.versionstamp());
	}
	let mut session = store.session();
	let id = identity(execute_value(
		&mut session,
		"LET $sub = LIVE SELECT VALUE count() FROM person SINCE $since; RETURN $sub;",
		&Object::from_iter([("since", Value::from(floor.get()))]),
	)?)?;
	let mut handle = session.take_live_subscription(id)?;
	for (index, stamp) in stamps.into_iter().enumerate() {
		let LiveNext::Delivery(delivery) = handle.next()? else {
			return Err(FerrosGrynnError::invalid_argument("GQL aggregate history missing"));
		};
		let LivePayload::Batch(batch) = delivery.payload() else {
			return Err(FerrosGrynnError::invalid_argument("GQL aggregate gap"));
		};
		assert_eq!(batch.versionstamp(), stamp);
		assert!(
			matches!(batch.events()[0].identity(), LiveIdentity::Aggregate(keys) if keys.is_empty())
		);
		assert_eq!(batch.events()[0].before(), Some(&Value::from(index as i64)));
		assert_eq!(batch.events()[0].after(), Some(&Value::from(index as i64 + 1)));
		handle.ack(delivery.token())?;
	}
	assert!(matches!(handle.next()?, LiveNext::Idle));
	Ok(())
}

/// Unsupported syntax and invalid options cannot publish a partial descriptor
/// or leave an attached consumer.
#[test]
fn live_gql_rejects_unrepresented_clauses_and_zero_buffer() -> FerrosGrynnResult<()> {
	let store = Datastore::in_memory();
	let mut setup = store.begin();
	setup.define_class(ClassSchema::new("person", AtomKind::Node, [])?)?;
	setup.commit()?;
	let mut session = store.session();
	for text in [
		"LIVE SELECT * FROM person ORDER BY RAND();",
		"LIVE SELECT * FROM person START AFTER $cursor;",
		"LIVE SELECT * FROM person BUFFER 0;",
		"LIVE SELECT * FROM person WITH NOINDEX;",
	] {
		let before = store.inner.current_root().commit_ts;
		assert!(execute_value(&mut session, text, &Object::new()).is_err(), "{text}");
		assert_eq!(store.inner.current_root().commit_ts, before);
		assert!(store.inner.current_root().live_catalog().queries.is_empty());
	}
	Ok(())
}

/// Alias qualification follows ordinary case-insensitive schema resolution;
/// a bare alias spelling can still name a real source property.
#[test]
fn live_gql_alias_qualification_matches_select() -> FerrosGrynnResult<()> {
	let store = Datastore::in_memory();
	let mut setup = store.begin();
	setup.define_class(ClassSchema::new("person", AtomKind::Node, [
		ClassField::new("age", Type::Int),
		ClassField::new("p", Type::Int),
	])?)?;
	let age = setup.intern_property("age")?;
	let p = setup.intern_property("p")?;
	setup.create_atom(
		"person:a".parse()?,
		PropertyMap::from_iter([(age, 17.into()), (p, 3.into())]),
	)?;
	setup.commit()?;
	for (expression, expected) in [("P.age", Value::from(19)), ("p", Value::from(4))] {
		let mut session = store.session();
		let id = identity(execute_value(
			&mut session,
			&format!("LIVE SELECT VALUE {expression} FROM person AS p;"),
			&Object::new(),
		)?)?;
		let info = store.begin().live_info(id)?;
		assert_eq!(info.query().alias().map(crate::values::Text::as_str), Some("p"));
		if expression == "P.age" {
			assert!(matches!(info.query().projection(), LiveProjection::Value(
				crate::live::LiveExpr::Field(path)
			) if path.segments().iter().map(crate::values::Text::as_str)
				.eq(["P", "age"])));
		}
		let mut handle = session.take_live_subscription(id)?;
		let mut write = store.begin();
		write.set_base_property(&"person:a".parse()?, age, 19.into())?;
		write.set_base_property(&"person:a".parse()?, p, 4.into())?;
		write.commit()?;
		let LiveNext::Delivery(delivery) = handle.next()? else {
			return Err(FerrosGrynnError::invalid_argument("qualified LIVE delivery missing"));
		};
		assert!(
			matches!(delivery.payload(), LivePayload::Batch(batch) if batch.events()[0].after() == Some(&expected))
		);
		let mut restore = store.begin();
		restore.set_base_property(&"person:a".parse()?, age, 17.into())?;
		restore.set_base_property(&"person:a".parse()?, p, 3.into())?;
		restore.commit()?;
	}
	Ok(())
}

/// DIFF/PATCH select presentation even when properties share their spellings.
#[test]
fn live_gql_diff_patch_are_output_modes() -> FerrosGrynnResult<()> {
	let store = Datastore::in_memory();
	let mut setup = store.begin();
	setup.define_class(ClassSchema::new("person", AtomKind::Node, [
		ClassField::new("DIFF", Type::Int),
		ClassField::new("PATCH", Type::Int),
	])?)?;
	setup.commit()?;
	let mut session = store.session();
	for (mode, expected) in [("DIFF", LiveOutputMode::Diff), ("PATCH", LiveOutputMode::Patch)] {
		let id = identity(execute_value(
			&mut session,
			&format!("LIVE SELECT {mode} FROM person;"),
			&Object::new(),
		)?)?;
		let info = store.begin().live_info(id)?;
		assert_eq!(info.query().output(), expected);
		assert!(matches!(info.query().projection(), LiveProjection::Records));
	}
	Ok(())
}

/// A property matching the class name remains a row-relative object path
/// when ordinary SELECT uses another source alias.
#[test]
fn live_gql_nested_class_name_property_matches_select() -> FerrosGrynnResult<()> {
	let store = Datastore::in_memory();
	let mut setup = store.begin();
	setup.define_class(ClassSchema::new("person", AtomKind::Node, [
		ClassField::new("person", Type::Object),
		ClassField::new("age", Type::Int),
	])?)?;
	let object = setup.intern_property("person")?;
	let age = setup.intern_property("age")?;
	let record: RecordId = "person:a".parse()?;
	setup.create_atom(
		record.clone(),
		PropertyMap::from_iter([
			(object, Value::Object(Object::from_iter([("age", 1)]))),
			(age, 17.into()),
		]),
	)?;
	setup.commit()?;
	let mut session = store.session();
	let id = identity(execute_value(
		&mut session,
		"LIVE SELECT VALUE person.age FROM person AS q;",
		&Object::new(),
	)?)?;
	let mut handle = session.take_live_subscription(id)?;
	let mut write = store.begin();
	write.set_base_property(&record, age, 19.into())?;
	write.set_base_property(&record, object, Value::Object(Object::from_iter([("age", 2)])))?;
	write.commit()?;
	let oracle =
		execute_value(&mut session, "SELECT VALUE person.age FROM person AS q;", &Object::new())?;
	assert_eq!(oracle, Value::from(vec![Value::from(2)]));
	let LiveNext::Delivery(delivery) = handle.next()? else {
		return Err(FerrosGrynnError::invalid_argument("nested LIVE delivery missing"));
	};
	assert!(
		matches!(delivery.payload(), LivePayload::Batch(batch) if batch.events()[0].before() == Some(&Value::from(1)) && batch.events()[0].after() == Some(&Value::from(2)))
	);
	Ok(())
}

/// LIVE planning describes real phases without registration, evaluation or
/// queue attachment.
#[test]
fn live_explain_hash_phases_and_single_analyze() -> FerrosGrynnResult<()> {
	fn explain(session: &mut GqlSession, text: &str, inputs: &Object) -> FerrosGrynnResult<Object> {
		let Value::Object(row) = execute_value(session, text, inputs)? else {
			return Err(FerrosGrynnError::invalid_argument("LIVE explain object absent"));
		};
		Ok(row)
	}
	fn hash(row: &Object) -> FerrosGrynnResult<&Value> {
		row.get("plan_hash").ok_or_else(|| FerrosGrynnError::invalid_argument("LIVE hash absent"))
	}
	fn operators(row: &Object) -> FerrosGrynnResult<Vec<&str>> {
		let Some(Value::Array(nodes)) = row.get("nodes") else {
			return Err(FerrosGrynnError::invalid_argument("LIVE explain nodes absent"));
		};
		nodes
			.iter()
			.map(|node| {
				node.as_object()
					.and_then(|row| row.get("operator"))
					.and_then(Value::as_str)
					.ok_or_else(|| FerrosGrynnError::invalid_argument("LIVE phase absent"))
			})
			.collect()
	}
	let store = Datastore::in_memory();
	let mut session = store.session();
	execute_value(
		&mut session,
		"DEFINE CLASS person TYPE NODE SCHEMALESS; CREATE person:a SET age = 7; BEGIN;",
		&Object::new(),
	)?;
	let text = "EXPLAIN FULL FORMAT JSON LIVE SELECT PATCH VALUE properties.age FROM person WHERE \
	            properties.age >= $minimum ON OVERFLOW GAP BUFFER $buffer + 1;";
	let inputs = Object::from_iter([
		("minimum", Value::from(1)),
		("buffer", Value::from(10)),
		("unrelated", Value::from("first")),
	]);
	let first = explain(&mut session, text, &inputs)?;
	assert_eq!(operators(&first)?, [
		"LiveCaptureInputs",
		"LiveValidateRecipe",
		"LiveCatalogMutation",
		"Publication"
	]);
	assert_eq!(hash(&first)?, hash(&explain(&mut session, text, &inputs)?)?);
	for (name, value) in [("minimum", Value::from(2)), ("buffer", Value::from(11))] {
		let mut changed = inputs.clone();
		changed.insert(name, value);
		assert_ne!(hash(&first)?, hash(&explain(&mut session, text, &changed)?)?, "{name}");
	}
	let mut unrelated = inputs.clone();
	unrelated.insert("unrelated", Value::from("second"));
	assert_eq!(hash(&first)?, hash(&explain(&mut session, text, &unrelated)?)?);
	let split = text.replace("PATCH", "DIFF");
	assert_ne!(hash(&first)?, hash(&explain(&mut session, &split, &inputs)?)?);
	let mut changed = inputs.clone();
	changed.insert("buffer", Value::from(12));
	let closure = text.replace("$buffer + 1", "(|$local: int| $local + $buffer)(1)");
	let closure_first = explain(&mut session, &closure, &inputs)?;
	assert_ne!(hash(&closure_first)?, hash(&explain(&mut session, &closure, &changed)?)?);
	let shadow = text.replace("$buffer + 1", "(|$buffer: int| $buffer + 1)(1)");
	assert_eq!(
		hash(&explain(&mut session, &shadow, &inputs)?)?,
		hash(&explain(&mut session, &shadow, &changed)?)?
	);
	let wrapper = explain(
		&mut session,
		"EXPLAIN FULL FORMAT JSON LET $preview = LIVE SELECT VALUE properties.age FROM person;",
		&Object::new(),
	)?;
	assert_eq!(operators(&wrapper)?[0], "LiveCaptureInputs");
	let unevaluated = Object::from_iter([("zero", Value::from(0))]);
	explain(
		&mut session,
		"EXPLAIN FULL FORMAT JSON LIVE SELECT VALUE properties.age / $zero FROM person;",
		&unevaluated,
	)?;
	assert_eq!(store.live_statistics()?.registered_queries, 0);
	assert_eq!(store.live_statistics()?.query_evaluation_attempts, 0);
	execute_value(&mut session, "CANCEL;", &Object::new())?;
	let nested = text.replace("$buffer + 1", "(SELECT VALUE $buffer + 1 FROM person)");
	let rejected = session.execute(&nested, &inputs)?;
	assert!(rejected.iter().any(|result| result.result.is_err()));
	let analyzed = explain(
		&mut session,
		"EXPLAIN ANALYZE FORMAT JSON LIVE SELECT VALUE properties.age FROM person;",
		&Object::new(),
	)?;
	assert_eq!(operators(&analyzed)?[0], "LiveCaptureInputs");
	assert_eq!(store.live_statistics()?.registered_queries, 1);
	assert_eq!(store.live_statistics()?.query_evaluation_attempts, 1);
	Ok(())
}

/// Metadata plans retain their statement root and exclude current
/// binding/result state.
#[test]
fn live_explain_fixed_root_metadata_and_admission() -> FerrosGrynnResult<()> {
	let store = Datastore::in_memory();
	let mut session = store.session();
	execute_value(
		&mut session,
		"DEFINE CLASS person TYPE NODE SCHEMALESS; CREATE person:a SET age = 1; LIVE LET $limit = \
		 1; LET $sub = LIVE SELECT VALUE properties.age FROM person WHERE properties.age >= \
		 $limit; BEGIN;",
		&Object::new(),
	)?;
	let text = "EXPLAIN FULL FORMAT JSON INFO FOR LIVE $sub;";
	let Value::Object(first) = execute_value(&mut session, text, &Object::new())? else {
		return Err(FerrosGrynnError::invalid_argument("LIVE metadata plan absent"));
	};
	let mut writer = store.begin();
	let age = writer.intern_property("age")?;
	writer.set_base_property(&"person:a".parse()?, age, 2.into())?;
	writer.commit()?;
	let Value::Object(second) = execute_value(&mut session, text, &Object::new())? else {
		return Err(FerrosGrynnError::invalid_argument("LIVE repeated plan absent"));
	};
	assert_eq!(first.get("plan_hash"), second.get("plan_hash"));
	let Some(Value::Array(nodes)) = second.get("nodes") else {
		return Err(FerrosGrynnError::invalid_argument("LIVE metadata phases absent"));
	};
	assert_eq!(nodes.len(), 1);
	assert_eq!(
		nodes.iter().next().and_then(Value::as_object).and_then(|row| row.get("operator")),
		Some(&Value::from("LiveSnapshotMetadataRead"))
	);
	let binding = execute_value(
		&mut session,
		"EXPLAIN FULL FORMAT JSON INFO FOR LIVE LET $limit;",
		&Object::new(),
	)?;
	assert!(matches!(binding, Value::Object(_)));
	execute_value(&mut session, "CANCEL;", &Object::new())?;
	let inputs = Object::from_iter([("huge", Value::from("x".repeat(32 * 1024)))]);
	let options = crate::gql::QueryExecutionOptions {
		max_loop_iterations: 128,
		..crate::gql::QueryExecutionOptions::default()
	};
	let results = session.execute_with_options(
		"EXPLAIN FULL FORMAT JSON LIVE SELECT VALUE $huge FROM person;",
		&inputs,
		options,
	)?;
	assert!(results.iter().any(|result| result.result.is_err()));
	assert_eq!(store.live_statistics()?.registered_queries, 1);
	Ok(())
}

/// Canonical operand inputs retain lexical name/value correspondence across
/// frame layouts.
#[test]
fn live_explain_operand_names_ignore_declaration_order() -> FerrosGrynnResult<()> {
	let store = Datastore::in_memory();
	let mut setup = store.begin();
	setup.define_class(ClassSchema::new("person", AtomKind::Node, [])?)?;
	setup.commit()?;
	let explain = "EXPLAIN FULL FORMAT JSON LIVE SELECT * FROM person BUFFER $a + $b * 10;";
	let mut hashes = Vec::new();
	for declarations in
		["LET $a = 2; LET $b = 6;", "LET $b = 2; LET $a = 6;", "LET $b = 6; LET $a = 2;"]
	{
		let mut session = store.session();
		execute_value(&mut session, &format!("BEGIN; {declarations}"), &Object::new())?;
		let Value::Object(row) = execute_value(&mut session, explain, &Object::new())? else {
			return Err(FerrosGrynnError::invalid_argument("LIVE operand plan absent"));
		};
		hashes.push(
			row.get("plan_hash")
				.cloned()
				.ok_or_else(|| FerrosGrynnError::invalid_argument("LIVE operand hash absent"))?,
		);
		execute_value(&mut session, "CANCEL;", &Object::new())?;
	}
	assert_ne!(hashes[0], hashes[1]);
	assert_eq!(hashes[0], hashes[2]);
	Ok(())
}
