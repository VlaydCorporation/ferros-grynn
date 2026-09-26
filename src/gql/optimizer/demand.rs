//! Statistics-demand discovery over typed logical SELECT plans.

use super::{PlannerDemand, PlannerPropertyDemand};
use crate::{
	gql::ir::{
		LogicalGraphDmlStatement,
		LogicalGraphReference,
		LogicalMatch,
		LogicalMatchSelector,
		LogicalSelect,
		LogicalSelectItem,
		LogicalSelectSource,
		LogicalSelectSourceKind,
		LogicalStatement,
		MatchBinding,
		MatchOp,
		TypedExpr,
		TypedExprKind,
	},
	stats::{DegreeDirection, DegreeHistogramKey, LabelStatisticKey, StatisticsScope},
};

#[derive(Clone)]
struct AtomColumns {
	start: usize,
	width: usize,
	class: Box<str>,
	scope: Option<StatisticsScope>,
	columns: Vec<Box<str>>,
}

impl PlannerDemand {
	/// Discovers only metadata and distributions referenced by one logical
	/// SELECT, including nested SELECT expressions.
	pub(in crate::gql) fn for_select(select: &LogicalSelect) -> Self {
		let mut demand = Self::default();
		let mut owners = Vec::new();
		let mut offset = 0;
		collect_source(&select.source, offset, &mut demand, &mut owners);
		offset += select.source.schema.columns.len();
		for join in &select.joins {
			collect_source(&join.source, offset, &mut demand, &mut owners);
			collect_expr(join.predicate.as_ref(), &owners, &mut demand);
			offset += join.source.schema.columns.len();
		}
		collect_expr(select.filter.as_ref(), &owners, &mut demand);
		for expression in &select.split {
			collect_expr(Some(expression), &owners, &mut demand);
		}
		if let Some(group) = &select.group {
			for expression in group {
				collect_expr(Some(expression), &owners, &mut demand);
			}
		}
		for item in &select.projection {
			if let LogicalSelectItem::Expression {
				expression,
				..
			} = item
			{
				collect_expr(Some(expression), &owners, &mut demand);
			}
		}
		for order in &select.order {
			collect_expr(Some(&order.expression), &owners, &mut demand);
		}
		if let Some(limit) = &select.limit {
			collect_expr(Some(limit), &owners, &mut demand);
		}
		demand
	}

	/// Discovers graph, class, label, and degree metadata used by current
	/// MATCH.
	pub(in crate::gql) fn for_match(statement: &LogicalMatch) -> Self {
		let mut demand = Self::default();
		let graph = statement.graph.id();
		demand.graphs.insert(graph);
		for pattern in &statement.patterns {
			collect_match_binding(&pattern.start, &mut demand);
			for operation in &pattern.ops {
				let (source, edge, target, direction) = match operation {
					MatchOp::Expand(expand) => {
						(&expand.source, &expand.edge, &expand.target, expand.direction)
					}
					MatchOp::PathExpand(expand) => {
						(&expand.source, &expand.edge, &expand.target, expand.direction)
					}
				};
				collect_match_binding(source, &mut demand);
				collect_match_binding(edge, &mut demand);
				collect_match_binding(target, &mut demand);
				for direction in degree_directions(direction) {
					demand.degrees.entry(graph).or_default().insert(DegreeHistogramKey {
						label: source.label.as_ref().map(|label| LabelStatisticKey {
							kind: source.domain.atom_kind(),
							label: label.as_ref().into(),
						}),
						direction,
					});
				}
			}
		}
		demand
	}

	/// Discovers metadata needed to explain one supported statement.
	pub(in crate::gql) fn for_statement(statement: &LogicalStatement) -> Self {
		match statement {
			LogicalStatement::Explain(explain) => Self::for_statement(&explain.statement),
			LogicalStatement::Select(select) => Self::for_select(select),
			LogicalStatement::Match(statement) => Self::for_match(statement),
			LogicalStatement::Dml(statement) => {
				let mut demand = Self::default();
				if let Some(graph) = &statement.graph {
					demand.graphs.insert(graph.id());
				}
				for target in &statement.targets {
					demand.classes.insert(target.class.clone());
				}
				demand
			}
			LogicalStatement::GraphDml(statement) => {
				let mut demand = Self::default();
				match statement {
					LogicalGraphDmlStatement::Attach {
						record,
						graph,
						..
					}
					| LogicalGraphDmlStatement::Detach {
						record,
						graph,
						..
					} => {
						demand.classes.insert(record.class.clone());
						demand.graphs.insert(graph.id());
					}
					LogicalGraphDmlStatement::Relate {
						source,
						edge,
						target,
						graph,
						..
					} => {
						demand.classes.insert(source.class.clone());
						demand.classes.insert(edge.class.clone());
						demand.classes.insert(target.class.clone());
						demand.graphs.insert(graph.id());
					}
				}
				demand
			}
			LogicalStatement::Schema(statement) => {
				let mut demand = Self::default();
				match statement {
					crate::gql::ir::LogicalSchemaStatement::DefineField {
						class,
						..
					}
					| crate::gql::ir::LogicalSchemaStatement::DefineIndex {
						class,
						..
					}
					| crate::gql::ir::LogicalSchemaStatement::RemoveClass {
						name: class,
						..
					}
					| crate::gql::ir::LogicalSchemaStatement::RemoveField {
						class,
						..
					}
					| crate::gql::ir::LogicalSchemaStatement::RemoveIndex {
						class,
						..
					}
					| crate::gql::ir::LogicalSchemaStatement::AlterClassMode {
						name: class,
						..
					}
					| crate::gql::ir::LogicalSchemaStatement::AlterFieldType {
						class,
						..
					}
					| crate::gql::ir::LogicalSchemaStatement::PrepareIndexRemoval {
						class,
						..
					} => {
						demand.classes.insert(class.clone());
					}
					crate::gql::ir::LogicalSchemaStatement::UpdateGraph {
						graph,
						..
					} => {
						demand.graphs.insert(graph.id());
					}
					_ => {}
				}
				demand
			}
			_ => Self::default(),
		}
	}

	fn merge(&mut self, other: Self) {
		self.classes.extend(other.classes);
		self.graphs.extend(other.graphs);
		self.all_graphs |= other.all_graphs;
		self.properties.extend(other.properties);
		for (graph, keys) in other.degrees {
			self.degrees.entry(graph).or_default().extend(keys);
		}
	}
}

fn collect_match_binding(binding: &MatchBinding, demand: &mut PlannerDemand) {
	match &binding.selector {
		LogicalMatchSelector::Class(class) => {
			demand.classes.insert(class.clone());
		}
		LogicalMatchSelector::Record(record) => {
			demand.classes.insert(record.class.clone());
		}
		LogicalMatchSelector::Any => {}
	}
}

fn degree_directions(direction: crate::syn::MatchDirection) -> Vec<DegreeDirection> {
	match direction {
		crate::syn::MatchDirection::Forward => vec![DegreeDirection::Out],
		crate::syn::MatchDirection::Backward => vec![DegreeDirection::In],
		crate::syn::MatchDirection::Undirected => vec![DegreeDirection::Undirected],
		crate::syn::MatchDirection::Bidirectional => {
			vec![DegreeDirection::Out, DegreeDirection::In]
		}
	}
}

fn collect_source(
	source: &LogicalSelectSource,
	start: usize,
	demand: &mut PlannerDemand,
	owners: &mut Vec<AtomColumns>,
) {
	match &source.kind {
		LogicalSelectSourceKind::Atoms {
			target,
			graph,
		} => {
			let class: Box<str> = target.class().into();
			demand.classes.insert(class.clone());
			let scope = match graph {
				None => Some(StatisticsScope::Base),
				Some(LogicalGraphReference::Static(graph)) => {
					demand.graphs.insert(graph.id());
					Some(StatisticsScope::Effective {
						graph: graph.id(),
					})
				}
				Some(LogicalGraphReference::Expression(expression)) => {
					collect_expr(Some(expression), owners, demand);
					None
				}
			};
			owners.push(AtomColumns {
				start,
				width: source.schema.columns.len(),
				class,
				scope,
				columns: source.schema.columns.iter().map(|column| column.name.clone()).collect(),
			});
		}
		LogicalSelectSourceKind::Graphs {
			references,
		} => match references {
			Some(references) => {
				for reference in references {
					match reference {
						LogicalGraphReference::Static(graph) => {
							demand.graphs.insert(graph.id());
						}
						LogicalGraphReference::Expression(expression) => {
							collect_expr(Some(expression), owners, demand);
						}
					}
				}
			}
			None => demand.all_graphs = true,
		},
		LogicalSelectSourceKind::Derived(select) => {
			demand.merge(PlannerDemand::for_select(select));
		}
	}
}

fn collect_expr(
	expression: Option<&TypedExpr>,
	owners: &[AtomColumns],
	demand: &mut PlannerDemand,
) {
	let Some(expression) = expression else {
		return;
	};
	match &expression.kind {
		TypedExprKind::Column(column) => demand_column(*column, None, owners, demand),
		TypedExprKind::Field {
			base,
			field,
			..
		} => {
			if let TypedExprKind::Column(column) = &base.kind {
				demand_column(*column, Some(field), owners, demand);
			}
			collect_expr(Some(base), owners, demand);
		}
		TypedExprKind::Array(values) | TypedExprKind::Set(values) => {
			for value in values {
				collect_expr(Some(value), owners, demand);
			}
		}
		TypedExprKind::Object(fields) => {
			for (_, value) in fields {
				collect_expr(Some(value), owners, demand);
			}
		}
		TypedExprKind::Range {
			start,
			end,
			..
		} => {
			collect_expr(start.as_deref(), owners, demand);
			collect_expr(end.as_deref(), owners, demand);
		}
		TypedExprKind::Unary {
			expr,
			..
		}
		| TypedExprKind::Cast {
			expr,
			..
		} => collect_expr(Some(expr), owners, demand),
		TypedExprKind::Binary {
			left,
			right,
			..
		} => {
			collect_expr(Some(left), owners, demand);
			collect_expr(Some(right), owners, demand);
		}
		TypedExprKind::Conditional {
			condition,
			then_value,
			else_value,
		} => {
			collect_expr(Some(condition), owners, demand);
			collect_expr(Some(then_value), owners, demand);
			collect_expr(Some(else_value), owners, demand);
		}
		TypedExprKind::Case {
			operand,
			branches,
			else_value,
		} => {
			collect_expr(operand.as_deref(), owners, demand);
			for (condition, value) in branches {
				collect_expr(Some(condition), owners, demand);
				collect_expr(Some(value), owners, demand);
			}
			collect_expr(else_value.as_deref(), owners, demand);
		}
		TypedExprKind::Destructure {
			base,
			..
		} => collect_expr(Some(base), owners, demand),
		TypedExprKind::Index {
			base,
			index,
		} => {
			collect_expr(Some(base), owners, demand);
			collect_expr(Some(index), owners, demand);
		}
		TypedExprKind::Filter {
			base,
			predicate,
		} => {
			collect_expr(Some(base), owners, demand);
			collect_expr(Some(predicate), owners, demand);
		}
		TypedExprKind::Builtin {
			arguments,
			..
		} => {
			for argument in arguments {
				collect_expr(Some(argument), owners, demand);
			}
		}
		TypedExprKind::Aggregate {
			argument,
			..
		} => collect_expr(argument.as_deref(), owners, demand),
		TypedExprKind::Subquery {
			select,
			..
		} => demand.merge(PlannerDemand::for_select(select)),
		TypedExprKind::Constant(_) | TypedExprKind::Slot(_) | TypedExprKind::CurrentField(_) => {}
	}
}

fn demand_column(
	column: usize,
	nested: Option<&Box<str>>,
	owners: &[AtomColumns],
	demand: &mut PlannerDemand,
) {
	if let Some(property) = resolve_column(column, nested, owners) {
		demand.properties.insert(property);
	}
}

fn resolve_column(
	column: usize,
	nested: Option<&Box<str>>,
	owners: &[AtomColumns],
) -> Option<PlannerPropertyDemand> {
	let Some(owner) =
		owners.iter().find(|owner| column >= owner.start && column < owner.start + owner.width)
	else {
		return None;
	};
	let Some(scope) = owner.scope else {
		return None;
	};
	let local = column - owner.start;
	let field = match (owner.columns.get(local).map(Box::as_ref), nested) {
		(Some("properties"), Some(field)) => field.clone(),
		(Some(field), _) if matches!(field, "id" | "class" | "kind" | "label" | "properties") => {
			return None;
		}
		(None, _) => return None,
		(Some(field), None) => field.into(),
		(Some(_), Some(_)) => return None,
	};
	Some(PlannerPropertyDemand {
		class: owner.class.clone(),
		field,
		scope,
	})
}

/// Resolves one typed input column to a declared property statistic.
pub(in crate::gql) fn property_demand(
	select: &LogicalSelect,
	column: usize,
	nested: Option<&Box<str>>,
) -> Option<PlannerPropertyDemand> {
	let mut demand = PlannerDemand::default();
	let mut owners = Vec::new();
	let mut offset = 0;
	collect_source(&select.source, offset, &mut demand, &mut owners);
	offset += select.source.schema.columns.len();
	for join in &select.joins {
		collect_source(&join.source, offset, &mut demand, &mut owners);
		offset += join.source.schema.columns.len();
	}
	resolve_column(column, nested, &owners)
}
