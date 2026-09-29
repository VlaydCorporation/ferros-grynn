//! MVCC-owned planner statistics for normalized base atoms and graph-effective
//! occurrences.

mod analyze;
mod bounded_histogram;
mod delta;
mod model;
mod store;

pub(crate) use analyze::{
	AnalyzeMethod,
	analyze_base_with_method_at,
	analyze_database,
	analyze_database_at,
	analyze_database_with_method_at,
	analyze_graph_with_method_at,
};
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
mod tests;
