//! MVCC-owned planner statistics for normalized base atoms and graph-effective
//! occurrences.

/// Bulk construction of planner statistics.
mod analyze;
mod bounded_histogram;
mod delta;
/// Statistics scopes, counts and histogram metadata.
mod model;
/// Snapshot statistics storage and transactional deltas.
mod store;

#[cfg(test)]
pub(crate) use analyze::analyze_database;
pub(crate) use analyze::{
	AnalyzeMethod,
	analyze_base_with_method_at,
	analyze_database_at,
	analyze_database_with_method_at,
	analyze_graph_with_method_at,
};
pub(crate) use delta::effective_property_count;
pub use model::{
	BaseStatistics,
	DegreeBucket,
	DegreeDirection,
	DegreeHistogram,
	DegreeHistogramKey,
	GraphStatistics,
	HistogramMethod,
	LabelStatistic,
	LabelStatisticKey,
	PropertyBucket,
	PropertyHistogram,
	PropertyHistogramKey,
	StatisticField,
	StatisticsScope,
};
pub use store::StatisticsStore;

/// Maximum number of equi-depth buckets stored for one distribution.
pub const HISTOGRAM_BUCKETS: usize = 100;

#[cfg(test)]
/// Regression tests for stats behavior.
mod tests;
