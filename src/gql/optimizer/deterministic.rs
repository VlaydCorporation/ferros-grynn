//! Validation of the strict deterministic execution profile.

use crate::{
	gql::ir::{
		Builtin,
		LogicalBlock,
		LogicalDmlData,
		LogicalDmlReturn,
		LogicalDmlSelector,
		LogicalDmlStatement,
		LogicalExpect,
		LogicalGraphDmlStatement,
		LogicalGraphPropertyMutation,
		LogicalGraphReference,
		LogicalKnnDistance,
		LogicalLetValue,
		LogicalMatch,
		LogicalMatchSelector,
		LogicalSchemaStatement,
		LogicalSelect,
		LogicalSelectItem,
		LogicalSelectSource,
		LogicalSelectSourceKind,
		LogicalSelectStart,
		LogicalStatement,
		LogicalTraverseSource,
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

/// Returns whether the statement contains impure condition holds.
fn statement_contains_impure(statement: &LogicalStatement) -> bool {
	match statement {
		LogicalStatement::Explain(explain) => statement_contains_impure(&explain.statement),
		LogicalStatement::Schema(statement) => schema_contains_impure(statement),
		LogicalStatement::Dml(statement) => dml_contains_impure(statement),
		LogicalStatement::GraphDml(statement) => graph_dml_contains_impure(statement),
		LogicalStatement::ReturnGraph(graph) => return_graph_contains_impure(graph),
		LogicalStatement::BuildGraph(build) => build_graph_contains_impure(build),
		LogicalStatement::Let {
			value,
			..
		} => match value {
			LogicalLetValue::Live(statement) => schema_contains_impure(statement),
			LogicalLetValue::Expression(value) => expr_contains_impure(value),
			LogicalLetValue::BuildGraph(build) => build_graph_contains_impure(build),
		},
		LogicalStatement::SetBinding {
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
		LogicalStatement::Sleep(_) => true,
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
		LogicalStatement::Traverse(statement) => {
			let source = match &statement.source {
				LogicalTraverseSource::Expression(expression) => expr_contains_impure(expression),
				LogicalTraverseSource::Match(statement) => match_contains_impure(statement),
				LogicalTraverseSource::Select(statement) => select_contains_impure(statement),
			};
			source
				|| statement.steps.iter().any(|step| {
					step.match_plan.as_deref().is_some_and(match_contains_impure)
						|| step.named_function.is_some_and(|function| {
							matches!(function.effect, crate::datastore::FunctionEffect::Impure)
						})
						|| step.arguments.iter().any(expr_contains_impure)
						|| step.repeat.as_ref().is_some_and(|repeat| {
							expr_contains_impure(&repeat.body)
								|| repeat.times.as_ref().is_some_and(expr_contains_impure)
								|| repeat.until.as_ref().is_some_and(expr_contains_impure)
						})
				})
				|| statement.returning.as_ref().is_some_and(expr_contains_impure)
				|| statement.return_graph.as_ref().is_some_and(return_graph_contains_impure)
		}
		LogicalStatement::Break
		| LogicalStatement::Continue
		| LogicalStatement::AssertInvariants {
			..
		} => false,
	}
}

/// Builds graph contains impure.
fn build_graph_contains_impure(build: &crate::gql::ir::LogicalBuildGraph) -> bool {
	let source = match &build.source {
		crate::gql::ir::LogicalBuildGraphSource::Match(statement) => {
			match_contains_impure(statement)
		}
		crate::gql::ir::LogicalBuildGraphSource::Select(statement) => {
			select_contains_impure(statement)
		}
		crate::gql::ir::LogicalBuildGraphSource::Traverse(statement) => {
			statement.returning.as_ref().is_some_and(expr_contains_impure)
				|| statement.return_graph.as_ref().is_some_and(return_graph_contains_impure)
				|| statement
					.steps
					.iter()
					.any(|step| step.arguments.iter().any(expr_contains_impure))
		}
		crate::gql::ir::LogicalBuildGraphSource::Expression(expression) => {
			expr_contains_impure(expression)
		}
	};
	source
		|| build.root.as_ref().is_some_and(expr_contains_impure)
		|| build.nodes.as_ref().is_some_and(expr_contains_impure)
		|| build.edges.as_ref().is_some_and(expr_contains_impure)
}

/// Returns whether the return graph contains impure condition holds.
fn return_graph_contains_impure(graph: &crate::gql::ir::LogicalReturnGraph) -> bool {
	graph.root.as_ref().is_some_and(expr_contains_impure)
		|| expr_contains_impure(&graph.nodes)
		|| expr_contains_impure(&graph.edges)
}

/// Returns whether the block contains impure condition holds.
fn block_contains_impure(block: &LogicalBlock) -> bool {
	block.statements.iter().any(|statement| statement_contains_impure(&statement.statement))
}

/// Returns whether the schema contains impure condition holds.
fn schema_contains_impure(statement: &LogicalSchemaStatement) -> bool {
	match statement {
		LogicalSchemaStatement::RegisterLive(plan) => {
			plan.frozen_graphs.iter().any(|capture| expr_contains_impure(&capture.graph))
				|| plan.since.as_ref().is_some_and(expr_contains_impure)
				|| plan.buffer.as_ref().is_some_and(expr_contains_impure)
		}
		LogicalSchemaStatement::DefineLiveBinding(_) | LogicalSchemaStatement::Event(_) => false,
		LogicalSchemaStatement::ReplaceLiveBinding {
			id,
			..
		}
		| LogicalSchemaStatement::InfoLiveBinding {
			id,
		}
		| LogicalSchemaStatement::RemoveLiveBinding {
			id,
		} => expr_contains_impure(id),
		LogicalSchemaStatement::AckLive {
			id,
			cursor,
			..
		} => expr_contains_impure(id) || expr_contains_impure(cursor),
		LogicalSchemaStatement::DefineGraph {
			root,
			content,
			..
		} => {
			root.as_ref().is_some_and(expr_contains_impure)
				|| content.as_ref().is_some_and(expr_contains_impure)
		}
		LogicalSchemaStatement::InfoChangefeed {
			id,
			..
		}
		| LogicalSchemaStatement::InfoLive {
			id,
		}
		| LogicalSchemaStatement::CloseLive {
			id,
		} => expr_contains_impure(id),
		LogicalSchemaStatement::ShowChanges {
			target,
			since,
			limit,
			..
		} => {
			let target = match target {
				crate::gql::ir::LogicalChangefeedTarget::Named(_) => false,
				crate::gql::ir::LogicalChangefeedTarget::Graph(value)
				| crate::gql::ir::LogicalChangefeedTarget::Id(value) => expr_contains_impure(value),
			};
			target
				|| expr_contains_impure(since)
				|| limit.as_ref().is_some_and(expr_contains_impure)
		}
		LogicalSchemaStatement::UpdateGraph {
			mutation,
			returning,
			..
		} => graph_property_mutation_contains_impure(mutation) || return_contains_impure(returning),
		LogicalSchemaStatement::DefineParameter {
			value,
			..
		} => expr_contains_impure(value),
		LogicalSchemaStatement::AlterParameter {
			value,
			..
		} => value.as_ref().is_some_and(expr_contains_impure),
		LogicalSchemaStatement::DefineFunction {
			..
		}
		| LogicalSchemaStatement::AlterFunction {
			..
		}
		| LogicalSchemaStatement::DefineClass {
			..
		}
		| LogicalSchemaStatement::DefineField {
			..
		}
		| LogicalSchemaStatement::DefineIndex {
			..
		}
		| LogicalSchemaStatement::DefineGraphIndex {
			..
		}
		| LogicalSchemaStatement::DefineAnalyzer {
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
		| LogicalSchemaStatement::RemoveGraphIndex {
			..
		}
		| LogicalSchemaStatement::RemoveParameter {
			..
		}
		| LogicalSchemaStatement::RemoveFunction {
			..
		}
		| LogicalSchemaStatement::RemoveAnalyzer {
			..
		}
		| LogicalSchemaStatement::AlterClassMode {
			..
		}
		| LogicalSchemaStatement::AlterChangefeed {
			..
		}
		| LogicalSchemaStatement::AlterFieldType {
			..
		}
		| LogicalSchemaStatement::PrepareIndexRemoval {
			..
		}
		| LogicalSchemaStatement::AlterAnalyzer {
			..
		}
		| LogicalSchemaStatement::InfoDatabase
		| LogicalSchemaStatement::InfoAnalyzer {
			..
		}
		| LogicalSchemaStatement::InfoClass {
			..
		}
		| LogicalSchemaStatement::InfoGraph {
			..
		}
		| LogicalSchemaStatement::InfoIndex {
			..
		}
		| LogicalSchemaStatement::InfoGraphIndex {
			..
		}
		| LogicalSchemaStatement::InfoParameter {
			..
		}
		| LogicalSchemaStatement::InfoFunction {
			..
		} => false,
	}
}

/// Returns whether the graph property mutation contains impure condition holds.
fn graph_property_mutation_contains_impure(mutation: &LogicalGraphPropertyMutation) -> bool {
	match mutation {
		LogicalGraphPropertyMutation::Set(values) => {
			values.iter().any(|(_path, value)| expr_contains_impure(value))
		}
		LogicalGraphPropertyMutation::Unset(_) => false,
	}
}

/// Returns whether the dml contains impure condition holds.
fn dml_contains_impure(statement: &LogicalDmlStatement) -> bool {
	statement.targets.iter().any(|target| {
		selector_contains_impure(&target.selector)
			|| target.data.as_ref().is_some_and(data_contains_impure)
			|| target.filter.as_ref().is_some_and(expr_contains_impure)
			|| return_contains_impure(&target.returning)
			|| target.expect.as_ref().is_some_and(expect_contains_impure)
	})
}

/// Returns whether the selector contains impure condition holds.
fn selector_contains_impure(selector: &LogicalDmlSelector) -> bool {
	match selector {
		LogicalDmlSelector::Record(value) => expr_contains_impure(value),
		LogicalDmlSelector::Class | LogicalDmlSelector::Range(_) => false,
	}
}

/// Returns whether the data contains impure condition holds.
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

/// Returns whether the return contains impure condition holds.
fn return_contains_impure(returning: &LogicalDmlReturn) -> bool {
	match returning {
		LogicalDmlReturn::Fields(values) => values
			.iter()
			.any(|field| field.as_ref().is_some_and(|field| expr_contains_impure(&field.value))),
		LogicalDmlReturn::Value(value) => expr_contains_impure(value),
		LogicalDmlReturn::None
		| LogicalDmlReturn::Before
		| LogicalDmlReturn::After
		| LogicalDmlReturn::Diff => false,
	}
}

/// Returns whether the expect contains impure condition holds.
fn expect_contains_impure(expect: &LogicalExpect) -> bool {
	matches!(expect, LogicalExpect::Predicate(value) if expr_contains_impure(value))
}

/// Returns whether the graph dml contains impure condition holds.
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
		LogicalGraphDmlStatement::Relate(relate) => {
			let crate::gql::ir::LogicalRelate {
				source,
				edge,
				target,
				weight,
				..
			} = relate.as_ref();
			expr_contains_impure(source.expression())
				|| expr_contains_impure(target.expression())
				|| edge.key.as_ref().is_some_and(expr_contains_impure)
				|| edge.data.as_ref().is_some_and(data_contains_impure)
				|| return_contains_impure(&edge.returning)
				|| edge.expect.as_ref().is_some_and(expect_contains_impure)
				|| weight.as_ref().is_some_and(expr_contains_impure)
		}
	}
}

/// Matches contains impure.
fn match_contains_impure(statement: &LogicalMatch) -> bool {
	statement.patterns.iter().any(|pattern| match &pattern.kind {
		crate::gql::ir::LogicalMatchPatternKind::Linear(pattern) => {
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
		}
		crate::gql::ir::LogicalMatchPatternKind::Hyper(pattern) => {
			binding_contains_impure(&pattern.edge)
				|| pattern
					.contains
					.iter()
					.flat_map(|group| &group.endpoints)
					.any(binding_contains_impure)
		}
		crate::gql::ir::LogicalMatchPatternKind::Within(pattern) => {
			binding_contains_impure(&pattern.child) || binding_contains_impure(&pattern.owner)
		}
	}) || statement.filter.as_ref().is_some_and(expr_contains_impure)
		|| statement.projection.iter().any(expr_contains_impure)
}

/// Returns whether the binding contains impure condition holds.
fn binding_contains_impure(binding: &crate::gql::ir::MatchBinding) -> bool {
	let selector = match &binding.selector {
		LogicalMatchSelector::Record(record) => expr_contains_impure(&record.key),
		LogicalMatchSelector::Any | LogicalMatchSelector::Class(_) => false,
	};
	selector
		|| binding.properties.iter().any(|(_, value)| expr_contains_impure(value))
		|| binding.predicate.as_ref().is_some_and(expr_contains_impure)
}

/// Returns whether the select contains impure condition holds.
fn select_contains_impure(statement: &LogicalSelect) -> bool {
	statement.return_graph.as_deref().is_some_and(return_graph_contains_impure)
		|| statement.union_inputs.iter().any(select_contains_impure)
		|| source_contains_impure(&statement.source)
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
			LogicalSelectItem::WildcardVariants {
				selector,
				..
			} => expr_contains_impure(selector),
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

/// Returns whether the source contains impure condition holds.
fn source_contains_impure(source: &LogicalSelectSource) -> bool {
	match &source.kind {
		LogicalSelectSourceKind::Empty {
			graph,
		}
		| LogicalSelectSourceKind::Atoms {
			graph,
			..
		}
		| LogicalSelectSourceKind::AtomUnion {
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

/// Returns whether the expr contains impure condition holds.
fn expr_contains_impure(expression: &TypedExpr) -> bool {
	match &expression.kind {
		TypedExprKind::Builtin {
			function:
				Builtin::TimeNow(_)
				| Builtin::Uuid
				| Builtin::Ulid
				| Builtin::RandId
				| Builtin::RegexMatcher
				| Builtin::RegexNext
				| Builtin::ArrayPush(_)
				| Builtin::ArrayPop(_)
				| Builtin::ArrayClear(_)
				| Builtin::SetAdd(_)
				| Builtin::SetClear(_),
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
		TypedExprKind::Fulltext {
			left,
			right,
			..
		} => expr_contains_impure(left) || expr_contains_impure(right),
		TypedExprKind::Knn {
			left,
			k,
			ef,
			distance,
			right,
			..
		} => {
			expr_contains_impure(left)
				|| expr_contains_impure(k)
				|| ef.as_deref().is_some_and(expr_contains_impure)
				|| match distance {
					LogicalKnnDistance::Minkowski(value) | LogicalKnnDistance::Callable(value) => {
						expr_contains_impure(value)
					}
					LogicalKnnDistance::Builtin(_) | LogicalKnnDistance::Named(_) => false,
				}
				|| expr_contains_impure(right)
		}
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
		TypedExprKind::Closure(closure) => closure.effect != crate::gql::ir::ClosureEffect::Pure,
		TypedExprKind::Invoke {
			callee,
			arguments,
		} => expr_contains_impure(callee) || arguments.iter().any(expr_contains_impure),
		TypedExprKind::NamedInvoke {
			function,
			arguments,
		} => {
			function.effect != crate::datastore::FunctionEffect::Pure
				|| arguments.iter().any(expr_contains_impure)
		}
		TypedExprKind::Traversal(traverse) => {
			let source = match &traverse.source {
				LogicalTraverseSource::Expression(expression) => expr_contains_impure(expression),
				LogicalTraverseSource::Match(_) | LogicalTraverseSource::Select(_) => false,
			};
			source
				|| traverse.steps.iter().any(|step| {
					step.match_plan.as_deref().is_some_and(match_contains_impure)
						|| step.named_function.is_some_and(|function| {
							matches!(function.effect, crate::datastore::FunctionEffect::Impure)
						})
						|| step.arguments.iter().any(expr_contains_impure)
						|| step.repeat.as_ref().is_some_and(|repeat| {
							expr_contains_impure(&repeat.body)
								|| repeat.times.as_ref().is_some_and(expr_contains_impure)
								|| repeat.until.as_ref().is_some_and(expr_contains_impure)
						})
				})
		}
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
		| TypedExprKind::LiveSlot(_)
		| TypedExprKind::Column(_)
		| TypedExprKind::CurrentField(_) => false,
	}
}
