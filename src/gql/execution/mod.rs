#![allow(
	clippy::pattern_type_mismatch,
	reason = "the executor intentionally uses match ergonomics for borrowed IR and runtime values"
)]

mod adapter;
mod builtins;
mod cursor;
mod dml;
mod eval;
mod explain;
mod graph_properties;
mod schema;
mod select;
mod traversal;

use std::{cell::RefCell, rc::Rc, sync::Arc, time::Instant};

use adapter::TransactionRead;
#[cfg(test)]
pub(crate) use adapter::execute_parsed_transaction_with_bindings;
pub(crate) use adapter::{analyze_parsed_transaction, execute_parsed_statement_with_bindings};
pub use adapter::{execute_transaction, execute_transaction_with_options};
use dml::{execute_dml, execute_graph_dml};
use rust_decimal::Decimal;
use schema::execute_schema;
use select::execute_select;
#[allow(
	clippy::wildcard_imports,
	reason = "move-only executor split keeps the existing cursor and evaluator names during stage \
	          1"
)]
use {cursor::*, eval::*};

pub(in crate::gql) fn evaluate_constant(expression: &TypedExpr) -> Option<Value> {
	eval(expression, None, &[], "", None, None).ok()
}

use super::{
	QueryResult,
	QueryStatementKind,
	analysis::{QueryCatalog, analyze, analyze_statement, compatible},
	ir::{
		Aggregate,
		Builtin,
		ClosureEffect,
		Column,
		EndpointDomain,
		Expand,
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
	control: &ExecutionControl,
) -> FerrosGrynnResult<QueryResult> {
	let snapshot = read.snapshot()?;
	let logical = analyze_statement(statement, source, read.catalog(snapshot), bindings)?;
	let mut slots = initialize_slots(&logical, bindings);
	let statement = logical.statements.first().ok_or_else(|| {
		FerrosGrynnError::invalid_argument("Analyzed statement produced no executable IR.")
	})?;
	execute_logical_statement(read, source, statement, &mut slots, bindings, control)
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
		let result =
			execute_logical_statement(read, source, statement, &mut slots, bindings, control)?;
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
) -> FerrosGrynnResult<QueryResult> {
	if control.deterministic() {
		crate::gql::optimizer::validate_deterministic_statement(statement)?;
	}
	let start = Instant::now();
	let kind = logical_statement_kind(statement);
	let mut runtime = ControlRuntime::default();
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
		elapsed: start.elapsed(),
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
) -> FerrosGrynnResult<()> {
	if logical.statements.len() != timeouts.len() {
		return Err(FerrosGrynnError::invalid_argument(
			"Import execution metadata does not match the analyzed program.",
		));
	}
	let mut slots = initialize_slots(logical, variables);
	let mut bindings = variables.clone();
	for (statement, timeout) in logical.statements.iter().zip(timeouts) {
		let control = parent_control.child(*timeout);
		control.check()?;
		let mut runtime = ControlRuntime::default();
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
				let mut flow = None;
				let outcome = session.apply_item(|transaction| {
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

		session.apply_item(|transaction| {
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
		transaction,
		snapshot,
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
		transaction,
		snapshot,
	};
	execute_block(&mut read, block, slots, bindings, source, control, runtime, 1)
}

#[derive(Default)]
pub(in crate::gql::execution) struct ControlRuntime {
	iterations: u64,
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
	if !matches!(statement, LogicalStatement::Select(_)) {
		control.check()?;
	}
	if depth > control.max_control_depth {
		return Err(FerrosGrynnError::control_limit_exceeded(
			"nesting depth",
			u64::from(control.max_control_depth),
		));
	}
	let value = match statement {
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
			value,
		} => {
			let value = eval_query_expression(read, value, slots, source, control, runtime)?;
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

fn eval_query_expression<Read: QueryRead>(
	read: &mut Read,
	expression: &TypedExpr,
	slots: &[Value],
	source: &str,
	control: &ExecutionControl,
	runtime: &mut ControlRuntime,
) -> FerrosGrynnResult<Value> {
	match &expression.kind {
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
			let callee = eval_query_expression(read, callee, slots, source, control, runtime)?;
			let Value::Closure(closure) = callee else {
				return Err(FerrosGrynnError::invalid_argument(
					"Only a closure value can be invoked.",
				));
			};
			let mut values = Vec::with_capacity(arguments.len());
			for argument in arguments {
				values
					.push(eval_query_expression(read, argument, slots, source, control, runtime)?);
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
		_ => eval(expression, None, slots, source, None, Some(control)),
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
		let control = control.child(statement.timeout);
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
	let effect_allowed = match (entry.plan.effect, mode) {
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
			format!("{:?}", entry.plan.effect),
			position.to_owned(),
		));
	}
	if entry.plan.parameters.len() != arguments.len() {
		return Err(FerrosGrynnError::conversion_error(
			format!("{} closure arguments", arguments.len()),
			format!("{} arguments", entry.plan.parameters.len()),
		));
	}
	let mut slots = vec![Value::None; entry.plan.slot_count];
	for (slot, value) in entry.captures {
		let target = slots.get_mut(slot as usize).ok_or_else(|| {
			FerrosGrynnError::invalid_argument("Closure capture slot is outside its typed frame.")
		})?;
		*target = value;
	}
	for ((slot, _), value) in entry.plan.parameters.iter().zip(arguments) {
		let target = slots.get_mut(*slot as usize).ok_or_else(|| {
			FerrosGrynnError::invalid_argument("Closure parameter slot is outside its typed frame.")
		})?;
		*target = value.clone();
	}
	let savepoint = read.savepoint();
	let result = match &entry.plan.body {
		LogicalClosureBody::Expression(expression) => {
			if matches!(mode, ClosureInvocationMode::Standalone) {
				eval_query_expression(read, expression, &slots, source, control, runtime)
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
	runtime.iterations = runtime.iterations.saturating_add(1);
	if runtime.iterations > control.max_loop_iterations {
		return Err(FerrosGrynnError::control_limit_exceeded(
			"loop iterations",
			control.max_loop_iterations,
		));
	}
	Ok(())
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
		LogicalStatement::AssertInvariants {
			..
		} => QueryStatementKind::Assert,
		LogicalStatement::Match(_) => QueryStatementKind::Match,
		LogicalStatement::Select(_) => QueryStatementKind::Select,
		LogicalStatement::Traverse(_) => QueryStatementKind::Traverse,
	}
}

fn initialize_slots(program: &LogicalProgram, variables: &Object) -> Vec<Value> {
	let mut slots = vec![Value::None; program.slot_count];
	for (name, slot) in &program.initial_slots {
		if let Some(value) = variables.get(name) {
			slots[*slot as usize] = value.clone();
		}
	}
	slots
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
	let demand = crate::gql::optimizer::PlannerDemand::for_match(plan);
	let planner_snapshot = read.planner_snapshot(snapshot, &demand)?;
	let deterministic_snapshot;
	let planner_snapshot = if control.deterministic() {
		deterministic_snapshot = planner_snapshot.deterministic_projection();
		&deterministic_snapshot
	} else {
		&planner_snapshot
	};
	let physical =
		crate::gql::optimizer::plan_match(plan, planner_snapshot, control.deterministic())?;
	control.record_match_graph_open();
	let graph = Rc::new(read.graph(snapshot, &plan.graph)?);
	let graph_ref = Arc::new(plan.graph.clone());
	let has_chain = plan.patterns.iter().any(|pattern| {
		matches!(
			&pattern.kind,
			LogicalMatchPatternKind::Linear(pattern) if pattern.ops.len() > 1
		)
	});
	let has_path = plan
		.patterns
		.iter()
		.filter_map(|pattern| match &pattern.kind {
			LogicalMatchPatternKind::Linear(pattern) => Some(pattern.ops.as_slice()),
			LogicalMatchPatternKind::Hyper(_) | LogicalMatchPatternKind::Within(_) => None,
		})
		.flatten()
		.any(|op| matches!(op, MatchOp::PathExpand(_)));
	let semantics = if plan.atom || plan.patterns.iter().any(pattern_uses_edge_endpoint) {
		TraversalSemantics::Atom
	} else {
		TraversalSemantics::Node
	};
	let index = (has_chain || has_path)
		.then(|| TraversalIndex::new(graph.as_ref(), semantics).map(Rc::new))
		.transpose()?;
	let fields = MatchGraphFields::new(
		read,
		snapshot,
		read.property_dictionary(snapshot),
		graph_ref.clone(),
	);
	let scoped_index = physical
		.uses_scoped_traversal()
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
				let start = resolve_match_binding(&pattern.start, slots, source, control)?;
				let candidates = read.match_candidates(snapshot, &plan.graph, &start)?;
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
							let expand = resolve_expand(expand, slots, source, control, plan.mode)?;
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
							let expand =
								resolve_path_expand(expand, slots, source, control, plan.mode)?;
							let shortest = matches!(
								operator,
								crate::gql::optimizer::MatchOperator::BreadthFirstShortest { .. }
									| crate::gql::optimizer::MatchOperator::DijkstraShortest { .. }
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
							if scoped {
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
				let edge = resolve_match_binding(&pattern.edge, slots, source, control)?;
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
									resolve_match_binding(binding, slots, source, control)
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
					child: resolve_match_binding(&pattern.child, slots, source, control)?,
					owner: resolve_match_binding(&pattern.owner, slots, source, control)?,
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

	let mut result = Array::new(Vec::new());
	while let Some(row) = cursor.next(control).map_err(execution_phase)? {
		let object = plan
			.result
			.columns
			.iter()
			.zip(row.values)
			.map(|(column, value)| (column.name.as_ref(), value))
			.collect::<Object>();
		result.push(Value::Object(object));
	}
	Ok(Value::Array(result))
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
