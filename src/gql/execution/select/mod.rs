#![allow(
	clippy::wildcard_imports,
	reason = "the SELECT executor composes the existing concrete query runtime"
)]

use std::borrow::Cow;

use super::*;
use crate::{
	gql::{
		cursor::{CursorAnchor, CursorRecord, IndexSeekCheckout, IndexSeekPlan},
		ir::LogicalKnnDistance,
		optimizer::fingerprint,
		spill::{QuerySorter, SortField, SortedRecords, compare_record_anchor},
	},
	values::Text,
};

mod admission;
mod aggregate_union;
#[cfg(test)]
mod atom_union_tests;
mod expression;
mod identity;
mod join;
mod join_family;
mod pagination;
mod rows;
mod search;
mod union;
#[cfg(test)]
mod window_tests;

use expression::*;
use identity::JoinedSources;
use join::*;
pub(super) use pagination::check_select_cardinality;
use pagination::*;
use rows::*;
use search::*;

/// Normalizes the root SDK family before global class-union reduction.
pub(in crate::gql) fn normalize_root_select(
	read: &RootRead<'_>,
	plan: &mut LogicalSelect,
	slots: &[Value],
) -> FerrosGrynnResult<()> {
	if let Cow::Owned(normalized) = join_family::collapse(read, plan, slots)? {
		*plan = normalized;
	}
	Ok(())
}

/// Row-free reactive values share SELECT special-expression materialization.
pub(super) fn eval_row_expression<Read: QueryRead>(
	read: &Read,
	snapshot: Snapshot,
	expression: &TypedExpr,
	row: &Row,
	slots: &[Value],
	source: &str,
	control: &ExecutionControl,
) -> FerrosGrynnResult<Value> {
	eval_select_expr(read, snapshot, expression, row, None, slots, source, control)
}

/// Keeps source identity through the shared SELECT projection and FETCH path.
pub(super) fn admit_graph_cursor_frame<Read: QueryRead>(
	read: &Read,
	plan: &LogicalSelect,
	slots: &[Value],
	source: &str,
) -> FerrosGrynnResult<()> {
	admission::select_copy(read, plan, 0)?;
	if let Some(fields) = read.bounded_fields() {
		fields.admit(
			source
				.len()
				.checked_add(
					slots.len().checked_mul(std::mem::size_of::<Value>()).ok_or_else(|| {
						FerrosGrynnError::operation_overflow("graph cursor slots")
					})?,
				)
				.ok_or_else(|| FerrosGrynnError::operation_overflow("graph cursor frame"))?,
		)?;
		for value in slots {
			fields.admit_value(value)?;
		}
	}
	Ok(())
}

/// Keeps source identity through the shared SELECT projection and FETCH path.
#[allow(clippy::too_many_arguments, reason = "scope binding retains the SELECT invocation context")]
pub(super) fn bind_terminal_graph_scope<'plan, Read: QueryRead>(
	read: &Read,
	snapshot: Snapshot,
	plan: &'plan LogicalSelect,
	slots: &[Value],
	source: &str,
	control: &ExecutionControl,
	outer: Option<&Row>,
) -> FerrosGrynnResult<(Cow<'plan, LogicalSelect>, Option<GraphRef>)> {
	let Some(reference) = plan.source.kind.graph_scope() else {
		return Ok((Cow::Borrowed(plan), None));
	};
	let graph = resolve_graph_reference(read, snapshot, reference, outer, slots, source, control)?;
	if matches!(reference, LogicalGraphReference::Static(_)) {
		return Ok((Cow::Borrowed(plan), Some(graph)));
	}
	admission::select_copy(read, plan, 0)?;
	if let Some(fields) = read.bounded_fields() {
		fields.admit(
			graph
				.name()
				.len()
				.checked_add(std::mem::size_of::<GraphRef>())
				.ok_or_else(|| FerrosGrynnError::operation_overflow("graph terminal scope copy"))?,
		)?;
	}
	let mut bound = plan.clone();
	let reference =
		bound.source.kind.graph_scope_mut().ok_or_else(|| {
			FerrosGrynnError::invalid_argument("graph terminal scope disappeared")
		})?;
	*reference = LogicalGraphReference::Static(graph.clone());
	Ok((Cow::Owned(bound), Some(graph)))
}

/// Keeps source identity through the shared SELECT projection and FETCH path.
pub(in crate::gql) fn visit_root_select(
	read: &RootRead<'_>,
	logical: &LogicalSelect,
	slots: &[Value],
	control: &ExecutionControl,
	mut visit: impl FnMut(crate::live::LiveIdentity, Value) -> FerrosGrynnResult<()>,
) -> FerrosGrynnResult<()> {
	let snapshot = read.snapshot()?;
	let (mut plan, physical) = prepare_physical_plan(read, snapshot, logical, control)?;
	let joined_sources = if plan.joins.is_empty() {
		None
	} else {
		Some(JoinedSources::prepare(read, snapshot, &mut plan, slots, control)?)
	};
	let fields = RecordFields {
		read,
		snapshot,
		graph: None,
	};
	let (start, limit) = select_window(&plan, slots, "", &fields, control)?;
	if limit == 0 {
		validate_zero_window(read, snapshot, &plan, slots, "", control, None)?;
		return Ok(());
	}
	if let Some(count) = visit_streaming_select_values(
		read,
		snapshot,
		&plan,
		&physical,
		slots,
		"",
		control,
		None,
		start,
		limit,
		true,
		&mut |row, value| {
			let identity = if let Some(row) = row {
				let Some(Value::Record(record)) = row.value(0) else {
					return Err(FerrosGrynnError::invalid_storage_file(
						"LIVE source row has no logical record identity",
					));
				};
				root_record_identity(read, record, row.graph.as_ref())?
			} else {
				crate::live::LiveIdentity::Aggregate(Vec::new())
			};
			visit(identity, value)
		},
	)? {
		return check_select_cardinality(plan.only, count);
	}
	let graph = match &plan.source.kind {
		LogicalSelectSourceKind::Atoms {
			graph,
			..
		} => graph
			.as_ref()
			.map(|reference| {
				resolve_graph_reference(read, snapshot, reference, None, slots, "", control)
			})
			.transpose()?,
		_ => None,
	};
	let output = execute_select_output(
		read,
		snapshot,
		&plan,
		&physical,
		slots,
		"",
		control,
		None,
		joined_sources.as_ref(),
	)?;
	let count = visit_records(read, output, start, limit, control, |record| {
		let identity = if plan.group.is_some() {
			let mut keys = record.anchor.stable_key;
			keys.pop();
			crate::live::LiveIdentity::Aggregate(keys)
		} else if !plan.joins.is_empty() {
			identity::joined_identity(
				read,
				&record.anchor.stable_key,
				plan.joins.len() + 1,
				plan.split.len(),
			)?
		} else {
			root_row_identity(read, &record.anchor.stable_key, graph.as_ref(), plan.split.len())?
		};
		visit(identity, record.value)
	})?;
	check_select_cardinality(plan.only, count)
}

/// Copies a qualified identity independently of its projected result image.
fn root_record_identity(
	read: &RootRead<'_>,
	record: &RecordId,
	graph: Option<&GraphRef>,
) -> FerrosGrynnResult<crate::live::LiveIdentity> {
	read.fields().admit(
		record
			.estimated_heap_bytes()
			.checked_add(128)
			.ok_or_else(|| FerrosGrynnError::operation_overflow("LIVE record identity"))?,
	)?;
	if let Some(graph) = graph {
		read.fields().admit(
			graph
				.name()
				.len()
				.checked_add(std::mem::size_of::<GraphRef>())
				.ok_or_else(|| FerrosGrynnError::operation_overflow("LIVE graph identity"))?,
		)?;
		Ok(crate::live::LiveIdentity::Occurrence(AtomOccurrenceRef::new(
			crate::graph::AtomRef::new(read.class_kind(record.class())?, record.clone()),
			Arc::new(graph.clone()),
		)))
	} else {
		Ok(crate::live::LiveIdentity::Record(record.clone()))
	}
}

/// Applies one row-local window over all class outputs of a qualified domain.
pub(in crate::gql) fn visit_root_row_union(
	read: &RootRead<'_>,
	plans: &[&LogicalSelect],
	slots: &[Value],
	control: &ExecutionControl,
	mut visit: impl FnMut(crate::live::LiveIdentity, Value) -> FerrosGrynnResult<()>,
) -> FerrosGrynnResult<()> {
	let snapshot = read.snapshot()?;
	let graph = plans
		.first()
		.filter(|plan| plan.joins.is_empty())
		.and_then(|plan| plan.source.kind.graph_scope())
		.map(|reference| {
			resolve_graph_reference(read, snapshot, reference, None, slots, "", control)
		})
		.transpose()?;
	union::visit_rows(read, plans, slots, control, None, true, |record| {
		visit(
			if plans.first().is_some_and(|plan| !plan.joins.is_empty()) {
				identity::joined_identity(
					read,
					&record.anchor.stable_key,
					plans[0].joins.len() + 1,
					plans[0].split.len(),
				)?
			} else {
				root_row_identity(
					read,
					&record.anchor.stable_key,
					graph.as_ref(),
					plans.first().map_or(0, |plan| plan.split.len()),
				)?
			},
			record.value,
		)
	})?;
	Ok(())
}

/// SPLIT adds exact positions to an original source anchor. The final output
/// ordinal is only a sorter tie breaker and never a result identity part.
fn root_row_identity(
	read: &RootRead<'_>,
	stable: &[Value],
	graph: Option<&GraphRef>,
	splits: usize,
) -> FerrosGrynnResult<crate::live::LiveIdentity> {
	use crate::live::{LiveIdentity, LiveIdentityPart};
	let Some(Value::Record(record)) = stable.first() else {
		return Err(FerrosGrynnError::invalid_storage_file(
			"LIVE output lost its original source identity",
		));
	};
	if splits == 0 {
		return root_record_identity(read, record, graph);
	}
	let count = splits
		.checked_add(1)
		.ok_or_else(|| FerrosGrynnError::operation_overflow("LIVE SPLIT identity"))?;
	if stable.len()
		!= count
			.checked_add(1)
			.ok_or_else(|| FerrosGrynnError::operation_overflow("LIVE SPLIT anchor"))?
	{
		return Err(FerrosGrynnError::invalid_storage_file("LIVE SPLIT anchor layout mismatch"));
	}
	read.fields().work(count as u64)?;
	read.fields().admit(
		count.checked_mul(std::mem::size_of::<LiveIdentityPart>()).ok_or_else(|| {
			FerrosGrynnError::operation_overflow("LIVE SPLIT identity allocation")
		})?,
	)?;
	let mut parts = Vec::new();
	parts.try_reserve_exact(count).map_err(|error| {
		FerrosGrynnError::traversal_resource_limit_exceeded("LIVE SPLIT identity", count as u64)
			.with_technical_source(error)
	})?;
	parts.push(match root_record_identity(read, record, graph)? {
		LiveIdentity::Record(record) => LiveIdentityPart::Record(record),
		LiveIdentity::Occurrence(occurrence) => LiveIdentityPart::Occurrence(occurrence),
		_ => {
			return Err(FerrosGrynnError::invalid_storage_file(
				"LIVE source identity kind mismatch",
			));
		}
	});
	for position in &stable[1..count] {
		let Value::Number(number) = position else {
			return Err(FerrosGrynnError::invalid_storage_file(
				"LIVE SPLIT position is not numeric",
			));
		};
		parts.push(LiveIdentityPart::Split(u64::try_from(*number)?));
	}
	Ok(LiveIdentity::Composite(parts))
}

/// Keeps class layouts tagged through one existing aggregate reduction.
pub(in crate::gql) fn visit_root_aggregate_union(
	read: &RootRead<'_>,
	plans: &[&LogicalSelect],
	slots: &[Value],
	control: &ExecutionControl,
	visit: impl FnMut(crate::live::LiveIdentity, Value) -> FerrosGrynnResult<()>,
) -> FerrosGrynnResult<()> {
	visit_aggregate_union(read, plans, slots, control, visit)
}

/// Nested independent SELECTs share the reduction without exposing the
/// executor's private read trait outside its owner.
fn visit_aggregate_union<Read: QueryRead>(
	read: &Read,
	plans: &[&LogicalSelect],
	slots: &[Value],
	control: &ExecutionControl,
	mut visit: impl FnMut(crate::live::LiveIdentity, Value) -> FerrosGrynnResult<()>,
) -> FerrosGrynnResult<()> {
	let Some(first) = plans.first() else {
		return Ok(());
	};
	if plans.iter().any(|input| input.only != first.only) {
		return Err(FerrosGrynnError::invalid_argument("LIVE union ONLY mismatch"));
	}
	let fields = read.bounded_fields().ok_or_else(|| {
		FerrosGrynnError::invalid_argument("independent SELECT union requires bounded evaluation")
	})?;
	let snapshot = read.snapshot()?;
	let (mut plan, mut physical) = prepare_physical_plan(read, snapshot, first, control)?;
	let row_fields = RecordFields {
		read,
		snapshot,
		graph: None,
	};
	let (start, limit) = select_window(&plan, slots, "", &row_fields, control)?;
	if limit == 0 {
		for input in plans {
			if !input.joins.is_empty() {
				let (mut scoped, _physical) =
					prepare_physical_plan(read, snapshot, input, control)?;
				JoinedSources::prepare(read, snapshot, &mut scoped, slots, control)?;
				validate_zero_window(read, snapshot, &scoped, slots, "", control, None)?;
			} else {
				validate_zero_window(read, snapshot, input, slots, "", control, None)?;
			}
		}
		return Ok(());
	}
	let Some(group) = &mut plan.group else {
		return Err(FerrosGrynnError::invalid_argument(
			"LIVE graph aggregate union requires grouping",
		));
	};
	let class_column = plans.iter().map(|plan| plan.input_schema.columns.len()).max().unwrap_or(0);
	fields.admit(
		plans
			.len()
			.checked_mul(std::mem::size_of::<&[TypedExpr]>())
			.ok_or_else(|| FerrosGrynnError::operation_overflow("LIVE union group variants"))?,
	)?;
	let groups = plans
		.iter()
		.map(|input| {
			input
				.group
				.as_deref()
				.filter(|keys| keys.len() == group.len())
				.ok_or_else(|| FerrosGrynnError::invalid_argument("LIVE union group mismatch"))
		})
		.collect::<FerrosGrynnResult<Vec<_>>>()?;
	for (index, key) in group.iter_mut().enumerate() {
		fields.admit(plans.len().checked_mul(std::mem::size_of::<&TypedExpr>()).ok_or_else(
			|| FerrosGrynnError::operation_overflow("LIVE union group key variants"),
		)?)?;
		let variants = groups.iter().map(|keys| &keys[index]).collect::<Vec<_>>();
		*key = aggregate_union::row_expression(read, &variants, class_column)?;
	}
	for (index, output) in plan.projection.iter_mut().enumerate() {
		let LogicalSelectItem::Expression {
			expression,
			..
		} = output
		else {
			return Err(FerrosGrynnError::invalid_argument("LIVE aggregate wildcard projection"));
		};
		fields.admit(plans.len().checked_mul(std::mem::size_of::<&TypedExpr>()).ok_or_else(
			|| FerrosGrynnError::operation_overflow("LIVE aggregate projection variants"),
		)?)?;
		let variants = plans
			.iter()
			.map(|plan| match plan.projection.get(index) {
				Some(LogicalSelectItem::Expression {
					expression,
					..
				}) => Ok(expression),
				_ => Err(FerrosGrynnError::invalid_argument("LIVE aggregate projection mismatch")),
			})
			.collect::<FerrosGrynnResult<Vec<_>>>()?;
		*expression =
			aggregate_union::merge_expression(read, &variants, class_column, slots, &groups)?;
	}
	for input in plans {
		union::check_order(first, input)?;
	}
	for (index, order) in plan.order.iter_mut().enumerate() {
		order.expression =
			aggregate_union::merge_order(read, plans, index, class_column, slots, &groups)?;
	}
	physical.requires_sort |= !plan.order.is_empty();
	let mut merged = RowCollector::new(read, control, false);
	for (ordinal, input) in plans.iter().enumerate() {
		if input.filter.as_ref().is_some_and(contains_search) {
			return Err(FerrosGrynnError::query_feature_not_supported(
				"LIVE aggregate union with relational barriers",
			));
		}
		let (mut input, input_physical) = prepare_physical_plan(read, snapshot, input, control)?;
		if !input.joins.is_empty() {
			JoinedSources::prepare(read, snapshot, &mut input, slots, control)?;
		}
		let (rows, prepared_search) = prepare_select_input(
			read,
			snapshot,
			&input,
			&input_physical,
			slots,
			"",
			control,
			None,
		)?;
		let rows = prepare_select_rows(
			read,
			snapshot,
			&input,
			&input_physical,
			rows,
			slots,
			"",
			control,
			prepared_search.as_ref(),
		)?;
		rows.consume(|mut row| {
			fields.work(class_column as u64)?;
			fields.admit(
				class_column
					.checked_add(1)
					.and_then(|count| count.checked_mul(2 * std::mem::size_of::<Value>()))
					.ok_or_else(|| FerrosGrynnError::operation_overflow("LIVE union row"))?,
			)?;
			row.values.try_reserve_exact(class_column + 1 - row.values.len()).map_err(|error| {
				FerrosGrynnError::traversal_resource_limit_exceeded(
					"LIVE union row",
					class_column as u64,
				)
				.with_technical_source(error)
			})?;
			row.values.resize(class_column, Value::None);
			row.values.push(Value::from(ordinal));
			merged.push(read, snapshot, row)
		})?;
	}
	plan.filter = None;
	plan.split.clear();
	let output = finish_select_rows(
		read,
		snapshot,
		&plan,
		&physical,
		merged.finish()?,
		slots,
		"",
		control,
		None,
		None,
	)?;
	let count = visit_records(read, output, start, limit, control, |record| {
		let mut keys = record.anchor.stable_key;
		keys.pop();
		visit(crate::live::LiveIdentity::Aggregate(keys), record.value)
	})?;
	check_select_cardinality(plan.only, count)
}

/// Prepares physical plan.
fn prepare_physical_plan<Read: QueryRead>(
	read: &Read,
	snapshot: Snapshot,
	logical: &LogicalSelect,
	control: &ExecutionControl,
) -> FerrosGrynnResult<(LogicalSelect, crate::gql::optimizer::PhysicalSelectPlan)> {
	if read.bounded_fields().is_some() {
		admission::select_copy(read, logical, 0)?;
	}
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

/// Retires a direct continuation when preparation fails before evaluation.
/// Computed inputs are never evaluated again for cleanup.
pub(super) fn discard_supplied_cursor(
	plan: &LogicalSelect,
	slots: &[Value],
	control: &ExecutionControl,
) {
	let Some(LogicalSelectStart::After {
		cursor,
		..
	}) = plan.start.as_ref()
	else {
		return;
	};
	let supplied = match &cursor.kind {
		TypedExprKind::Slot(slot) => slots.get(*slot as usize),
		TypedExprKind::Constant(value) => Some(value),
		_ => None,
	};
	if let Some(Value::Cursor(cursor)) = supplied {
		control.discard_cursor(*cursor);
	}
}

/// Executes select.
pub(super) fn execute_select<Read: QueryRead>(
	read: &Read,
	snapshot: Snapshot,
	plan: &LogicalSelect,
	slots: &[Value],
	source: &str,
	control: &ExecutionControl,
) -> FerrosGrynnResult<Value> {
	if plan.return_graph.is_some() {
		return graph_result::execute_select_graph(
			read, snapshot, plan, slots, source, control, None, true,
		);
	}
	execute_select_projected(read, snapshot, plan, slots, source, control)
}

/// Preserves ordinary retained-session pagination before terminal assembly.
pub(super) fn execute_select_projected<Read: QueryRead>(
	read: &Read,
	snapshot: Snapshot,
	plan: &LogicalSelect,
	slots: &[Value],
	source: &str,
	control: &ExecutionControl,
) -> FerrosGrynnResult<Value> {
	let values = execute_select_projected_page(read, snapshot, plan, slots, source, control)?;
	if plan.only {
		return Ok(values.into_iter().next().unwrap_or(Value::None));
	}
	Ok(Value::Array(values.into_iter().collect()))
}

/// Exact pages use their original spool/index authority without fresh planning.
pub(super) fn resume_retained_page(
	control: &ExecutionControl,
	cursor: crate::values::QueryCursor,
	identity: (u64, u64),
	page_size: usize,
) -> FerrosGrynnResult<Vec<Value>> {
	let runtime = control.cursors().ok_or_else(|| {
		FerrosGrynnError::invalid_query_cursor("continuation requires a query session")
	})?;
	control.check()?;
	let checkout = runtime.lock().checkout_index_seek(cursor, identity.0, identity.1)?;
	if let Some(checkout) = checkout {
		return resume_index_seek(runtime, checkout, page_size, control);
	}
	let page = runtime.lock().page(cursor, identity.0, identity.1, page_size)?;
	if let Err(error) = control.check() {
		control.discard_cursor(cursor);
		return Err(error);
	}
	if let Some(cursor) = page.continuation {
		control.set_continuation(cursor);
	}
	Ok(page.values)
}

/// Keeps an empty ONLY window distinct from a selected VALUE NONE row.
pub(super) fn execute_select_projected_page<Read: QueryRead>(
	read: &Read,
	snapshot: Snapshot,
	plan: &LogicalSelect,
	slots: &[Value],
	source: &str,
	control: &ExecutionControl,
) -> FerrosGrynnResult<Vec<Value>> {
	if let Some(aliases) = &plan.fetch_bridges {
		for alias in aliases.iter().flatten() {
			super::bridge::resolve(read, snapshot, alias, control, false)?;
		}
	}
	let fields = RecordFields {
		read,
		snapshot,
		graph: None,
	};
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
			let value = match eval(cursor, None, slots, source, Some(&fields), Some(control)) {
				Ok(value) => value,
				Err(error) => {
					discard_supplied_cursor(plan, slots, control);
					return Err(error);
				}
			};
			let cursor = match value {
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
			let page_size = match eval_usize(limit, slots, source, &fields, control) {
				Ok(page_size) => page_size,
				Err(error) => {
					control.discard_cursor(cursor);
					return Err(error);
				}
			};
			if !*read_committed {
				resume_retained_page(control, cursor, (fingerprint, order_fingerprint), page_size)?
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
						read, snapshot, plan, &physical, slots, source, control, None, None,
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
					eval_usize(value, slots, source, &fields, control)?
				}
				None => 0,
				Some(LogicalSelectStart::After {
					..
				}) => unreachable!(),
			};
			let limit = select_limit(plan, slots, source, &fields, control)?;
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
					read, snapshot, plan, &physical, slots, source, control, None, None,
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
		check_select_cardinality(true, values.len())?;
	}
	Ok(values)
}

#[allow(
	clippy::too_many_arguments,
	reason = "continuation registration retains the complete analyzed read state"
)]
/// Finishes new index seek page.
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

/// Returns the resume index seek result.
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
			scalar_graph: None,
			transaction: &mut checkout.transaction,
			snapshot,
			metadata_only: false,
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
/// Executes index seek page.
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
		|| matches!(
			plan.source.kind,
			LogicalSelectSourceKind::Derived(_) | LogicalSelectSourceKind::AtomUnion { .. }
		) {
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
		false,
		|row| {
			let output = build_output_row(
				read, snapshot, plan, row, None, ordinal, None, slots, source, control, None,
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

/// Executes select values.
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
	let mut values = Vec::new();
	visit_select_values(read, snapshot, plan, slots, source, control, outer, maximum, |value| {
		if let Some(fields) = read.bounded_fields() {
			fields.work(1)?;
			fields.admit(2 * std::mem::size_of::<Value>())?;
		}
		values.push(value);
		Ok(())
	})?;
	Ok(values)
}

#[allow(
	clippy::too_many_arguments,
	reason = "SELECT pull execution retains the complete analyzed statement context"
)]
/// Visits select values.
pub(super) fn visit_select_values<Read: QueryRead>(
	read: &Read,
	snapshot: Snapshot,
	plan: &LogicalSelect,
	slots: &[Value],
	source: &str,
	control: &ExecutionControl,
	outer: Option<&Row>,
	maximum: Option<usize>,
	mut visit: impl FnMut(Value) -> FerrosGrynnResult<()>,
) -> FerrosGrynnResult<usize> {
	if plan.return_graph.is_some() {
		if maximum == Some(0) {
			return Ok(0);
		}
		visit(graph_result::execute_select_graph(
			read, snapshot, plan, slots, source, control, outer, false,
		)?)?;
		return Ok(1);
	}
	visit_select_projected_values(
		read, snapshot, plan, slots, source, control, outer, maximum, visit,
	)
}

/// Visits ordinary projected rows before any terminal graph materialization.
#[allow(
	clippy::too_many_arguments,
	reason = "the shared SELECT visitor retains the invocation context"
)]
pub(super) fn visit_select_projected_values<Read: QueryRead>(
	read: &Read,
	snapshot: Snapshot,
	plan: &LogicalSelect,
	slots: &[Value],
	source: &str,
	control: &ExecutionControl,
	outer: Option<&Row>,
	maximum: Option<usize>,
	mut visit: impl FnMut(Value) -> FerrosGrynnResult<()>,
) -> FerrosGrynnResult<usize> {
	visit_select_values_dyn(
		read, snapshot, plan, slots, source, control, outer, maximum, &mut visit,
	)
}

/// Recursive independent unions reuse one callback type instead of nesting
/// mutable references at each lexical SELECT level.
#[allow(
	clippy::too_many_arguments,
	reason = "SELECT pull execution retains the complete analyzed statement context"
)]
fn visit_select_values_dyn<Read: QueryRead>(
	read: &Read,
	snapshot: Snapshot,
	plan: &LogicalSelect,
	slots: &[Value],
	source: &str,
	control: &ExecutionControl,
	outer: Option<&Row>,
	maximum: Option<usize>,
	visit: &mut dyn FnMut(Value) -> FerrosGrynnResult<()>,
) -> FerrosGrynnResult<usize> {
	if let Some(aliases) = &plan.fetch_bridges {
		for alias in aliases.iter().flatten() {
			super::bridge::resolve(read, snapshot, alias, control, false)?;
		}
	}
	if let Cow::Owned(normalized) = join_family::collapse(read, plan, slots)? {
		return visit_select_values_dyn(
			read,
			snapshot,
			&normalized,
			slots,
			source,
			control,
			outer,
			maximum,
			visit,
		);
	}
	if !plan.union_inputs.is_empty() {
		let fields = read.bounded_fields().ok_or_else(|| {
			FerrosGrynnError::invalid_argument(
				"independent SELECT union requires bounded evaluation",
			)
		})?;
		let inputs = if let Some(reference) = plan.source.kind.graph_scope() {
			admission::graph_copy(read, reference, 0)?;
			let graph =
				resolve_graph_reference(read, snapshot, reference, None, slots, source, control)?;
			read.graph(snapshot, &graph)?;
			fields.admit(
				plan.union_inputs
					.len()
					.checked_mul(std::mem::size_of::<LogicalSelect>())
					.ok_or_else(|| {
						FerrosGrynnError::operation_overflow("LIVE union scope plans")
					})?,
			)?;
			let mut inputs = Vec::with_capacity(plan.union_inputs.len());
			for input in &plan.union_inputs {
				admission::select_copy(read, input, 0)?;
				let mut input = input.clone();
				let reference = input.source.kind.graph_scope_mut().ok_or_else(|| {
					FerrosGrynnError::invalid_storage_file("LIVE union input has no graph scope")
				})?;
				fields.admit(
					graph.name().len().checked_add(std::mem::size_of_val(&graph)).ok_or_else(
						|| FerrosGrynnError::operation_overflow("LIVE union scope copy"),
					)?,
				)?;
				*reference = LogicalGraphReference::Static(graph.clone());
				inputs.push(input);
			}
			Cow::Owned(inputs)
		} else {
			Cow::Borrowed(plan.union_inputs.as_slice())
		};
		let inputs = if inputs.iter().any(|input| {
			input.union_purpose == crate::syn::SelectUnionPurpose::JoinedTypingVariants
				&& !input.union_inputs.is_empty()
		}) {
			fields.admit(
				inputs
					.len()
					.checked_mul(std::mem::size_of::<LogicalSelect>())
					.ok_or_else(|| FerrosGrynnError::operation_overflow("nested JOIN families"))?,
			)?;
			let mut normalized = Vec::with_capacity(inputs.len());
			for input in inputs.iter() {
				match join_family::collapse(read, input, slots)? {
					Cow::Owned(input) => normalized.push(input),
					Cow::Borrowed(input) => {
						admission::select_copy(read, input, 0)?;
						normalized.push(input.clone());
					}
				}
			}
			Cow::Owned(normalized)
		} else {
			inputs
		};
		let limit = maximum.unwrap_or(usize::MAX);
		let mut count = 0_usize;
		if inputs[0].group.is_some() {
			fields.admit(
				inputs
					.len()
					.checked_mul(std::mem::size_of::<&LogicalSelect>())
					.ok_or_else(|| FerrosGrynnError::operation_overflow("LIVE union plans"))?,
			)?;
			let inputs = inputs.iter().collect::<Vec<_>>();
			visit_aggregate_union(read, &inputs, slots, control, |_identity, value| {
				if count < limit {
					visit(value)?;
					count += 1;
				}
				Ok(())
			})?;
		} else if union::needs_window(&inputs[0]) {
			fields.admit(
				inputs
					.len()
					.checked_mul(std::mem::size_of::<&LogicalSelect>())
					.ok_or_else(|| FerrosGrynnError::operation_overflow("LIVE union plans"))?,
			)?;
			let inputs = inputs.iter().collect::<Vec<_>>();
			count = union::visit_rows(read, &inputs, slots, control, maximum, false, |record| {
				visit(record.value)
			})?;
		} else {
			for input in inputs.iter() {
				if count == limit {
					break;
				}
				count += visit_select_values_dyn(
					read,
					snapshot,
					input,
					slots,
					source,
					control,
					None,
					Some(limit - count),
					visit,
				)?;
			}
		}
		check_select_cardinality(plan.only, count)?;
		return Ok(count);
	}
	let (plan, physical) = prepare_physical_plan(read, snapshot, plan, control)?;
	let plan = &plan;
	let fields = RecordFields {
		read,
		snapshot,
		graph: None,
	};
	let (start, limit) = select_window(plan, slots, source, &fields, control)?;
	let limit = limit.min(maximum.unwrap_or(usize::MAX));
	if limit == 0 {
		validate_zero_window(read, snapshot, plan, slots, source, control, outer)?;
		return Ok(0);
	}
	let count = match visit_streaming_select_values(
		read,
		snapshot,
		plan,
		&physical,
		slots,
		source,
		control,
		outer,
		start,
		limit,
		false,
		&mut |_row, value| visit(value),
	)? {
		Some(count) => count,
		None => {
			let output = execute_select_output(
				read, snapshot, plan, &physical, slots, source, control, outer, None,
			)?;
			visit_record_values(read, output, start, limit, control, &mut *visit)?
		}
	};
	check_select_cardinality(plan.only, count)?;
	Ok(count)
}

#[allow(
	clippy::too_many_arguments,
	reason = "the direct pull path retains the complete analyzed SELECT context"
)]
/// Executes streaming select values.
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
	let mut values = Vec::with_capacity(limit.min(1024));
	let result = visit_streaming_select_values(
		read,
		snapshot,
		plan,
		physical,
		slots,
		source,
		control,
		outer,
		start,
		limit,
		false,
		&mut |_row, value| {
			values.push(value);
			Ok(())
		},
	)?;
	Ok(result.map(|_count| values))
}

#[allow(
	clippy::too_many_arguments,
	reason = "the direct pull path retains the complete analyzed SELECT context"
)]
/// Visits streaming select values.
fn visit_streaming_select_values<Read: QueryRead>(
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
	transient_rows: bool,
	visit: &mut impl FnMut(Option<&Row>, Value) -> FerrosGrynnResult<()>,
) -> FerrosGrynnResult<Option<usize>> {
	if !plan.joins.is_empty()
		|| plan.group.is_some()
		|| !plan.split.is_empty()
		|| (!plan.order.is_empty() && (physical.requires_sort || control.cursors().is_some()))
		|| plan.tempfiles
		|| plan.filter.as_ref().is_some_and(contains_search)
		|| physical.sources.iter().any(|source| {
			matches!(
				source.operator,
				crate::gql::optimizer::SourceOperator::GeometryIndexScan { .. }
			)
		})
		|| matches!(
			plan.source.kind,
			LogicalSelectSourceKind::Derived(_) | LogicalSelectSourceKind::AtomUnion { .. }
		) {
		return Ok(None);
	}
	if let LogicalSelectSourceKind::Empty {
		graph,
	} = &plan.source.kind
	{
		if let Some(reference) = graph {
			validate_empty_graph(read, snapshot, reference, outer, slots, source, control)?;
		}
		return Ok(Some(0));
	}
	let limit = select_pull_limit(plan.only, limit);
	if limit == 0 {
		return Ok(Some(0));
	}
	if let Some(crate::gql::optimizer::SourceOperator::CountLookup {
		index,
		..
	}) = physical.sources.first().map(|source| &source.operator)
	{
		if start != 0 {
			return Ok(Some(0));
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
			let [
				LogicalSelectItem::Expression {
					name,
					..
				},
			] = plan.projection.as_slice()
			else {
				return Err(FerrosGrynnError::invalid_argument(
					"CountLookup requires one aggregate projection",
				));
			};
			let mut value = Object::new();
			value.insert(name.as_ref(), count);
			Value::Object(value)
		};
		visit(None, value)?;
		return Ok(Some(1));
	}
	let mut visited = 0_usize;
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
		transient_rows,
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
			visit(Some(&row), output)?;
			visited = visited.checked_add(1).ok_or_else(|| {
				FerrosGrynnError::invalid_argument("projected row count overflow")
			})?;
			Ok(visited < limit)
		},
	)?;
	Ok(Some(visited))
}

#[allow(
	clippy::too_many_arguments,
	reason = "source streaming retains snapshot, expression, and outer-binding context"
)]
/// Visits streaming source rows.
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
	transient_rows: bool,
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
		LogicalSelectSourceKind::Empty {
			graph,
		} => {
			if let Some(reference) = graph {
				validate_empty_graph(read, snapshot, reference, outer, slots, source, control)?;
			}
		}
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
						(slots, source),
						full_row,
					)?;
					let mut ordinal = 0_usize;
					loop {
						let _scope = if transient_rows {
							read.bounded_fields().and_then(|fields| fields.transient_scope())
						} else {
							None
						};
						let Some(row) = cursor.next(control)? else {
							break;
						};
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
		LogicalSelectSourceKind::Derived(_)
		| LogicalSelectSourceKind::AtomUnion {
			..
		} => {
			return Err(FerrosGrynnError::invalid_argument(
				"materialized source reached the direct pull path",
			));
		}
	}
	Ok(())
}

#[allow(
	clippy::too_many_arguments,
	reason = "root identities share the existing SELECT execution context"
)]
/// Executes select output.
fn execute_select_output<Read: QueryRead>(
	read: &Read,
	snapshot: Snapshot,
	plan: &LogicalSelect,
	physical: &crate::gql::optimizer::PhysicalSelectPlan,
	slots: &[Value],
	source: &str,
	control: &ExecutionControl,
	outer: Option<&Row>,
	joined_sources: Option<&JoinedSources>,
) -> FerrosGrynnResult<SortedRecords> {
	let (rows, prepared_search) =
		prepare_select_input(read, snapshot, plan, physical, slots, source, control, outer)?;
	finish_select_rows(
		read,
		snapshot,
		plan,
		physical,
		rows,
		slots,
		source,
		control,
		prepared_search.as_ref(),
		joined_sources,
	)
}

/// Source scans, joins and outer-row bindings share one prepared input owner.
///
/// # Errors
/// Propagates the existing read, expression, resource and cancellation errors.
#[allow(
	clippy::too_many_arguments,
	reason = "input preparation retains the existing SELECT context"
)]
fn prepare_select_input<Read: QueryRead>(
	read: &Read,
	snapshot: Snapshot,
	plan: &LogicalSelect,
	physical: &crate::gql::optimizer::PhysicalSelectPlan,
	slots: &[Value],
	source: &str,
	control: &ExecutionControl,
	outer: Option<&Row>,
) -> FerrosGrynnResult<(RowSet, Option<PreparedSearch>)> {
	control.check()?;
	let fields = RecordFields {
		read,
		snapshot,
		graph: None,
	};
	let mut prepared_search = plan
		.filter
		.as_ref()
		.filter(|filter| contains_search(filter))
		.map(|filter| prepare_search_inputs(filter, &plan.access, slots, source, &fields, control))
		.transpose()?;
	let local_width = plan.input_schema.columns.len().saturating_sub(plan.outer_columns);
	let physical_search = if plan.joins.is_empty() {
		match (plan.filter.as_ref(), prepared_search.as_mut()) {
			(Some(filter), Some(prepared)) => Some((filter, &plan.access, prepared)),
			_ => None,
		}
	} else {
		None
	};
	let mut rows = source_rows(
		read,
		snapshot,
		logical_source(plan, physical.execution_order.first().copied().unwrap_or(0)),
		physical.execution_order.first().and_then(|source| physical.sources.get(*source)),
		plan.filter.as_ref(),
		physical
			.pushed_filter
			.then_some(plan.filter.as_ref())
			.flatten()
			.filter(|filter| !contains_search(filter)),
		SourceEvaluation {
			slots,
			source,
			control,
			outer,
		},
		physical_search,
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
			plan.filter.as_ref(),
			None,
			SourceEvaluation {
				slots,
				source,
				control,
				outer: None,
			},
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
	Ok((rows, prepared_search))
}

/// Shared WHERE/SPLIT preparation preserves ordinary filtering, refinement,
/// ordering of clauses, spill behavior and resource controls.
#[allow(clippy::too_many_arguments, reason = "row preparation retains the complete query context")]
fn prepare_select_rows<Read: QueryRead>(
	read: &Read,
	snapshot: Snapshot,
	plan: &LogicalSelect,
	physical: &crate::gql::optimizer::PhysicalSelectPlan,
	mut rows: RowSet,
	slots: &[Value],
	source: &str,
	control: &ExecutionControl,
	prepared_search: Option<&PreparedSearch>,
) -> FerrosGrynnResult<RowSet> {
	if let Some(predicate) = &plan.filter {
		if contains_search(predicate) {
			let prepared = prepared_search.ok_or_else(|| {
				FerrosGrynnError::invalid_argument(
					"search inputs were not prepared before source row pull",
				)
			})?;
			rows = prepare_search_rows(
				read, snapshot, rows, predicate, prepared, slots, source, control,
			)?;
		}
		let mut filtered = RowCollector::new(read, control, false);
		let spatial = physical.sources.iter().any(|source| {
			matches!(
				source.operator,
				crate::gql::optimizer::SourceOperator::GeometryIndexScan { .. }
			)
		});
		rows.consume(|row| {
			if spatial {
				control.record_spatial_refinement();
			}
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

	Ok(rows)
}
/// Ordinary SELECT and graph-wide LIVE aggregates share filtering/grouping.
#[allow(
	clippy::too_many_arguments,
	reason = "SELECT finishing retains the complete statement and prepared search context"
)]
fn finish_select_rows<Read: QueryRead>(
	read: &Read,
	snapshot: Snapshot,
	plan: &LogicalSelect,
	physical: &crate::gql::optimizer::PhysicalSelectPlan,
	rows: RowSet,
	slots: &[Value],
	source: &str,
	control: &ExecutionControl,
	prepared_search: Option<&PreparedSearch>,
	joined_sources: Option<&JoinedSources>,
) -> FerrosGrynnResult<SortedRecords> {
	let rows = prepare_select_rows(
		read,
		snapshot,
		plan,
		physical,
		rows,
		slots,
		source,
		control,
		prepared_search,
	)?;
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
				joined_sources,
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
				joined_sources,
			)?;
			ordinal += 1;
			output.push(output_row)
		})?;
	}
	output.finish()
}

/// Returns whether the select requires expanded atom row condition holds.
fn select_requires_expanded_atom_row(plan: &LogicalSelect) -> bool {
	let metadata = ["class", "kind", "label", "properties"]
		.into_iter()
		.filter_map(|name| plan.source.schema.index_of(name))
		.collect::<Vec<_>>();
	plan.projection.iter().any(|item| match item {
		LogicalSelectItem::Wildcard(fields) => {
			fields.iter().any(|field| matches!(field, LogicalWildcardField::Properties { .. }))
		}
		LogicalSelectItem::WildcardVariants {
			selector,
			choices,
		} => {
			choices
				.iter()
				.flatten()
				.any(|field| matches!(field, LogicalWildcardField::Properties { .. }))
				|| metadata.iter().any(|column| expression_uses_column(selector, *column))
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

/// Returns whether the expression uses column condition holds.
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
		TypedExprKind::Fulltext {
			left,
			right,
			..
		} => expression_uses_column(left, column) || expression_uses_column(right, column),
		TypedExprKind::Knn {
			left,
			k,
			ef,
			distance,
			right,
			..
		} => {
			expression_uses_column(left, column)
				|| expression_uses_column(k, column)
				|| ef.as_deref().is_some_and(|value| expression_uses_column(value, column))
				|| match distance {
					LogicalKnnDistance::Minkowski(value) | LogicalKnnDistance::Callable(value) => {
						expression_uses_column(value, column)
					}
					LogicalKnnDistance::Builtin(_) | LogicalKnnDistance::Named(_) => false,
				}
				|| expression_uses_column(right, column)
		}
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
		TypedExprKind::Invoke {
			callee,
			arguments,
		} => {
			expression_uses_column(callee, column)
				|| arguments.iter().any(|value| expression_uses_column(value, column))
		}
		TypedExprKind::NamedInvoke {
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
		TypedExprKind::Traversal(_) => false,
		TypedExprKind::Constant(_)
		| TypedExprKind::Slot(_)
		| TypedExprKind::LiveSlot(_)
		| TypedExprKind::CurrentField(_)
		| TypedExprKind::Closure(_) => false,
	}
}

#[allow(clippy::too_many_arguments, reason = "projection uses the complete statement context")]
/// Builds output row.
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
	joined_sources: Option<&JoinedSources>,
) -> FerrosGrynnResult<OutputRow> {
	let identity = if group.is_none() {
		joined_sources.map(|sources| sources.capture(read, &row)).transpose()?
	} else {
		None
	};
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
			let value = eval_select_expr(
				read,
				snapshot,
				&order.expression,
				&order_row,
				group,
				slots,
				source,
				control,
			)?;
			super::collation::order_key(value, order)
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
	if let Some(identity) = identity {
		if let Some(fields) = read.bounded_fields() {
			fields.admit(2 * std::mem::size_of::<Value>())?;
		}
		stable.try_reserve(1).map_err(|error| {
			FerrosGrynnError::traversal_resource_limit_exceeded(
				"LIVE JOIN output anchor",
				stable.len() as u64,
			)
			.with_technical_source(error)
		})?;
		stable.push(identity);
	}
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
/// Builds projected value.
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
	let (mut value, mut projected) =
		project(read, snapshot, plan, row, group, slots, source, control)?;
	for path in &plan.omit {
		if path.len() > control.max_control_depth as usize {
			return Err(FerrosGrynnError::control_limit_exceeded(
				"OMIT depth",
				u64::from(control.max_control_depth),
			));
		}
		omit_path(read, &mut value, path, control)?;
	}
	for (index, path) in plan.fetch.iter().enumerate() {
		if path.len() > control.max_control_depth as usize {
			return Err(FerrosGrynnError::control_limit_exceeded(
				"FETCH depth",
				u64::from(control.max_control_depth),
			));
		}
		let alias = plan
			.fetch_bridges
			.as_ref()
			.and_then(|bridges| bridges.get(index))
			.and_then(Option::as_deref);
		fetch_path(read, snapshot, &mut value, path, row.graph.as_ref(), alias, control)?;
	}
	if plan.fetch_bridges.is_some() {
		if plan.value {
			projected = vec![value.clone()];
		} else if let Value::Object(object) = &value {
			projected = plan
				.result
				.columns
				.iter()
				.map(|column| object.get(column.name.as_ref()).cloned().unwrap_or(Value::None))
				.collect();
		}
	}
	Ok((value, projected))
}

#[allow(clippy::too_many_arguments, reason = "direct projection uses the statement context")]
/// Builds output value.
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

/// Borrowed lexical values and resource authority for one SELECT source.
struct SourceEvaluation<'a> {
	/// Runtime values bound by the statement.
	slots: &'a [Value],
	/// Query text retained for expression diagnostics.
	source: &'a str,
	/// Existing cancellation, deadline and resource authority.
	control: &'a ExecutionControl,
	/// Existing outer row used by correlated expressions, when present.
	outer: Option<&'a Row>,
}

/// Returns the source rows result.
fn source_rows<Read: QueryRead>(
	read: &Read,
	snapshot: Snapshot,
	source_plan: &LogicalSelectSource,
	physical: Option<&crate::gql::optimizer::PhysicalSource>,
	spatial_filter: Option<&TypedExpr>,
	predicate: Option<&TypedExpr>,
	evaluation: SourceEvaluation<'_>,
	physical_search: Option<(&TypedExpr, &LogicalSelectAccess, &mut PreparedSearch)>,
	total_width: Option<usize>,
) -> FerrosGrynnResult<RowSet> {
	let SourceEvaluation {
		slots,
		source,
		control,
		outer,
	} = evaluation;

	let layout = physical
		.zip(total_width)
		.map(|(source, total)| (source.column_offset, source.column_width, total));
	match &source_plan.kind {
		LogicalSelectSourceKind::AtomUnion {
			targets,
			graph,
			tag_column,
			..
		} => {
			let fields = read.bounded_fields().ok_or_else(|| {
				FerrosGrynnError::invalid_argument("atom union requires bounded evaluation")
			})?;
			fields.work(targets.len() as u64)?;
			if *tag_column < 5
				|| tag_column.checked_add(1) != Some(source_plan.schema.columns.len())
				|| targets.windows(2).any(|pair| pair[0].class() >= pair[1].class())
				|| targets.iter().any(|target| !matches!(target, ScanTarget::Class(_)))
			{
				return Err(FerrosGrynnError::invalid_storage_file(
					"atom union source layout is invalid",
				));
			}
			let graph = graph
				.as_ref()
				.map(|reference| {
					resolve_graph_reference(
						read, snapshot, reference, outer, slots, source, control,
					)
				})
				.transpose()?;
			if let Some(graph) = &graph {
				read.graph(snapshot, graph)?;
			}
			let mut rows = RowCollector::new(read, control, false);
			for (tag, target) in targets.iter().enumerate() {
				let mut cursor = read.scan(
					snapshot,
					target.class(),
					graph.as_ref(),
					None,
					None,
					None,
					(slots, source),
					true,
				)?;
				let mut ordinal = 0_usize;
				while let Some(mut row) = cursor.next(control)? {
					fields.work(1)?;
					let extra = tag_column.checked_sub(row.values.len()).ok_or_else(|| {
						FerrosGrynnError::invalid_storage_file(
							"atom union row exceeds its class layout",
						)
					})?;
					fields.admit(
						extra
							.checked_add(4)
							.and_then(|count| count.checked_mul(2 * std::mem::size_of::<Value>()))
							.ok_or_else(|| {
								FerrosGrynnError::operation_overflow("atom union row padding")
							})?,
					)?;
					row.values.try_reserve_exact(extra + 1).map_err(|error| {
						FerrosGrynnError::traversal_resource_limit_exceeded(
							"atom union row padding",
							*tag_column as u64,
						)
						.with_technical_source(error)
					})?;
					row.values.resize(*tag_column, Value::None);
					row.values.push(Value::from(tag));
					rows.push(
						read,
						snapshot,
						scoped_layout_row(
							read,
							row,
							graph.as_ref(),
							Some(&[tag, ordinal]),
							layout,
						)?,
					)?;
					ordinal = ordinal.checked_add(1).ok_or_else(|| {
						FerrosGrynnError::operation_overflow("atom union row ordinal")
					})?;
				}
			}
			rows.finish()
		}
		LogicalSelectSourceKind::Empty {
			graph,
		} => {
			if let Some(reference) = graph {
				validate_empty_graph(read, snapshot, reference, outer, slots, source, control)?;
			}
			rows_to_set(read, snapshot, control, Vec::new())
		}
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
					if let Some(physical) = physical
						&& let crate::gql::optimizer::SourceOperator::GeometryIndexScan {
							index,
							column,
							forced,
							..
						} = physical.operator
						&& let Some(query) = spatial_filter
							.and_then(|filter| crate::gql::optimizer::spatial_query(filter, column))
					{
						let value = eval(query, None, slots, source, None, None)?;
						let mut memory = control.barrier_account();
						let mut admit = |bytes| {
							control.check()?;
							if memory.reserve(bytes) {
								Ok(())
							} else {
								Err(FerrosGrynnError::control_limit_exceeded(
									"geometry candidate memory",
									control.barrier_memory_limit() as u64,
								))
							}
						};
						let candidates = match &value {
							Value::None | Value::Null => {
								Some(crate::datastore::QueryGeometryIndexRead::default())
							}
							Value::Geometry(geometry) => match read.geometry_index(
								snapshot,
								class,
								graph.as_ref(),
								index,
								geometry,
								&mut admit,
							) {
								Ok(candidates) => candidates,
								Err(error) if forced => return Err(error),
								Err(error)
									if matches!(
										error.status().status(),
										crate::gql_status::Status::InvalidStorageFile { .. }
											| crate::gql_status::Status::InvalidStoragePage { .. }
									) =>
								{
									None
								}
								Err(error) => return Err(error),
							},
							_ => {
								return Err(FerrosGrynnError::invalid_argument(
									"spatial query operand must be geometry",
								));
							}
						};
						if let Some(candidates) = candidates {
							control.record_spatial_candidates(candidates.records.len());
							control.record_spatial_work(
								candidates.page_reads,
								candidates.delta_records,
							);
							let mut rows = RowCollector::new(read, control, false);
							for (ordinal, record) in candidates.records.into_iter().enumerate() {
								control.check()?;
								let row = read.get(snapshot, &record, graph.as_ref())?.ok_or_else(
									|| {
										FerrosGrynnError::invalid_storage_file(
											"geometry candidate is absent from its snapshot",
										)
									},
								)?;
								rows.push(
									read,
									snapshot,
									scoped_layout_row(
										read,
										row,
										graph.as_ref(),
										Some(&[ordinal]),
										layout,
									)?,
								)?;
							}
							return rows.finish();
						}
						control.record_spatial_fallback();
					}
					if let Some((filter, access, prepared)) = physical_search {
						let planned_index = physical.and_then(planned_search_index);
						if let Some(candidates) = try_physical_fulltext(
							read,
							snapshot,
							filter,
							access,
							planned_index,
							class,
							graph.as_ref(),
							prepared,
							control,
						)? {
							let mut rows = RowCollector::new(read, control, false);
							for (ordinal, (record, matched)) in candidates.into_iter().enumerate() {
								let mut row = read
									.get(snapshot, &record, graph.as_ref())?
									.ok_or_else(|| {
										FerrosGrynnError::invalid_storage_file(
											"Fulltext candidate is absent from its snapshot source",
										)
									})?;
								row.fulltext.push(matched);
								rows.push(
									read,
									snapshot,
									scoped_layout_row(
										read,
										row,
										graph.as_ref(),
										Some(&[ordinal]),
										layout,
									)?,
								)?;
							}
							return rows.finish();
						}
						if let Some(candidates) = try_physical_knn(
							read,
							snapshot,
							filter,
							access,
							planned_index,
							class,
							graph.as_ref(),
							prepared,
						)? {
							let mut rows = RowCollector::new(read, control, false);
							for (ordinal, (record, matched)) in candidates.into_iter().enumerate() {
								let mut row = read
									.get(snapshot, &record, graph.as_ref())?
									.ok_or_else(|| {
										FerrosGrynnError::invalid_storage_file(
											"ANN candidate is absent from its snapshot source",
										)
									})?;
								row.knn.push(matched);
								rows.push(
									read,
									snapshot,
									scoped_layout_row(
										read,
										row,
										graph.as_ref(),
										Some(&[ordinal]),
										layout,
									)?,
								)?;
							}
							return rows.finish();
						}
					}
					let mut cursor = read.scan(
						snapshot,
						class,
						graph.as_ref(),
						predicate,
						physical.and_then(planned_scalar_index),
						None,
						(slots, source),
						true,
					)?;
					let mut rows = RowCollector::new(read, control, false);
					let mut ordinal = 0_usize;
					loop {
						let _scope =
							read.bounded_fields().and_then(|fields| fields.transient_scope());
						let Some(row) = cursor.next(control)? else {
							break;
						};
						// Keep construction scratch through retained admission,
						// including the collector's spill transition.
						rows.push(
							read,
							snapshot,
							scoped_layout_row(read, row, graph.as_ref(), Some(&[ordinal]), layout)?,
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
						.map(|row| scoped_layout_row(read, row, graph.as_ref(), None, layout))
						.transpose()?
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
					.map(|row| layout_row(read, row, layout))
					.collect::<FerrosGrynnResult<Vec<_>>>()?,
			)
		}
		LogicalSelectSourceKind::Derived(select) => {
			let rows = derived_source_rows(read, snapshot, select, slots, source, control)?;
			if let Some(layout) = layout {
				map_row_set(read, snapshot, control, rows, |row| {
					layout_row(read, row, Some(layout))
				})
			} else {
				Ok(rows)
			}
		}
	}
}

/// Returns the logical source result.
fn logical_source(plan: &LogicalSelect, index: usize) -> &LogicalSelectSource {
	if index == 0 {
		&plan.source
	} else {
		&plan.joins[index - 1].source
	}
}

/// Checks the complete allocation and slice bounds before copying source cells.
fn admit_row_layout<Read: QueryRead>(
	read: &Read,
	length: usize,
	layout: Option<(usize, usize, usize)>,
) -> FerrosGrynnResult<()> {
	if let Some((offset, width, total)) = layout {
		if length != width || offset.checked_add(width).is_none_or(|end| end > total) {
			return Err(FerrosGrynnError::invalid_storage_file("source row layout is invalid"));
		}
		if offset == 0 && width == total {
			return Ok(());
		}
		if let Some(fields) = read.bounded_fields() {
			fields.work(total as u64)?;
			fields.admit(
				total
					.checked_mul(2 * std::mem::size_of::<Value>())
					.ok_or_else(|| FerrosGrynnError::operation_overflow("source row layout"))?,
			)?;
		}
	}
	Ok(())
}

/// Scoped scans share preflight for graph names, sequence and padded layout.
fn scoped_layout_row<Read: QueryRead>(
	read: &Read,
	mut row: Row,
	graph: Option<&GraphRef>,
	sequence: Option<&[usize]>,
	layout: Option<(usize, usize, usize)>,
) -> FerrosGrynnResult<Row> {
	admit_row_layout(read, row.values.len(), layout)?;
	if let Some(fields) = read.bounded_fields() {
		let graph_bytes = graph
			.map(|graph| graph.name().len().checked_add(std::mem::size_of::<GraphRef>()))
			.unwrap_or(Some(0))
			.ok_or_else(|| FerrosGrynnError::operation_overflow("source graph metadata"))?;
		let bytes = sequence
			.map_or(0, <[usize]>::len)
			.checked_mul(2 * std::mem::size_of::<usize>())
			.and_then(|bytes| bytes.checked_add(graph_bytes))
			.ok_or_else(|| FerrosGrynnError::operation_overflow("source scope metadata"))?;
		fields.admit(bytes)?;
	}
	row = row.with_graph(graph.cloned());
	if let Some(sequence) = sequence {
		row = row.with_sequence(sequence.to_vec());
	}
	Ok(apply_row_layout(row, layout))
}

/// Unscoped/derived inputs use the same checked allocation owner.
fn layout_row<Read: QueryRead>(
	read: &Read,
	row: Row,
	layout: Option<(usize, usize, usize)>,
) -> FerrosGrynnResult<Row> {
	admit_row_layout(read, row.values.len(), layout)?;
	Ok(apply_row_layout(row, layout))
}

/// Applies a pre-admitted layout; every caller validates its exact bounds.
fn apply_row_layout(mut row: Row, layout: Option<(usize, usize, usize)>) -> Row {
	let Some((offset, width, total)) = layout else {
		return row;
	};
	if offset == 0 && width == total {
		return row;
	}
	let mut values = vec![Value::None; total];
	for (target, value) in
		values[offset..offset.saturating_add(width)].iter_mut().zip(row.values.drain(..))
	{
		*target = value;
	}
	row.values = values;
	row
}

/// Returns the map row set result.
fn map_row_set<Read: QueryRead>(
	read: &Read,
	snapshot: Snapshot,
	control: &ExecutionControl,
	rows: RowSet,
	mut map: impl FnMut(Row) -> FerrosGrynnResult<Row>,
) -> FerrosGrynnResult<RowSet> {
	let mut output = RowCollector::new(read, control, false);
	rows.consume(|row| output.push(read, snapshot, map(row)?))?;
	output.finish()
}

/// Returns the planned scalar index result.
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

/// Returns the planned search index result.
fn planned_search_index(
	source: &crate::gql::optimizer::PhysicalSource,
) -> Option<crate::datastore::IndexId> {
	match source.operator {
		crate::gql::optimizer::SourceOperator::FulltextIndexScan {
			index,
			..
		}
		| crate::gql::optimizer::SourceOperator::HnswSearch {
			index,
			..
		}
		| crate::gql::optimizer::SourceOperator::DiskAnnSearch {
			index,
			..
		} => Some(index),
		_ => None,
	}
}

/// Returns the derived source rows result.
fn derived_source_rows<Read: QueryRead>(
	read: &Read,
	snapshot: Snapshot,
	select: &LogicalSelect,
	slots: &[Value],
	source: &str,
	control: &ExecutionControl,
) -> FerrosGrynnResult<RowSet> {
	if select.return_graph.is_some() {
		let value = graph_result::execute_select_graph(
			read, snapshot, select, slots, source, control, None, false,
		)?;
		let mut rows = RowCollector::new(read, control, false);
		rows.push(
			read,
			snapshot,
			Row::new(vec![value]).with_stable(Vec::new()).with_sequence(vec![0]),
		)?;
		return rows.finish();
	}
	let (select, physical) = prepare_physical_plan(read, snapshot, select, control)?;
	let select = &select;
	let output = execute_select_output(
		read, snapshot, select, &physical, slots, source, control, None, None,
	)?;
	let fields = RecordFields {
		read,
		snapshot,
		graph: None,
	};
	let start = match &select.start {
		None => 0,
		Some(LogicalSelectStart::Offset(value)) => {
			eval_usize(value, slots, source, &fields, control)?
		}
		Some(LogicalSelectStart::After {
			..
		}) => {
			return Err(FerrosGrynnError::query_feature_not_supported(
				"START AFTER inside a derived SELECT source",
			));
		}
	};
	let mut remaining =
		select_limit(select, slots, source, &fields, control)?.unwrap_or(usize::MAX);
	remaining = select_pull_limit(select.only, remaining);
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
	check_select_cardinality(select.only, ordinal)?;
	rows.finish()
}

/// Returns the rows to set result.
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

/// A zero result window still observes and validates its graph scope.
fn validate_zero_window<Read: QueryRead>(
	read: &Read,
	snapshot: Snapshot,
	plan: &LogicalSelect,
	slots: &[Value],
	source: &str,
	control: &ExecutionControl,
	outer: Option<&Row>,
) -> FerrosGrynnResult<()> {
	control.check()?;
	if let Some(reference) = plan.source.kind.graph_scope() {
		validate_empty_graph(read, snapshot, reference, outer, slots, source, control)?;
	}
	Ok(())
}

/// Validates an empty scoped relation without materializing graph rows.
///
/// # Errors
///
/// Returns scope evaluation, snapshot, graph identity or budget errors.
fn validate_empty_graph<Read: QueryRead>(
	read: &Read,
	snapshot: Snapshot,
	reference: &LogicalGraphReference,
	outer: Option<&Row>,
	slots: &[Value],
	source: &str,
	control: &ExecutionControl,
) -> FerrosGrynnResult<()> {
	let graph = resolve_graph_reference(read, snapshot, reference, outer, slots, source, control)?;
	read.graph(snapshot, &graph)?;
	Ok(())
}

/// Resolves and refreshes a root scope before collecting any LIVE rows.
///
/// # Errors
///
/// Returns bounded expression, graph lookup, snapshot or interruption errors.
pub(in crate::gql) fn resolve_root_graph_reference(
	read: &RootRead<'_>,
	reference: &LogicalGraphReference,
	slots: &[Value],
	control: &ExecutionControl,
	anchor: Option<(crate::graph::GraphId, &str)>,
) -> FerrosGrynnResult<(GraphRef, Option<Text>)> {
	let snapshot = read.snapshot()?;
	let value = match reference {
		LogicalGraphReference::Static(reference) => {
			read.fields().admit(
				reference
					.name()
					.len()
					.checked_add(std::mem::size_of::<GraphRef>() + std::mem::size_of::<Value>())
					.ok_or_else(|| FerrosGrynnError::operation_overflow("LIVE scope reference"))?,
			)?;
			Value::from(reference.clone())
		}
		LogicalGraphReference::Expression(expression) => eval_select_expr(
			read,
			snapshot,
			expression,
			&Row::new(Vec::new()),
			None,
			slots,
			"",
			control,
		)?,
	};
	read.fields().admit_value(&value)?;
	let graph = match &value {
		Value::GraphRef(reference) => reference.id(),
		Value::String(name) => match anchor {
			// The durable last-successful operand keeps its identity through
			// renames and rejects removal/recreate, including quiet intervals.
			Some((graph, previous)) if previous == name.as_str() => graph,
			_ => read.catalog(snapshot).graph(name.as_str())?.id(),
		},
		value => {
			return Err(FerrosGrynnError::invalid_argument(format!(
				"GRAPH reference requires graph_ref or string, found {}.",
				value.kind()
			)));
		}
	};
	let name = match value {
		Value::String(name) => Some(name),
		_ => None,
	};
	Ok((read.graph_reference(snapshot, graph)?, name))
}

/// Resolves graph reference.
pub(in crate::gql::execution) fn resolve_graph_reference<Read: QueryRead>(
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
		LogicalGraphReference::Expression(expression) => resolve_graph_value(
			eval_select_expr(
				read,
				snapshot,
				expression,
				row.unwrap_or(&Row::new(Vec::new())),
				None,
				slots,
				source,
				control,
			)?,
			read.catalog(snapshot),
		),
	}
}

/// Resolves an already evaluated scope through the caller's fixed catalog.
///
/// # Errors
///
/// Returns missing graph names or the ordinary GRAPH operand type error.
pub(in crate::gql::execution) fn resolve_graph_value(
	value: Value,
	catalog: &dyn QueryCatalog,
) -> FerrosGrynnResult<GraphRef> {
	match value {
		Value::GraphRef(reference) => Ok(*reference),
		Value::String(name) => catalog.graph(name.as_str()),
		value => Err(FerrosGrynnError::invalid_argument(format!(
			"GRAPH reference requires graph_ref or string, found {}.",
			value.kind()
		))),
	}
}

/// Splits rows.
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
			control.check()?;
			if let Some(fields) = read.bounded_fields() {
				fields.work(1)?;
				let growth = row
					.stable
					.len()
					.checked_add(1)
					.and_then(|count| count.checked_mul(std::mem::size_of::<Value>()))
					.and_then(|bytes| {
						row.sequence
							.len()
							.checked_add(1)
							.and_then(|count| count.checked_mul(std::mem::size_of::<usize>()))
							.and_then(|sequence| bytes.checked_add(sequence))
					})
					.ok_or_else(|| FerrosGrynnError::operation_overflow("SPLIT row growth"))?;
				fields.admit(
					estimated_row_bytes(&row)
						.checked_add(growth)
						.ok_or_else(|| FerrosGrynnError::operation_overflow("SPLIT row copy"))?,
				)?;
				for value in row.values.iter().chain(&row.stable).chain(&row.order) {
					fields.admit_value(value)?;
				}
			}
			let mut expanded = row.clone();
			expanded.stable.try_reserve_exact(1).map_err(|error| {
				FerrosGrynnError::traversal_resource_limit_exceeded("SPLIT stable growth", 1)
					.with_technical_source(error)
			})?;
			expanded.sequence.try_reserve_exact(1).map_err(|error| {
				FerrosGrynnError::traversal_resource_limit_exceeded("SPLIT sequence growth", 1)
					.with_technical_source(error)
			})?;
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

/// Splits binding.
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

/// Sets split path.
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

/// Returns the for each group result.
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
	);
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
	consume_records(sorted, |mut record| {
		let key = std::mem::take(&mut record.anchor.order_keys);
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

/// Projects the current state.
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
	for (index, item) in plan.projection.iter().enumerate() {
		match item {
			LogicalSelectItem::Wildcard(fields) => {
				project_wildcard_fields(row, fields, &mut object, &mut projected);
			}
			LogicalSelectItem::WildcardVariants {
				selector,
				choices,
			} => {
				let ordinal =
					eval_select_expr(read, snapshot, selector, row, group, slots, source, control)?;
				let Value::Number(ordinal) = ordinal else {
					return Err(FerrosGrynnError::invalid_storage_file(
						"JOIN wildcard selector is not numeric",
					));
				};
				let ordinal = usize::try_from(u64::try_from(ordinal)?).map_err(|_error| {
					FerrosGrynnError::operation_overflow("JOIN wildcard ordinal")
				})?;
				let fields = choices.get(ordinal).ok_or_else(|| {
					FerrosGrynnError::invalid_storage_file("JOIN wildcard ordinal is out of range")
				})?;
				project_wildcard_fields(row, fields, &mut object, &mut projected);
			}
			LogicalSelectItem::Expression {
				name,
				expression,
			} => {
				let value = eval_select_expr(
					read, snapshot, expression, row, group, slots, source, control,
				)?;
				projected.push(value.clone());
				if let Some(Some(path)) = plan.projection_paths.get(index) {
					insert_projection_path(&mut object, path, value, read.bounded_fields())?;
				} else {
					object.insert(name.as_ref(), value);
				}
			}
		}
	}
	if plan.value {
		Ok((projected.first().cloned().unwrap_or(Value::None), projected))
	} else {
		Ok((Value::Object(object), projected))
	}
}

/// Shared schemafull columns and schemaless property flattening.
fn project_wildcard_fields(
	row: &Row,
	fields: &[LogicalWildcardField],
	object: &mut Object,
	projected: &mut Vec<Value>,
) {
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

/// Shapes checked SDK aliases after expression evaluation and before FETCH.
fn insert_projection_path(
	object: &mut Object,
	path: &[Box<str>],
	value: Value,
	fields: Option<&dyn AtomFieldRead>,
) -> FerrosGrynnResult<()> {
	let Some((head, tail)) = path.split_first() else {
		return Err(FerrosGrynnError::invalid_argument("SELECT output path is empty"));
	};
	if let Some(fields) = fields {
		fields.work(1)?;
		fields.admit(head.len() + 128)?;
	}
	if tail.is_empty() {
		if object.get(head.as_ref()).is_some() {
			return Err(FerrosGrynnError::invalid_argument("SELECT output paths overlap"));
		}
		object.insert(head.as_ref(), value);
		return Ok(());
	}
	if object.get(head.as_ref()).is_none() {
		object.insert(head.as_ref(), Value::Object(Object::new()));
	}
	let Some(Value::Object(nested)) = object.get_mut(head.as_ref()) else {
		return Err(FerrosGrynnError::invalid_argument("SELECT output paths overlap"));
	};
	insert_projection_path(nested, tail, value, fields)
}

/// Removes literal object paths with the shared cancellation and work budget.
fn omit_path<Read: QueryRead>(
	read: &Read,
	value: &mut Value,
	path: &[Box<str>],
	control: &ExecutionControl,
) -> FerrosGrynnResult<()> {
	control.check()?;
	if let Some(fields) = read.bounded_fields() {
		fields.work(1)?;
	}
	let Some((head, tail)) = path.split_first() else {
		return Ok(());
	};
	let Value::Object(object) = value else {
		return Ok(());
	};
	if tail.is_empty() {
		object.remove(head);
	} else if let Some(value) = object.get_mut(head) {
		omit_path(read, value, tail, control)?;
	}
	Ok(())
}

/// Returns the fetch path result.
fn fetch_path<Read: QueryRead>(
	read: &Read,
	snapshot: Snapshot,
	value: &mut Value,
	path: &[Box<str>],
	graph: Option<&GraphRef>,
	bridge: Option<&str>,
	control: &ExecutionControl,
) -> FerrosGrynnResult<()> {
	control.check()?;
	if let Some(fields) = read.bounded_fields() {
		fields.work(1)?;
	}
	if let Value::Array(values) = value {
		for value in values {
			fetch_path(read, snapshot, value, path, graph, bridge, control)?;
		}
		return Ok(());
	}
	if let Value::Set(values) = value {
		let mut fetched = crate::values::Set::new();
		for mut value in std::mem::take(values) {
			fetch_path(read, snapshot, &mut value, path, graph, bridge, control)?;
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
	let owned = match read.bounded_fields() {
		Some(fields) => fields.copy_value(target)?,
		None => target.clone(),
	};
	*target = fetch_value(read, snapshot, owned, graph, bridge, control)?;
	if !tail.is_empty() {
		fetch_path(read, snapshot, target, tail, graph, bridge, control)?;
	}
	Ok(())
}

/// Returns the fetch value result.
fn fetch_value<Read: QueryRead>(
	read: &Read,
	snapshot: Snapshot,
	value: Value,
	graph: Option<&GraphRef>,
	bridge: Option<&str>,
	control: &ExecutionControl,
) -> FerrosGrynnResult<Value> {
	control.check()?;
	if matches!(&value, Value::Array(_) | Value::Set(_))
		&& let Some(alias) = bridge
	{
		super::bridge::prefetch(read, snapshot, &value, graph, alias, control)?;
	}
	if let Some(fields) = read.bounded_fields() {
		fields.work(1)?;
		let count = match &value {
			Value::Array(values) => values.len(),
			Value::Set(values) => values.len(),
			_ => 0,
		};
		fields.admit(
			count
				.checked_mul(2 * std::mem::size_of::<Value>() + 128)
				.ok_or_else(|| FerrosGrynnError::operation_overflow("LIVE FETCH collection"))?,
		)?;
	}
	match value {
		Value::Record(record) => {
			let row = match graph {
				Some(graph) => match read.get(snapshot, &record, Some(graph))? {
					Some(row) => Some(row),
					None => read.get(snapshot, &record, None)?,
				},
				None => read.get(snapshot, &record, None)?,
			};
			let Some(row) = row else {
				return Ok(Value::None);
			};
			let schema = read.catalog(snapshot).schema(record.class())?;
			let object: Object = schema
				.columns
				.iter()
				.zip(row.values)
				.map(|(column, value)| (column.name.as_ref(), value))
				.collect();
			match bridge {
				Some(alias) => {
					super::bridge::fetch(read, snapshot, &record, graph, alias, object, control)
				}
				None => Ok(Value::Object(object)),
			}
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
			.map(|value| fetch_value(read, snapshot, value, graph, bridge, control))
			.collect::<FerrosGrynnResult<Array>>()
			.map(Value::Array),
		Value::Set(values) => values
			.into_iter()
			.map(|value| fetch_value(read, snapshot, value, graph, bridge, control))
			.collect::<FerrosGrynnResult<crate::values::Set>>()
			.map(Value::Set),
		Value::Object(object) if bridge.is_some() => {
			if let Some(Value::Record(record)) = object.get("id") {
				let record = record.as_ref().clone();
				super::bridge::fetch(
					read,
					snapshot,
					&record,
					graph,
					bridge.ok_or_else(|| {
						FerrosGrynnError::bridge_operation_error("bridge alias is absent")
					})?,
					object,
					control,
				)
			} else {
				Ok(Value::Object(object))
			}
		}
		value => Ok(value),
	}
}

#[cfg(test)]
/// Regression tests for select behavior.
mod omit_tests {
	use std::cell::Cell;

	use super::*;

	/// Failed work or cancellation stops a nested path before removal.
	#[test]
	fn live_omit_traversal_checks_work_and_cancellation() {
		let store = crate::datastore::Datastore::in_memory();
		let root = store.inner.current_root();
		let path = vec![Box::<str>::from("nested"), Box::<str>::from("secret")];
		let original = Value::Object(Object::from_iter([(
			"nested",
			Value::Object(Object::from_iter([("secret", Value::from(1))])),
		)]));
		for cancellation in [false, true] {
			let token = crate::gql::QueryCancellation::default();
			let control = ExecutionControl::new(
				&crate::gql::QueryExecutionOptions {
					cancellation: Some(token.clone()),
					..crate::gql::QueryExecutionOptions::default()
				},
				None,
			);
			let calls = Cell::new(0);
			let work = |_amount| {
				calls.set(calls.get() + 1);
				if cancellation {
					token.cancel();
					Ok(())
				} else if calls.get() > 1 {
					Err(FerrosGrynnError::traversal_resource_limit_exceeded("OMIT test work", 1))
				} else {
					Ok(())
				}
			};
			let read = RootRead::new(&root, &control, &|_| {}, &work, std::env::temp_dir());
			let mut value = original.clone();
			assert!(omit_path(&read, &mut value, &path, &control).is_err());
			assert_eq!(value, original);
			assert_eq!(
				calls.get(),
				if cancellation {
					1
				} else {
					2
				}
			);
		}
	}
}
