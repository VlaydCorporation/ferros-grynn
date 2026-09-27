//! Fixed-point planning for the currently executable MATCH profile.

use std::collections::BTreeSet;

use super::{
	Cardinality,
	CostWeights,
	MatchOperator,
	PhysicalMatchNode,
	PhysicalMatchPlan,
	PlanCost,
	PlanWarning,
	PlannerGraph,
	PlannerSnapshot,
	lookup_cost,
	scan_cost,
};
use crate::{
	gql::ir::{
		EndpointDomain,
		LogicalMatch,
		LogicalMatchPattern,
		LogicalMatchPatternKind,
		LogicalMatchSelector,
		MatchBinding,
		MatchOp,
	},
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	stats::{DegreeDirection, DegreeHistogram, DegreeHistogramKey, LabelStatisticKey},
};

/// Selects a connected, stable pattern order using shared cardinality and cost
/// primitives.
pub(in crate::gql) fn plan_match(
	statement: &LogicalMatch,
	snapshot: &PlannerSnapshot,
	deterministic: bool,
) -> FerrosGrynnResult<PhysicalMatchPlan> {
	let graph = snapshot.graph(statement.graph.id()).ok_or_else(|| {
		FerrosGrynnError::graph_does_not_exist(statement.graph.name().to_string())
	})?;
	let mut remaining = (0..statement.patterns.len()).collect::<Vec<_>>();
	let mut known = BTreeSet::new();
	let mut pattern_order = Vec::with_capacity(remaining.len());
	let mut cardinality = Cardinality::exact(1);
	let mut cost = PlanCost::default();
	let mut warnings = Vec::new();
	let mut pattern_estimates = vec![Cardinality::exact(0); statement.patterns.len()];
	let mut pattern_costs = vec![PlanCost::default(); statement.patterns.len()];
	while !remaining.is_empty() {
		let mut best = None;
		for (position, pattern_index) in remaining.iter().copied().enumerate() {
			let pattern = &statement.patterns[pattern_index];
			let connected = pattern_columns(pattern).any(|column| known.contains(&column));
			let (estimate, candidate_cost, candidate_warnings) =
				estimate_pattern(pattern, graph, snapshot, statement.graph.id());
			let key = (
				!connected && !known.is_empty(),
				candidate_cost.total(),
				estimate.expected,
				pattern.original_index,
			);
			if best.as_ref().is_none_or(|(_position, best_key, _, _, _)| key < *best_key) {
				best = Some((position, key, estimate, candidate_cost, candidate_warnings));
			}
		}
		let (position, _key, estimate, candidate_cost, candidate_warnings) = best
			.ok_or_else(|| FerrosGrynnError::invalid_argument("MATCH planner has no candidate"))?;
		let pattern_index = remaining.remove(position);
		pattern_estimates[pattern_index] = estimate;
		pattern_costs[pattern_index] = candidate_cost;
		known.extend(pattern_columns(&statement.patterns[pattern_index]));
		pattern_order.push(pattern_index);
		cardinality = if pattern_order.len() == 1 {
			estimate
		} else {
			Cardinality::new(
				0,
				cardinality.expected.saturating_mul(estimate.expected.max(1)),
				cardinality.upper.saturating_mul(estimate.upper.max(1)),
			)
		};
		cost = cost.saturating_add(candidate_cost);
		warnings.extend(candidate_warnings);
	}
	warnings.sort_by_key(|warning| format!("{warning:?}"));
	warnings.dedup();
	let nodes = match_nodes(
		statement,
		graph,
		&pattern_order,
		&pattern_estimates,
		&pattern_costs,
		cardinality,
		deterministic,
	);
	Ok(PhysicalMatchPlan {
		pattern_order,
		nodes,
		cardinality,
		cost,
		warnings,
	})
}

fn match_nodes(
	statement: &LogicalMatch,
	graph: &PlannerGraph,
	pattern_order: &[usize],
	pattern_estimates: &[Cardinality],
	pattern_costs: &[PlanCost],
	final_cardinality: Cardinality,
	deterministic: bool,
) -> Vec<PhysicalMatchNode> {
	let mut nodes = Vec::new();
	for (execution_index, pattern_index) in pattern_order.iter().copied().enumerate() {
		let estimate = pattern_estimates[pattern_index];
		if execution_index != 0 {
			push_match_node(
				&mut nodes,
				Some(pattern_index),
				None,
				MatchOperator::MatchJoin,
				estimate,
				PlanCost::default(),
			);
		}
		match &statement.patterns[pattern_index].kind {
			LogicalMatchPatternKind::Linear(pattern) => {
				push_match_node(
					&mut nodes,
					Some(pattern_index),
					None,
					MatchOperator::MatchSeed,
					estimate,
					pattern_costs[pattern_index],
				);
				for (operation_index, operation) in pattern.ops.iter().enumerate() {
					let (filters, direction) = match operation {
						MatchOp::Expand(expand) => (&expand.filters, expand.direction),
						MatchOp::PathExpand(expand) => (&expand.filters, expand.direction),
					};
					let scoped = graph.reference.kind() == crate::graph::GraphKind::Metagraph
						|| filters.contains(&crate::syn::MatchPathFilter::CrossLevel);
					if scoped {
						push_match_node(
							&mut nodes,
							Some(pattern_index),
							Some(operation_index),
							MatchOperator::ResolveNestedEndpoint,
							estimate,
							PlanCost::default(),
						);
					}
					let operator = match operation {
						MatchOp::Expand(expand)
							if expand
								.filters
								.contains(&crate::syn::MatchPathFilter::HyperEdges) =>
						{
							MatchOperator::HyperExpand {
								direction,
							}
						}
						MatchOp::Expand(_) if statement.atom => MatchOperator::AtomExpand {
							direction,
						},
						MatchOp::Expand(_) => MatchOperator::FixedExpand {
							direction,
						},
						MatchOp::PathExpand(expand) => match expand.objective {
							Some(crate::syn::MatchPathObjective::Shortest) => {
								MatchOperator::BreadthFirstShortest {
									direction,
									mode: expand.mode,
									min: expand.min,
									max: expand.max,
								}
							}
							Some(crate::syn::MatchPathObjective::WeightedShortest) => {
								MatchOperator::DijkstraShortest {
									direction,
									mode: expand.mode,
									min: expand.min,
									max: expand.max,
								}
							}
							None => MatchOperator::PathEnumerate {
								direction,
								mode: expand.mode,
								min: expand.min,
								max: expand.max,
							},
						},
					};
					push_match_node(
						&mut nodes,
						Some(pattern_index),
						Some(operation_index),
						operator,
						estimate,
						PlanCost::default(),
					);
				}
			}
			LogicalMatchPatternKind::Hyper(_) => push_match_node(
				&mut nodes,
				Some(pattern_index),
				None,
				MatchOperator::HyperContains,
				estimate,
				pattern_costs[pattern_index],
			),
			LogicalMatchPatternKind::Within(pattern) => push_match_node(
				&mut nodes,
				Some(pattern_index),
				None,
				if pattern.min == 1 && pattern.max == 1 {
					MatchOperator::WithinJoin
				} else {
					MatchOperator::TransitiveWithin {
						min: pattern.min,
						max: pattern.max,
					}
				},
				estimate,
				pattern_costs[pattern_index],
			),
		}
	}
	if statement.optional {
		push_match_node(
			&mut nodes,
			None,
			None,
			MatchOperator::OptionalFallback,
			final_cardinality,
			PlanCost::default(),
		);
	}
	if statement.binding_distinct {
		push_match_node(
			&mut nodes,
			None,
			None,
			MatchOperator::BindingDistinct,
			final_cardinality,
			PlanCost::default(),
		);
	}
	push_match_node(
		&mut nodes,
		None,
		None,
		MatchOperator::MatchProject,
		final_cardinality,
		PlanCost::default(),
	);
	if statement.projection_distinct {
		push_match_node(
			&mut nodes,
			None,
			None,
			MatchOperator::ProjectionDistinct,
			final_cardinality,
			PlanCost::default(),
		);
	}
	if !statement.order.is_empty() || deterministic {
		push_match_node(
			&mut nodes,
			None,
			None,
			MatchOperator::MatchOrder,
			final_cardinality,
			PlanCost::default(),
		);
	}
	nodes
}

fn push_match_node(
	nodes: &mut Vec<PhysicalMatchNode>,
	pattern_index: Option<usize>,
	operation_index: Option<usize>,
	operator: MatchOperator,
	cardinality: Cardinality,
	cost: PlanCost,
) {
	nodes.push(PhysicalMatchNode {
		node_id: u32::try_from(nodes.len()).unwrap_or(u32::MAX),
		pattern_index,
		operation_index,
		operator,
		cardinality,
		cost,
	});
}

fn estimate_pattern(
	pattern: &LogicalMatchPattern,
	graph: &PlannerGraph,
	snapshot: &PlannerSnapshot,
	graph_id: crate::graph::GraphId,
) -> (Cardinality, PlanCost, Vec<PlanWarning>) {
	let mut warnings = Vec::new();
	let pattern = match &pattern.kind {
		LogicalMatchPatternKind::Linear(pattern) => pattern,
		LogicalMatchPatternKind::Hyper(pattern) => {
			let mut rows = binding_cardinality(&pattern.edge, graph, snapshot);
			for endpoint in pattern.contains.iter().flat_map(|group| &group.endpoints) {
				let endpoint_rows = binding_cardinality(endpoint, graph, snapshot);
				rows = Cardinality::new(
					0,
					rows.expected.saturating_mul(endpoint_rows.expected.max(1)),
					rows.upper.saturating_mul(endpoint_rows.upper.max(1)),
				);
			}
			warnings.push(PlanWarning::ResourceRisk(
				"hyperedge endpoint fanout uses a conservative estimate".into(),
			));
			return (rows, scan_cost(rows, 1, CostWeights::default()), warnings);
		}
		LogicalMatchPatternKind::Within(pattern) => {
			let owner = binding_cardinality(&pattern.owner, graph, snapshot);
			let child = binding_cardinality(&pattern.child, graph, snapshot);
			warnings.push(PlanWarning::ResourceRisk(
				"containment relation may require a visible-meta scan".into(),
			));
			let rows = Cardinality::new(
				0,
				owner.expected.saturating_mul(child.expected.max(1)),
				owner
					.upper
					.saturating_mul(child.upper.max(1))
					.saturating_mul(u64::from(pattern.max)),
			);
			return (rows, scan_cost(rows, 1, CostWeights::default()), warnings);
		}
	};
	let mut rows = binding_cardinality(&pattern.start, graph, snapshot);
	let mut cost = scan_cost(rows, 1, CostWeights::default());
	for operation in &pattern.ops {
		let (source, direction, min_hops, max_hops) = match operation {
			MatchOp::Expand(expand) => (&expand.source, expand.direction, 1, 1),
			MatchOp::PathExpand(expand) => {
				(&expand.source, expand.direction, expand.min, expand.max)
			}
		};
		let (expected_degree, maximum_degree) =
			degree_estimate(graph, snapshot, graph_id, source, direction, &mut warnings);
		let expected = path_rows(rows.expected, expected_degree, min_hops, max_hops);
		let upper = path_rows(rows.upper, maximum_degree.max(expected_degree), min_hops, max_hops);
		let next = Cardinality::new(0, expected, upper.max(expected));
		cost = cost.saturating_add(lookup_cost(rows.expected.max(1), next, CostWeights::default()));
		rows = next;
	}
	(rows, cost, warnings)
}

fn binding_cardinality(
	binding: &MatchBinding,
	graph: &PlannerGraph,
	snapshot: &PlannerSnapshot,
) -> Cardinality {
	let domain_rows = match binding.domain {
		EndpointDomain::Node => graph.node_rows,
		EndpointDomain::Edge => graph.edge_rows,
	};
	let selector_rows = match &binding.selector {
		LogicalMatchSelector::Any => domain_rows,
		LogicalMatchSelector::Record(_) => 1,
		LogicalMatchSelector::Class(class) => snapshot
			.class(class)
			.and_then(|class| class.graph_rows.get(&graph.reference.id()).copied())
			.unwrap_or(domain_rows),
	};
	let rows = binding.label.as_ref().map_or(selector_rows, |label| {
		graph
			.labels
			.get(&LabelStatisticKey {
				kind: binding.domain.atom_kind(),
				label: label.as_ref().into(),
			})
			.map_or(0, |statistics| statistics.atom_count)
			.min(selector_rows)
	});
	if matches!(binding.selector, LogicalMatchSelector::Record(_)) {
		Cardinality::new(0, rows.min(1), 1)
	} else {
		Cardinality::exact(rows)
	}
}

fn degree_estimate(
	graph: &PlannerGraph,
	snapshot: &PlannerSnapshot,
	graph_id: crate::graph::GraphId,
	source: &MatchBinding,
	direction: crate::syn::MatchDirection,
	warnings: &mut Vec<PlanWarning>,
) -> (u64, u64) {
	let directions = match direction {
		crate::syn::MatchDirection::Forward => vec![DegreeDirection::Out],
		crate::syn::MatchDirection::Backward => vec![DegreeDirection::In],
		crate::syn::MatchDirection::Undirected => vec![DegreeDirection::Undirected],
		crate::syn::MatchDirection::Bidirectional => {
			vec![DegreeDirection::Out, DegreeDirection::In]
		}
	};
	let label = source.label.as_ref().map(|label| LabelStatisticKey {
		kind: source.domain.atom_kind(),
		label: label.as_ref().into(),
	});
	let population = graph.node_rows.saturating_add(graph.edge_rows);
	let dirty_limit = population.div_ceil(10).max(1024);
	let stale = graph.statistics.revision != graph.statistics.analyzed_revision
		&& graph.statistics.dirty_rows > dirty_limit;
	let mut expected = 0_u64;
	let mut maximum = 0_u64;
	for direction in directions {
		let key = DegreeHistogramKey {
			label: label.clone(),
			direction,
		};
		if stale {
			warnings.push(PlanWarning::StatisticsStale(
				format!("graph {} degree {direction:?}", graph.reference.name()).into_boxed_str(),
			));
		} else if let Some(histogram) = snapshot.degree_histograms.get(&(graph_id, key)) {
			let (mean, max) = histogram_degree(histogram);
			expected = expected.saturating_add(mean);
			maximum = maximum.saturating_add(max);
			continue;
		} else {
			warnings.push(PlanWarning::StatisticsMissing(
				format!("graph {} degree {direction:?}", graph.reference.name()).into_boxed_str(),
			));
		}
		let (total, fallback_max) = match direction {
			DegreeDirection::Out => (graph.total_out_degree, graph.maximum_out_degree),
			DegreeDirection::In => (graph.total_in_degree, graph.maximum_in_degree),
			DegreeDirection::Undirected => (
				graph.total_undirected_degree,
				graph.maximum_out_degree.max(graph.maximum_in_degree),
			),
		};
		expected = expected.saturating_add(total.div_ceil(graph.node_rows.max(1)));
		maximum = maximum.saturating_add(fallback_max.max(expected));
	}
	(expected, maximum)
}

fn histogram_degree(histogram: &DegreeHistogram) -> (u64, u64) {
	let mut count = 0_u64;
	let mut weighted = 0_u64;
	let mut maximum = 0_u64;
	for bucket in &histogram.buckets {
		let midpoint = u64::from(bucket.minimum).saturating_add(u64::from(bucket.maximum)) / 2;
		count = count.saturating_add(bucket.node_count);
		weighted = weighted.saturating_add(midpoint.saturating_mul(bucket.node_count));
		maximum = maximum.max(u64::from(bucket.maximum));
	}
	(weighted.div_ceil(count.max(1)), maximum)
}

fn path_rows(seed: u64, degree: u64, minimum: u32, maximum: u32) -> u64 {
	(minimum..=maximum).fold(0_u64, |total, hops| {
		let rows = (0..hops).fold(seed, |rows, _hop| rows.saturating_mul(degree));
		total.saturating_add(rows)
	})
}

fn pattern_columns(pattern: &LogicalMatchPattern) -> impl Iterator<Item = usize> + '_ {
	let mut columns = Vec::new();
	match &pattern.kind {
		LogicalMatchPatternKind::Linear(pattern) => {
			columns.extend(pattern.start.column);
			for operation in &pattern.ops {
				match operation {
					MatchOp::Expand(expand) => {
						columns.extend(expand.source.column);
						columns.extend(expand.edge.column);
						columns.extend(expand.target.column);
					}
					MatchOp::PathExpand(expand) => {
						columns.extend(expand.source.column);
						columns.extend(expand.edge.column);
						columns.extend(expand.target.column);
						columns.extend(expand.path_column);
					}
				}
			}
		}
		LogicalMatchPatternKind::Hyper(pattern) => {
			columns.extend(pattern.edge.column);
			for endpoint in pattern.contains.iter().flat_map(|group| &group.endpoints) {
				columns.extend(endpoint.column);
			}
		}
		LogicalMatchPatternKind::Within(pattern) => {
			columns.extend(pattern.child.column);
			columns.extend(pattern.owner.column);
		}
	}
	columns.into_iter()
}
