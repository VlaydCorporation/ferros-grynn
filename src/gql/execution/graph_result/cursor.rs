//! Exact graph pages keep their root and evaluated frame in the existing cursor
//! spool.

use std::sync::Arc;

use super::{
	ExecutionControl,
	FerrosGrynnError,
	FerrosGrynnResult,
	LogicalSelect,
	QueryRead,
	RecordFields,
	RootRead,
	Snapshot,
	Value,
	eval,
	eval_usize,
	execute_select_graph_from_root,
	invalid,
	select,
};
use crate::gql::{cursor::GraphCursorContext, ir::LogicalSelectStart, optimizer::fingerprint};

/// Captures only a successfully materialized direct page with a continuation.
pub(super) fn capture<Read: QueryRead>(
	read: &Read,
	snapshot: Snapshot,
	template: &LogicalSelect,
	bound: &LogicalSelect,
	slots: &[Value],
	source: &str,
	control: &ExecutionControl,
) -> FerrosGrynnResult<()> {
	let Some(token) = control.continuation() else {
		return Ok(());
	};
	let Some(runtime) = control.cursors() else {
		return Ok(());
	};
	if runtime.lock().graph_context(token)?.is_some() {
		return Ok(());
	}
	let root = read.snapshot_root(snapshot)?;
	let work = |amount| {
		read.bounded_fields().map_or_else(|| control.check(), |fields| fields.work(amount))
	};
	let observe = |_key| {};
	let capture_read = RootRead::new(&root, control, &observe, &work, read.query_scratch_root());
	let before = control.admitted_memory();
	select::admit_graph_cursor_frame(&capture_read, bound, slots, source)?;
	let bytes = control
		.admitted_memory()
		.checked_sub(before)
		.and_then(|bytes| bytes.checked_add(std::mem::size_of::<GraphCursorContext>()))
		.ok_or_else(|| invalid("graph cursor frame size overflows"))?;
	let mut account = control.barrier_account();
	if !account.reserve(bytes) {
		return Err(FerrosGrynnError::traversal_resource_limit_exceeded(
			"graph cursor frame",
			control.barrier_memory_limit() as u64,
		));
	}
	drop(capture_read);
	let context = Arc::new(GraphCursorContext {
		root,
		plan: bound.clone(),
		slots: slots.to_vec(),
		source: source.to_owned(),
		template: fingerprint::cursor_fingerprints(template, source),
		account,
	});
	runtime.lock().attach_graph_context(token, context)
}

/// Exact resume uses original selectors/data; READ COMMITTED follows the
/// caller's new root.
pub(super) fn resume<Read: QueryRead>(
	read: &Read,
	snapshot: Snapshot,
	plan: &LogicalSelect,
	slots: &[Value],
	source: &str,
	control: &ExecutionControl,
) -> FerrosGrynnResult<Option<Value>> {
	let Some(LogicalSelectStart::After {
		cursor,
		read_committed: false,
	}) = &plan.start
	else {
		return Ok(None);
	};
	let Some(runtime) = control.cursors() else {
		return Ok(None);
	};
	let fields = RecordFields {
		read,
		snapshot,
		graph: None,
	};
	let value = eval(cursor, None, slots, source, Some(&fields), Some(control))?;
	let Value::Cursor(token) = value else {
		return Err(FerrosGrynnError::invalid_query_cursor("graph continuation requires a cursor"));
	};
	let Some(context) = runtime.lock().graph_context(token)? else {
		return Ok(None);
	};
	if fingerprint::cursor_fingerprints(plan, source) != context.template {
		return Err(FerrosGrynnError::invalid_query_cursor(
			"graph query or ORDER fingerprint does not match",
		));
	}
	let limit = plan
		.limit
		.as_ref()
		.ok_or_else(|| FerrosGrynnError::invalid_argument("START AFTER requires LIMIT"))?;
	if let Err(error) = control.check() {
		control.discard_cursor(token);
		return Err(error);
	}
	let page_size = match eval_usize(limit, slots, source, &fields, control) {
		Ok(size) => size,
		Err(error) => {
			control.discard_cursor(token);
			return Err(error);
		}
	};
	let work = |amount| {
		read.bounded_fields().map_or_else(|| control.check(), |fields| fields.work(amount))
	};
	let observe = |_key| {};
	let old_read =
		RootRead::new(&context.root, control, &observe, &work, read.query_scratch_root());
	// The existing spool owns projected rows and any physical index checkout.
	let identity = runtime.lock().graph_page_fingerprints(token)?;
	let values = select::resume_retained_page(control, token, identity, page_size)?;
	select::check_select_cardinality(context.plan.only, values.len())?;
	execute_select_graph_from_root(
		&old_read,
		old_read.snapshot()?,
		&context.plan,
		&context.slots,
		&context.source,
		control,
		None,
		false,
		Some(values),
	)
	.map(Some)
}
