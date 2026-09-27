//! Concrete rule and cost optimizer used between typed IR and execution.

mod cost;
mod demand;
mod deterministic;
mod encode;
mod estimate;
mod match_plan;
mod plan;
mod rules;
mod select_plan;
mod snapshot;
mod write_plan;

pub(crate) use cost::{Cardinality, PlanCost};
pub(in crate::gql) use deterministic::validate_deterministic_statement;
pub(crate) use encode::{CanonicalHasher, canonical_hash};
pub(in crate::gql) use estimate::estimate_predicate;
pub(crate) use estimate::{CostWeights, join_cost, lookup_cost, range_lookup_cost, scan_cost};
pub(in crate::gql) use match_plan::plan_match;
pub(crate) use plan::{
	AppliedRule,
	IndexAccessKind,
	IndexDirection,
	IndexJoinProbe,
	JoinBuildSide,
	JoinOperator,
	MatchOperator,
	PhysicalJoin,
	PhysicalMatchNode,
	PhysicalMatchPlan,
	PhysicalPlan,
	PhysicalSelectPlan,
	PhysicalSource,
	PhysicalWritePlan,
	PlanWarning,
	SourceOperator,
	WriteOperator,
};
pub(in crate::gql) use rules::prepare_select;
pub(in crate::gql) use select_plan::plan_select;
pub(crate) use snapshot::{
	PlannerClass,
	PlannerDemand,
	PlannerField,
	PlannerGraph,
	PlannerIndex,
	PlannerPropertyDemand,
	PlannerPropertyHistogram,
	PlannerSnapshot,
	PlannerStatisticsMeta,
};
pub(in crate::gql) use write_plan::plan_write;

/// Version mixed into canonical query and physical-plan fingerprints.
pub(crate) const PLAN_ENCODING_VERSION: u16 = 1;

#[cfg(test)]
mod tests;
