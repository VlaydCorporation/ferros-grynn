//! Canonical logical rewrites applied before physical alternative generation.

use super::{AppliedRule, PhysicalSelectPlan, PlannerSnapshot, plan_select};
use crate::{
	gql::ir::{
		Builtin,
		LogicalGraphReference,
		LogicalSelect,
		LogicalSelectItem,
		LogicalSelectSource,
		LogicalSelectSourceKind,
		LogicalSelectStart,
		TypedExpr,
		TypedExprKind,
	},
	gql_status::FerrosGrynnResult,
	syn::BinaryOperator,
};

/// Applies canonical logical rules and selects one physical plan.
pub(in crate::gql) fn prepare_select(
	logical: &LogicalSelect,
	snapshot: &PlannerSnapshot,
	barrier_memory_bytes: usize,
) -> FerrosGrynnResult<(LogicalSelect, PhysicalSelectPlan)> {
	let mut rewritten = logical.clone();
	let folded = fold_select(&mut rewritten);
	let conjunction = rewritten.filter.as_ref().is_some_and(contains_conjunction)
		|| rewritten
			.joins
			.iter()
			.filter_map(|join| join.predicate.as_ref())
			.any(contains_conjunction);
	let mut physical = plan_select(&rewritten, snapshot, barrier_memory_bytes)?;
	if conjunction {
		physical.rules.insert(0, AppliedRule::ConjunctionDecomposition);
	}
	if folded {
		physical.rules.insert(0, AppliedRule::ConstantFolding);
	}
	Ok((rewritten, physical))
}

fn fold_select(select: &mut LogicalSelect) -> bool {
	let mut changed = fold_source(&mut select.source);
	for join in &mut select.joins {
		changed |= fold_source(&mut join.source);
		if let Some(predicate) = &mut join.predicate {
			changed |= fold_expr(predicate);
		}
	}
	if let Some(filter) = &mut select.filter {
		changed |= fold_expr(filter);
	}
	for expression in &mut select.split {
		changed |= fold_expr(expression);
	}
	if let Some(group) = &mut select.group {
		for expression in group {
			changed |= fold_expr(expression);
		}
	}
	for projection in &mut select.projection {
		if let LogicalSelectItem::Expression {
			expression,
			..
		} = projection
		{
			changed |= fold_expr(expression);
		}
	}
	for order in &mut select.order {
		if !order.random {
			changed |= fold_expr(&mut order.expression);
		}
	}
	if let Some(start) = &mut select.start {
		changed |= match start {
			LogicalSelectStart::Offset(value)
			| LogicalSelectStart::After {
				cursor: value,
				..
			} => fold_expr(value),
		};
	}
	if let Some(limit) = &mut select.limit {
		changed |= fold_expr(limit);
	}
	changed
}

fn fold_source(source: &mut LogicalSelectSource) -> bool {
	match &mut source.kind {
		LogicalSelectSourceKind::Atoms {
			graph: Some(LogicalGraphReference::Expression(expression)),
			..
		} => fold_expr(expression),
		LogicalSelectSourceKind::Graphs {
			references: Some(references),
		} => references.iter_mut().fold(false, |changed, reference| {
			changed
				| match reference {
					LogicalGraphReference::Expression(expression) => fold_expr(expression),
					LogicalGraphReference::Static(_) => false,
				}
		}),
		LogicalSelectSourceKind::Derived(select) => fold_select(select),
		LogicalSelectSourceKind::Atoms {
			..
		}
		| LogicalSelectSourceKind::Graphs {
			..
		} => false,
	}
}

fn fold_expr(expression: &mut TypedExpr) -> bool {
	let mut changed = match &mut expression.kind {
		TypedExprKind::Array(values) | TypedExprKind::Set(values) => {
			values.iter_mut().fold(false, |changed, value| changed | fold_expr(value))
		}
		TypedExprKind::Object(fields) => {
			fields.iter_mut().fold(false, |changed, (_name, value)| changed | fold_expr(value))
		}
		TypedExprKind::Range {
			start,
			end,
			..
		} => start.as_deref_mut().is_some_and(fold_expr) | end.as_deref_mut().is_some_and(fold_expr),
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
		} => fold_expr(expr),
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
		} => fold_expr(left) | fold_expr(right),
		TypedExprKind::Conditional {
			condition,
			then_value,
			else_value,
		} => fold_expr(condition) | fold_expr(then_value) | fold_expr(else_value),
		TypedExprKind::Case {
			operand,
			branches,
			else_value,
		} => {
			let mut changed = operand.as_deref_mut().is_some_and(fold_expr);
			for (condition, value) in branches {
				changed |= fold_expr(condition) | fold_expr(value);
			}
			changed | else_value.as_deref_mut().is_some_and(fold_expr)
		}
		TypedExprKind::Builtin {
			arguments,
			..
		} => arguments.iter_mut().fold(false, |changed, argument| changed | fold_expr(argument)),
		TypedExprKind::Aggregate {
			argument,
			..
		} => argument.as_deref_mut().is_some_and(fold_expr),
		TypedExprKind::Subquery {
			select,
			..
		} => fold_select(select),
		TypedExprKind::Constant(_)
		| TypedExprKind::Slot(_)
		| TypedExprKind::Column(_)
		| TypedExprKind::CurrentField(_) => false,
	};
	if !matches!(expression.kind, TypedExprKind::Constant(_))
		&& constant_expression(expression)
		&& let Some(value) = crate::gql::execution::evaluate_constant(expression)
	{
		expression.kind = TypedExprKind::Constant(value);
		changed = true;
	}
	changed
}

fn constant_expression(expression: &TypedExpr) -> bool {
	match &expression.kind {
		TypedExprKind::Constant(_) => true,
		TypedExprKind::Array(values) | TypedExprKind::Set(values) => {
			values.iter().all(constant_expression)
		}
		TypedExprKind::Object(fields) => {
			fields.iter().all(|(_name, value)| constant_expression(value))
		}
		TypedExprKind::Range {
			start,
			end,
			..
		} => {
			start.as_deref().is_none_or(constant_expression)
				&& end.as_deref().is_none_or(constant_expression)
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
		} => constant_expression(expr),
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
		} => constant_expression(left) && constant_expression(right),
		TypedExprKind::Conditional {
			condition,
			then_value,
			else_value,
		} => {
			constant_expression(condition)
				&& constant_expression(then_value)
				&& constant_expression(else_value)
		}
		TypedExprKind::Case {
			operand,
			branches,
			else_value,
		} => {
			operand.as_deref().is_none_or(constant_expression)
				&& branches.iter().all(|(condition, value)| {
					constant_expression(condition) && constant_expression(value)
				})
				&& else_value.as_deref().is_none_or(constant_expression)
		}
		TypedExprKind::Builtin {
			function,
			arguments,
		} => !matches!(function, Builtin::TimeNow(_)) && arguments.iter().all(constant_expression),
		TypedExprKind::Slot(_)
		| TypedExprKind::Column(_)
		| TypedExprKind::CurrentField(_)
		| TypedExprKind::Aggregate {
			..
		}
		| TypedExprKind::Subquery {
			..
		} => false,
	}
}

fn contains_conjunction(expression: &TypedExpr) -> bool {
	match &expression.kind {
		TypedExprKind::Binary {
			op: BinaryOperator::And,
			..
		} => true,
		TypedExprKind::Binary {
			left,
			right,
			..
		} => contains_conjunction(left) || contains_conjunction(right),
		_ => false,
	}
}
