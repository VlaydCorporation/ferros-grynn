//! Physical-plan orchestration for relational SELECT.

mod access;
mod join_order;

use std::collections::BTreeSet;

use access::{index_join_probe, plan_source, resolve_hints, sort_eliminated, source_cardinality};
use join_order::{
	enumerate_join_order,
	estimated_row_width,
	join_cardinality,
	join_keys_across,
	ready_join_predicates,
};

use super::{
	AppliedRule,
	Cardinality,
	CostWeights,
	IndexAccessKind,
	IndexDirection,
	JoinBuildSide,
	JoinOperator,
	PhysicalJoin,
	PhysicalSelectPlan,
	PhysicalSource,
	PlanCost,
	PlannerSnapshot,
	SourceOperator,
	estimate_predicate,
	join_cost,
};
use crate::{
	gql::ir::{LogicalSelect, LogicalSelectAccess, LogicalSelectSourceKind},
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	syn::SelectJoinKind,
};

/// Builds one concrete physical plan using canonical rule and candidate order.
pub(in crate::gql) fn plan_select(
	logical: &LogicalSelect,
	snapshot: &PlannerSnapshot,
	barrier_memory_bytes: usize,
) -> FerrosGrynnResult<PhysicalSelectPlan> {
	let weights = CostWeights::default();
	let sources_logical = std::iter::once(&logical.source)
		.chain(logical.joins.iter().map(|join| &join.source))
		.collect::<Vec<_>>();
	let hints = resolve_hints(logical, snapshot, &sources_logical)?;
	let (single_source_filtered, mut warnings) = estimate_predicate(
		logical,
		snapshot,
		source_cardinality(&logical.source, snapshot)?,
		logical.filter.as_ref(),
	);
	let mut sources = Vec::with_capacity(sources_logical.len());
	let mut source_cards = Vec::with_capacity(sources_logical.len());
	let mut source_offsets = Vec::with_capacity(sources_logical.len());
	let mut offset = 0;
	for (index, source) in sources_logical.iter().enumerate() {
		source_offsets.push(offset);
		let forced = hints.get(&index);
		let (operator, cardinality, cost, source_warnings) = plan_source(
			source,
			offset,
			logical,
			snapshot,
			forced,
			matches!(logical.access, LogicalSelectAccess::NoIndex),
			weights,
		)?;
		warnings.extend(source_warnings);
		source_cards.push(cardinality);
		sources.push(PhysicalSource {
			node_id: u32::try_from(index).map_err(|_| {
				FerrosGrynnError::invalid_argument("SELECT source count exceeds u32")
			})?,
			alias: source.alias.clone(),
			column_offset: offset,
			column_width: source.schema.columns.len(),
			operator,
			cardinality,
			cost,
		});
		offset = offset.saturating_add(source.schema.columns.len());
	}

	let execution_order = enumerate_join_order(
		logical,
		&source_cards,
		&sources,
		&source_offsets,
		barrier_memory_bytes,
		weights,
	);
	let mut joins = Vec::with_capacity(logical.joins.len());
	let first_source = execution_order.first().copied().unwrap_or(0);
	let mut cardinality = source_cards.get(first_source).copied().unwrap_or(Cardinality::exact(0));
	let mut total_cost =
		sources.get(first_source).map_or_else(PlanCost::default, |source| source.cost);
	let mut joined = BTreeSet::from([first_source]);
	let mut assigned_predicates = vec![false; logical.joins.len()];
	for (step, right_source) in execution_order.iter().copied().enumerate().skip(1) {
		let predicates = ready_join_predicates(
			logical,
			&joined,
			right_source,
			&source_offsets,
			&mut assigned_predicates,
		);
		let kind = predicates
			.iter()
			.map(|index| logical.joins[*index].kind)
			.find(|kind| *kind != SelectJoinKind::Inner)
			.unwrap_or(SelectJoinKind::Inner);
		let key_columns =
			join_keys_across(logical, &predicates, &joined, right_source, &source_offsets)
				.unwrap_or_default();
		let right = source_cards[right_source];
		let build_side = if kind == SelectJoinKind::Left || right.expected <= cardinality.expected {
			JoinBuildSide::Right
		} else {
			JoinBuildSide::Left
		};
		let build_rows = match build_side {
			JoinBuildSide::Left => cardinality.expected,
			JoinBuildSide::Right => right.expected,
		};
		let row_bytes = estimated_row_width(sources_logical[right_source]);
		let build_bytes = build_rows.saturating_mul(row_bytes);
		let index_probe = if cardinality.expected <= 128 && !key_columns.is_empty() {
			index_join_probe(
				sources_logical[right_source],
				right_source,
				&key_columns,
				&source_offsets,
				snapshot,
				hints.get(&right_source),
			)
		} else {
			None
		};
		if let Some((probe, name)) = &index_probe {
			sources[right_source].operator = SourceOperator::ScalarIndexScan {
				index: probe.index,
				name: name.clone(),
				access: IndexAccessKind::Exact,
				direction: IndexDirection::Forward,
			};
		}
		let exceeds_barrier = build_bytes > u64::try_from(barrier_memory_bytes).unwrap_or(u64::MAX);
		let operator = if index_probe.is_some() {
			JoinOperator::IndexNestedLoop
		} else if key_columns.is_empty() {
			JoinOperator::BlockNestedLoop
		} else if exceeds_barrier {
			JoinOperator::PartitionedHash
		} else {
			JoinOperator::Hash
		};
		if exceeds_barrier && index_probe.is_none() && !key_columns.is_empty() {
			warnings.push(super::PlanWarning::ResourceRisk(
				format!(
					"join build estimates {build_bytes} bytes above the \
					 {barrier_memory_bytes}-byte barrier"
				)
				.into_boxed_str(),
			));
		}
		let output = join_cardinality(cardinality, right, kind, !key_columns.is_empty());
		let cost = join_cost(cardinality, right, row_bytes, barrier_memory_bytes, weights);
		total_cost = total_cost.saturating_add(sources[right_source].cost).saturating_add(cost);
		joins.push(PhysicalJoin {
			node_id: u32::try_from(sources.len() + step - 1).map_err(|_| {
				FerrosGrynnError::invalid_argument("SELECT plan node count exceeds u32")
			})?,
			left_sources: execution_order[..step].to_vec(),
			right_source,
			predicates,
			kind,
			key_columns,
			operator,
			build_side,
			index_probe: index_probe.map(|(probe, _name)| probe),
			cardinality: output,
			cost,
		});
		joined.insert(right_source);
		cardinality = output;
	}
	if logical.filter.is_some() {
		total_cost.expression = total_cost
			.expression
			.saturating_add(cardinality.expected.saturating_mul(weights.expression));
		cardinality = if logical.joins.is_empty() {
			single_source_filtered
		} else {
			estimate_predicate(logical, snapshot, cardinality, logical.filter.as_ref()).0
		};
	}
	let pushed_filter = logical.filter.is_some()
		&& logical.joins.is_empty()
		&& !matches!(logical.source.kind, LogicalSelectSourceKind::Derived(_));
	let pushed_limit = safe_limit_pushdown(logical);
	let sort_eliminated = sort_eliminated(logical, snapshot, &sources);
	let mut rules = Vec::new();
	if pushed_filter {
		rules.push(AppliedRule::PredicatePushdown);
	}
	if projection_pruned(logical) {
		rules.push(AppliedRule::ProjectionPruning);
		rules.push(AppliedRule::LateMaterialization);
	}
	if pushed_limit {
		rules.push(AppliedRule::LimitPushdown);
	}
	if matches!(
		sources.first().map(|source| &source.operator),
		Some(SourceOperator::CountLookup { .. })
	) {
		rules.push(AppliedRule::CountRewrite);
	}
	if sort_eliminated {
		rules.push(AppliedRule::SortElimination);
	}
	Ok(PhysicalSelectPlan {
		sources,
		execution_order,
		joins,
		rules,
		pushed_filter,
		pushed_limit,
		requires_sort: !logical.order.is_empty() && !sort_eliminated,
		cardinality,
		cost: total_cost,
		warnings,
	})
}

fn safe_limit_pushdown(select: &LogicalSelect) -> bool {
	select.limit.is_some()
		&& select.joins.is_empty()
		&& select.group.is_none()
		&& select.split.is_empty()
		&& select.order.is_empty()
		&& !select.only
		&& !matches!(select.source.kind, LogicalSelectSourceKind::Derived(_))
}

fn projection_pruned(select: &LogicalSelect) -> bool {
	!select
		.projection
		.iter()
		.any(|item| matches!(item, crate::gql::ir::LogicalSelectItem::Wildcard(_)))
}
