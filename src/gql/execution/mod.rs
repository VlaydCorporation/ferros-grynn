#![allow(
	clippy::pattern_type_mismatch,
	reason = "the executor intentionally uses match ergonomics for borrowed IR and runtime values"
)]

mod adapter;
mod bridge;
mod builtins;
mod changefeed;
mod collation;
mod cursor;
mod dml;
mod eval;
/// Source-only event compilation through the ordinary closure analyzer.
mod event;
#[allow(
	clippy::redundant_pub_crate,
	reason = "the GQL facade re-exports these functions to datastore dispatch"
)]
pub(crate) use event::{execute_event, execute_event_handler, validate_event_definition};
mod explain;
mod graph_properties;
mod graph_result;
mod live_planning;
mod root_read;
pub(in crate::gql) use root_read::RootRead;
mod schema;
mod select;
mod traversal;

use std::{
	cell::RefCell,
	rc::Rc,
	sync::Arc,
	time::{Duration, Instant},
};

use adapter::TransactionRead;
pub(super) use adapter::analyze_import_preflight;
#[cfg(test)]
pub(crate) use adapter::execute_parsed_transaction_with_bindings;
pub(crate) use adapter::{analyze_parsed_transaction, execute_parsed_statement_with_bindings};
pub(in crate::gql) use adapter::{atom_source_schema, query_schema};
pub use adapter::{execute_transaction, execute_transaction_with_options};
pub(in crate::gql) use cursor::{AtomFieldRead, RootFields};
use dml::{execute_dml, execute_graph_dml};
pub(in crate::gql) use eval::{invoke_logical_closure, invoke_root_logical_closure};
pub(in crate::gql) use graph_result::GraphAssemblyAccumulator;
use rust_decimal::Decimal;
use schema::execute_schema;
use select::execute_select;
pub(in crate::gql) use select::{
	normalize_root_select,
	resolve_root_graph_reference,
	visit_root_aggregate_union,
	visit_root_row_union,
	visit_root_select,
};
#[allow(
	clippy::wildcard_imports,
	reason = "move-only executor split keeps the existing cursor and evaluator names during stage \
	          1"
)]
use {cursor::*, eval::*};

pub(in crate::gql) fn evaluate_constant(expression: &TypedExpr) -> Option<Value> {
	eval(expression, None, &[], "", None, None).ok()
}

/// Standalone reactive expressions use the same special SELECT materialization.
pub(in crate::gql) fn evaluate_root_expression(
	read: &RootRead<'_>,
	expression: &TypedExpr,
	slots: &[Value],
	control: &ExecutionControl,
) -> FerrosGrynnResult<Value> {
	select::eval_row_expression(
		read,
		read.snapshot()?,
		expression,
		&Row::new(Vec::new()),
		slots,
		"",
		control,
	)
}

use super::{
	QueryResult,
	QueryStatementKind,
	analysis::{QueryCatalog, analyze, analyze_statement_with_runtime, compatible},
	ir::{
		Aggregate,
		Builtin,
		ClosureEffect,
		Column,
		EndpointDomain,
		Expand,
		LogicalClosure,
		LogicalClosureBody,
		LogicalDmlData,
		LogicalDmlKind,
		LogicalDmlReturn,
		LogicalDmlReturnItem,
		LogicalDmlSelector,
		LogicalDmlStatement,
		LogicalDmlTarget,
		LogicalExpect,
		LogicalGraphDmlStatement,
		LogicalGraphPropertyMutation,
		LogicalGraphReference,
		LogicalLetValue,
		LogicalMatch,
		LogicalMatchPattern,
		LogicalMatchPatternKind,
		LogicalMatchSelector,
		LogicalOrder,
		LogicalProgram,
		LogicalSchemaStatement,
		LogicalSelect,
		LogicalSelectAccess,
		LogicalSelectItem,
		LogicalSelectSource,
		LogicalSelectSourceKind,
		LogicalSelectStart,
		LogicalStatement,
		LogicalTraverse,
		LogicalTraverseSource,
		LogicalWildcardField,
		MatchBinding,
		MatchOp,
		PathExpand,
		ScanTarget,
		Schema,
		TraverseStepKind,
		TypedExpr,
		TypedExprKind,
	},
	options::{BarrierAccount, ExecutionControl},
};
use crate::{
	datastore::{
		ImportItemOutcome,
		ImportSession,
		QuerySnapshot,
		QueryStorageCursor,
		QueryStorageRow,
		ScalarComparison,
		ScalarIndexConstraint,
		ScalarIndexWindow,
		Transaction,
		TransactionGraphView,
	},
	gql_status::{ExecutionPhase, FerrosGrynnError, FerrosGrynnResult, Location},
	graph::{
		AtomId,
		AtomOccurrenceRef,
		EdgeDirection,
		EdgeEndpoint,
		EdgeRead,
		EdgeTopology,
		EndpointRole,
		GraphRead,
		NodeRead,
		PathDirection,
		PathStep,
		PathTraceArena,
		PathTraceId,
		TraversalDirection,
		TraversalIndex,
		TraversalSemantics,
	},
	math::{add_exact, divide_signed_exact, multiply_exact, subtract_exact},
	syn::{BinaryOperator, ParserSettings, Program, UnaryOperator},
	values::{
		Array,
		AtomKind,
		Closure,
		GraphRef,
		Number,
		Object,
		Path,
		PropertyDictionary,
		PropertyMap,
		RecordId,
		Type,
		Value,
	},
};

/// Parses and executes text through a statically dispatched read adapter.
pub(in crate::gql::execution) fn execute_text<Read: QueryRead>(
	read: &mut Read,
	source: &str,
	variables: &Object,
	settings: ParserSettings,
) -> FerrosGrynnResult<Vec<QueryResult>> {
	let program = crate::gql::parse(source, settings)?;
	let mut bindings = variables.clone();
	let control = ExecutionControl::new(&crate::gql::QueryExecutionOptions::default(), None);
	execute_program(read, source, &program, &mut bindings, &control)
}

/// Executes an already parsed program against one captured snapshot.
fn execute_program<Read: QueryRead>(
	read: &mut Read,
	source: &str,
	program: &Program,
	bindings: &mut Object,
	control: &ExecutionControl,
) -> FerrosGrynnResult<Vec<QueryResult>> {
	let snapshot = read.snapshot()?;
	let logical = analyze(program, source, read.catalog(snapshot), bindings)?;
	execute_logical_program(read, source, &logical, bindings, control)
}

fn execute_parsed_statement<Read: QueryRead>(
	read: &mut Read,
	source: &str,
	statement: &crate::syn::Statement,
	bindings: &mut Object,
	mutable_bindings: &std::collections::BTreeSet<Box<str>>,
	control: &mut ExecutionControl,
) -> FerrosGrynnResult<QueryResult> {
	let snapshot = read.snapshot()?;
	let (logical, timeout) = analyze_statement_with_runtime(
		statement,
		source,
		read.catalog(snapshot),
		bindings,
		mutable_bindings,
		control,
	)?;
	let mut slots = initialize_slots(&logical, bindings);
	let statement = logical.statements.first().ok_or_else(|| {
		FerrosGrynnError::invalid_argument("Analyzed statement produced no executable IR.")
	})?;
	let preparation = (|| {
		if let Some(duration) = timeout.as_ref() {
			let duration = eval_query_duration(read, duration, &slots, source, control, "TIMEOUT")?;
			control.restrict_timeout(duration)?;
		}
		control.check()
	})();
	if let Err(error) = preparation {
		discard_statement_cursor(statement, &slots, control);
		return Err(error);
	}
	if control.infer_write_access()
		&& logical.statements.iter().any(|statement| {
			crate::gql::analysis::logical_statement_effect(statement) >= ClosureEffect::DataMutation
		}) {
		read.prepare_mutating_statement()?;
	}
	let result =
		execute_logical_statement(read, source, statement, &mut slots, bindings, control, false)?;
	if result.result.is_ok() {
		persist_mutable_initial_slots(&logical, &slots, bindings)?;
	}
	Ok(result)
}

/// All preparation boundaries retire only a directly supplied continuation.
/// EXPLAIN is unwrapped without evaluating its SELECT inputs a second time.
fn discard_statement_cursor(
	statement: &LogicalStatement,
	slots: &[Value],
	control: &ExecutionControl,
) {
	let plan = match statement {
		LogicalStatement::Select(plan) => Some(plan),
		LogicalStatement::Explain(explain) => match explain.statement.as_ref() {
			LogicalStatement::Select(plan) => Some(plan),
			_ => None,
		},
		_ => None,
	};
	if let Some(plan) = plan {
		select::discard_supplied_cursor(plan, slots, control);
	}
}

fn execute_logical_program<Read: QueryRead>(
	read: &mut Read,
	source: &str,
	logical: &LogicalProgram,
	bindings: &mut Object,
	control: &ExecutionControl,
) -> FerrosGrynnResult<Vec<QueryResult>> {
	let mut slots = initialize_slots(logical, bindings);
	let mut results = Vec::with_capacity(logical.statements.len());
	for statement in &logical.statements {
		let result = execute_logical_statement(
			read, source, statement, &mut slots, bindings, control, true,
		)?;
		let failed = result.result.is_err();
		results.push(result);
		if failed {
			break;
		}
	}
	Ok(results)
}

fn execute_logical_statement<Read: QueryRead>(
	read: &mut Read,
	source: &str,
	statement: &LogicalStatement,
	slots: &mut [Value],
	bindings: &mut Object,
	control: &ExecutionControl,
	measure_elapsed: bool,
) -> FerrosGrynnResult<QueryResult> {
	if control.deterministic() {
		crate::gql::optimizer::validate_deterministic_statement(statement)?;
	}
	let start = measure_elapsed.then(Instant::now);
	let kind = logical_statement_kind(statement);
	control.reset_loop_statement();
	let mut runtime = ControlRuntime::new(control);
	let result =
		execute_statement(read, statement, slots, bindings, source, control, &mut runtime, 0, true)
			.and_then(|flow| match flow {
				ControlFlow::Value(value) | ControlFlow::Return(value) => control.outcome(value),
				ControlFlow::Break | ControlFlow::Continue => {
					Err(FerrosGrynnError::invalid_argument(
						"BREAK or CONTINUE escaped its enclosing loop.",
					))
				}
			})
			.and_then(|outcome| {
				control.check()?;
				Ok(outcome)
			});
	let continuation = if result.is_ok() {
		control.take_continuation()
	} else {
		control.discard_continuation();
		None
	};
	Ok(QueryResult {
		kind,
		elapsed: start.map_or(Duration::ZERO, |start| start.elapsed()),
		result,
		continuation,
	})
}

pub(crate) fn execute_import_program(
	session: &mut ImportSession,
	source: &str,
	logical: &LogicalProgram,
	variables: &Object,
	parent_control: &ExecutionControl,
	timeouts: &[Option<std::time::Duration>],
	payloads: &[super::import::ImportItemPayload],
) -> FerrosGrynnResult<()> {
	if logical.statements.len() != timeouts.len() || logical.statements.len() != payloads.len() {
		return Err(FerrosGrynnError::invalid_argument(
			"Import execution metadata does not match the analyzed program.",
		));
	}
	let mut slots = initialize_slots(logical, variables);
	let mut bindings = variables.clone();
	for ((statement, timeout), payload) in logical.statements.iter().zip(timeouts).zip(payloads) {
		let control = parent_control.checked_child(*timeout)?;
		control.check()?;
		control.reset_loop_statement();
		let mut runtime = ControlRuntime::new(&control);
		if let LogicalStatement::For {
			slot,
			iterable,
			body,
		} = statement
		{
			let values =
				iterable_values(eval(iterable, None, &slots, source, None, Some(&control))?)?;
			for value in values {
				control.check()?;
				record_iteration(&mut runtime, &control)?;
				slots[*slot as usize] = value;
				let binding_name = payload.binding_name().ok_or_else(|| {
					FerrosGrynnError::invalid_argument(
						"Import loop binding is absent from its source metadata.",
					)
				})?;
				let payload = payload.value(
					session.next_item_ordinal()?,
					Some((binding_name, &slots[*slot as usize])),
				);
				let mut flow = None;
				let outcome = session.apply_gql_item(payload, |transaction| {
					flow = Some(execute_import_block(
						transaction,
						body,
						&mut slots,
						&mut bindings,
						source,
						&control,
						&mut runtime,
					)?);
					Ok(())
				})?;
				if matches!(outcome, ImportItemOutcome::Skipped(_)) {
					continue;
				}
				match flow {
					Some(ControlFlow::Break) => break,
					Some(
						ControlFlow::Value(_) | ControlFlow::Return(_) | ControlFlow::Continue,
					) => {}
					None => {
						return Err(FerrosGrynnError::invalid_argument(
							"Applied import iteration produced no control-flow result.",
						));
					}
				}
			}
			continue;
		}

		let payload = payload.value(session.next_item_ordinal()?, None);
		session.apply_gql_item(payload, |transaction| {
			execute_import_statement(
				transaction,
				statement,
				&mut slots,
				&mut bindings,
				source,
				&control,
				&mut runtime,
			)
		})?;
	}
	Ok(())
}

#[allow(clippy::too_many_arguments, reason = "the import item reuses the concrete interpreter")]
fn execute_import_statement(
	transaction: &mut Transaction,
	statement: &LogicalStatement,
	slots: &mut [Value],
	bindings: &mut Object,
	source: &str,
	control: &ExecutionControl,
	runtime: &mut ControlRuntime,
) -> FerrosGrynnResult<()> {
	let snapshot = transaction.query_snapshot()?;
	let mut read = TransactionRead {
		scalar_graph: None,
		transaction,
		snapshot,
		metadata_only: false,
	};
	match execute_statement(
		&mut read, statement, slots, bindings, source, control, runtime, 0, false,
	)? {
		ControlFlow::Value(_) | ControlFlow::Return(_) => Ok(()),
		ControlFlow::Break | ControlFlow::Continue => Err(FerrosGrynnError::invalid_argument(
			"BREAK or CONTINUE escaped its enclosing import loop.",
		)),
	}
}

#[allow(clippy::too_many_arguments, reason = "the import item reuses the concrete interpreter")]
fn execute_import_block(
	transaction: &mut Transaction,
	block: &crate::gql::ir::LogicalBlock,
	slots: &mut [Value],
	bindings: &mut Object,
	source: &str,
	control: &ExecutionControl,
	runtime: &mut ControlRuntime,
) -> FerrosGrynnResult<ControlFlow> {
	let snapshot = transaction.query_snapshot()?;
	let mut read = TransactionRead {
		scalar_graph: None,
		transaction,
		snapshot,
		metadata_only: false,
	};
	execute_block(&mut read, block, slots, bindings, source, control, runtime, 1)
}

pub(in crate::gql::execution) struct ControlRuntime {
	/// Invocation-local view of the control's shared loop gas authority.
	budget: Arc<crate::gql::options::RuntimeBudget>,
}

impl ControlRuntime {
	/// Pins shared gas without allocating another counter or resetting it.
	pub(in crate::gql::execution) fn new(control: &ExecutionControl) -> Self {
		Self {
			budget: control.runtime_budget(),
		}
	}
}

enum ControlFlow {
	Value(Value),
	Return(Value),
	Break,
	Continue,
}

#[derive(Clone, Copy)]
pub(in crate::gql::execution) enum ClosureInvocationMode {
	ReadOnlyTraversal,
	TraversalSideEffect,
	Standalone,
}

#[allow(clippy::too_many_arguments, reason = "the interpreter keeps one concrete execution path")]
fn execute_statement<Read: QueryRead>(
	read: &mut Read,
	statement: &LogicalStatement,
	slots: &mut [Value],
	bindings: &mut Object,
	source: &str,
	control: &ExecutionControl,
	runtime: &mut ControlRuntime,
	depth: u32,
	persist_let: bool,
) -> FerrosGrynnResult<ControlFlow> {
	if !matches!(statement, LogicalStatement::Select(_))
		&& let Err(error) = control.check()
	{
		discard_statement_cursor(statement, slots, control);
		return Err(error);
	}
	if depth > control.max_control_depth {
		discard_statement_cursor(statement, slots, control);
		return Err(FerrosGrynnError::control_limit_exceeded(
			"nesting depth",
			u64::from(control.max_control_depth),
		));
	}
	if control.effect_is_limited() {
		let _admitted_effect =
			control.closure_scope(super::analysis::logical_statement_effect(statement))?;
	}
	let value = match statement {
		LogicalStatement::SetBinding {
			slot,
			expected,
			value,
		} => {
			let value = eval_query_expression(read, value, slots, source, control, runtime)?;
			super::bindings::validate_value(expected, &value, control, None)?;
			let target = slots.get_mut(*slot as usize).ok_or_else(|| {
				FerrosGrynnError::invalid_argument("ALTER LET slot is outside its typed frame")
			})?;
			*target = value;
			Value::None
		}
		LogicalStatement::Explain(explain) => {
			return explain::execute_explain(
				read, explain, slots, bindings, source, control, runtime, depth,
			);
		}
		LogicalStatement::Schema(statement) => {
			read.execute_schema(statement, slots, source, control)?
		}
		LogicalStatement::Dml(statement) => read.execute_dml(statement, slots, source, control)?,
		LogicalStatement::GraphDml(statement) => {
			read.execute_graph_dml(statement, slots, source, control)?
		}
		LogicalStatement::Let {
			name,
			slot,
			expected,
			value,
		} => {
			let reactive = matches!(value, LogicalLetValue::Live(command)
				if matches!(command.as_ref(), crate::gql::ir::LogicalSchemaStatement::DefineLiveBinding(_)));
			let value = match value {
				LogicalLetValue::Expression(value) => {
					eval_query_expression(read, value, slots, source, control, runtime)?
				}
				LogicalLetValue::Live(command) => {
					read.execute_schema(command, slots, source, control)?
				}
				LogicalLetValue::BuildGraph(build) => graph_result::execute_build_graph(
					read,
					read.snapshot()?,
					build,
					slots,
					source,
					control,
					runtime,
				)?,
			};
			super::bindings::validate_value(expected, &value, control, None)?;
			if persist_let && name.as_ref() != "_" {
				if reactive {
					read.note_reactive_binding(name, control)?;
				} else {
					read.note_binding_type(name, expected, control)?;
				}
			}
			slots[*slot as usize] = value.clone();
			if persist_let && name.as_ref() != "_" {
				bindings.insert(name.as_ref(), value);
			}
			Value::None
		}
		LogicalStatement::Return {
			value,
		} => {
			return eval_query_expression(read, value, slots, source, control, runtime)
				.map(ControlFlow::Return);
		}
		LogicalStatement::ReturnGraph(plan) => graph_result::execute_return_graph(
			read,
			read.snapshot()?,
			plan,
			slots,
			source,
			control,
		)?,
		LogicalStatement::BuildGraph(plan) => graph_result::execute_build_graph(
			read,
			read.snapshot()?,
			plan,
			slots,
			source,
			control,
			runtime,
		)?,
		LogicalStatement::Expression {
			value,
		} => eval_query_expression(read, value, slots, source, control, runtime)?,
		LogicalStatement::If {
			branches,
			else_block,
		} => {
			let mut selected = None;
			for (condition, block) in branches {
				if eval_query_expression(read, condition, slots, source, control, runtime)?
					.is_truthy()
				{
					selected = Some(block);
					break;
				}
			}
			if let Some(block) = selected.or(else_block.as_ref()) {
				return execute_block(
					read,
					block,
					slots,
					bindings,
					source,
					control,
					runtime,
					depth + 1,
				);
			}
			Value::None
		}
		LogicalStatement::For {
			slot,
			iterable,
			body,
		} => {
			let values = iterable_values(eval_query_expression(
				read, iterable, slots, source, control, runtime,
			)?)?;
			let mut last = Value::None;
			for value in values {
				control.check()?;
				record_iteration(runtime, control)?;
				slots[*slot as usize] = value;
				match execute_block(
					read,
					body,
					slots,
					bindings,
					source,
					control,
					runtime,
					depth + 1,
				)? {
					ControlFlow::Value(value) => last = value,
					ControlFlow::Break => break,
					ControlFlow::Continue => continue,
					flow @ ControlFlow::Return(_) => return Ok(flow),
				}
			}
			last
		}
		LogicalStatement::While {
			condition,
			body,
		} => {
			let mut last = Value::None;
			while eval_query_expression(read, condition, slots, source, control, runtime)?
				.is_truthy()
			{
				control.check()?;
				record_iteration(runtime, control)?;
				match execute_block(
					read,
					body,
					slots,
					bindings,
					source,
					control,
					runtime,
					depth + 1,
				)? {
					ControlFlow::Value(value) => last = value,
					ControlFlow::Break => break,
					ControlFlow::Continue => continue,
					flow @ ControlFlow::Return(_) => return Ok(flow),
				}
			}
			last
		}
		LogicalStatement::Break => return Ok(ControlFlow::Break),
		LogicalStatement::Continue => return Ok(ControlFlow::Continue),
		LogicalStatement::Try {
			body,
			catch,
		} => {
			let savepoint = read.savepoint();
			let saved_slots = slots.to_vec();
			let saved_bindings = bindings.clone();
			match execute_block(read, body, slots, bindings, source, control, runtime, depth + 1) {
				Ok(flow) => return Ok(flow),
				Err(error) if catch.as_ref().is_some_and(|_| catchable(&error)) => {
					read.restore_savepoint(savepoint)?;
					slots.clone_from_slice(&saved_slots);
					*bindings = saved_bindings;
					let (slot, handler) = catch.as_ref().ok_or_else(|| {
						FerrosGrynnError::invalid_argument("CATCH handler is missing.")
					})?;
					slots[*slot as usize] =
						Value::Error(Box::new(crate::values::ErrorValue::from_error(&error)));
					return execute_block(
						read,
						handler,
						slots,
						bindings,
						source,
						control,
						runtime,
						depth + 1,
					);
				}
				Err(error) => return Err(error),
			}
		}
		LogicalStatement::Throw {
			value,
		} => {
			let value = eval_query_expression(read, value, slots, source, control, runtime)?;
			let Value::String(message) = value else {
				return Err(FerrosGrynnError::invalid_argument("THROW requires a string value."));
			};
			return Err(FerrosGrynnError::user_thrown(message.to_string()));
		}
		LogicalStatement::Sleep(duration) => {
			let duration = eval_query_duration(read, duration, slots, source, control, "SLEEP")?;
			cooperative_sleep(duration, control)?;
			Value::None
		}
		LogicalStatement::AssertInvariants {
			graph,
			report,
		} => {
			let checked = read.assert_invariants(graph.as_ref(), *report)?;
			let mut value = Object::new();
			value.insert("checked_graphs", i64::try_from(checked).unwrap_or(i64::MAX));
			value.insert("violation_total", 0_i64);
			value.insert("violation_totals", Value::Object(Object::new()));
			value.insert("violation_details", Value::Array(Array::new(Vec::new())));
			Value::Object(value)
		}
		LogicalStatement::Match(plan) => {
			execute_match(read, read.snapshot()?, plan, slots, source, control)?
		}
		LogicalStatement::Select(select) => {
			execute_select(read, read.snapshot()?, select, slots, source, control)?
		}
		LogicalStatement::Traverse(traverse) => traversal::execute_traverse(
			read,
			read.snapshot()?,
			traverse,
			slots,
			source,
			control,
			runtime,
		)?,
	};
	control.check()?;
	Ok(ControlFlow::Value(value))
}

/// Both duration consumers borrow from the same lexical or reactive read frame.
fn eval_query_duration<Read: QueryRead>(
	read: &Read,
	duration: &crate::gql::ir::LogicalDuration,
	slots: &[Value],
	source: &str,
	control: &ExecutionControl,
	subject: &str,
) -> FerrosGrynnResult<Duration> {
	if matches!(
		duration,
		crate::gql::ir::LogicalDuration::Parameter(TypedExpr {
			kind: TypedExprKind::LiveSlot(_),
			..
		})
	) {
		let fields = cursor::RecordFields {
			read,
			snapshot: read.snapshot()?,
			graph: None,
		};
		eval::eval_duration(duration, slots, source, Some(&fields), control, subject)
	} else {
		eval::eval_duration(duration, slots, source, read.bounded_fields(), control, subject)
	}
}

fn cooperative_sleep(duration: Duration, control: &ExecutionControl) -> FerrosGrynnResult<()> {
	let deadline = Instant::now().checked_add(duration).ok_or_else(|| {
		FerrosGrynnError::invalid_argument("SLEEP duration is too large for this platform.")
	})?;
	loop {
		control.check()?;
		let now = Instant::now();
		if now >= deadline {
			return Ok(());
		}
		std::thread::sleep(deadline.saturating_duration_since(now).min(Duration::from_millis(10)));
	}
}

fn eval_query_expression<Read: QueryRead>(
	read: &mut Read,
	expression: &TypedExpr,
	slots: &mut [Value],
	source: &str,
	control: &ExecutionControl,
	runtime: &mut ControlRuntime,
) -> FerrosGrynnResult<Value> {
	eval_query_expression_current(read, expression, slots, source, control, runtime, None, None)
}

/// Preserves a candidate row through transaction-aware expression recursion.
#[allow(
	clippy::too_many_arguments,
	reason = "one shared evaluator retains query, row, lexical and collection-filter context"
)]
fn eval_query_expression_current<Read: QueryRead>(
	read: &mut Read,
	expression: &TypedExpr,
	slots: &mut [Value],
	source: &str,
	control: &ExecutionControl,
	runtime: &mut ControlRuntime,
	current: Option<&Value>,
	row: Option<&Row>,
) -> FerrosGrynnResult<Value> {
	if !query_expression_requires_read(expression) {
		let fields = cursor::RecordFields {
			read,
			snapshot: read.snapshot()?,
			graph: read.scalar_graph(),
		};
		return eval_mut_current(
			expression,
			row,
			slots,
			source,
			Some(&fields),
			current,
			Some(control),
		);
	}
	match &expression.kind {
		TypedExprKind::LiveSlot(_) => {
			let fields = cursor::RecordFields {
				read,
				snapshot: read.snapshot()?,
				graph: read.scalar_graph(),
			};
			eval_mut_current(expression, row, slots, source, Some(&fields), current, Some(control))
		}
		TypedExprKind::Traversal(plan) => traversal::execute_traverse(
			read,
			read.snapshot()?,
			plan,
			slots,
			source,
			control,
			runtime,
		),
		TypedExprKind::Invoke {
			callee,
			arguments,
		} => {
			let callee = eval_query_expression_current(
				read, callee, slots, source, control, runtime, current, row,
			)?;
			let Value::Closure(closure) = callee else {
				return Err(FerrosGrynnError::invalid_argument(
					"Only a closure value can be invoked.",
				));
			};
			let mut values = Vec::with_capacity(arguments.len());
			for argument in arguments {
				values.push(eval_query_expression_current(
					read, argument, slots, source, control, runtime, current, row,
				)?);
			}
			invoke_closure(
				read,
				&closure,
				&values,
				source,
				control,
				runtime,
				ClosureInvocationMode::Standalone,
			)
		}
		TypedExprKind::NamedInvoke {
			function,
			arguments,
		} => {
			let mut values = Vec::with_capacity(arguments.len());
			for argument in arguments {
				values.push(eval_query_expression_current(
					read, argument, slots, source, control, runtime, current, row,
				)?);
			}
			let plan = control.resolve_named_function(*function)?;
			invoke_logical_closure_with_read(
				read,
				&plan,
				&[],
				&values,
				source,
				control,
				runtime,
				ClosureInvocationMode::Standalone,
			)
		}
		TypedExprKind::Array(values) => values
			.iter()
			.map(|value| {
				eval_query_expression_current(
					read, value, slots, source, control, runtime, current, row,
				)
			})
			.collect::<FerrosGrynnResult<Array>>()
			.map(Value::Array),
		TypedExprKind::Set(values) => values
			.iter()
			.map(|value| {
				eval_query_expression_current(
					read, value, slots, source, control, runtime, current, row,
				)
			})
			.collect::<FerrosGrynnResult<crate::values::Set>>()
			.map(Value::Set),
		TypedExprKind::Object(fields) => fields
			.iter()
			.map(|(name, value)| {
				eval_query_expression_current(
					read, value, slots, source, control, runtime, current, row,
				)
				.map(|value| (name.as_ref(), value))
			})
			.collect::<FerrosGrynnResult<Object>>()
			.map(Value::Object),
		TypedExprKind::Binary {
			left,
			op,
			right,
		} => {
			let left_value = eval_query_expression_current(
				read, left, slots, source, control, runtime, current, row,
			)?;
			match op {
				BinaryOperator::And if !left_value.is_truthy() => Ok(Value::Bool(false)),
				BinaryOperator::Or if left_value.is_truthy() => Ok(Value::Bool(true)),
				BinaryOperator::Coalesce if !matches!(left_value, Value::None | Value::Null) => {
					Ok(left_value)
				}
				BinaryOperator::Elvis if left_value.is_truthy() => Ok(left_value),
				_ => {
					let right_value = eval_query_expression_current(
						read, right, slots, source, control, runtime, current, row,
					)?;
					let materialized = TypedExpr {
						kind: TypedExprKind::Binary {
							left: Box::new(query_constant(left, left_value)),
							op: *op,
							right: Box::new(query_constant(right, right_value)),
						},
						ty: expression.ty.clone(),
						span: expression.span,
					};
					eval_mut_current(
						&materialized,
						row,
						slots,
						source,
						read.bounded_fields(),
						current,
						Some(control),
					)
				}
			}
		}
		TypedExprKind::Conditional {
			condition,
			then_value,
			else_value,
		} => {
			if eval_query_expression_current(
				read, condition, slots, source, control, runtime, current, row,
			)?
			.is_truthy()
			{
				eval_query_expression_current(
					read, then_value, slots, source, control, runtime, current, row,
				)
			} else {
				eval_query_expression_current(
					read, else_value, slots, source, control, runtime, current, row,
				)
			}
		}
		TypedExprKind::Case {
			operand,
			branches,
			else_value,
		} => {
			let operand = operand
				.as_deref()
				.map(|value| {
					eval_query_expression_current(
						read, value, slots, source, control, runtime, current, row,
					)
				})
				.transpose()?;
			for (condition, value) in branches {
				let condition = eval_query_expression_current(
					read, condition, slots, source, control, runtime, current, row,
				)?;
				if operand
					.as_ref()
					.map_or_else(|| condition.is_truthy(), |operand| operand == &condition)
				{
					return eval_query_expression_current(
						read, value, slots, source, control, runtime, current, row,
					);
				}
			}
			else_value.as_deref().map_or(Ok(Value::None), |value| {
				eval_query_expression_current(
					read, value, slots, source, control, runtime, current, row,
				)
			})
		}
		TypedExprKind::Filter {
			base,
			predicate,
		} => {
			let Value::Array(values) = eval_query_expression_current(
				read, base, slots, source, control, runtime, current, row,
			)?
			else {
				return Err(FerrosGrynnError::invalid_argument(
					"Collection filter requires an array.",
				));
			};
			let mut filtered = Array::new(Vec::new());
			for value in values {
				if eval_query_expression_current(
					read,
					predicate,
					slots,
					source,
					control,
					runtime,
					Some(&value),
					row,
				)?
				.is_truthy()
				{
					filtered.push(value);
				}
			}
			Ok(Value::Array(filtered))
		}
		TypedExprKind::Builtin {
			function,
			arguments,
		} => {
			let mutable_first = matches!(
				function,
				Builtin::ArrayPush(_)
					| Builtin::ArrayPop(_)
					| Builtin::ArrayClear(_)
					| Builtin::SetAdd(_)
					| Builtin::SetClear(_)
			);
			let mut materialized = Vec::with_capacity(arguments.len());
			for (index, argument) in arguments.iter().enumerate() {
				if mutable_first && index == 0 {
					materialized.push(argument.clone());
				} else {
					let value = eval_query_expression_current(
						read, argument, slots, source, control, runtime, current, row,
					)?;
					materialized.push(query_constant(argument, value));
				}
			}
			let expression = TypedExpr {
				kind: TypedExprKind::Builtin {
					function: function.clone(),
					arguments: materialized,
				},
				ty: expression.ty.clone(),
				span: expression.span,
			};
			if matches!(function, Builtin::PathCostWith(_)) {
				let fields = RecordFields {
					read,
					snapshot: read.snapshot()?,
					graph: read.scalar_graph(),
				};
				return eval_mut_current(
					&expression,
					row,
					slots,
					source,
					Some(&fields),
					current,
					Some(control),
				);
			}
			eval_mut_current(
				&expression,
				row,
				slots,
				source,
				read.bounded_fields(),
				current,
				Some(control),
			)
		}
		TypedExprKind::Range {
			start,
			end,
			inclusive_end,
		} => {
			let start = start
				.as_deref()
				.map(|value| {
					eval_query_expression_current(
						read, value, slots, source, control, runtime, current, row,
					)
					.map(|result| Box::new(query_constant(value, result)))
				})
				.transpose()?;
			let end = end
				.as_deref()
				.map(|value| {
					eval_query_expression_current(
						read, value, slots, source, control, runtime, current, row,
					)
					.map(|result| Box::new(query_constant(value, result)))
				})
				.transpose()?;
			let expression = TypedExpr {
				kind: TypedExprKind::Range {
					start,
					end,
					inclusive_end: *inclusive_end,
				},
				ty: expression.ty.clone(),
				span: expression.span,
			};
			eval_mut_current(
				&expression,
				row,
				slots,
				source,
				read.bounded_fields(),
				current,
				Some(control),
			)
		}
		TypedExprKind::Unary {
			op,
			expr,
		} => {
			let value = eval_query_expression_current(
				read, expr, slots, source, control, runtime, current, row,
			)?;
			let expression = TypedExpr {
				kind: TypedExprKind::Unary {
					op: *op,
					expr: Box::new(query_constant(expr, value)),
				},
				ty: expression.ty.clone(),
				span: expression.span,
			};
			eval_mut_current(
				&expression,
				row,
				slots,
				source,
				read.bounded_fields(),
				current,
				Some(control),
			)
		}
		TypedExprKind::Field {
			base,
			field,
			null_safe,
		} => {
			let value = eval_query_expression_current(
				read, base, slots, source, control, runtime, current, row,
			)?;
			let fields = RecordFields {
				read,
				snapshot: read.snapshot()?,
				graph: read.scalar_graph(),
			};
			let expression = TypedExpr {
				kind: TypedExprKind::Field {
					base: Box::new(query_constant(base, value)),
					field: field.clone(),
					null_safe: *null_safe,
				},
				ty: expression.ty.clone(),
				span: expression.span,
			};
			eval_mut_current(&expression, row, slots, source, Some(&fields), current, Some(control))
		}
		TypedExprKind::Destructure {
			base,
			fields,
		} => {
			let value = eval_query_expression_current(
				read, base, slots, source, control, runtime, current, row,
			)?;
			let expression = TypedExpr {
				kind: TypedExprKind::Destructure {
					base: Box::new(query_constant(base, value)),
					fields: fields.clone(),
				},
				ty: expression.ty.clone(),
				span: expression.span,
			};
			eval_mut_current(
				&expression,
				row,
				slots,
				source,
				read.bounded_fields(),
				current,
				Some(control),
			)
		}
		TypedExprKind::Index {
			base,
			index,
		} => {
			let base_value = eval_query_expression_current(
				read, base, slots, source, control, runtime, current, row,
			)?;
			let index_value = eval_query_expression_current(
				read, index, slots, source, control, runtime, current, row,
			)?;
			let expression = TypedExpr {
				kind: TypedExprKind::Index {
					base: Box::new(query_constant(base, base_value)),
					index: Box::new(query_constant(index, index_value)),
				},
				ty: expression.ty.clone(),
				span: expression.span,
			};
			eval_mut_current(
				&expression,
				row,
				slots,
				source,
				read.bounded_fields(),
				current,
				Some(control),
			)
		}
		TypedExprKind::Cast {
			target,
			expr,
		} => {
			let value = eval_query_expression_current(
				read, expr, slots, source, control, runtime, current, row,
			)?;
			let expression = TypedExpr {
				kind: TypedExprKind::Cast {
					target: target.clone(),
					expr: Box::new(query_constant(expr, value)),
				},
				ty: expression.ty.clone(),
				span: expression.span,
			};
			eval_mut_current(
				&expression,
				row,
				slots,
				source,
				read.bounded_fields(),
				current,
				Some(control),
			)
		}
		_ => {
			let fields = cursor::RecordFields {
				read,
				snapshot: read.snapshot()?,
				graph: read.scalar_graph(),
			};
			eval_mut_current(expression, row, slots, source, Some(&fields), current, Some(control))
		}
	}
}

fn query_constant(expression: &TypedExpr, value: Value) -> TypedExpr {
	TypedExpr {
		kind: TypedExprKind::Constant(value),
		ty: expression.ty.clone(),
		span: expression.span,
	}
}

fn query_expression_requires_read(expression: &TypedExpr) -> bool {
	match &expression.kind {
		TypedExprKind::LiveSlot(_) => true,
		TypedExprKind::Invoke {
			..
		}
		| TypedExprKind::NamedInvoke {
			..
		}
		| TypedExprKind::Traversal(_)
		| TypedExprKind::Fulltext {
			..
		}
		| TypedExprKind::Knn {
			..
		} => true,
		TypedExprKind::Array(values) | TypedExprKind::Set(values) => {
			values.iter().any(query_expression_requires_read)
		}
		TypedExprKind::Object(fields) => {
			fields.iter().any(|(_, value)| query_expression_requires_read(value))
		}
		TypedExprKind::Range {
			start,
			end,
			..
		} => {
			start.as_deref().is_some_and(query_expression_requires_read)
				|| end.as_deref().is_some_and(query_expression_requires_read)
		}
		TypedExprKind::Field {
			..
		} => true,
		TypedExprKind::Unary {
			expr,
			..
		}
		| TypedExprKind::Destructure {
			base: expr,
			..
		}
		| TypedExprKind::Cast {
			expr,
			..
		} => query_expression_requires_read(expr),
		TypedExprKind::Binary {
			left,
			right,
			..
		}
		| TypedExprKind::Index {
			base: left,
			index: right,
		}
		| TypedExprKind::Filter {
			base: left,
			predicate: right,
		} => query_expression_requires_read(left) || query_expression_requires_read(right),
		TypedExprKind::Conditional {
			condition,
			then_value,
			else_value,
		} => {
			query_expression_requires_read(condition)
				|| query_expression_requires_read(then_value)
				|| query_expression_requires_read(else_value)
		}
		TypedExprKind::Case {
			operand,
			branches,
			else_value,
		} => {
			operand.as_deref().is_some_and(query_expression_requires_read)
				|| branches.iter().any(|(condition, value)| {
					query_expression_requires_read(condition)
						|| query_expression_requires_read(value)
				})
				|| else_value.as_deref().is_some_and(query_expression_requires_read)
		}
		TypedExprKind::Builtin {
			function,
			arguments,
			..
		} => {
			matches!(function, Builtin::PathCostWith(_))
				|| arguments.iter().any(query_expression_requires_read)
		}
		TypedExprKind::Aggregate {
			argument,
			..
		} => argument.as_deref().is_some_and(query_expression_requires_read),
		TypedExprKind::Constant(_)
		| TypedExprKind::Slot(_)
		| TypedExprKind::Column(_)
		| TypedExprKind::CurrentField(_)
		| TypedExprKind::Closure(_)
		| TypedExprKind::Subquery {
			..
		} => false,
	}
}

#[allow(clippy::too_many_arguments, reason = "the interpreter keeps one concrete execution path")]
fn execute_block<Read: QueryRead>(
	read: &mut Read,
	block: &crate::gql::ir::LogicalBlock,
	slots: &mut [Value],
	bindings: &mut Object,
	source: &str,
	control: &ExecutionControl,
	runtime: &mut ControlRuntime,
	depth: u32,
) -> FerrosGrynnResult<ControlFlow> {
	let mut last = Value::None;
	for statement in &block.statements {
		let preparation = (|| {
			let timeout = statement
				.timeout
				.as_ref()
				.map(|duration| {
					eval_query_duration(read, duration, slots, source, control, "TIMEOUT")
				})
				.transpose()?;
			control.checked_child(timeout)
		})();
		let control = match preparation {
			Ok(control) => control,
			Err(error) => {
				discard_statement_cursor(&statement.statement, slots, control);
				return Err(error);
			}
		};
		match execute_statement(
			read,
			&statement.statement,
			slots,
			bindings,
			source,
			&control,
			runtime,
			depth,
			false,
		)? {
			ControlFlow::Value(value) => last = value,
			flow @ (ControlFlow::Return(_) | ControlFlow::Break | ControlFlow::Continue) => {
				return Ok(flow);
			}
		}
	}
	Ok(ControlFlow::Value(last))
}

/// Invokes a native or session-owned closure through the transaction-aware
/// evaluator used by traversal steps.
#[allow(
	clippy::too_many_arguments,
	reason = "closure invocation preserves the active query and transaction contexts"
)]
pub(in crate::gql::execution) fn invoke_closure<Read: QueryRead>(
	read: &mut Read,
	closure: &Closure,
	arguments: &[Value],
	source: &str,
	control: &ExecutionControl,
	runtime: &mut ControlRuntime,
	mode: ClosureInvocationMode,
) -> FerrosGrynnResult<Value> {
	if closure.session_owner().is_none() {
		return closure.call(arguments);
	}
	let entry = control.resolve_closure(closure)?;
	invoke_logical_closure_with_read(
		read,
		&entry.plan,
		&entry.captures,
		arguments,
		source,
		control,
		runtime,
		mode,
	)
}

/// Runs one isolated closure frame through the common transaction-aware kernel.
#[allow(
	clippy::too_many_arguments,
	reason = "named and closure calls share one transaction-aware evaluator"
)]
pub(in crate::gql::execution) fn invoke_logical_closure_with_read<Read: QueryRead>(
	read: &mut Read,
	plan: &LogicalClosure,
	captures: &[(u32, Value)],
	arguments: &[Value],
	source: &str,
	control: &ExecutionControl,
	runtime: &mut ControlRuntime,
	mode: ClosureInvocationMode,
) -> FerrosGrynnResult<Value> {
	let effect = plan.invocation_effect();
	let effect_allowed = match (effect, mode) {
		(ClosureEffect::Pure | ClosureEffect::StableRead, _) => true,
		(ClosureEffect::DataMutation, ClosureInvocationMode::TraversalSideEffect)
		| (
			ClosureEffect::DataMutation | ClosureEffect::SchemaMutation,
			ClosureInvocationMode::Standalone,
		) => true,
		(
			ClosureEffect::DataMutation | ClosureEffect::SchemaMutation,
			ClosureInvocationMode::ReadOnlyTraversal | ClosureInvocationMode::TraversalSideEffect,
		) => false,
	};
	if !effect_allowed {
		let position = match mode {
			ClosureInvocationMode::ReadOnlyTraversal => "read-only traversal step",
			ClosureInvocationMode::TraversalSideEffect => "tv::side_effect",
			ClosureInvocationMode::Standalone => "standalone closure",
		};
		return Err(FerrosGrynnError::closure_effect_not_allowed(
			format!("{effect:?}"),
			position.to_owned(),
		));
	}
	if plan.parameters.len() != arguments.len() {
		return Err(FerrosGrynnError::conversion_error(
			format!("{} closure arguments", arguments.len()),
			format!("{} arguments", plan.parameters.len()),
		));
	}
	let control = &control.closure_scope(effect)?;
	if control.infer_write_access() && effect >= ClosureEffect::DataMutation {
		read.prepare_mutating_statement()?;
	}
	let _frame =
		plan.named_function.map(|function| control.enter_named_function(function)).transpose()?;
	let mut slots = vec![Value::None; plan.slot_count];
	for (slot, value) in captures {
		let target = slots.get_mut(*slot as usize).ok_or_else(|| {
			FerrosGrynnError::invalid_argument("Closure capture slot is outside its typed frame.")
		})?;
		*target = value.clone();
	}
	for ((slot, _), value) in plan.parameters.iter().zip(arguments) {
		let target = slots.get_mut(*slot as usize).ok_or_else(|| {
			FerrosGrynnError::invalid_argument("Closure parameter slot is outside its typed frame.")
		})?;
		*target = value.clone();
	}
	let savepoint = read.savepoint();
	let result = match &plan.body {
		LogicalClosureBody::Expression(expression) => {
			if matches!(mode, ClosureInvocationMode::Standalone) {
				eval_query_expression(read, expression, &mut slots, source, control, runtime)
			} else {
				let occurrence = arguments.iter().find_map(Value::as_atom_occurrence);
				if let Some(occurrence) = occurrence {
					let snapshot = read.snapshot()?;
					let fields = MatchGraphFields::new(
						&*read,
						snapshot,
						read.property_dictionary(snapshot),
						Arc::new(occurrence.graph().clone()),
					);
					eval(expression, None, &slots, source, Some(&fields), Some(control))
				} else if query_expression_requires_read(expression) {
					eval_query_expression(read, expression, &mut slots, source, control, runtime)
				} else {
					eval(expression, None, &slots, source, None, Some(control))
				}
			}
		}
		LogicalClosureBody::Block(block) => {
			let mut bindings = Object::new();
			match execute_block(read, block, &mut slots, &mut bindings, source, control, runtime, 1)
			{
				Ok(ControlFlow::Value(value) | ControlFlow::Return(value)) => Ok(value),
				Ok(ControlFlow::Break | ControlFlow::Continue) => {
					Err(FerrosGrynnError::invalid_argument(
						"BREAK or CONTINUE cannot cross a closure boundary.",
					))
				}
				Err(error) => Err(error),
			}
		}
	};
	if let Err(error) = result {
		read.restore_savepoint(savepoint)?;
		return Err(error);
	}
	result
}

fn record_iteration(
	runtime: &mut ControlRuntime,
	control: &ExecutionControl,
) -> FerrosGrynnResult<()> {
	runtime.budget.record_iteration(control.max_loop_iterations)
}

fn iterable_values(value: Value) -> FerrosGrynnResult<IterableValues> {
	match value {
		Value::Array(values) => Ok(IterableValues::Array(values.into_iter())),
		Value::Set(values) => Ok(IterableValues::Set(values.into_iter())),
		Value::Range(range) => integer_range(range).map(IterableValues::Integer),
		value => Err(FerrosGrynnError::invalid_argument(format!(
			"FOR requires an array, set, or finite integer range, found {}.",
			value.kind()
		))),
	}
}

enum IterableValues {
	Array(std::vec::IntoIter<Value>),
	Set(std::collections::btree_set::IntoIter<Value>),
	Integer(IntegerValues),
}

impl Iterator for IterableValues {
	type Item = Value;

	fn next(&mut self) -> Option<Self::Item> {
		match self {
			Self::Array(values) => values.next(),
			Self::Set(values) => values.next(),
			Self::Integer(values) => values.next(),
		}
	}
}

struct IntegerValues {
	current: i64,
	end: i64,
	inclusive: bool,
	done: bool,
}

impl Iterator for IntegerValues {
	type Item = Value;

	fn next(&mut self) -> Option<Self::Item> {
		if self.done || self.current > self.end || self.current == self.end && !self.inclusive {
			return None;
		}
		let value = self.current;
		match self.current.checked_add(1) {
			Some(next) => self.current = next,
			None => self.done = true,
		}
		Some(Value::from(value))
	}
}

fn integer_range(range: crate::values::Range) -> FerrosGrynnResult<IntegerValues> {
	let (start, end) = range.into_bounds();
	let std::ops::Bound::Included(Value::Number(Number::Int(start))) = start else {
		return Err(FerrosGrynnError::invalid_argument(
			"FOR requires a finite integer range with an included start.",
		));
	};
	let (end, inclusive) = match end {
		std::ops::Bound::Included(Value::Number(Number::Int(end))) => (end, true),
		std::ops::Bound::Excluded(Value::Number(Number::Int(end))) => (end, false),
		_ => {
			return Err(FerrosGrynnError::invalid_argument("FOR requires a finite integer range."));
		}
	};
	Ok(IntegerValues {
		current: start,
		end,
		inclusive,
		done: false,
	})
}

fn catchable(error: &FerrosGrynnError) -> bool {
	use crate::gql_status::{Condition, Domain, ErrorClassification};
	let status = error.status();
	status.error_classification() == Some(ErrorClassification::ClientError)
		&& !matches!(
			status.domain(),
			Domain::Storage
				| Domain::Io
				| Domain::Configuration
				| Domain::Security
				| Domain::External
		)
		&& !matches!(
			status.condition(),
			Condition::ProgramLimitExceeded
				| Condition::TransactionRollback
				| Condition::InvalidTransactionState
				| Condition::InvalidTransactionTermination
				| Condition::SystemConfigurationOrOperationException
		)
}

const fn logical_statement_kind(statement: &LogicalStatement) -> QueryStatementKind {
	match statement {
		LogicalStatement::SetBinding {
			..
		} => QueryStatementKind::Alter,
		LogicalStatement::Explain(_) => QueryStatementKind::Explain,
		LogicalStatement::Schema(statement) => statement.statement_kind(),
		LogicalStatement::Dml(statement) => statement.kind.statement_kind(),
		LogicalStatement::GraphDml(statement) => statement.statement_kind(),
		LogicalStatement::Let {
			..
		} => QueryStatementKind::Let,
		LogicalStatement::Return {
			..
		} => QueryStatementKind::Return,
		LogicalStatement::ReturnGraph(_) => QueryStatementKind::ReturnGraph,
		LogicalStatement::BuildGraph(_) => QueryStatementKind::BuildGraph,
		LogicalStatement::Expression {
			..
		}
		| LogicalStatement::Break
		| LogicalStatement::Continue => QueryStatementKind::Expression,
		LogicalStatement::If {
			..
		} => QueryStatementKind::If,
		LogicalStatement::For {
			..
		} => QueryStatementKind::For,
		LogicalStatement::While {
			..
		} => QueryStatementKind::While,
		LogicalStatement::Try {
			..
		} => QueryStatementKind::Try,
		LogicalStatement::Throw {
			..
		} => QueryStatementKind::Throw,
		LogicalStatement::Sleep(_) => QueryStatementKind::Sleep,
		LogicalStatement::AssertInvariants {
			..
		} => QueryStatementKind::Assert,
		LogicalStatement::Match(_) => QueryStatementKind::Match,
		LogicalStatement::Select(_) => QueryStatementKind::Select,
		LogicalStatement::Traverse(_) => QueryStatementKind::Traverse,
	}
}

pub(in crate::gql) fn initialize_slots(program: &LogicalProgram, variables: &Object) -> Vec<Value> {
	let mut slots = vec![Value::None; program.slot_count];
	for (name, slot, _) in &program.initial_slots {
		if let Some(value) = variables.get(name) {
			slots[*slot as usize] = value.clone();
		}
	}
	slots
}

fn persist_mutable_initial_slots(
	program: &LogicalProgram,
	slots: &[Value],
	bindings: &mut Object,
) -> FerrosGrynnResult<()> {
	for (name, slot, mutable) in &program.initial_slots {
		if !mutable {
			continue;
		}
		let value = slots.get(*slot as usize).cloned().ok_or_else(|| {
			FerrosGrynnError::invalid_argument("Mutable binding slot is outside its typed frame.")
		})?;
		bindings.insert(name.as_ref(), value);
	}
	Ok(())
}

/// Resolves and validates the graph identity against the invocation snapshot.
fn resolve_match_graph<Read: QueryRead>(
	read: &Read,
	snapshot: Snapshot,
	plan: &LogicalMatch,
	slots: &[Value],
	source: &str,
	control: &ExecutionControl,
) -> FerrosGrynnResult<GraphRef> {
	let reference = match &plan.graph {
		LogicalGraphReference::Static(reference) => reference.clone(),
		LogicalGraphReference::Expression(expression) => {
			let fields = RecordFields {
				read,
				snapshot,
				graph: None,
			};
			let value = eval(expression, None, slots, source, Some(&fields), Some(control))?;
			let Value::GraphRef(reference) = value else {
				return Err(FerrosGrynnError::invalid_argument(
					"MATCH graph parameters must contain a GraphRef.",
				));
			};
			*reference
		}
	};
	let visible = read.catalog(snapshot).graph(reference.name())?;
	if visible.id() != reference.id() {
		return Err(FerrosGrynnError::graph_does_not_exist(reference.name().to_owned()));
	}
	Ok(visible)
}

/// Resolves a lexical clause bound before planning or pulling any MATCH row.
fn resolve_match_max_depth<Read: QueryRead>(
	read: &Read,
	snapshot: Snapshot,
	plan: &LogicalMatch,
	slots: &[Value],
	source: &str,
	control: &ExecutionControl,
) -> FerrosGrynnResult<u32> {
	let max_depth = if let Some(expression) = &plan.max_depth_binding {
		let fields = RecordFields {
			read,
			snapshot,
			graph: None,
		};
		let value = eval(expression, None, slots, source, Some(&fields), Some(control))?;
		crate::gql::ir::validate_match_max_depth(&value)?
	} else {
		plan.max_depth
	};
	for pattern in &plan.patterns {
		if let LogicalMatchPatternKind::Linear(pattern) = &pattern.kind {
			for operation in &pattern.ops {
				if let MatchOp::PathExpand(expand) = operation
					&& expand.min > expand.maximum(max_depth)
				{
					return Err(FerrosGrynnError::invalid_argument(
						"MATCH path minimum exceeds its maximum.",
					));
				}
			}
		}
	}
	Ok(max_depth)
}

/// Resolves an embedded deadline without replacing the caller control when
/// the MATCH has no clause.
///
/// # Errors
///
/// Returns snapshot, duration conversion, cancellation, or deadline errors.
fn resolve_match_control<Read: QueryRead>(
	read: &Read,
	snapshot: Snapshot,
	plan: &LogicalMatch,
	slots: &[Value],
	source: &str,
	control: &ExecutionControl,
) -> FerrosGrynnResult<Option<ExecutionControl>> {
	plan.timeout
		.as_ref()
		.map(|timeout| {
			let fields = RecordFields {
				read,
				snapshot,
				graph: None,
			};
			let duration =
				eval::eval_duration(timeout, slots, source, Some(&fields), control, "TIMEOUT")?;
			let child = control.checked_child(Some(duration))?;
			child.check()?;
			Ok(child)
		})
		.transpose()
}

/// Executes a logical MATCH as a synchronous pull pipeline.
fn execute_match<Read: QueryRead>(
	read: &Read,
	snapshot: Snapshot,
	plan: &LogicalMatch,
	slots: &[Value],
	source: &str,
	control: &ExecutionControl,
) -> FerrosGrynnResult<Value> {
	execute_match_with_environment(read, snapshot, plan, slots, source, control, &Object::new())
}

fn execute_match_with_environment<Read: QueryRead>(
	read: &Read,
	snapshot: Snapshot,
	plan: &LogicalMatch,
	slots: &[Value],
	source: &str,
	control: &ExecutionControl,
	environment: &Object,
) -> FerrosGrynnResult<Value> {
	let mut result = Array::new(Vec::new());
	if let Some(value) = visit_match_with_environment(
		read,
		snapshot,
		plan,
		slots,
		source,
		control,
		environment,
		&mut |value| {
			result.push(value);
			Ok(())
		},
	)? {
		return Ok(value);
	}
	Ok(Value::Array(result))
}

#[allow(
	clippy::too_many_arguments,
	reason = "MATCH pull execution retains the analyzed statement and lexical environment"
)]
fn visit_match_with_environment<Read: QueryRead>(
	read: &Read,
	snapshot: Snapshot,
	plan: &LogicalMatch,
	slots: &[Value],
	source: &str,
	control: &ExecutionControl,
	environment: &Object,
	visit: &mut impl FnMut(Value) -> FerrosGrynnResult<()>,
) -> FerrosGrynnResult<Option<Value>> {
	let invocation_control = resolve_match_control(read, snapshot, plan, slots, source, control)?;
	let control = invocation_control.as_ref().unwrap_or(control);
	let reference = resolve_match_graph(read, snapshot, plan, slots, source, control)?;
	let max_depth = resolve_match_max_depth(read, snapshot, plan, slots, source, control)?;
	let demand = crate::gql::optimizer::PlannerDemand::for_match_bound(plan, reference.id());
	let planner_snapshot = read.planner_snapshot(snapshot, &demand)?;
	let deterministic_snapshot;
	let planner_snapshot = if control.deterministic() {
		deterministic_snapshot = planner_snapshot.deterministic_projection();
		&deterministic_snapshot
	} else {
		&planner_snapshot
	};
	let physical = crate::gql::optimizer::plan_match_bound(
		plan,
		planner_snapshot,
		control.deterministic(),
		Some(&reference),
		max_depth,
	)?;
	control.record_match_graph_open();
	let graph = Rc::new(read.graph(snapshot, &reference)?);
	let graph_ref = Arc::new(reference);
	let (needs_local_index, needs_scoped_index) = match_adjacency_requirements(plan, &physical);
	let semantics = if plan.atom || plan.patterns.iter().any(pattern_uses_edge_endpoint) {
		TraversalSemantics::Atom
	} else {
		TraversalSemantics::Node
	};
	let index = needs_local_index
		.then(|| TraversalIndex::new(graph.as_ref(), semantics).map(Rc::new))
		.transpose()?;
	let fields = MatchGraphFields::new(
		read,
		snapshot,
		read.property_dictionary(snapshot),
		graph_ref.clone(),
	);
	let scoped_index = needs_scoped_index
		.then(|| {
			ScopedTraversalIndex::build(
				read,
				snapshot,
				graph_ref.clone(),
				&fields,
				semantics,
				control,
			)
			.map(Rc::new)
		})
		.transpose()?;
	let match_predicate = MatchPredicateContext {
		slots,
		source,
		fields: &fields,
	};
	let mut seed = Row::bindings(plan.bindings.columns.len());
	let mut seed_is_valid = true;
	for (column, binding) in plan.bindings.columns.iter().enumerate() {
		if let Some(value) = environment.get(&binding.name) {
			let Value::AtomOccurrence(occurrence) = value else {
				seed.values[column] = value.clone();
				continue;
			};
			if occurrence.graph().id() != graph_ref.id() {
				seed.values[column] = value.clone();
				continue;
			}
			let Some(atom) = graph.atom_id(occurrence.atom())? else {
				seed.values[column] = value.clone();
				continue;
			};
			seed_is_valid &= seed.seed_binding(
				column,
				value.clone(),
				BoundAtom::new(graph_ref.id(), atom),
				plan.mode,
			);
		}
	}
	let mut cursor: Box<dyn RowCursor + '_> = Box::new(OneRowCursor {
		row: seed_is_valid.then_some(seed),
	});
	for pattern_index in physical.pattern_order.iter().copied() {
		let logical = &plan.patterns[pattern_index];
		match &logical.kind {
			LogicalMatchPatternKind::Linear(pattern) => {
				let trace = pattern
					.path_column
					.map(|_column| Rc::new(RefCell::new(MatchPathTrace::new(control))));
				let start = resolve_match_binding(&pattern.start, match_predicate, control)?;
				let candidates = read.match_candidates(snapshot, &graph_ref, &start)?;
				cursor = Box::new(SeedCursor::new(
					cursor,
					graph.clone(),
					graph_ref.clone(),
					start,
					plan.mode,
					match_predicate,
					candidates,
				));
				if let Some(arena) = &trace {
					cursor = Box::new(PathTraceStartCursor {
						input: cursor,
						graph: graph.clone(),
						graph_ref: graph_ref.clone(),
						arena: arena.clone(),
					});
				}
				for (operation_index, op) in pattern.ops.iter().enumerate() {
					let operator =
						physical.operation(pattern_index, operation_index).ok_or_else(|| {
							FerrosGrynnError::invalid_argument(
								"MATCH physical plan has no operator for a linear expansion.",
							)
						})?;
					let scoped = physical.operation_is_scoped(pattern_index, operation_index);
					cursor = match op {
						MatchOp::Expand(expand) => {
							let expand =
								resolve_expand(expand, match_predicate, control, plan.mode)?;
							if !matches!(
								operator,
								crate::gql::optimizer::MatchOperator::FixedExpand { .. }
									| crate::gql::optimizer::MatchOperator::AtomExpand { .. }
									| crate::gql::optimizer::MatchOperator::HyperExpand { .. }
							) {
								return Err(FerrosGrynnError::invalid_argument(
									"MATCH physical plan selected a non-fixed operator for a \
									 fixed expansion.",
								));
							}
							if scoped {
								let scoped_index = scoped_index.as_ref().ok_or_else(|| {
									FerrosGrynnError::invalid_argument(
										"MATCH physical plan requires scoped adjacency.",
									)
								})?;
								Box::new(CrossLevelExpandCursor::new(
									cursor,
									read,
									snapshot,
									&fields,
									scoped_index.clone(),
									expand,
									match_predicate,
									trace.clone(),
								))
							} else if let Some(index) = &index {
								Box::new(IndexExpandCursor::new(
									cursor,
									graph.clone(),
									graph_ref.clone(),
									index.clone(),
									expand,
									match_predicate,
									trace.clone(),
								))
							} else {
								Box::new(EdgeExpandCursor::new(
									cursor,
									graph.clone(),
									graph_ref.clone(),
									expand,
									match_predicate,
									trace.clone(),
								)?)
							}
						}
						MatchOp::PathExpand(expand) => {
							let expand = resolve_path_expand(
								expand,
								match_predicate,
								control,
								plan.mode,
								max_depth,
							)?;
							let shortest = matches!(
								operator,
								crate::gql::optimizer::MatchOperator::BreadthFirstShortest { .. }
									| crate::gql::optimizer::MatchOperator::DijkstraShortest { .. }
									| crate::gql::optimizer::MatchOperator::LandmarkShortest { .. }
									| crate::gql::optimizer::MatchOperator::TensorAllShortest { .. }
							);
							if !shortest
								&& !matches!(
									operator,
									crate::gql::optimizer::MatchOperator::PathEnumerate { .. }
								) {
								return Err(FerrosGrynnError::invalid_argument(
									"MATCH physical plan selected a non-path operator for a path \
									 expansion.",
								));
							}
							if expand.objective.is_some_and(crate::syn::MatchPathObjective::is_all)
							{
								Box::new(TensorShortestPathCursor::new(
									cursor,
									read,
									snapshot,
									&fields,
									graph_ref.clone(),
									expand,
									match_predicate,
									trace.clone(),
									semantics,
									if scoped {
										crate::graph::tensor::TensorScope::Rooted
									} else {
										crate::graph::tensor::TensorScope::Local
									},
									true,
									std::collections::BTreeSet::new(),
									1,
									match operator { crate::gql::optimizer::MatchOperator::TensorAllShortest { projection, .. } => projection.binding.clone(), _ => None },
								))
							} else if scoped {
								let scoped_index = scoped_index.as_ref().ok_or_else(|| {
									FerrosGrynnError::invalid_argument(
										"MATCH physical plan requires scoped adjacency.",
									)
								})?;
								if shortest {
									Box::new(ScopedShortestPathCursor::new(
										cursor,
										read,
										snapshot,
										&fields,
										scoped_index.clone(),
										expand,
										match_predicate,
										trace.clone(),
										graph.identity(),
										graph.snapshot_id(),
									))
								} else {
									Box::new(ScopedPathExpandCursor::new(
										cursor,
										read,
										snapshot,
										&fields,
										scoped_index.clone(),
										expand,
										match_predicate,
										trace.clone(),
										graph.identity(),
										graph.snapshot_id(),
									))
								}
							} else if shortest {
								let index = index.clone().ok_or_else(|| {
									FerrosGrynnError::invalid_argument(
										"MATCH path expansion requires a traversal index.",
									)
								})?;
								Box::new(ShortestPathCursor::new(
									cursor,
									graph.clone(),
									graph_ref.clone(),
									index,
									expand,
									match_predicate,
									trace.clone(),
									read,
									snapshot,
									match operator { crate::gql::optimizer::MatchOperator::LandmarkShortest { binding, .. } => Some(binding.clone()), _ => None },
								))
							} else {
								let index = index.clone().ok_or_else(|| {
									FerrosGrynnError::invalid_argument(
										"MATCH path expansion requires a traversal index.",
									)
								})?;
								Box::new(PathExpandCursor::new(
									cursor,
									graph.clone(),
									graph_ref.clone(),
									index,
									expand,
									match_predicate,
									trace.clone(),
								))
							}
						}
					};
				}
				if let (Some(column), Some(arena)) = (pattern.path_column, trace) {
					cursor = Box::new(PathBindCursor {
						input: cursor,
						arena,
						column,
						origin: graph.identity(),
						snapshot: graph.snapshot_id(),
					});
				}
			}
			LogicalMatchPatternKind::Hyper(pattern) => {
				let edge = resolve_match_binding(&pattern.edge, match_predicate, control)?;
				let contains = pattern
					.contains
					.iter()
					.map(|group| {
						Ok(ResolvedHyperContains {
							side: group.side,
							endpoints: group
								.endpoints
								.iter()
								.map(|binding| {
									resolve_match_binding(binding, match_predicate, control)
								})
								.collect::<FerrosGrynnResult<Vec<_>>>()?,
						})
					})
					.collect::<FerrosGrynnResult<Vec<_>>>()?;
				cursor = Box::new(HyperContainsCursor::new(
					cursor,
					read,
					snapshot,
					&fields,
					graph.clone(),
					graph_ref.clone(),
					edge,
					contains,
					plan.mode,
					match_predicate,
				)?);
			}
			LogicalMatchPatternKind::Within(pattern) => {
				let relation = ResolvedWithin {
					child: resolve_match_binding(&pattern.child, match_predicate, control)?,
					owner: resolve_match_binding(&pattern.owner, match_predicate, control)?,
					min: pattern.min,
					max: pattern.max,
				};
				cursor = Box::new(WithinCursor::new(
					cursor,
					read,
					snapshot,
					graph_ref.clone(),
					&fields,
					relation,
					plan.mode,
					match_predicate,
				));
			}
		}
	}
	if let Some(predicate) = &plan.filter {
		cursor = Box::new(FilterCursor {
			input: cursor,
			predicate,
			slots,
			source,
			fields: Some(&fields),
		});
	}
	if plan.optional {
		cursor = Box::new(OptionalCursor {
			input: cursor,
			fallback: Some(Row::bindings(plan.bindings.columns.len())),
			yielded: false,
		});
	}
	if plan.binding_distinct {
		cursor = distinct_rows(read, snapshot, cursor, control)?;
	}
	if let Some(graph_plan) = &plan.return_graph {
		let mut assembly = graph_result::GraphAssemblyAccumulator::new(
			Some(graph_ref.as_ref().clone()),
			None,
			None,
			control,
		);
		while let Some(row) = cursor.next(control).map_err(execution_phase)? {
			assembly.record_source_row();
			assembly.add_selectors(
				Some(&graph_plan.nodes),
				Some(&graph_plan.edges),
				graph_plan.root.as_ref(),
				|expression| {
					eval(expression, Some(&row), slots, source, Some(&fields), Some(control))
				},
			)?;
		}
		let graph = assembly.finish(read, snapshot, graph_plan.contract)?;
		invocation_control.as_ref().map_or(Ok(()), ExecutionControl::check)?;
		return Ok(Some(Value::Graph(Box::new(graph))));
	}
	let random_seed = plan.order.iter().any(|order| order.random).then(|| {
		control.statement_seed(
			crate::gql::optimizer::PhysicalPlan::Match(physical.clone()).hash(),
			snapshot.0,
		)
	});
	cursor = Box::new(MatchProjectCursor {
		input: cursor,
		expressions: &plan.projection,
		order: &plan.order,
		slots,
		source,
		fields: &fields,
		random_seed,
		ordinal: 0,
	});
	if plan.projection_distinct {
		cursor = distinct_rows(read, snapshot, cursor, control)?;
	}
	if !plan.order.is_empty() || control.deterministic() {
		cursor = sort_match_rows(read, snapshot, cursor, &plan.order, control)?;
	}

	while let Some(row) = cursor.next(control).map_err(execution_phase)? {
		let object = plan
			.result
			.columns
			.iter()
			.zip(row.values)
			.map(|(column, value)| (column.name.as_ref(), value))
			.collect::<Object>();
		visit(Value::Object(object))?;
	}
	invocation_control.as_ref().map_or(Ok(()), ExecutionControl::check)?;
	Ok(None)
}

/// Builds legacy adjacency only for operations that consume it.
fn match_adjacency_requirements(
	plan: &LogicalMatch,
	physical: &crate::gql::optimizer::PhysicalMatchPlan,
) -> (bool, bool) {
	let mut local = false;
	let mut scoped = false;
	for (pattern_index, pattern) in plan.patterns.iter().enumerate() {
		let LogicalMatchPatternKind::Linear(pattern) = &pattern.kind else {
			continue;
		};
		for (operation_index, operation) in pattern.ops.iter().enumerate() {
			if matches!(operation, MatchOp::PathExpand(expand) if expand.objective.is_some_and(crate::syn::MatchPathObjective::is_all))
			{
				continue;
			}
			if physical.operation_is_scoped(pattern_index, operation_index) {
				scoped = true;
			} else {
				local |= matches!(operation, MatchOp::PathExpand(_)) || pattern.ops.len() > 1;
			}
		}
	}
	(local, scoped)
}

/// Reports whether a pattern requires AtomWalk traversal semantics.
fn pattern_uses_edge_endpoint(pattern: &LogicalMatchPattern) -> bool {
	match &pattern.kind {
		LogicalMatchPatternKind::Linear(pattern) => {
			pattern.start.domain == EndpointDomain::Edge
				|| pattern.ops.iter().any(|op| match op {
					MatchOp::Expand(expand) => {
						expand.source.domain == EndpointDomain::Edge
							|| expand.target.domain == EndpointDomain::Edge
					}
					MatchOp::PathExpand(expand) => {
						expand.source.domain == EndpointDomain::Edge
							|| expand.target.domain == EndpointDomain::Edge
					}
				})
		}
		LogicalMatchPatternKind::Hyper(pattern) => pattern
			.contains
			.iter()
			.flat_map(|group| &group.endpoints)
			.any(|binding| binding.domain == EndpointDomain::Edge),
		LogicalMatchPatternKind::Within(pattern) => {
			pattern.child.domain == EndpointDomain::Edge
				|| pattern.owner.domain == EndpointDomain::Edge
		}
	}
}

/// Reports whether a linear pattern explicitly requests cross-level branches.
fn pattern_requires_scoped(pattern: &LogicalMatchPattern) -> bool {
	let LogicalMatchPatternKind::Linear(pattern) = &pattern.kind else {
		return false;
	};
	pattern.ops.iter().any(|operation| match operation {
		MatchOp::Expand(expand) => {
			expand.filters.contains(&crate::syn::MatchPathFilter::CrossLevel)
		}
		MatchOp::PathExpand(expand) => {
			expand.filters.contains(&crate::syn::MatchPathFilter::CrossLevel)
		}
	})
}

#[cfg(test)]
struct VecCursor {
	rows: std::vec::IntoIter<Row>,
}

#[cfg(test)]
impl VecCursor {
	fn new(rows: Vec<Row>) -> Self {
		Self {
			rows: rows.into_iter(),
		}
	}
}

#[cfg(test)]
impl RowCursor for VecCursor {
	fn next(&mut self, control: &ExecutionControl) -> FerrosGrynnResult<Option<Row>> {
		control.check()?;
		Ok(self.rows.next())
	}
}

#[cfg(test)]
mod tests;
