//! Statistics-demand discovery over typed logical SELECT plans.

use super::{PlannerDemand, PlannerPropertyDemand};
use crate::{
	gql::ir::{
		LogicalGraphDmlStatement,
		LogicalGraphReference,
		LogicalKnnDistance,
		LogicalMatch,
		LogicalMatchPatternKind,
		LogicalMatchSelector,
		LogicalSelect,
		LogicalSelectItem,
		LogicalSelectSource,
		LogicalSelectSourceKind,
		LogicalStatement,
		LogicalTraverse,
		LogicalTraverseSource,
		MatchBinding,
		MatchOp,
		TypedExpr,
		TypedExprKind,
	},
	stats::{DegreeDirection, DegreeHistogramKey, LabelStatisticKey, StatisticsScope},
};

#[derive(Clone)]
/// Atom columns state.
struct AtomColumns {
	/// Start associated with this atom columns.
	start: usize,
	/// Width associated with this atom columns.
	width: usize,
	/// Class associated with this atom columns.
	class: Box<str>,
	/// Scope associated with this atom columns.
	scope: Option<StatisticsScope>,
	/// Columns associated with this atom columns.
	columns: Vec<Box<str>>,
}

impl PlannerDemand {
	/// Discovers only metadata and distributions referenced by one logical
	/// SELECT, including nested SELECT expressions.
	pub(in crate::gql) fn for_select(select: &LogicalSelect) -> Self {
		let mut demand = Self::default();
		for input in &select.union_inputs {
			demand.merge(Self::for_select(input));
		}
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
			}
			| LogicalSelectItem::WildcardVariants {
				selector: expression,
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
		if let Some(graph) = &select.return_graph {
			for expression in graph.expressions() {
				collect_expr(Some(expression), &[], &mut demand);
			}
		}
		demand
	}

	/// Discovers graph, class, label, and degree metadata used by current
	/// MATCH.
	pub(in crate::gql) fn for_match(statement: &LogicalMatch) -> Self {
		let graph = match &statement.graph {
			LogicalGraphReference::Static(graph) => Some(graph.id()),
			LogicalGraphReference::Expression(_) => None,
		};
		Self::match_graph(statement, graph)
	}

	/// Collects statistics for the graph resolved by this MATCH invocation.
	pub(in crate::gql) fn for_match_bound(
		statement: &LogicalMatch,
		graph: crate::graph::GraphId,
	) -> Self {
		Self::match_graph(statement, Some(graph))
	}

	/// Retains expression demand while graph-specific statistics await binding.
	fn match_graph(statement: &LogicalMatch, graph: Option<crate::graph::GraphId>) -> Self {
		let mut demand = Self::default();
		if let Some(graph) = graph {
			demand.graphs.insert(graph);
		}
		if let LogicalGraphReference::Expression(expression) = &statement.graph {
			collect_expr(Some(expression), &[], &mut demand);
		}
		collect_expr(statement.max_depth_binding.as_ref(), &[], &mut demand);
		for pattern in &statement.patterns {
			match &pattern.kind {
				LogicalMatchPatternKind::Linear(pattern) => {
					collect_match_binding(&pattern.start, &mut demand);
					for operation in &pattern.ops {
						let (source, edge, target, direction) = match operation {
							MatchOp::Expand(expand) => {
								(&expand.source, &expand.edge, &expand.target, expand.direction)
							}
							MatchOp::PathExpand(expand) => {
								demand.artifacts.tensor_caches |= expand
									.objective
									.is_some_and(crate::syn::MatchPathObjective::is_all);
								demand.artifacts.landmark_paths |=
									expand.objective.is_some_and(|objective| !objective.is_all());
								(&expand.source, &expand.edge, &expand.target, expand.direction)
							}
						};
						collect_match_binding(source, &mut demand);
						collect_match_binding(edge, &mut demand);
						collect_match_binding(target, &mut demand);
						for direction in degree_directions(direction) {
							if let Some(graph) = graph {
								demand.degrees.entry(graph).or_default().insert(
									DegreeHistogramKey {
										label: source.label.as_ref().map(|label| {
											LabelStatisticKey {
												kind: source.domain.atom_kind(),
												label: label.as_ref().into(),
											}
										}),
										direction,
									},
								);
							}
						}
					}
				}
				LogicalMatchPatternKind::Hyper(pattern) => {
					collect_match_binding(&pattern.edge, &mut demand);
					for endpoint in pattern.contains.iter().flat_map(|group| &group.endpoints) {
						collect_match_binding(endpoint, &mut demand);
					}
				}
				LogicalMatchPatternKind::Within(pattern) => {
					collect_match_binding(&pattern.child, &mut demand);
					collect_match_binding(&pattern.owner, &mut demand);
				}
			}
		}
		if let Some(result) = &statement.return_graph {
			collect_expr(result.root.as_ref(), &[], &mut demand);
			collect_expr(Some(&result.nodes), &[], &mut demand);
			collect_expr(Some(&result.edges), &[], &mut demand);
		}
		demand
	}

	/// Discovers nested source, graph, and expression metadata used by one
	/// traversal pipeline.
	pub(in crate::gql) fn for_traverse(statement: &LogicalTraverse) -> Self {
		let mut demand = match &statement.source {
			LogicalTraverseSource::Expression(expression) => {
				let mut demand = Self::default();
				collect_expr(Some(expression), &[], &mut demand);
				demand
			}
			LogicalTraverseSource::Match(statement) => Self::for_match(statement),
			LogicalTraverseSource::Select(statement) => Self::for_select(statement),
		};
		if let Some(graph) = &statement.graph {
			match graph {
				LogicalGraphReference::Static(graph) => {
					demand.graphs.insert(graph.id());
				}
				LogicalGraphReference::Expression(expression) => {
					collect_expr(Some(expression), &[], &mut demand);
				}
			}
		}
		for step in &statement.steps {
			demand.artifacts.tensor_caches |= step.kind.is_tensor_analytics()
				|| matches!(
					step.kind,
					crate::gql::ir::TraverseStepKind::AllShortestPaths
						| crate::gql::ir::TraverseStepKind::AllWeightedShortestPaths
				);
			demand.artifacts.neighbourhood_sketches |=
				step.kind == crate::gql::ir::TraverseStepKind::KHopEstimate;
			demand.artifacts.landmark_paths |= matches!(
				step.kind,
				crate::gql::ir::TraverseStepKind::ShortestPath
					| crate::gql::ir::TraverseStepKind::WeightedShortestPath
					| crate::gql::ir::TraverseStepKind::DistanceTo
			);
			if let Some(child) = &step.match_plan {
				demand.merge(Self::for_match(child));
			}
			for argument in &step.arguments {
				collect_expr(Some(argument), &[], &mut demand);
			}
			if let Some(repeat) = &step.repeat {
				collect_expr(Some(&repeat.body), &[], &mut demand);
				collect_expr(repeat.times.as_ref(), &[], &mut demand);
				collect_expr(repeat.until.as_ref(), &[], &mut demand);
			}
		}
		collect_expr(statement.returning.as_ref(), &[], &mut demand);
		if let Some(result) = &statement.return_graph {
			collect_expr(result.root.as_ref(), &[], &mut demand);
			collect_expr(Some(&result.nodes), &[], &mut demand);
			collect_expr(Some(&result.edges), &[], &mut demand);
		}
		demand
	}

	/// Discovers metadata needed to explain one supported statement.
	pub(in crate::gql) fn for_statement(statement: &LogicalStatement) -> Self {
		match statement {
			LogicalStatement::Explain(explain) => Self::for_statement(&explain.statement),
			LogicalStatement::Select(select) => Self::for_select(select),
			LogicalStatement::Match(statement) => Self::for_match(statement),
			LogicalStatement::Traverse(statement) => Self::for_traverse(statement),
			LogicalStatement::ReturnGraph(result) => {
				let mut demand = Self::default();
				collect_expr(result.root.as_ref(), &[], &mut demand);
				collect_expr(Some(&result.nodes), &[], &mut demand);
				collect_expr(Some(&result.edges), &[], &mut demand);
				demand
			}
			LogicalStatement::BuildGraph(build) => {
				let mut demand = match &build.source {
					crate::gql::ir::LogicalBuildGraphSource::Match(statement) => {
						Self::for_match(statement)
					}
					crate::gql::ir::LogicalBuildGraphSource::Select(statement) => {
						Self::for_select(statement)
					}
					crate::gql::ir::LogicalBuildGraphSource::Traverse(statement) => {
						Self::for_traverse(statement)
					}
					crate::gql::ir::LogicalBuildGraphSource::Expression(expression) => {
						let mut demand = Self::default();
						collect_expr(Some(expression), &[], &mut demand);
						demand
					}
				};
				collect_expr(build.root.as_ref(), &[], &mut demand);
				collect_expr(build.nodes.as_ref(), &[], &mut demand);
				collect_expr(build.edges.as_ref(), &[], &mut demand);
				demand
			}
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
					LogicalGraphDmlStatement::Relate(relate) => {
						let crate::gql::ir::LogicalRelate {
							source,
							edge,
							target,
							graph,
							..
						} = relate.as_ref();
						for endpoint in [source, target] {
							if let crate::gql::ir::LogicalRelateEndpoint::Record(record) = endpoint
							{
								demand.classes.insert(record.class.clone());
							}
						}
						demand.classes.insert(edge.class.clone());
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

	/// Merges planner demand.
	fn merge(&mut self, other: Self) {
		self.classes.extend(other.classes);
		self.graphs.extend(other.graphs);
		self.all_graphs |= other.all_graphs;
		self.artifacts.neighbourhood_sketches |= other.artifacts.neighbourhood_sketches;
		self.artifacts.landmark_paths |= other.artifacts.landmark_paths;
		self.artifacts.tensor_caches |= other.artifacts.tensor_caches;
		self.properties.extend(other.properties);
		for (graph, keys) in other.degrees {
			self.degrees.entry(graph).or_default().extend(keys);
		}
	}
}

/// Collects match binding.
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

/// Returns the degree directions result.
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

/// Collects source.
fn collect_source(
	source: &LogicalSelectSource,
	start: usize,
	demand: &mut PlannerDemand,
	owners: &mut Vec<AtomColumns>,
) {
	match &source.kind {
		LogicalSelectSourceKind::Empty {
			graph,
		} => match graph {
			Some(LogicalGraphReference::Static(graph)) => {
				demand.graphs.insert(graph.id());
			}
			Some(LogicalGraphReference::Expression(expression)) => {
				collect_expr(Some(expression), owners, demand);
			}
			None => {}
		},
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
		LogicalSelectSourceKind::AtomUnion {
			targets,
			graph,
			..
		} => {
			for target in targets {
				demand.classes.insert(target.class().into());
			}
			match graph {
				Some(LogicalGraphReference::Static(graph)) => {
					demand.graphs.insert(graph.id());
				}
				Some(LogicalGraphReference::Expression(expression)) => {
					collect_expr(Some(expression), owners, demand);
				}
				None => {}
			}
		}
		LogicalSelectSourceKind::Derived(select) => {
			demand.merge(PlannerDemand::for_select(select));
		}
	}
}

/// Collects expr.
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
		TypedExprKind::Fulltext {
			left,
			right,
			..
		} => {
			collect_expr(Some(left), owners, demand);
			collect_expr(Some(right), owners, demand);
		}
		TypedExprKind::Knn {
			left,
			k,
			ef,
			distance,
			right,
			..
		} => {
			collect_expr(Some(left), owners, demand);
			collect_expr(Some(k), owners, demand);
			collect_expr(ef.as_deref(), owners, demand);
			match distance {
				LogicalKnnDistance::Minkowski(value) | LogicalKnnDistance::Callable(value) => {
					collect_expr(Some(value), owners, demand);
				}
				LogicalKnnDistance::Builtin(_) | LogicalKnnDistance::Named(_) => {}
			}
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
		TypedExprKind::Invoke {
			callee,
			arguments,
		} => {
			collect_expr(Some(callee), owners, demand);
			for argument in arguments {
				collect_expr(Some(argument), owners, demand);
			}
		}
		TypedExprKind::NamedInvoke {
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
		TypedExprKind::Traversal(traversal) => {
			demand.merge(PlannerDemand::for_traverse(traversal));
		}
		TypedExprKind::Constant(_)
		| TypedExprKind::Slot(_)
		| TypedExprKind::LiveSlot(_)
		| TypedExprKind::CurrentField(_)
		| TypedExprKind::Closure(_) => {}
	}
}

/// Performs the demand column operation.
fn demand_column(
	column: usize,
	nested: Option<&str>,
	owners: &[AtomColumns],
	demand: &mut PlannerDemand,
) {
	if let Some(property) = resolve_column(column, nested, owners) {
		demand.properties.insert(property);
	}
}

/// Resolves column.
fn resolve_column(
	column: usize,
	nested: Option<&str>,
	owners: &[AtomColumns],
) -> Option<PlannerPropertyDemand> {
	let owner =
		owners.iter().find(|owner| column >= owner.start && column < owner.start + owner.width)?;
	let scope = owner.scope?;
	let local = column - owner.start;
	let field = match (owner.columns.get(local).map(Box::as_ref), nested) {
		(Some("properties"), Some(field)) => field.into(),
		(Some("id" | "class" | "kind" | "label" | "properties"), _) => {
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
	nested: Option<&str>,
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
