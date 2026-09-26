//! Validation of the strict deterministic execution profile.

use crate::{
	gql::ir::{
		Builtin,
		LogicalBlock,
		LogicalDmlData,
		LogicalDmlReturn,
		LogicalDmlReturnItem,
		LogicalDmlSelector,
		LogicalDmlStatement,
		LogicalExpect,
		LogicalGraphDmlStatement,
		LogicalGraphPropertyMutation,
		LogicalGraphReference,
		LogicalMatch,
		LogicalMatchSelector,
		LogicalSchemaStatement,
		LogicalSelect,
		LogicalSelectItem,
		LogicalSelectSource,
		LogicalSelectSourceKind,
		LogicalSelectStart,
		LogicalStatement,
		TypedExpr,
		TypedExprKind,
	},
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
};

/// Rejects impure expressions before the executor pulls the first input row.
pub(in crate::gql) fn validate_deterministic_statement(
	statement: &LogicalStatement,
) -> FerrosGrynnResult<()> {
	if statement_contains_impure(statement) {
		return Err(FerrosGrynnError::query_feature_not_supported(
			"impure functions in EXECUTION MODE DETERMINISTIC",
		));
	}
	Ok(())
}

fn statement_contains_impure(statement: &LogicalStatement) -> bool {
	match statement {
		LogicalStatement::Explain(explain) => statement_contains_impure(&explain.statement),
		LogicalStatement::Schema(statement) => schema_contains_impure(statement),
		LogicalStatement::Dml(statement) => dml_contains_impure(statement),
		LogicalStatement::GraphDml(statement) => graph_dml_contains_impure(statement),
		LogicalStatement::Let {
			value,
			..
		}
		| LogicalStatement::Return {
			value,
		}
		| LogicalStatement::Expression {
			value,
		}
		| LogicalStatement::Throw {
			value,
		} => expr_contains_impure(value),
		LogicalStatement::If {
			branches,
			else_block,
		} => {
			branches.iter().any(|(condition, block)| {
				expr_contains_impure(condition) || block_contains_impure(block)
			}) || else_block.as_ref().is_some_and(block_contains_impure)
		}
		LogicalStatement::For {
			iterable,
			body,
			..
		} => expr_contains_impure(iterable) || block_contains_impure(body),
		LogicalStatement::While {
			condition,
			body,
		} => expr_contains_impure(condition) || block_contains_impure(body),
		LogicalStatement::Try {
			body,
			catch,
		} => {
			block_contains_impure(body)
				|| catch.as_ref().is_some_and(|(_slot, block)| block_contains_impure(block))
		}
		LogicalStatement::Match(statement) => match_contains_impure(statement),
		LogicalStatement::Select(statement) => select_contains_impure(statement),
		LogicalStatement::Break
		| LogicalStatement::Continue
		| LogicalStatement::AssertInvariants {
			..
		} => false,
	}
}

fn block_contains_impure(block: &LogicalBlock) -> bool {
	block.statements.iter().any(|statement| statement_contains_impure(&statement.statement))
}

fn schema_contains_impure(statement: &LogicalSchemaStatement) -> bool {
	match statement {
		LogicalSchemaStatement::DefineGraph {
			content,
			..
		} => content.as_ref().is_some_and(expr_contains_impure),
		LogicalSchemaStatement::UpdateGraph {
			mutation,
			returning,
			..
		} => graph_property_mutation_contains_impure(mutation) || return_contains_impure(returning),
		LogicalSchemaStatement::DefineClass {
			..
		}
		| LogicalSchemaStatement::DefineField {
			..
		}
		| LogicalSchemaStatement::DefineIndex {
			..
		}
		| LogicalSchemaStatement::RemoveGraph {
			..
		}
		| LogicalSchemaStatement::RemoveClass {
			..
		}
		| LogicalSchemaStatement::RemoveField {
			..
		}
		| LogicalSchemaStatement::RemoveIndex {
			..
		}
		| LogicalSchemaStatement::AlterClassMode {
			..
		}
		| LogicalSchemaStatement::AlterFieldType {
			..
		}
		| LogicalSchemaStatement::PrepareIndexRemoval {
			..
		}
		| LogicalSchemaStatement::InfoDatabase
		| LogicalSchemaStatement::InfoClass {
			..
		}
		| LogicalSchemaStatement::InfoIndex {
			..
		} => false,
	}
}

fn graph_property_mutation_contains_impure(mutation: &LogicalGraphPropertyMutation) -> bool {
	match mutation {
		LogicalGraphPropertyMutation::Set(values) => {
			values.iter().any(|(_path, value)| expr_contains_impure(value))
		}
		LogicalGraphPropertyMutation::Unset(_) => false,
	}
}

fn dml_contains_impure(statement: &LogicalDmlStatement) -> bool {
	statement.targets.iter().any(|target| {
		selector_contains_impure(&target.selector)
			|| target.data.as_ref().is_some_and(data_contains_impure)
			|| target.filter.as_ref().is_some_and(expr_contains_impure)
			|| return_contains_impure(&target.returning)
			|| target.expect.as_ref().is_some_and(expect_contains_impure)
	})
}

fn selector_contains_impure(selector: &LogicalDmlSelector) -> bool {
	match selector {
		LogicalDmlSelector::Record(value) => expr_contains_impure(value),
		LogicalDmlSelector::Class | LogicalDmlSelector::Range(_) => false,
	}
}

fn data_contains_impure(data: &LogicalDmlData) -> bool {
	match data {
		LogicalDmlData::Content(value)
		| LogicalDmlData::Merge(value)
		| LogicalDmlData::Patch(value)
		| LogicalDmlData::Replace(value) => expr_contains_impure(value),
		LogicalDmlData::Set(values) => {
			values.iter().any(|(_name, value)| expr_contains_impure(value))
		}
		LogicalDmlData::Unset(_) | LogicalDmlData::Reset(_) => false,
	}
}

fn return_contains_impure(returning: &LogicalDmlReturn) -> bool {
	match returning {
		LogicalDmlReturn::Fields(values) => values.iter().any(|value| match value {
			LogicalDmlReturnItem::Expr {
				value,
				..
			} => expr_contains_impure(value),
			LogicalDmlReturnItem::Wildcard => false,
		}),
		LogicalDmlReturn::Value(value) => expr_contains_impure(value),
		LogicalDmlReturn::None
		| LogicalDmlReturn::Before
		| LogicalDmlReturn::After
		| LogicalDmlReturn::Diff => false,
	}
}

fn expect_contains_impure(expect: &LogicalExpect) -> bool {
	matches!(expect, LogicalExpect::Predicate(value) if expr_contains_impure(value))
}

fn graph_dml_contains_impure(statement: &LogicalGraphDmlStatement) -> bool {
	match statement {
		LogicalGraphDmlStatement::Attach {
			record,
			..
		}
		| LogicalGraphDmlStatement::Detach {
			record,
			..
		} => expr_contains_impure(&record.key),
		LogicalGraphDmlStatement::Relate {
			source,
			edge,
			target,
			weight,
			..
		} => {
			expr_contains_impure(&source.key)
				|| expr_contains_impure(&target.key)
				|| edge.key.as_ref().is_some_and(expr_contains_impure)
				|| edge.data.as_ref().is_some_and(data_contains_impure)
				|| return_contains_impure(&edge.returning)
				|| edge.expect.as_ref().is_some_and(expect_contains_impure)
				|| weight.as_ref().is_some_and(expr_contains_impure)
		}
	}
}

fn match_contains_impure(statement: &LogicalMatch) -> bool {
	statement.patterns.iter().any(|pattern| {
		binding_contains_impure(&pattern.start)
			|| pattern.ops.iter().any(|operation| match operation {
				crate::gql::ir::MatchOp::Expand(expand) => {
					binding_contains_impure(&expand.source)
						|| binding_contains_impure(&expand.edge)
						|| binding_contains_impure(&expand.target)
				}
				crate::gql::ir::MatchOp::PathExpand(expand) => {
					binding_contains_impure(&expand.source)
						|| binding_contains_impure(&expand.edge)
						|| binding_contains_impure(&expand.target)
				}
			})
	}) || statement.filter.as_ref().is_some_and(expr_contains_impure)
		|| statement.projection.iter().any(expr_contains_impure)
}

fn binding_contains_impure(binding: &crate::gql::ir::MatchBinding) -> bool {
	match &binding.selector {
		LogicalMatchSelector::Record(record) => expr_contains_impure(&record.key),
		LogicalMatchSelector::Any | LogicalMatchSelector::Class(_) => false,
	}
}

fn select_contains_impure(statement: &LogicalSelect) -> bool {
	source_contains_impure(&statement.source)
		|| statement.joins.iter().any(|join| {
			source_contains_impure(&join.source)
				|| join.predicate.as_ref().is_some_and(expr_contains_impure)
		})
		|| statement.filter.as_ref().is_some_and(expr_contains_impure)
		|| statement.split.iter().any(expr_contains_impure)
		|| statement.group.as_ref().is_some_and(|values| values.iter().any(expr_contains_impure))
		|| statement.projection.iter().any(|item| match item {
			LogicalSelectItem::Expression {
				expression,
				..
			} => expr_contains_impure(expression),
			LogicalSelectItem::Wildcard(_) => false,
		})
		|| statement.order.iter().any(|order| expr_contains_impure(&order.expression))
		|| statement.start.as_ref().is_some_and(|start| match start {
			LogicalSelectStart::Offset(value)
			| LogicalSelectStart::After {
				cursor: value,
				..
			} => expr_contains_impure(value),
		})
		|| statement.limit.as_ref().is_some_and(expr_contains_impure)
}

fn source_contains_impure(source: &LogicalSelectSource) -> bool {
	match &source.kind {
		LogicalSelectSourceKind::Atoms {
			graph,
			..
		} => graph.as_ref().is_some_and(|graph| match graph {
			LogicalGraphReference::Static(_) => false,
			LogicalGraphReference::Expression(value) => expr_contains_impure(value),
		}),
		LogicalSelectSourceKind::Graphs {
			references,
		} => references.as_ref().is_some_and(|references| {
			references.iter().any(|graph| match graph {
				LogicalGraphReference::Static(_) => false,
				LogicalGraphReference::Expression(value) => expr_contains_impure(value),
			})
		}),
		LogicalSelectSourceKind::Derived(select) => select_contains_impure(select),
	}
}

fn expr_contains_impure(expression: &TypedExpr) -> bool {
	match &expression.kind {
		TypedExprKind::Builtin {
			function: Builtin::TimeNow(_),
			..
		} => true,
		TypedExprKind::Array(values) | TypedExprKind::Set(values) => {
			values.iter().any(expr_contains_impure)
		}
		TypedExprKind::Object(fields) => {
			fields.iter().any(|(_name, value)| expr_contains_impure(value))
		}
		TypedExprKind::Range {
			start,
			end,
			..
		} => {
			start.as_deref().is_some_and(expr_contains_impure)
				|| end.as_deref().is_some_and(expr_contains_impure)
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
		} => expr_contains_impure(expr),
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
		} => expr_contains_impure(left) || expr_contains_impure(right),
		TypedExprKind::Conditional {
			condition,
			then_value,
			else_value,
		} => {
			expr_contains_impure(condition)
				|| expr_contains_impure(then_value)
				|| expr_contains_impure(else_value)
		}
		TypedExprKind::Case {
			operand,
			branches,
			else_value,
		} => {
			operand.as_deref().is_some_and(expr_contains_impure)
				|| branches.iter().any(|(condition, value)| {
					expr_contains_impure(condition) || expr_contains_impure(value)
				})
				|| else_value.as_deref().is_some_and(expr_contains_impure)
		}
		TypedExprKind::Builtin {
			arguments,
			..
		} => arguments.iter().any(expr_contains_impure),
		TypedExprKind::Aggregate {
			argument,
			..
		} => argument.as_deref().is_some_and(expr_contains_impure),
		TypedExprKind::Subquery {
			select,
			..
		} => select_contains_impure(select),
		TypedExprKind::Constant(_)
		| TypedExprKind::Slot(_)
		| TypedExprKind::Column(_)
		| TypedExprKind::CurrentField(_) => false,
	}
}
