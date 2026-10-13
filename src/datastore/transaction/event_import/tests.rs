//! Import handlers, private invocations and source history share item rollback.

use crate::{
	datastore::{
		ClassSchema,
		Datastore,
		EventAsyncOptions,
		EventDefinition,
		EventExecution,
		EventTarget,
		ImportChangefeedMode,
		ImportEventsMode,
		ImportItemOutcome,
		ImportOptions,
		ImportViolationPolicy,
	},
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	graph::AtomKind,
	values::{Object, PropertyMap, Value},
};

/// Definitions precede imported data; ASYNC output remains private until END.
fn fixture(rejected_handler: bool) -> FerrosGrynnResult<(Datastore, crate::values::PropertyKeyId)> {
	let store = Datastore::in_memory();
	let mut seed = store.begin();
	let class = seed.define_class(ClassSchema::new("item", AtomKind::Node, [])?)?;
	let audit = seed.define_class(ClassSchema::new("audit", AtomKind::Node, [])?)?;
	let policy =
		crate::changefeed::ChangefeedConfig::from_nanoseconds(86_400_000_000_000)?.with_full(true);
	seed.configure_changefeed(crate::changefeed::ChangefeedSource::Class(class), policy)?;
	seed.configure_changefeed(crate::changefeed::ChangefeedSource::Class(audit), policy)?;
	let value = seed.intern_property("value")?;
	seed.define_event(
		EventDefinition::new("queued", EventTarget::Class(class), "$event = 'CREATE'", "1")?
			.with_execution(EventExecution::Async(EventAsyncOptions::default())),
	)?;
	if rejected_handler {
		seed.create_atom("audit:fixed".parse()?, PropertyMap::new())?;
		seed.define_event(EventDefinition::new(
			"sync",
			EventTarget::Class(class),
			"$after.value = 2",
			"{ CREATE audit:partial; CREATE audit:fixed; }",
		)?)?;
	} else {
		seed.define_event(EventDefinition::new(
			"sync",
			EventTarget::Class(class),
			"$event = 'CREATE'",
			"{ UPSERT audit:sync SET value = $after.value; }",
		)?)?;
	}
	seed.commit()?;
	Ok((store, value))
}

#[test]
fn event_import_off_on_are_independent_of_changefeed_and_keep_jobs_private() -> FerrosGrynnResult<()>
{
	for events in [ImportEventsMode::Off, ImportEventsMode::On] {
		for changefeed in [ImportChangefeedMode::Off, ImportChangefeedMode::On] {
			let (store, value) = fixture(false)?;
			store.import(
				ImportOptions {
					events,
					changefeed,
					segment_size: 1,
					..ImportOptions::default()
				},
				|session| {
					for (name, number) in [("a", 1), ("b", 3)] {
						session.apply_item(|tx| {
							tx.create_atom(
								format!("item:{name}").parse()?,
								PropertyMap::from_iter([(value, Value::from(number))]),
							)
						})?;
						assert!(store.begin().atom(&format!("item:{name}").parse()?)?.is_none());
						assert!(store.jobs()?.is_empty());
					}
					Ok(())
				},
			)?;
			assert!(store.begin().atom(&"item:a".parse()?)?.is_some());
			assert_eq!(
				store
					.begin()
					.atom(&"audit:sync".parse()?)?
					.and_then(|atom| atom.properties().get(value).cloned()),
				if events == ImportEventsMode::On {
					Some(Value::from(3))
				} else {
					None
				}
			);
			assert_eq!(
				store.jobs()?.len(),
				if events == ImportEventsMode::On {
					2
				} else {
					0
				}
			);
			let root = store.inner.current_root();
			let captured = root
				.change_history()
				.iter()
				.filter(|((_, stamp), group)| {
					*stamp == root.commit_ts
						&& (!group.net.entries().is_empty()
							|| group.full.iter().any(|batch| !batch.entries().is_empty()))
				})
				.count();
			assert_eq!(
				captured,
				if changefeed == ImportChangefeedMode::Off {
					0
				} else if events == ImportEventsMode::On {
					2
				} else {
					1
				}
			);
		}
	}
	Ok(())
}

#[test]
fn event_import_skip_and_quarantine_restore_handler_effects_and_spool() -> FerrosGrynnResult<()> {
	for policy in [ImportViolationPolicy::Skip, ImportViolationPolicy::Quarantine] {
		let (store, value) = fixture(true)?;
		let report = store.import(
			ImportOptions {
				events: ImportEventsMode::On,
				segment_size: 1,
				on_violation: policy,
				reject_limit: Some(1),
				..ImportOptions::default()
			},
			|session| {
				let rejected = session.apply_item_with_payload(Value::from("bad"), |tx| {
					tx.create_atom(
						"item:bad".parse()?,
						PropertyMap::from_iter([(value, Value::from(1))]),
					)?;
					tx.set_base_property(&"item:bad".parse()?, value, Value::from(2))
				})?;
				assert!(matches!(rejected, ImportItemOutcome::Skipped(_)));
				session.apply_item_with_payload(Value::from("good"), |tx| {
					tx.create_atom(
						"item:good".parse()?,
						PropertyMap::from_iter([(value, Value::from(3))]),
					)
				})?;
				assert!(store.jobs()?.is_empty());
				Ok(())
			},
		)?;
		assert_eq!(report.skipped_items(), 1);
		assert!(store.begin().atom(&"item:bad".parse()?)?.is_none());
		assert!(store.begin().atom(&"audit:partial".parse()?)?.is_none());
		assert!(store.begin().atom(&"item:good".parse()?)?.is_some());
		assert_eq!(store.jobs()?.len(), 1);
		if let Some(id) = report.quarantine() {
			let mut reader = store.open_quarantine(id)?;
			assert_eq!(
				reader.read_next()?.map(|item| item.payload().clone()),
				Some(Value::from("bad"))
			);
			assert!(reader.read_next()?.is_none());
		}
	}
	Ok(())
}

#[test]
fn event_import_abort_publishes_no_primary_handler_or_invocation_effects() -> FerrosGrynnResult<()>
{
	let (store, value) = fixture(false)?;
	let result = store.import(
		ImportOptions {
			events: ImportEventsMode::On,
			segment_size: 1,
			..ImportOptions::default()
		},
		|session| {
			session.apply_item(|tx| {
				tx.create_atom("item:a".parse()?, PropertyMap::from_iter([(value, Value::from(1))]))
			})?;
			Err(FerrosGrynnError::invalid_argument("abort after private segment"))
		},
	);
	assert!(result.is_err());
	assert!(store.begin().atom(&"item:a".parse()?)?.is_none());
	assert!(store.begin().atom(&"audit:sync".parse()?)?.is_none());
	assert!(store.jobs()?.is_empty());
	Ok(())
}

#[test]
fn event_import_gql_options_share_the_sdk_dispatch_path() -> FerrosGrynnResult<()> {
	let (store, _value) = fixture(false)?;
	for result in store.execute(
		"BEGIN IMPORT WITH (EVENTS = ON, CHANGEFEED = OFF, SEGMENT SIZE = 1); CREATE item:a \
		 CONTENT { value: 4 }; END IMPORT;",
		&Object::new(),
	)? {
		let _value = result.result?;
	}
	assert!(store.begin().atom(&"audit:sync".parse()?)?.is_some());
	assert_eq!(store.jobs()?.len(), 1);
	Ok(())
}

#[test]
fn event_import_preserves_each_sdk_change_before_the_next_mutation() -> FerrosGrynnResult<()> {
	let (store, value) = fixture(false)?;
	store.import(
		ImportOptions {
			events: ImportEventsMode::On,
			..ImportOptions::default()
		},
		|session| {
			session.apply_item(|tx| {
				let record: crate::values::RecordId = "item:a".parse()?;
				tx.create_atom(record.clone(), PropertyMap::from_iter([(value, Value::from(1))]))?;
				tx.set_base_property(&record, value, Value::from(2))?;
				Ok(())
			})?;
			session.apply_item(|tx| {
				let record: crate::values::RecordId = "item:ephemeral".parse()?;
				tx.create_atom(record.clone(), PropertyMap::from_iter([(value, Value::from(3))]))?;
				tx.delete_atom(&record)
			})?;
			Ok(())
		},
	)?;
	assert_eq!(
		store
			.begin()
			.atom(&"audit:sync".parse()?)?
			.and_then(|atom| atom.properties().get(value).cloned()),
		Some(Value::from(3))
	);
	assert!(store.begin().atom(&"item:ephemeral".parse()?)?.is_none());
	assert_eq!(store.jobs()?.len(), 2);
	Ok(())
}

#[test]
fn event_import_gql_handlers_keep_the_callers_loop_budget() -> FerrosGrynnResult<()> {
	let (store, _value) = fixture(false)?;
	let mut tx = store.begin();
	let class = tx
		.class("item")?
		.and_then(|class| class.id())
		.ok_or_else(|| FerrosGrynnError::invalid_argument("missing item class"))?;
	tx.define_event(EventDefinition::new(
		"bounded",
		EventTarget::Class(class),
		"true",
		"{ FOR $n IN 0..256 { 1; }; }",
	)?)?;
	tx.commit()?;
	let options = crate::gql::QueryExecutionOptions {
		max_loop_iterations: 128,
		..crate::gql::QueryExecutionOptions::default()
	};
	let result = store.session().execute_with_options(
		"BEGIN IMPORT WITH (EVENTS = ON); CREATE item:a; END IMPORT;",
		&Object::new(),
		options,
	)?;
	assert!(result[0].result.is_err(), "{result:?}");
	assert!(store.begin().atom(&"item:a".parse()?)?.is_none());
	assert!(store.begin().atom(&"audit:sync".parse()?)?.is_none());
	assert!(store.jobs()?.is_empty());
	Ok(())
}

#[test]
fn event_import_gql_iteration_dispatches_each_statement() -> FerrosGrynnResult<()> {
	let (store, value) = fixture(false)?;
	for result in store.execute(
		"BEGIN IMPORT WITH (EVENTS = ON, CHANGEFEED = ON); FOR $n IN [1] { CREATE item:a CONTENT \
		 { value: 1 }; UPDATE item:a SET value = 2; CREATE item:b CONTENT { value: 3 }; DELETE \
		 item:b; }; END IMPORT;",
		&Object::new(),
	)? {
		result.result?;
	}
	assert_eq!(
		store
			.begin()
			.atom(&"audit:sync".parse()?)?
			.and_then(|atom| atom.properties().get(value).cloned()),
		Some(Value::from(3))
	);
	assert_eq!(store.jobs()?.len(), 2);
	let root = store.inner.current_root();
	for ((_feed, stamp), group) in root.change_history().iter() {
		if *stamp == root.commit_ts {
			let batch = group
				.full
				.as_ref()
				.ok_or_else(|| FerrosGrynnError::invalid_argument("missing FULL batch"))?;
			assert!(
				batch.entries().iter().all(|entry| entry.operation() == 0),
				"one Changefeed ordinal per import iteration"
			);
		}
	}
	Ok(())
}

#[test]
fn event_import_failed_end_publishes_no_accepted_handler_or_job() -> FerrosGrynnResult<()> {
	let (store, value) = fixture(false)?;
	let mut seed = store.begin();
	seed.define_field(
		"item",
		crate::datastore::ClassField::new("value", crate::values::Type::Int),
	)?;
	seed.set_class_schema_mode("item", crate::datastore::ClassSchemaMode::Schemafull)?;
	seed.commit()?;
	let before = store.inner.current_root().commit_ts;
	let mut accepted = false;
	let result = store.import(
		ImportOptions {
			events: ImportEventsMode::On,
			full_validation: true,
			segment_size: 1,
			..ImportOptions::default()
		},
		|session| {
			session.apply_item(|tx| {
				tx.create_atom(
					"item:bad".parse()?,
					PropertyMap::from_iter([(value, Value::from("wrong type"))]),
				)
			})?;
			accepted = true;
			Ok(())
		},
	);
	assert!(accepted, "the accepted item must reach END validation");
	assert!(result.is_err());
	assert_eq!(store.inner.current_root().commit_ts, before);
	assert!(store.begin().atom(&"item:bad".parse()?)?.is_none());
	assert!(store.begin().atom(&"audit:sync".parse()?)?.is_none());
	assert!(store.jobs()?.is_empty());
	Ok(())
}

#[test]
fn event_import_end_preserves_independent_job_cancellation() -> FerrosGrynnResult<()> {
	let (store, value) = fixture(false)?;
	let mut source = store.begin();
	source.create_atom("item:old".parse()?, PropertyMap::from_iter([(value, 1.into())]))?;
	source.commit()?;
	let old = store
		.jobs()?
		.first()
		.ok_or_else(|| FerrosGrynnError::invalid_argument("missing job"))?
		.id();
	store.import(
		ImportOptions {
			events: ImportEventsMode::On,
			segment_size: 1,
			..ImportOptions::default()
		},
		|session| {
			session.apply_item(|tx| {
				tx.create_atom("item:new".parse()?, PropertyMap::from_iter([(value, 2.into())]))
			})?;
			assert_eq!(store.jobs()?.len(), 1);
			assert_eq!(
				store.kill_job(old)?.state(),
				crate::datastore::MaintenanceJobState::Cancelled
			);
			Ok(())
		},
	)?;
	let jobs = store.jobs()?;
	assert_eq!(jobs.len(), 2);
	assert_eq!(
		store.job(old)?.map(|job| job.state()),
		Some(crate::datastore::MaintenanceJobState::Cancelled)
	);
	assert_eq!(store.inner.current_root().event_outbox().pending_count, 1);
	Ok(())
}

#[test]
fn event_import_private_spool_counts_capacity_and_rejects_the_next_source() -> FerrosGrynnResult<()>
{
	let (store, _value) = fixture(false)?;
	let result = store.import(
		ImportOptions {
			events: ImportEventsMode::On,
			segment_size: 1,
			..ImportOptions::default()
		},
		|session| {
			session.apply_item(|tx| {
				let event = tx
					.working
					.schema
					.events
					.values()
					.find(|event| matches!(event.execution(), EventExecution::Async(_)))
					.cloned()
					.ok_or_else(|| FerrosGrynnError::invalid_argument("missing ASYNC event"))?;
				let context =
					[Value::from("CREATE"), Value::Null, Value::Null, Value::Null, Value::Null];
				let payload: std::sync::Arc<[u8]> =
					crate::storage::event_outbox::encode_payload(&event, &context, &[], 1 << 20)?
						.into();
				for _ in 0..10_000 {
					tx.spool_event_invocation(std::sync::Arc::new(
						crate::datastore::EventInvocation::queued(
							&event,
							crate::datastore::EventCause::next(None, event.execution())?,
							std::sync::Arc::clone(&payload),
							jiff::Timestamp::now().as_nanosecond(),
						)?,
					))?;
				}
				let spool = tx
					.import_events
					.as_ref()
					.ok_or_else(|| FerrosGrynnError::invalid_argument("missing spool"))?;
				assert_eq!(spool.invocations.len(), 10_000);
				assert!(tx.write_set_bytes() >= spool.retained_bytes());
				Ok(())
			})?;
			session.apply_item(|tx| {
				assert!(tx.create_atom("item:overflow".parse()?, PropertyMap::new()).is_err());
				assert!(tx.atom(&"item:overflow".parse()?)?.is_none());
				let spool = tx
					.import_events
					.as_ref()
					.ok_or_else(|| FerrosGrynnError::invalid_argument("missing spool"))?;
				assert_eq!(spool.invocations.len(), 10_000);
				assert!(tx.write_set_bytes() >= spool.retained_bytes());
				Ok(())
			})?;
			assert!(store.jobs()?.is_empty());
			Err(FerrosGrynnError::invalid_argument("abort private fixture"))
		},
	);
	assert!(result.is_err());
	assert!(store.jobs()?.is_empty());
	Ok(())
}
