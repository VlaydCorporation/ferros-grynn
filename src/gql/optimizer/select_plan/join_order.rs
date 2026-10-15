//! Bounded join enumeration, predicate assignment, and cardinality composition.

use std::collections::BTreeSet;

use super::super::{Cardinality, CostWeights, PhysicalSource, PlanCost, join_cost};
use crate::{
	gql::ir::{LogicalSelect, LogicalSelectSource, TypedExpr, TypedExprKind},
	syn::{BinaryOperator, SelectJoinKind},
};

#[derive(Clone)]
/// Candidate source order with the cardinality and cost used during join
/// enumeration.
struct JoinOrderState {
	/// Order associated with this join order state.
	order: Vec<usize>,
	/// Cardinality associated with this join order state.
	cardinality: Cardinality,
	/// Cost associated with this join order state.
	cost: PlanCost,
}

/// Returns the enumerate join order result.
pub(super) fn enumerate_join_order(
	logical: &LogicalSelect,
	cards: &[Cardinality],
	sources: &[PhysicalSource],
	offsets: &[usize],
	barrier_memory_bytes: usize,
	weights: CostWeights,
) -> Vec<usize> {
	if sources.len() <= 1 {
		return (0..sources.len()).collect();
	}
	let reorderable = reorderable_sources(logical);
	let mut order = if reorderable <= 2 {
		(0..reorderable).collect()
	} else if reorderable <= 8 {
		dp_join_order(logical, cards, sources, offsets, reorderable, barrier_memory_bytes, weights)
	} else {
		greedy_join_order(
			logical,
			cards,
			sources,
			offsets,
			reorderable,
			barrier_memory_bytes,
			weights,
		)
	};
	order.extend(reorderable..sources.len());
	order
}

/// Enumeration and predicate scheduling share the same INNER prefix boundary.
fn reorderable_sources(logical: &LogicalSelect) -> usize {
	logical
		.joins
		.iter()
		.take_while(|join| {
			join.kind == SelectJoinKind::Inner
				&& join.predicate.as_ref().and_then(equality_column_pairs).is_some()
		})
		.count()
		.saturating_add(1)
}

#[allow(
	clippy::too_many_arguments,
	reason = "bounded DP needs the source estimates and fixed resource model"
)]
/// Returns the dp join order result.
fn dp_join_order(
	logical: &LogicalSelect,
	cards: &[Cardinality],
	sources: &[PhysicalSource],
	offsets: &[usize],
	count: usize,
	barrier_memory_bytes: usize,
	weights: CostWeights,
) -> Vec<usize> {
	let state_count = 1_usize << count;
	let mut states = vec![None::<JoinOrderState>; state_count];
	for source in 0..count {
		states[1 << source] = Some(JoinOrderState {
			order: vec![source],
			cardinality: cards[source],
			cost: sources[source].cost,
		});
	}
	for mask in 1..state_count {
		let Some(state) = states[mask].clone() else {
			continue;
		};
		for right in 0..count {
			if mask & (1 << right) != 0
				|| !source_connects_to_mask(logical, offsets, right, mask, count)
			{
				continue;
			}
			let row_bytes = u64::try_from(sources[right].column_width)
				.unwrap_or(u64::MAX)
				.saturating_mul(32)
				.max(64);
			let join = join_cost(
				state.cardinality,
				cards[right],
				row_bytes,
				barrier_memory_bytes,
				weights,
			);
			let candidate_cardinality =
				join_cardinality(state.cardinality, cards[right], SelectJoinKind::Inner, true);
			let candidate_cost =
				state.cost.saturating_add(sources[right].cost).saturating_add(join);
			let mut candidate_order = state.order.clone();
			candidate_order.push(right);
			let next = mask | (1 << right);
			let replace = states[next].as_ref().is_none_or(|existing| {
				(candidate_cost.total(), candidate_cardinality.expected, &candidate_order)
					< (existing.cost.total(), existing.cardinality.expected, &existing.order)
			});
			if replace {
				states[next] = Some(JoinOrderState {
					order: candidate_order,
					cardinality: candidate_cardinality,
					cost: candidate_cost,
				});
			}
		}
	}
	states[state_count - 1].take().map_or_else(|| (0..count).collect(), |state| state.order)
}

#[allow(
	clippy::too_many_arguments,
	reason = "deterministic greedy planning shares the bounded cost inputs"
)]
/// Returns the greedy join order result.
fn greedy_join_order(
	logical: &LogicalSelect,
	cards: &[Cardinality],
	sources: &[PhysicalSource],
	offsets: &[usize],
	count: usize,
	barrier_memory_bytes: usize,
	weights: CostWeights,
) -> Vec<usize> {
	let first = (0..count)
		.min_by_key(|source| (sources[*source].cost.total(), cards[*source].expected, *source))
		.unwrap_or(0);
	let mut order = vec![first];
	let mut mask = 1_usize << first;
	let mut cardinality = cards[first];
	while order.len() < count {
		let candidate = (0..count)
			.filter(|right| mask & (1 << right) == 0)
			.min_by_key(|right| {
				let connected = source_connects_to_mask(logical, offsets, *right, mask, count);
				let row_bytes = u64::try_from(sources[*right].column_width)
					.unwrap_or(u64::MAX)
					.saturating_mul(32)
					.max(64);
				let cost =
					join_cost(cardinality, cards[*right], row_bytes, barrier_memory_bytes, weights)
						.saturating_add(sources[*right].cost);
				(!connected, cost.total(), cards[*right].expected, *right)
			})
			.unwrap_or(0);
		order.push(candidate);
		mask |= 1 << candidate;
		cardinality = join_cardinality(cardinality, cards[candidate], SelectJoinKind::Inner, true);
	}
	order
}

/// Returns whether the source connects to mask condition holds.
fn source_connects_to_mask(
	logical: &LogicalSelect,
	offsets: &[usize],
	right: usize,
	mask: usize,
	count: usize,
) -> bool {
	logical.joins.iter().take(count.saturating_sub(1)).any(|join| {
		join.predicate.as_ref().and_then(equality_column_pairs).is_some_and(|pairs| {
			pairs.into_iter().any(|(left, candidate)| {
				let left = source_for_column(logical, offsets, left);
				let candidate = source_for_column(logical, offsets, candidate);
				(left == Some(right) && candidate.is_some_and(|source| mask & (1 << source) != 0))
					|| (candidate == Some(right)
						&& left.is_some_and(|source| mask & (1 << source) != 0))
			})
		})
	})
}

/// Returns the ready join predicates result.
pub(super) fn ready_join_predicates(
	logical: &LogicalSelect,
	joined: &BTreeSet<usize>,
	right: usize,
	offsets: &[usize],
	assigned: &mut [bool],
) -> Vec<usize> {
	let mut ready = Vec::new();
	let reorderable = reorderable_sources(logical);
	for (index, join) in logical.joins.iter().enumerate() {
		if assigned[index] {
			continue;
		}
		let is_ready = if index + 1 >= reorderable {
			index + 1 == right
		} else if let Some(predicate) = &join.predicate {
			let sources = expression_sources(logical, offsets, predicate);
			(sources.contains(&right)
				&& sources.iter().all(|source| *source == right || joined.contains(source)))
				|| (right < reorderable && joined.len() + 1 == reorderable)
		} else {
			index + 1 == right
		};
		if is_ready {
			assigned[index] = true;
			ready.push(index);
		}
	}
	ready
}

/// Joins keys across.
pub(super) fn join_keys_across(
	logical: &LogicalSelect,
	predicates: &[usize],
	joined: &BTreeSet<usize>,
	right: usize,
	offsets: &[usize],
) -> Option<Vec<(usize, usize)>> {
	let mut keys = Vec::new();
	for index in predicates {
		let predicate = logical.joins[*index].predicate.as_ref()?;
		for (left, candidate) in equality_column_pairs(predicate)? {
			let left_source = source_for_column(logical, offsets, left)?;
			let candidate_source = source_for_column(logical, offsets, candidate)?;
			if joined.contains(&left_source) && candidate_source == right {
				keys.push((left, candidate));
			} else if joined.contains(&candidate_source) && left_source == right {
				keys.push((candidate, left));
			} else {
				return None;
			}
		}
	}
	(!keys.is_empty()).then_some(keys)
}

/// Returns the equality column pairs result.
fn equality_column_pairs(expression: &TypedExpr) -> Option<Vec<(usize, usize)>> {
	match &expression.kind {
		TypedExprKind::Binary {
			left,
			op: BinaryOperator::And,
			right,
		} => {
			let mut pairs = equality_column_pairs(left)?;
			pairs.extend(equality_column_pairs(right)?);
			Some(pairs)
		}
		TypedExprKind::Binary {
			left,
			op: BinaryOperator::Equal,
			right,
		} => match (&left.kind, &right.kind) {
			(TypedExprKind::Column(left), TypedExprKind::Column(right)) => {
				Some(vec![(*left, *right)])
			}
			_ => None,
		},
		_ => None,
	}
}

/// Returns the expression sources result.
fn expression_sources(
	logical: &LogicalSelect,
	offsets: &[usize],
	expression: &TypedExpr,
) -> BTreeSet<usize> {
	let mut columns = Vec::new();
	let mut conservative = false;
	match crate::gql::analysis::visit_typed_expression(expression, &mut |node| {
		match &node.kind {
			TypedExprKind::Column(column) => columns.push(*column),
			TypedExprKind::Subquery {
				..
			}
			| TypedExprKind::Closure(_)
			| TypedExprKind::Traversal(_) => conservative = true,
			_ => {}
		}
		Ok::<(), std::convert::Infallible>(())
	}) {
		Ok(()) => {}
		Err(never) => match never {},
	}
	if conservative {
		(0..offsets.len()).collect()
	} else {
		columns
			.into_iter()
			.filter_map(|column| source_for_column(logical, offsets, column))
			.collect()
	}
}

/// Returns the source for column result.
fn source_for_column(logical: &LogicalSelect, offsets: &[usize], column: usize) -> Option<usize> {
	let sources =
		std::iter::once(&logical.source).chain(logical.joins.iter().map(|join| &join.source));
	sources.enumerate().find_map(|(index, source)| {
		let start = offsets[index];
		let end = start.saturating_add(source.schema.columns.len());
		(start..end).contains(&column).then_some(index)
	})
}

/// Joins cardinality.
pub(super) fn join_cardinality(
	left: Cardinality,
	right: Cardinality,
	kind: SelectJoinKind,
	equality: bool,
) -> Cardinality {
	let product_upper = left.upper.saturating_mul(right.upper);
	if kind == SelectJoinKind::Cross {
		return Cardinality::new(
			left.lower.saturating_mul(right.lower),
			left.expected.saturating_mul(right.expected),
			product_upper,
		);
	}
	let expected = if equality {
		left.expected.max(right.expected)
	} else {
		left.expected.saturating_mul(right.expected).div_ceil(3)
	};
	let lower = if kind == SelectJoinKind::Left {
		left.lower
	} else {
		0
	};
	Cardinality::new(lower, expected.max(lower), product_upper.max(lower))
}

/// Returns the estimated row width result.
pub(super) fn estimated_row_width(source: &LogicalSelectSource) -> u64 {
	u64::try_from(source.schema.columns.len()).unwrap_or(u64::MAX).saturating_mul(32).max(64)
}
