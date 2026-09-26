//! Physical phase planning for the currently executable write statements.

use super::{
	Cardinality,
	CostWeights,
	PhysicalWritePlan,
	PlanCost,
	PlannerSnapshot,
	WriteOperator,
	lookup_cost,
	scan_cost,
};
use crate::gql::ir::{LogicalDmlSelector, LogicalSchemaStatement, LogicalStatement};

/// Builds the ordered mutation/catalog phases without executing the statement.
pub(in crate::gql) fn plan_write(
	statement: &LogicalStatement,
	snapshot: &PlannerSnapshot,
) -> PhysicalWritePlan {
	let (operators, cardinality, selection_cost) = match statement {
		LogicalStatement::Dml(statement) => {
			let mut rows = Cardinality::exact(0);
			let mut cost = PlanCost::default();
			for target in &statement.targets {
				let target_rows = match &target.selector {
					LogicalDmlSelector::Record(_) => Cardinality::new(0, 1, 1),
					LogicalDmlSelector::Range(count) => Cardinality::exact(u64::from(*count)),
					LogicalDmlSelector::Class => {
						snapshot.class(&target.class).map_or(Cardinality::unknown(1), |class| {
							let count = statement.graph.as_ref().map_or(class.base_rows, |graph| {
								class.graph_rows.get(&graph.id()).copied().unwrap_or(0)
							});
							Cardinality::exact(count)
						})
					}
				};
				cost = cost.saturating_add(match &target.selector {
					LogicalDmlSelector::Record(_) => {
						lookup_cost(1, target_rows, CostWeights::default())
					}
					LogicalDmlSelector::Class | LogicalDmlSelector::Range(_) => {
						scan_cost(target_rows, 1, CostWeights::default())
					}
				});
				rows = Cardinality::new(
					rows.lower.saturating_add(target_rows.lower),
					rows.expected.saturating_add(target_rows.expected),
					rows.upper.saturating_add(target_rows.upper),
				);
			}
			(
				vec![
					WriteOperator::TargetSelection,
					WriteOperator::Mutation,
					WriteOperator::ConstraintValidation,
					WriteOperator::Publication,
				],
				rows,
				cost,
			)
		}
		LogicalStatement::GraphDml(_) => (
			vec![
				WriteOperator::TargetSelection,
				WriteOperator::Mutation,
				WriteOperator::ConstraintValidation,
				WriteOperator::Publication,
			],
			Cardinality::new(0, 1, 1),
			lookup_cost(1, Cardinality::new(0, 1, 1), CostWeights::default()),
		),
		LogicalStatement::Schema(schema) => {
			let mut operators = vec![WriteOperator::CatalogMutation];
			if matches!(schema, LogicalSchemaStatement::DefineIndex { .. }) {
				operators.push(WriteOperator::IndexBuild);
			}
			operators.push(WriteOperator::ConstraintValidation);
			operators.push(WriteOperator::Publication);
			(operators, Cardinality::exact(1), PlanCost::default())
		}
		_ => (Vec::new(), Cardinality::exact(0), PlanCost::default()),
	};
	PhysicalWritePlan {
		operators,
		cardinality,
		cost: selection_cost.saturating_add(scan_cost(cardinality, 1, CostWeights::default())),
	}
}
