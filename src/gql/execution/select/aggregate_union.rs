//! Class-specific aggregate arguments share one ordinary reduction domain.

use super::*;

/// Private row tags select original typed expressions without retyping values.
#[derive(Debug, Clone, Copy)]
pub(super) enum RowSelector<'a> {
	/// Existing primary class union keeps its scalar ordinal CASE.
	Class(usize),
	/// Root JOIN families distinguish every declared class and missing LEFT
	/// slot.
	Joined {
		/// Fixed padded input columns, in declaration order.
		columns: &'a [usize],
		/// Each row typing state; None is an unmatched slot, never class zero.
		labels: &'a [Vec<Option<usize>>],
	},
}

impl RowSelector<'_> {
	/// Rejects inconsistent private metadata before allocating a CASE tree.
	fn validate(self, variants: usize) -> FerrosGrynnResult<()> {
		if let Self::Joined {
			columns,
			labels,
			..
		} = self
			&& (columns.is_empty()
				|| labels.len() != variants
				|| labels.iter().any(|label| label.len() != columns.len()))
		{
			return Err(incompatible());
		}
		Ok(())
	}

	/// Admits and constructs the ordinary CASE operand.
	fn operand<Read: QueryRead>(
		self,
		read: &Read,
		span: crate::gql_status::TextSpan,
	) -> FerrosGrynnResult<TypedExpr> {
		let column = |column| TypedExpr {
			kind: TypedExprKind::Column(column),
			ty: Type::Int,
			span,
		};
		match self {
			Self::Class(index) => Ok(column(index)),
			Self::Joined {
				columns,
				..
			} => {
				let fields = read.bounded_fields().ok_or_else(incompatible)?;
				fields.work(columns.len() as u64)?;
				fields.admit(
					columns
						.len()
						.checked_mul(2 * std::mem::size_of::<TypedExpr>())
						.ok_or_else(incompatible)?,
				)?;
				Ok(TypedExpr {
					kind: TypedExprKind::Array(columns.iter().copied().map(column).collect()),
					ty: Type::Array(Box::new(Type::Any)),
					span,
				})
			}
		}
	}

	/// Copies only admitted canonical tag values, preserving exact NONE slots.
	fn label<Read: QueryRead>(self, read: &Read, ordinal: usize) -> FerrosGrynnResult<Value> {
		match self {
			Self::Class(_) => Ok(Value::from(ordinal)),
			Self::Joined {
				labels,
				..
			} => {
				let label = labels.get(ordinal).ok_or_else(incompatible)?;
				let fields = read.bounded_fields().ok_or_else(incompatible)?;
				fields.work(label.len() as u64)?;
				fields.admit(
					label
						.len()
						.checked_mul(2 * std::mem::size_of::<Value>())
						.ok_or_else(incompatible)?,
				)?;
				Ok(Value::Array(
					label
						.iter()
						.map(|value| value.map_or(Value::None, Value::from))
						.collect::<Vec<_>>()
						.into(),
				))
			}
		}
	}
}

/// Projected aliases move past the padded input and class discriminator.
pub(super) fn merge_order<Read: QueryRead>(
	read: &Read,
	plans: &[&LogicalSelect],
	index: usize,
	class_column: usize,
	slots: &[Value],
	groups: &[&[TypedExpr]],
) -> FerrosGrynnResult<TypedExpr> {
	let first = plans.first().ok_or_else(incompatible)?;
	let fields = read.bounded_fields().ok_or_else(incompatible)?;
	fields.admit(
		plans
			.len()
			.checked_mul(std::mem::size_of::<&TypedExpr>())
			.ok_or_else(|| FerrosGrynnError::operation_overflow("LIVE union ORDER variants"))?,
	)?;
	fields.work(plans.len() as u64)?;
	let variants = plans
		.iter()
		.map(|input| input.order.get(index).map(|order| &order.expression).ok_or_else(incompatible))
		.collect::<FerrosGrynnResult<Vec<_>>>()?;
	let expression = variants.first().ok_or_else(incompatible)?;
	if let TypedExprKind::Column(column) = expression.kind
		&& let Some(relative) = column.checked_sub(first.input_schema.columns.len())
	{
		if !plans.iter().zip(&variants).all(|(input, expression)| {
			matches!(expression.kind, TypedExprKind::Column(column) if column.checked_sub(input.input_schema.columns.len()) == Some(relative))
		}) {
			return Err(incompatible());
		}
		admission::expression_copy(read, expression, 0)?;
		let mut expression = (*expression).clone();
		expression.kind = TypedExprKind::Column(
			class_column
				.checked_add(1)
				.and_then(|column| column.checked_add(relative))
				.ok_or_else(|| FerrosGrynnError::operation_overflow("LIVE union ORDER alias"))?,
		);
		Ok(expression)
	} else {
		merge_expression(read, &variants, class_column, slots, groups)
	}
}

/// Selects the original row expression without crossing class layouts.
pub(super) fn row_expression<Read: QueryRead>(
	read: &Read,
	variants: &[&TypedExpr],
	class_column: usize,
) -> FerrosGrynnResult<TypedExpr> {
	row_expression_selected(read, variants, RowSelector::Class(class_column))
}

/// Keeps each original row plan inside a lazy, declaration-owned CASE branch.
pub(super) fn row_expression_selected<Read: QueryRead>(
	read: &Read,
	variants: &[&TypedExpr],
	selector: RowSelector<'_>,
) -> FerrosGrynnResult<TypedExpr> {
	selector.validate(variants.len())?;
	let first = variants.first().ok_or_else(incompatible)?;
	let fields = read.bounded_fields().ok_or_else(incompatible)?;
	fields.work(variants.len() as u64)?;
	fields.admit(
		variants
			.len()
			.checked_add(2)
			.and_then(|count| count.checked_mul(2 * std::mem::size_of::<TypedExpr>()))
			.ok_or_else(|| FerrosGrynnError::operation_overflow("LIVE row expression variants"))?,
	)?;
	let mut branches = Vec::with_capacity(variants.len());
	for (ordinal, value) in variants.iter().enumerate() {
		admission::expression_copy(read, value, 0)?;
		let label = selector.label(read, ordinal)?;
		branches.push((
			TypedExpr {
				ty: Type::of(&label),
				kind: TypedExprKind::Constant(label),
				span: first.span,
			},
			(*value).clone(),
		));
	}
	Ok(TypedExpr {
		kind: TypedExprKind::Case {
			operand: Some(Box::new(selector.operand(read, first.span)?)),
			branches,
			else_value: None,
		},
		ty: Type::Any,
		span: first.span,
	})
}

/// Keeps each overload and column layout inside a lazy per-row CASE branch.
pub(super) fn merge_expression<Read: QueryRead>(
	read: &Read,
	variants: &[&TypedExpr],
	class_column: usize,
	slots: &[Value],
	groups: &[&[TypedExpr]],
) -> FerrosGrynnResult<TypedExpr> {
	merge_expression_selected(read, variants, RowSelector::Class(class_column), slots, groups)
}

/// Aggregate nodes reduce once; row arguments retain the original typing state.
pub(super) fn merge_expression_selected<Read: QueryRead>(
	read: &Read,
	variants: &[&TypedExpr],
	selector: RowSelector<'_>,
	slots: &[Value],
	groups: &[&[TypedExpr]],
) -> FerrosGrynnResult<TypedExpr> {
	selector.validate(variants.len())?;
	let first = variants.first().ok_or_else(incompatible)?;
	let fields = read.bounded_fields().ok_or_else(|| {
		FerrosGrynnError::invalid_argument("independent SELECT union requires bounded evaluation")
	})?;
	admit_special_copy(read, first)?;
	fields.work(variants.len() as u64)?;
	fields.admit(
		variants
			.len()
			.checked_mul(4 * std::mem::size_of::<TypedExpr>())
			.ok_or_else(|| FerrosGrynnError::operation_overflow("LIVE aggregate variants"))?,
	)?;
	if let Some(keys) = groups.first() {
		for ordinal in 0..keys.len() {
			let mut matches = groups.len() == variants.len();
			for (variant, keys) in variants.iter().zip(groups) {
				let Some(key) = keys.get(ordinal) else {
					return Err(incompatible());
				};
				for expression in [*variant, key] {
					crate::gql::analysis::visit_typed_expression(expression, &mut |_node| {
						fields.work(1)
					})?;
				}
				matches &= crate::gql::analysis::group_expr_equivalent(variant, key);
			}
			if matches {
				return row_expression_selected(read, variants, selector);
			}
		}
	}
	let children = |child: &dyn Fn(&TypedExprKind) -> Option<&TypedExpr>| {
		variants
			.iter()
			.map(|variant| child(&variant.kind).ok_or_else(incompatible))
			.collect::<FerrosGrynnResult<Vec<_>>>()
	};
	let merge_child = |child: &dyn Fn(&TypedExprKind) -> Option<&TypedExpr>| {
		merge_expression_selected(read, &children(child)?, selector, slots, groups).map(Box::new)
	};
	let kind = match &first.kind {
		TypedExprKind::Array(values) | TypedExprKind::Set(values) => {
			let mut merged = Vec::with_capacity(values.len());
			if !variants.iter().all(|other| match (&first.kind, &other.kind) {
				(TypedExprKind::Array(_), TypedExprKind::Array(other))
				| (TypedExprKind::Set(_), TypedExprKind::Set(other)) => other.len() == values.len(),
				_ => false,
			}) {
				return Err(incompatible());
			}
			for index in 0..values.len() {
				let inputs = children(&|kind| match kind {
					TypedExprKind::Array(values) | TypedExprKind::Set(values) => values.get(index),
					_ => None,
				})?;
				merged.push(merge_expression_selected(read, &inputs, selector, slots, groups)?);
			}
			if matches!(&first.kind, TypedExprKind::Array(_)) {
				TypedExprKind::Array(merged)
			} else {
				TypedExprKind::Set(merged)
			}
		}
		TypedExprKind::Object(values) => {
			let mut merged = Vec::with_capacity(values.len());
			if !variants.iter().all(
				|other| matches!(&other.kind, TypedExprKind::Object(other) if other.len() == values.len()),
			) {
				return Err(incompatible());
			}
			for (index, (name, _)) in values.iter().enumerate() {
				let inputs = children(&|kind| match kind {
					TypedExprKind::Object(values) => values
						.get(index)
						.and_then(|(other, value)| (name == other).then_some(value)),
					_ => None,
				})?;
				merged.push((
					name.clone(),
					merge_expression_selected(read, &inputs, selector, slots, groups)?,
				));
			}
			TypedExprKind::Object(merged)
		}
		TypedExprKind::Conditional {
			..
		} => TypedExprKind::Conditional {
			condition: merge_child(&|kind| match kind {
				TypedExprKind::Conditional {
					condition,
					..
				} => Some(condition),
				_ => None,
			})?,
			then_value: merge_child(&|kind| match kind {
				TypedExprKind::Conditional {
					then_value,
					..
				} => Some(then_value),
				_ => None,
			})?,
			else_value: merge_child(&|kind| match kind {
				TypedExprKind::Conditional {
					else_value,
					..
				} => Some(else_value),
				_ => None,
			})?,
		},
		TypedExprKind::Case {
			operand,
			branches,
			else_value,
		} => {
			if !variants.iter().all(|other| {
				matches!(&other.kind,
				TypedExprKind::Case { operand: other_operand, branches: other_branches, else_value: other_else }
				if operand.is_some() == other_operand.is_some()
				&& branches.len() == other_branches.len()
				&& else_value.is_some() == other_else.is_some())
			}) {
				return Err(incompatible());
			}
			let mut merged = Vec::with_capacity(branches.len());
			for index in 0..branches.len() {
				let conditions = children(&|kind| match kind {
					TypedExprKind::Case {
						branches,
						..
					} => branches.get(index).map(|(condition, _)| condition),
					_ => None,
				})?;
				let values = children(&|kind| match kind {
					TypedExprKind::Case {
						branches,
						..
					} => branches.get(index).map(|(_, value)| value),
					_ => None,
				})?;
				merged.push((
					merge_expression_selected(read, &conditions, selector, slots, groups)?,
					merge_expression_selected(read, &values, selector, slots, groups)?,
				));
			}
			TypedExprKind::Case {
				operand: operand
					.as_ref()
					.map(|_| {
						merge_child(&|kind| match kind {
							TypedExprKind::Case {
								operand,
								..
							} => operand.as_deref(),
							_ => None,
						})
					})
					.transpose()?,
				branches: merged,
				else_value: else_value
					.as_ref()
					.map(|_| {
						merge_child(&|kind| match kind {
							TypedExprKind::Case {
								else_value,
								..
							} => else_value.as_deref(),
							_ => None,
						})
					})
					.transpose()?,
			}
		}
		TypedExprKind::Field {
			field,
			null_safe,
			..
		} => {
			if !variants.iter().all(|other| {
				matches!(&other.kind,
				TypedExprKind::Field { field: other, null_safe: other_safe, .. }
				if field == other && null_safe == other_safe)
			}) {
				return Err(incompatible());
			}
			TypedExprKind::Field {
				field: field.clone(),
				null_safe: *null_safe,
				base: merge_child(&|kind| match kind {
					TypedExprKind::Field {
						base,
						..
					} => Some(base),
					_ => None,
				})?,
			}
		}
		TypedExprKind::Index {
			..
		} => TypedExprKind::Index {
			base: merge_child(&|kind| match kind {
				TypedExprKind::Index {
					base,
					..
				} => Some(base),
				_ => None,
			})?,
			index: merge_child(&|kind| match kind {
				TypedExprKind::Index {
					index,
					..
				} => Some(index),
				_ => None,
			})?,
		},
		TypedExprKind::Cast {
			target,
			..
		} => {
			if !variants.iter().all(
				|other| matches!(&other.kind, TypedExprKind::Cast { target: other, .. } if target == other),
			) {
				return Err(incompatible());
			}
			TypedExprKind::Cast {
				target: target.clone(),
				expr: merge_child(&|kind| match kind {
					TypedExprKind::Cast {
						expr,
						..
					} => Some(expr),
					_ => None,
				})?,
			}
		}
		TypedExprKind::Range {
			start,
			end,
			inclusive_end,
		} => {
			if !variants.iter().all(|other| {
				matches!(&other.kind,
				TypedExprKind::Range { start: other_start, end: other_end, inclusive_end: other_inclusive }
				if start.is_some() == other_start.is_some()
				&& end.is_some() == other_end.is_some() && inclusive_end == other_inclusive)
			}) {
				return Err(incompatible());
			}
			TypedExprKind::Range {
				start: start
					.as_ref()
					.map(|_| {
						merge_child(&|kind| match kind {
							TypedExprKind::Range {
								start,
								..
							} => start.as_deref(),
							_ => None,
						})
					})
					.transpose()?,
				end: end
					.as_ref()
					.map(|_| {
						merge_child(&|kind| match kind {
							TypedExprKind::Range {
								end,
								..
							} => end.as_deref(),
							_ => None,
						})
					})
					.transpose()?,
				inclusive_end: *inclusive_end,
			}
		}
		TypedExprKind::Aggregate {
			function,
			argument,
			distinct,
		} => {
			let mut arguments = Vec::with_capacity(variants.len());
			for variant in variants {
				let TypedExprKind::Aggregate {
					function: other,
					argument: value,
					distinct: other_distinct,
				} = &variant.kind
				else {
					return Err(incompatible());
				};
				if function != other
					|| distinct != other_distinct
					|| argument.is_some() != value.is_some()
				{
					return Err(incompatible());
				}
				if let Some(value) = value {
					arguments.push(value.as_ref());
				}
			}
			TypedExprKind::Aggregate {
				function: *function,
				distinct: *distinct,
				argument: argument
					.as_ref()
					.map(|_| row_expression_selected(read, &arguments, selector).map(Box::new))
					.transpose()?,
			}
		}
		TypedExprKind::Constant(value) => {
			if !variants.iter().all(|other| matches!(&other.kind, TypedExprKind::Constant(other) if crate::values::runtime::same_value(value, other))) {
				return Err(incompatible());
			}
			first.kind.clone()
		}
		TypedExprKind::Slot(slot) => {
			let value = slots.get(*slot as usize).ok_or_else(incompatible)?;
			if !variants.iter().all(|other| matches!(other.kind, TypedExprKind::Slot(other) if slots.get(other as usize).is_some_and(|other| crate::values::runtime::same_value(value, other)))) {
				return Err(incompatible());
			}
			first.kind.clone()
		}
		TypedExprKind::Unary {
			op,
			..
		} => {
			if !variants.iter().all(
				|other| matches!(&other.kind, TypedExprKind::Unary { op: other, .. } if op == other),
			) {
				return Err(incompatible());
			}
			TypedExprKind::Unary {
				op: *op,
				expr: merge_child(&|kind| {
					if let TypedExprKind::Unary {
						expr,
						..
					} = kind
					{
						Some(expr)
					} else {
						None
					}
				})?,
			}
		}
		TypedExprKind::Binary {
			op,
			..
		} => {
			if !variants.iter().all(
				|other| matches!(&other.kind, TypedExprKind::Binary { op: other, .. } if op == other),
			) {
				return Err(incompatible());
			}
			TypedExprKind::Binary {
				op: *op,
				left: merge_child(&|kind| {
					if let TypedExprKind::Binary {
						left,
						..
					} = kind
					{
						Some(left)
					} else {
						None
					}
				})?,
				right: merge_child(&|kind| {
					if let TypedExprKind::Binary {
						right,
						..
					} = kind
					{
						Some(right)
					} else {
						None
					}
				})?,
			}
		}
		TypedExprKind::Builtin {
			arguments,
			..
		}
		| TypedExprKind::NamedInvoke {
			arguments,
			..
		} => {
			let mut merged = Vec::new();
			for index in 0..arguments.len() {
				let inputs = children(&|kind| match (&first.kind, kind) {
					(
						TypedExprKind::Builtin {
							function,
							..
						},
						TypedExprKind::Builtin {
							function: other,
							arguments,
						},
					) if function == other
						&& arguments.len()
							== match &first.kind {
								TypedExprKind::Builtin {
									arguments,
									..
								} => arguments.len(),
								_ => 0,
							} =>
					{
						arguments.get(index)
					}
					(
						TypedExprKind::NamedInvoke {
							function,
							..
						},
						TypedExprKind::NamedInvoke {
							function: other,
							arguments,
						},
					) if function == other
						&& arguments.len()
							== match &first.kind {
								TypedExprKind::NamedInvoke {
									arguments,
									..
								} => arguments.len(),
								_ => 0,
							} =>
					{
						arguments.get(index)
					}
					_ => None,
				})?;
				merged.push(merge_expression_selected(read, &inputs, selector, slots, groups)?);
			}
			match &first.kind {
				TypedExprKind::Builtin {
					function,
					..
				} => TypedExprKind::Builtin {
					function: function.clone(),
					arguments: merged,
				},
				TypedExprKind::NamedInvoke {
					function,
					..
				} => TypedExprKind::NamedInvoke {
					function: *function,
					arguments: merged,
				},
				_ => return Err(incompatible()),
			}
		}
		TypedExprKind::Subquery {
			select,
			collection,
		} => {
			if select.lexical_id.is_none()
				|| !variants.iter().all(|other| {
					matches!(&other.kind,
				TypedExprKind::Subquery { select: other, collection: other_collection }
				if select.lexical_id == other.lexical_id && collection == other_collection)
				}) {
				return Err(incompatible());
			}
			admission::select_copy(read, select, 0)?;
			first.kind.clone()
		}
		_ => {
			return Err(FerrosGrynnError::query_feature_not_supported(
				"LIVE graph aggregate outer expression",
			));
		}
	};
	Ok(TypedExpr {
		kind,
		ty: first.ty.clone(),
		span: first.span,
	})
}

/// Differing outer overloads cannot be evaluated using another class's plan.
fn incompatible() -> FerrosGrynnError {
	FerrosGrynnError::invalid_argument(
		"LIVE graph aggregate has incompatible outer typed expressions",
	)
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::{datastore::Datastore, gql::QueryExecutionOptions};

	/// Missing LEFT states stay distinct; one aggregate reduces selected row
	/// arguments instead of selecting among already reduced groups.
	#[test]
	fn live_join_row_selector_preserves_none_and_aggregate_domain() -> FerrosGrynnResult<()> {
		let store = Datastore::in_memory();
		let root = store.inner.current_root();
		let control = ExecutionControl::new(&QueryExecutionOptions::default(), None);
		let read = RootRead::new(&root, &control, &|_| {}, &|_| Ok(()), std::env::temp_dir());
		let span = crate::gql_status::TextSpan::new(0, 0);
		let constant = |value: Value| TypedExpr {
			ty: Type::of(&value),
			kind: TypedExprKind::Constant(value),
			span,
		};
		let values = [constant(10.into()), constant(Value::None), constant(20.into())];
		let variants = values.iter().collect::<Vec<_>>();
		let columns = [0, 1];
		let labels = vec![vec![Some(0), Some(1)], vec![None, Some(1)], vec![Some(1), None]];
		let selector = RowSelector::Joined {
			columns: &columns,
			labels: &labels,
		};
		let selected = row_expression_selected(&read, &variants, selector)?;
		let rows = labels
			.iter()
			.map(|label| {
				Row::new(label.iter().map(|value| value.map_or(Value::None, Value::from)).collect())
			})
			.collect::<Vec<_>>();
		let snapshot = read.snapshot()?;
		for (row, value) in rows.iter().zip(&values) {
			assert_eq!(
				eval_select_expr(&read, snapshot, &selected, row, None, &[], "", &control)?,
				match &value.kind {
					TypedExprKind::Constant(value) => value.clone(),
					_ => Value::None,
				}
			);
		}
		let sums = values
			.iter()
			.map(|value| TypedExpr {
				kind: TypedExprKind::Aggregate {
					function: crate::gql::ir::Aggregate::Sum,
					argument: Some(Box::new(value.clone())),
					distinct: false,
				},
				ty: Type::Number,
				span,
			})
			.collect::<Vec<_>>();
		let sum =
			merge_expression_selected(&read, &sums.iter().collect::<Vec<_>>(), selector, &[], &[])?;
		assert!(matches!(&sum.kind, TypedExprKind::Aggregate { argument: Some(argument), .. }
			if matches!(argument.kind, TypedExprKind::Case { .. })));
		let row = rows[0].clone();
		let group = RowSet::Memory {
			rows,
			_account: BarrierAccount::new(4096),
		};
		assert_eq!(
			eval_select_expr(&read, snapshot, &sum, &row, Some(&group), &[], "", &control)?,
			Value::from(30)
		);
		let bad_labels = vec![vec![Some(0)]];
		assert!(
			row_expression_selected(&read, &variants, RowSelector::Joined {
				columns: &columns,
				labels: &bad_labels,
			})
			.is_err()
		);
		let ordinal = row_expression(&read, &variants, 0)?;
		assert_eq!(
			eval_select_expr(
				&read,
				snapshot,
				&ordinal,
				&Row::new(vec![2.into()]),
				None,
				&[],
				"",
				&control
			)?,
			Value::from(20)
		);
		Ok(())
	}
}
