//! Fixed-point planning for the currently executable MATCH profile.

use std::collections::BTreeSet;

use super::{
	Cardinality,
	CostWeights,
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
	Ok(PhysicalMatchPlan {
		pattern_order,
		cardinality,
		cost,
		warnings,
	})
}

fn estimate_pattern(
	pattern: &LogicalMatchPattern,
	graph: &PlannerGraph,
	snapshot: &PlannerSnapshot,
	graph_id: crate::graph::GraphId,
) -> (Cardinality, PlanCost, Vec<PlanWarning>) {
	let mut warnings = Vec::new();
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
	let start = pattern.start.column;
	let operations = pattern.ops.iter().flat_map(|operation| match operation {
		MatchOp::Expand(expand) => {
			[expand.source.column, expand.edge.column, expand.target.column, None]
		}
		MatchOp::PathExpand(expand) => {
			[expand.source.column, expand.edge.column, expand.target.column, expand.path_column]
		}
	});
	start.into_iter().chain(operations.flatten())
}
