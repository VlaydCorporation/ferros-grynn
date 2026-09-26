#![allow(
	clippy::wildcard_imports,
	reason = "the SELECT executor composes the existing concrete query runtime"
)]

use super::*;
use crate::gql::{
	cursor::{CursorAnchor, CursorPage, CursorRecord, IndexSeekCheckout, IndexSeekPlan},
	spill::{QuerySorter, SortField, SortedRecords, compare_record_anchor},
};

mod expression;
pub(super) mod fingerprint;
mod join;
mod pagination;
mod rows;

use expression::*;
use join::*;
use pagination::*;
use rows::*;

fn prepare_physical_plan<Read: QueryRead>(
	read: &Read,
	snapshot: Snapshot,
	logical: &LogicalSelect,
	control: &ExecutionControl,
) -> FerrosGrynnResult<(LogicalSelect, crate::gql::optimizer::PhysicalSelectPlan)> {
	let demand = crate::gql::optimizer::PlannerDemand::for_select(logical);
	let planner_snapshot = read.planner_snapshot(snapshot, &demand)?;
	let deterministic_snapshot;
	let planner_snapshot = if control.deterministic() {
		deterministic_snapshot = planner_snapshot.deterministic_projection();
		&deterministic_snapshot
	} else {
		&planner_snapshot
	};
	crate::gql::optimizer::prepare_select(logical, planner_snapshot, control.barrier_memory_limit())
}

pub(super) fn execute_select<Read: QueryRead>(
	read: &Read,
	snapshot: Snapshot,
	plan: &LogicalSelect,
	slots: &[Value],
	source: &str,
	control: &ExecutionControl,
) -> FerrosGrynnResult<Value> {
	let (plan, physical) = prepare_physical_plan(read, snapshot, plan, control)?;
	let plan = &plan;
	let (fingerprint, order_fingerprint) = fingerprint::fingerprints(plan);
	let values = match &plan.start {
		Some(LogicalSelectStart::After {
			cursor,
			read_committed,
		}) => {
			if plan.order.is_empty() {
				return Err(FerrosGrynnError::invalid_argument("START AFTER requires ORDER BY."));
			}
			let limit = plan
				.limit
				.as_ref()
				.ok_or_else(|| FerrosGrynnError::invalid_argument("START AFTER requires LIMIT."))?;
			let cursor = match eval(cursor, None, slots, source, None, None)? {
				Value::Cursor(cursor) => cursor,
				value => {
					return Err(FerrosGrynnError::invalid_argument(format!(
						"START AFTER requires cursor, found {}.",
						value.kind()
					)));
				}
			};
			let runtime = control.cursors().ok_or_else(|| {
				FerrosGrynnError::invalid_query_cursor(
					"continuations require a persistent GqlSession",
				)
			})?;
			if let Err(error) = control.check() {
				control.discard_cursor(cursor);
				return Err(error);
			}
			let page_size = match eval_usize(limit, slots, source, control) {
				Ok(page_size) => page_size,
				Err(error) => {
					control.discard_cursor(cursor);
					return Err(error);
				}
			};
			if !*read_committed {
				let checkout =
					runtime.lock().checkout_index_seek(cursor, fingerprint, order_fingerprint)?;
				if let Some(checkout) = checkout {
					resume_index_seek(runtime, checkout, page_size, control)?
				} else {
					let page =
						runtime.lock().page(cursor, fingerprint, order_fingerprint, page_size)?;
					if let Err(error) = control.check() {
						control.discard_cursor(cursor);
						return Err(error);
					}
					let CursorPage {
						values,
						continuation,
					} = page;
					if let Some(cursor) = continuation {
						control.set_continuation(cursor);
					}
					values
				}
			} else {
				let anchor =
					runtime.lock().read_committed_anchor(cursor, fingerprint, order_fingerprint)?;
				runtime.lock().close(cursor)?;
				if page_size != 0
					&& !control.publication_deferred()
					&& let Some(records) = execute_index_seek_page(
						read,
						snapshot,
						plan,
						&physical,
						slots,
						source,
						control,
						Some(&anchor),
						page_size.saturating_add(1),
					)? {
					finish_new_index_seek_page(
						records,
						page_size,
						plan,
						&physical,
						slots,
						source,
						fingerprint,
						order_fingerprint,
						snapshot,
						control,
					)?
				} else {
					let output = execute_select_output(
						read, snapshot, plan, &physical, slots, source, control, None,
					)?;
					let output = output_after_anchor(output, &anchor, plan)?;
					page_output(
						output,
						0,
						Some(page_size),
						plan,
						snapshot,
						control,
						fingerprint,
						order_fingerprint,
					)?
				}
			}
		}
		_ => {
			control.check()?;
			let start = match &plan.start {
				Some(LogicalSelectStart::Offset(value)) => {
					eval_usize(value, slots, source, control)?
				}
				None => 0,
				Some(LogicalSelectStart::After {
					..
				}) => unreachable!(),
			};
			let limit = select_limit(plan, slots, source, control)?;
			if start == 0
				&& limit.is_some_and(|limit| limit != 0)
				&& (control.cursors().is_none() || !control.publication_deferred())
				&& let Some(records) = execute_index_seek_page(
					read,
					snapshot,
					plan,
					&physical,
					slots,
					source,
					control,
					None,
					limit.unwrap_or(0).saturating_add(1),
				)? {
				finish_new_index_seek_page(
					records,
					limit.unwrap_or(0),
					plan,
					&physical,
					slots,
					source,
					fingerprint,
					order_fingerprint,
					snapshot,
					control,
				)?
			} else if let Some(values) = execute_streaming_select_values(
				read,
				snapshot,
				plan,
				&physical,
				slots,
				source,
				control,
				None,
				start,
				limit.unwrap_or(usize::MAX),
			)? {
				values
			} else {
				let output = execute_select_output(
					read, snapshot, plan, &physical, slots, source, control, None,
				)?;
				page_output(
					output,
					start,
					limit,
					plan,
					snapshot,
					control,
					fingerprint,
					order_fingerprint,
				)?
			}
		}
	};
	if plan.only {
		return match values.as_slice() {
			[] => Ok(Value::None),
			[value] => Ok(value.clone()),
			_ => Err(FerrosGrynnError::select_cardinality_violation(values.len() as u64)
				.with_phase(ExecutionPhase::Execution)),
		};
	}
	Ok(Value::Array(values.into_iter().collect()))
}

#[allow(
	clippy::too_many_arguments,
	reason = "continuation registration retains the complete analyzed read state"
)]
fn finish_new_index_seek_page(
	mut records: Vec<CursorRecord>,
	page_size: usize,
	plan: &LogicalSelect,
	physical: &crate::gql::optimizer::PhysicalSelectPlan,
	slots: &[Value],
	source: &str,
	fingerprint: u64,
	order_fingerprint: u64,
	snapshot: Snapshot,
	control: &ExecutionControl,
) -> FerrosGrynnResult<Vec<Value>> {
	let has_more = records.len() > page_size;
	records.truncate(page_size);
	let values = records.iter().map(|record| record.value.clone()).collect::<Vec<_>>();
	if has_more && let Some(runtime) = control.cursors() {
		let anchor = records
			.last()
			.ok_or_else(|| {
				FerrosGrynnError::invalid_query_cursor("index continuation page is empty")
			})?
			.anchor
			.clone();
		let cursor = runtime.lock().register_index_seek(
			IndexSeekPlan {
				plan: plan.clone(),
				physical: physical.clone(),
				slots: slots.to_vec(),
				source: source.to_owned(),
			},
			fingerprint,
			order_fingerprint,
			snapshot.0,
			anchor,
		)?;
		control.set_pending_index_seek(cursor);
		control.set_continuation(cursor);
	}
	Ok(values)
}

fn resume_index_seek(
	runtime: &Arc<parking_lot::Mutex<crate::gql::cursor::CursorRuntime>>,
	mut checkout: IndexSeekCheckout,
	page_size: usize,
	control: &ExecutionControl,
) -> FerrosGrynnResult<Vec<Value>> {
	let page = (|| {
		let snapshot = checkout.transaction.query_snapshot()?;
		let snapshot_id = Snapshot(snapshot.commit_ts());
		let read = TransactionRead {
			transaction: &mut checkout.transaction,
			snapshot,
		};
		execute_index_seek_page(
			&read,
			snapshot_id,
			&checkout.plan,
			&checkout.physical,
			&checkout.slots,
			&checkout.source,
			control,
			Some(&checkout.anchor),
			page_size.saturating_add(1),
		)
	})();
	let Some(mut records) = (match page {
		Ok(records) => records,
		Err(error) => {
			runtime.lock().abort_index_seek(&checkout);
			return Err(error);
		}
	}) else {
		runtime.lock().abort_index_seek(&checkout);
		return Err(FerrosGrynnError::invalid_query_cursor(
			"captured query is no longer eligible for index continuation",
		));
	};
	let has_more = records.len() > page_size;
	records.truncate(page_size);
	let next_anchor =
		has_more.then(|| records.last().map(|record| record.anchor.clone())).flatten();
	let values = records.into_iter().map(|record| record.value).collect();
	let continuation = runtime.lock().finish_index_seek(checkout, next_anchor)?;
	if let Some(cursor) = continuation {
		control.set_continuation(cursor);
	}
	Ok(values)
}

#[allow(
	clippy::too_many_arguments,
	reason = "keyset execution retains the analyzed query and exact snapshot context"
)]
fn execute_index_seek_page<Read: QueryRead>(
	read: &Read,
	snapshot: Snapshot,
	plan: &LogicalSelect,
	physical: &crate::gql::optimizer::PhysicalSelectPlan,
	slots: &[Value],
	source: &str,
	control: &ExecutionControl,
	after: Option<&CursorAnchor>,
	maximum: usize,
) -> FerrosGrynnResult<Option<Vec<CursorRecord>>> {
	if maximum == 0
		|| plan.only
		|| !plan.joins.is_empty()
		|| plan.group.is_some()
		|| !plan.split.is_empty()
		|| plan.filter.is_some()
		|| plan.tempfiles
		|| plan.order.is_empty()
		|| physical.requires_sort
		|| matches!(plan.source.kind, LogicalSelectSourceKind::Derived(_))
	{
		return Ok(None);
	}
	let Some(index) = physical.sources.first().and_then(planned_scalar_index) else {
		return Ok(None);
	};
	let mut records = Vec::with_capacity(maximum.min(1_024));
	let mut ordinal = 0_usize;
	visit_streaming_source_rows(
		read,
		snapshot,
		&plan.source,
		slots,
		source,
		control,
		None,
		true,
		Some(index),
		Some(IndexScanWindow {
			after,
			maximum,
		}),
		None,
		|row| {
			let output = build_output_row(
				read, snapshot, plan, row, None, ordinal, None, slots, source, control,
			)?;
			ordinal = ordinal.checked_add(1).ok_or_else(|| {
				FerrosGrynnError::invalid_argument("index continuation ordinal overflow")
			})?;
			records.push(output_record(output));
			Ok(records.len() < maximum)
		},
	)?;
	Ok(Some(records))
}

fn execute_select_values<Read: QueryRead>(
	read: &Read,
	snapshot: Snapshot,
	plan: &LogicalSelect,
	slots: &[Value],
	source: &str,
	control: &ExecutionControl,
	outer: Option<&Row>,
	maximum: Option<usize>,
) -> FerrosGrynnResult<Vec<Value>> {
	let (plan, physical) = prepare_physical_plan(read, snapshot, plan, control)?;
	let plan = &plan;
	let start = match &plan.start {
		None => 0,
		Some(LogicalSelectStart::Offset(value)) => eval_usize(value, slots, source, control)?,
		Some(LogicalSelectStart::After {
			..
		}) => {
			return Err(FerrosGrynnError::query_feature_not_supported(
				"START AFTER inside a SELECT subquery",
			));
		}
	};
	let limit = select_limit(plan, slots, source, control)?
		.unwrap_or(usize::MAX)
		.min(maximum.unwrap_or(usize::MAX));
	let values = match execute_streaming_select_values(
		read, snapshot, plan, &physical, slots, source, control, outer, start, limit,
	)? {
		Some(values) => values,
		None => {
			let output = execute_select_output(
				read, snapshot, plan, &physical, slots, source, control, outer,
			)?;
			record_values(output, start, limit)?
		}
	};
	if plan.only && values.len() > 1 {
		return Err(FerrosGrynnError::select_cardinality_violation(values.len() as u64)
			.with_phase(ExecutionPhase::Execution));
	}
	Ok(values)
}

#[allow(
	clippy::too_many_arguments,
	reason = "the direct pull path retains the complete analyzed SELECT context"
)]
fn execute_streaming_select_values<Read: QueryRead>(
	read: &Read,
	snapshot: Snapshot,
	plan: &LogicalSelect,
	physical: &crate::gql::optimizer::PhysicalSelectPlan,
	slots: &[Value],
	source: &str,
	control: &ExecutionControl,
	outer: Option<&Row>,
	start: usize,
	limit: usize,
) -> FerrosGrynnResult<Option<Vec<Value>>> {
	if !plan.joins.is_empty()
		|| plan.group.is_some()
		|| !plan.split.is_empty()
		|| (!plan.order.is_empty() && (physical.requires_sort || control.cursors().is_some()))
		|| plan.tempfiles
		|| matches!(plan.source.kind, LogicalSelectSourceKind::Derived(_))
	{
		return Ok(None);
	}
	let limit = if plan.only {
		limit.min(2)
	} else {
		limit
	};
	if limit == 0 {
		return Ok(Some(Vec::new()));
	}
	if let Some(crate::gql::optimizer::SourceOperator::CountLookup {
		index,
		..
	}) = physical.sources.first().map(|source| &source.operator)
	{
		if start != 0 {
			return Ok(Some(Vec::new()));
		}
		let graph = match &plan.source.kind {
			LogicalSelectSourceKind::Atoms {
				graph,
				..
			} => graph
				.as_ref()
				.map(|reference| {
					resolve_graph_reference(
						read, snapshot, reference, outer, slots, source, control,
					)
				})
				.transpose()?,
			_ => None,
		};
		let count = Value::from(read.count_index(snapshot, *index, graph.as_ref())?);
		let value = if plan.value {
			count
		} else {
			let name = match plan.projection.as_slice() {
				[
					LogicalSelectItem::Expression {
						name,
						..
					},
				] => name,
				_ => {
					return Err(FerrosGrynnError::invalid_argument(
						"CountLookup requires one aggregate projection",
					));
				}
			};
			let mut value = Object::new();
			value.insert(name.as_ref(), count);
			Value::Object(value)
		};
		return Ok(Some(vec![value]));
	}
	let mut values = Vec::with_capacity(limit.min(1024));
	let mut projected = 0_usize;
	let full_row = outer.is_some() || select_requires_expanded_atom_row(plan);
	let index = physical.sources.first().and_then(planned_scalar_index);
	let window = index.map(|_index| IndexScanWindow {
		after: None,
		maximum: start.saturating_add(limit),
	});
	visit_streaming_source_rows(
		read,
		snapshot,
		&plan.source,
		slots,
		source,
		control,
		outer,
		full_row,
		index,
		window,
		physical.pushed_filter.then_some(plan.filter.as_ref()).flatten(),
		|row| {
			if let Some(predicate) = &plan.filter
				&& !eval_select_expr(read, snapshot, predicate, &row, None, slots, source, control)?
					.is_truthy()
			{
				return Ok(true);
			}
			let output =
				build_output_value(read, snapshot, plan, &row, None, slots, source, control)?;
			projected = projected.checked_add(1).ok_or_else(|| {
				FerrosGrynnError::invalid_argument("projected row ordinal overflow")
			})?;
			if projected <= start {
				return Ok(true);
			}
			values.push(output);
			Ok(values.len() < limit)
		},
	)?;
	Ok(Some(values))
}

#[allow(
	clippy::too_many_arguments,
	reason = "source streaming retains snapshot, expression, and outer-binding context"
)]
fn visit_streaming_source_rows<Read: QueryRead>(
	read: &Read,
	snapshot: Snapshot,
	source_plan: &LogicalSelectSource,
	slots: &[Value],
	source: &str,
	control: &ExecutionControl,
	outer: Option<&Row>,
	full_row: bool,
	index: Option<(crate::datastore::IndexId, bool)>,
	window: Option<IndexScanWindow<'_>>,
	predicate: Option<&TypedExpr>,
	mut visit: impl FnMut(Row) -> FerrosGrynnResult<bool>,
) -> FerrosGrynnResult<()> {
	let append_outer = |mut row: Row| {
		if let Some(outer) = outer {
			row.values.extend(outer.values.iter().cloned());
			row.stable.extend(outer.stable.iter().cloned());
		}
		row
	};
	match &source_plan.kind {
		LogicalSelectSourceKind::Atoms {
			target,
			graph,
		} => {
			let graph = graph
				.as_ref()
				.map(|reference| {
					resolve_graph_reference(
						read, snapshot, reference, outer, slots, source, control,
					)
				})
				.transpose()?;
			match target {
				ScanTarget::Class(class) => {
					let mut cursor = read.scan(
						snapshot,
						class,
						graph.as_ref(),
						predicate,
						index,
						window,
						slots,
						source,
						full_row,
					)?;
					let mut ordinal = 0_usize;
					while let Some(row) = cursor.next(control)? {
						let row = append_outer(row.with_graph(graph.clone()));
						ordinal = ordinal.checked_add(1).ok_or_else(|| {
							FerrosGrynnError::invalid_argument("source row ordinal overflow")
						})?;
						if !visit(row)? {
							break;
						}
					}
				}
				ScanTarget::Record(record) => {
					if let Some(row) = read.get(snapshot, record, graph.as_ref())?
						&& !visit(append_outer(row.with_graph(graph).with_sequence(vec![0])))?
					{
						return Ok(());
					}
				}
			}
		}
		LogicalSelectSourceKind::Graphs {
			references,
		} => {
			let references = references
				.as_ref()
				.map(|references| {
					references
						.iter()
						.map(|reference| {
							resolve_graph_reference(
								read, snapshot, reference, outer, slots, source, control,
							)
						})
						.collect::<FerrosGrynnResult<Vec<_>>>()
				})
				.transpose()?;
			for (ordinal, row) in
				read.graph_rows(snapshot, references.as_deref())?.into_iter().enumerate()
			{
				if !visit(append_outer(row.with_sequence(vec![ordinal])))? {
					break;
				}
			}
		}
		LogicalSelectSourceKind::Derived(_) => {
			return Err(FerrosGrynnError::invalid_argument(
				"derived source reached the direct pull path",
			));
		}
	}
	Ok(())
}

fn execute_select_output<Read: QueryRead>(
	read: &Read,
	snapshot: Snapshot,
	plan: &LogicalSelect,
	physical: &crate::gql::optimizer::PhysicalSelectPlan,
	slots: &[Value],
	source: &str,
	control: &ExecutionControl,
	outer: Option<&Row>,
) -> FerrosGrynnResult<SortedRecords> {
	control.check()?;
	let local_width = plan.input_schema.columns.len().saturating_sub(plan.outer_columns);
	let mut rows = source_rows(
		read,
		snapshot,
		logical_source(plan, physical.execution_order.first().copied().unwrap_or(0)),
		physical.execution_order.first().and_then(|source| physical.sources.get(*source)),
		physical.pushed_filter.then_some(plan.filter.as_ref()).flatten(),
		slots,
		source,
		control,
		outer,
		Some(local_width),
	)?;
	for physical_join in &physical.joins {
		let source_plan = logical_source(plan, physical_join.right_source);
		let predicates = physical_join
			.predicates
			.iter()
			.filter_map(|index| plan.joins.get(*index).and_then(|join| join.predicate.as_ref()))
			.collect::<Vec<_>>();
		let right_layout = physical
			.sources
			.get(physical_join.right_source)
			.map(|source| (source.column_offset, source.column_width))
			.unwrap_or((0, source_plan.schema.columns.len()));
		if physical_join.operator == crate::gql::optimizer::JoinOperator::IndexNestedLoop {
			let probe = physical_join.index_probe.as_ref().ok_or_else(|| {
				FerrosGrynnError::invalid_argument("IndexNestedLoop has no exact probe")
			})?;
			rows = index_nested_join_rows(
				read,
				snapshot,
				rows,
				source_plan,
				probe,
				physical_join.kind,
				&predicates,
				right_layout,
				slots,
				source,
				control,
			)?;
			continue;
		}
		let right = source_rows(
			read,
			snapshot,
			source_plan,
			physical.sources.get(physical_join.right_source),
			None,
			slots,
			source,
			control,
			None,
			Some(local_width),
		)?;
		rows = join_rows(
			read,
			snapshot,
			rows,
			right,
			physical_join.kind,
			&predicates,
			&physical_join.key_columns,
			physical_join.operator,
			physical_join.build_side,
			right_layout,
			slots,
			source,
			control,
		)?;
	}
	if let Some(outer) = outer {
		let mut transformed = RowCollector::new(read, control, false);
		rows.consume(|mut row| {
			row.values.extend(outer.values.iter().cloned());
			row.stable.extend(outer.stable.iter().cloned());
			transformed.push(read, snapshot, row)
		})?;
		rows = transformed.finish()?;
	}
	if let Some(predicate) = &plan.filter {
		let mut filtered = RowCollector::new(read, control, false);
		rows.consume(|row| {
			if eval_select_expr(read, snapshot, predicate, &row, None, slots, source, control)?
				.is_truthy()
			{
				filtered.push(read, snapshot, row)?;
			}
			Ok(())
		})?;
		rows = filtered.finish()?;
	}
	for split in &plan.split {
		rows = split_rows(read, snapshot, rows, split, slots, source, control)?;
	}
	let random_seed = plan
		.order
		.iter()
		.any(|order| order.random)
		.then(|| control.statement_seed(fingerprint::fingerprints(plan).0, snapshot.0));

	let mut output = OutputCollector::new(read, snapshot, plan, physical.requires_sort, control)?;
	if plan.group.is_some() {
		for_each_group(read, snapshot, plan, rows, slots, source, control, |ordinal, group| {
			let row = group.first()?.unwrap_or_else(|| Row::new(Vec::new()));
			let output_row = build_output_row(
				read,
				snapshot,
				plan,
				row,
				Some(&group),
				ordinal,
				random_seed,
				slots,
				source,
				control,
			)?;
			output.push(output_row)
		})?;
	} else {
		let mut ordinal = 0;
		rows.consume(|row| {
			let output_row = build_output_row(
				read,
				snapshot,
				plan,
				row,
				None,
				ordinal,
				random_seed,
				slots,
				source,
				control,
			)?;
			ordinal += 1;
			output.push(output_row)
		})?;
	}
	output.finish()
}

fn select_requires_expanded_atom_row(plan: &LogicalSelect) -> bool {
	let metadata = ["class", "kind", "label", "properties"]
		.into_iter()
		.filter_map(|name| plan.source.schema.index_of(name))
		.collect::<Vec<_>>();
	plan.projection.iter().any(|item| match item {
		LogicalSelectItem::Wildcard(fields) => {
			fields.iter().any(|field| matches!(field, LogicalWildcardField::Properties { .. }))
		}
		LogicalSelectItem::Expression {
			expression,
			..
		} => metadata.iter().any(|column| expression_uses_column(expression, *column)),
	}) || plan
		.filter
		.as_ref()
		.is_some_and(|filter| metadata.iter().any(|column| expression_uses_column(filter, *column)))
}

fn expression_uses_column(expression: &TypedExpr, column: usize) -> bool {
	match &expression.kind {
		TypedExprKind::Column(candidate) => *candidate == column,
		TypedExprKind::Array(values) | TypedExprKind::Set(values) => {
			values.iter().any(|value| expression_uses_column(value, column))
		}
		TypedExprKind::Object(fields) => {
			fields.iter().any(|(_name, value)| expression_uses_column(value, column))
		}
		TypedExprKind::Range {
			start,
			end,
			..
		} => {
			start.as_deref().is_some_and(|value| expression_uses_column(value, column))
				|| end.as_deref().is_some_and(|value| expression_uses_column(value, column))
		}
		TypedExprKind::Unary {
			expr,
			..
		}
		| TypedExprKind::Field {
			base: expr,
			..
		}
		| TypedExprKind::Destructure {
			base: expr,
			..
		}
		| TypedExprKind::Cast {
			expr,
			..
		} => expression_uses_column(expr, column),
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
		} => expression_uses_column(left, column) || expression_uses_column(right, column),
		TypedExprKind::Conditional {
			condition,
			then_value,
			else_value,
		} => {
			expression_uses_column(condition, column)
				|| expression_uses_column(then_value, column)
				|| expression_uses_column(else_value, column)
		}
		TypedExprKind::Case {
			operand,
			branches,
			else_value,
		} => {
			operand.as_deref().is_some_and(|value| expression_uses_column(value, column))
				|| branches.iter().any(|(condition, value)| {
					expression_uses_column(condition, column)
						|| expression_uses_column(value, column)
				})
				|| else_value.as_deref().is_some_and(|value| expression_uses_column(value, column))
		}
		TypedExprKind::Builtin {
			arguments,
			..
		} => arguments.iter().any(|value| expression_uses_column(value, column)),
		TypedExprKind::Aggregate {
			argument,
			..
		} => argument.as_deref().is_some_and(|value| expression_uses_column(value, column)),
		TypedExprKind::Subquery {
			..
		} => true,
		TypedExprKind::Constant(_) | TypedExprKind::Slot(_) | TypedExprKind::CurrentField(_) => {
			false
		}
	}
}

#[allow(clippy::too_many_arguments, reason = "projection uses the complete statement context")]
fn build_output_row<Read: QueryRead>(
	read: &Read,
	snapshot: Snapshot,
	plan: &LogicalSelect,
	row: Row,
	group: Option<&RowSet>,
	ordinal: usize,
	random_seed: Option<u64>,
	slots: &[Value],
	source: &str,
	control: &ExecutionControl,
) -> FerrosGrynnResult<OutputRow> {
	let (value, projected) =
		build_projected_value(read, snapshot, plan, &row, group, slots, source, control)?;
	let mut order_row = row.clone();
	order_row.values.extend(projected);
	let keys = plan
		.order
		.iter()
		.map(|order| {
			if order.random {
				let seed = random_seed.ok_or_else(|| {
					FerrosGrynnError::invalid_argument("RAND ordering has no statement seed")
				})?;
				let stable = if row.stable.is_empty() {
					row.sequence.iter().copied().map(Value::from).collect::<Vec<_>>()
				} else {
					row.stable.clone()
				};
				return Ok(Value::from(crate::gql::optimizer::canonical_hash(
					crate::gql::optimizer::PLAN_ENCODING_VERSION,
					b"order-rand-v3",
					&(seed, stable),
				)));
			}
			eval_select_expr(
				read,
				snapshot,
				&order.expression,
				&order_row,
				group,
				slots,
				source,
				control,
			)
		})
		.collect::<FerrosGrynnResult<Vec<_>>>()?;
	let mut stable = if let Some(group_keys) = &plan.group {
		group_keys
			.iter()
			.map(|key| eval_select_expr(read, snapshot, key, &row, group, slots, source, control))
			.collect::<FerrosGrynnResult<Vec<_>>>()?
	} else {
		row.stable.clone()
	};
	stable.push(Value::from(ordinal));
	let sequence = row.sequence.iter().copied().map(Value::from).collect();
	Ok(OutputRow {
		value,
		keys,
		stable,
		sequence,
		ordinal,
	})
}

#[allow(clippy::too_many_arguments, reason = "projection uses the complete statement context")]
fn build_projected_value<Read: QueryRead>(
	read: &Read,
	snapshot: Snapshot,
	plan: &LogicalSelect,
	row: &Row,
	group: Option<&RowSet>,
	slots: &[Value],
	source: &str,
	control: &ExecutionControl,
) -> FerrosGrynnResult<(Value, Vec<Value>)> {
	let (mut value, projected) = project(read, snapshot, plan, row, group, slots, source, control)?;
	for path in &plan.omit {
		omit_path(&mut value, path);
	}
	for path in &plan.fetch {
		if path.len() > control.max_control_depth as usize {
			return Err(FerrosGrynnError::control_limit_exceeded(
				"FETCH depth",
				u64::from(control.max_control_depth),
			));
		}
		fetch_path(read, snapshot, &mut value, path, row.graph.as_ref(), control)?;
	}
	Ok((value, projected))
}

#[allow(clippy::too_many_arguments, reason = "direct projection uses the statement context")]
fn build_output_value<Read: QueryRead>(
	read: &Read,
	snapshot: Snapshot,
	plan: &LogicalSelect,
	row: &Row,
	group: Option<&RowSet>,
	slots: &[Value],
	source: &str,
	control: &ExecutionControl,
) -> FerrosGrynnResult<Value> {
	if plan.value
		&& plan.omit.is_empty()
		&& plan.fetch.is_empty()
		&& let [
			LogicalSelectItem::Expression {
				expression,
				..
			},
		] = plan.projection.as_slice()
	{
		return eval_select_expr(read, snapshot, expression, row, group, slots, source, control);
	}
	build_projected_value(read, snapshot, plan, row, group, slots, source, control)
		.map(|(value, _projected)| value)
}

fn source_rows<Read: QueryRead>(
	read: &Read,
	snapshot: Snapshot,
	source_plan: &LogicalSelectSource,
	physical: Option<&crate::gql::optimizer::PhysicalSource>,
	predicate: Option<&TypedExpr>,
	slots: &[Value],
	source: &str,
	control: &ExecutionControl,
	outer: Option<&Row>,
	total_width: Option<usize>,
) -> FerrosGrynnResult<RowSet> {
	let layout = physical
		.zip(total_width)
		.map(|(source, total)| (source.column_offset, source.column_width, total));
	match &source_plan.kind {
		LogicalSelectSourceKind::Atoms {
			target,
			graph,
		} => {
			let graph = graph
				.as_ref()
				.map(|reference| {
					resolve_graph_reference(
						read, snapshot, reference, outer, slots, source, control,
					)
				})
				.transpose()?;
			match target {
				ScanTarget::Class(class) => {
					let mut cursor = read.scan(
						snapshot,
						class,
						graph.as_ref(),
						predicate,
						physical.and_then(planned_scalar_index),
						None,
						slots,
						source,
						true,
					)?;
					let mut rows = RowCollector::new(read, control, false);
					let mut ordinal = 0_usize;
					while let Some(row) = cursor.next(control)? {
						rows.push(
							read,
							snapshot,
							layout_row(
								row.with_graph(graph.clone()).with_sequence(vec![ordinal]),
								layout,
							),
						)?;
						ordinal = ordinal.checked_add(1).ok_or_else(|| {
							FerrosGrynnError::invalid_argument("source row ordinal overflow")
						})?;
					}
					rows.finish()
				}
				ScanTarget::Record(record) => rows_to_set(
					read,
					snapshot,
					control,
					read.get(snapshot, record, graph.as_ref())?
						.map(|row| layout_row(row.with_graph(graph), layout))
						.into_iter()
						.collect(),
				),
			}
		}
		LogicalSelectSourceKind::Graphs {
			references,
		} => {
			let references = references
				.as_ref()
				.map(|references| {
					references
						.iter()
						.map(|reference| {
							resolve_graph_reference(
								read, snapshot, reference, outer, slots, source, control,
							)
						})
						.collect::<FerrosGrynnResult<Vec<_>>>()
				})
				.transpose()?;
			rows_to_set(
				read,
				snapshot,
				control,
				read.graph_rows(snapshot, references.as_deref())?
					.into_iter()
					.map(|row| layout_row(row, layout))
					.collect(),
			)
		}
		LogicalSelectSourceKind::Derived(select) => {
			let rows = derived_source_rows(read, snapshot, select, slots, source, control)?;
			if let Some(layout) = layout {
				map_row_set(read, snapshot, control, rows, |row| layout_row(row, Some(layout)))
			} else {
				Ok(rows)
			}
		}
	}
}

fn logical_source(plan: &LogicalSelect, index: usize) -> &LogicalSelectSource {
	if index == 0 {
		&plan.source
	} else {
		&plan.joins[index - 1].source
	}
}

fn layout_row(mut row: Row, layout: Option<(usize, usize, usize)>) -> Row {
	let Some((offset, width, total)) = layout else {
		return row;
	};
	debug_assert_eq!(row.values.len(), width);
	let mut values = vec![Value::None; total];
	for (target, value) in
		values[offset..offset.saturating_add(width)].iter_mut().zip(row.values.drain(..))
	{
		*target = value;
	}
	row.values = values;
	row
}

fn map_row_set<Read: QueryRead>(
	read: &Read,
	snapshot: Snapshot,
	control: &ExecutionControl,
	rows: RowSet,
	mut map: impl FnMut(Row) -> Row,
) -> FerrosGrynnResult<RowSet> {
	let mut output = RowCollector::new(read, control, false);
	rows.consume(|row| output.push(read, snapshot, map(row)))?;
	output.finish()
}

fn planned_scalar_index(
	source: &crate::gql::optimizer::PhysicalSource,
) -> Option<(crate::datastore::IndexId, bool)> {
	match source.operator {
		crate::gql::optimizer::SourceOperator::ScalarIndexScan {
			index,
			direction,
			..
		} => Some((index, direction == crate::gql::optimizer::IndexDirection::Reverse)),
		_ => None,
	}
}

fn derived_source_rows<Read: QueryRead>(
	read: &Read,
	snapshot: Snapshot,
	select: &LogicalSelect,
	slots: &[Value],
	source: &str,
	control: &ExecutionControl,
) -> FerrosGrynnResult<RowSet> {
	let (select, physical) = prepare_physical_plan(read, snapshot, select, control)?;
	let select = &select;
	let output =
		execute_select_output(read, snapshot, select, &physical, slots, source, control, None)?;
	let start = match &select.start {
		None => 0,
		Some(LogicalSelectStart::Offset(value)) => eval_usize(value, slots, source, control)?,
		Some(LogicalSelectStart::After {
			..
		}) => {
			return Err(FerrosGrynnError::query_feature_not_supported(
				"START AFTER inside a derived SELECT source",
			));
		}
	};
	let mut remaining = select_limit(select, slots, source, control)?.unwrap_or(usize::MAX);
	if select.only {
		remaining = remaining.min(2);
	}
	let mut stream = output.into_stream()?;
	for _index in 0..start {
		if stream.next()?.is_none() {
			return RowCollector::new(read, control, false).finish();
		}
	}
	let mut rows = RowCollector::new(read, control, false);
	let mut ordinal = 0_usize;
	while ordinal < remaining {
		let Some(record) = stream.next()? else {
			break;
		};
		let row = match record.value {
			Value::Object(object) => Row::new(
				select
					.result
					.columns
					.iter()
					.map(|column| object.get(&column.name).cloned().unwrap_or(Value::None))
					.collect(),
			),
			value => Row::new(vec![value]),
		}
		.with_stable(record.anchor.stable_key)
		.with_sequence(vec![ordinal]);
		rows.push(read, snapshot, row)?;
		ordinal = ordinal.checked_add(1).ok_or_else(|| {
			FerrosGrynnError::invalid_argument("derived source row ordinal overflow")
		})?;
	}
	if select.only && ordinal > 1 {
		return Err(FerrosGrynnError::select_cardinality_violation(ordinal as u64)
			.with_phase(ExecutionPhase::Execution));
	}
	rows.finish()
}

fn rows_to_set<Read: QueryRead>(
	read: &Read,
	snapshot: Snapshot,
	control: &ExecutionControl,
	rows: Vec<Row>,
) -> FerrosGrynnResult<RowSet> {
	let mut collector = RowCollector::new(read, control, false);
	for (ordinal, mut row) in rows.into_iter().enumerate() {
		if row.sequence.is_empty() {
			row.sequence.push(ordinal);
		}
		collector.push(read, snapshot, row)?;
	}
	collector.finish()
}

fn resolve_graph_reference<Read: QueryRead>(
	read: &Read,
	snapshot: Snapshot,
	reference: &LogicalGraphReference,
	row: Option<&Row>,
	slots: &[Value],
	source: &str,
	control: &ExecutionControl,
) -> FerrosGrynnResult<GraphRef> {
	match reference {
		LogicalGraphReference::Static(reference) => Ok(reference.clone()),
		LogicalGraphReference::Expression(expression) => {
			match eval_select_expr(
				read,
				snapshot,
				expression,
				row.unwrap_or(&Row::new(Vec::new())),
				None,
				slots,
				source,
				control,
			)? {
				Value::GraphRef(reference) => Ok(*reference),
				Value::String(name) => read.catalog(snapshot).graph(name.as_str()),
				value => Err(FerrosGrynnError::invalid_argument(format!(
					"GRAPH reference requires graph_ref or string, found {}.",
					value.kind()
				))),
			}
		}
	}
}

fn split_rows<Read: QueryRead>(
	read: &Read,
	snapshot: Snapshot,
	rows: RowSet,
	expression: &TypedExpr,
	slots: &[Value],
	source: &str,
	control: &ExecutionControl,
) -> FerrosGrynnResult<RowSet> {
	let Some((column, path)) = split_binding(expression) else {
		return Err(FerrosGrynnError::invalid_argument(
			"SPLIT requires a bound source field or one of its nested paths.",
		));
	};
	let mut split = RowCollector::new(read, control, false);
	rows.consume(|row| {
		let value =
			eval_select_expr(read, snapshot, expression, &row, None, slots, source, control)?;
		let values: Vec<Value> = match value {
			Value::Array(values) => values.into_iter().collect(),
			Value::Set(values) => values.into_iter().collect(),
			Value::None | Value::Null => Vec::new(),
			value => {
				return Err(FerrosGrynnError::invalid_argument(format!(
					"SPLIT requires array or set, found {}.",
					value.kind()
				)));
			}
		};
		for (ordinal, value) in values.into_iter().enumerate() {
			let mut expanded = row.clone();
			let target = expanded.values.get_mut(column).ok_or_else(|| {
				FerrosGrynnError::invalid_argument("SPLIT source column is outside the row.")
			})?;
			set_split_path(target, &path, value)?;
			expanded.stable.push(Value::from(ordinal));
			expanded.sequence.push(ordinal);
			split.push(read, snapshot, expanded)?;
		}
		Ok(())
	})?;
	split.finish()
}

fn split_binding(expression: &TypedExpr) -> Option<(usize, Vec<Box<str>>)> {
	match &expression.kind {
		TypedExprKind::Column(column) => Some((*column, Vec::new())),
		TypedExprKind::Field {
			base,
			field,
			..
		} => {
			let (column, mut path) = split_binding(base)?;
			path.push(field.clone());
			Some((column, path))
		}
		_ => None,
	}
}

fn set_split_path(target: &mut Value, path: &[Box<str>], value: Value) -> FerrosGrynnResult<()> {
	let Some((field, tail)) = path.split_first() else {
		*target = value;
		return Ok(());
	};
	let Value::Object(object) = target else {
		return Err(FerrosGrynnError::invalid_argument(
			"SPLIT nested path crosses a non-object value.",
		));
	};
	let nested = object.get_mut(field).ok_or_else(|| {
		FerrosGrynnError::invalid_argument("SPLIT nested path is absent from its source object.")
	})?;
	set_split_path(nested, tail, value)
}

fn for_each_group<Read: QueryRead>(
	read: &Read,
	snapshot: Snapshot,
	plan: &LogicalSelect,
	rows: RowSet,
	slots: &[Value],
	source: &str,
	control: &ExecutionControl,
	mut visit: impl FnMut(usize, RowSet) -> FerrosGrynnResult<()>,
) -> FerrosGrynnResult<()> {
	let Some(keys) = &plan.group else {
		return rows.consume(|row| {
			let mut collector = RowCollector::new(read, control, false);
			collector.push(read, snapshot, row)?;
			visit(0, collector.finish()?)
		});
	};
	if keys.is_empty() {
		return visit(0, rows);
	}
	let root = control
		.temporary_directory
		.as_deref()
		.map(ToOwned::to_owned)
		.unwrap_or_else(|| read.query_scratch_root());
	let fields = keys
		.iter()
		.map(|_key| SortField {
			descending: false,
			nulls_first: false,
		})
		.collect();
	let mut sorter = QuerySorter::new(
		&root,
		read.spill_context(snapshot)?,
		fields,
		control.barrier_account(),
		false,
	)?;
	let mut ordinal = 0_usize;
	rows.consume(|row| {
		let key = keys
			.iter()
			.map(|expression| {
				eval_select_expr(read, snapshot, expression, &row, None, slots, source, control)
			})
			.collect::<FerrosGrynnResult<Vec<_>>>()?;
		let mut record = row_record(row, ordinal);
		record.anchor.order_keys = key;
		record.anchor.stable_key.push(Value::from(ordinal));
		ordinal = ordinal
			.checked_add(1)
			.ok_or_else(|| FerrosGrynnError::invalid_argument("query row ordinal overflow"))?;
		sorter.push(record)
	})?;
	let sorted = sorter.finish()?;
	let mut current_key = None::<Vec<Value>>;
	let mut current = None::<RowCollector>;
	let mut group_ordinal = 0_usize;
	consume_records(sorted, |record| {
		let key = record.anchor.order_keys.clone();
		if current_key.as_ref().is_some_and(|current| current != &key) {
			let group = current
				.take()
				.ok_or_else(|| FerrosGrynnError::invalid_argument("group collector is absent"))?
				.finish()?;
			visit(group_ordinal, group)?;
			group_ordinal = group_ordinal
				.checked_add(1)
				.ok_or_else(|| FerrosGrynnError::invalid_argument("group ordinal overflow"))?;
			current_key = None;
		}
		if current.is_none() {
			current = Some(RowCollector::new(read, control, false));
			current_key = Some(key);
		}
		current
			.as_mut()
			.ok_or_else(|| FerrosGrynnError::invalid_argument("group collector is absent"))?
			.push(read, snapshot, row_from_record(record)?)
	})?;
	if let Some(current) = current {
		visit(group_ordinal, current.finish()?)?;
	}
	Ok(())
}

fn project<Read: QueryRead>(
	read: &Read,
	snapshot: Snapshot,
	plan: &LogicalSelect,
	row: &Row,
	group: Option<&RowSet>,
	slots: &[Value],
	source: &str,
	control: &ExecutionControl,
) -> FerrosGrynnResult<(Value, Vec<Value>)> {
	let mut object = Object::new();
	let mut projected = Vec::new();
	for item in &plan.projection {
		match item {
			LogicalSelectItem::Wildcard(fields) => {
				for field in fields {
					match field {
						LogicalWildcardField::Column {
							name,
							index,
						} => {
							let value = row.value(*index).cloned().unwrap_or(Value::None);
							object.insert(name.as_ref(), value.clone());
							projected.push(value);
						}
						LogicalWildcardField::Properties {
							index,
						} => {
							let Value::Object(properties) =
								row.values.get(*index).cloned().unwrap_or(Value::None)
							else {
								continue;
							};
							for (name, value) in properties {
								object.insert(name, value);
							}
						}
					}
				}
			}
			LogicalSelectItem::Expression {
				name,
				expression,
			} => {
				let value = eval_select_expr(
					read, snapshot, expression, row, group, slots, source, control,
				)?;
				projected.push(value.clone());
				object.insert(name.as_ref(), value);
			}
		}
	}
	if plan.value {
		Ok((projected.first().cloned().unwrap_or(Value::None), projected))
	} else {
		Ok((Value::Object(object), projected))
	}
}

fn omit_path(value: &mut Value, path: &[Box<str>]) {
	let Some((head, tail)) = path.split_first() else {
		return;
	};
	let Value::Object(object) = value else {
		return;
	};
	if tail.is_empty() {
		object.remove(head);
	} else if let Some(value) = object.get_mut(head) {
		omit_path(value, tail);
	}
}

fn fetch_path<Read: QueryRead>(
	read: &Read,
	snapshot: Snapshot,
	value: &mut Value,
	path: &[Box<str>],
	graph: Option<&GraphRef>,
	control: &ExecutionControl,
) -> FerrosGrynnResult<()> {
	control.check()?;
	if let Value::Array(values) = value {
		for value in values {
			fetch_path(read, snapshot, value, path, graph, control)?;
		}
		return Ok(());
	}
	if let Value::Set(values) = value {
		let mut fetched = crate::values::Set::new();
		for mut value in std::mem::take(values) {
			fetch_path(read, snapshot, &mut value, path, graph, control)?;
			fetched.insert(value);
		}
		*values = fetched;
		return Ok(());
	}
	let Some((head, tail)) = path.split_first() else {
		return Ok(());
	};
	let Value::Object(object) = value else {
		return Ok(());
	};
	let Some(target) = object.get_mut(head) else {
		return Ok(());
	};
	*target = fetch_value(read, snapshot, target.clone(), graph, control)?;
	if !tail.is_empty() {
		fetch_path(read, snapshot, target, tail, graph, control)?;
	}
	Ok(())
}

fn fetch_value<Read: QueryRead>(
	read: &Read,
	snapshot: Snapshot,
	value: Value,
	graph: Option<&GraphRef>,
	control: &ExecutionControl,
) -> FerrosGrynnResult<Value> {
	control.check()?;
	match value {
		Value::Record(record) => {
			let row = match graph {
				Some(graph) => {
					read.get(snapshot, &record, Some(graph))?.or(read.get(snapshot, &record, None)?)
				}
				None => read.get(snapshot, &record, None)?,
			};
			let Some(row) = row else {
				return Ok(Value::None);
			};
			let schema = read.catalog(snapshot).schema(record.class())?;
			Ok(Value::Object(
				schema
					.columns
					.iter()
					.zip(row.values)
					.map(|(column, value)| (column.name.as_ref(), value))
					.collect(),
			))
		}
		Value::GraphRef(reference) => {
			let rows = read.graph_rows(snapshot, Some(std::slice::from_ref(reference.as_ref())))?;
			let Some(row) = rows.into_iter().next() else {
				return Ok(Value::None);
			};
			let schema = [
				"id",
				"name",
				"kind",
				"storage_mode",
				"revision",
				"node_count",
				"edge_count",
				"properties",
			];
			Ok(Value::Object(schema.into_iter().zip(row.values).collect()))
		}
		Value::Array(values) => values
			.into_iter()
			.map(|value| fetch_value(read, snapshot, value, graph, control))
			.collect::<FerrosGrynnResult<Array>>()
			.map(Value::Array),
		Value::Set(values) => values
			.into_iter()
			.map(|value| fetch_value(read, snapshot, value, graph, control))
			.collect::<FerrosGrynnResult<crate::values::Set>>()
			.map(Value::Set),
		value => Ok(value),
	}
}
