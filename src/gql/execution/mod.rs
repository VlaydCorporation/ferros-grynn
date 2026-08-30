#![allow(
	clippy::pattern_type_mismatch,
	reason = "the executor intentionally uses match ergonomics for borrowed IR and runtime values"
)]

mod adapter;
mod cursor;
mod eval;

use std::{collections::BTreeSet, rc::Rc, sync::Arc, time::Instant};

#[cfg(test)]
use adapter::TransactionRead;
pub use adapter::execute_transaction;
use rust_decimal::Decimal;
#[allow(
	clippy::wildcard_imports,
	reason = "move-only executor split keeps the existing cursor and evaluator names during stage \
	          1"
)]
use {cursor::*, eval::*};

use super::{
	QueryResult,
	QueryStatementKind,
	analysis::{QueryCatalog, analyze},
	ir::{
		Column,
		EndpointDomain,
		Expand,
		LogicalMatch,
		LogicalMatchPattern,
		LogicalNodeKind,
		LogicalProgram,
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
};
use crate::{
	datastore::{
		PredicateEvaluator,
		QuerySnapshot,
		QueryStorageRow,
		ScalarComparison,
		ScalarIndexConstraint,
		TableSchema,
		Transaction,
		TransactionGraphView,
	},
	gql_status::{ExecutionPhase, FerrosGrynnError, FerrosGrynnResult, Location, Outcome},
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
	read: &Read,
	source: &str,
	variables: &Object,
	settings: ParserSettings,
) -> FerrosGrynnResult<Vec<QueryResult>> {
	let program = crate::gql::parse(source, settings)?;
	execute_program(read, source, &program, variables)
}

/// Executes an already parsed program against one captured snapshot.
fn execute_program<Read: QueryRead>(
	read: &Read,
	source: &str,
	program: &Program,
	variables: &Object,
) -> FerrosGrynnResult<Vec<QueryResult>> {
	let snapshot = read.snapshot()?;
	let logical = analyze(program, source, read.catalog(snapshot), variables)?;
	let mut slots = initialize_slots(&logical, variables);
	let mut results = Vec::with_capacity(logical.statements.len());
	for statement in &logical.statements {
		let start = Instant::now();
		let (kind, result) = match statement {
			LogicalStatement::Let {
				slot,
				value,
			} => {
				let result = eval(value, None, &slots, source, None).map(|value| {
					slots[*slot as usize] = value;
					Outcome::success(Value::None)
				});
				(QueryStatementKind::Let, result)
			}
			LogicalStatement::Return {
				value,
			} => (
				QueryStatementKind::Return,
				eval(value, None, &slots, source, None).map(Outcome::success),
			),
			LogicalStatement::Match(plan) => (
				QueryStatementKind::Match,
				execute_match(read, snapshot, plan, &slots, source).map(Outcome::success),
			),
			LogicalStatement::Select(select) => (
				QueryStatementKind::Select,
				execute_select(read, snapshot, select, &slots, source).map(Outcome::success),
			),
		};
		let failed = result.is_err();
		results.push(QueryResult {
			kind,
			elapsed: start.elapsed(),
			result,
		});
		if failed {
			break;
		}
	}
	Ok(results)
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
	};
	let mut cursor: Box<dyn RowCursor + '_> = Box::new(OneRowCursor {
		row: Some(Row::bindings(plan.bindings.columns.len())),
	});
	for pattern in ordered {
		cursor = Box::new(SeedCursor::new(cursor, graph.clone(), pattern.start.clone())?);
		for op in &pattern.ops {
			cursor = match op {
				MatchOp::Expand(expand) => {
					if let Some(index) = &index {
						Box::new(IndexExpandCursor::new(
							cursor,
							graph.clone(),
							index.clone(),
							expand.clone(),
						))
					} else {
						Box::new(EdgeExpandCursor::new(cursor, graph.clone(), expand.clone())?)
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
					expand.clone(),
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
	while let Some(row) = cursor.next().map_err(execution_phase)? {
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
) -> FerrosGrynnResult<Value> {
	let predicate = plan.nodes.iter().find_map(|node| match &node.kind {
		LogicalNodeKind::Filter {
			predicate,
			..
		} => Some(predicate),
		_ => None,
	});
	let mut cursor = build_cursor(read, snapshot, plan, plan.root, slots, source, predicate)?;
	let schema = &plan.node(plan.root).schema;
	let mut result = Array::new(Vec::new());
	while let Some(row) = cursor.next().map_err(execution_phase)? {
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
) -> FerrosGrynnResult<Box<dyn RowCursor + 'a>> {
	let node = plan.node(node_id);
	match &node.kind {
		LogicalNodeKind::Scan {
			target,
		} => {
			let inner: Box<dyn RowCursor + 'a> = match target {
				ScanTarget::Table(table) => {
					read.scan(snapshot, table, scan_predicate, slots, source)?
				}
				ScanTarget::Record(record) => Box::new(OneRowCursor {
					row: read.get(snapshot, record)?,
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
			input: build_cursor(read, snapshot, plan, *input, slots, source, scan_predicate)?,
			predicate,
			slots,
			source,
			fields: None,
		})),
		LogicalNodeKind::Project {
			input,
			expressions,
		} => Ok(Box::new(ProjectCursor {
			input: build_cursor(read, snapshot, plan, *input, slots, source, scan_predicate)?,
			expressions,
			slots,
			source,
			fields: None,
		})),
		LogicalNodeKind::Limit {
			input,
			limit,
			start,
		} => {
			let limit = limit.as_ref().map(|value| eval_usize(value, slots, source)).transpose()?;
			let start = start
				.as_ref()
				.map(|value| eval_usize(value, slots, source))
				.transpose()?
				.unwrap_or(0);
			Ok(Box::new(LimitCursor {
				input: build_cursor(read, snapshot, plan, *input, slots, source, scan_predicate)?,
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
	fn next(&mut self) -> FerrosGrynnResult<Option<Row>> {
		Ok(self.rows.next())
	}
}

#[cfg(test)]
mod tests;
