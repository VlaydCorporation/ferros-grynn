//! Read-only catalog and statistics projection captured for one statement.

use std::collections::{BTreeMap, BTreeSet};

use crate::{
	datastore::{
		ClassId,
		ClassIndexKind,
		ClassIndexState,
		IndexId,
		IndexScope,
		SchemaId,
		SchemaVersion,
	},
	graph::{GraphId, GraphRef},
	stats::{
		DegreeHistogram,
		DegreeHistogramKey,
		HistogramMethod,
		LabelStatistic,
		LabelStatisticKey,
		PropertyHistogram,
		PropertyHistogramKey,
		StatisticsScope,
	},
	values::{Text, Type},
};

/// One property histogram requested by the current logical plan.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct PlannerPropertyDemand {
	/// Owning class name resolved by typed analysis.
	pub(crate) class: Box<str>,
	/// Declared field name.
	pub(crate) field: Box<str>,
	/// Exact base or graph-effective population.
	pub(crate) scope: StatisticsScope,
}

/// Lazy statistics requested while constructing a planner snapshot.
#[derive(Debug, Clone, Default)]
pub(crate) struct PlannerDemand {
	/// Atom classes participating in the logical plan.
	pub(crate) classes: BTreeSet<Box<str>>,
	/// Exact graphs participating in the logical plan.
	pub(crate) graphs: BTreeSet<GraphId>,
	/// Whether an unbounded graph-catalog source requires every visible graph.
	pub(crate) all_graphs: bool,
	/// Property distributions used by predicates, grouping, or ordering.
	pub(crate) properties: BTreeSet<PlannerPropertyDemand>,
	/// Degree distributions used by current MATCH estimates.
	pub(crate) degrees: BTreeMap<GraphId, BTreeSet<DegreeHistogramKey>>,
}

/// One declared field retained by the planner.
#[derive(Debug, Clone)]
pub(crate) struct PlannerField {
	/// Stable field identity.
	pub(crate) id: SchemaId,
	/// Class-local field name.
	pub(crate) name: Text,
	/// Declared value type.
	pub(crate) value_type: Type,
}

/// Transaction-visible index definition and physical availability.
#[derive(Debug, Clone)]
pub(crate) struct PlannerIndex {
	/// Stable logical index identity.
	pub(crate) id: IndexId,
	/// Definition version.
	pub(crate) version: SchemaVersion,
	/// Class-local user-facing name.
	pub(crate) name: Text,
	/// Standard, Unique, or Count contract.
	pub(crate) kind: ClassIndexKind,
	/// Ready, stale, or prepared state.
	pub(crate) state: ClassIndexState,
	/// Base or exact graph-effective scope.
	pub(crate) scope: IndexScope,
	/// Composite fields in key order.
	pub(crate) fields: Vec<Text>,
	/// Whether the immutable snapshot contains a matching physical binding.
	pub(crate) physical_binding: bool,
	/// Exact Count value when this is an available Count index.
	pub(crate) exact_count: Option<u64>,
}

/// Class metadata and exact cardinalities used by source planning.
#[derive(Debug, Clone)]
pub(crate) struct PlannerClass {
	/// Stable class identity.
	pub(crate) id: ClassId,
	/// Definition version.
	pub(crate) version: SchemaVersion,
	/// Case-sensitive class name.
	pub(crate) name: Text,
	/// Declared fields in catalog order.
	pub(crate) fields: Vec<PlannerField>,
	/// Declared indexes in catalog order.
	pub(crate) indexes: Vec<PlannerIndex>,
	/// Exact database-global atom count.
	pub(crate) base_rows: u64,
	/// Exact graph-effective occurrence count by graph.
	pub(crate) graph_rows: BTreeMap<GraphId, u64>,
}

/// Scalar freshness information shared by all histograms in one scope.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct PlannerStatisticsMeta {
	/// Revision represented by exact counters.
	pub(crate) revision: u64,
	/// Revision represented by bulk distributions.
	pub(crate) analyzed_revision: u64,
	/// Changed rows since bulk analysis.
	pub(crate) dirty_rows: u64,
}

/// Graph catalog identity and exact topology counters.
#[derive(Debug, Clone)]
pub(crate) struct PlannerGraph {
	/// Public graph reference.
	pub(crate) reference: GraphRef,
	/// Graph schema version.
	pub(crate) version: SchemaVersion,
	/// Histogram freshness for this graph-effective population.
	pub(crate) statistics: PlannerStatisticsMeta,
	/// Exact node occurrence count.
	pub(crate) node_rows: u64,
	/// Exact edge occurrence count.
	pub(crate) edge_rows: u64,
	/// Exact outgoing incidence total.
	pub(crate) total_out_degree: u64,
	/// Exact incoming incidence total.
	pub(crate) total_in_degree: u64,
	/// Exact undirected incidence total.
	pub(crate) total_undirected_degree: u64,
	/// Exact or conservative maximum outgoing degree.
	pub(crate) maximum_out_degree: u64,
	/// Exact or conservative maximum incoming degree.
	pub(crate) maximum_in_degree: u64,
	/// Exact graph-local label counters.
	pub(crate) labels: BTreeMap<LabelStatisticKey, LabelStatistic>,
}

/// One demanded histogram together with its build method.
#[derive(Debug, Clone)]
pub(crate) struct PlannerPropertyHistogram {
	/// Exact resolved identity.
	pub(crate) key: PropertyHistogramKey,
	/// Exact or sampled distribution.
	pub(crate) histogram: PropertyHistogram,
	/// Cached method for compact EXPLAIN projection.
	pub(crate) method: HistogramMethod,
}

/// Planner-owned projection of one immutable statement snapshot.
#[derive(Debug, Clone, Default)]
pub(crate) struct PlannerSnapshot {
	/// Database commit timestamp captured for the statement.
	pub(crate) revision: u64,
	/// Base histogram freshness.
	pub(crate) base_statistics: PlannerStatisticsMeta,
	/// Classes keyed by exact catalog name.
	pub(crate) classes: BTreeMap<Text, PlannerClass>,
	/// Graphs keyed by stable identity.
	pub(crate) graphs: BTreeMap<GraphId, PlannerGraph>,
	/// Only property histograms requested by [`PlannerDemand`].
	pub(crate) property_histograms: BTreeMap<PropertyHistogramKey, PlannerPropertyHistogram>,
	/// Only degree histograms requested by [`PlannerDemand`].
	pub(crate) degree_histograms: BTreeMap<(GraphId, DegreeHistogramKey), DegreeHistogram>,
}

impl PlannerSnapshot {
	/// Returns exact class metadata by analyzed name.
	pub(crate) fn class(&self, name: &str) -> Option<&PlannerClass> {
		self.classes.get(name)
	}

	/// Returns one transaction-visible graph projection.
	pub(crate) fn graph(&self, graph: GraphId) -> Option<&PlannerGraph> {
		self.graphs.get(&graph)
	}

	/// Removes rebuild-sensitive distributions for strict replay planning.
	/// Exact catalog identities and counters remain available.
	pub(crate) fn deterministic_projection(&self) -> Self {
		let mut snapshot = self.clone();
		snapshot.property_histograms.clear();
		snapshot.degree_histograms.clear();
		snapshot
	}
}
