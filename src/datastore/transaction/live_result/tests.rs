//! Independent final-state result checks through public mutation/builder APIs.

use super::*;
/// Runtime-only diagnostic source whose ownership can be observed without
/// measuring allocator noise.
#[derive(Debug)]
struct TechnicalOwner(Arc<()>);

impl std::fmt::Display for TechnicalOwner {
	fn fmt(&self, output: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		write!(output, "technical owner {}", Arc::strong_count(&self.0))
	}
}

impl std::error::Error for TechnicalOwner {}

#[test]
fn live_fault_retains_only_bounded_declared_diagnostic() -> FerrosGrynnResult<()> {
	let owner = Arc::new(());
	let observed = Arc::downgrade(&owner);
	let original = FerrosGrynnError::invalid_argument("original declared cause")
		.with_technical_source(TechnicalOwner(owner));
	let id = crate::live::LiveId::from_uuid(crate::values::Uuid::now_v7())?;
	let (diagnostic, payload, bytes) =
		prepare_fault(id, crate::live::LiveVersion::default(), 2, 0, original)?;
	assert!(
		observed.upgrade().is_none(),
		"runtime technical source must not be retained by catalog or queue"
	);
	assert!(diagnostic.chain().all(|error| error.technical_source().is_none()));
	let LivePayload::Fault {
		diagnostic: delivered,
		..
	} = payload
	else {
		return Err(FerrosGrynnError::invalid_argument("missing declared fault"));
	};
	assert_eq!(delivered.status().code(), diagnostic.status().code());
	assert!(delivered.cause().is_some(), "declared status chain remains exact");
	assert!(matches!(
		crate::storage::live_history::decode(&bytes, 64 * 1024)?,
		LivePayload::Fault { .. }
	));
	Ok(())
}
use crate::{
	datastore::{ClassField, ClassSchema, Datastore},
	graph::AtomKind,
	live::{LiveBinaryOperator, LiveExpr, LiveOptions, LiveProjection, LiveQuery, LiveSource},
	values::{Object, PropertyMap, RecordId, Type},
};

/// Query output mode prepares native changes once and restores saved images.
#[test]
fn live_output_modes_prepare_and_resume_without_current_reads() -> FerrosGrynnResult<()> {
	use crate::{
		changefeed::{ChangefeedConfig, ChangefeedSource},
		live::{LiveEventOutput, LiveNext, LiveOutputMode, LivePatchKind},
	};
	let store = Datastore::in_memory();
	let mut setup = store.begin();
	let class =
		setup.define_class(ClassSchema::new("item", AtomKind::Node, [ClassField::new(
			"age",
			Type::Int,
		)])?)?;
	let age = setup.intern_property("age")?;
	let record: RecordId = "item:a".parse()?;
	setup.create_atom(record.clone(), PropertyMap::from_iter([(age, Value::from(18))]))?;
	setup.configure_changefeed(
		ChangefeedSource::Class(class),
		ChangefeedConfig::new("P1D".parse()?)?,
	)?;
	setup.commit()?;
	let mut subscriptions = Vec::new();
	for mode in [LiveOutputMode::Diff, LiveOutputMode::Patch] {
		subscriptions.push(store.subscribe_live(
			LiveQuery::new(LiveSource::Class(class)).with_output(mode),
			LiveOptions::default(),
		)?);
	}
	let mut update = store.begin();
	update.set_base_property(&record, age, Value::from(21))?;
	update.commit()?;
	for (index, mut handle) in subscriptions.into_iter().enumerate() {
		let cursor = handle.cursor().clone();
		let LiveNext::Delivery(saved) = handle.next()? else {
			return Err(FerrosGrynnError::invalid_argument("formatted delivery missing"));
		};
		let LivePayload::Batch(batch) = saved.payload() else {
			return Err(FerrosGrynnError::invalid_argument("formatted batch missing"));
		};
		assert_eq!(batch.events().len(), 1);
		let event = &batch.events()[0];
		assert!(
			matches!(event.before(), Some(Value::Object(values)) if values.get("age") == Some(&Value::from(18)))
		);
		assert!(
			matches!(event.after(), Some(Value::Object(values)) if values.get("age") == Some(&Value::from(21)))
		);
		match event.output() {
			Some(LiveEventOutput::Diff(changes)) => {
				assert_eq!(changes.len(), 1);
				assert_eq!(changes[0].path(), &[crate::values::Text::from("age")]);
				assert_eq!(changes[0].before(), Some(&Value::from(18)));
				assert_eq!(changes[0].after(), Some(&Value::from(21)));
			}
			Some(LiveEventOutput::Patch(operations)) => {
				assert_eq!(operations.len(), 1);
				assert_eq!(operations[0].path(), "/age");
				assert_eq!(operations[0].kind(), LivePatchKind::Replace);
				assert_eq!(operations[0].value(), Some(&Value::from(21)));
			}
			_ => return Err(FerrosGrynnError::invalid_argument("query mode was not prepared")),
		}
		let LiveNext::Delivery(repeated) = handle.next()? else {
			return Err(FerrosGrynnError::invalid_argument("repeated delivery missing"));
		};
		assert_eq!(repeated.token(), saved.token());
		drop(handle);
		if index == 0 {
			let mut remove = store.begin();
			remove.delete_atom(&record)?;
			remove.commit()?;
		}
		let mut resumed = store.resume_live(&cursor)?;
		let LiveNext::Delivery(replayed) = resumed.next()? else {
			return Err(FerrosGrynnError::invalid_argument("formatted replay missing"));
		};
		let (LivePayload::Batch(replayed), LivePayload::Batch(saved)) =
			(replayed.payload(), saved.payload())
		else {
			return Err(FerrosGrynnError::invalid_argument("formatted replay is not a batch"));
		};
		assert_eq!(replayed, saved);
		resumed.close()?;
	}
	Ok(())
}

/// GQL output modifiers preserve explicit projection and wildcard shorthand.
#[test]
fn live_output_modes_gql_fields_value_and_shorthand() -> FerrosGrynnResult<()> {
	use crate::live::{LiveEventOutput, LiveId, LiveNext};
	let store = Datastore::in_memory();
	let mut setup = store.begin();
	setup.define_class(ClassSchema::new("item", AtomKind::Node, [ClassField::new(
		"age",
		Type::Int,
	)])?)?;
	let age = setup.intern_property("age")?;
	let record: RecordId = "item:a".parse()?;
	setup.create_atom(record.clone(), PropertyMap::from_iter([(age, Value::from(18))]))?;
	setup.commit()?;
	for ((sql, scalar), value) in [
		("LIVE SELECT PATCH age FROM item;", false),
		("LIVE SELECT DIFF VALUE age FROM item;", true),
		("LIVE SELECT PATCH FROM item;", false),
		("LIVE SELECT PATCH ONLY FROM item;", false),
	]
	.into_iter()
	.zip(19_i64..)
	{
		let result = store
			.execute(sql, &Object::new())?
			.into_iter()
			.next()
			.ok_or_else(|| FerrosGrynnError::invalid_argument("GQL output registration missing"))?
			.result?;
		let Value::Uuid(uuid) = result.value() else {
			return Err(FerrosGrynnError::invalid_argument("GQL LIVE id missing"));
		};
		let id = LiveId::from_uuid(*uuid)?;
		let cursor = store.begin().live_info(id)?.cursor().clone();
		let mut handle = store.resume_live(&cursor)?;
		let mut update = store.begin();
		update.set_base_property(&record, age, Value::from(value))?;
		update.commit()?;
		let LiveNext::Delivery(delivery) = handle.next()? else {
			return Err(FerrosGrynnError::invalid_argument("GQL formatted delivery missing"));
		};
		let LivePayload::Batch(batch) = delivery.payload() else {
			return Err(FerrosGrynnError::invalid_argument("GQL formatted batch missing"));
		};
		assert_eq!(batch.events().len(), 1);
		if scalar {
			assert!(
				matches!(batch.events()[0].output(), Some(LiveEventOutput::Diff(changes)) if changes.len() == 1 && changes[0].path().is_empty())
			);
		} else {
			assert!(
				matches!(batch.events()[0].output(), Some(LiveEventOutput::Patch(operations)) if operations.len() == 1 && operations[0].path() == "/age")
			);
		}
		handle.close()?;
	}
	Ok(())
}

/// Presentation faults suspend LIVE while the source mutation commits.
#[test]
fn live_output_mode_resource_fault_preserves_source_commit() -> FerrosGrynnResult<()> {
	use crate::live::{LiveNext, LiveOutputMode, LiveState};
	let store = Datastore::in_memory();
	let mut setup = store.begin();
	let class =
		setup.define_class(ClassSchema::new("item", AtomKind::Node, [ClassField::new(
			"text",
			Type::String,
		)])?)?;
	let text = setup.intern_property("text")?;
	let record: RecordId = "item:a".parse()?;
	setup.create_atom(record.clone(), PropertyMap::from_iter([(text, Value::from("initial"))]))?;
	setup.commit()?;
	let options = LiveOptions::default().with_budgets(256 * 1024 * 1024, 4096)?;
	let mut handle = store.subscribe_live(
		LiveQuery::new(LiveSource::Class(class)).with_output(LiveOutputMode::Patch),
		options,
	)?;
	let updated = "x".repeat(16 * 1024);
	let initial_statistics = store.live_statistics()?;
	let mut update = store.begin();
	update.set_base_property(&record, text, Value::from(updated.clone()))?;
	update.commit()?;
	assert_eq!(store.begin().live_info(handle.id())?.state(), LiveState::Paused);
	let LiveNext::Delivery(delivery) = handle.next()? else {
		return Err(FerrosGrynnError::invalid_argument("presentation fault missing"));
	};
	assert!(matches!(delivery.payload(), LivePayload::Fault { .. }));
	let statistics = store.live_statistics()?;
	assert_eq!(statistics.published_faults, initial_statistics.published_faults + 1);
	assert_eq!(statistics.published_batches, initial_statistics.published_batches);
	assert!(statistics.query_evaluation_attempts > initial_statistics.query_evaluation_attempts);
	let result = store
		.execute("SELECT VALUE text FROM item:a;", &Object::new())?
		.into_iter()
		.next()
		.ok_or_else(|| FerrosGrynnError::invalid_argument("source verification missing"))?
		.result?;
	assert_eq!(result.value(), &Value::from(vec![Value::from(updated)]));
	Ok(())
}

/// Registration observes absent namespaces and an empty graph's class set.
#[test]
fn live_result_registration_ssi_tracks_absent_class_and_namespace() -> FerrosGrynnResult<()> {
	use crate::{
		graph::GraphKind,
		live::{LiveField, LivePath},
	};
	let store = Datastore::in_memory();
	let mut setup = store.begin();
	let class =
		setup.define_class(ClassSchema::new("item", AtomKind::Node, [ClassField::new(
			"link",
			Type::Record,
		)])?)?;
	let link = setup.intern_property("link")?;
	setup.create_atom(
		"item:a".parse()?,
		PropertyMap::from_iter([(link, Value::from("missing:a".parse::<RecordId>()?))]),
	)?;
	let graph = setup.define_graph("scope", GraphKind::Metagraph)?;
	setup.commit()?;
	let mut reader = store.begin();
	reader.register_live(
		LiveQuery::new(LiveSource::Class(class))
			.with_projection(LiveProjection::Fields(vec![LiveField::new(LiveExpr::field("link")?)]))
			.with_fetch(vec![LivePath::field("link")?]),
		LiveOptions::default(),
	)?;
	let mut writer = store.begin();
	writer.define_class(ClassSchema::new("missing", AtomKind::Node, [])?)?;
	writer.create_atom("missing:a".parse()?, PropertyMap::new())?;
	writer.commit()?;
	assert!(reader.commit().is_err(), "negative FETCH schema dependency must validate");
	let mut reader = store.begin();
	reader.register_live(
		LiveQuery::new(LiveSource::Graph(graph)).with_projection(LiveProjection::Value(
			LiveExpr::Call {
				name: "count".into(),
				arguments: Vec::new(),
			},
		)),
		LiveOptions::default(),
	)?;
	let mut writer = store.begin();
	writer.define_class(ClassSchema::new("new_class", AtomKind::Node, [])?)?;
	writer.commit()?;
	assert!(reader.commit().is_err(), "graph-wide registration must validate class namespace");
	Ok(())
}

/// Data and tracked parameters produce one canonical enter/update/leave batch.
#[test]
fn live_result_final_parameters_net_changes_and_owned_images() -> FerrosGrynnResult<()> {
	let store = Datastore::in_memory();
	let mut tx = store.begin();
	let class = tx.define_class(ClassSchema::new("item", AtomKind::Node, [ClassField::new(
		"age",
		Type::Int,
	)])?)?;
	let age = tx.intern_property("age")?;
	let first: RecordId = "item:a".parse()?;
	let second: RecordId = "item:b".parse()?;
	tx.create_atom(first.clone(), PropertyMap::from_iter([(age, 10_i64.into())]))?;
	tx.create_atom(second.clone(), PropertyMap::from_iter([(age, 20_i64.into())]))?;
	let threshold = tx.define_live_binding(
		LiveExpr::Literal(15_i64.into()),
		Object::new(),
		LiveBindingOptions::default(),
	)?;
	let query = LiveQuery::new(LiveSource::Class(class))
		.with_filter(
			LiveExpr::field("age")?
				.binary(LiveBinaryOperator::GreaterOrEqual, LiveExpr::Binding(threshold)),
		)
		.with_projection(LiveProjection::Value(LiveExpr::field("age")?));
	let live = tx.register_live(query, LiveOptions::default())?;
	tx.commit()?;
	assert!(
		store.inner.current_root().live_catalog().prepared.is_empty(),
		"registration emits no initial rows"
	);
	let mut change = store.begin();
	change.set_base_property(&first, age, 30_i64.into())?;
	change.replace_live_binding_value(threshold, 25_i64.into())?;
	let stamp = change.commit()?.versionstamp();
	let old = store.inner.current_root();
	let payload = old
		.live_catalog()
		.prepared
		.iter()
		.find(|(id, _)| *id == live)
		.ok_or_else(|| FerrosGrynnError::invalid_argument("missing test LIVE batch"))?
		.1
		.clone();
	let LivePayload::Batch(batch) = payload.as_ref() else {
		return Err(FerrosGrynnError::invalid_argument("expected batch"));
	};
	assert_eq!(batch.versionstamp(), stamp);
	assert_eq!(batch.events().len(), 2);
	assert_eq!(batch.events()[0].identity(), &LiveIdentity::Record(first.clone()));
	assert_eq!(batch.events()[0].before(), None);
	assert_eq!(batch.events()[0].after(), Some(&Value::from(30_i64)));
	assert_eq!(batch.events()[1].before(), Some(&Value::from(20_i64)));
	assert_eq!(batch.events()[1].after(), None);
	assert_eq!(batch.bindings()[0].value(), stamp);
	let envelope = old
		.live_catalog()
		.prepared_encoded
		.iter()
		.find(|(id, _)| *id == live)
		.ok_or_else(|| FerrosGrynnError::invalid_argument("missing computed result envelope"))?
		.1
		.clone();
	let mut restore = store.begin();
	restore.set_base_property(&first, age, 100_i64.into())?;
	restore.set_base_property(&first, age, 30_i64.into())?;
	restore.commit()?;
	assert!(
		store.inner.current_root().live_catalog().prepared.is_empty(),
		"net update-to-restore has no result event"
	);
	assert!(store.inner.current_root().live_catalog().prepared_encoded.is_empty());
	let LivePayload::Batch(restored) =
		crate::storage::live_history::decode(&envelope, 8 * 1024 * 1024)?
	else {
		return Err(FerrosGrynnError::invalid_argument("expected captured result batch"));
	};
	assert_eq!(
		&restored, batch,
		"captured results are independent of current primary and parameters"
	);
	assert_eq!(
		batch.events()[0].after(),
		Some(&Value::from(30_i64)),
		"owned payload remains exact"
	);
	let mut parameter = store.begin();
	parameter.replace_live_binding_value(threshold, 5_i64.into())?;
	parameter.commit()?;
	assert_eq!(store.inner.current_root().live_catalog().prepared.len(), 1);
	Ok(())
}

/// Active result errors pause delivery while source DML commits; new errors
/// reject registration.
#[test]
fn live_result_fault_boundary_and_registration_errors() -> FerrosGrynnResult<()> {
	let store = Datastore::in_memory();
	let mut tx = store.begin();
	let class = tx.define_class(ClassSchema::new("item", AtomKind::Node, [ClassField::new(
		"divisor",
		Type::Int,
	)])?)?;
	let divisor = tx.intern_property("divisor")?;
	let record: RecordId = "item:a".parse()?;
	tx.create_atom(record.clone(), PropertyMap::from_iter([(divisor, 2_i64.into())]))?;
	let query = LiveQuery::new(LiveSource::Class(class)).with_projection(LiveProjection::Value(
		LiveExpr::Literal(10_i64.into())
			.binary(LiveBinaryOperator::Divide, LiveExpr::field("divisor")?),
	));
	let live = tx.register_live(query.clone(), LiveOptions::default())?;
	tx.commit()?;
	let mut old = store.begin();
	let mut change = store.begin();
	change.set_base_property(&record, divisor, 0_i64.into())?;
	change.commit()?;
	let info = store.begin().live_info(live)?;
	assert_eq!(info.state(), LiveState::Paused);
	assert!(info.diagnostic().is_some());
	assert_eq!(old.live_info(live)?.state(), LiveState::Active);
	assert!(
		matches!(store.inner.current_root().live_catalog().prepared[0].1.as_ref(), LivePayload::Fault { gap, .. } if gap.reason() == LiveGapReason::EvaluationFault)
	);
	let count = store.inner.current_root().live_catalog().queries.len();
	assert!(store.begin().register_live(query, LiveOptions::default()).is_err());
	assert_eq!(store.inner.current_root().live_catalog().queries.len(), count);
	Ok(())
}

/// Unchanged name operands retain identity; explicit changes can choose another
/// graph.
#[test]
fn live_result_scope_expression_name_identity() -> FerrosGrynnResult<()> {
	use crate::{changefeed::ChangefeedSource, graph::GraphKind};
	let store = Datastore::in_memory();
	let mut setup = store.begin();
	let class = setup.define_class(ClassSchema::new("item", AtomKind::Node, [])?)?;
	let key = setup.intern_property("value")?;
	let record: RecordId = "item:a".parse()?;
	setup.create_atom(record.clone(), PropertyMap::from_iter([(key, 1.into())]))?;
	let first = setup.define_graph("name_scope_a", GraphKind::Graph)?;
	let second = setup.define_graph("name_scope_b", GraphKind::Graph)?;
	for graph in [&first, &second] {
		setup.attach_node(graph.id(), &record, None, None)?;
	}
	let scope = setup.define_live_binding(
		LiveExpr::Literal(Value::from("name_scope_a")),
		Object::new(),
		LiveBindingOptions::default(),
	)?;
	setup.commit()?;
	let mut registration = store.begin();
	let mut ids = Vec::new();
	for source in [
		LiveSource::GraphExpr(Box::new(LiveExpr::Binding(scope))),
		LiveSource::GraphClassExpr {
			graph: Box::new(LiveExpr::Binding(scope)),
			class,
		},
		LiveSource::GraphRecordExpr {
			graph: Box::new(LiveExpr::Binding(scope)),
			record: record.clone(),
		},
	] {
		ids.push(registration.register_live(LiveQuery::new(source), LiveOptions::default())?);
	}
	registration.commit()?;
	let mut changed = store.begin();
	changed.replace_live_binding_value(scope, Value::from("name_scope_b"))?;
	changed.commit()?;
	let old = store.inner.current_root();
	for id in &ids {
		assert_eq!(
			old.live_catalog().queries[id].capture_source,
			ChangefeedSource::Graph(second.id())
		);
		assert_eq!(old.live_catalog().queries[id].state, LiveState::Active);
	}
	let mut recreate = store.begin();
	recreate.remove_graph("name_scope_b")?;
	let replacement = recreate.define_graph("name_scope_b", GraphKind::Graph)?;
	assert_ne!(replacement.id(), second.id());
	recreate.attach_node(replacement.id(), &record, None, None)?;
	recreate.set_overlay(replacement.id(), &record, key, 2.into())?;
	recreate.set_base_property(&record, key, 3.into())?;
	recreate.commit()?;
	let fault = store.inner.current_root();
	for id in ids {
		assert_eq!(
			fault.live_catalog().queries[&id].capture_source,
			ChangefeedSource::Graph(second.id())
		);
		assert_eq!(fault.live_catalog().queries[&id].state, LiveState::Paused);
		assert!(matches!(
			fault
				.live_catalog()
				.prepared
				.iter()
				.find(|(owner, _)| *owner == id)
				.map(|(_, payload)| payload.as_ref()),
			Some(LivePayload::Fault { .. })
		));
		assert_eq!(old.live_catalog().queries[&id].state, LiveState::Active);
	}
	assert_eq!(
		store
			.begin()
			.atom(&record)?
			.ok_or_else(|| FerrosGrynnError::invalid_argument("scope identity DML missing"))?
			.properties()
			.get(key),
		Some(&Value::from(3))
	);
	Ok(())
}

/// Root JOIN scopes retain separate identities until their own values change.
#[test]
fn live_result_rhs_scope_anchors_are_independent() -> FerrosGrynnResult<()> {
	use crate::{
		graph::GraphKind,
		live::{LiveJoin, LiveJoinKind, LiveNext},
	};
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
	let a = setup.define_graph("rhs_a", GraphKind::Graph)?;
	let b = setup.define_graph("rhs_b", GraphKind::Graph)?;
	let c = setup.define_graph("rhs_c", GraphKind::Graph)?;
	for (graph, value) in [(&a, 10), (&b, 20), (&c, 30)] {
		setup.attach_node(graph.id(), &record, None, None)?;
		setup.set_overlay(graph.id(), &record, key, value.into())?;
	}
	let first = setup.define_live_binding(
		LiveExpr::Literal("rhs_a".into()),
		Object::new(),
		LiveBindingOptions::default(),
	)?;
	let second = setup.define_live_binding(
		LiveExpr::Literal("rhs_b".into()),
		Object::new(),
		LiveBindingOptions::default(),
	)?;
	setup.commit()?;
	let path = |alias: &str| -> FerrosGrynnResult<LiveExpr> {
		Ok(LiveExpr::Field(crate::live::LivePath::new(vec![alias.into(), "value".into()])?))
	};
	let query = LiveQuery::new(LiveSource::Record(record.clone()))
		.with_alias("l")?
		.with_joins(vec![
			LiveJoin::new(
				LiveSource::GraphClassExpr {
					graph: Box::new(LiveExpr::Binding(first)),
					class,
				},
				"a",
				LiveJoinKind::Cross,
				None,
			)?,
			LiveJoin::new(
				LiveSource::GraphRecordExpr {
					graph: Box::new(LiveExpr::Binding(second)),
					record: record.clone(),
				},
				"b",
				LiveJoinKind::Cross,
				None,
			)?,
		])
		.with_projection(LiveProjection::Value(LiveExpr::Array(vec![path("a")?, path("b")?])));
	let mut handle = store.subscribe_live(query, LiveOptions::default())?;
	let id = handle.id();
	let before = store.inner.current_root();
	let anchors = &before.live_catalog().queries[&id].rhs_scope_anchors;
	assert_eq!(anchors.len(), 2);
	assert_eq!((anchors[0].source, anchors[0].graph), (1, a.id()));
	assert_eq!((anchors[1].source, anchors[1].graph), (2, b.id()));
	let mut change = store.begin();
	change.replace_live_binding_value(first, "rhs_c".into())?;
	change.commit()?;
	let after = store.inner.current_root();
	assert_eq!(after.live_catalog().queries[&id].rhs_scope_anchors[0].graph, c.id());
	assert_eq!(after.live_catalog().queries[&id].rhs_scope_anchors[1].graph, b.id());
	assert_eq!(before.live_catalog().queries[&id].rhs_scope_anchors[0].graph, a.id());
	let LiveNext::Delivery(delivery) = handle.next()? else {
		return Err(FerrosGrynnError::invalid_argument("RHS scope delivery missing"));
	};
	let LivePayload::Batch(batch) = delivery.payload() else {
		return Err(FerrosGrynnError::invalid_argument("RHS scope batch missing"));
	};
	assert!(
		batch.events().iter().any(
			|event| event.after() == Some(&Value::from(vec![Value::from(30), Value::from(20)]))
		)
	);
	handle.ack(delivery.token())?;
	let mut replace = store.begin();
	replace.remove_graph("rhs_b")?;
	let new_b = replace.define_graph("rhs_b", GraphKind::Graph)?;
	replace.attach_node(new_b.id(), &record, None, None)?;
	replace.set_overlay(new_b.id(), &record, key, 99.into())?;
	replace.set_base_property(&record, key, 2.into())?;
	replace.commit()?;
	assert_ne!(new_b.id(), b.id());
	let fault = store.inner.current_root();
	assert_eq!(fault.live_catalog().queries[&id].state, LiveState::Paused);
	assert_eq!(fault.live_catalog().queries[&id].rhs_scope_anchors[1].graph, b.id());
	assert_eq!(after.live_catalog().queries[&id].state, LiveState::Active);
	assert_eq!(
		store
			.begin()
			.atom(&record)?
			.ok_or_else(|| FerrosGrynnError::invalid_argument("RHS source DML missing"))?
			.properties()
			.get(key),
		Some(&Value::from(2))
	);
	Ok(())
}

/// Quiet future-SINCE intervals retain the successful operand, so a changed
/// binding resolves its new graph when result evaluation resumes.
#[test]
fn live_result_scope_expression_future_boundary() -> FerrosGrynnResult<()> {
	use crate::{graph::GraphKind, live::LiveSince};
	let store = Datastore::in_memory();
	let mut setup = store.begin();
	setup.define_class(ClassSchema::new("item", AtomKind::Node, [ClassField::new(
		"value",
		Type::Int,
	)])?)?;
	let key = setup.intern_property("value")?;
	let record: RecordId = "item:a".parse()?;
	setup.create_atom(record.clone(), PropertyMap::from_iter([(key, 1.into())]))?;
	let first = setup.define_graph("future_scope_a", GraphKind::Graph)?;
	let second = setup.define_graph("future_scope_b", GraphKind::Graph)?;
	for graph in [&first, &second] {
		setup.attach_node(graph.id(), &record, None, None)?;
	}
	setup.set_overlay(first.id(), &record, key, 10.into())?;
	let scope = setup.define_live_binding(
		LiveExpr::Literal(Value::from("future_scope_a")),
		Object::new(),
		LiveBindingOptions::default(),
	)?;
	setup.commit()?;
	let future_ns = store.inner.current_root().committed_at_ns + 86_400_000_000_000;
	let future = crate::values::DateTime::from(
		jiff::Timestamp::from_nanosecond(future_ns)?.to_zoned(jiff::tz::TimeZone::UTC),
	);
	let mut registration = store.begin();
	let id = registration.register_live(
		LiveQuery::new(LiveSource::GraphRecordExpr {
			graph: Box::new(LiveExpr::Binding(scope)),
			record: record.clone(),
		})
		.with_projection(LiveProjection::Value(LiveExpr::field("value")?)),
		LiveOptions::default().with_since(LiveSince::Timestamp(future)),
	)?;
	registration.commit()?;
	let mut quiet = store.begin();
	quiet.replace_live_binding_value(scope, Value::from("future_scope_b"))?;
	quiet.commit()?;
	let before = store.inner.current_root();
	assert_eq!(
		before.live_catalog().queries[&id].capture_scope_name.as_ref().map(|name| name.as_str()),
		Some("future_scope_a")
	);
	let mut update = store.begin();
	update.set_overlay(second.id(), &record, key, 2.into())?;
	update.commit()?;
	// Only the clock boundary is advanced: data and bindings are real committed
	// roots.
	let mut candidate = (*store.inner.current_root()).clone();
	candidate.committed_at_ns = future_ns;
	evaluate_queries(
		&before,
		&mut candidate,
		std::env::temp_dir(),
		super::super::LiveQueryTargets::All,
		&store.inner.live_observation,
	)?;
	assert_eq!(candidate.live_catalog().queries[&id].state, LiveState::Active);
	assert_eq!(
		candidate.live_catalog().queries[&id].capture_source,
		crate::changefeed::ChangefeedSource::Graph(second.id())
	);
	let Some((_, payload)) =
		candidate.live_catalog().prepared.iter().find(|(owner, _)| *owner == id)
	else {
		return Err(FerrosGrynnError::invalid_argument("future scope batch missing"));
	};
	let LivePayload::Batch(batch) = payload.as_ref() else {
		return Err(FerrosGrynnError::invalid_argument("future scope fault"));
	};
	assert_eq!(batch.events().len(), 1);
	assert_eq!(batch.events()[0].before(), Some(&Value::from(1)));
	assert_eq!(batch.events()[0].after(), Some(&Value::from(2)));
	assert!(
		matches!(batch.events()[0].identity(), LiveIdentity::Occurrence(occurrence) if occurrence.graph().id() == second.id())
	);
	Ok(())
}

/// Pending registration cannot silently adopt a replacement with the same name.
#[test]
fn live_result_scope_expression_pending_name_reuse() -> FerrosGrynnResult<()> {
	use crate::graph::GraphKind;
	let store = Datastore::in_memory();
	let mut setup = store.begin();
	let class = setup.define_class(ClassSchema::new("item", AtomKind::Node, [])?)?;
	let graph = setup.define_graph("pending_scope", GraphKind::Graph)?;
	setup.commit()?;
	let mut registration = store.begin();
	let id = registration.register_live(
		LiveQuery::new(LiveSource::GraphClassExpr {
			graph: Box::new(LiveExpr::Literal(Value::from("pending_scope"))),
			class,
		}),
		LiveOptions::default(),
	)?;
	registration.remove_graph("pending_scope")?;
	let replacement = registration.define_graph("pending_scope", GraphKind::Graph)?;
	assert_ne!(replacement.id(), graph.id());
	assert!(registration.commit().is_err());
	assert!(store.begin().live_info(id).is_err());
	assert_eq!(
		store
			.inner
			.current_root()
			.schema
			.named_graph("pending_scope")
			.ok_or_else(|| FerrosGrynnError::invalid_argument("pending rollback graph missing"))?
			.id(),
		graph.id()
	);
	Ok(())
}

/// Scope changes compare qualified before/after roots and choose the final
/// graph feed.
#[test]
fn live_result_scope_expression_policy_and_faults() -> FerrosGrynnResult<()> {
	use crate::{
		changefeed::{ChangefeedConfig, ChangefeedSource},
		graph::GraphKind,
		live::LiveSource,
	};
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
	let first = setup.define_graph("expr_scope_a", GraphKind::Graph)?;
	let second = setup.define_graph("expr_scope_b", GraphKind::Graph)?;
	for (graph, value) in [(&first, 10), (&second, 20)] {
		setup.attach_node(graph.id(), &record, None, None)?;
		setup.set_overlay(graph.id(), &record, key, value.into())?;
	}
	let config = ChangefeedConfig::from_nanoseconds(86_400_000_000_000)?;
	setup.configure_changefeed(ChangefeedSource::Class(class), config)?;
	setup.configure_changefeed(ChangefeedSource::Graph(first.id()), config)?;
	let scope = setup.define_live_binding(
		LiveExpr::Literal(first.clone().into()),
		Object::new(),
		LiveBindingOptions::default(),
	)?;
	setup.commit()?;
	let mut registered = store.begin();
	let mut ids = Vec::new();
	for source in [
		LiveSource::GraphClassExpr {
			graph: Box::new(LiveExpr::Binding(scope)),
			class,
		},
		LiveSource::GraphRecordExpr {
			graph: Box::new(LiveExpr::Binding(scope)),
			record: record.clone(),
		},
	] {
		ids.push(
			registered.register_live(
				LiveQuery::new(source)
					.with_projection(LiveProjection::Value(LiveExpr::field("value")?)),
				LiveOptions::default(),
			)?,
		);
	}
	registered.commit()?;
	let old = store.inner.current_root();
	let mut switch = store.begin();
	switch.set_overlay(first.id(), &record, key, 11.into())?;
	switch.set_overlay(second.id(), &record, key, 21.into())?;
	switch.replace_live_binding_value(scope, second.clone().into())?;
	let stamp = switch.commit()?.versionstamp().get();
	let switched = store.inner.current_root();
	for id in &ids {
		assert_eq!(
			old.live_catalog().queries[id].capture_source,
			ChangefeedSource::Graph(first.id())
		);
		assert_eq!(
			switched.live_catalog().queries[id].capture_source,
			ChangefeedSource::Graph(second.id())
		);
		let payload = switched
			.live_catalog()
			.prepared
			.iter()
			.find(|(owner, _)| owner == id)
			.ok_or_else(|| FerrosGrynnError::invalid_argument("missing scope switch batch"))?;
		let LivePayload::Batch(batch) = payload.1.as_ref() else {
			return Err(FerrosGrynnError::invalid_argument("scope switch is not a batch"));
		};
		assert_eq!(batch.events().len(), 2);
		for event in batch.events() {
			let LiveIdentity::Occurrence(occurrence) = event.identity() else {
				return Err(FerrosGrynnError::invalid_argument("scope lost qualified identity"));
			};
			assert_eq!(occurrence.atom().id(), &record);
			if occurrence.graph().id() == first.id() {
				assert_eq!(event.before(), Some(&Value::from(10)));
				assert_eq!(event.after(), None);
			} else {
				assert_eq!(occurrence.graph().id(), second.id());
				assert_eq!(event.before(), None);
				assert_eq!(event.after(), Some(&Value::from(21)));
			}
		}
		assert_eq!(
			switched.live_catalog().histories[id].epochs.last().and_then(|epoch| epoch.policy),
			None
		);
		assert!(
			!switched.live_history().contains_key(&(*id, stamp)),
			"graph scope cannot inherit the CLASS feed"
		);
	}
	let mut enable = store.begin();
	let feed = enable.configure_changefeed(ChangefeedSource::Graph(second.id()), config)?;
	enable.replace_live_binding_value(scope, Value::from("expr_scope_b"))?;
	enable.set_overlay(second.id(), &record, key, 22.into())?;
	let captured = enable.commit()?.versionstamp().get();
	let valid = store.inner.current_root();
	for id in &ids {
		assert_eq!(
			valid.live_catalog().histories[id]
				.epochs
				.last()
				.and_then(|epoch| epoch.policy)
				.map(|policy| policy.source),
			Some(feed)
		);
		assert!(valid.live_history().contains_key(&(*id, captured)));
	}
	let mut invalid = store.begin();
	invalid.replace_live_binding_value(scope, Value::Null)?;
	invalid.set_base_property(&record, key, 99.into())?;
	let failed = invalid.commit()?.versionstamp().get();
	let fault = store.inner.current_root();
	for id in ids {
		assert_eq!(fault.live_catalog().queries[&id].state, LiveState::Paused);
		assert_eq!(
			fault.live_catalog().queries[&id].capture_source,
			ChangefeedSource::Graph(second.id())
		);
		assert!(
			fault.live_history().contains_key(&(id, failed)),
			"fault uses the last successful owner"
		);
		assert_eq!(valid.live_catalog().queries[&id].state, LiveState::Active);
	}
	assert_eq!(
		store
			.begin()
			.atom(&record)?
			.ok_or_else(|| FerrosGrynnError::invalid_argument("source DML lost record"))?
			.properties()
			.get(key),
		Some(&Value::from(99))
	);
	Ok(())
}

/// Exact representation comparisons retain numeric variant and scalar absence.
#[test]
fn live_result_representation_and_none_are_distinct() -> FerrosGrynnResult<()> {
	let identity = LiveIdentity::Record("item:a".parse()?);
	let mut budget = BindingAdmission::new(&LiveBindingOptions::default());
	let entries = net_changes(
		BTreeMap::from([(identity.clone(), Value::from(1_i64))]),
		BTreeMap::from([(identity.clone(), Value::from(1.0_f64))]),
		&mut budget,
	)?;
	assert_eq!(entries.len(), 1);
	let entries =
		net_changes(BTreeMap::new(), BTreeMap::from([(identity, Value::None)]), &mut budget)?;
	assert_eq!(entries[0].before(), None);
	assert_eq!(entries[0].after(), Some(&Value::None));
	Ok(())
}

/// Public path equality stays numeric, while LIVE images retain exact cost
/// variants.
#[test]
fn live_result_path_cost_representation_is_observable() -> FerrosGrynnResult<()> {
	use crate::{
		graph::{Path, PathStep, SnapshotId},
		values::Number,
	};
	let record: RecordId = "item:a".parse()?;
	let origin: crate::values::Uuid = uuid::Uuid::now_v7().into();
	let path = |cost| {
		Value::from(Path::new(origin, SnapshotId::new(1, 0), Vec::<PathStep>::new(), Some(cost)))
	};
	let integer = path(Number::Int(1));
	let floating = path(Number::Float(1.0));
	assert_eq!(integer, floating, "public runtime equality is preserved");
	let store = Datastore::in_memory();
	let mut setup = store.begin();
	let class =
		setup.define_class(ClassSchema::new("item", AtomKind::Node, [ClassField::new(
			"path",
			Type::Any,
		)])?)?;
	let key = setup.intern_property("path")?;
	setup.create_atom(record.clone(), PropertyMap::from_iter([(key, integer)]))?;
	setup.register_live(
		LiveQuery::new(LiveSource::Class(class))
			.with_projection(LiveProjection::Value(LiveExpr::field("path")?)),
		LiveOptions::default(),
	)?;
	setup.commit()?;
	let mut change = store.begin();
	change.set_base_property(&record, key, floating)?;
	change.commit()?;
	let root = store.inner.current_root();
	let LivePayload::Batch(batch) = root.live_catalog().prepared[0].1.as_ref() else {
		return Err(FerrosGrynnError::invalid_argument("fixture path batch"));
	};
	assert_eq!(batch.events().len(), 1);
	let Value::Path(path) = batch.events()[0]
		.after()
		.ok_or_else(|| FerrosGrynnError::invalid_argument("fixture path image"))?
	else {
		return Err(FerrosGrynnError::invalid_argument("fixture path image type"));
	};
	assert!(matches!(path.cost(), Some(Number::Float(_))));
	Ok(())
}

/// Oversized active diagnostics pause LIVE and preserve the otherwise valid
/// source commit.
#[test]
fn live_result_oversized_fault_diagnostic_preserves_source_commit() -> FerrosGrynnResult<()> {
	use crate::datastore::{FunctionEffect, SchemaFunction, SchemaFunctionParameter};
	let store = Datastore::in_memory();
	let mut setup = store.begin();
	let class =
		setup.define_class(ClassSchema::new("item", AtomKind::Node, [ClassField::new(
			"text",
			Type::String,
		)])?)?;
	let key = setup.intern_property("text")?;
	let record: RecordId = "item:a".parse()?;
	setup.create_atom(record.clone(), PropertyMap::from_iter([(key, "1".into())]))?;
	setup.define_function(SchemaFunction::new(
		"fn",
		"parse_int",
		vec![SchemaFunctionParameter::new("value", Type::String)?],
		Type::Int,
		FunctionEffect::Pure,
		false,
		None,
		"{ RETURN <int>$value; }",
	)?)?;
	let live = setup.register_live(
		LiveQuery::new(LiveSource::Class(class)).with_projection(LiveProjection::Value(
			LiveExpr::Call {
				name: "fn::parse_int".into(),
				arguments: vec![LiveExpr::field("text")?],
			},
		)),
		LiveOptions::default(),
	)?;
	setup.commit()?;
	let mut change = store.begin();
	change.set_base_property(&record, key, Value::from("x".repeat(17 * 1024)))?;
	let stamp = change.commit()?.versionstamp();
	let atom = store
		.begin()
		.atom(&record)?
		.ok_or_else(|| FerrosGrynnError::invalid_argument("fixture committed source"))?;
	assert_eq!(atom.properties().get(key), Some(&Value::from("x".repeat(17 * 1024))));
	assert_eq!(store.begin().live_info(live)?.state(), LiveState::Paused);
	let root = store.inner.current_root();
	let envelope = &root.live_catalog().prepared_encoded[0].1;
	let LivePayload::Fault {
		diagnostic,
		gap,
	} = crate::storage::live_history::decode(envelope, 64 * 1024)?
	else {
		return Err(FerrosGrynnError::invalid_argument("fixture capture resource fault"));
	};
	assert_eq!(gap.through(), Some(stamp));
	assert!(
		diagnostic
			.status()
			.message()
			.is_some_and(|message| message.contains("original diagnostic unavailable"))
	);
	assert!(diagnostic.cause().is_some(), "capture failure remains typed");
	Ok(())
}

/// Initial and final private results must be restorable even though activation
/// does not emit the initial rows.
#[test]
fn live_result_registration_rejects_nonrestorable_current_and_final_rows() -> FerrosGrynnResult<()>
{
	let store = Datastore::in_memory();
	let mut setup = store.begin();
	let class =
		setup.define_class(ClassSchema::new("item", AtomKind::Node, [ClassField::new(
			"value",
			Type::Any,
		)])?)?;
	let key = setup.intern_property("value")?;
	let record: RecordId = "item:a".parse()?;
	let runtime = || {
		Value::from(crate::values::Closure::new(None, Vec::new(), Type::Int, |_arguments| {
			Ok(Value::from(1))
		}))
	};
	setup.create_atom(record.clone(), PropertyMap::from_iter([(key, runtime())]))?;
	setup.commit()?;
	let query = || -> FerrosGrynnResult<LiveQuery> {
		Ok(LiveQuery::new(LiveSource::Class(class))
			.with_projection(LiveProjection::Value(LiveExpr::field("value")?)))
	};
	let mut tx = store.begin();
	assert!(tx.register_live(query()?, LiveOptions::default()).is_err());
	assert!(tx.working.live_catalog().queries.is_empty());
	tx.set_base_property(&record, key, Value::from(1))?;
	tx.commit()?;
	let stamp = store.inner.current_root().commit_ts;
	let mut tx = store.begin();
	tx.register_live(query()?, LiveOptions::default())?;
	tx.set_base_property(&record, key, runtime())?;
	assert!(tx.commit().is_err(), "final pending results are rechecked before activation");
	assert_eq!(store.inner.current_root().commit_ts, stamp);
	assert!(store.inner.current_root().live_catalog().queries.is_empty());
	Ok(())
}

/// Unrelated registrations evaluate one new query and leave queued history
/// untouched.
#[test]
fn live_registration_only_preserves_old_delivery_and_evaluation_count() -> FerrosGrynnResult<()> {
	use crate::{
		changefeed::{ChangefeedConfig, ChangefeedSource},
		live::LiveNext,
	};
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
	setup.configure_changefeed(
		ChangefeedSource::Class(class),
		ChangefeedConfig::new("P1D".parse()?)?,
	)?;
	setup.commit()?;
	let query = LiveQuery::new(LiveSource::Class(class))
		.with_projection(LiveProjection::Value(LiveExpr::field("value")?));
	let mut first = store.subscribe_live(query.clone(), LiveOptions::default())?;
	let mut change = store.begin();
	change.set_base_property(&record, key, 2.into())?;
	change.commit()?;
	let LiveNext::Delivery(saved) = first.next()? else {
		return Err(FerrosGrynnError::invalid_argument("old delivery missing"));
	};
	let token = saved.token().clone();
	let old_root = store.inner.current_root();
	let old_history = old_root.live_catalog().histories[&first.id()].clone();
	let old_definition = old_root.live_catalog().queries[&first.id()].clone();
	let mut others = Vec::new();
	for _ in 0..3 {
		let attempts = store.live_statistics()?.query_evaluation_attempts;
		let handle = store.subscribe_live(query.clone(), LiveOptions::default())?;
		assert_eq!(store.live_statistics()?.query_evaluation_attempts - attempts, 1);
		others.push(handle);
	}
	let now = store.inner.current_root();
	assert!(Arc::ptr_eq(&old_definition, &now.live_catalog().queries[&first.id()]));
	assert_eq!(old_history.last_stamp, now.live_catalog().histories[&first.id()].last_stamp);
	let LiveNext::Delivery(repeated) = first.next()? else {
		return Err(FerrosGrynnError::invalid_argument("old delivery replaced"));
	};
	assert_eq!(repeated.token(), &token);
	first.ack(&token)?;
	assert!(matches!(first.next()?, LiveNext::Idle));
	for handle in &mut others {
		assert!(matches!(handle.next()?, LiveNext::Idle));
	}
	Ok(())
}

/// Mixed data mutations and binding catalogs retain the complete evaluator.
#[test]
fn live_registration_mixed_data_and_bindings_use_full_evaluation() -> FerrosGrynnResult<()> {
	use crate::live::{LiveBindingOptions, LiveNext};
	for bindings in [false, true] {
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
		if bindings {
			setup.define_live_binding(
				LiveExpr::Literal(1.into()),
				Object::new(),
				LiveBindingOptions::default(),
			)?;
		}
		setup.commit()?;
		let query = LiveQuery::new(LiveSource::Class(class))
			.with_projection(LiveProjection::Value(LiveExpr::field("value")?));
		let mut first = store.subscribe_live(query.clone(), LiveOptions::default())?;
		let attempts = store.live_statistics()?.query_evaluation_attempts;
		let mut registration = store.begin();
		registration.register_live(query, LiveOptions::default())?;
		if !bindings {
			registration.set_base_property(&record, key, 2.into())?;
		}
		registration.commit()?;
		assert_eq!(store.live_statistics()?.query_evaluation_attempts - attempts, 2);
		if bindings {
			assert!(matches!(first.next()?, LiveNext::Idle));
		} else {
			assert!(matches!(first.next()?, LiveNext::Delivery(_)));
		}
	}
	Ok(())
}

/// A non-NOW definition conservatively disables registration-only selection.
#[test]
fn live_registration_historical_since_keeps_existing_evaluator() -> FerrosGrynnResult<()> {
	use crate::{
		changefeed::{ChangefeedConfig, ChangefeedSource},
		live::LiveSince,
	};
	let store = Datastore::in_memory();
	let mut setup = store.begin();
	let class = setup.define_class(ClassSchema::new("item", AtomKind::Node, [])?)?;
	setup.configure_changefeed(
		ChangefeedSource::Class(class),
		ChangefeedConfig::new("P1D".parse()?)?,
	)?;
	let stamp = setup.commit()?.versionstamp();
	let query = LiveQuery::new(LiveSource::Class(class));
	let _first = store.subscribe_live(
		query.clone(),
		LiveOptions::default().with_since(LiveSince::Versionstamp(stamp)),
	)?;
	let attempts = store.live_statistics()?.query_evaluation_attempts;
	let _second = store.subscribe_live(query, LiveOptions::default())?;
	assert_eq!(store.live_statistics()?.query_evaluation_attempts - attempts, 2);
	Ok(())
}

/// An unrelated registration still exposes a latent function fault at future
/// SINCE.
#[test]
fn live_registration_future_timestamp_activation_keeps_capture_validation() -> FerrosGrynnResult<()>
{
	use crate::{
		datastore::{FunctionEffect, InMemoryOptions, SchemaFunction, SchemaFunctionParameter},
		live::LiveSince,
	};
	let store = Datastore::in_memory();
	let mut setup = store.begin();
	let class =
		setup.define_class(ClassSchema::new("item", AtomKind::Node, [ClassField::new(
			"value",
			Type::Int,
		)])?)?;
	let key = setup.intern_property("value")?;
	setup.create_atom("item:a".parse()?, PropertyMap::from_iter([(key, 1.into())]))?;
	setup.define_function(SchemaFunction::new(
		"fn",
		"identity",
		vec![SchemaFunctionParameter::new("value", Type::Int)?],
		Type::Int,
		FunctionEffect::Pure,
		false,
		None,
		"{ RETURN $value; }",
	)?)?;
	setup.commit()?;
	let future_ns = store.inner.current_root().committed_at_ns + 86_400_000_000_000;
	let future = crate::values::DateTime::from(
		jiff::Timestamp::from_nanosecond(future_ns)?.to_zoned(jiff::tz::TimeZone::UTC),
	);
	let mut registration = store.begin();
	let id = registration.register_live(
		LiveQuery::new(LiveSource::Class(class)).with_projection(LiveProjection::Value(
			LiveExpr::Call {
				name: "fn::identity".into(),
				arguments: vec![LiveExpr::field("value")?],
			},
		)),
		LiveOptions::default().with_since(LiveSince::Timestamp(future)),
	)?;
	registration.commit()?;
	let root = store.inner.current_root();
	let function = root
		.schema
		.functions()
		.next()
		.and_then(|function| function.id())
		.ok_or_else(|| FerrosGrynnError::invalid_argument("function fixture missing"))?;
	drop(root);
	let mut removal = store.begin();
	removal.remove_function(function)?;
	removal.commit()?;
	assert_eq!(store.begin().live_info(id)?.state(), LiveState::Active);
	// Advance only the authoritative clock boundary, with real committed data.
	let mut root = (*store.inner.current_root()).clone();
	root.committed_at_ns = future_ns;
	let restored = Datastore::from_root_for_test(InMemoryOptions::default(), root);
	let _new = restored
		.subscribe_live(LiveQuery::new(LiveSource::Class(class)), LiveOptions::default())?;
	assert_eq!(restored.begin().live_info(id)?.state(), LiveState::Paused);
	assert!(restored.begin().live_info(id)?.diagnostic().is_some());
	Ok(())
}

/// A concurrent source write still rejects stale serializable registration.
#[test]
fn live_registration_only_preserves_concurrent_source_validation() -> FerrosGrynnResult<()> {
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
	setup.commit()?;
	let mut registration = store.begin();
	let id = registration
		.register_live(LiveQuery::new(LiveSource::Class(class)), LiveOptions::default())?;
	let mut writer = store.begin();
	writer.set_base_property(&record, key, 2.into())?;
	writer.commit()?;
	let Err(error) = registration.commit() else {
		return Err(FerrosGrynnError::invalid_argument("stale registration accepted"));
	};
	assert!(matches!(
		error.status().status(),
		crate::gql_status::Status::SerializationFailure { .. }
	));
	assert!(store.begin().live_info(id).is_err());
	Ok(())
}
