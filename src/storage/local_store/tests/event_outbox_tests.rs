//! Whole source/outbox WAL publication, strict reopen and completion markers.

use std::{
	fs,
	io::{Read, Seek, SeekFrom, Write},
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
	use crate::test_assertions::{AssertComparison as _, AssertCondition as _};

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
	(&(invocation.state), &(EventInvocationState::Running)).assert_equal();
	(&(invocation.attempts), &(1)).assert_equal();
	(invocation.payload.is_some()).assert_true();
	(root.resolve_atom(&"item:a".parse()?).is_some()).assert_true();
	// This low-level fixture bypasses transaction preparation; exact counter
	// maps must be loaded before inserting the synchronous effect directly.
	local.hydrate_statistics(&mut root)?;
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
	(&(root.event_outbox().invocations[&id].state), &(EventInvocationState::Completed))
		.assert_equal();
	(root.event_outbox().invocations[&id].payload.is_none()).assert_true();
	(&(root.event_outbox().pending_count), &(0)).assert_equal();
	(root.resolve_atom(&"item:effect".parse()?).is_some()).assert_true();
	Ok(())
}

/// Verifies event outbox local sync reopens running and completion.
#[test]
fn event_outbox_local_sync_reopens_running_and_completion() -> FerrosGrynnResult<()> {
	reopen_attempt_and_completion(IoMode::Sync)
}

/// Verifies event outbox local native reopens running and completion.
#[cfg(feature = "native-async-io")]
#[test]
fn event_outbox_local_native_reopens_running_and_completion() -> FerrosGrynnResult<()> {
	reopen_attempt_and_completion(IoMode::NativeAsync)
}

/// Verifies event outbox near pending byte limit keeps checkpoint lifecycle
/// writable.
#[test]
fn event_outbox_near_pending_byte_limit_keeps_checkpoint_lifecycle_writable()
-> FerrosGrynnResult<()> {
	checkpoint_lifecycle(8, false, 0)
}

/// Verifies event outbox pending payload and diagnostic limits keep checkpoint
/// writable.
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
	use crate::test_assertions::{AssertComparison as _, AssertCondition as _};
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
	(root.event_outbox().pending_bytes > 63 * 1024 * 1024).assert_true();
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
		(&(root.event_outbox_segments().len()), &(1)).assert_equal();
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
	(&(recovered.event_outbox().pending_count), &(count - 2)).assert_equal();
	(&(recovered.event_outbox().invocations[&ids[0]].state), &(EventInvocationState::Completed))
		.assert_equal();
	(&(recovered.event_outbox().invocations[&ids[1]].state), &(EventInvocationState::Cancelled))
		.assert_equal();
	Ok(())
}

/// Verifies event outbox before wal barrier publishes neither source nor
/// payload.
#[test]
fn event_outbox_before_wal_barrier_publishes_neither_source_nor_payload() {
	use crate::test_assertions::AssertResult as _;

	let directory = TestDirectory::new();
	let options = LocalOptions::default();
	let store = Datastore::create_local(&directory.0, options).assert_ok();
	let event = accepted(&store).assert_ok();
	{
		let _failure = GenerationFailureGuard::set(GenerationFailurePoint::BeforeWalCommit);
		assert!(enqueue(&store, &event, true).is_err());
	}
	assert!(store.inner.current_root().event_outbox().invocations.is_empty());
	drop(store);
	let reopened = Datastore::open_local(&directory.0, options).assert_ok();
	assert!(reopened.begin().atom(&"item:a".parse().assert_ok()).assert_ok().is_none());
	assert!(reopened.inner.current_root().event_outbox().invocations.is_empty());
}

/// END is the sole publication point for data, synchronous effects and the
/// accepted asynchronous contexts retained across private seals.
fn import_reopen(
	mode: IoMode,
	failure: Option<crate::storage::wal::CommitFailurePoint>,
) -> FerrosGrynnResult<()> {
	use crate::test_assertions::{AssertComparison as _, AssertCondition as _};

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
					(store.jobs()?.is_empty()).assert_true();
				}
				Ok(())
			},
		)
	};
	(&(result.is_ok()), &(failure.is_none())).assert_equal();
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
	(&(root.resolve_atom(&"item:a".parse()?).is_some()), &(published)).assert_equal();
	(&(root.resolve_atom(&"item:b".parse()?).is_some()), &(published)).assert_equal();
	(&(root.resolve_atom(&"audit:last".parse()?).is_some()), &(published)).assert_equal();
	(
		&(root.event_outbox().pending_count),
		&(if published {
			2
		} else {
			0
		}),
	)
		.assert_equal();
	for invocation in root.event_outbox().invocations.values() {
		(&(invocation.state), &(EventInvocationState::Queued)).assert_equal();
		(&(invocation.attempts), &(0)).assert_equal();
		(&(invocation.accepted_at), &(root.commit_ts)).assert_equal();
		(&(invocation.created_at_ns), &(root.committed_at_ns)).assert_equal();
	}
	Ok(())
}

/// Verifies event import local sync reopens private segments.
#[test]
fn event_import_local_sync_reopens_private_segments() -> FerrosGrynnResult<()> {
	import_reopen(IoMode::Sync, None)
}

/// Verifies event import local native reopens private segments.
#[cfg(feature = "native-async-io")]
#[test]
fn event_import_local_native_reopens_private_segments() -> FerrosGrynnResult<()> {
	import_reopen(IoMode::NativeAsync, None)
}

/// Verifies event import local uncertain end recovers primary effects and
/// spool.
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

/// Verifies event outbox uncertain source and completion recover one identity.
#[test]
fn event_outbox_uncertain_source_and_completion_recover_one_identity() {
	use crate::{
		storage::wal::{CommitFailureGuard, CommitFailurePoint},
		test_assertions::AssertResult as _,
	};
	for completion in [false, true] {
		for point in [
			CommitFailurePoint::BeforeAppend,
			CommitFailurePoint::AfterAppend,
			CommitFailurePoint::AfterFlush,
			CommitFailurePoint::AfterSync,
		] {
			let directory = TestDirectory::new();
			let options = LocalOptions::default();
			let store = Datastore::create_local(&directory.0, options).assert_ok();
			store.inner.maintenance.shutdown();
			let event = accepted(&store).assert_ok();
			let (candidate, id) = if completion {
				let id = enqueue(&store, &event, true).assert_ok();
				let mut claim = store.begin();
				claim
					.transition_event_invocation(id, 0, EventInvocationState::Running, 5, None)
					.assert_ok();
				claim.commit().assert_ok();
				let mut effects = store.begin();
				effects
					.create_atom("item:effect".parse().assert_ok(), PropertyMap::new())
					.assert_ok();
				effects
					.transition_event_invocation(id, 1, EventInvocationState::Completed, 5, None)
					.assert_ok();
				(effects, id)
			} else {
				prepare_invocation(&store, &event, true).assert_ok()
			};
			let old_stamp = store.inner.current_root().commit_ts;
			{
				let _failure = CommitFailureGuard::set(point);
				assert!(candidate.commit().is_err());
			}
			assert_eq!(store.inner.current_root().commit_ts, old_stamp);
			let (retry, _retry_id) = prepare_invocation(&store, &event, false).assert_ok();
			assert!(
				retry.commit().is_err(),
				"uncertain writer remains fenced before any repeated effects"
			);
			drop(store);
			let validated = validate_local_options(options).assert_ok();
			let (_local, root) = LocalStore::open(
				&directory.0,
				validated.pool_pages,
				validated.io_mode,
				validated.checkpoint_page_threshold,
				options.checkpoint.interval,
			)
			.assert_ok();
			let published = point != CommitFailurePoint::BeforeAppend;
			assert_eq!(root.commit_ts, old_stamp + u64::from(published));
			let record = if completion {
				"item:effect"
			} else {
				"item:a"
			};
			assert_eq!(root.resolve_atom(&record.parse().assert_ok()).is_some(), published);
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
}

/// Verifies event outbox pending page corruption is fatal on reopen.
#[test]
fn event_outbox_pending_page_corruption_is_fatal_on_reopen() {
	use crate::test_assertions::AssertResult as _;

	let directory = TestDirectory::new();
	let options = LocalOptions::default();
	let store = Datastore::create_local(&directory.0, options).assert_ok();
	let event = accepted(&store).assert_ok();
	enqueue(&store, &event, false).assert_ok();
	store.checkpoint(CheckpointMode::Restart).assert_ok();
	drop(store);
	let owner = fs::read_dir(directory.0.join("events"))
		.map_err(|error| crate::gql_status::FerrosGrynnError::invalid_argument(error.to_string()))
		.assert_ok()
		.next()
		.ok_or_else(|| crate::gql_status::FerrosGrynnError::invalid_argument("owner missing"))
		.assert_ok()
		.map_err(|error| crate::gql_status::FerrosGrynnError::invalid_argument(error.to_string()))
		.assert_ok()
		.path();
	let path = fs::read_dir(owner)
		.map_err(|error| crate::gql_status::FerrosGrynnError::invalid_argument(error.to_string()))
		.assert_ok()
		.next()
		.ok_or_else(|| crate::gql_status::FerrosGrynnError::invalid_argument("segment missing"))
		.assert_ok()
		.map_err(|error| crate::gql_status::FerrosGrynnError::invalid_argument(error.to_string()))
		.assert_ok()
		.path();
	let corrupt = || -> std::io::Result<()> {
		let mut file = fs::OpenOptions::new().read(true).write(true).open(path)?;
		let offset = PAGE_SIZE as u64 + 128;
		file.seek(SeekFrom::Start(offset))?;
		let mut byte = [0];
		file.read_exact(&mut byte)?;
		// A constant overwrite can leave a random UUID byte unchanged.
		byte[0] ^= 1;
		file.seek(SeekFrom::Start(offset))?;
		file.write_all(&byte)?;
		file.sync_all()
	};
	corrupt()
		.map_err(|error| crate::gql_status::FerrosGrynnError::invalid_argument(error.to_string()))
		.assert_ok();
	assert!(Datastore::open_local(&directory.0, options).is_err());
}

/// Verifies event outbox commented transitive sources reopen and execute after
/// remove.
#[test]
fn event_outbox_commented_transitive_sources_reopen_and_execute_after_remove() {
	use crate::test_assertions::AssertResult as _;

	let directory = TestDirectory::new();
	let options = LocalOptions::default();
	let store = Datastore::create_local(&directory.0, options).assert_ok();
	store.inner.maintenance.shutdown();
	let mut seed = store.begin();
	let class =
		seed.define_class(ClassSchema::new("item", AtomKind::Node, []).assert_ok()).assert_ok();
	seed.define_class(ClassSchema::new("audit", AtomKind::Node, []).assert_ok()).assert_ok();
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
			)
			.assert_ok()
			.with_comment("c".repeat(4096)),
		)
		.assert_ok();
	}
	let snapshot = seed.query_snapshot().assert_ok();
	for index in 0..300 {
		functions.push(
			seed.query_schema_functions(&snapshot, "fn", &format!("f{index}"))
				.assert_ok()
				.first()
				.and_then(|function| function.id())
				.ok_or_else(|| {
					crate::gql_status::FerrosGrynnError::invalid_argument(
						"fixture function identity absent",
					)
				})
				.assert_ok(),
		);
	}
	drop(snapshot);
	let calls = (0..300).map(|index| format!("fn::f{index}()")).collect::<Vec<_>>().join(", ");
	let event = seed
		.define_event(
			EventDefinition::new(
				"commented",
				EventTarget::Class(class),
				"true",
				format!("{{ LET $ignored = [{calls}]; CREATE audit:done; }}"),
			)
			.assert_ok()
			.with_execution(EventExecution::Async(EventAsyncOptions::default())),
		)
		.assert_ok();
	seed.commit().assert_ok();
	let mut source = store.begin();
	source.create_atom("item:a".parse().assert_ok(), PropertyMap::new()).assert_ok();
	source.remove_event(event).assert_ok();
	for function in functions {
		source.remove_function(function).assert_ok();
	}
	source.commit().assert_ok();
	let id = *store
		.inner
		.current_root()
		.event_outbox()
		.invocations
		.keys()
		.next()
		.ok_or_else(|| {
			crate::gql_status::FerrosGrynnError::invalid_argument("fixture invocation absent")
		})
		.assert_ok();
	drop(store);
	let reopened = Datastore::open_local(&directory.0, options).assert_ok();
	let started = std::time::Instant::now();
	loop {
		let job = reopened
			.job(id)
			.assert_ok()
			.ok_or_else(|| {
				crate::gql_status::FerrosGrynnError::invalid_argument("recovered invocation absent")
			})
			.assert_ok();
		if job.state() == crate::datastore::MaintenanceJobState::Completed {
			break;
		}
		if job.state() == crate::datastore::MaintenanceJobState::Failed
			|| started.elapsed() > std::time::Duration::from_secs(10)
		{
			return {
				let test_result: FerrosGrynnResult<()> =
					Err(crate::gql_status::FerrosGrynnError::invalid_argument(format!(
						"recovered commented handler did not complete: {job:?}"
					)));
				test_result.assert_ok();
			};
		}
		std::thread::sleep(std::time::Duration::from_millis(10));
	}
	assert!(reopened.begin().atom(&"audit:done".parse().assert_ok()).assert_ok().is_some());
	assert_eq!(
		reopened.job(id).assert_ok().and_then(|job| job.event().map(|event| event.attempts())),
		Some(1)
	);
}
