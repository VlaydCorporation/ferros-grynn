//! Accepted invocation attempts, independent of the non-authoritative job
//! ledger.

use std::{
	num::NonZeroUsize,
	sync::Arc,
	time::{Duration, SystemTime, UNIX_EPOCH},
};

use super::{
	Datastore,
	EventCause,
	EventInvocation,
	EventInvocationState,
	EventJobInfo,
	EventTarget,
	JobId,
	MaintenanceJobInfo,
	MaintenanceJobKind,
	MaintenanceJobState,
	MaintenanceTarget,
	StoreInner,
};
use crate::{
	gql::{ExecutionControl, QueryCancellation, QueryExecutionOptions},
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	values::Text,
};

/// Captures the existing setting in milliseconds, matching the technical
/// specification.
pub(super) fn configured_interval() -> Duration {
	interval_from_value(std::env::var("FG_ASYNC_EVENT_PROCESSING_INTERVAL").ok().as_deref())
}

/// Invalid or absent values retain the normative five-second default.
fn interval_from_value(value: Option<&str>) -> Duration {
	Duration::from_millis(
		value
			.and_then(|value| value.parse::<u64>().ok())
			.filter(|value| *value > 0)
			.unwrap_or(5000),
	)
}

impl StoreInner {
	/// Uses the next due time while retaining the existing one-second idle
	/// cleanup tick.
	pub(super) fn event_wait_duration(&self, now: i128) -> Duration {
		let root = self.current_root();
		let due = root
			.event_outbox()
			.invocations
			.values()
			.filter(|invocation| invocation.state == EventInvocationState::Queued)
			.map(|invocation| invocation.due_at_ns)
			.min();
		let nanos = due
			.filter(|due| *due > now)
			.map_or(1_000_000_000, |due| (due - now).min(1_000_000_000) as u64);
		Duration::from_nanos(nanos)
	}

	/// Projects root metadata without retaining accepted contexts in INFO
	/// results.
	pub(super) fn event_job_info(
		&self,
		invocation: &EventInvocation,
	) -> FerrosGrynnResult<MaintenanceJobInfo> {
		let options = self.transaction_limits().maintenance;
		let target = match invocation.target {
			EventTarget::Class(class) => MaintenanceTarget::BaseClass(class),
			EventTarget::Graph(graph) => MaintenanceTarget::Graph(graph),
		};
		let mut job = MaintenanceJobInfo::new(
			MaintenanceJobKind::EventInvocation,
			target,
			invocation.accepted_at,
			options,
			None,
		);
		job.id = invocation.id;
		job.event = Some(EventJobInfo {
			id: invocation.event,
			version: invocation.version,
			attempts: invocation.attempts,
			depth: invocation.depth,
			retries: invocation.retries,
			max_depth: invocation.ceiling,
		});
		job.state = match invocation.state {
			EventInvocationState::Queued => MaintenanceJobState::Queued,
			EventInvocationState::Running => MaintenanceJobState::Running,
			EventInvocationState::Completed => MaintenanceJobState::Completed,
			EventInvocationState::Cancelled => MaintenanceJobState::Cancelled,
			EventInvocationState::Failed => MaintenanceJobState::Failed,
		};
		job.max_parallelism = NonZeroUsize::MIN;
		job.restart_count = u32::from(invocation.attempts.saturating_sub(1));
		job.diagnostic = invocation.diagnostic.as_ref().map(|text| text.as_str().to_owned());
		job.created_at = system_time(invocation.created_at_ns)?;
		job.updated_at = system_time(invocation.updated_at_ns)?;
		Ok(job)
	}

	/// The single maintenance worker selects one due identity in stable order.
	pub(super) fn run_due_event(self: &Arc<Self>, now: i128) -> FerrosGrynnResult<bool> {
		if self.maintenance.is_stopping()
			|| self.import_owner.load(std::sync::atomic::Ordering::Acquire) != 0
		{
			return Ok(false);
		}
		let root = self.current_root();
		let selected = root
			.event_outbox()
			.invocations
			.values()
			.find(|invocation| {
				invocation.state == EventInvocationState::Running
					|| invocation.state == EventInvocationState::Queued
						&& invocation.due_at_ns <= now
			})
			.cloned();
		drop(root);
		let Some(invocation) = selected else {
			return Ok(false);
		};
		let token = self.maintenance.begin_event_attempt(invocation.id);
		let cancellation = QueryCancellation::from_token(token);
		let result = self.run_event_attempt(&invocation, cancellation);
		self.maintenance.end_event_attempt(invocation.id);
		result.map(|()| true)
	}

	/// Running claims precede user code; recovery resumes the same started
	/// attempt.
	fn run_event_attempt(
		self: &Arc<Self>,
		invocation: &EventInvocation,
		cancellation: QueryCancellation,
	) -> FerrosGrynnResult<()> {
		let store = Datastore::background(Arc::clone(self));
		let mut options = QueryExecutionOptions::default();
		options.cancellation = Some(cancellation);
		options.barrier_memory_bytes = self
			.transaction_limits()
			.maintenance
			.max_memory_bytes
			.min(options.barrier_memory_bytes);
		let control = ExecutionControl::new(&options, None);
		control.check()?;
		let mut claim = store.begin();
		claim.transition_event_invocation(
			invocation.id,
			invocation.attempts,
			EventInvocationState::Running,
			invocation.due_at_ns,
			invocation.diagnostic.clone(),
		)?;
		claim.commit_controlled(None, &|| control.check())?;
		let current = self.current_root();
		let running = current
			.event_outbox()
			.invocations
			.get(&invocation.id)
			.ok_or_else(|| FerrosGrynnError::maintenance_job_not_found(invocation.id.to_string()))?
			.clone();
		if running.state != EventInvocationState::Running {
			return Ok(());
		}
		drop(current);
		let result = Self::execute_event_effects(&store, &running, &control);
		match result {
			Ok(()) => Ok(()),
			Err(_error)
				if self.maintenance.is_stopping()
					|| self.import_owner.load(std::sync::atomic::Ordering::Acquire) != 0 =>
			{
				Ok(())
			}
			Err(error) => {
				// KILL and a successfully published marker win over a stale
				// attempt failure.
				let root = self.current_root();
				if !root.event_outbox().invocations.get(&running.id).is_some_and(|invocation| {
					invocation.state == EventInvocationState::Running
						&& invocation.attempts == running.attempts
				}) {
					return Ok(());
				}
				drop(root);
				let state = if running.attempts <= u16::from(running.retries) {
					EventInvocationState::Queued
				} else {
					EventInvocationState::Failed
				};
				let due = jiff::Timestamp::now()
					.as_nanosecond()
					.checked_add(
						i128::try_from(self.event_processing_interval.as_nanos()).map_err(
							|error| {
								FerrosGrynnError::operation_overflow("EVENT retry interval")
									.with_technical_source(error)
							},
						)?,
					)
					.ok_or_else(|| FerrosGrynnError::operation_overflow("EVENT retry time"))?;
				let mut transition = store.begin();
				transition.transition_event_invocation(
					running.id,
					running.attempts,
					state,
					due,
					Some(diagnostic(&error)),
				)?;
				transition.commit()?;
				Ok(())
			}
		}
	}

	/// Historical context and sources use fresh data; completion shares the
	/// effects commit.
	fn execute_event_effects(
		store: &Datastore,
		invocation: &EventInvocation,
		control: &ExecutionControl,
	) -> FerrosGrynnResult<()> {
		control.check()?;
		let bytes = invocation
			.payload
			.as_ref()
			.ok_or_else(|| FerrosGrynnError::invalid_argument("running EVENT payload is absent"))?;
		let payload = crate::storage::event_outbox::decode_payload(
			bytes,
			control.barrier_memory_available(),
		)?;
		let mut account = control.barrier_account();
		if !account.reserve(payload.retained_bytes) {
			return Err(FerrosGrynnError::traversal_resource_limit_exceeded(
				"EVENT accepted payload",
				payload.retained_bytes as u64,
			));
		}
		let mut attempt = store.begin();
		attempt.run_accepted_event(
			&payload,
			EventCause {
				depth: u16::from(invocation.depth),
				ceiling: Some(invocation.ceiling),
			},
			control,
		)?;
		control.check()?;
		attempt.transition_event_invocation(
			invocation.id,
			invocation.attempts,
			EventInvocationState::Completed,
			invocation.due_at_ns,
			None,
		)?;
		attempt.commit_controlled(None, &|| control.check())?;
		Ok(())
	}
}

impl Datastore {
	/// Root cancellation and cooperative interruption share the same checked
	/// identity.
	pub(super) fn kill_event_job(&self, id: JobId) -> FerrosGrynnResult<MaintenanceJobInfo> {
		let mut transaction = self.begin();
		let root = self.inner.current_root();
		let invocation = root
			.event_outbox()
			.invocations
			.get(&id)
			.ok_or_else(|| FerrosGrynnError::maintenance_job_not_found(id.to_string()))?;
		if !invocation.state.is_pending() {
			return self.inner.event_job_info(invocation);
		}
		transaction.transition_event_invocation(
			id,
			invocation.attempts,
			EventInvocationState::Cancelled,
			invocation.due_at_ns,
			Some("cancelled explicitly".into()),
		)?;
		drop(root);
		transaction.commit()?;
		self.inner.maintenance.cancel_event_attempt(id);
		self.job(id)?.ok_or_else(|| FerrosGrynnError::maintenance_job_not_found(id.to_string()))
	}
}

/// Keeps UTF-8 diagnostics inside the byte ceiling used by the durable codec.
fn diagnostic(error: &FerrosGrynnError) -> Text {
	let mut output = String::new();
	for character in error.status().description().chars() {
		if output.len() + character.len_utf8() > 4096 {
			break;
		}
		output.push(if character == '\0' {
			' '
		} else {
			character
		});
	}
	output.into()
}

/// Checked signed UTC conversion avoids treating logical stamps as wall-clock
/// times.
fn system_time(nanoseconds: i128) -> FerrosGrynnResult<SystemTime> {
	let absolute = nanoseconds.unsigned_abs();
	let seconds = u64::try_from(absolute / 1_000_000_000).map_err(|error| {
		FerrosGrynnError::invalid_argument("EVENT timestamp exceeds platform domain")
			.with_technical_source(error)
	})?;
	let duration = Duration::new(seconds, (absolute % 1_000_000_000) as u32);
	if nanoseconds < 0 {
		UNIX_EPOCH.checked_sub(duration)
	} else {
		UNIX_EPOCH.checked_add(duration)
	}
	.ok_or_else(|| FerrosGrynnError::invalid_argument("EVENT timestamp exceeds platform domain"))
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::{
		datastore::{ClassSchema, EventAsyncOptions, EventDefinition},
		graph::AtomKind,
		values::{Object, PropertyMap, Value},
	};

	/// Sets the private clock policy before any handle or worker can observe
	/// it.
	fn fast_store() -> FerrosGrynnResult<(Datastore, EventTarget, crate::values::PropertyKeyId)> {
		let mut store = Datastore::in_memory();
		Arc::get_mut(&mut store.inner)
			.ok_or_else(|| FerrosGrynnError::invalid_argument("fixture owner shared"))?
			.event_processing_interval = Duration::from_millis(1);
		let mut seed = store.begin();
		let class = seed.define_class(ClassSchema::new("item", AtomKind::Node, [])?)?;
		seed.define_class(ClassSchema::new("audit", AtomKind::Node, [])?)?;
		let value = seed.intern_property("value")?;
		seed.commit()?;
		Ok((store, EventTarget::Class(class), value))
	}

	/// Polls authoritative metadata with a bounded deadline, never driving a
	/// second worker.
	fn wait_jobs(store: &Datastore, expected: usize) -> FerrosGrynnResult<Vec<MaintenanceJobInfo>> {
		let started = std::time::Instant::now();
		loop {
			let jobs =
				store.jobs()?.into_iter().filter(|job| job.event().is_some()).collect::<Vec<_>>();
			if jobs.len() == expected
				&& jobs.iter().all(|job| {
					matches!(
						job.state(),
						MaintenanceJobState::Completed
							| MaintenanceJobState::Cancelled
							| MaintenanceJobState::Failed
					)
				}) {
				return Ok(jobs);
			}
			if started.elapsed() > Duration::from_secs(10) {
				return Err(FerrosGrynnError::invalid_argument(format!(
					"EVENT jobs did not finish: {jobs:?}"
				)));
			}
			std::thread::sleep(Duration::from_millis(2));
		}
	}

	#[test]
	fn event_async_worker_success_and_retry_exhaustion_roll_back_effects() -> FerrosGrynnResult<()>
	{
		for retries in [0, 2] {
			let (store, target, _value) = fast_store()?;
			let mut define = store.begin();
			define.define_event(
				EventDefinition::new(
					"fail",
					target,
					"true",
					"{ CREATE audit:partial; THROW 'failure'; }",
				)?
				.with_execution(super::super::EventExecution::Async(
					EventAsyncOptions::new(retries, 3)?,
				)),
			)?;
			define.commit()?;
			let mut source = store.begin();
			source.create_atom("item:a".parse()?, PropertyMap::new())?;
			source.commit()?;
			let jobs = wait_jobs(&store, 1)?;
			assert_eq!(jobs[0].state(), MaintenanceJobState::Failed);
			assert_eq!(jobs[0].event().map(EventJobInfo::attempts), Some(u16::from(retries) + 1));
			assert_eq!(jobs[0].event().map(EventJobInfo::depth), Some(0));
			assert!(jobs[0].diagnostic().is_some_and(|message| message.contains("failure")));
			assert!(store.begin().atom(&"item:a".parse()?)?.is_some());
			assert!(store.begin().atom(&"audit:partial".parse()?)?.is_none());
			assert_eq!(store.inner.current_root().event_outbox().pending_bytes, 0);
		}
		let (store, target, value) = fast_store()?;
		let mut define = store.begin();
		define.define_event(
			EventDefinition::new(
				"ok",
				target,
				"true",
				"{ CREATE audit:done CONTENT { value: $after.value }; }",
			)?
			.with_execution(super::super::EventExecution::Async(EventAsyncOptions::default())),
		)?;
		define.commit()?;
		let mut source = store.begin();
		source.create_atom("item:a".parse()?, PropertyMap::from_iter([(value, Value::from(7))]))?;
		source.commit()?;
		assert_eq!(wait_jobs(&store, 1)?[0].state(), MaintenanceJobState::Completed);
		assert_eq!(
			store
				.begin()
				.atom(&"audit:done".parse()?)?
				.and_then(|atom| atom.properties().get(value).cloned()),
			Some(Value::from(7))
		);
		Ok(())
	}

	#[test]
	fn event_async_atomwalk_uses_frozen_qualified_context_and_common_relate()
	-> FerrosGrynnResult<()> {
		let (store, _target, _value) = fast_store()?;
		let mut seed = store.begin();
		seed.define_class(ClassSchema::new("link", AtomKind::Edge, [])?)?;
		let graph = seed.define_graph("scope", crate::graph::GraphKind::Graph)?;
		for record in ["item:a", "item:b"] {
			seed.create_atom(record.parse()?, PropertyMap::new())?;
			seed.attach_node(graph.id(), &record.parse()?, None, None)?;
		}
		seed.define_event(
			EventDefinition::new(
				"generate",
				EventTarget::Graph(graph.id()),
				"$event = 'RELATE' AND meta::id($value.edge) = 'base'",
				"{ RELATE ATOM \
				 <($value.endpoints.source[0])>~[link:vv]~><($value.endpoints.target[0])> IN \
				 GRAPH scope; RELATE ATOM \
				 <($value.endpoints.source[0])>~[link:ve]~><[$value.edge]> IN GRAPH scope; RELATE \
				 ATOM <[$value.edge]>~[link:ev]~><($value.endpoints.target[0])> IN GRAPH scope; \
				 RELATE ATOM <[$value.edge]>~[link:ee]~><[$value.edge]> IN GRAPH scope; }",
			)?
			.with_execution(super::super::EventExecution::Async(EventAsyncOptions::default())),
		)?;
		seed.commit()?;
		for result in store
			.execute("RELATE (item:a)-[link:base]->(item:b) IN GRAPH scope;", &Object::new())?
		{
			result.result?;
		}
		let jobs = wait_jobs(&store, 1)?;
		assert_eq!(jobs[0].state(), MaintenanceJobState::Completed);
		assert_eq!(jobs[0].event().map(EventJobInfo::attempts), Some(1));
		for record in ["link:base", "link:vv", "link:ve", "link:ev", "link:ee"] {
			assert!(store.begin().membership(graph.id(), &record.parse()?)?.is_some(), "{record}");
		}
		Ok(())
	}

	#[test]
	fn event_async_worker_maxdepth_zero_three_and_sixteen_preserve_ancestry()
	-> FerrosGrynnResult<()> {
		for maximum in [0_u8, 3, 16] {
			let (store, target, value) = fast_store()?;
			let mut define = store.begin();
			define.define_event(
				EventDefinition::new(
					"chain",
					target,
					"$after.value < 20",
					"{ UPDATE item:root SET value = $after.value + 1; }",
				)?
				.with_execution(super::super::EventExecution::Async(
					EventAsyncOptions::new(0, maximum)?,
				)),
			)?;
			define.commit()?;
			let mut source = store.begin();
			source.create_atom(
				"item:root".parse()?,
				PropertyMap::from_iter([(value, Value::from(0))]),
			)?;
			source.commit()?;
			let jobs = wait_jobs(&store, usize::from(maximum) + 1)?;
			assert_eq!(
				jobs.iter().filter(|job| job.state() == MaintenanceJobState::Failed).count(),
				1
			);
			let depths = jobs
				.iter()
				.filter_map(|job| job.event().map(EventJobInfo::depth))
				.collect::<Vec<_>>();
			assert_eq!(depths, (0..=maximum).collect::<Vec<_>>());
			assert_eq!(
				store
					.begin()
					.atom(&"item:root".parse()?)?
					.and_then(|atom| atom.properties().get(value).cloned()),
				Some(Value::from(i64::from(maximum)))
			);
		}
		Ok(())
	}

	#[test]
	fn event_async_kill_pending_and_running_keep_effects_unpublished() -> FerrosGrynnResult<()> {
		for running in [false, true] {
			let (mut store, target, _value) = fast_store()?;
			if !running {
				Arc::get_mut(&mut store.inner)
					.ok_or_else(|| FerrosGrynnError::invalid_argument("fixture owner shared"))?
					.event_processing_interval = Duration::from_secs(5);
			}
			let mut define = store.begin();
			define.define_event(
				EventDefinition::new(
					"busy",
					target,
					"true",
					"{ CREATE audit:partial; WHILE (true) { LET $x = 1; }; }",
				)?
				.with_execution(super::super::EventExecution::Async(EventAsyncOptions::default())),
			)?;
			define.commit()?;
			let mut source = store.begin();
			source.create_atom("item:a".parse()?, PropertyMap::new())?;
			source.commit()?;
			let id =
				*store.inner.current_root().event_outbox().invocations.keys().next().ok_or_else(
					|| FerrosGrynnError::invalid_argument("fixture invocation absent"),
				)?;
			if running {
				let started = std::time::Instant::now();
				while store.job(id)?.is_some_and(|job| job.state() == MaintenanceJobState::Queued) {
					if started.elapsed() > Duration::from_secs(5) {
						return Err(FerrosGrynnError::invalid_argument("attempt did not start"));
					}
					std::thread::sleep(Duration::from_millis(1));
				}
			} else {
				assert_eq!(
					store.job(id)?.map(|job| job.state()),
					Some(MaintenanceJobState::Queued)
				);
			}
			assert_eq!(store.kill_job(id)?.state(), MaintenanceJobState::Cancelled);
			assert_eq!(wait_jobs(&store, 1)?[0].state(), MaintenanceJobState::Cancelled);
			assert!(store.begin().atom(&"audit:partial".parse()?)?.is_none());
			assert_eq!(store.inner.current_root().event_outbox().pending_count, 0);
		}
		Ok(())
	}

	#[test]
	fn event_processing_interval_uses_positive_milliseconds() {
		assert_eq!(interval_from_value(None), Duration::from_secs(5));
		assert_eq!(interval_from_value(Some("10")), Duration::from_millis(10));
		for value in ["0", "-1", "bad"] {
			assert_eq!(interval_from_value(Some(value)), Duration::from_secs(5));
		}
	}

	#[test]
	fn event_async_worker_keeps_definition_context_and_then_sources_after_remove()
	-> FerrosGrynnResult<()> {
		let (store, target, value) = fast_store()?;
		let mut seed = store.begin();
		for result in seed.execute(
			"DEFINE PURE FUNCTION fn::gate() -> bool { RETURN true; }; DEFINE STABLE FUNCTION \
			 fn::read() -> int { RETURN <int>item:a.value; };",
			&Object::new(),
		)? {
			let _value = result.result?;
		}
		let event = seed.define_event(
			EventDefinition::new(
				"frozen",
				target,
				"$event = 'CREATE' AND fn::gate()",
				"{ CREATE audit:old CONTENT { value: fn::read(), accepted: $after.value, input: \
				 $input.value }; }",
			)?
			.with_execution(super::super::EventExecution::Async(EventAsyncOptions::default())),
		)?;
		let snapshot = seed.query_snapshot()?;
		let mut functions = Vec::new();
		for name in ["gate", "read"] {
			functions.push(
				seed.query_schema_functions(&snapshot, "fn", name)?
					.first()
					.and_then(|function| function.id())
					.ok_or_else(|| FerrosGrynnError::invalid_argument("fixture function absent"))?,
			);
		}
		seed.commit()?;
		let record = "item:a".parse()?;
		let mut source = store.begin();
		source.create_atom(record, PropertyMap::from_iter([(value, Value::from(1))]))?;
		source.set_base_property(&"item:a".parse()?, value, Value::from(9))?;
		let current = source
			.event(event)?
			.ok_or_else(|| FerrosGrynnError::invalid_argument("fixture EVENT absent"))?;
		let replacement = EventDefinition::new(
			current.name().clone(),
			current.target(),
			current.when().clone(),
			"{ CREATE audit:new; }",
		)?
		.with_execution(current.execution());
		source.replace_event(event, replacement)?;
		source.remove_event(event)?;
		for function in functions {
			source.remove_function(function)?;
		}
		for result in source
			.execute("DEFINE STABLE FUNCTION fn::read() -> int { RETURN 100; };", &Object::new())?
		{
			let _value = result.result?;
		}
		source.commit()?;
		let jobs = wait_jobs(&store, 1)?;
		assert_eq!(jobs[0].state(), MaintenanceJobState::Completed);
		assert_eq!(jobs[0].event().map(EventJobInfo::id), Some(event));
		assert_eq!(
			jobs[0].event().map(EventJobInfo::version),
			Some(super::super::SchemaVersion::INITIAL)
		);
		let dictionary = store.property_dictionary();
		let accepted = dictionary
			.get("accepted")
			.ok_or_else(|| FerrosGrynnError::invalid_argument("accepted key absent"))?;
		let input = dictionary
			.get("input")
			.ok_or_else(|| FerrosGrynnError::invalid_argument("input key absent"))?;
		let atom = store
			.begin()
			.atom(&"audit:old".parse()?)?
			.ok_or_else(|| FerrosGrynnError::invalid_argument("frozen effect absent"))?;
		assert_eq!(atom.properties().get(value), Some(&Value::from(9)));
		assert_eq!(atom.properties().get(accepted), Some(&Value::from(1)));
		assert_eq!(atom.properties().get(input), Some(&Value::from(1)));
		assert!(store.begin().atom(&"audit:new".parse()?)?.is_none());
		Ok(())
	}

	#[test]
	fn event_async_try_rollback_removes_accepted_payload_and_handler_effects()
	-> FerrosGrynnResult<()> {
		let (store, target, value) = fast_store()?;
		let mut define = store.begin();
		define.define_event(
			EventDefinition::new(
				"queued",
				target,
				"true",
				"{ CREATE audit:queued CONTENT { value: $after.value }; }",
			)?
			.with_execution(super::super::EventExecution::Async(EventAsyncOptions::default())),
		)?;
		define.define_event(EventDefinition::new(
			"rejected",
			target,
			"$after.value = 2",
			"{ CREATE audit:partial; THROW 'blocked'; }",
		)?)?;
		define.commit()?;
		let mut source = store.begin();
		for result in source.execute(
			"TRY { CREATE item:bad CONTENT { value: 2 }; } CATCH |$e| { $e.message; }; CREATE \
			 item:good CONTENT { value: 3 };",
			&Object::new(),
		)? {
			let _value = result.result?;
		}
		source.commit()?;
		assert_eq!(wait_jobs(&store, 1)?[0].state(), MaintenanceJobState::Completed);
		assert!(store.begin().atom(&"item:bad".parse()?)?.is_none());
		assert!(store.begin().atom(&"audit:partial".parse()?)?.is_none());
		assert_eq!(
			store
				.begin()
				.atom(&"audit:queued".parse()?)?
				.and_then(|atom| atom.properties().get(value).cloned()),
			Some(Value::from(3))
		);
		Ok(())
	}

	#[test]
	fn event_async_graph_delete_accepts_before_metadata_and_fails_on_current_schema()
	-> FerrosGrynnResult<()> {
		let (store, _target, _value) = fast_store()?;
		let mut seed = store.begin();
		let graph = seed.define_graph("scope", crate::graph::GraphKind::Graph)?;
		let record = "item:a".parse()?;
		seed.create_atom(record, PropertyMap::new())?;
		seed.attach_node(graph.id(), &"item:a".parse()?, None, None)?;
		seed.define_event(
			EventDefinition::new(
				"deleted",
				EventTarget::Graph(graph.id()),
				"$event = 'DELETE'",
				"{ SELECT * FROM GRAPH scope; }",
			)?
			.with_execution(super::super::EventExecution::Async(EventAsyncOptions::default())),
		)?;
		seed.commit()?;
		let mut remove = store.begin();
		remove.remove_graph("scope")?;
		remove.commit()?;
		let jobs = wait_jobs(&store, 1)?;
		assert_eq!(jobs[0].state(), MaintenanceJobState::Failed);
		assert!(jobs[0].diagnostic().is_some_and(|message| message.contains("scope")));
		assert!(store.inner.current_root().schema.named_graph("scope").is_none());
		assert!(store.begin().atom(&"item:a".parse()?)?.is_some());
		Ok(())
	}

	#[test]
	fn event_async_pending_count_and_byte_limits_reject_source_atomically() -> FerrosGrynnResult<()>
	{
		for count_limit in [true, false] {
			let (store, target, _value) = fast_store()?;
			let mut define = store.begin();
			let id = define.define_event(
				EventDefinition::new("capacity", target, "true", "1")?.with_execution(
					super::super::EventExecution::Async(EventAsyncOptions::default()),
				),
			)?;
			define.commit()?;
			let event = store
				.begin()
				.event(id)?
				.ok_or_else(|| FerrosGrynnError::invalid_argument("fixture definition absent"))?;
			let mut arguments =
				[Value::from("CREATE"), Value::Null, Value::Null, Value::Null, Value::Null];
			let fixed = crate::storage::event_outbox::encode_payload(
				&event,
				&arguments,
				&[],
				64 * 1024 * 1024,
			)?
			.len();
			if !count_limit {
				arguments[3] =
					Value::Bytes(crate::values::Bytes::new(vec![1; 8 * 1024 * 1024 - fixed - 32]));
			}
			let payload: Arc<[u8]> = crate::storage::event_outbox::encode_payload(
				&event,
				&arguments,
				&[],
				128 * 1024 * 1024,
			)?
			.into();
			let mut root = store.inner.current_root().as_ref().clone();
			let count = if count_limit {
				10_000
			} else {
				8
			};
			let due = jiff::Timestamp::now().as_nanosecond() + 86_400_000_000_000;
			let stamp = root.commit_ts;
			for _ in 0..count {
				let invocation = EventInvocation::queued(
					&event,
					EventCause::next(None, event.execution())?,
					Arc::clone(&payload),
					due,
				)?;
				let id = invocation.id;
				Arc::make_mut(root.event_outbox_mut()).enqueue(Arc::new(invocation), stamp)?;
				root.mark_version(super::super::ConflictKey::EventInvocation(id), stamp);
			}
			if count_limit {
				assert_eq!(root.event_outbox().pending_count, 10_000);
			} else {
				assert!(root.event_outbox().pending_bytes > 63 * 1024 * 1024);
			}
			*store.inner.current.write() = Arc::new(root);
			let mut source = store.begin();
			let record = "item:rejected".parse()?;
			assert!(source.create_atom(record, PropertyMap::new()).is_err());
			assert!(source.atom(&"item:rejected".parse()?)?.is_none());
			assert_eq!(store.inner.current_root().event_outbox().pending_count, count);
			let imported = store.import(
				super::super::ImportOptions {
					events: super::super::ImportEventsMode::On,
					..super::super::ImportOptions::default()
				},
				|session| {
					session.apply_item(|tx| {
						tx.create_atom("item:import_rejected".parse()?, PropertyMap::new())
					})?;
					Ok(())
				},
			);
			assert!(imported.is_err());
			assert!(store.begin().atom(&"item:import_rejected".parse()?)?.is_none());
			assert_eq!(store.inner.current_root().event_outbox().pending_count, count);
		}
		Ok(())
	}

	#[test]
	fn event_async_gql_info_and_kill_use_authoritative_job_projection() -> FerrosGrynnResult<()> {
		let (mut store, target, _value) = fast_store()?;
		Arc::get_mut(&mut store.inner)
			.ok_or_else(|| FerrosGrynnError::invalid_argument("fixture owner shared"))?
			.event_processing_interval = Duration::from_secs(5);
		let mut define = store.begin();
		define
			.define_event(EventDefinition::new("pending", target, "true", "1")?.with_execution(
				super::super::EventExecution::Async(EventAsyncOptions::default()),
			))?;
		define.commit()?;
		let mut source = store.begin();
		source.create_atom("item:a".parse()?, PropertyMap::new())?;
		source.commit()?;
		let id = *store
			.inner
			.current_root()
			.event_outbox()
			.invocations
			.keys()
			.next()
			.ok_or_else(|| FerrosGrynnError::invalid_argument("fixture invocation absent"))?;
		let input = Object::from_iter([("job", Value::from(id.as_uuid()))]);
		for (query, state) in [
			("INFO FOR JOB $job;", "queued"),
			("KILL $job;", "cancelled"),
			("INFO FOR JOB $job;", "cancelled"),
		] {
			let value = store
				.execute(query, &input)?
				.pop()
				.ok_or_else(|| FerrosGrynnError::invalid_argument("job result absent"))?
				.result?
				.into_parts()
				.0;
			let object = value.as_object().ok_or_else(|| {
				FerrosGrynnError::invalid_argument("job projection not an object")
			})?;
			assert_eq!(object.get("kind").and_then(Value::as_str), Some("event_invocation"));
			assert_eq!(object.get("state").and_then(Value::as_str), Some(state));
			assert_eq!(object.get("attempts"), Some(&Value::from(0)));
			assert_eq!(object.get("depth"), Some(&Value::from(0)));
		}
		assert!(store.begin().atom(&"item:a".parse()?)?.is_some());
		Ok(())
	}
}
