//! Concrete rule and cost optimizer used between typed IR and execution.

mod cost;
mod demand;
mod deterministic;
mod encode;
mod estimate;
pub(in crate::gql) mod fingerprint;
mod graph_result_plan;
mod match_plan;
mod plan;
mod rules;
mod select_plan;
mod snapshot;
mod traverse_plan;
mod write_plan;

pub(crate) use cost::{Cardinality, PlanCost};
pub(in crate::gql) use deterministic::validate_deterministic_statement;
pub(crate) use encode::{CanonicalHasher, canonical_hash};
pub(in crate::gql) use estimate::estimate_predicate;
pub(crate) use estimate::{CostWeights, join_cost, lookup_cost, range_lookup_cost, scan_cost};
pub(in crate::gql) use graph_result_plan::{
	finish_match_graph_result,
	plan_build_graph,
	plan_match_graph_result,
	plan_return_graph,
	plan_select_graph_result,
	plan_traverse_graph_result,
};
pub(in crate::gql) use match_plan::{plan_match, plan_match_bound};
#[allow(
	clippy::redundant_pub_crate,
	reason = "Physical metadata is re-exported to execution consumers"
)]
pub(crate) use plan::PhysicalChangefeedPlan;
#[allow(
	clippy::redundant_pub_crate,
	reason = "Shared physical metadata is read by both query consumers"
)]
pub(crate) use plan::TensorProjectionPlan;
pub(crate) use plan::{
	AppliedRule,
	GraphResultMode,
	GraphResultOperator,
	IndexAccessKind,
	IndexDirection,
	IndexJoinProbe,
	JoinBuildSide,
	JoinOperator,
	MatchOperator,
	PhysicalGraphResultPlan,
	PhysicalJoin,
	PhysicalMatchNode,
	PhysicalMatchPlan,
	PhysicalPlan,
	PhysicalSelectPlan,
	PhysicalSource,
	PhysicalTraversalNode,
	PhysicalTraversalPlan,
	PhysicalWritePlan,
	PlanWarning,
	SourceOperator,
	TraversalOperator,
	TraversalSourceOperator,
	WriteOperator,
};
pub(in crate::gql) use plan::{LiveOperation, PhysicalLivePlan, PhysicalLiveSource};
pub(in crate::gql) use rules::prepare_select;
pub(in crate::gql) use select_plan::{plan_select, spatial_query};
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
pub(in crate::gql) use traverse_plan::plan_traverse;
pub(in crate::gql) use write_plan::plan_write;

/// Version mixed into canonical query and physical-plan fingerprints.
pub(crate) const PLAN_ENCODING_VERSION: u16 = 10;

#[cfg(test)]
mod tests;
