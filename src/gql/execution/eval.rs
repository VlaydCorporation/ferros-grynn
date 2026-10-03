#![allow(
	clippy::wildcard_imports,
	reason = "move-only executor split keeps the existing imports during stage 1"
)]

use super::*;
use crate::values::Vector;

pub(super) fn eval(
	expr: &TypedExpr,
	row: Option<&Row>,
	slots: &[Value],
	source: &str,
	fields: Option<&dyn AtomFieldRead>,
	control: Option<&ExecutionControl>,
) -> FerrosGrynnResult<Value> {
	let mut slots = SlotFrame::Read(slots);
	eval_current(expr, row, &mut slots, source, fields, None, control)
}

pub(super) fn eval_mut(
	expr: &TypedExpr,
	row: Option<&Row>,
	slots: &mut [Value],
	source: &str,
	fields: Option<&dyn AtomFieldRead>,
	control: Option<&ExecutionControl>,
) -> FerrosGrynnResult<Value> {
	let mut slots = SlotFrame::Write(slots);
	eval_current(expr, row, &mut slots, source, fields, None, control)
}

pub(super) fn eval_mut_current(
	expr: &TypedExpr,
	row: Option<&Row>,
	slots: &mut [Value],
	source: &str,
	fields: Option<&dyn AtomFieldRead>,
	current: Option<&Value>,
	control: Option<&ExecutionControl>,
) -> FerrosGrynnResult<Value> {
	let mut slots = SlotFrame::Write(slots);
	eval_current(expr, row, &mut slots, source, fields, current, control)
}

enum SlotFrame<'a> {
	Read(&'a [Value]),
	Write(&'a mut [Value]),
}

impl SlotFrame<'_> {
	fn values(&self) -> &[Value] {
		match self {
			Self::Read(values) => values,
			Self::Write(values) => values,
		}
	}

	fn get_mut(&mut self, slot: u32) -> FerrosGrynnResult<&mut Value> {
		match self {
			Self::Read(_) => Err(FerrosGrynnError::invalid_argument(
				"Mutable collection helpers require a writable lexical scope.",
			)),
			Self::Write(values) => values.get_mut(slot as usize).ok_or_else(|| {
				FerrosGrynnError::invalid_argument("Mutable collection binding slot is invalid.")
			}),
		}
	}
}

fn eval_current(
	expr: &TypedExpr,
	row: Option<&Row>,
	slots: &mut SlotFrame<'_>,
	source: &str,
	fields: Option<&dyn AtomFieldRead>,
	current: Option<&Value>,
	control: Option<&ExecutionControl>,
) -> FerrosGrynnResult<Value> {
	if let Some(control) = control {
		control.check()?;
	}
	if let Some(fields) = fields {
		fields.work(1)?;
	}
	let result = match &expr.kind {
		TypedExprKind::Constant(value) => copy_evaluation_value(value, fields),
		TypedExprKind::Slot(slot) => slots
			.values()
			.get(*slot as usize)
			.ok_or_else(|| FerrosGrynnError::invalid_argument("Expression slot is invalid."))
			.and_then(|value| copy_evaluation_value(value, fields)),
		TypedExprKind::Column(column) => row
			.and_then(|row| row.value(*column))
			.ok_or_else(|| {
				FerrosGrynnError::invalid_argument("Column is missing from the input row.")
			})
			.and_then(|value| copy_evaluation_value(value, fields)),
		TypedExprKind::Array(values) | TypedExprKind::Set(values) => {
			admit_evaluation_values(std::iter::empty(), values.len(), fields)?;
			let values = values
				.iter()
				.map(|value| eval_current(value, row, slots, source, fields, current, control));
			if matches!(expr.kind, TypedExprKind::Array(_)) {
				values.collect::<FerrosGrynnResult<Array>>().map(Value::Array)
			} else {
				values.collect::<FerrosGrynnResult<crate::values::Set>>().map(Value::Set)
			}
		}
		TypedExprKind::Object(object_fields) => {
			admit_evaluation_values(std::iter::empty(), object_fields.len(), fields)?;
			object_fields
				.iter()
				.map(|(name, value)| {
					if let Some(fields) = fields {
						fields.admit(name.len())?;
					}
					eval_current(value, row, slots, source, fields, current, control)
						.map(|value| (name.as_ref(), value))
				})
				.collect::<FerrosGrynnResult<Object>>()
				.map(Value::Object)
		}
		TypedExprKind::Range {
			start,
			end,
			inclusive_end,
		} => {
			let start = start
				.as_ref()
				.map(|value| eval_current(value, row, slots, source, fields, current, control))
				.transpose()?
				.map_or(std::ops::Bound::Unbounded, std::ops::Bound::Included);
			let end = end
				.as_ref()
				.map(|value| eval_current(value, row, slots, source, fields, current, control))
				.transpose()?
				.map_or(std::ops::Bound::Unbounded, |value| {
					if *inclusive_end {
						std::ops::Bound::Included(value)
					} else {
						std::ops::Bound::Excluded(value)
					}
				});
			crate::values::Range::new(start, end).map(Value::Range)
		}
		TypedExprKind::Unary {
			op,
			expr,
		} => unary(*op, eval_current(expr, row, slots, source, fields, current, control)?),
		TypedExprKind::Binary {
			left,
			op,
			right,
		} => {
			let left = eval_current(left, row, slots, source, fields, current, control)?;
			match op {
				BinaryOperator::And if !left.is_truthy() => Ok(Value::Bool(false)),
				BinaryOperator::Or if left.is_truthy() => Ok(Value::Bool(true)),
				BinaryOperator::Coalesce if !matches!(left, Value::None | Value::Null) => Ok(left),
				BinaryOperator::Elvis if left.is_truthy() => Ok(left),
				_ => {
					let right = eval_current(right, row, slots, source, fields, current, control)?;
					if let Some(fields) = fields.filter(|fields| fields.bounded()) {
						fields.admit_value(&left)?;
						fields.admit_value(&right)?;
						let left = left.estimated_heap_bytes().saturating_add(1);
						let right = right.estimated_heap_bytes().saturating_add(1);
						let work = left.checked_mul(right).ok_or_else(|| {
							FerrosGrynnError::operation_overflow("PURE comparison work")
						})?;
						fields.work(work as u64)?;
					}
					binary(*op, left, right)
				}
			}
		}
		TypedExprKind::Conditional {
			condition,
			then_value,
			else_value,
		} => {
			if eval_current(condition, row, slots, source, fields, current, control)?.is_truthy() {
				eval_current(then_value, row, slots, source, fields, current, control)
			} else {
				eval_current(else_value, row, slots, source, fields, current, control)
			}
		}
		TypedExprKind::Case {
			operand,
			branches,
			else_value,
		} => {
			let operand = operand
				.as_ref()
				.map(|value| eval_current(value, row, slots, source, fields, current, control))
				.transpose()?;
			let mut selected = None;
			for (condition, value) in branches {
				let condition =
					eval_current(condition, row, slots, source, fields, current, control)?;
				let matches = operand
					.as_ref()
					.map_or_else(|| condition.is_truthy(), |operand| operand == &condition);
				if matches {
					selected =
						Some(eval_current(value, row, slots, source, fields, current, control)?);
					break;
				}
			}
			if let Some(value) = selected {
				Ok(value)
			} else if let Some(value) = else_value {
				eval_current(value, row, slots, source, fields, current, control)
			} else {
				Ok(Value::None)
			}
		}
		TypedExprKind::Field {
			base,
			field,
			null_safe,
		} => {
			if let TypedExprKind::Column(column) = base.kind
				&& let Some(atom) = row.and_then(|row| row.atoms.get(column)).and_then(|atom| *atom)
				&& let Some(fields) = fields
			{
				return fields
					.field(atom, field)
					.map_err(|error| locate_execution(error, expr, source));
			}
			let base = eval_current(base, row, slots, source, fields, current, control)?;
			match base {
				Value::Object(object) => object
					.get(field)
					.map_or(Ok(Value::None), |value| copy_evaluation_value(value, fields)),
				Value::Error(error) => Ok(error_field(&error, field)),
				Value::Matcher(matcher) => matcher_field(&matcher, field),
				Value::AtomOccurrence(occurrence) if fields.is_some() => fields
					.ok_or_else(|| {
						FerrosGrynnError::invalid_argument(
							"Atom occurrence resolver is unavailable.",
						)
					})?
					.occurrence_field(&occurrence, field),
				Value::Record(record) if fields.is_some() => fields
					.ok_or_else(|| {
						FerrosGrynnError::invalid_argument("Record resolver is unavailable.")
					})?
					.record_field(&record, field),
				Value::None | Value::Null if *null_safe => Ok(Value::None),
				value => Err(FerrosGrynnError::invalid_argument(format!(
					"Cannot access field `{field}` on {}.",
					value.kind()
				))),
			}
		}
		TypedExprKind::CurrentField(field) => {
			if let Some(current) = current {
				admit_evaluation_values(std::iter::once(current), 0, fields)?;
			}
			current_field(current, field)
		}
		TypedExprKind::Destructure {
			base,
			fields: names,
		} => {
			let Value::Object(base) =
				eval_current(base, row, slots, source, fields, current, control)?
			else {
				return Err(locate_execution(
					FerrosGrynnError::invalid_argument("Object destructuring requires an object."),
					expr,
					source,
				));
			};
			admit_evaluation_values(base.values(), names.len(), fields)?;
			Ok(Value::Object(
				names
					.iter()
					.map(|name| (name.as_ref(), base.get(name).cloned().unwrap_or(Value::None)))
					.collect(),
			))
		}
		TypedExprKind::Index {
			base,
			index,
		} => {
			let base = eval_current(base, row, slots, source, fields, current, control)?;
			let index = eval_current(index, row, slots, source, fields, current, control)?;
			admit_evaluation_values(std::iter::once(&base), 0, fields)?;
			index_value(base, index)
		}
		TypedExprKind::Filter {
			base,
			predicate,
		} => {
			let Value::Array(values) =
				eval_current(base, row, slots, source, fields, current, control)?
			else {
				return Err(locate_execution(
					FerrosGrynnError::invalid_argument("Collection filter requires an array."),
					expr,
					source,
				));
			};
			admit_evaluation_values(values.iter(), values.len(), fields)?;
			let mut filtered = Array::new(Vec::new());
			for value in values {
				if eval_current(predicate, row, slots, source, fields, Some(&value), control)?
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
		} if matches!(
			function,
			Builtin::ArrayPush(_)
				| Builtin::ArrayPop(_)
				| Builtin::ArrayClear(_)
				| Builtin::SetAdd(_)
				| Builtin::SetClear(_)
		) =>
		{
			let arguments = arguments
				.iter()
				.map(|argument| {
					eval_current(argument, row, slots, source, fields, current, control)
				})
				.collect::<FerrosGrynnResult<Vec<_>>>()?;
			admit_evaluation_values(arguments.iter(), arguments.len(), fields)?;
			mutate_collection(function, arguments, slots)
		}
		TypedExprKind::Builtin {
			function,
			arguments,
		} if matches!(function, Builtin::SearchScore(_) | Builtin::SearchHighlight(_)) => {
			let row = row.ok_or_else(|| {
				FerrosGrynnError::invalid_argument(
					"search score and highlight require a SELECT match row",
				)
			})?;
			match function {
				Builtin::SearchScore(slot) => {
					let score = row
						.fulltext_slot(*slot)
						.map_or(0.0, |entry| f64::from_bits(entry.score_bits));
					Ok(Value::from(score))
				}
				Builtin::SearchHighlight(slot) => {
					let open = eval_current(
						arguments.first().ok_or_else(|| {
							FerrosGrynnError::invalid_argument("search::highlight has no open tag")
						})?,
						Some(row),
						slots,
						source,
						fields,
						current,
						control,
					)?;
					let close = eval_current(
						arguments.get(1).ok_or_else(|| {
							FerrosGrynnError::invalid_argument("search::highlight has no close tag")
						})?,
						Some(row),
						slots,
						source,
						fields,
						current,
						control,
					)?;
					let (Value::String(open), Value::String(close)) = (open, close) else {
						return Err(FerrosGrynnError::invalid_argument(
							"search::highlight tags must be strings",
						));
					};
					let entry = row.fulltext_slot(*slot).ok_or_else(|| {
						FerrosGrynnError::invalid_argument(
							"search::highlight slot has no Fulltext match in this row",
						)
					})?;
					if !entry.highlights {
						return Err(FerrosGrynnError::invalid_argument(
							"search::highlight requires a Fulltext definition with HIGHLIGHTS",
						));
					}
					Ok(Value::from(render_highlight(
						&entry.text,
						&entry.spans,
						open.as_str(),
						close.as_str(),
					)?))
				}
				_ => unreachable!("search result dispatch was guarded above"),
			}
		}
		TypedExprKind::Builtin {
			function,
			arguments,
		} if matches!(function, Builtin::SearchAnalyze { .. }) => {
			let control = control.ok_or_else(|| {
				FerrosGrynnError::invalid_argument("search::analyze requires an execution session.")
			})?;
			let arguments = arguments
				.iter()
				.map(|argument| {
					eval_current(argument, row, slots, source, fields, current, Some(control))
				})
				.collect::<FerrosGrynnResult<Vec<_>>>()?;
			let [_, Value::String(input)] = arguments.as_slice() else {
				return Err(FerrosGrynnError::invalid_argument(
					"search::analyze requires an analyzer name and string input.",
				));
			};
			let Builtin::SearchAnalyze {
				analyzer,
				function,
			} = function
			else {
				unreachable!("search analyzer dispatch was guarded above")
			};
			if let Some(fields) = fields {
				if let Some(id) = analyzer.id() {
					fields.observe(crate::datastore::ConflictKey::SchemaAnalyzer(id));
				}
				fields.admit(input.len())?;
			}
			let tokens = if let Some(function) = function {
				let plan = control.resolve_named_function(*function)?;
				let _frame = control.enter_named_function(*function)?;
				let result = invoke_logical_closure_with_fields(
					control,
					&plan,
					&[],
					&[Value::String(input.clone())],
					source,
					fields,
				)?;
				admit_evaluation_values(std::iter::once(&result), 0, fields)?;
				if let Some(fields) = fields.filter(|fields| fields.bounded()) {
					fields.admit(result.estimated_heap_bytes().checked_mul(4).ok_or_else(
						|| FerrosGrynnError::operation_overflow("PURE analyzer token copies"),
					)?)?;
				}
				crate::datastore::analyze_function_result_bounded(
					analyzer,
					input.as_str(),
					&result,
					&|bytes, work| {
						if let Some(fields) = fields {
							fields.admit(bytes)?;
							fields.work(work)?;
						}
						Ok(())
					},
				)?
			} else {
				if let Some(fields) = fields.filter(|fields| fields.bounded()) {
					fields.work(input.len() as u64)?;
					fields.admit(input.len().checked_mul(256).ok_or_else(|| {
						FerrosGrynnError::operation_overflow("PURE analyzer scratch")
					})?)?;
				}
				crate::datastore::analyze_builtin_bounded(
					analyzer,
					input.as_str(),
					&|bytes, work| {
						if let Some(fields) = fields {
							fields.admit(bytes)?;
							fields.work(work)?;
						}
						Ok(())
					},
				)?
			};
			admit_evaluation_values(std::iter::empty(), tokens.len(), fields)?;
			Ok(Value::Array(crate::values::Array::from_iter(
				tokens.into_iter().map(|token| Value::String(token.term)),
			)))
		}
		TypedExprKind::Builtin {
			function,
			arguments,
		} if matches!(function, Builtin::GraphInner | Builtin::GraphLevelPath) => {
			let argument = arguments.first().ok_or_else(|| {
				FerrosGrynnError::invalid_argument("graph::inner requires one argument")
			})?;
			let value = eval_current(argument, row, slots, source, fields, current, control)?;
			let Value::AtomOccurrence(occurrence) = value else {
				return Err(FerrosGrynnError::invalid_argument(
					"graph::inner requires an atom occurrence",
				));
			};
			let fields = fields.ok_or_else(|| {
				FerrosGrynnError::invalid_argument(
					"graph occurrence functions require a MATCH field resolver",
				)
			})?;
			match function {
				Builtin::GraphInner => fields.inner_graph(&occurrence),
				Builtin::GraphLevelPath => fields.level_path(&occurrence),
				_ => Err(FerrosGrynnError::invalid_argument(
					"invalid graph occurrence function dispatch",
				)),
			}
		}
		TypedExprKind::Builtin {
			function: Builtin::PathCostWith(cost),
			arguments,
		} => {
			let argument = arguments.first().ok_or_else(|| {
				FerrosGrynnError::invalid_argument("custom path cost has no path")
			})?;
			let value = eval_current(argument, row, slots, source, fields, current, control)?;
			let Value::Path(path) = value else {
				return Err(FerrosGrynnError::invalid_argument("path::cost requires a path"));
			};
			let fields = fields.ok_or_else(|| {
				FerrosGrynnError::invalid_argument("custom path cost requires a field resolver")
			})?;
			let control = control.ok_or_else(|| {
				FerrosGrynnError::invalid_argument("custom path cost requires an execution session")
			})?;
			custom_path_cost(
				&path,
				*cost,
				MatchPredicateContext {
					slots: slots.values(),
					source,
					fields,
				},
				control,
			)
			.map(Value::Number)
		}
		TypedExprKind::Builtin {
			function,
			arguments,
		} => arguments
			.iter()
			.map(|argument| eval_current(argument, row, slots, source, fields, current, control))
			.collect::<FerrosGrynnResult<Vec<_>>>()
			.and_then(|arguments| {
				crate::gql::execution::builtins::execute_with_fields(
					function, arguments, control, fields,
				)
			}),
		TypedExprKind::Closure(closure) => control
			.ok_or_else(|| {
				FerrosGrynnError::invalid_argument(
					"Language closure creation requires an execution session.",
				)
			})?
			.create_closure(closure, slots.values())
			.map(|closure| Value::Closure(Box::new(closure))),
		TypedExprKind::Invoke {
			callee,
			arguments,
		} => {
			let callee = eval_current(callee, row, slots, source, fields, current, control)?;
			let Value::Closure(closure) = callee else {
				return Err(FerrosGrynnError::invalid_argument(
					"Only a closure value can be invoked.",
				));
			};
			let arguments = arguments
				.iter()
				.map(|argument| {
					eval_current(argument, row, slots, source, fields, current, control)
				})
				.collect::<FerrosGrynnResult<Vec<_>>>()?;
			if closure.session_owner().is_none() {
				closure.call(&arguments)
			} else {
				invoke_expression_closure_with_fields(
					control.ok_or_else(|| {
						FerrosGrynnError::closure_requires_session(closure.id().get())
					})?,
					&closure,
					&arguments,
					source,
					fields,
				)
			}
		}
		TypedExprKind::NamedInvoke {
			function,
			arguments,
		} => {
			let control = control.ok_or_else(|| {
				FerrosGrynnError::invalid_argument(
					"Named function invocation requires an execution session.",
				)
			})?;
			let arguments = arguments
				.iter()
				.map(|argument| {
					eval_current(argument, row, slots, source, fields, current, Some(control))
				})
				.collect::<FerrosGrynnResult<Vec<_>>>()?;
			let plan = control.resolve_named_function(*function)?;
			let _frame = control.enter_named_function(*function)?;
			invoke_logical_closure_with_fields(control, &plan, &[], &arguments, source, fields)
		}
		TypedExprKind::Traversal(_) => Err(FerrosGrynnError::invalid_argument(
			"Traversal expression requires the transaction-aware evaluator.",
		)),
		TypedExprKind::Fulltext {
			..
		} => row
			.and_then(|row| row.fulltext_match(expr.span.offset))
			.map(|entry| Value::Bool(entry.matched))
			.ok_or_else(|| {
				FerrosGrynnError::invalid_argument(
					"Fulltext predicate reached evaluation before SELECT search preparation",
				)
			}),
		TypedExprKind::Knn {
			..
		} => {
			row.map(|row| Value::Bool(row.knn_match(expr.span.offset).is_some())).ok_or_else(|| {
				FerrosGrynnError::invalid_argument(
					"KNN predicate reached evaluation before SELECT search preparation",
				)
			})
		}
		TypedExprKind::Aggregate {
			..
		}
		| TypedExprKind::Subquery {
			..
		} => Err(FerrosGrynnError::invalid_argument(
			"SELECT aggregate or subquery reached the scalar evaluator before materialization.",
		)),
		TypedExprKind::Cast {
			target,
			expr,
		} => cast(eval_current(expr, row, slots, source, fields, current, control)?, target),
	};
	result.map_err(|error| locate_execution(error, expr, source))
}

/// Admits deep clones before allocating any owned evaluation value.
fn copy_evaluation_value(
	value: &Value,
	fields: Option<&dyn AtomFieldRead>,
) -> FerrosGrynnResult<Value> {
	fields.map_or_else(|| Ok(value.clone()), |fields| fields.copy_value(value))
}

/// Conservatively accounts payload copies and collection-node growth.
fn admit_evaluation_values<'a>(
	values: impl Iterator<Item = &'a Value>,
	shells: usize,
	fields: Option<&dyn AtomFieldRead>,
) -> FerrosGrynnResult<()> {
	if let Some(fields) = fields.filter(|fields| fields.bounded()) {
		fields.admit(
			shells
				.checked_mul(128)
				.ok_or_else(|| FerrosGrynnError::operation_overflow("PURE value scratch"))?,
		)?;
		for value in values {
			fields.admit_value(value)?;
		}
	}
	Ok(())
}

fn render_highlight(
	text: &str,
	spans: &[(u32, u32)],
	open: &str,
	close: &str,
) -> FerrosGrynnResult<String> {
	if spans.is_empty() {
		return Ok(text.to_owned());
	}
	let mut scalar_offsets = text.char_indices().map(|(offset, _)| offset).collect::<Vec<_>>();
	scalar_offsets.push(text.len());
	let mut output = String::with_capacity(
		text.len().saturating_add(spans.len().saturating_mul(open.len() + close.len())),
	);
	let mut copied = 0_usize;
	for &(start, end) in spans {
		let start = usize::try_from(start).map_err(|_| {
			FerrosGrynnError::invalid_argument("highlight span start does not fit usize")
		})?;
		let end = usize::try_from(end).map_err(|_| {
			FerrosGrynnError::invalid_argument("highlight span end does not fit usize")
		})?;
		let (Some(&start), Some(&end)) = (scalar_offsets.get(start), scalar_offsets.get(end))
		else {
			return Err(FerrosGrynnError::invalid_argument(
				"highlight span is outside the source text",
			));
		};
		if start < copied || start > end {
			return Err(FerrosGrynnError::invalid_argument(
				"highlight spans are not sorted and disjoint",
			));
		}
		output.push_str(&text[copied..start]);
		output.push_str(open);
		output.push_str(&text[start..end]);
		output.push_str(close);
		copied = end;
	}
	output.push_str(&text[copied..]);
	Ok(output)
}

pub(super) fn invoke_expression_closure(
	control: &ExecutionControl,
	closure: &crate::values::Closure,
	arguments: &[Value],
	source: &str,
) -> FerrosGrynnResult<Value> {
	invoke_expression_closure_with_fields(control, closure, arguments, source, None)
}

/// Preserves the captured snapshot resolver through nested language closures.
fn invoke_expression_closure_with_fields(
	control: &ExecutionControl,
	closure: &crate::values::Closure,
	arguments: &[Value],
	source: &str,
	fields: Option<&dyn AtomFieldRead>,
) -> FerrosGrynnResult<Value> {
	let entry = control.resolve_closure(closure)?;
	let _frame = entry
		.plan
		.named_function
		.map(|function| control.enter_named_function(function))
		.transpose()?;
	invoke_logical_closure_with_fields(
		control,
		&entry.plan,
		&entry.captures,
		arguments,
		source,
		fields,
	)
}

pub(in crate::gql) fn invoke_logical_closure(
	control: &ExecutionControl,
	plan: &LogicalClosure,
	captures: &[(u32, Value)],
	arguments: &[Value],
	source: &str,
) -> FerrosGrynnResult<Value> {
	invoke_logical_closure_with_fields(control, plan, captures, arguments, source, None)
}

/// Evaluates one PURE function with the immutable builder's field resolver.
///
/// # Errors
/// Propagates typed evaluation, field resolution and bounded execution errors.
pub(in crate::gql) fn invoke_root_logical_closure(
	root: &crate::datastore::DatabaseRoot,
	control: &ExecutionControl,
	plan: &LogicalClosure,
	arguments: &[Value],
	source: &str,
	read: &dyn Fn(crate::datastore::ConflictKey),
	work: &dyn Fn(u64) -> FerrosGrynnResult<()>,
) -> FerrosGrynnResult<Value> {
	invoke_logical_closure_with_fields(
		control,
		plan,
		&[],
		arguments,
		source,
		Some(&RootFields::new(root, control, read, work)),
	)
}

/// Evaluates the existing typed IR while retaining resolver scope in every
/// call.
pub(in crate::gql::execution) fn invoke_logical_closure_with_fields(
	control: &ExecutionControl,
	plan: &LogicalClosure,
	captures: &[(u32, Value)],
	arguments: &[Value],
	source: &str,
	fields: Option<&dyn AtomFieldRead>,
) -> FerrosGrynnResult<Value> {
	if plan.parameters.len() != arguments.len() {
		return Err(FerrosGrynnError::conversion_error(
			format!("{} closure arguments", arguments.len()),
			format!("{} arguments", plan.parameters.len()),
		));
	}
	control.check()?;
	let frame_bytes = captures
		.iter()
		.map(|(_, value)| value)
		.chain(arguments)
		.fold(plan.slot_count.checked_mul(std::mem::size_of::<Value>()), |bytes, value| {
			bytes?.checked_add(value.estimated_heap_bytes())
		})
		.ok_or_else(|| FerrosGrynnError::operation_overflow("PURE frame scratch"))?;
	let mut frame = control.barrier_account();
	if !frame.reserve(frame_bytes) {
		return Err(FerrosGrynnError::traversal_resource_limit_exceeded(
			"PURE frame scratch",
			control.barrier_memory_limit() as u64,
		));
	}
	let mut slots = Vec::new();
	slots.try_reserve_exact(plan.slot_count).map_err(|cause| {
		FerrosGrynnError::traversal_resource_limit_exceeded(
			"PURE frame allocation",
			control.barrier_memory_limit() as u64,
		)
		.with_technical_source(cause)
	})?;
	slots.resize(plan.slot_count, Value::None);
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
	match &plan.body {
		crate::gql::ir::LogicalClosureBody::Expression(expression) => {
			eval_mut(expression, None, &mut slots, source, fields, Some(control))
		}
		crate::gql::ir::LogicalClosureBody::Block(block) => {
			let mut runtime = super::ControlRuntime::default();
			match execute_pure_block(block, &mut slots, source, control, &mut runtime, 1, fields)? {
				PureControlFlow::Value(value) | PureControlFlow::Return(value) => Ok(value),
				PureControlFlow::Break | PureControlFlow::Continue => {
					Err(FerrosGrynnError::invalid_argument(
						"BREAK or CONTINUE cannot cross a closure boundary.",
					))
				}
			}
		}
	}
}

enum PureControlFlow {
	Value(Value),
	Return(Value),
	Break,
	Continue,
}

fn execute_pure_block(
	block: &crate::gql::ir::LogicalBlock,
	slots: &mut [Value],
	source: &str,
	control: &ExecutionControl,
	runtime: &mut super::ControlRuntime,
	depth: u32,
	fields: Option<&dyn AtomFieldRead>,
) -> FerrosGrynnResult<PureControlFlow> {
	let mut last = Value::None;
	for statement in &block.statements {
		let child = control.child(statement.timeout);
		match execute_pure_statement(
			&statement.statement,
			slots,
			source,
			&child,
			runtime,
			depth,
			fields,
		)? {
			PureControlFlow::Value(value) => last = value,
			flow @ (PureControlFlow::Return(_)
			| PureControlFlow::Break
			| PureControlFlow::Continue) => return Ok(flow),
		}
	}
	Ok(PureControlFlow::Value(last))
}

fn execute_pure_statement(
	statement: &crate::gql::ir::LogicalStatement,
	slots: &mut [Value],
	source: &str,
	control: &ExecutionControl,
	runtime: &mut super::ControlRuntime,
	depth: u32,
	fields: Option<&dyn AtomFieldRead>,
) -> FerrosGrynnResult<PureControlFlow> {
	use crate::gql::ir::{LogicalLetValue, LogicalStatement};

	control.check()?;
	if depth > control.max_control_depth {
		return Err(FerrosGrynnError::control_limit_exceeded(
			"nesting depth",
			u64::from(control.max_control_depth),
		));
	}
	if let Some(fields) = fields {
		fields.work(1)?;
	}
	let value = match statement {
		LogicalStatement::Let {
			slot,
			value: LogicalLetValue::Expression(value),
			..
		} => {
			slots[*slot as usize] = eval_mut(value, None, slots, source, fields, Some(control))?;
			Value::None
		}
		LogicalStatement::Return {
			value,
		} => {
			return eval_mut(value, None, slots, source, fields, Some(control))
				.map(PureControlFlow::Return);
		}
		LogicalStatement::Expression {
			value,
		} => eval_mut(value, None, slots, source, fields, Some(control))?,
		LogicalStatement::If {
			branches,
			else_block,
		} => {
			let mut selected = None;
			for (condition, block) in branches {
				if eval_mut(condition, None, slots, source, fields, Some(control))?.is_truthy() {
					selected = Some(block);
					break;
				}
			}
			if let Some(block) = selected.or(else_block.as_ref()) {
				return execute_pure_block(
					block,
					slots,
					source,
					control,
					runtime,
					depth + 1,
					fields,
				);
			}
			Value::None
		}
		LogicalStatement::For {
			slot,
			iterable,
			body,
		} => {
			let values = super::iterable_values(eval_mut(
				iterable,
				None,
				slots,
				source,
				fields,
				Some(control),
			)?)?;
			let mut last = Value::None;
			for value in values {
				control.check()?;
				if let Some(fields) = fields {
					fields.work(1)?;
				}
				super::record_iteration(runtime, control)?;
				slots[*slot as usize] = value;
				match execute_pure_block(body, slots, source, control, runtime, depth + 1, fields)?
				{
					PureControlFlow::Value(value) => last = value,
					PureControlFlow::Break => break,
					PureControlFlow::Continue => continue,
					flow @ PureControlFlow::Return(_) => return Ok(flow),
				}
			}
			last
		}
		LogicalStatement::While {
			condition,
			body,
		} => {
			let mut last = Value::None;
			while eval_mut(condition, None, slots, source, fields, Some(control))?.is_truthy() {
				control.check()?;
				if let Some(fields) = fields {
					fields.work(1)?;
				}
				super::record_iteration(runtime, control)?;
				match execute_pure_block(body, slots, source, control, runtime, depth + 1, fields)?
				{
					PureControlFlow::Value(value) => last = value,
					PureControlFlow::Break => break,
					PureControlFlow::Continue => continue,
					flow @ PureControlFlow::Return(_) => return Ok(flow),
				}
			}
			last
		}
		LogicalStatement::Break => return Ok(PureControlFlow::Break),
		LogicalStatement::Continue => return Ok(PureControlFlow::Continue),
		LogicalStatement::Try {
			body,
			catch,
		} => {
			admit_evaluation_values(slots.iter(), slots.len(), fields)?;
			let saved_slots = slots.to_vec();
			match execute_pure_block(body, slots, source, control, runtime, depth + 1, fields) {
				Ok(flow) => return Ok(flow),
				Err(error) if catch.as_ref().is_some_and(|_| super::catchable(&error)) => {
					admit_evaluation_values(saved_slots.iter(), saved_slots.len(), fields)?;
					slots.clone_from_slice(&saved_slots);
					let (slot, handler) = catch.as_ref().ok_or_else(|| {
						FerrosGrynnError::invalid_argument("CATCH handler is missing.")
					})?;
					slots[*slot as usize] =
						Value::Error(Box::new(crate::values::ErrorValue::from_error(&error)));
					return execute_pure_block(
						handler,
						slots,
						source,
						control,
						runtime,
						depth + 1,
						fields,
					);
				}
				Err(error) => return Err(error),
			}
		}
		LogicalStatement::Throw {
			value,
		} => {
			let Value::String(message) =
				eval_mut(value, None, slots, source, fields, Some(control))?
			else {
				return Err(FerrosGrynnError::invalid_argument("THROW requires a string value."));
			};
			return Err(FerrosGrynnError::user_thrown(message.to_string()));
		}
		_ => {
			return Err(FerrosGrynnError::invalid_storage_file(
				"PURE function body contains a non-pure statement",
			));
		}
	};
	control.check()?;
	Ok(PureControlFlow::Value(value))
}

fn mutate_collection(
	function: &Builtin,
	mut arguments: Vec<Value>,
	slots: &mut SlotFrame<'_>,
) -> FerrosGrynnResult<Value> {
	let slot = match function {
		Builtin::ArrayPush(slot)
		| Builtin::ArrayPop(slot)
		| Builtin::ArrayClear(slot)
		| Builtin::SetAdd(slot)
		| Builtin::SetClear(slot) => *slot,
		_ => {
			return Err(FerrosGrynnError::invalid_argument(
				"Invalid mutable collection helper dispatch.",
			));
		}
	};
	let target = slots.get_mut(slot)?;
	match function {
		Builtin::ArrayPush(_) => {
			let value = arguments.pop().ok_or_else(|| {
				FerrosGrynnError::invalid_argument("array::push requires a value.")
			})?;
			let Value::Array(values) = target else {
				return Err(FerrosGrynnError::invalid_argument(
					"array::push requires an array binding.",
				));
			};
			values.push(value);
			i64::try_from(values.len())
				.map(Value::from)
				.map_err(|_| FerrosGrynnError::operation_overflow("array length"))
		}
		Builtin::ArrayPop(_) => {
			let Value::Array(values) = target else {
				return Err(FerrosGrynnError::invalid_argument(
					"array::pop requires an array binding.",
				));
			};
			Ok(values.pop().unwrap_or(Value::None))
		}
		Builtin::ArrayClear(_) => {
			let Value::Array(values) = target else {
				return Err(FerrosGrynnError::invalid_argument(
					"array::clear requires an array binding.",
				));
			};
			values.clear();
			Ok(Value::None)
		}
		Builtin::SetAdd(_) => {
			let value = arguments
				.pop()
				.ok_or_else(|| FerrosGrynnError::invalid_argument("set::add requires a value."))?;
			let Value::Set(values) = target else {
				return Err(FerrosGrynnError::invalid_argument("set::add requires a set binding."));
			};
			Ok(Value::Bool(values.insert(value)))
		}
		Builtin::SetClear(_) => {
			let Value::Set(values) = target else {
				return Err(FerrosGrynnError::invalid_argument(
					"set::clear requires a set binding.",
				));
			};
			values.clear();
			Ok(Value::None)
		}
		_ => Err(FerrosGrynnError::invalid_argument("Invalid mutable collection helper dispatch.")),
	}
}

fn unary(operator: UnaryOperator, value: Value) -> FerrosGrynnResult<Value> {
	match operator {
		UnaryOperator::Not => Ok(Value::Bool(!value.is_truthy())),
		UnaryOperator::Truthy => Ok(Value::Bool(value.is_truthy())),
		UnaryOperator::Positive => Ok(value),
		UnaryOperator::Negate => match value {
			Value::Number(Number::Int(value)) => value
				.checked_neg()
				.map(Value::from)
				.ok_or_else(|| FerrosGrynnError::operation_overflow("unary -")),
			Value::Number(Number::Float(value)) => Ok(Value::from(-value)),
			Value::Number(Number::Decimal(value)) => Decimal::ZERO
				.checked_sub(value)
				.map(|value| Value::Number(Number::Decimal(value)))
				.ok_or_else(|| FerrosGrynnError::operation_overflow("unary -")),
			value => Err(FerrosGrynnError::invalid_argument(format!(
				"Unary minus requires a number, found {}.",
				value.kind()
			))),
		},
	}
}

pub(super) fn binary(
	operator: BinaryOperator,
	left: Value,
	right: Value,
) -> FerrosGrynnResult<Value> {
	match operator {
		BinaryOperator::Inside
		| BinaryOperator::NotInside
		| BinaryOperator::Outside
		| BinaryOperator::Intersects => spatial(operator, &left, &right).map(Value::Bool),
		BinaryOperator::AllInside | BinaryOperator::AnyInside | BinaryOperator::NoneInside => {
			if matches!(left, Value::None | Value::Null)
				|| matches!(right, Value::None | Value::Null)
			{
				return Ok(Value::Bool(false));
			}
			let Value::Geometry(geometry) = &right else {
				return Err(FerrosGrynnError::invalid_argument(
					"Spatial quantifiers require a geometry right operand.",
				));
			};
			geometry.validate()?;
			let mut any = false;
			let mut all = true;
			for value in collection_values(&left)? {
				let matched = spatial(BinaryOperator::Inside, value, &right)?;
				any |= matched;
				all &= matched;
			}
			Ok(Value::Bool(match operator {
				BinaryOperator::AllInside => all,
				BinaryOperator::AnyInside => any,
				_ => !any,
			}))
		}
		BinaryOperator::Power
		| BinaryOperator::Multiply
		| BinaryOperator::Divide
		| BinaryOperator::Remainder
		| BinaryOperator::Add
		| BinaryOperator::Subtract => arithmetic(operator, left, right),
		BinaryOperator::Less => Ok(Value::Bool(left < right)),
		BinaryOperator::LessOrEqual => Ok(Value::Bool(left <= right)),
		BinaryOperator::Greater => Ok(Value::Bool(left > right)),
		BinaryOperator::GreaterOrEqual => Ok(Value::Bool(left >= right)),
		BinaryOperator::Equal | BinaryOperator::Is => Ok(Value::Bool(left == right)),
		BinaryOperator::NotEqual | BinaryOperator::NotIs => Ok(Value::Bool(left != right)),
		BinaryOperator::In | BinaryOperator::NotIn => {
			let contains = contains(&right, &left)?;
			Ok(Value::Bool(if matches!(operator, BinaryOperator::NotIn) {
				!contains
			} else {
				contains
			}))
		}
		BinaryOperator::AllIn | BinaryOperator::AnyIn | BinaryOperator::NoneIn => {
			let values = collection_values(&left)?;
			let matches = values
				.iter()
				.map(|value| contains(&right, value))
				.collect::<FerrosGrynnResult<Vec<_>>>()?;
			Ok(Value::Bool(match operator {
				BinaryOperator::AllIn => matches.into_iter().all(|value| value),
				BinaryOperator::AnyIn => matches.into_iter().any(|value| value),
				BinaryOperator::NoneIn => matches.into_iter().all(|value| !value),
				_ => false,
			}))
		}
		BinaryOperator::AnyEqual | BinaryOperator::AllEqual => {
			let values = collection_values(&left)?;
			Ok(Value::Bool(if matches!(operator, BinaryOperator::AnyEqual) {
				values.iter().any(|value| *value == &right)
			} else {
				values.iter().all(|value| *value == &right)
			}))
		}
		BinaryOperator::Contains | BinaryOperator::ContainsNot => {
			let contains = contains(&left, &right)?;
			Ok(Value::Bool(if matches!(operator, BinaryOperator::ContainsNot) {
				!contains
			} else {
				contains
			}))
		}
		BinaryOperator::ContainsAny | BinaryOperator::ContainsAll => {
			let values = collection_values(&right)?;
			let matches = values
				.iter()
				.map(|value| contains(&left, value))
				.collect::<FerrosGrynnResult<Vec<_>>>()?;
			Ok(Value::Bool(if matches!(operator, BinaryOperator::ContainsAny) {
				matches.into_iter().any(|value| value)
			} else {
				matches.into_iter().all(|value| value)
			}))
		}
		BinaryOperator::And => Ok(Value::Bool(left.is_truthy() && right.is_truthy())),
		BinaryOperator::Or => Ok(Value::Bool(left.is_truthy() || right.is_truthy())),
		BinaryOperator::Coalesce => Ok(if matches!(left, Value::None | Value::Null) {
			right
		} else {
			left
		}),
		BinaryOperator::Elvis => Ok(if left.is_truthy() {
			left
		} else {
			right
		}),
		BinaryOperator::Pipeline => {
			Err(FerrosGrynnError::query_feature_not_supported("expression pipeline"))
		}
	}
}

/// Shared exact spatial predicate; sentinel checks precede every negation.
fn spatial(operator: BinaryOperator, left: &Value, right: &Value) -> FerrosGrynnResult<bool> {
	if matches!(left, Value::None | Value::Null) || matches!(right, Value::None | Value::Null) {
		return Ok(false);
	}
	let (Value::Geometry(left), Value::Geometry(right)) = (left, right) else {
		return Err(FerrosGrynnError::invalid_argument(
			"Spatial operators require geometry operands.",
		));
	};
	match operator {
		BinaryOperator::Intersects => left.intersects(right),
		BinaryOperator::NotInside | BinaryOperator::Outside => {
			left.within(right).map(|matched| !matched)
		}
		_ => left.within(right),
	}
}

fn contains(container: &Value, needle: &Value) -> FerrosGrynnResult<bool> {
	match container {
		Value::Array(values) => Ok(values.iter().any(|value| value == needle)),
		Value::Set(values) => Ok(values.contains(needle)),
		Value::Object(values) => {
			Ok(needle.as_str().is_some_and(|needle| values.contains_key(needle)))
		}
		Value::String(value) => {
			Ok(needle.as_str().is_some_and(|needle| value.as_str().contains(needle)))
		}
		Value::Range(value) => Ok(value.contains(needle)),
		value => Err(FerrosGrynnError::invalid_argument(format!(
			"{} does not support containment.",
			value.kind()
		))),
	}
}

fn collection_values(value: &Value) -> FerrosGrynnResult<Vec<&Value>> {
	match value {
		Value::Array(values) => Ok(values.iter().collect()),
		Value::Set(values) => Ok(values.iter().collect()),
		value => Err(FerrosGrynnError::invalid_argument(format!(
			"{} is not an iterable collection.",
			value.kind()
		))),
	}
}

fn current_field(current: Option<&Value>, field: &str) -> FerrosGrynnResult<Value> {
	match current {
		Some(Value::Object(value)) => Ok(value.get(field).cloned().unwrap_or(Value::None)),
		Some(Value::Error(error)) => Ok(error_field(error, field)),
		Some(value) => Err(FerrosGrynnError::invalid_argument(format!(
			"Cannot resolve field `{field}` on {}.",
			value.kind()
		))),
		None => Err(FerrosGrynnError::invalid_argument(format!(
			"Field `{field}` has no current collection item."
		))),
	}
}

fn error_field(error: &crate::values::ErrorValue, field: &str) -> Value {
	match field.to_ascii_lowercase().as_str() {
		"code" => Value::from(error.code()),
		"message" => Value::from(error.message()),
		"condition" => Value::from(error.condition()),
		"classification" => Value::from(error.classification()),
		"category" => Value::from(error.category()),
		"domain" => Value::from(error.domain()),
		"causes" => Value::Array(
			error.causes().iter().cloned().map(|value| Value::Error(Box::new(value))).collect(),
		),
		_ => Value::None,
	}
}

fn matcher_field(matcher: &crate::values::Matcher, field: &str) -> FerrosGrynnResult<Value> {
	let offset = match field.to_ascii_lowercase().as_str() {
		"start" => matcher.start(),
		"end" => matcher.end(),
		_ => {
			return Err(FerrosGrynnError::invalid_argument(
				"Matcher exposes only `start` and `end` fields.",
			));
		}
	};
	offset.map_or(Ok(Value::None), |offset| {
		i64::try_from(offset)
			.map(Value::from)
			.map_err(|_| FerrosGrynnError::operation_overflow("matcher Unicode scalar offset"))
	})
}

fn arithmetic(operator: BinaryOperator, left: Value, right: Value) -> FerrosGrynnResult<Value> {
	let (Value::Number(left), Value::Number(right)) = (left, right) else {
		return Err(FerrosGrynnError::invalid_argument("Arithmetic operators require numbers."));
	};
	match (left, right) {
		(Number::Int(left), Number::Int(right)) => integer_arithmetic(operator, left, right),
		(Number::Float(left), Number::Float(right)) => float_arithmetic(operator, left, right),
		(Number::Float(left), Number::Int(right)) => float_arithmetic(operator, left, right as f64),
		(Number::Int(left), Number::Float(right)) => float_arithmetic(operator, left as f64, right),
		(Number::Decimal(left), Number::Decimal(right)) => {
			decimal_arithmetic(operator, left, right)
		}
		(Number::Decimal(left), Number::Int(right)) => {
			decimal_arithmetic(operator, left, Decimal::from(right))
		}
		(Number::Int(left), Number::Decimal(right)) => {
			decimal_arithmetic(operator, Decimal::from(left), right)
		}
		(Number::Float(_), Number::Decimal(_)) | (Number::Decimal(_), Number::Float(_)) => {
			Err(FerrosGrynnError::invalid_argument(
				"Float and decimal arithmetic requires an explicit cast.",
			))
		}
	}
}

fn integer_arithmetic(operator: BinaryOperator, left: i64, right: i64) -> FerrosGrynnResult<Value> {
	let value = match operator {
		BinaryOperator::Add => add_exact(&left, &right)?,
		BinaryOperator::Subtract => subtract_exact(&left, &right)?,
		BinaryOperator::Multiply => multiply_exact(&left, &right)?,
		BinaryOperator::Divide => divide_signed_exact(&left, &right)?,
		BinaryOperator::Remainder if right == 0 => return Err(FerrosGrynnError::division_by_zero()),
		BinaryOperator::Remainder => {
			left.checked_rem(right).ok_or_else(|| FerrosGrynnError::operation_overflow("%"))?
		}
		BinaryOperator::Power => {
			let exponent = u32::try_from(right).map_err(|_conversion_error| {
				FerrosGrynnError::invalid_argument("Integer exponent must be non-negative.")
			})?;
			left.checked_pow(exponent).ok_or_else(|| FerrosGrynnError::operation_overflow("**"))?
		}
		_ => return Err(FerrosGrynnError::invalid_argument("Invalid arithmetic operator.")),
	};
	Ok(Value::from(value))
}

fn float_arithmetic(operator: BinaryOperator, left: f64, right: f64) -> FerrosGrynnResult<Value> {
	if matches!(operator, BinaryOperator::Divide | BinaryOperator::Remainder) && right == 0.0 {
		return Err(FerrosGrynnError::division_by_zero());
	}
	let value = match operator {
		BinaryOperator::Add => left + right,
		BinaryOperator::Subtract => left - right,
		BinaryOperator::Multiply => left * right,
		BinaryOperator::Divide => left / right,
		BinaryOperator::Remainder => left % right,
		BinaryOperator::Power => left.powf(right),
		_ => return Err(FerrosGrynnError::invalid_argument("Invalid arithmetic operator.")),
	};
	if value.is_nan() {
		return Err(FerrosGrynnError::invalid_argument(
			"Exponentiation received a value outside its domain.",
		));
	}
	if value.is_infinite() {
		return Err(FerrosGrynnError::operation_overflow(operator_name(operator)));
	}
	Ok(Value::from(value))
}

fn decimal_arithmetic(
	operator: BinaryOperator,
	left: Decimal,
	right: Decimal,
) -> FerrosGrynnResult<Value> {
	if matches!(operator, BinaryOperator::Divide | BinaryOperator::Remainder) && right.is_zero() {
		return Err(FerrosGrynnError::division_by_zero());
	}
	let value = match operator {
		BinaryOperator::Add => left.checked_add(right),
		BinaryOperator::Subtract => left.checked_sub(right),
		BinaryOperator::Multiply => left.checked_mul(right),
		BinaryOperator::Divide => left.checked_div(right),
		BinaryOperator::Remainder => left.checked_rem(right),
		BinaryOperator::Power => {
			return decimal_power(left, right).map(Number::Decimal).map(Value::Number);
		}
		_ => return Err(FerrosGrynnError::invalid_argument("Invalid arithmetic operator.")),
	}
	.ok_or_else(|| FerrosGrynnError::operation_overflow(operator_name(operator)))?;
	Ok(Value::Number(Number::Decimal(value)))
}

fn decimal_power(base: Decimal, exponent: Decimal) -> FerrosGrynnResult<Decimal> {
	let exponent = i64::try_from(Number::Decimal(exponent)).map_err(|_error| {
		FerrosGrynnError::invalid_argument("Decimal exponent must be an integer.")
	})?;
	if exponent.is_negative() && base.is_zero() {
		return Err(FerrosGrynnError::division_by_zero());
	}

	let mut remaining = exponent.unsigned_abs();
	let mut factor = base;
	let mut value = Decimal::ONE;
	while remaining != 0 {
		if remaining & 1 == 1 {
			value = value
				.checked_mul(factor)
				.ok_or_else(|| FerrosGrynnError::operation_overflow("**"))?;
		}
		remaining >>= 1;
		if remaining != 0 {
			factor = factor
				.checked_mul(factor)
				.ok_or_else(|| FerrosGrynnError::operation_overflow("**"))?;
		}
	}
	if exponent.is_negative() {
		Decimal::ONE.checked_div(value).ok_or_else(|| FerrosGrynnError::operation_overflow("**"))
	} else {
		Ok(value)
	}
}

const fn operator_name(operator: BinaryOperator) -> &'static str {
	match operator {
		BinaryOperator::Power => "**",
		BinaryOperator::Multiply => "*",
		BinaryOperator::Divide => "/",
		BinaryOperator::Remainder => "%",
		BinaryOperator::Add => "+",
		BinaryOperator::Subtract => "-",
		_ => "?",
	}
}

fn index_value(base: Value, index: Value) -> FerrosGrynnResult<Value> {
	match (base, index) {
		(Value::Array(values), Value::Number(number)) => usize::try_from(number)
			.ok()
			.and_then(|index| values.get(index).cloned())
			.ok_or_else(|| FerrosGrynnError::invalid_argument("Array index is out of bounds.")),
		(Value::Vector(vector), Value::Number(number)) => {
			let index = usize::try_from(number).map_err(|_| {
				FerrosGrynnError::invalid_argument("Vector index must be non-negative.")
			})?;
			let value = match vector {
				Vector::F64(values) => values.get(index).copied().map(Number::Float),
				Vector::F32(values) => {
					values.get(index).copied().map(|value| Number::Float(f64::from(value)))
				}
				Vector::F16(values) => {
					values.get(index).copied().map(|value| Number::Float(f64::from(value.to_f32())))
				}
				Vector::I64(values) => values.get(index).copied().map(Number::Int),
				Vector::I32(values) => {
					values.get(index).copied().map(|value| Number::Int(i64::from(value)))
				}
				Vector::I16(values) => {
					values.get(index).copied().map(|value| Number::Int(i64::from(value)))
				}
				Vector::I8(values) => {
					values.get(index).copied().map(|value| Number::Int(i64::from(value)))
				}
				Vector::U8(values) => {
					values.get(index).copied().map(|value| Number::Int(i64::from(value)))
				}
			};
			value
				.map(Value::Number)
				.ok_or_else(|| FerrosGrynnError::invalid_argument("Vector index is out of bounds."))
		}
		(Value::Object(values), Value::String(key)) => {
			Ok(values.get(key.as_str()).cloned().unwrap_or(Value::None))
		}
		(value, _) => {
			Err(FerrosGrynnError::invalid_argument(format!("Cannot index {}.", value.kind())))
		}
	}
}

pub(super) fn cast(value: Value, target: &Type) -> FerrosGrynnResult<Value> {
	match target {
		Type::Geometry => match value {
			Value::Geometry(geometry) => {
				geometry.validate()?;
				Ok(Value::Geometry(geometry))
			}
			Value::String(value) => value.parse::<crate::values::Geometry>().map(Value::Geometry),
			Value::Object(object) => {
				crate::values::Geometry::from_object(&object).map(Value::Geometry)
			}
			_ => Err(FerrosGrynnError::conversion_error(value.kind().to_string(), "geometry")),
		},
		Type::Any => Ok(value),
		Type::Bool => Ok(Value::Bool(value.is_truthy())),
		Type::String => Ok(Value::from(value.to_string())),
		Type::Number => match value {
			Value::Number(_) => Ok(value),
			Value::String(value) => value.parse::<Number>().map(Value::Number),
			_ => Err(FerrosGrynnError::conversion_error(value.to_string(), "number")),
		},
		Type::Int => match value {
			Value::Number(value) => i64::try_from(value).map(Value::from),
			Value::String(value) => value
				.parse::<i64>()
				.map(Value::from)
				.map_err(|_parse_error| FerrosGrynnError::conversion_error(value.as_str(), "int")),
			_ => Err(FerrosGrynnError::conversion_error(value.to_string(), "int")),
		},
		Type::Float => match value {
			Value::Number(value) => f64::try_from(value).map(Value::from),
			Value::String(value) => value.parse::<f64>().map(Value::from).map_err(|_parse_error| {
				FerrosGrynnError::conversion_error(value.as_str(), "float")
			}),
			_ => Err(FerrosGrynnError::conversion_error(value.to_string(), "float")),
		},
		Type::Decimal => match value {
			Value::Number(value) => {
				Decimal::try_from(value).map(|value| Value::Number(Number::Decimal(value)))
			}
			Value::String(value) => {
				value.parse::<Decimal>().map(|value| Value::Number(Number::Decimal(value))).map_err(
					|_parse_error| FerrosGrynnError::conversion_error(value.as_str(), "decimal"),
				)
			}
			_ => Err(FerrosGrynnError::conversion_error(value.to_string(), "decimal")),
		},
		Type::Object => match value {
			Value::Object(_) => Ok(value),
			Value::Array(values) => merge_objects(values),
			Value::Set(values) => merge_objects(values),
			_ => Err(FerrosGrynnError::conversion_error(value.to_string(), "object")),
		},
		Type::None if matches!(value, Value::None) => Ok(value),
		Type::Null if matches!(value, Value::Null) => Ok(value),
		_ => Err(FerrosGrynnError::conversion_error(value.to_string(), target.to_string())),
	}
}

fn merge_objects(values: impl IntoIterator<Item = Value>) -> FerrosGrynnResult<Value> {
	let mut merged = Object::new();
	for value in values {
		let Value::Object(value) = value else {
			return Err(FerrosGrynnError::invalid_argument(
				"Object conversion requires a collection containing only objects.",
			));
		};
		for (key, value) in value {
			if merged.insert(key.clone(), value).is_some() {
				return Err(FerrosGrynnError::invalid_argument(format!(
					"Object conversion contains duplicate key `{key}`."
				)));
			}
		}
	}
	Ok(Value::Object(merged))
}

pub(super) fn eval_usize(
	expr: &TypedExpr,
	slots: &[Value],
	source: &str,
	control: &ExecutionControl,
) -> FerrosGrynnResult<usize> {
	let value = eval(expr, None, slots, source, None, Some(control))?;
	let Value::Number(number) = value else {
		return Err(locate_execution(
			FerrosGrynnError::invalid_argument("Expected a non-negative integer."),
			expr,
			source,
		));
	};
	usize::try_from(number).map_err(|error| locate_execution(error, expr, source))
}

fn locate_execution(error: FerrosGrynnError, expr: &TypedExpr, source: &str) -> FerrosGrynnError {
	let error = error.with_phase(ExecutionPhase::Execution);
	Location::from_source(None, source, expr.span)
		.map_or(error.clone(), |location| error.with_location(location))
}

pub(super) fn execution_phase(error: FerrosGrynnError) -> FerrosGrynnError {
	error.with_phase(ExecutionPhase::Execution)
}
