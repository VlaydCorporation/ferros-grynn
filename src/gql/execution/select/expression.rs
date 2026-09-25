//! SELECT expression materialization and aggregate evaluation.

use super::*;

pub(super) fn eval_select_expr<Read: QueryRead>(
	read: &Read,
	snapshot: Snapshot,
	expression: &TypedExpr,
	row: &Row,
	group: Option<&RowSet>,
	slots: &[Value],
	source: &str,
	control: &ExecutionControl,
) -> FerrosGrynnResult<Value> {
	let fields = RecordFields {
		read,
		snapshot,
		graph: row.graph.as_ref(),
	};
	if !contains_select_special(expression) {
		return eval(expression, Some(row), slots, source, Some(&fields), Some(control));
	}
	let expression =
		materialize_specials(read, snapshot, expression, row, group, slots, source, control)?;
	eval(&expression, Some(row), slots, source, Some(&fields), Some(control))
}

fn contains_select_special(expression: &TypedExpr) -> bool {
	match &expression.kind {
		TypedExprKind::Aggregate {
			..
		}
		| TypedExprKind::Subquery {
			..
		} => true,
		TypedExprKind::Array(values) | TypedExprKind::Set(values) => {
			values.iter().any(contains_select_special)
		}
		TypedExprKind::Object(fields) => {
			fields.iter().any(|(_name, value)| contains_select_special(value))
		}
		TypedExprKind::Range {
			start,
			end,
			..
		} => {
			start.as_deref().is_some_and(contains_select_special)
				|| end.as_deref().is_some_and(contains_select_special)
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
		} => contains_select_special(expr),
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
		} => contains_select_special(left) || contains_select_special(right),
		TypedExprKind::Conditional {
			condition,
			then_value,
			else_value,
		} => {
			contains_select_special(condition)
				|| contains_select_special(then_value)
				|| contains_select_special(else_value)
		}
		TypedExprKind::Case {
			operand,
			branches,
			else_value,
		} => {
			operand.as_deref().is_some_and(contains_select_special)
				|| branches.iter().any(|(condition, value)| {
					contains_select_special(condition) || contains_select_special(value)
				})
				|| else_value.as_deref().is_some_and(contains_select_special)
		}
		TypedExprKind::Builtin {
			arguments,
			..
		} => arguments.iter().any(contains_select_special),
		TypedExprKind::Constant(_)
		| TypedExprKind::Slot(_)
		| TypedExprKind::Column(_)
		| TypedExprKind::CurrentField(_) => false,
	}
}

fn materialize_specials<Read: QueryRead>(
	read: &Read,
	snapshot: Snapshot,
	expression: &TypedExpr,
	row: &Row,
	group: Option<&RowSet>,
	slots: &[Value],
	source: &str,
	control: &ExecutionControl,
) -> FerrosGrynnResult<TypedExpr> {
	let recurse = |value: &TypedExpr| {
		materialize_specials(read, snapshot, value, row, group, slots, source, control)
	};
	let kind = match &expression.kind {
		TypedExprKind::Aggregate {
			function,
			argument,
			distinct,
		} => {
			return aggregate_value(
				read,
				snapshot,
				*function,
				argument.as_deref(),
				*distinct,
				group,
				row,
				slots,
				source,
				control,
			)
			.map(|value| TypedExpr {
				kind: TypedExprKind::Constant(value),
				ty: expression.ty.clone(),
				span: expression.span,
			});
		}
		TypedExprKind::Subquery {
			select,
			collection,
		} => {
			let values = execute_select_values(
				read,
				snapshot,
				select,
				slots,
				source,
				control,
				Some(row),
				(!*collection).then_some(2),
			)?;
			let value = if *collection {
				Value::Array(values.into_iter().collect())
			} else {
				match values.as_slice() {
					[] => Value::None,
					[value] => value.clone(),
					_ => {
						return Err(FerrosGrynnError::subquery_cardinality_violation(
							values.len() as u64
						));
					}
				}
			};
			TypedExprKind::Constant(value)
		}
		TypedExprKind::Array(values) => {
			TypedExprKind::Array(values.iter().map(recurse).collect::<FerrosGrynnResult<_>>()?)
		}
		TypedExprKind::Set(values) => {
			TypedExprKind::Set(values.iter().map(recurse).collect::<FerrosGrynnResult<_>>()?)
		}
		TypedExprKind::Object(fields) => TypedExprKind::Object(
			fields
				.iter()
				.map(|(name, value)| recurse(value).map(|value| (name.clone(), value)))
				.collect::<FerrosGrynnResult<_>>()?,
		),
		TypedExprKind::Range {
			start,
			end,
			inclusive_end,
		} => TypedExprKind::Range {
			start: start.as_deref().map(recurse).transpose()?.map(Box::new),
			end: end.as_deref().map(recurse).transpose()?.map(Box::new),
			inclusive_end: *inclusive_end,
		},
		TypedExprKind::Unary {
			op,
			expr,
		} => TypedExprKind::Unary {
			op: *op,
			expr: Box::new(recurse(expr)?),
		},
		TypedExprKind::Binary {
			left,
			op,
			right,
		} => TypedExprKind::Binary {
			left: Box::new(recurse(left)?),
			op: *op,
			right: Box::new(recurse(right)?),
		},
		TypedExprKind::Conditional {
			condition,
			then_value,
			else_value,
		} => TypedExprKind::Conditional {
			condition: Box::new(recurse(condition)?),
			then_value: Box::new(recurse(then_value)?),
			else_value: Box::new(recurse(else_value)?),
		},
		TypedExprKind::Case {
			operand,
			branches,
			else_value,
		} => TypedExprKind::Case {
			operand: operand.as_deref().map(recurse).transpose()?.map(Box::new),
			branches: branches
				.iter()
				.map(|(condition, value)| Ok((recurse(condition)?, recurse(value)?)))
				.collect::<FerrosGrynnResult<_>>()?,
			else_value: else_value.as_deref().map(recurse).transpose()?.map(Box::new),
		},
		TypedExprKind::Field {
			base,
			field,
			null_safe,
		} => TypedExprKind::Field {
			base: Box::new(recurse(base)?),
			field: field.clone(),
			null_safe: *null_safe,
		},
		TypedExprKind::Destructure {
			base,
			fields,
		} => TypedExprKind::Destructure {
			base: Box::new(recurse(base)?),
			fields: fields.clone(),
		},
		TypedExprKind::Index {
			base,
			index,
		} => TypedExprKind::Index {
			base: Box::new(recurse(base)?),
			index: Box::new(recurse(index)?),
		},
		TypedExprKind::Filter {
			base,
			predicate,
		} => TypedExprKind::Filter {
			base: Box::new(recurse(base)?),
			predicate: Box::new(recurse(predicate)?),
		},
		TypedExprKind::Builtin {
			function,
			arguments,
		} => TypedExprKind::Builtin {
			function: function.clone(),
			arguments: arguments.iter().map(recurse).collect::<FerrosGrynnResult<_>>()?,
		},
		TypedExprKind::Cast {
			target,
			expr,
		} => TypedExprKind::Cast {
			target: target.clone(),
			expr: Box::new(recurse(expr)?),
		},
		kind => kind.clone(),
	};
	Ok(TypedExpr {
		kind,
		ty: expression.ty.clone(),
		span: expression.span,
	})
}

#[allow(clippy::too_many_arguments, reason = "aggregate evaluation needs the statement context")]
fn aggregate_value<Read: QueryRead>(
	read: &Read,
	snapshot: Snapshot,
	function: Aggregate,
	argument: Option<&TypedExpr>,
	distinct: bool,
	group: Option<&RowSet>,
	fallback: &Row,
	slots: &[Value],
	source: &str,
	control: &ExecutionControl,
) -> FerrosGrynnResult<Value> {
	let mut state = AggregateState::new(function);
	let mut push_value = |row: Row| {
		let value = match argument {
			Some(argument) => {
				eval_select_expr(read, snapshot, argument, &row, None, slots, source, control)?
			}
			None => Value::from(1_i64),
		};
		state.push(value, argument.is_none())
	};
	if distinct {
		let root = control
			.temporary_directory
			.as_deref()
			.map(ToOwned::to_owned)
			.unwrap_or_else(|| read.query_scratch_root());
		let mut sorter = QuerySorter::new(
			&root,
			read.spill_context(snapshot)?,
			vec![SortField {
				descending: false,
				nulls_first: false,
			}],
			control.barrier_account(),
			false,
		)?;
		let mut ordinal = 0_usize;
		visit_group(group, fallback, |row| {
			let value = match argument {
				Some(argument) => {
					eval_select_expr(read, snapshot, argument, &row, None, slots, source, control)?
				}
				None => Value::from(1_i64),
			};
			sorter.push(CursorRecord {
				value: value.clone(),
				anchor: CursorAnchor {
					order_keys: vec![value],
					stable_key: vec![Value::from(ordinal)],
				},
			})?;
			ordinal = ordinal
				.checked_add(1)
				.ok_or_else(|| FerrosGrynnError::invalid_argument("aggregate ordinal overflow"))?;
			Ok(())
		})?;
		let mut previous = None::<Value>;
		consume_records(sorter.finish()?, |record| {
			if previous.as_ref() == Some(&record.value) {
				return Ok(());
			}
			previous = Some(record.value.clone());
			state.push(record.value, argument.is_none())
		})?;
	} else {
		visit_group(group, fallback, &mut push_value)?;
	}
	state.finish()
}

fn visit_group(
	group: Option<&RowSet>,
	fallback: &Row,
	mut visit: impl FnMut(Row) -> FerrosGrynnResult<()>,
) -> FerrosGrynnResult<()> {
	match group {
		Some(group) => group.visit(visit),
		None => visit(fallback.clone()),
	}
}

enum AggregateState {
	Count(usize),
	Sum(Value),
	Average {
		count: usize,
		sum: f64,
	},
	Minimum(Option<Value>),
	Maximum(Option<Value>),
	ArrayGroup(Array),
	Variance {
		count: usize,
		mean: f64,
		m2: f64,
		sample: bool,
		standard_deviation: bool,
	},
}

impl AggregateState {
	fn new(function: Aggregate) -> Self {
		match function {
			Aggregate::Count => Self::Count(0),
			Aggregate::Sum => Self::Sum(Value::from(0_i64)),
			Aggregate::Average => Self::Average {
				count: 0,
				sum: 0.0,
			},
			Aggregate::Minimum => Self::Minimum(None),
			Aggregate::Maximum => Self::Maximum(None),
			Aggregate::ArrayGroup => Self::ArrayGroup(Array::new(Vec::new())),
			Aggregate::VariancePopulation | Aggregate::StandardDeviationPopulation => {
				Self::Variance {
					count: 0,
					mean: 0.0,
					m2: 0.0,
					sample: false,
					standard_deviation: function == Aggregate::StandardDeviationPopulation,
				}
			}
			Aggregate::VarianceSample | Aggregate::StandardDeviationSample => Self::Variance {
				count: 0,
				mean: 0.0,
				m2: 0.0,
				sample: true,
				standard_deviation: function == Aggregate::StandardDeviationSample,
			},
		}
	}

	fn push(&mut self, value: Value, count_all: bool) -> FerrosGrynnResult<()> {
		if matches!(self, Self::Count(_)) {
			if count_all || !matches!(value, Value::None | Value::Null) {
				let Self::Count(count) = self else {
					unreachable!();
				};
				*count = count.checked_add(1).ok_or_else(|| {
					FerrosGrynnError::invalid_argument("aggregate count overflow")
				})?;
			}
			return Ok(());
		}
		if matches!(value, Value::None) {
			return Ok(());
		}
		if matches!(value, Value::Null) && !matches!(self, Self::ArrayGroup(_)) {
			return Ok(());
		}
		match self {
			Self::Count(_) => unreachable!(),
			Self::Sum(sum) => *sum = binary(BinaryOperator::Add, sum.clone(), value)?,
			Self::Average {
				count,
				sum,
			} => {
				*sum += numeric_value(&value)?;
				*count = count.checked_add(1).ok_or_else(|| {
					FerrosGrynnError::invalid_argument("aggregate count overflow")
				})?;
			}
			Self::Minimum(minimum) => {
				if minimum.as_ref().is_none_or(|current| value < *current) {
					*minimum = Some(value);
				}
			}
			Self::Maximum(maximum) => {
				if maximum.as_ref().is_none_or(|current| value > *current) {
					*maximum = Some(value);
				}
			}
			Self::ArrayGroup(grouped) => match value {
				Value::Array(values) => values.into_iter().for_each(|value| grouped.push(value)),
				Value::Set(values) => values.into_iter().for_each(|value| grouped.push(value)),
				value => grouped.push(value),
			},
			Self::Variance {
				count,
				mean,
				m2,
				..
			} => {
				let value = numeric_value(&value)?;
				*count = count.checked_add(1).ok_or_else(|| {
					FerrosGrynnError::invalid_argument("aggregate count overflow")
				})?;
				let count = *count as f64;
				let delta = value - *mean;
				*mean += delta / count;
				*m2 += delta * (value - *mean);
			}
		}
		Ok(())
	}

	fn finish(self) -> FerrosGrynnResult<Value> {
		match self {
			Self::Count(count) => Ok(Value::from(count)),
			Self::Sum(sum) => Ok(sum),
			Self::Average {
				count: 0,
				..
			} => Ok(Value::None),
			Self::Average {
				count,
				sum,
			} => Ok(Value::from(sum / count as f64)),
			Self::Minimum(value) | Self::Maximum(value) => Ok(value.unwrap_or(Value::None)),
			Self::ArrayGroup(values) => Ok(Value::Array(values)),
			Self::Variance {
				count,
				m2: _,
				sample,
				standard_deviation: _,
				..
			} if count == 0 || sample && count < 2 => Ok(Value::None),
			Self::Variance {
				count,
				m2,
				sample,
				standard_deviation,
				..
			} => {
				let divisor = if sample {
					count - 1
				} else {
					count
				} as f64;
				let variance = m2 / divisor;
				Ok(Value::from(if standard_deviation {
					variance.sqrt()
				} else {
					variance
				}))
			}
		}
	}
}

fn numeric_value(value: &Value) -> FerrosGrynnResult<f64> {
	let Value::Number(number) = value else {
		return Err(FerrosGrynnError::invalid_argument("Aggregate requires numeric values."));
	};
	number.as_f64().ok_or_else(|| {
		FerrosGrynnError::invalid_argument("Numeric value cannot be represented as f64.")
	})
}
