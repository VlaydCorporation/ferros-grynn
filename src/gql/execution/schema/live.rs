//! LIVE registration uses the normal SDK and captures current lexical slots.

use crate::{
	changefeed::ChangefeedSource,
	datastore::Transaction,
	gql::{
		ExecutionControl,
		execution::eval::eval,
		ir::{LogicalLiveBinding, LogicalLiveCapture, LogicalLiveSelect, TypedExpr},
	},
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	live::{
		LiveBindingId,
		LiveBindingOptions,
		LiveExpr,
		LiveId,
		LiveOptions,
		LiveProjection,
		LiveQuery,
		LiveSince,
		LiveState,
	},
	values::{Object, Value},
};

/// Decodes the complete token, then stages only a session-issued delivery.
pub(super) fn ack(
	transaction: &mut Transaction,
	snapshot: &crate::datastore::QuerySnapshot,
	id: &TypedExpr,
	cursor: &TypedExpr,
	slots: &[Value],
	source: &str,
	control: &ExecutionControl,
) -> FerrosGrynnResult<Value> {
	let id = identity(id, slots, source, control)?;
	let authority = control.live_attachment(id)?;
	let cursor = if let crate::gql::ir::TypedExprKind::Slot(slot) = cursor.kind {
		let Some(Value::Bytes(bytes)) = slots.get(slot as usize) else {
			return Err(FerrosGrynnError::invalid_argument("ACK LIVE CURSOR requires Bytes"));
		};
		crate::live::LiveCursor::decode(bytes.as_slice())?
	} else {
		let Value::Bytes(bytes) = operand(transaction, snapshot, cursor, slots, control)? else {
			return Err(FerrosGrynnError::invalid_argument("ACK LIVE CURSOR requires Bytes"));
		};
		crate::live::LiveCursor::decode(bytes.as_slice())?
	};
	if cursor.live_id() != id {
		return Err(FerrosGrynnError::invalid_argument("ACK LIVE cursor/query mismatch"));
	}
	transaction.acknowledge_gql_live(authority, cursor)?;
	Ok(Value::None)
}

/// Command inputs share the fixed-root evaluator, admission and ordinary
/// dependency recorder instead of opening another transaction.
fn operand(
	transaction: &Transaction,
	snapshot: &crate::datastore::QuerySnapshot,
	expression: &TypedExpr,
	slots: &[Value],
	control: &ExecutionControl,
) -> FerrosGrynnResult<Value> {
	with_command_read(transaction, snapshot, control, |read| {
		crate::gql::execution::evaluate_root_expression(read, expression, slots, control)
	})
}

/// Both expression and named-scope resolution share one bounded root read.
///
/// # Errors
///
/// Returns snapshot, interruption, resource or callback errors.
fn with_command_read<Output>(
	transaction: &Transaction,
	snapshot: &crate::datastore::QuerySnapshot,
	control: &ExecutionControl,
	evaluate: impl FnOnce(&crate::gql::execution::RootRead<'_>) -> FerrosGrynnResult<Output>,
) -> FerrosGrynnResult<Output> {
	let work = std::cell::Cell::new(0_u64);
	let observe = |key| transaction.observe_query_dependency(key);
	let admit_work = |amount: u64| {
		control.check()?;
		let next = work
			.get()
			.checked_add(amount)
			.ok_or_else(|| FerrosGrynnError::operation_overflow("LIVE command expression work"))?;
		if next > control.max_loop_iterations {
			return Err(FerrosGrynnError::traversal_resource_limit_exceeded(
				"LIVE command expression work",
				control.max_loop_iterations,
			));
		}
		work.set(next);
		Ok(())
	};
	let read = crate::gql::execution::RootRead::new(
		snapshot.logical_root(),
		control,
		&observe,
		&admit_work,
		transaction.query_scratch_root(),
	);
	evaluate(&read)
}

/// Resolves an owned query identity through ordinary typed slot evaluation.
pub(super) fn identity(
	expression: &TypedExpr,
	slots: &[Value],
	source: &str,
	control: &ExecutionControl,
) -> FerrosGrynnResult<LiveId> {
	if let crate::gql::ir::TypedExprKind::Slot(slot) = expression.kind {
		control.check()?;
		return match slots.get(slot as usize) {
			Some(Value::Uuid(uuid)) => LiveId::from_uuid(*uuid),
			_ => Err(FerrosGrynnError::invalid_argument("LIVE identity requires a UUID")),
		};
	}
	let Value::Uuid(uuid) = eval(expression, None, slots, source, None, Some(control))? else {
		return Err(FerrosGrynnError::invalid_argument("LIVE identity requires a UUID"));
	};
	LiveId::from_uuid(uuid)
}

/// Owns a bounded metadata row without attaching a consumer or pulling rows.
pub(super) fn info(
	transaction: &Transaction,
	snapshot: &crate::datastore::QuerySnapshot,
	expression: &TypedExpr,
	slots: &[Value],
	control: &ExecutionControl,
) -> FerrosGrynnResult<Value> {
	let Value::Uuid(uuid) = operand(transaction, snapshot, expression, slots, control)? else {
		return Err(FerrosGrynnError::invalid_argument("LIVE identity requires a UUID"));
	};
	let id = LiveId::from_uuid(uuid)?;
	let mut account = control.barrier_account();
	let resource = || {
		FerrosGrynnError::traversal_resource_limit_exceeded(
			"LIVE metadata output",
			control.barrier_memory_limit() as u64,
		)
	};
	if !account.reserve(4096) {
		return Err(resource());
	}
	let metadata_bytes = control.barrier_memory_available() / 2;
	if metadata_bytes == 0 || !account.reserve(metadata_bytes) {
		return Err(resource());
	}
	let info = transaction.query_live_info(snapshot, id, metadata_bytes)?;
	let row_bytes = crate::memory::tree_allocation_bytes(
		5,
		std::mem::size_of::<(crate::values::Text, Value)>(),
	)?;
	let root_bytes = crate::memory::tree_allocation_bytes(
		24,
		std::mem::size_of::<(crate::values::Text, Value)>(),
	)?;
	let count = info
		.bindings()
		.len()
		.checked_add(info.function_dependencies().len())
		.and_then(|count| count.checked_add(info.record_sources().len()))
		.and_then(|count| {
			count.checked_add(
				info.history().map_or(0, |history| history.epochs().len()).checked_mul(2)?,
			)
		})
		.ok_or_else(resource)?;
	let bytes = count
		.checked_mul(row_bytes.checked_add(std::mem::size_of::<Value>()).ok_or_else(resource)?)
		.and_then(|bytes| bytes.checked_add(row_bytes.checked_mul(2)?)?.checked_add(root_bytes))
		.ok_or_else(resource)?;
	if !account.reserve(bytes) {
		return Err(resource());
	}
	for (name, _) in info.record_sources() {
		if !account.reserve(name.len()) {
			return Err(resource());
		}
	}
	let state = match info.state() {
		LiveState::Pending => "pending",
		LiveState::Active => "active",
		LiveState::Paused => "paused",
		LiveState::Closed => "closed",
	};
	let bindings = info
		.bindings()
		.iter()
		.map(|revision| {
			Value::Object(Object::from_iter([
				("id", Value::from(revision.id().as_uuid())),
				("definition", Value::from(revision.definition().get())),
				("value", Value::from(revision.value().get())),
			]))
		})
		.collect::<Vec<_>>();
	let mut row = Object::from_iter([
		("id", Value::from(info.id().as_uuid())),
		("version", Value::from(info.version().get())),
		("state", Value::from(state)),
		("defined_at", Value::from(info.defined_at().get())),
		("published_at", Value::from(info.published_at().get())),
		("snapshot", Value::from(info.snapshot().get())),
		("starting_after", Value::from(info.starting_after().get())),
		("read_strategy", Value::from(info.read_strategy())),
		("acknowledged", Value::from(info.cursor().versionstamp().get())),
		("cursor", Value::from(info.cursor().encode())),
		("bindings", Value::from(bindings)),
	]);
	let functions = info
		.function_dependencies()
		.iter()
		.map(|(id, version)| {
			Value::Object(Object::from_iter([
				("id", Value::from(id.get())),
				("version", Value::from(version.get())),
			]))
		})
		.collect::<Vec<_>>();
	row.insert("functions", Value::from(functions));
	let owners = info
		.record_sources()
		.iter()
		.map(|(name, class)| {
			Value::Object(Object::from_iter([
				("name", Value::from(name.clone())),
				("class", Value::from(class.get())),
			]))
		})
		.collect::<Vec<_>>();
	row.insert("record_sources", Value::from(owners));
	let source = match info.capture_source() {
		ChangefeedSource::Class(class) => Value::Object(Object::from_iter([
			("kind", Value::from("class")),
			("id", Value::from(class.get())),
		])),
		ChangefeedSource::Graph(graph) => Value::Object(Object::from_iter([
			("kind", Value::from("graph")),
			("id", Value::from(graph.get())),
		])),
	};
	row.insert("capture_source", source);
	row.insert(
		"source_class",
		info.source_class().map_or(Value::None, |class| Value::from(class.get())),
	);
	let history = info.history().map_or(Value::None, |history| {
		let epochs = history
			.epochs()
			.iter()
			.map(|epoch| {
				let policy = epoch.policy().map_or(Value::None, |policy| {
					Value::Object(Object::from_iter([
						("source", Value::from(policy.source().as_uuid())),
						("revision", Value::from(policy.revision().get())),
						("retention", Value::Duration(policy.retention())),
					]))
				});
				Value::Object(Object::from_iter([
					("started_at", Value::from(epoch.started_at().get())),
					("policy", policy),
				]))
			})
			.collect::<Vec<_>>();
		Value::Object(Object::from_iter([
			("last_stamp", Value::from(history.last_stamp().get())),
			("purged_through", Value::from(history.purged_through().get())),
			(
				"purged_at",
				history.purged_at().map_or(Value::None, |time| Value::DateTime(time.clone())),
			),
			("epochs", Value::from(epochs)),
		]))
	});
	row.insert("history", history);
	row.insert("queue_entries", info.options().queue_entries() as u64);
	row.insert("queue_bytes", info.options().queue_bytes() as u64);
	row.insert("scratch_bytes", info.options().scratch_bytes() as u64);
	row.insert("work", info.options().work());
	row.insert(
		"diagnostic",
		info.diagnostic()
			.map_or(Value::None, |error| Value::from(error.status().code().to_string())),
	);
	control.check()?;
	Ok(Value::Object(row))
}

/// Freezes ordinary LET/execute values at the registration boundary.
pub(super) fn register(
	transaction: &mut Transaction,
	snapshot: &crate::datastore::QuerySnapshot,
	plan: &LogicalLiveSelect,
	slots: &[Value],
	control: &ExecutionControl,
) -> FerrosGrynnResult<Value> {
	control.check()?;
	let mut account = control.barrier_account();
	if !account.reserve(plan.retained_bytes) {
		return Err(FerrosGrynnError::traversal_resource_limit_exceeded(
			"LIVE command recipe",
			control.barrier_memory_limit() as u64,
		));
	}
	let parameters = capture_parameters(&plan.captures, slots, control)?;
	let mut query = plan.query.clone();
	if plan.frozen_graphs.windows(2).any(|pair| pair[0].source >= pair[1].source)
		|| plan.frozen_graphs.iter().any(|capture| capture.source as usize > query.joins.len())
	{
		return Err(FerrosGrynnError::invalid_argument(
			"frozen LIVE graph capture has an invalid source declaration",
		));
	}
	for capture in &plan.frozen_graphs {
		control.admit_reactive_work(1)?;
		let graph = with_command_read(transaction, snapshot, control, |read| {
			let value = crate::gql::execution::evaluate_root_expression(
				read,
				&capture.graph,
				slots,
				control,
			)?;
			crate::gql::execution::select::resolve_graph_value(value, read)
		})?;
		let source = if capture.source == 0 {
			&mut query.source
		} else {
			&mut query
				.joins
				.get_mut(capture.source as usize - 1)
				.ok_or_else(|| {
					FerrosGrynnError::invalid_argument(
						"frozen LIVE JOIN source is outside its recipe",
					)
				})?
				.source
		};
		let replacement = match source {
			crate::live::LiveSource::Class(class) => crate::live::LiveSource::GraphClass {
				graph,
				class: *class,
			},
			crate::live::LiveSource::Record(record) => {
				control.admit_reactive_bytes(
					record
						.estimated_heap_bytes()
						.checked_add(std::mem::size_of_val(record))
						.ok_or_else(|| {
							FerrosGrynnError::operation_overflow("frozen LIVE point source")
						})?,
				)?;
				crate::live::LiveSource::GraphRecord {
					graph,
					record: record.clone(),
				}
			}
			_ => {
				return Err(FerrosGrynnError::invalid_argument(
					"frozen LIVE graph scope requires a CLASS or record source",
				));
			}
		};
		*source = replacement;
	}
	bind_query(&mut query, &plan.captures, slots, control, 0, &mut 0)?;
	let mut options = LiveOptions::default().with_overflow(plan.overflow);
	if let Some(since) = &plan.since {
		let since = match operand(transaction, snapshot, since, slots, control)? {
			Value::Number(number) => LiveSince::Versionstamp(u64::try_from(number)?.into()),
			Value::DateTime(time) => LiveSince::Timestamp(time),
			_ => {
				return Err(FerrosGrynnError::invalid_argument(
					"LIVE SINCE requires a versionstamp or timestamp",
				));
			}
		};
		options = options.with_since(since);
	}
	if let Some(buffer) = &plan.buffer {
		let Value::Number(number) = operand(transaction, snapshot, buffer, slots, control)? else {
			return Err(FerrosGrynnError::invalid_argument(
				"LIVE BUFFER requires a positive integer",
			));
		};
		let bytes = options.queue_bytes();
		options = options.with_queue(usize::try_from(number)?, bytes)?;
	}
	let work = options.work().min(control.max_loop_iterations);
	options = options.with_budgets(control.barrier_memory_available(), work)?;
	let id = transaction.register_live_controlled(
		query.with_parameters(parameters),
		options,
		control,
	)?;
	transaction.note_gql_live_registration(id)?;
	Ok(Value::from(id.as_uuid()))
}

/// Both registration families freeze ordinary values without storing reactive
/// UUIDs as values.
fn capture_parameters(
	captures: &[LogicalLiveCapture],
	slots: &[Value],
	control: &ExecutionControl,
) -> FerrosGrynnResult<Object> {
	let mut parameters = Object::new();
	for capture in captures.iter().filter(|capture| !capture.reactive) {
		let value = slots.get(capture.slot as usize).ok_or_else(|| {
			FerrosGrynnError::invalid_argument("LIVE capture slot is outside its typed frame")
		})?;
		let value = control.copy_reactive_value(value)?;
		parameters.insert(capture.name.as_ref(), value);
	}
	Ok(parameters)
}

/// Rewrites only lexically proven reactive captures in the admitted owned
/// recipe.
fn bind_expression(
	expression: &mut LiveExpr,
	captures: &[LogicalLiveCapture],
	slots: &[Value],
	control: &ExecutionControl,
	depth: u32,
	work: &mut u64,
) -> FerrosGrynnResult<()> {
	control.check()?;
	*work = work
		.checked_add(1)
		.ok_or_else(|| FerrosGrynnError::operation_overflow("LIVE captures work"))?;
	if depth > 64 || *work > control.max_loop_iterations {
		return Err(FerrosGrynnError::traversal_resource_limit_exceeded(
			"LIVE captures work/depth",
			control.max_loop_iterations,
		));
	}
	match expression {
		LiveExpr::Range {
			start,
			end,
			..
		} => {
			for child in start.iter_mut().chain(end.iter_mut()) {
				bind_expression(child, captures, slots, control, depth + 1, work)?;
			}
		}
		LiveExpr::Parameter(name) => {
			let index = captures
				.binary_search_by(|capture| capture.name.as_ref().cmp(name.as_str()))
				.map_err(|_index| {
					FerrosGrynnError::invalid_argument("LIVE capture provenance is absent")
				})?;
			let capture = &captures[index];
			if capture.reactive {
				let Some(Value::Uuid(uuid)) = slots.get(capture.slot as usize) else {
					return Err(FerrosGrynnError::invalid_argument(
						"LIVE LET capture identity requires a UUID",
					));
				};
				*expression = LiveExpr::Binding(LiveBindingId::from_uuid(*uuid)?);
			}
		}
		LiveExpr::Not(value)
		| LiveExpr::Negate(value)
		| LiveExpr::Positive(value)
		| LiveExpr::Cast {
			value,
			..
		} => {
			bind_expression(value, captures, slots, control, depth + 1, work)?;
		}
		LiveExpr::Access {
			base,
			..
		} => bind_expression(base, captures, slots, control, depth + 1, work)?,
		LiveExpr::Index {
			base,
			index,
		} => {
			bind_expression(base, captures, slots, control, depth + 1, work)?;
			bind_expression(index, captures, slots, control, depth + 1, work)?;
		}
		LiveExpr::Binary {
			left,
			right,
			..
		} => {
			bind_expression(left, captures, slots, control, depth + 1, work)?;
			bind_expression(right, captures, slots, control, depth + 1, work)?;
		}
		LiveExpr::Conditional {
			condition,
			then_value,
			else_value,
		} => {
			for child in [condition, then_value, else_value] {
				bind_expression(child, captures, slots, control, depth + 1, work)?;
			}
		}
		LiveExpr::Case {
			operand,
			branches,
			else_value,
		} => {
			for child in operand
				.iter_mut()
				.map(Box::as_mut)
				.chain(branches.iter_mut().flat_map(|(condition, value)| [condition, value]))
				.chain(else_value.iter_mut().map(Box::as_mut))
			{
				bind_expression(child, captures, slots, control, depth + 1, work)?;
			}
		}
		LiveExpr::Call {
			arguments,
			..
		}
		| LiveExpr::Array(arguments)
		| LiveExpr::Set(arguments) => {
			for argument in arguments {
				bind_expression(argument, captures, slots, control, depth + 1, work)?;
			}
		}
		LiveExpr::CallWithOptions {
			arguments,
			..
		} => {
			for (_, value) in arguments {
				bind_expression(value, captures, slots, control, depth + 1, work)?;
			}
		}
		LiveExpr::Object(fields) => {
			for (_, value) in fields {
				bind_expression(value, captures, slots, control, depth + 1, work)?;
			}
		}
		LiveExpr::Select(query) => bind_query(query, captures, slots, control, depth + 1, work)?,
		LiveExpr::Literal(_) | LiveExpr::Field(_) | LiveExpr::Binding(_) => {}
	}
	Ok(())
}

/// Nested SELECT uses the same captured identities as the surrounding
/// expression.
fn bind_query(
	query: &mut LiveQuery,
	captures: &[LogicalLiveCapture],
	slots: &[Value],
	control: &ExecutionControl,
	depth: u32,
	work: &mut u64,
) -> FerrosGrynnResult<()> {
	for source in query.sources_mut() {
		if let crate::live::LiveSource::GraphExpr(graph)
		| crate::live::LiveSource::GraphClassExpr {
			graph,
			..
		}
		| crate::live::LiveSource::GraphRecordExpr {
			graph,
			..
		} = source
		{
			bind_expression(graph, captures, slots, control, depth + 1, work)?;
		}
	}
	for join in &mut query.joins {
		control.check()?;
		if let Some(predicate) = &mut join.predicate {
			bind_expression(predicate, captures, slots, control, depth + 1, work)?;
		}
	}
	if let Some(filter) = &mut query.filter {
		bind_expression(filter, captures, slots, control, depth + 1, work)?;
	}
	for value in &mut query.split {
		bind_expression(value, captures, slots, control, depth + 1, work)?;
	}
	if let Some(keys) = &mut query.group {
		for key in keys {
			bind_expression(key, captures, slots, control, depth + 1, work)?;
		}
	}
	for order in &mut query.order {
		if let crate::live::LiveOrderKey::Expression(value) = &mut order.key {
			bind_expression(value, captures, slots, control, depth + 1, work)?;
		}
	}
	for value in query.limit.iter_mut().chain(query.start_offset.iter_mut()) {
		bind_expression(value, captures, slots, control, depth + 1, work)?;
	}
	match &mut query.projection {
		LiveProjection::Records => {}
		LiveProjection::Value(expression) => {
			bind_expression(expression, captures, slots, control, depth + 1, work)?;
		}
		LiveProjection::Fields(fields) => {
			for field in fields {
				bind_expression(&mut field.expression, captures, slots, control, depth + 1, work)?;
			}
		}
	}
	Ok(())
}

/// Defines or replaces a recoverable expression through the existing SDK
/// lifecycle.
pub(super) fn binding(
	transaction: &mut Transaction,
	plan: &LogicalLiveBinding,
	replace: Option<&TypedExpr>,
	slots: &[Value],
	source: &str,
	control: &ExecutionControl,
) -> FerrosGrynnResult<Value> {
	control.check()?;
	let mut account = control.barrier_account();
	if !account.reserve(plan.retained_bytes) {
		return Err(FerrosGrynnError::traversal_resource_limit_exceeded(
			"LIVE LET recipe",
			control.barrier_memory_limit() as u64,
		));
	}
	let parameters = capture_parameters(&plan.captures, slots, control)?;
	let mut expression = plan.expression.clone();
	bind_expression(&mut expression, &plan.captures, slots, control, 0, &mut 0)?;
	let defaults = LiveBindingOptions::default();
	let work = defaults.work().min(control.max_loop_iterations);
	let options = defaults.with_budgets(control.barrier_memory_available(), work)?;
	if let Some(replace) = replace {
		let id = LiveBindingId::from_uuid(identity(replace, slots, source, control)?.as_uuid())?;
		transaction.replace_live_binding_controlled(
			id,
			expression,
			parameters,
			options,
			Some(control),
		)?;
		Ok(Value::None)
	} else {
		let id = transaction.define_live_binding_controlled(
			expression,
			parameters,
			options,
			Some(control),
		)?;
		Ok(Value::from(id.as_uuid()))
	}
}

/// Reports definition and value revisions without attaching any consumer.
pub(super) fn binding_info(
	transaction: &Transaction,
	snapshot: &crate::datastore::QuerySnapshot,
	expression: &TypedExpr,
	slots: &[Value],
	control: &ExecutionControl,
) -> FerrosGrynnResult<Value> {
	let Value::Uuid(uuid) = operand(transaction, snapshot, expression, slots, control)? else {
		return Err(FerrosGrynnError::invalid_argument("LIVE LET identity requires a UUID"));
	};
	let id = LiveBindingId::from_uuid(uuid)?;
	let mut account = control.barrier_account();
	if !account.reserve(4096) {
		return Err(FerrosGrynnError::traversal_resource_limit_exceeded(
			"LIVE LET metadata",
			control.barrier_memory_limit() as u64,
		));
	}
	let (info, _metadata_account) = transaction.query_live_binding_info(snapshot, id, control)?;
	let bytes = info
		.dependencies()
		.len()
		.checked_mul(128)
		.ok_or_else(|| FerrosGrynnError::operation_overflow("LIVE LET dependency metadata"))?;
	if !account.reserve(bytes) {
		return Err(FerrosGrynnError::traversal_resource_limit_exceeded(
			"LIVE LET dependencies",
			control.barrier_memory_limit() as u64,
		));
	}
	let dependencies =
		info.dependencies().iter().map(|id| Value::from(id.as_uuid())).collect::<Vec<_>>();
	let state = match info.state() {
		LiveState::Pending => "pending",
		LiveState::Active => "active",
		LiveState::Paused => "paused",
		LiveState::Closed => "closed",
	};
	let mut row = Object::from_iter([
		("id", Value::from(id.as_uuid())),
		("definition", Value::from(info.revision().definition().get())),
		("value_version", Value::from(info.revision().value().get())),
		("state", Value::from(state)),
		("dependencies", Value::from(dependencies)),
		(
			"diagnostic",
			info.diagnostic()
				.map_or(Value::None, |error| Value::from(error.status().code().to_string())),
		),
	]);
	row.insert("value", info.value);
	Ok(Value::Object(row))
}
