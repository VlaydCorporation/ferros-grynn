//! Whole source/outbox WAL publication, strict reopen and completion markers.

use std::{
	fs,
	io::{Seek, SeekFrom, Write},
};

use super::{GenerationFailureGuard, GenerationFailurePoint, TestDirectory};
use crate::{
	datastore::{
		CheckpointMode,
		ClassSchema,
		Datastore,
		EventAsyncOptions,
		EventCause,
		EventDefinition,
		EventExecution,
		EventId,
		EventInvocationState,
		EventTarget,
		IoMode,
		JobId,
		LocalOptions,
		validate_local_options,
	},
	gql::{ExecutionControl, QueryExecutionOptions},
	gql_status::FerrosGrynnResult,
	graph::AtomKind,
	storage::{LocalStore, event_outbox::encode_payload, page::PAGE_SIZE},
	values::{PropertyMap, Value},
};

/// Accepted descriptors remain valid even after their schema owner retires.
fn accepted(store: &Datastore) -> FerrosGrynnResult<EventDefinition> {
	let mut seed = store.begin();
	let class = seed.define_class(ClassSchema::new("item", AtomKind::Node, [])?)?;
	seed.commit()?;
	let mut event = EventDefinition::new("a", EventTarget::Class(class), "true", "1")?
		.with_execution(EventExecution::Async(EventAsyncOptions::default()));
	event.id = Some(EventId::from_storage(1)?);
	Ok(event)
}

/// Exercises the internal accepted-payload boundary before enabling dispatch.
fn enqueue(store: &Datastore, event: &EventDefinition, create: bool) -> FerrosGrynnResult<JobId> {
	let (write, id) = prepare_invocation(store, event, create)?;
	write.commit()?;
	Ok(id)
}

/// Leaves source and accepted identity private until the caller's tested WAL
/// boundary.
fn prepare_invocation(
	store: &Datastore,
	event: &EventDefinition,
	create: bool,
) -> FerrosGrynnResult<(crate::datastore::Transaction, JobId)> {
	let context = [Value::from("CREATE"), Value::Null, Value::Null, Value::Null, Value::Null];
	let payload = encode_payload(event, &context, &[], 1 << 20)?;
	let mut write = store.begin();
	if create {
		write.create_atom("item:a".parse()?, PropertyMap::new())?;
	}
	let id = write.enqueue_event_payload(
		event,
		EventCause::next(None, event.execution())?,
		payload,
		&ExecutionControl::new(&QueryExecutionOptions::default(), None),
	)?;
	Ok((write, id))
}

/// Shared assertions cover both local I/O modes without combining backend
/// targets.
fn reopen_attempt_and_completion(mode: IoMode) -> FerrosGrynnResult<()> {
	let directory = TestDirectory::new();
	let options = LocalOptions {
		io_mode: mode,
		..LocalOptions::default()
	};
	let store = Datastore::create_local(&directory.0, options)?;
	store.inner.maintenance.shutdown();
	let event = accepted(&store)?;
	let id = enqueue(&store, &event, true)?;
	let mut start = store.begin();
	start.transition_event_invocation(id, 0, EventInvocationState::Running, 5, None)?;
	start.commit()?;
	drop(store);
	let validated = validate_local_options(options)?;
	let (local, mut root) = LocalStore::open(
		&directory.0,
		validated.pool_pages,
		validated.io_mode,
		validated.checkpoint_page_threshold,
		options.checkpoint.interval,
	)?;
	let invocation = &root.event_outbox().invocations[&id];
	assert_eq!(invocation.state, EventInvocationState::Running);
	assert_eq!(invocation.attempts, 1);
	assert!(invocation.payload.is_some());
	assert!(root.resolve_atom(&"item:a".parse()?).is_some());
	let before = root.clone();
	let class = root.schema.class("item").ok_or_else(|| {
		crate::gql_status::FerrosGrynnError::invalid_argument("fixture class absent")
	})?;
	root.insert_atom(&class, "item:effect".parse()?, PropertyMap::new())?;
	root.commit_ts += 1;
	root.advance_commit_time(jiff::Timestamp::now().as_nanosecond());
	let stamp = root.commit_ts;
	let time = root.committed_at_ns;
	std::sync::Arc::make_mut(root.event_outbox_mut()).transition(
		id,
		1,
		EventInvocationState::Completed,
		5,
		None,
		stamp,
	)?;
	std::sync::Arc::make_mut(root.event_outbox_mut()).stamp_time(id, stamp, time)?;
	local.persist_root(
		&root,
		Some(&before),
		None,
		false,
		false,
		&mut |_analyzer, _text| {
			Err(crate::gql_status::FerrosGrynnError::invalid_argument(
				"unexpected fixture analyzer",
			))
		},
		&mut |_id, _version, _left, _right| {
			Err(crate::gql_status::FerrosGrynnError::invalid_argument(
				"unexpected fixture distance",
			))
		},
	)?;
	local.checkpoint(CheckpointMode::Restart)?;
	drop(local);
	let (_local, root) = LocalStore::open(
		&directory.0,
		validated.pool_pages,
		validated.io_mode,
		validated.checkpoint_page_threshold,
		options.checkpoint.interval,
	)?;
	assert_eq!(root.event_outbox().invocations[&id].state, EventInvocationState::Completed);
	assert!(root.event_outbox().invocations[&id].payload.is_none());
	assert_eq!(root.event_outbox().pending_count, 0);
	assert!(root.resolve_atom(&"item:effect".parse()?).is_some());
	Ok(())
}

#[test]
fn event_outbox_local_sync_reopens_running_and_completion() -> FerrosGrynnResult<()> {
	reopen_attempt_and_completion(IoMode::Sync)
}

#[cfg(feature = "native-async-io")]
#[test]
fn event_outbox_local_native_reopens_running_and_completion() -> FerrosGrynnResult<()> {
	reopen_attempt_and_completion(IoMode::NativeAsync)
}

#[test]
fn event_outbox_near_pending_byte_limit_keeps_checkpoint_lifecycle_writable()
-> FerrosGrynnResult<()> {
	checkpoint_lifecycle(8, false, 0)
}

#[test]
fn event_outbox_pending_payload_and_diagnostic_limits_keep_checkpoint_writable()
-> FerrosGrynnResult<()> {
	checkpoint_lifecycle(10_000, true, 1000)
}

/// Both the payload limit and the permitted diagnostic envelope use real WAL
/// checkpoints.
fn checkpoint_lifecycle(
	count: usize,
	diagnostics: bool,
	terminal_count: usize,
) -> FerrosGrynnResult<()> {
	use super::super::event_outbox::CompactionTargetGuard;
	let _compact = CompactionTargetGuard::set(128 * 1024);
	let directory = TestDirectory::new();
	let options = LocalOptions::default();
	let validated = validate_local_options(options)?;
	let (local, mut root) = LocalStore::create(
		&directory.0,
		validated.pool_pages,
		validated.io_mode,
		validated.checkpoint_page_threshold,
		options.checkpoint.interval,
	)?;
	let mut event = EventDefinition::new(
		"retired",
		EventTarget::Class(crate::datastore::ClassId::from_storage(1)?),
		"true",
		"1",
	)?
	.with_execution(EventExecution::Async(EventAsyncOptions::default()));
	event.id = Some(EventId::from_storage(2)?);
	let mut arguments = [
		Value::from("CREATE"),
		Value::Null,
		Value::Null,
		Value::Bytes(crate::values::Bytes::new(Vec::new())),
		Value::Null,
	];
	let fixed = encode_payload(&event, &arguments, &[], 128 * 1024 * 1024)?.len();
	let payload_size = (64 * 1024 * 1024 / count).saturating_sub(fixed + 32);
	arguments[3] = Value::Bytes(crate::values::Bytes::new(vec![7; payload_size]));
	let payload: std::sync::Arc<[u8]> =
		encode_payload(&event, &arguments, &[], 128 * 1024 * 1024)?.into();
	drop(arguments);
	root.commit_ts = 1;
	root.advance_identity_clocks(3, 1);
	root.advance_commit_time(jiff::Timestamp::now().as_nanosecond());
	let message = crate::values::Text::from("d".repeat(4096));
	for _ in 0..terminal_count {
		let invocation = crate::datastore::EventInvocation::queued(
			&event,
			EventCause::next(None, event.execution())?,
			std::sync::Arc::from([1_u8]),
			5,
		)?;
		let id = invocation.id;
		let outbox = std::sync::Arc::make_mut(root.event_outbox_mut());
		outbox.enqueue(std::sync::Arc::new(invocation), 1)?;
		outbox.transition(id, 0, EventInvocationState::Running, 5, None, 1)?;
		outbox.transition(id, 1, EventInvocationState::Completed, 5, Some(message.clone()), 1)?;
		let time = root.committed_at_ns;
		std::sync::Arc::make_mut(root.event_outbox_mut()).stamp_time(id, 1, time)?;
	}
	let mut ids = Vec::new();
	for _ in 0..count {
		let mut invocation = crate::datastore::EventInvocation::queued(
			&event,
			EventCause::next(None, event.execution())?,
			std::sync::Arc::clone(&payload),
			5,
		)?;
		if diagnostics {
			invocation.diagnostic = Some(message.clone());
		}
		let id = invocation.id;
		std::sync::Arc::make_mut(root.event_outbox_mut())
			.enqueue(std::sync::Arc::new(invocation), 1)?;
		let time = root.committed_at_ns;
		std::sync::Arc::make_mut(root.event_outbox_mut()).stamp_time(id, 1, time)?;
		ids.push(id);
	}
	assert!(root.event_outbox().pending_bytes > 63 * 1024 * 1024);
	let publish = |root: &crate::datastore::DatabaseRoot| {
		local.persist_root(
			root,
			None,
			None,
			true,
			false,
			&mut |_analyzer, _text| {
				Err(crate::gql_status::FerrosGrynnError::invalid_argument("unexpected analyzer"))
			},
			&mut |_id, _version, _left, _right| {
				Err(crate::gql_status::FerrosGrynnError::invalid_argument("unexpected distance"))
			},
		)
	};
	publish(&root)?;
	root.set_event_outbox_segments(local.event_outbox_segments());
	for (id, state, attempts) in [
		(ids[0], EventInvocationState::Running, 0),
		(ids[0], EventInvocationState::Completed, 1),
		(ids[1], EventInvocationState::Cancelled, 0),
	] {
		root.commit_ts += 1;
		root.advance_commit_time(jiff::Timestamp::now().as_nanosecond());
		let stamp = root.commit_ts;
		let time = root.committed_at_ns;
		std::sync::Arc::make_mut(root.event_outbox_mut())
			.transition(id, attempts, state, 5, None, stamp)?;
		std::sync::Arc::make_mut(root.event_outbox_mut()).stamp_time(id, stamp, time)?;
		publish(&root)?;
		root.set_event_outbox_segments(local.event_outbox_segments());
		assert_eq!(root.event_outbox_segments().len(), 1);
	}
	local.checkpoint(CheckpointMode::Restart)?;
	drop(local);
	let (_local, recovered) = LocalStore::open(
		&directory.0,
		validated.pool_pages,
		validated.io_mode,
		validated.checkpoint_page_threshold,
		options.checkpoint.interval,
	)?;
	assert_eq!(recovered.event_outbox().pending_count, count - 2);
	assert_eq!(
		recovered.event_outbox().invocations[&ids[0]].state,
		EventInvocationState::Completed
	);
	assert_eq!(
		recovered.event_outbox().invocations[&ids[1]].state,
		EventInvocationState::Cancelled
	);
	Ok(())
}

#[test]
fn event_outbox_before_wal_barrier_publishes_neither_source_nor_payload() -> FerrosGrynnResult<()> {
	let directory = TestDirectory::new();
	let options = LocalOptions::default();
	let store = Datastore::create_local(&directory.0, options)?;
	let event = accepted(&store)?;
	{
		let _failure = GenerationFailureGuard::set(GenerationFailurePoint::BeforeWalCommit);
		assert!(enqueue(&store, &event, true).is_err());
	}
	assert!(store.inner.current_root().event_outbox().invocations.is_empty());
	drop(store);
	let reopened = Datastore::open_local(&directory.0, options)?;
	assert!(reopened.begin().atom(&"item:a".parse()?)?.is_none());
	assert!(reopened.inner.current_root().event_outbox().invocations.is_empty());
	Ok(())
}

/// END is the sole publication point for data, synchronous effects and the
/// accepted asynchronous contexts retained across private seals.
fn import_reopen(
	mode: IoMode,
	failure: Option<crate::storage::wal::CommitFailurePoint>,
) -> FerrosGrynnResult<()> {
	let directory = TestDirectory::new();
	let options = LocalOptions {
		io_mode: mode,
		..LocalOptions::default()
	};
	let store = Datastore::create_local(&directory.0, options)?;
	store.inner.maintenance.shutdown();
	let mut seed = store.begin();
	let class = seed.define_class(ClassSchema::new("item", AtomKind::Node, [])?)?;
	seed.define_class(ClassSchema::new("audit", AtomKind::Node, [])?)?;
	let key = seed.intern_property("value")?;
	seed.define_event(EventDefinition::new(
		"sync",
		EventTarget::Class(class),
		"true",
		"{ UPSERT audit:last SET value = $after.value; }",
	)?)?;
	seed.define_event(
		EventDefinition::new("queued", EventTarget::Class(class), "true", "1")?
			.with_execution(EventExecution::Async(EventAsyncOptions::default())),
	)?;
	seed.commit()?;
	let result = {
		let _failure = failure.map(crate::storage::wal::CommitFailureGuard::set);
		store.import(
			crate::datastore::ImportOptions {
				events: crate::datastore::ImportEventsMode::On,
				segment_size: 1,
				..crate::datastore::ImportOptions::default()
			},
			|session| {
				for (name, value) in [("a", 1), ("b", 2)] {
					session.apply_item(|tx| {
						tx.create_atom(
							format!("item:{name}").parse()?,
							PropertyMap::from_iter([(key, value.into())]),
						)
					})?;
					assert!(store.jobs()?.is_empty());
				}
				Ok(())
			},
		)
	};
	assert_eq!(result.is_ok(), failure.is_none());
	drop(store);
	let validated = validate_local_options(options)?;
	let (_local, root) = LocalStore::open(
		&directory.0,
		validated.pool_pages,
		validated.io_mode,
		validated.checkpoint_page_threshold,
		options.checkpoint.interval,
	)?;
	let published = failure != Some(crate::storage::wal::CommitFailurePoint::BeforeAppend);
	assert_eq!(root.resolve_atom(&"item:a".parse()?).is_some(), published);
	assert_eq!(root.resolve_atom(&"item:b".parse()?).is_some(), published);
	assert_eq!(root.resolve_atom(&"audit:last".parse()?).is_some(), published);
	assert_eq!(
		root.event_outbox().pending_count,
		if published {
			2
		} else {
			0
		}
	);
	for invocation in root.event_outbox().invocations.values() {
		assert_eq!(invocation.state, EventInvocationState::Queued);
		assert_eq!(invocation.attempts, 0);
		assert_eq!(invocation.accepted_at, root.commit_ts);
		assert_eq!(invocation.created_at_ns, root.committed_at_ns);
	}
	Ok(())
}

#[test]
fn event_import_local_sync_reopens_private_segments() -> FerrosGrynnResult<()> {
	import_reopen(IoMode::Sync, None)
}

#[cfg(feature = "native-async-io")]
#[test]
fn event_import_local_native_reopens_private_segments() -> FerrosGrynnResult<()> {
	import_reopen(IoMode::NativeAsync, None)
}

#[test]
fn event_import_local_uncertain_end_recovers_primary_effects_and_spool() -> FerrosGrynnResult<()> {
	use crate::storage::wal::CommitFailurePoint;
	for point in [
		CommitFailurePoint::BeforeAppend,
		CommitFailurePoint::AfterAppend,
		CommitFailurePoint::AfterFlush,
		CommitFailurePoint::AfterSync,
	] {
		import_reopen(IoMode::Sync, Some(point))?;
	}
	Ok(())
}

#[test]
fn event_outbox_uncertain_source_and_completion_recover_one_identity() -> FerrosGrynnResult<()> {
	use crate::storage::wal::{CommitFailureGuard, CommitFailurePoint};
	for completion in [false, true] {
		for point in [
			CommitFailurePoint::BeforeAppend,
			CommitFailurePoint::AfterAppend,
			CommitFailurePoint::AfterFlush,
			CommitFailurePoint::AfterSync,
		] {
			let directory = TestDirectory::new();
			let options = LocalOptions::default();
			let store = Datastore::create_local(&directory.0, options)?;
			store.inner.maintenance.shutdown();
			let event = accepted(&store)?;
			let (candidate, id) = if completion {
				let id = enqueue(&store, &event, true)?;
				let mut claim = store.begin();
				claim.transition_event_invocation(id, 0, EventInvocationState::Running, 5, None)?;
				claim.commit()?;
				let mut effects = store.begin();
				effects.create_atom("item:effect".parse()?, PropertyMap::new())?;
				effects.transition_event_invocation(
					id,
					1,
					EventInvocationState::Completed,
					5,
					None,
				)?;
				(effects, id)
			} else {
				prepare_invocation(&store, &event, true)?
			};
			let old_stamp = store.inner.current_root().commit_ts;
			{
				let _failure = CommitFailureGuard::set(point);
				assert!(candidate.commit().is_err());
			}
			assert_eq!(store.inner.current_root().commit_ts, old_stamp);
			let (retry, _retry_id) = prepare_invocation(&store, &event, false)?;
			assert!(
				retry.commit().is_err(),
				"uncertain writer remains fenced before any repeated effects"
			);
			drop(store);
			let validated = validate_local_options(options)?;
			let (_local, root) = LocalStore::open(
				&directory.0,
				validated.pool_pages,
				validated.io_mode,
				validated.checkpoint_page_threshold,
				options.checkpoint.interval,
			)?;
			let published = point != CommitFailurePoint::BeforeAppend;
			assert_eq!(root.commit_ts, old_stamp + u64::from(published));
			let record = if completion {
				"item:effect"
			} else {
				"item:a"
			};
			assert_eq!(root.resolve_atom(&record.parse()?).is_some(), published);
			if completion {
				let invocation = &root.event_outbox().invocations[&id];
				assert_eq!(invocation.attempts, 1);
				assert_eq!(
					invocation.state,
					if published {
						EventInvocationState::Completed
					} else {
						EventInvocationState::Running
					}
				);
				assert_eq!(invocation.payload.is_some(), !published);
			} else {
				assert_eq!(root.event_outbox().invocations.contains_key(&id), published);
				assert_eq!(root.event_outbox().invocations.len(), usize::from(published));
			}
		}
	}
	Ok(())
}

#[test]
fn event_outbox_pending_page_corruption_is_fatal_on_reopen() -> FerrosGrynnResult<()> {
	let directory = TestDirectory::new();
	let options = LocalOptions::default();
	let store = Datastore::create_local(&directory.0, options)?;
	let event = accepted(&store)?;
	enqueue(&store, &event, false)?;
	store.checkpoint(CheckpointMode::Restart)?;
	drop(store);
	let owner = fs::read_dir(directory.0.join("events"))
		.map_err(|error| crate::gql_status::FerrosGrynnError::invalid_argument(error.to_string()))?
		.next()
		.ok_or_else(|| crate::gql_status::FerrosGrynnError::invalid_argument("owner missing"))?
		.map_err(|error| crate::gql_status::FerrosGrynnError::invalid_argument(error.to_string()))?
		.path();
	let path = fs::read_dir(owner)
		.map_err(|error| crate::gql_status::FerrosGrynnError::invalid_argument(error.to_string()))?
		.next()
		.ok_or_else(|| crate::gql_status::FerrosGrynnError::invalid_argument("segment missing"))?
		.map_err(|error| crate::gql_status::FerrosGrynnError::invalid_argument(error.to_string()))?
		.path();
	let corrupt = || -> std::io::Result<()> {
		let mut file = fs::OpenOptions::new().write(true).open(path)?;
		file.seek(SeekFrom::Start(PAGE_SIZE as u64 + 128))?;
		file.write_all(&[255])?;
		file.sync_all()
	};
	corrupt().map_err(|error| {
		crate::gql_status::FerrosGrynnError::invalid_argument(error.to_string())
	})?;
	assert!(Datastore::open_local(&directory.0, options).is_err());
	Ok(())
}

#[test]
fn event_outbox_commented_transitive_sources_reopen_and_execute_after_remove()
-> FerrosGrynnResult<()> {
	let directory = TestDirectory::new();
	let options = LocalOptions::default();
	let store = Datastore::create_local(&directory.0, options)?;
	store.inner.maintenance.shutdown();
	let mut seed = store.begin();
	let class = seed.define_class(ClassSchema::new("item", AtomKind::Node, [])?)?;
	seed.define_class(ClassSchema::new("audit", AtomKind::Node, [])?)?;
	let mut functions = Vec::new();
	for index in 0..300 {
		seed.define_function(
			crate::datastore::SchemaFunction::new(
				"fn",
				format!("f{index}"),
				Vec::new(),
				crate::values::Type::Int,
				crate::datastore::FunctionEffect::Pure,
				false,
				None,
				"{ RETURN 1; }",
			)?
			.with_comment("c".repeat(4096)),
		)?;
	}
	let snapshot = seed.query_snapshot()?;
	for index in 0..300 {
		functions.push(
			seed.query_schema_functions(&snapshot, "fn", &format!("f{index}"))?
				.first()
				.and_then(|function| function.id())
				.ok_or_else(|| {
					crate::gql_status::FerrosGrynnError::invalid_argument(
						"fixture function identity absent",
					)
				})?,
		);
	}
	drop(snapshot);
	let calls = (0..300).map(|index| format!("fn::f{index}()")).collect::<Vec<_>>().join(", ");
	let event = seed.define_event(
		EventDefinition::new(
			"commented",
			EventTarget::Class(class),
			"true",
			format!("{{ LET $ignored = [{calls}]; CREATE audit:done; }}"),
		)?
		.with_execution(EventExecution::Async(EventAsyncOptions::default())),
	)?;
	seed.commit()?;
	let mut source = store.begin();
	source.create_atom("item:a".parse()?, PropertyMap::new())?;
	source.remove_event(event)?;
	for function in functions {
		source.remove_function(function)?;
	}
	source.commit()?;
	let id =
		*store.inner.current_root().event_outbox().invocations.keys().next().ok_or_else(|| {
			crate::gql_status::FerrosGrynnError::invalid_argument("fixture invocation absent")
		})?;
	drop(store);
	let reopened = Datastore::open_local(&directory.0, options)?;
	let started = std::time::Instant::now();
	loop {
		let job = reopened.job(id)?.ok_or_else(|| {
			crate::gql_status::FerrosGrynnError::invalid_argument("recovered invocation absent")
		})?;
		if job.state() == crate::datastore::MaintenanceJobState::Completed {
			break;
		}
		if job.state() == crate::datastore::MaintenanceJobState::Failed
			|| started.elapsed() > std::time::Duration::from_secs(10)
		{
			return Err(crate::gql_status::FerrosGrynnError::invalid_argument(format!(
				"recovered commented handler did not complete: {job:?}"
			)));
		}
		std::thread::sleep(std::time::Duration::from_millis(10));
	}
	assert!(reopened.begin().atom(&"audit:done".parse()?)?.is_some());
	assert_eq!(
		reopened.job(id)?.and_then(|job| job.event().map(|event| event.attempts())),
		Some(1)
	);
	Ok(())
}
