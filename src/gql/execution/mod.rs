#![allow(
	clippy::pattern_type_mismatch,
	reason = "the executor intentionally uses match ergonomics for borrowed IR and runtime values"
)]

mod adapter;
mod builtins;
mod cursor;
mod dml;
mod eval;
mod schema;

use std::{collections::BTreeSet, rc::Rc, sync::Arc, time::Instant};

use adapter::TransactionRead;
#[cfg(test)]
pub(crate) use adapter::execute_parsed_transaction_with_bindings;
pub(crate) use adapter::{analyze_parsed_transaction, execute_parsed_statement_with_bindings};
pub use adapter::{execute_transaction, execute_transaction_with_options};
use dml::{execute_dml, execute_graph_dml};
use rust_decimal::Decimal;
use schema::execute_schema;
#[allow(
	clippy::wildcard_imports,
	reason = "move-only executor split keeps the existing cursor and evaluator names during stage \
	          1"
)]
use {cursor::*, eval::*};

use super::{
	QueryResult,
	QueryStatementKind,
	analysis::{QueryCatalog, analyze, analyze_statement, compatible},
	ir::{
		Column,
		EndpointDomain,
		Expand,
		LogicalDmlData,
		LogicalDmlKind,
		LogicalDmlReturn,
		LogicalDmlReturnItem,
		LogicalDmlSelector,
		LogicalDmlStatement,
		LogicalDmlTarget,
		LogicalExpect,
		LogicalGraphDmlStatement,
		LogicalMatch,
		LogicalMatchPattern,
		LogicalMatchSelector,
		LogicalNodeKind,
		LogicalProgram,
		LogicalSchemaStatement,
		LogicalSelect,
		LogicalStatement,
		MatchBinding,
		MatchOp,
		NodeId,
		PathExpand,
		ScanTarget,
		Schema,
		TypedExpr,
		TypedExprKind,
	},
	options::ExecutionControl,
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
		Transaction,
		TransactionGraphView,
	},
	gql_status::{ExecutionPhase, FerrosGrynnError, FerrosGrynnResult, Location},
	graph::{
		AtomId,
		EdgeDirection,
		EdgeEndpoint,
		EdgeRead,
		EdgeTopology,
		EndpointRole,
		GraphRead,
		NodeRead,
		PathDirection,
		PathStep,
		TraversalDirection,
		TraversalIndex,
		TraversalSemantics,
	},
	math::{add_exact, divide_signed_exact, multiply_exact, subtract_exact},
	syn::{BinaryOperator, ParserSettings, Program, UnaryOperator},
	values::{
		Array,
		AtomKind,
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
	let start = Instant::now();
	control.check()?;
	let kind = logical_statement_kind(statement);
	let mut runtime = ControlRuntime::default();
	let result =
		execute_statement(read, statement, slots, bindings, source, control, &mut runtime, 0, true)
			.and_then(|flow| match flow {
				ControlFlow::Value(value) => control.outcome(value),
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
	Ok(QueryResult {
		kind,
		elapsed: start.elapsed(),
		result,
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
					Some(ControlFlow::Value(_) | ControlFlow::Continue) => {}
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
		ControlFlow::Value(_) => Ok(()),
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
struct ControlRuntime {
	iterations: u64,
}

enum ControlFlow {
	Value(Value),
	Break,
	Continue,
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
	control.check()?;
	if depth > control.max_control_depth {
		return Err(FerrosGrynnError::control_limit_exceeded(
			"nesting depth",
			u64::from(control.max_control_depth),
		));
	}
	let value = match statement {
		LogicalStatement::Schema(statement) => read.execute_schema(statement)?,
		LogicalStatement::Dml(statement) => read.execute_dml(statement, slots, source, control)?,
		LogicalStatement::GraphDml(statement) => {
			read.execute_graph_dml(statement, slots, source, control)?
		}
		LogicalStatement::Let {
			name,
			slot,
			value,
		} => {
			let value = eval(value, None, slots, source, None, Some(control))?;
			slots[*slot as usize] = value.clone();
			if persist_let && name.as_ref() != "_" {
				bindings.insert(name.as_ref(), value);
			}
			Value::None
		}
		LogicalStatement::Return {
			value,
		}
		| LogicalStatement::Expression {
			value,
		} => eval(value, None, slots, source, None, Some(control))?,
		LogicalStatement::If {
			branches,
			else_block,
		} => {
			let mut selected = None;
			for (condition, block) in branches {
				if eval(condition, None, slots, source, None, Some(control))?.is_truthy() {
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
			let values =
				iterable_values(eval(iterable, None, slots, source, None, Some(control))?)?;
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
				}
			}
			last
		}
		LogicalStatement::While {
			condition,
			body,
		} => {
			let mut last = Value::None;
			while eval(condition, None, slots, source, None, Some(control))?.is_truthy() {
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
			let value = eval(value, None, slots, source, None, Some(control))?;
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
	};
	control.check()?;
	Ok(ControlFlow::Value(value))
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
			flow @ (ControlFlow::Break | ControlFlow::Continue) => return Ok(flow),
		}
	}
	Ok(ControlFlow::Value(last))
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
	let graph = Rc::new(read.graph(snapshot, &plan.graph)?);
	let ordered = optimize_match_patterns(graph.as_ref(), &plan.patterns)?;
	let has_chain = plan.patterns.iter().any(|pattern| pattern.ops.len() > 1);
	let has_path = plan
		.patterns
		.iter()
		.flat_map(|pattern| &pattern.ops)
		.any(|op| matches!(op, MatchOp::PathExpand(_)));
	let semantics = if plan.patterns.iter().any(pattern_uses_edge_endpoint) {
		TraversalSemantics::Atom
	} else {
		TraversalSemantics::Node
	};
	let index = (has_chain || has_path)
		.then(|| TraversalIndex::new(graph.as_ref(), semantics).map(Rc::new))
		.transpose()?;
	let fields = GraphFields {
		graph: graph.clone(),
		dictionary: read.property_dictionary(snapshot),
		records: RecordFields {
			read,
			snapshot,
			graph: Some(&plan.graph),
		},
	};
	let mut cursor: Box<dyn RowCursor + '_> = Box::new(OneRowCursor {
		row: Some(Row::bindings(plan.bindings.columns.len())),
	});
	for pattern in ordered {
		let start = resolve_match_binding(&pattern.start, slots, source, control)?;
		let candidates = read.match_candidates(snapshot, &plan.graph, &start)?;
		cursor = Box::new(SeedCursor::new(cursor, graph.clone(), start, candidates));
		for op in &pattern.ops {
			cursor = match op {
				MatchOp::Expand(expand) => {
					let expand = resolve_expand(expand, slots, source, control)?;
					if let Some(index) = &index {
						Box::new(IndexExpandCursor::new(
							cursor,
							graph.clone(),
							index.clone(),
							expand,
						))
					} else {
						Box::new(EdgeExpandCursor::new(cursor, graph.clone(), expand)?)
					}
				}
				MatchOp::PathExpand(expand) => Box::new(PathExpandCursor::new(
					cursor,
					graph.clone(),
					index.clone().ok_or_else(|| {
						FerrosGrynnError::invalid_argument(
							"MATCH path expansion requires a traversal index.",
						)
					})?,
					resolve_path_expand(expand, slots, source, control)?,
				)),
			};
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
	cursor = Box::new(ProjectCursor {
		input: cursor,
		expressions: &plan.projection,
		slots,
		source,
		fields: Some(&fields),
	});

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

/// Orders patterns by connectivity, estimated cardinality, and source order.
fn optimize_match_patterns<'a, Graph: GraphRead>(
	graph: &Graph,
	patterns: &'a [LogicalMatchPattern],
) -> FerrosGrynnResult<Vec<&'a LogicalMatchPattern>> {
	let nodes = graph.node_count()?;
	let edges = graph.edge_count()?;
	let average_degree =
		edges.saturating_mul(2).saturating_add(nodes.max(1).saturating_sub(1)) / nodes.max(1);
	let mut remaining = patterns.iter().collect::<Vec<_>>();
	let mut known = BTreeSet::new();
	let mut ordered = Vec::with_capacity(patterns.len());
	while !remaining.is_empty() {
		let mut best = 0;
		for index in 1..remaining.len() {
			let candidate = remaining[index];
			let current = remaining[best];
			let candidate_connected =
				pattern_columns(candidate).any(|column| known.contains(&column));
			let current_connected = pattern_columns(current).any(|column| known.contains(&column));
			let candidate_key = (
				!candidate_connected,
				pattern_estimate(candidate, nodes, edges, average_degree),
				candidate.original_index,
			);
			let current_key = (
				!current_connected,
				pattern_estimate(current, nodes, edges, average_degree),
				current.original_index,
			);
			if candidate_key < current_key {
				best = index;
			}
		}
		let pattern = remaining.remove(best);
		known.extend(pattern_columns(pattern));
		ordered.push(pattern);
	}
	Ok(ordered)
}

/// Estimates one pattern using exact graph counts and average degree.
fn pattern_estimate(
	pattern: &LogicalMatchPattern,
	nodes: usize,
	edges: usize,
	average_degree: usize,
) -> usize {
	let hops = pattern.ops.iter().fold(0_usize, |total, op| {
		total.saturating_add(match op {
			MatchOp::Expand(_) => 1,
			MatchOp::PathExpand(expand) => expand.max as usize,
		})
	});
	if hops == 0 {
		return match pattern.start.domain {
			EndpointDomain::Node => nodes,
			EndpointDomain::Edge => edges,
		};
	}
	(1..hops).fold(edges, |estimate, _hop| estimate.saturating_mul(average_degree))
}

/// Iterates all binding columns mentioned by a pattern.
fn pattern_columns(pattern: &LogicalMatchPattern) -> impl Iterator<Item = usize> + '_ {
	pattern.start.column.into_iter().chain(pattern.ops.iter().flat_map(|op| {
		match op {
			MatchOp::Expand(expand) => {
				[expand.source.column, expand.edge.column, expand.target.column]
					.into_iter()
					.flatten()
					.collect::<Vec<_>>()
			}
			MatchOp::PathExpand(expand) => {
				[expand.source.column, expand.target.column, expand.path_column]
					.into_iter()
					.flatten()
					.collect::<Vec<_>>()
			}
		}
	}))
}

/// Reports whether a pattern requires AtomWalk traversal semantics.
fn pattern_uses_edge_endpoint(pattern: &LogicalMatchPattern) -> bool {
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

/// Executes the existing SELECT pull pipeline.
fn execute_select<Read: QueryRead>(
	read: &Read,
	snapshot: Snapshot,
	plan: &LogicalSelect,
	slots: &[Value],
	source: &str,
	control: &ExecutionControl,
) -> FerrosGrynnResult<Value> {
	let predicate = plan.nodes.iter().find_map(|node| match &node.kind {
		LogicalNodeKind::Filter {
			predicate,
			..
		} => Some(predicate),
		_ => None,
	});
	let fields = RecordFields {
		read,
		snapshot,
		graph: plan.graph.as_ref(),
	};
	let mut cursor = build_cursor(
		read,
		snapshot,
		plan,
		plan.root,
		slots,
		source,
		predicate,
		Some(&fields),
		control,
	)?;
	let schema = &plan.node(plan.root).schema;
	let mut result = Array::new(Vec::new());
	while let Some(row) = cursor.next(control).map_err(execution_phase)? {
		let value = if plan.value {
			row.values.into_iter().next().unwrap_or(Value::None)
		} else {
			let object = schema
				.columns
				.iter()
				.zip(row.values)
				.map(|(column, value)| (column.name.as_ref(), value))
				.collect::<Object>();
			Value::Object(object)
		};
		result.push(value);
		if plan.only && result.len() > 1 {
			return Err(FerrosGrynnError::invalid_argument(
				"SELECT ONLY returned more than one row.",
			)
			.with_phase(ExecutionPhase::Execution));
		}
	}
	Ok(Value::Array(result))
}

/// Builds the existing SELECT cursor chain for one logical node.
fn build_cursor<'a, Read: QueryRead>(
	read: &'a Read,
	snapshot: Snapshot,
	plan: &'a LogicalSelect,
	node_id: NodeId,
	slots: &'a [Value],
	source: &'a str,
	scan_predicate: Option<&'a TypedExpr>,
	fields: Option<&'a dyn AtomFieldRead>,
	control: &'a ExecutionControl,
) -> FerrosGrynnResult<Box<dyn RowCursor + 'a>> {
	let node = plan.node(node_id);
	match &node.kind {
		LogicalNodeKind::Scan {
			target,
		} => {
			let inner: Box<dyn RowCursor + 'a> = match target {
				ScanTarget::Class(class) => {
					read.scan(snapshot, class, plan.graph.as_ref(), scan_predicate, slots, source)?
				}
				ScanTarget::Record(record) => Box::new(OneRowCursor {
					row: read.get(snapshot, record, plan.graph.as_ref())?,
				}),
			};
			Ok(Box::new(ScanCursor {
				inner,
			}))
		}
		LogicalNodeKind::Filter {
			input,
			predicate,
		} => Ok(Box::new(FilterCursor {
			input: build_cursor(
				read,
				snapshot,
				plan,
				*input,
				slots,
				source,
				scan_predicate,
				fields,
				control,
			)?,
			predicate,
			slots,
			source,
			fields,
		})),
		LogicalNodeKind::Project {
			input,
			expressions,
		} => Ok(Box::new(ProjectCursor {
			input: build_cursor(
				read,
				snapshot,
				plan,
				*input,
				slots,
				source,
				scan_predicate,
				fields,
				control,
			)?,
			expressions,
			slots,
			source,
			fields,
		})),
		LogicalNodeKind::Limit {
			input,
			limit,
			start,
		} => {
			let limit = limit
				.as_ref()
				.map(|value| eval_usize(value, slots, source, control))
				.transpose()?;
			let start = start
				.as_ref()
				.map(|value| eval_usize(value, slots, source, control))
				.transpose()?
				.unwrap_or(0);
			Ok(Box::new(LimitCursor {
				input: build_cursor(
					read,
					snapshot,
					plan,
					*input,
					slots,
					source,
					scan_predicate,
					fields,
					control,
				)?,
				remaining: limit,
				skip: start,
			}))
		}
	}
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
