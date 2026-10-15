//! Integer cardinality estimates and calibrated cost components.

use super::{
	Cardinality,
	PlanCost,
	PlanWarning,
	PlannerSnapshot,
	PlannerStatisticsMeta,
	demand::property_demand,
};
use crate::{
	gql::ir::{LogicalSelect, TypedExpr, TypedExprKind},
	stats::{HistogramMethod, PropertyHistogramKey, StatisticsScope},
	syn::BinaryOperator,
	values::{
		Value,
		storage::{EncodeContext, encode_index_key},
	},
};

/// Fixed-point cost weights. O12 may change these values only from measured
/// microprobes without changing the cost representation.
#[derive(Debug, Clone, Copy)]
pub struct CostWeights {
	/// Source startup associated with this cost weights.
	pub(crate) source_startup: u64,
	/// Sequential row associated with this cost weights.
	pub(crate) sequential_row: u64,
	/// Random lookup associated with this cost weights.
	pub(crate) random_lookup: u64,
	/// Expression associated with this cost weights.
	pub(crate) expression: u64,
	/// Memory kib associated with this cost weights.
	pub(crate) memory_kib: u64,
	/// Spill kib associated with this cost weights.
	pub(crate) spill_kib: u64,
	/// Uncertainty associated with this cost weights.
	pub(crate) uncertainty: u64,
}

impl Default for CostWeights {
	/// Constructs the default initial state of `CostWeights`.
	fn default() -> Self {
		Self {
			source_startup: 64,
			sequential_row: 8,
			random_lookup: 256,
			expression: 3,
			memory_kib: 1,
			spill_kib: 8,
			uncertainty: 32,
		}
	}
}

/// Returns the cost of one sequential source scan.
pub fn scan_cost(rows: Cardinality, expressions: u64, weights: CostWeights) -> PlanCost {
	PlanCost {
		startup: weights.source_startup,
		sequential: rows.expected.saturating_mul(weights.sequential_row),
		expression: rows.expected.saturating_mul(expressions).saturating_mul(weights.expression),
		risk: rows.upper.saturating_sub(rows.expected).saturating_mul(weights.uncertainty),
		..PlanCost::default()
	}
}

/// Returns the fixed cost of primary or secondary random probes.
pub fn lookup_cost(probes: u64, rows: Cardinality, weights: CostWeights) -> PlanCost {
	PlanCost {
		startup: weights.source_startup,
		random: probes.saturating_mul(weights.random_lookup),
		expression: rows.expected.saturating_mul(weights.expression),
		risk: rows.upper.saturating_sub(rows.expected).saturating_mul(weights.uncertainty),
		..PlanCost::default()
	}
}

/// Returns the cost of seeking into an ordered index and then walking the
/// matching leaf range. A range is not one random lookup per returned row.
pub fn range_lookup_cost(seeks: u64, rows: Cardinality, weights: CostWeights) -> PlanCost {
	PlanCost {
		startup: weights.source_startup,
		random: seeks.saturating_mul(weights.random_lookup),
		sequential: rows.expected.saturating_mul(weights.sequential_row),
		expression: rows.expected.saturating_mul(weights.expression),
		risk: rows.upper.saturating_sub(rows.expected).saturating_mul(weights.uncertainty),
		..PlanCost::default()
	}
}

/// Returns join memory/spill cost for an estimated build side.
pub fn join_cost(
	left: Cardinality,
	right: Cardinality,
	row_bytes: u64,
	barrier_bytes: usize,
	weights: CostWeights,
) -> PlanCost {
	let build_rows = left.expected.min(right.expected);
	let build_bytes = build_rows.saturating_mul(row_bytes);
	let barrier_bytes = u64::try_from(barrier_bytes).unwrap_or(u64::MAX);
	let spill_bytes = build_bytes.saturating_sub(barrier_bytes);
	PlanCost {
		startup: weights.source_startup,
		sequential: left
			.expected
			.saturating_add(right.expected)
			.saturating_mul(weights.sequential_row),
		memory: build_bytes.div_ceil(1024).saturating_mul(weights.memory_kib),
		spill: spill_bytes.div_ceil(1024).saturating_mul(weights.spill_kib),
		risk: left
			.upper
			.saturating_sub(left.expected)
			.saturating_add(right.upper.saturating_sub(right.expected))
			.saturating_mul(weights.uncertainty),
		..PlanCost::default()
	}
}

/// Estimates a predicate while retaining correctness-independent uncertainty
/// and observable histogram fallback reasons.
pub(in crate::gql) fn estimate_predicate(
	select: &LogicalSelect,
	snapshot: &PlannerSnapshot,
	input: Cardinality,
	predicate: Option<&TypedExpr>,
) -> (Cardinality, Vec<PlanWarning>) {
	let Some(predicate) = predicate else {
		return (input, Vec::new());
	};
	let mut warnings = Vec::new();
	let cardinality = estimate_expression(select, snapshot, input, predicate, &mut warnings);
	(cardinality, warnings)
}

/// Estimates expression.
fn estimate_expression(
	select: &LogicalSelect,
	snapshot: &PlannerSnapshot,
	input: Cardinality,
	expression: &TypedExpr,
	warnings: &mut Vec<PlanWarning>,
) -> Cardinality {
	if let TypedExprKind::Binary {
		left,
		op: BinaryOperator::And,
		right,
	} = &expression.kind
	{
		let left = estimate_expression(select, snapshot, input, left, warnings);
		return estimate_expression(select, snapshot, left, right, warnings);
	}
	if let Some(column) = spatial_column(expression)
		&& let Some(query) = super::spatial_query(expression, column)
		&& let Some(demand) = property_demand(select, column, None)
		&& let Some(class) = snapshot.class(&demand.class)
		&& let Some(field) =
			class.fields.iter().find(|field| field.name.as_str() == demand.field.as_ref())
	{
		let key = PropertyHistogramKey {
			class_id: class.id,
			field_id: field.id,
			scope: demand.scope,
		};
		let Some(histogram) = snapshot.property_histograms.get(&key) else {
			warnings.push(PlanWarning::StatisticsMissing(
				format!("{}.{} MBR sample", demand.class, demand.field).into(),
			));
			return conservative_fraction(input, 1, 3);
		};
		let meta = statistics_meta(snapshot, demand.scope);
		if meta.revision != meta.analyzed_revision
			&& meta.dirty_rows > input.upper.div_ceil(10).max(1024)
		{
			warnings.push(PlanWarning::StatisticsStale(
				format!("{}.{} MBR sample", demand.class, demand.field).into(),
			));
			return conservative_fraction(input, 1, 3);
		}
		if let TypedExprKind::Constant(Value::Geometry(geometry)) = &query.kind
			&& let Ok(Some(mbr)) = geometry.mbr()
			&& !histogram.histogram.mbr_sample.is_empty()
		{
			let sample = &histogram.histogram.mbr_sample;
			let hits = sample
				.iter()
				.filter(|candidate| {
					candidate[0] <= mbr[2]
						&& mbr[0] <= candidate[2]
						&& candidate[1] <= mbr[3]
						&& mbr[1] <= candidate[3]
				})
				.count() as u64;
			let population = histogram.histogram.mbr_population;
			let observations = u64::from(histogram.histogram.sample_count).max(1);
			let expected = input
				.expected
				.saturating_mul(population)
				.saturating_mul(hits)
				.div_ceil(observations.saturating_mul(sample.len() as u64));
			return Cardinality::new(
				0,
				expected,
				expected
					.saturating_mul(4)
					.max(input.upper.div_ceil(sample.len() as u64))
					.min(input.upper),
			);
		}
		return conservative_fraction(input, 1, 3);
	}
	let Some((column, comparison, constant)) = scalar_column_comparison(expression) else {
		return conservative_fraction(input, 1, 3);
	};
	let Some(demand) = property_demand(select, column, None) else {
		return conservative_fraction(input, 1, 3);
	};
	let Some(class) = snapshot.class(&demand.class) else {
		return conservative_fraction(input, 1, 3);
	};
	let Some(field) =
		class.fields.iter().find(|field| field.name.as_str() == demand.field.as_ref())
	else {
		return conservative_fraction(input, 1, 3);
	};
	let key = PropertyHistogramKey {
		class_id: class.id,
		field_id: field.id,
		scope: demand.scope,
	};
	let Some(histogram) = snapshot.property_histograms.get(&key) else {
		warnings.push(PlanWarning::StatisticsMissing(
			format!("{}.{}", demand.class, demand.field).into(),
		));
		return conservative_fraction(input, 1, 3);
	};
	let metadata = statistics_meta(snapshot, demand.scope);
	let dirty_limit = input.upper.div_ceil(10).max(1024);
	if metadata.revision != metadata.analyzed_revision && metadata.dirty_rows > dirty_limit {
		warnings.push(PlanWarning::StatisticsStale(
			format!("{}.{}", demand.class, demand.field).into(),
		));
		return conservative_fraction(input, 1, 3);
	}
	let distribution = &histogram.histogram;
	let expected = constant
		.and_then(|value| {
			let mut context = EncodeContext::default();
			encode_index_key(&[(value, &field.value_type)], &mut context).ok().map(|key| {
				let sampled = histogram_cardinality(distribution, &key, comparison);
				scale_histogram_cardinality(distribution, sampled, input.expected)
			})
		})
		.unwrap_or_else(|| match comparison {
			ScalarEstimate::Equal => input.expected.div_ceil(distribution.distinct_count.max(1)),
			ScalarEstimate::Less
			| ScalarEstimate::LessOrEqual
			| ScalarEstimate::Greater
			| ScalarEstimate::GreaterOrEqual => input.expected.div_ceil(3),
		});
	let uncertainty = if metadata.dirty_rows == 0
		&& metadata.revision == metadata.analyzed_revision
		&& histogram.method == HistogramMethod::Exact
	{
		2
	} else if histogram.method == HistogramMethod::Approximate {
		8
	} else {
		4
	};
	Cardinality::new(
		0,
		expected.min(input.upper),
		expected.saturating_mul(uncertainty).min(input.upper),
	)
}

/// Locates candidate geometry columns; the shared proof checks polarity and
/// independence.
fn spatial_column(expression: &TypedExpr) -> Option<usize> {
	let operands: &[&TypedExpr] = match &expression.kind {
		TypedExprKind::Binary {
			left,
			right,
			..
		} => &[left, right],
		TypedExprKind::Builtin {
			arguments,
			..
		} if arguments.len() == 2 => &[&arguments[0], &arguments[1]],
		_ => return None,
	};
	operands.iter().find_map(|operand| {
		if let TypedExprKind::Column(column) = operand.kind {
			Some(column)
		} else {
			None
		}
	})
}

/// Returns the scale histogram cardinality result.
fn scale_histogram_cardinality(
	histogram: &crate::stats::PropertyHistogram,
	sampled: u64,
	population: u64,
) -> u64 {
	if histogram.method == HistogramMethod::Approximate {
		sampled.saturating_mul(population).div_ceil(u64::from(histogram.sample_count).max(1))
	} else {
		sampled
	}
}

/// Returns the statistics meta result.
fn statistics_meta(snapshot: &PlannerSnapshot, scope: StatisticsScope) -> PlannerStatisticsMeta {
	match scope {
		StatisticsScope::Base => snapshot.base_statistics,
		StatisticsScope::Effective {
			graph,
		} => snapshot.graph(graph).map_or_default(|graph| graph.statistics),
	}
}

#[derive(Clone, Copy, PartialEq, Eq)]
/// Scalar estimate alternatives.
enum ScalarEstimate {
	/// Equal case of scalar estimate.
	Equal,
	/// Less case of scalar estimate.
	Less,
	/// Less or equal case of scalar estimate.
	LessOrEqual,
	/// Greater case of scalar estimate.
	Greater,
	/// Greater or equal case of scalar estimate.
	GreaterOrEqual,
}

/// Returns the scalar column comparison result.
fn scalar_column_comparison(
	expression: &TypedExpr,
) -> Option<(usize, ScalarEstimate, Option<&Value>)> {
	let TypedExprKind::Binary {
		left,
		op,
		right,
	} = &expression.kind
	else {
		return None;
	};
	let comparison = match op {
		BinaryOperator::Equal | BinaryOperator::Is => ScalarEstimate::Equal,
		BinaryOperator::Less => ScalarEstimate::Less,
		BinaryOperator::LessOrEqual => ScalarEstimate::LessOrEqual,
		BinaryOperator::Greater => ScalarEstimate::Greater,
		BinaryOperator::GreaterOrEqual => ScalarEstimate::GreaterOrEqual,
		_ => return None,
	};
	match (&left.kind, &right.kind) {
		(TypedExprKind::Column(column), TypedExprKind::Constant(value)) => {
			Some((*column, comparison, Some(value)))
		}
		(TypedExprKind::Column(column), TypedExprKind::Slot(_)) => {
			Some((*column, comparison, None))
		}
		(TypedExprKind::Constant(value), TypedExprKind::Column(column)) => {
			Some((*column, reverse_comparison(comparison), Some(value)))
		}
		(TypedExprKind::Slot(_), TypedExprKind::Column(column)) => {
			Some((*column, reverse_comparison(comparison), None))
		}
		_ => None,
	}
}

/// Returns the reverse comparison result.
fn reverse_comparison(comparison: ScalarEstimate) -> ScalarEstimate {
	match comparison {
		ScalarEstimate::Equal => ScalarEstimate::Equal,
		ScalarEstimate::Less => ScalarEstimate::Greater,
		ScalarEstimate::LessOrEqual => ScalarEstimate::GreaterOrEqual,
		ScalarEstimate::Greater => ScalarEstimate::Less,
		ScalarEstimate::GreaterOrEqual => ScalarEstimate::LessOrEqual,
	}
}

/// Returns the histogram cardinality result.
fn histogram_cardinality(
	histogram: &crate::stats::PropertyHistogram,
	key: &[u8],
	comparison: ScalarEstimate,
) -> u64 {
	let total = histogram.buckets.iter().map(|bucket| bucket.frequency).sum::<u64>();
	match comparison {
		ScalarEstimate::Equal => histogram
			.buckets
			.iter()
			.find(|bucket| bucket.lower.as_slice() <= key && key <= bucket.upper.as_slice())
			.map_or(0, |bucket| bucket.frequency.div_ceil(bucket.distinct_count.max(1))),
		ScalarEstimate::Less | ScalarEstimate::LessOrEqual => histogram
			.buckets
			.iter()
			.map(|bucket| bucket_less_rows(bucket, key, comparison == ScalarEstimate::LessOrEqual))
			.sum(),
		ScalarEstimate::Greater | ScalarEstimate::GreaterOrEqual => {
			let excluded = histogram
				.buckets
				.iter()
				.map(|bucket| bucket_less_rows(bucket, key, comparison == ScalarEstimate::Greater))
				.sum::<u64>();
			total.saturating_sub(excluded)
		}
	}
}

/// Returns the bucket less rows result.
fn bucket_less_rows(bucket: &crate::stats::PropertyBucket, key: &[u8], inclusive: bool) -> u64 {
	if key < bucket.lower.as_slice() {
		return 0;
	}
	let equality = bucket.frequency.div_ceil(bucket.distinct_count.max(1));
	if key == bucket.lower.as_slice() {
		return if inclusive {
			equality
		} else {
			0
		};
	}
	if key > bucket.upper.as_slice() {
		return bucket.frequency;
	}
	if key == bucket.upper.as_slice() {
		return if inclusive {
			bucket.frequency
		} else {
			bucket.frequency.saturating_sub(equality)
		};
	}
	// Canonical byte keys do not support meaningful numeric interpolation. A
	// half-bucket estimate bounds the unavoidable equi-depth uncertainty.
	bucket.frequency.div_ceil(2)
}

/// Returns the conservative fraction result.
fn conservative_fraction(input: Cardinality, numerator: u64, denominator: u64) -> Cardinality {
	Cardinality::new(0, input.expected.saturating_mul(numerator).div_ceil(denominator), input.upper)
}

#[cfg(test)]
/// Regression tests for estimate behavior.
mod tests {
	use super::*;
	use crate::stats::{HistogramMethod, PropertyBucket, PropertyHistogram};

	/// Verifies equi depth boundaries estimate ranges without random lookup per
	/// row.
	#[test]
	fn equi_depth_boundaries_estimate_ranges_without_random_lookup_per_row() {
		let histogram = PropertyHistogram {
			mbr_sample: Vec::new(),
			mbr_population: 0,
			type_tag: 0,
			method: HistogramMethod::Exact,
			sample_count: 20,
			distinct_count: 20,
			null_fraction: 0.0,
			buckets: vec![
				PropertyBucket {
					lower: vec![0],
					upper: vec![9],
					frequency: 10,
					distinct_count: 10,
				},
				PropertyBucket {
					lower: vec![10],
					upper: vec![19],
					frequency: 10,
					distinct_count: 10,
				},
			],
		};
		assert_eq!(histogram_cardinality(&histogram, &[10], ScalarEstimate::Less), 10);
		assert_eq!(histogram_cardinality(&histogram, &[10], ScalarEstimate::LessOrEqual), 11);
		assert_eq!(histogram_cardinality(&histogram, &[10], ScalarEstimate::Greater), 9);
		assert_eq!(histogram_cardinality(&histogram, &[10], ScalarEstimate::GreaterOrEqual), 10);
		assert_eq!(histogram_cardinality(&histogram, &[10], ScalarEstimate::Equal), 1);
		let approximate = PropertyHistogram {
			method: HistogramMethod::Approximate,
			sample_count: 20,
			..histogram
		};
		assert_eq!(scale_histogram_cardinality(&approximate, 5, 100), 25);

		let weights = CostWeights::default();
		let rows = Cardinality::exact(100);
		assert!(
			range_lookup_cost(1, rows, weights).total() < lookup_cost(100, rows, weights).total()
		);
	}
}
