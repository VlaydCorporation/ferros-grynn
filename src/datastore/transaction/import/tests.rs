//! Import END shares reactive kernels without weakening its writer lease.

/// Pure ACK uses existing control conflict keys and survives final import
/// rebase.
#[test]
fn live_import_ack_final_stamp_and_private_read_visibility()
-> crate::gql_status::FerrosGrynnResult<()> {
	let f = fixture()?;
	let mut update = f.store.begin();
	update.set_base_property(&f.first, f.key, 3.into())?;
	let issued = update.commit()?.commit_ts();
	let root = f.store.inner.current_root();
	let token = crate::live::LiveCursor::new(
		root.database_uuid.into(),
		f.active,
		root.live_catalog().queries[&f.active].version,
		issued.into(),
	)?;
	let mut old = f.store.begin();
	let old_cursor = old.live_info(f.active)?.cursor().clone();
	let mut mixed = f.store.begin();
	mixed.set_base_property(&f.first, f.key, 3.into())?;
	mixed.acknowledge_live(&token)?;
	let mut control_stamp = 0;
	f.store.import(
		ImportOptions {
			segment_size: 1,
			changefeed: ImportChangefeedMode::On,
			..ImportOptions::default()
		},
		|session| {
			session.apply_item(|tx| tx.set_base_property(&f.first, f.key, 4.into()))?;
			assert!(mixed.commit().is_err(), "no-op DML cannot use the ACK lease exception");
			let mut control = f.store.begin();
			control.acknowledge_live(&token)?;
			control_stamp = control.commit()?.commit_ts();
			assert_eq!(control_stamp, issued + 1);
			assert_eq!(f.store.begin().live_info(f.active)?.cursor(), &token);
			let mut duplicate = f.store.begin();
			duplicate.acknowledge_live(&token)?;
			assert_eq!(
				duplicate.commit()?.commit_ts(),
				control_stamp,
				"idempotent ACK does not publish another clock tick"
			);
			session.apply_item(|tx| tx.set_base_property(&f.first, f.key, 5.into()))?;
			Ok(())
		},
	)?;
	let after = f.store.inner.current_root();
	assert_eq!(after.commit_ts, control_stamp + 1);
	assert_eq!(after.live_catalog().queries[&f.active].acknowledged, issued);
	assert_eq!(after.version(ConflictKey::LiveDefinition(f.active)), control_stamp);
	assert_eq!(old.live_info(f.active)?.cursor(), &old_cursor);
	assert_eq!(f.store.begin().live_info(f.active)?.cursor(), &token);
	assert!(after.live_history().contains_key(&(f.active, after.commit_ts)));
	Ok(())
}

use crate::{
	changefeed::{ChangefeedConfig, ChangefeedSource},
	datastore::{
		ClassField,
		ClassId,
		ClassSchema,
		ClassSchemaMode,
		ConflictKey,
		Datastore,
		ImportChangefeedMode,
		ImportOptions,
		TransactionAccess,
		TransactionOptions,
	},
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	graph::{AtomKind, GraphKind, GraphRef},
	live::{
		LiveBinaryOperator,
		LiveBindingOptions,
		LiveExpr,
		LiveId,
		LiveOptions,
		LivePayload,
		LiveProjection,
		LiveQuery,
		LiveSource,
		LiveState,
	},
	values::{Object, PropertyKeyId, PropertyMap, RecordId, Type, Value},
};

/// Independent public definitions with a FULL source and a qualified graph.
struct Fixture {
	store: Datastore,
	class: ClassId,
	graph: GraphRef,
	key: PropertyKeyId,
	first: RecordId,
	second: RecordId,
	active: LiveId,
	stopped: LiveId,
	feed: crate::changefeed::ChangefeedId,
}

/// Builds results from two existing records; registration emits no batch.
fn fixture() -> FerrosGrynnResult<Fixture> {
	let store = Datastore::in_memory();
	let mut setup = store.begin();
	let class = setup.define_class(
		ClassSchema::new("item", AtomKind::Node, [ClassField::new("value", Type::Int)])?
			.with_schema_mode(ClassSchemaMode::Schemafull),
	)?;
	let key = setup.intern_property("value")?;
	let first: RecordId = "item:a".parse()?;
	let second: RecordId = "item:b".parse()?;
	setup.create_atom(first.clone(), PropertyMap::from_iter([(key, 1.into())]))?;
	setup.create_atom(second.clone(), PropertyMap::from_iter([(key, 2.into())]))?;
	let graph = setup.define_graph("scope", GraphKind::Metagraph)?;
	setup.attach_node(graph.id(), &first, None, None)?;
	setup.commit()?;
	let mut setup = store.begin();
	let feed = setup.configure_changefeed(
		ChangefeedSource::Class(class),
		ChangefeedConfig::from_nanoseconds(86_400_000_000_000)?
			.with_full(true)
			.with_include_original(true),
	)?;
	let query = || -> FerrosGrynnResult<LiveQuery> {
		Ok(LiveQuery::new(LiveSource::Record(first.clone()))
			.with_projection(LiveProjection::Value(LiveExpr::field("value")?)))
	};
	let active = setup.register_live(query()?, LiveOptions::default())?;
	let stopped = setup.register_live(query()?, LiveOptions::default())?;
	setup.commit()?;
	Ok(Fixture {
		store,
		class,
		graph,
		key,
		first,
		second,
		active,
		stopped,
		feed,
	})
}

/// A close occupying the provisional stamp remains unchanged at END; data,
/// topology, source batches and reactive values receive the fresh final stamp.
#[test]
fn live_import_close_final_stamp_and_whole_commit_images() -> FerrosGrynnResult<()> {
	let f = fixture()?;
	let before = f.store.inner.current_root();
	let mut no_op = f.store.begin();
	no_op.set_base_property(&f.first, f.key, 1.into())?;
	no_op.close_live(f.active)?;
	let mut staged = f.store.begin();
	staged.set_base_property(&f.second, f.key, 9.into())?;
	staged.close_live(f.active)?;
	let binding = {
		let mut tx = f.store.begin();
		let binding = tx.define_live_binding(
			LiveExpr::Select(Box::new(LiveQuery::new(LiveSource::Class(f.class)).with_projection(
				LiveProjection::Value(LiveExpr::Call {
					name: "sum".into(),
					arguments: vec![LiveExpr::field("value")?],
				}),
			))),
			Object::new(),
			LiveBindingOptions::default(),
		)?;
		tx.commit()?;
		binding
	};
	let base = f.store.inner.current_root();
	let mut close_stamp = 0;
	f.store.import(
		ImportOptions {
			changefeed: ImportChangefeedMode::On,
			segment_size: 1,
			..ImportOptions::default()
		},
		|session| {
			session.apply_item(|tx| tx.set_base_property(&f.first, f.key, 3.into()))?;
			assert!(no_op.commit().is_err(), "FULL no-op is not a control publication");
			assert!(staged.commit().is_err(), "staged DML cannot bypass the import lease");
			let mut blocked = f.store.begin();
			assert!(blocked.set_base_property(&f.first, f.key, 8.into()).is_err());
			assert!(
				blocked
					.configure_changefeed(
						ChangefeedSource::Class(f.class),
						ChangefeedConfig::from_nanoseconds(1_000_000_000)?
					)
					.is_err()
			);
			assert!(
				blocked
					.register_live(
						LiveQuery::new(LiveSource::Class(f.class)),
						LiveOptions::default()
					)
					.is_err()
			);
			assert!(blocked.replace_live_binding_value(binding, 8.into()).is_err());
			let mut read_only = f.store.begin_with(TransactionOptions::new(
				crate::datastore::IsolationLevel::Snapshot,
				TransactionAccess::ReadOnly,
			));
			assert!(read_only.close_live(f.active).is_err());
			assert!(
				f.store
					.inner
					.run_foreground_maintenance(
						crate::datastore::MaintenanceJobKind::Analyze,
						crate::datastore::MaintenanceTarget::Database,
						crate::datastore::MaintenanceOptions::default(),
						Some(crate::datastore::MaintenanceAnalyzeMode::Full)
					)
					.is_err(),
				"maintenance publication remains leased"
			);
			let mut control = f.store.begin();
			control.close_live(f.stopped)?;
			close_stamp = control.commit()?.commit_ts();
			assert_eq!(close_stamp, base.commit_ts + 1);
			assert_eq!(f.store.begin().live_binding_value(binding)?, Value::from(3_i64));
			session.apply_item(|tx| {
				tx.set_base_property(&f.first, f.key, 5.into())?;
				tx.attach_node(f.graph.id(), &f.second, None, None)
			})?;
			Ok(())
		},
	)?;
	let root = f.store.inner.current_root();
	assert_eq!(root.commit_ts, close_stamp + 1);
	assert_eq!(root.version(ConflictKey::LiveDefinition(f.stopped)), close_stamp);
	assert_eq!(root.live_catalog().queries[&f.stopped].state, LiveState::Closed);
	assert_eq!(root.live_catalog().queries[&f.stopped].published_at, close_stamp);
	assert_eq!(before.live_catalog().queries[&f.stopped].state, LiveState::Active);
	assert_eq!(f.store.begin().live_binding_value(binding)?, Value::from(7_i64));
	assert_eq!(root.live_catalog().bindings[&binding].value_at, root.commit_ts);
	assert_eq!(root.graphs[&f.graph.id()].tensor_revision, root.commit_ts);
	assert_eq!(root.statistics.base().analyzed_revision, root.commit_ts);
	let source = root
		.change_history()
		.get(&(f.feed, root.commit_ts))
		.ok_or_else(|| FerrosGrynnError::invalid_argument("missing final import source group"))?;
	assert_eq!(source.net.versionstamp().get(), root.commit_ts);
	assert_eq!(source.full.as_ref().map(|batch| batch.versionstamp().get()), Some(root.commit_ts));
	assert!(source.full.as_ref().is_some_and(|batch| batch.entries().len() >= 2));
	let bytes = root
		.live_history()
		.get(&(f.active, root.commit_ts))
		.ok_or_else(|| FerrosGrynnError::invalid_argument("missing final computed group"))?;
	let LivePayload::Batch(batch) = crate::storage::live_history::decode(bytes, 8 * 1024 * 1024)?
	else {
		return Err(FerrosGrynnError::invalid_argument("expected computed batch"));
	};
	assert_eq!(batch.versionstamp().get(), root.commit_ts);
	assert_eq!(batch.committed_at(), source.net.committed_at());
	assert_eq!(batch.events()[0].before(), Some(&Value::from(1)));
	assert_eq!(batch.events()[0].after(), Some(&Value::from(5)));
	assert!(!root.live_catalog().prepared.iter().any(|(id, _)| *id == f.stopped));
	assert!(
		!root.live_history().keys().any(|(id, stamp)| *id == f.stopped && *stamp > base.commit_ts)
	);
	Ok(())
}

/// OFF emits honest history loss while retaining the current volatile net
/// result.
#[test]
fn live_import_off_gap_and_volatile_current_result() -> FerrosGrynnResult<()> {
	let f = fixture()?;
	f.store.import(
		ImportOptions {
			segment_size: 1,
			..ImportOptions::default()
		},
		|session| {
			session.apply_item(|tx| tx.set_base_property(&f.first, f.key, 4.into()))?;
			session.apply_item(|tx| tx.set_base_property(&f.first, f.key, 6.into()))?;
			Ok(())
		},
	)?;
	let root = f.store.inner.current_root();
	let bytes = root
		.live_history()
		.get(&(f.active, root.commit_ts))
		.ok_or_else(|| FerrosGrynnError::invalid_argument("missing import suppression control"))?;
	let LivePayload::Gap(gap) = crate::storage::live_history::decode(bytes, 8 * 1024 * 1024)?
	else {
		return Err(FerrosGrynnError::invalid_argument(
			"suppressed import must not persist a Batch",
		));
	};
	assert_eq!(gap.reason(), crate::live::LiveGapReason::ImportSuppressed);
	assert_eq!(gap.from().map(|stamp| stamp.get()), Some(root.commit_ts));
	let payload = root
		.live_catalog()
		.prepared
		.iter()
		.find(|(id, _)| *id == f.active)
		.ok_or_else(|| FerrosGrynnError::invalid_argument("missing current net result"))?;
	let LivePayload::Batch(batch) = payload.1.as_ref() else {
		return Err(FerrosGrynnError::invalid_argument("expected volatile batch"));
	};
	assert_eq!(batch.events()[0].after(), Some(&Value::from(6)));
	Ok(())
}

/// Caller failure discards primary work without undoing an earlier
/// independently committed close.
#[test]
fn live_import_failed_end_preserves_control_commit() -> FerrosGrynnResult<()> {
	let f = fixture()?;
	let mut stamp = 0;
	let result = f.store.import(
		ImportOptions {
			segment_size: 1,
			..ImportOptions::default()
		},
		|session| {
			session.apply_item(|tx| tx.set_base_property(&f.first, f.key, 8.into()))?;
			let mut close = f.store.begin();
			close.close_live(f.stopped)?;
			stamp = close.commit()?.commit_ts();
			Err(FerrosGrynnError::invalid_argument("caller rejected final import"))
		},
	);
	assert!(result.is_err());
	assert_eq!(f.store.inner.current_root().commit_ts, stamp);
	assert_eq!(f.store.begin().live_info(f.stopped)?.state(), LiveState::Closed);
	assert_eq!(
		f.store.begin().atom(&f.first)?.and_then(|atom| atom.properties().get(f.key).cloned()),
		Some(Value::from(1))
	);
	Ok(())
}

/// Final schema validation fails before preparing any LIVE result;
/// independently committed control metadata remains visible.
#[test]
fn live_import_final_validation_failure_keeps_close() -> FerrosGrynnResult<()> {
	let f = fixture()?;
	let mut stamp = 0;
	let result = f.store.import(
		ImportOptions {
			segment_size: 1,
			full_validation: true,
			..ImportOptions::default()
		},
		|session| {
			session.apply_item(|tx| {
				tx.set_base_property(&f.first, f.key, Value::from("wrong declared type"))
			})?;
			let mut close = f.store.begin();
			close.close_live(f.stopped)?;
			stamp = close.commit()?.commit_ts();
			Ok(())
		},
	);
	assert!(result.is_err());
	assert!(stamp > 0, "the accepted item reached the control commit");
	assert_eq!(f.store.inner.current_root().commit_ts, stamp);
	assert_eq!(f.store.begin().live_info(f.stopped)?.state(), LiveState::Closed);
	assert_eq!(
		f.store.begin().atom(&f.first)?.and_then(|atom| atom.properties().get(f.key).cloned()),
		Some(Value::from(1))
	);
	Ok(())
}

/// An active binding fault at END pauses its dependent query while accepted
/// source images and primary data publish at the same final stamp.
#[test]
fn live_import_evaluation_fault_keeps_primary_and_history() -> FerrosGrynnResult<()> {
	let f = fixture()?;
	let mut setup = f.store.begin();
	let selected = LiveExpr::Select(Box::new(
		LiveQuery::new(LiveSource::Record(f.first.clone()))
			.with_projection(LiveProjection::Value(LiveExpr::field("value")?)),
	));
	let binding = setup.define_live_binding(
		LiveExpr::Literal(10.into()).binary(LiveBinaryOperator::Divide, selected),
		Object::new(),
		LiveBindingOptions::default(),
	)?;
	let query = setup.register_live(
		LiveQuery::new(LiveSource::Record(f.first.clone()))
			.with_projection(LiveProjection::Value(LiveExpr::Binding(binding))),
		LiveOptions::default(),
	)?;
	setup.commit()?;
	let mut old = f.store.begin();
	f.store.import(
		ImportOptions {
			changefeed: ImportChangefeedMode::On,
			segment_size: 1,
			..ImportOptions::default()
		},
		|session| {
			session.apply_item(|tx| tx.set_base_property(&f.first, f.key, 2.into()))?;
			session.apply_item(|tx| tx.set_base_property(&f.first, f.key, 0.into()))?;
			Ok(())
		},
	)?;
	let root = f.store.inner.current_root();
	let mut current = f.store.begin();
	assert_eq!(current.live_binding_info(binding)?.state(), LiveState::Paused);
	assert_eq!(current.live_info(query)?.state(), LiveState::Paused);
	assert_eq!(old.live_info(query)?.state(), LiveState::Active);
	assert_eq!(
		current.atom(&f.first)?.and_then(|atom| atom.properties().get(f.key).cloned()),
		Some(Value::from(0))
	);
	let bytes = root
		.live_history()
		.get(&(query, root.commit_ts))
		.ok_or_else(|| FerrosGrynnError::invalid_argument("missing import fault envelope"))?;
	let LivePayload::Fault {
		gap,
		..
	} = crate::storage::live_history::decode(bytes, 8 * 1024 * 1024)?
	else {
		return Err(FerrosGrynnError::invalid_argument("expected import evaluation fault"));
	};
	assert_eq!(gap.reason(), crate::live::LiveGapReason::EvaluationFault);
	assert_eq!(gap.through().map(|stamp| stamp.get()), Some(root.commit_ts));
	let source = &root.change_history()[&(f.feed, root.commit_ts)];
	assert_eq!(source.net.entries().len(), 1);
	assert_eq!(source.full.as_ref().map(|batch| batch.entries().len()), Some(2));
	assert_eq!(source.net.versionstamp().get(), root.commit_ts);
	Ok(())
}

/// An aggregate scan exceeding its active work cap does not reject accepted
/// import records or replace the fault with a successful partial result.
#[test]
fn live_import_work_limit_pauses_without_partial_result() -> FerrosGrynnResult<()> {
	let f = fixture()?;
	let mut setup = f.store.begin();
	let query = setup.register_live(
		LiveQuery::new(LiveSource::Class(f.class))
			.with_filter(
				LiveExpr::field("id")?
					.binary(LiveBinaryOperator::NotEqual, LiveExpr::Literal(Value::Null)),
			)
			.with_projection(LiveProjection::Value(LiveExpr::Call {
				name: "count".into(),
				arguments: Vec::new(),
			})),
		LiveOptions::default().with_budgets(256 * 1024 * 1024, 500)?,
	)?;
	setup.commit()?;
	f.store.import(
		ImportOptions {
			changefeed: ImportChangefeedMode::On,
			segment_size: 32,
			..ImportOptions::default()
		},
		|session| {
			for ordinal in 0..1024 {
				let record: RecordId = format!("item:import-{ordinal}").parse()?;
				session.apply_item(|tx| {
					tx.create_atom(record, PropertyMap::from_iter([(f.key, 1.into())]))
						.map(|_atom| ())
				})?;
			}
			Ok(())
		},
	)?;
	let root = f.store.inner.current_root();
	assert_eq!(f.store.begin().live_info(query)?.state(), LiveState::Paused);
	assert!(f.store.begin().atom(&"item:import-1023".parse()?)?.is_some());
	let bytes = root
		.live_history()
		.get(&(query, root.commit_ts))
		.ok_or_else(|| FerrosGrynnError::invalid_argument("missing import budget fault"))?;
	assert!(matches!(
		crate::storage::live_history::decode(bytes, 8 * 1024 * 1024)?,
		LivePayload::Fault { .. }
	));
	assert_eq!(root.change_history()[&(f.feed, root.commit_ts)].net.entries().len(), 1024);
	Ok(())
}
