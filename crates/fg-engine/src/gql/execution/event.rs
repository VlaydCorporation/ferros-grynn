//! Event source compilation reuses the ordinary closure analyzer and effects.

use std::sync::Arc;

use fg_runtime::__internal::ValueInternal as _;

use super::{
	ClosureInvocationMode,
	ControlRuntime,
	LogicalStatement,
	TypedExprKind,
	adapter::{TransactionRead, analyze_event_source},
	invoke_logical_closure_with_read,
};
use crate::{
	datastore::{EventDefinition, EventExecution, SchemaCatalog, SchemaFunction, Transaction},
	gql::{
		closure::LanguageClosureState,
		ir::{ClosureEffect, LogicalClosure},
	},
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	values::Value,
};

/// Context parameters share one spelling and one pre-allocation byte estimate.
const EVENT_CONTEXT_PREFIX: &str = "|$event, $before, $after, $value, $input| ";

/// Executes one accepted context through the ordinary transaction-aware kernel.
///
/// # Errors
///
/// Returns compilation, effect, cancellation, resource or handler errors. The
/// enclosing logical mutation owns rollback of the original data change.
#[allow(
	clippy::redundant_pub_crate,
	reason = "datastore dispatch uses the private GQL execution boundary"
)]
pub(crate) fn execute_event(
	transaction: &mut Transaction,
	event: &EventDefinition,
	arguments: &[Value; 5],
	parent: &crate::gql::ExecutionControl,
	origin: &SchemaCatalog,
) -> FerrosGrynnResult<bool> {
	if !evaluate_event_source(
		transaction,
		event.when().as_str(),
		arguments,
		ClosureEffect::StableRead,
		parent,
		None,
	)?
	.is_truthy()
	{
		return Ok(false);
	}
	transaction.with_event_cause(event, |transaction, cause| match event.execution() {
		EventExecution::Sync => execute_event_handler(transaction, event, arguments, parent, None),
		EventExecution::Async(_) => {
			let payload = prepare_async_event(transaction, event, arguments, parent, origin)?;
			transaction.start_event_worker()?;
			transaction.enqueue_event_payload(event, cause, payload, parent).map(|_id| ())
		}
	})?;
	Ok(true)
}

/// Compiles THEN only for accepted predicates and persists its exact
/// dependencies.
fn prepare_async_event(
	transaction: &mut Transaction,
	event: &EventDefinition,
	arguments: &[Value; 5],
	parent: &crate::gql::ExecutionControl,
	origin: &SchemaCatalog,
) -> FerrosGrynnResult<Vec<u8>> {
	parent.check()?;
	let available = parent.barrier_memory_available().min(transaction.event_compiler_scratch(0));
	let cache = LanguageClosureState::bounded_with_work(available, parent.max_loop_iterations)
		.with_compile_checks(Some(parent.interruption()));
	let (source, plan, functions) = compile_event_source(
		transaction,
		event.then().as_str(),
		ClosureEffect::DataMutation,
		&cache,
		None,
		Some(origin),
	)?;
	let retained = source
		.len()
		.checked_add(plan.retained_plan_bytes)
		.and_then(|bytes| bytes.checked_add(cache.retained_bytes()))
		.and_then(|bytes| {
			bytes.checked_add(
				functions.len().saturating_mul(std::mem::size_of::<Arc<SchemaFunction>>()),
			)
		})
		.ok_or_else(|| FerrosGrynnError::operation_overflow("accepted EVENT compiler bytes"))?;
	let mut account = parent.barrier_account();
	if !account.reserve(retained) {
		return Err(FerrosGrynnError::traversal_resource_limit_exceeded(
			"accepted EVENT compiler",
			retained as u64,
		));
	}
	let payload = crate::storage::event_outbox::encode_payload(
		event,
		arguments,
		&functions,
		parent.barrier_memory_available().min(available.saturating_sub(retained)),
	)?;
	parent.check()?;
	Ok(payload)
}

/// Runs an accepted handler with optional frozen functions and current data.
///
/// # Errors
///
/// Returns ordinary source, effect, handler, interruption or resource errors.
#[allow(
	clippy::redundant_pub_crate,
	reason = "the durable worker invokes the private accepted-handler boundary"
)]
pub(crate) fn execute_event_handler(
	transaction: &mut Transaction,
	event: &EventDefinition,
	arguments: &[Value; 5],
	parent: &crate::gql::ExecutionControl,
	frozen: Option<&[Arc<SchemaFunction>]>,
) -> FerrosGrynnResult<()> {
	let _value = evaluate_event_source(
		transaction,
		event.then().as_str(),
		arguments,
		ClosureEffect::DataMutation,
		parent,
		frozen,
	)?;
	Ok(())
}

/// Predicate and handler compiler scopes release independently after
/// evaluation.
fn evaluate_event_source(
	transaction: &mut Transaction,
	source: &str,
	arguments: &[Value; 5],
	ceiling: ClosureEffect,
	parent: &crate::gql::ExecutionControl,
	frozen: Option<&[Arc<SchemaFunction>]>,
) -> FerrosGrynnResult<Value> {
	parent.check()?;
	let cache = Arc::new(
		LanguageClosureState::bounded_with_work(
			parent.barrier_memory_available(),
			parent.max_loop_iterations,
		)
		.with_compile_checks(Some(parent.interruption())),
	);
	let control = parent.clone().with_closure_registry(Arc::clone(&cache));
	let (source, plan, _sources) =
		compile_event_source(transaction, source, ceiling, &cache, frozen, None)?;
	let bytes = plan
		.slot_count
		.checked_mul(std::mem::size_of::<Value>())
		.and_then(|bytes| bytes.checked_add(plan.retained_plan_bytes))
		.and_then(|bytes| bytes.checked_add(source.len()))
		.and_then(|bytes| bytes.checked_add(cache.retained_bytes()))
		.and_then(|bytes| {
			bytes.checked_add(
				_sources.len().saturating_mul(std::mem::size_of::<Arc<SchemaFunction>>()),
			)
		})
		.and_then(|bytes| {
			arguments.iter().try_fold(bytes, |bytes, argument| {
				bytes.checked_add(argument.estimated_heap_bytes().saturating_mul(2))
			})
		})
		.ok_or_else(|| FerrosGrynnError::operation_overflow("EVENT invocation bytes"))?;
	let mut account = control.barrier_account();
	if !account.reserve(bytes) {
		return Err(FerrosGrynnError::traversal_resource_limit_exceeded(
			"EVENT invocation scratch",
			bytes as u64,
		));
	}
	let snapshot = transaction.query_snapshot_admitted(false, |bytes| {
		if account.reserve(bytes) {
			Ok(())
		} else {
			Err(FerrosGrynnError::traversal_resource_limit_exceeded(
				"EVENT execution snapshot",
				bytes as u64,
			))
		}
	})?;
	let mut read = TransactionRead {
		scalar_graph: None,
		transaction,
		snapshot,
		metadata_only: false,
	};
	invoke_logical_closure_with_read(
		&mut read,
		&plan,
		&[],
		arguments,
		&source,
		&control,
		&mut ControlRuntime::new(&control),
		ClosureInvocationMode::Standalone,
	)
}

/// Validates stored event sources without executing their predicates or bodies.
///
/// # Errors
///
/// Returns ordinary syntax, semantic, type, effect or resource errors. Context
/// parameters have dynamic types independently of caller lexical bindings.
#[allow(
	clippy::redundant_pub_crate,
	reason = "event source validation crosses the datastore/GQL boundary"
)]
pub(crate) fn validate_event_definition(
	transaction: &mut Transaction,
	event: &EventDefinition,
	scratch: usize,
	control: Option<&crate::gql::ExecutionControl>,
) -> FerrosGrynnResult<()> {
	let scratch =
		control.map_or(scratch, |control| scratch.min(control.barrier_memory_available()));
	let work = control.map_or(1_000_000, |control| control.max_loop_iterations);
	let cache = LanguageClosureState::bounded_with_work(scratch, work)
		.with_compile_checks(control.map(crate::gql::ExecutionControl::interruption));
	let (_predicate_source, _predicate, _predicate_functions) = compile_event_source(
		transaction,
		event.when().as_str(),
		ClosureEffect::StableRead,
		&cache,
		None,
		None,
	)?;
	let _predicate_reservation = cache.admit_compile_metadata(
		_predicate.retained_plan_bytes.saturating_add(_predicate_source.len()),
		0,
	)?;
	let _handler = compile_event_source(
		transaction,
		event.then().as_str(),
		ClosureEffect::DataMutation,
		&cache,
		None,
		None,
	)?;
	Ok(())
}

/// Compiles one context-parameter closure and checks the body's transitive
/// effect.
fn compile_event_source(
	transaction: &mut Transaction,
	source: &str,
	ceiling: ClosureEffect,
	cache: &LanguageClosureState,
	frozen: Option<&[Arc<SchemaFunction>]>,
	origin: Option<&SchemaCatalog>,
) -> FerrosGrynnResult<(String, Box<LogicalClosure>, Vec<Arc<SchemaFunction>>)> {
	let source_bytes = source
		.len()
		.checked_add(EVENT_CONTEXT_PREFIX.len())
		.ok_or_else(|| FerrosGrynnError::operation_overflow("EVENT compiler source"))?;
	let _compiler_reservation = cache.admit_compile(source_bytes)?;
	if crate::syn::has_bridge_clause(source)? {
		return Err(FerrosGrynnError::query_feature_not_supported(
			"WITH BRIDGE dependencies in durable EVENT recipes",
		));
	}
	let source = format!("{EVENT_CONTEXT_PREFIX}{source}");
	let parsed = crate::gql::parse(&source, crate::syn::ParserSettings::default())?;
	if parsed.statements.len() != 1 {
		return Err(FerrosGrynnError::invalid_argument(
			"EVENT source must contain exactly one expression or block",
		));
	}
	let (mut program, functions) =
		analyze_event_source(transaction, &source, &parsed, cache, frozen, origin)?;
	for function in &functions {
		if crate::syn::has_bridge_clause(function.body())? {
			return Err(FerrosGrynnError::query_feature_not_supported(
				"WITH BRIDGE function dependencies in durable EVENT recipes",
			));
		}
	}
	let Some(LogicalStatement::Expression {
		value,
	}) = program.statements.pop()
	else {
		return Err(FerrosGrynnError::invalid_argument(
			"EVENT source did not compile to a closure",
		));
	};
	let TypedExprKind::Closure(closure) = value.kind else {
		return Err(FerrosGrynnError::invalid_argument(
			"EVENT source did not compile to a closure",
		));
	};
	if closure.effect > ceiling {
		return Err(FerrosGrynnError::query_feature_not_supported(format!(
			"EVENT body effect {:?} exceeds {ceiling:?}",
			closure.effect
		)));
	}
	Ok((source, closure, functions))
}

#[cfg(test)]
/// Regression tests for event behavior.
mod tests {
	use super::*;
	use crate::{
		datastore::{ClassSchema, Datastore, EventTarget},
		graph::AtomKind,
		values::{Object, PropertyMap, RecordId},
	};

	/// Verifies event frozen functions survive remove and read current data.
	#[test]
	fn event_frozen_functions_survive_remove_and_read_current_data() {
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
		setup.define_class(ClassSchema::new("audit", AtomKind::Node, []).assert_ok()).assert_ok();
		let value = setup.intern_property("value").assert_ok();
		let record: RecordId = "item:a".parse().assert_ok();
		setup
			.create_atom(record.clone(), PropertyMap::from_iter([(value, Value::from(1))]))
			.assert_ok();
		for result in setup
			.execute(
				"DEFINE STABLE FUNCTION fn::read() -> int { RETURN <int>item:a.value; };",
				&Object::new(),
			)
			.assert_ok()
		{
			let _value = result.result.assert_ok();
		}
		let event = EventDefinition::new(
			"frozen",
			EventTarget::Class(class),
			"true",
			"{ CREATE audit:frozen CONTENT { value: fn::read() }; }",
		)
		.assert_ok();
		let cache = LanguageClosureState::bounded_with_work(16 * 1024 * 1024, 1_000_000);
		let (_source, _plan, functions) = compile_event_source(
			&mut setup,
			event.then().as_str(),
			ClosureEffect::DataMutation,
			&cache,
			None,
			None,
		)
		.assert_ok();
		assert_eq!(functions.len(), 1);
		let id = functions[0]
			.id()
			.ok_or_else(|| FerrosGrynnError::invalid_argument("captured function identity"))
			.assert_ok();
		setup.commit().assert_ok();
		let mut changed = store.begin();
		changed.remove_function(id).assert_ok();
		for result in changed
			.execute("DEFINE STABLE FUNCTION fn::read() -> int { RETURN 100; };", &Object::new())
			.assert_ok()
		{
			let _value = result.result.assert_ok();
		}
		changed.set_base_property(&record, value, Value::from(9)).assert_ok();
		changed.commit().assert_ok();
		let mut attempt = store.begin();
		let control =
			crate::gql::ExecutionControl::new(&crate::gql::QueryExecutionOptions::default(), None);
		execute_event_handler(
			&mut attempt,
			&event,
			&[Value::None, Value::None, Value::None, Value::None, Value::None],
			&control,
			Some(&functions),
		)
		.assert_ok();
		assert_eq!(
			attempt
				.atom(&"audit:frozen".parse().assert_ok())
				.assert_ok()
				.and_then(|atom| atom.properties().get(value).cloned()),
			Some(Value::from(9))
		);
		attempt.commit().assert_ok();
	}

	/// Verifies event false predicate does not compile retired handler
	/// function.
	#[test]
	fn event_false_predicate_does_not_compile_retired_handler_function() -> FerrosGrynnResult<()> {
		let store = Datastore::in_memory();
		let mut setup = store.begin();
		let class = setup.define_class(ClassSchema::new("item", AtomKind::Node, [])?)?;
		for result in setup
			.execute("DEFINE PURE FUNCTION fn::body() -> int { RETURN 1; };", &Object::new())?
		{
			let _value = result.result?;
		}
		setup.define_event(EventDefinition::new(
			"inactive",
			EventTarget::Class(class),
			"false",
			"fn::body()",
		)?)?;
		let snapshot = setup.query_snapshot()?;
		let function = setup
			.query_schema_functions(&snapshot, "fn", "body")?
			.first()
			.and_then(|function| function.id())
			.ok_or_else(|| FerrosGrynnError::invalid_argument("handler function"))?;
		setup.remove_function(function)?;
		setup.commit()?;
		let mut write = store.begin();
		write.create_atom("item:a".parse()?, PropertyMap::new())?;
		write.commit()?;
		Ok(())
	}

	/// Verifies event compiler snapshot copy uses handler scratch limit.
	#[test]
	fn event_compiler_snapshot_copy_uses_handler_scratch_limit() {
		use crate::test_assertions::AssertResult as _;

		let store = Datastore::in_memory();
		let mut setup = store.begin();
		let class = setup
			.define_class(ClassSchema::new("item", AtomKind::Node, []).assert_ok())
			.assert_ok();
		for index in 0..2000 {
			setup
				.define_graph(format!("scope_{index}"), crate::graph::GraphKind::Graph)
				.assert_ok();
		}
		setup.commit().assert_ok();
		let event =
			EventDefinition::new("small", EventTarget::Class(class), "true", "1").assert_ok();
		let options = crate::gql::QueryExecutionOptions {
			barrier_memory_bytes: std::num::NonZeroUsize::new(32 * 1024)
				.ok_or_else(|| FerrosGrynnError::invalid_argument("scratch limit"))
				.assert_ok(),
			..Default::default()
		};
		let control = crate::gql::ExecutionControl::new(&options, None);
		let mut attempt = store.begin();
		let error = execute_event_handler(
			&mut attempt,
			&event,
			&[Value::None, Value::None, Value::None, Value::None, Value::None],
			&control,
			None,
		)
		.err()
		.ok_or_else(|| {
			FerrosGrynnError::invalid_argument("graph-map copy bypassed handler scratch")
		})
		.assert_ok();
		assert!(error.to_string().contains("compiler scratch"));
	}
}
