//! Concrete Stage-18 graph-result planning without speculative alternatives.

use fg_runtime::__internal::GraphTopologyConstraintsInternal as _;

use super::{
	Cardinality,
	GraphResultMode,
	GraphResultOperator,
	PhysicalGraphResultPlan,
	PhysicalPlan,
	PlanCost,
	PlanWarning,
	plan_match,
	plan_traverse,
	prepare_select,
};
use crate::{
	gql::ir::{
		LogicalBuildGraph,
		LogicalBuildGraphSource,
		LogicalMatch,
		LogicalReturnGraph,
		LogicalSelect,
		LogicalTraverse,
	},
	gql_status::FerrosGrynnResult,
	syn::DefinitionMode,
};

/// Plans one standalone graph assembly.
pub(in crate::gql) fn plan_return_graph(
	result: &LogicalReturnGraph,
	max_nodes: u64,
	max_edges: u64,
	barrier_bytes: usize,
) -> PhysicalPlan {
	graph_plan(
		GraphResultOperator::GraphAssemble,
		GraphResultMode::Anonymous,
		None,
		None,
		result.contract.family,
		result.contract.validators.bits(),
		result.root.is_some(),
		true,
		true,
		max_nodes,
		max_edges,
		Cardinality::exact(1),
		barrier_bytes,
	)
}

/// Wraps ordinary SELECT rows when its terminal result is an owned graph.
pub(in crate::gql) fn plan_select_graph_result(
	statement: &LogicalSelect,
	snapshot: &super::PlannerSnapshot,
	max_nodes: u64,
	max_edges: u64,
	barrier_bytes: usize,
) -> FerrosGrynnResult<PhysicalPlan> {
	let (_, source) = prepare_select(statement, snapshot, barrier_bytes)?;
	let source = PhysicalPlan::Select(source);
	let Some(result) = &statement.return_graph else {
		return Ok(source);
	};
	let source_rows = output_cardinality(&source);
	Ok(graph_plan(
		GraphResultOperator::GraphAssemble,
		GraphResultMode::Anonymous,
		Some(source),
		None,
		result.contract.family,
		result.contract.validators.bits(),
		result.root.is_some(),
		true,
		true,
		max_nodes,
		max_edges,
		source_rows,
		barrier_bytes,
	))
}

/// Wraps a MATCH physical plan when its terminal result is an owned graph.
pub(in crate::gql) fn plan_match_graph_result(
	statement: &LogicalMatch,
	snapshot: &super::PlannerSnapshot,
	deterministic: bool,
	max_nodes: u64,
	max_edges: u64,
	barrier_bytes: usize,
) -> FerrosGrynnResult<PhysicalPlan> {
	Ok(finish_match_graph_result(
		statement,
		plan_match(statement, snapshot, deterministic)?,
		max_nodes,
		max_edges,
		barrier_bytes,
	))
}

/// Wraps an already bound MATCH plan without copying its logical patterns.
pub(in crate::gql) fn finish_match_graph_result(
	statement: &LogicalMatch,
	plan: super::PhysicalMatchPlan,
	max_nodes: u64,
	max_edges: u64,
	barrier_bytes: usize,
) -> PhysicalPlan {
	let source = PhysicalPlan::Match(plan);
	let Some(result) = &statement.return_graph else {
		return source;
	};
	graph_plan(
		GraphResultOperator::GraphAssemble,
		GraphResultMode::Anonymous,
		Some(source),
		None,
		result.contract.family,
		result.contract.validators.bits(),
		result.root.is_some(),
		true,
		true,
		max_nodes,
		max_edges,
		Cardinality::exact(1),
		barrier_bytes,
	)
}

/// Wraps a traversal plan when its terminal result is an owned graph.
#[allow(
	clippy::too_many_arguments,
	reason = "the traversal planner keeps all bounded modes explicit"
)]
pub(in crate::gql) fn plan_traverse_graph_result(
	statement: &LogicalTraverse,
	snapshot: &super::PlannerSnapshot,
	barrier_bytes: usize,
	deterministic: bool,
	fast: bool,
	max_nodes: u64,
	max_edges: u64,
) -> FerrosGrynnResult<PhysicalPlan> {
	let source = PhysicalPlan::Traversal(plan_traverse(
		statement,
		snapshot,
		barrier_bytes,
		deterministic,
		fast,
	)?);
	let Some(result) = &statement.return_graph else {
		return Ok(source);
	};
	Ok(graph_plan(
		GraphResultOperator::GraphAssemble,
		GraphResultMode::Anonymous,
		Some(source),
		None,
		result.contract.family,
		result.contract.validators.bits(),
		result.root.is_some(),
		true,
		true,
		max_nodes,
		max_edges,
		Cardinality::exact(1),
		barrier_bytes,
	))
}

/// Plans an anonymous or named `BUILD GRAPH` around its already-existing source
/// planner.
#[allow(
	clippy::too_many_arguments,
	reason = "graph assembly limits and execution modes are independent contracts"
)]
pub(in crate::gql) fn plan_build_graph(
	build: &LogicalBuildGraph,
	snapshot: &super::PlannerSnapshot,
	barrier_bytes: usize,
	deterministic: bool,
	fast: bool,
	max_nodes: u64,
	max_edges: u64,
) -> FerrosGrynnResult<PhysicalPlan> {
	let source = match &build.source {
		LogicalBuildGraphSource::Match(statement) => Some(plan_match_graph_result(
			statement,
			snapshot,
			deterministic,
			max_nodes,
			max_edges,
			barrier_bytes,
		)?),
		LogicalBuildGraphSource::Select(statement) => Some(plan_select_graph_result(
			statement,
			snapshot,
			max_nodes,
			max_edges,
			barrier_bytes,
		)?),
		LogicalBuildGraphSource::Traverse(statement) => Some(plan_traverse_graph_result(
			statement,
			snapshot,
			barrier_bytes,
			deterministic,
			fast,
			max_nodes,
			max_edges,
		)?),
		LogicalBuildGraphSource::Expression(_) => None,
	};
	let source_rows = source.as_ref().map_or(Cardinality::exact(1), output_cardinality);
	let node_limit = build.limit_nodes.map_or(max_nodes, |limit| limit.min(max_nodes));
	let edge_limit = build.limit_edges.map_or(max_edges, |limit| limit.min(max_edges));
	let mode = match (&build.name, build.mode) {
		(None, _) => GraphResultMode::Anonymous,
		(Some(_), DefinitionMode::Normal) => GraphResultMode::Create,
		(Some(_), DefinitionMode::IfNotExists) => GraphResultMode::IfNotExists,
		(Some(_), DefinitionMode::Overwrite) => GraphResultMode::Overwrite,

		_ => {
			return Err(crate::gql_status::FerrosGrynnError::invalid_argument(
				"Unsupported syntax variant at the engine boundary.",
			));
		}
	};
	Ok(graph_plan(
		if build.name.is_some() {
			GraphResultOperator::BuildGraph
		} else {
			GraphResultOperator::GraphAssemble
		},
		mode,
		source,
		build.name.clone(),
		build.contract.family,
		build.contract.validators.bits(),
		build.root.is_some(),
		build.nodes.is_some(),
		build.edges.is_some(),
		node_limit,
		edge_limit,
		source_rows,
		barrier_bytes,
	))
}

#[allow(
	clippy::too_many_arguments,
	reason = "one concrete graph-result node exposes its fixed contract"
)]
/// Returns the graph plan result.
fn graph_plan(
	operator: GraphResultOperator,
	mode: GraphResultMode,
	source: Option<PhysicalPlan>,
	name: Option<Box<str>>,
	family: Option<crate::graph::GraphKind>,
	validator_bits: u16,
	has_root: bool,
	has_node_selector: bool,
	has_edge_selector: bool,
	node_limit: u64,
	edge_limit: u64,
	source_rows: Cardinality,
	barrier_bytes: usize,
) -> PhysicalPlan {
	let expected_nodes = Cardinality::new(0, source_rows.expected.min(node_limit), node_limit);
	let expected_edges = Cardinality::new(0, source_rows.expected.min(edge_limit), edge_limit);
	let expected_scopes = Cardinality::new(0, 1, source_rows.upper.max(1));
	let estimated_bytes = expected_nodes
		.expected
		.saturating_mul(256)
		.saturating_add(expected_edges.expected.saturating_mul(384))
		.saturating_add(expected_scopes.expected.saturating_mul(64));
	let mut warnings = Vec::new();
	if estimated_bytes > u64::try_from(barrier_bytes).unwrap_or(u64::MAX) {
		warnings.push(PlanWarning::ResourceRisk(
			"estimated owned graph exceeds the statement barrier-memory budget".into(),
		));
	}
	PhysicalPlan::GraphResult(PhysicalGraphResultPlan {
		operator,
		mode,
		source: source.map(Box::new),
		name,
		family,
		validator_bits,
		has_root,
		has_node_selector,
		has_edge_selector,
		node_limit,
		edge_limit,
		expected_nodes,
		expected_edges,
		expected_scopes,
		cost: PlanCost {
			startup: 1,
			sequential: source_rows.expected,
			memory: estimated_bytes,
			..PlanCost::default()
		},
		warnings,
	})
}

/// Returns the output cardinality result.
fn output_cardinality(plan: &PhysicalPlan) -> Cardinality {
	match plan {
		PhysicalPlan::Select(plan) => plan.cardinality,
		PhysicalPlan::Match(plan) => plan.cardinality,
		PhysicalPlan::Traversal(plan) => plan.cardinality,
		PhysicalPlan::Write(plan) => plan.cardinality,
		PhysicalPlan::GraphResult(_) => Cardinality::exact(1),
		PhysicalPlan::Changefeed(plan) => Cardinality::unknown(plan.limit.unwrap_or(u64::MAX)),
		PhysicalPlan::Live(_) => Cardinality::exact(1),
	}
}
