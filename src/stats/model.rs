use std::collections::BTreeMap;

use crate::{
	datastore::{ClassId, SchemaId},
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	graph::{AtomKind, GraphId},
	values::{PropertyKeyId, Text, Type},
};

/// Base atoms or the effective view of one graph.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum StatisticsScope {
	/// Database-global base values.
	Base,
	/// Effective base-plus-overlay values of one graph.
	Effective {
		/// Graph whose occurrences define the population.
		graph: GraphId,
	},
}

/// Runtime key for one graph-local label statistic.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct LabelStatisticKey {
	/// Node or edge label namespace.
	pub kind: AtomKind,
	/// Label text resolved to a generation-local numeric ID by the disk codec.
	pub label: Text,
}

/// Exact cardinality and effective-property total of one graph label.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LabelStatistic {
	/// Exact number of labeled occurrences.
	pub atom_count: u64,
	/// Exact sum of effective property counts on those occurrences.
	pub property_count: u64,
}

/// Direction represented by one degree histogram.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[repr(u8)]
pub enum DegreeDirection {
	/// Outgoing directed incidence.
	Out = 0,
	/// Incoming directed incidence.
	In = 1,
	/// Undirected incidence.
	Undirected = 2,
}

impl DegreeDirection {
	/// Decodes the closed format-v1 direction set.
	pub const fn from_byte(value: u8) -> Option<Self> {
		match value {
			0 => Some(Self::Out),
			1 => Some(Self::In),
			2 => Some(Self::Undirected),
			_ => None,
		}
	}
}

/// Global or per-node-label degree distribution identity.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct DegreeHistogramKey {
	/// `None` addresses the whole graph.
	pub label: Option<LabelStatisticKey>,
	/// Degree direction.
	pub direction: DegreeDirection,
}

/// One exact range in an equi-depth degree histogram.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DegreeBucket {
	/// Inclusive lower degree.
	pub minimum: u32,
	/// Inclusive upper degree.
	pub maximum: u32,
	/// Number of nodes in this bucket.
	pub node_count: u64,
}

/// Bounded equi-depth degree distribution.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DegreeHistogram {
	/// Ordered non-empty buckets.
	pub buckets: Vec<DegreeBucket>,
}

/// Stable identity of one property distribution.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct PropertyHistogramKey {
	/// Owning class.
	pub class_id: ClassId,
	/// Stable field identity.
	pub field_id: SchemaId,
	/// Base or exact graph-effective scope.
	pub scope: StatisticsScope,
}

/// One exact range over canonical order-preserving value bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PropertyBucket {
	/// Inclusive lower value.
	pub lower: Vec<u8>,
	/// Inclusive upper value.
	pub upper: Vec<u8>,
	/// Number of non-null observations.
	pub frequency: u64,
	/// Number of distinct values.
	pub distinct_count: u64,
}

/// Algorithm used to build one property histogram.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum HistogramMethod {
	/// Full exact scan.
	Exact = 0,
	/// HLL plus deterministic reservoir sample.
	Approximate = 1,
}

impl HistogramMethod {
	/// Decodes the closed format-v1 method set.
	pub const fn from_byte(value: u8) -> Option<Self> {
		match value {
			0 => Some(Self::Exact),
			1 => Some(Self::Approximate),
			_ => None,
		}
	}
}

/// Property distribution for one class field and scope.
#[derive(Debug, Clone, PartialEq)]
pub struct PropertyHistogram {
	/// Format-v1 storage type tag.
	pub type_tag: u8,
	/// Build algorithm.
	pub method: HistogramMethod,
	/// Number of observations used by the builder.
	pub sample_count: u32,
	/// Exact or estimated distinct-value count.
	pub distinct_count: u64,
	/// Fraction of absent, NONE, or NULL observations.
	pub null_fraction: f32,
	/// Ordered equi-depth buckets.
	pub buckets: Vec<PropertyBucket>,
}

/// Resolved schema field eligible for histogram collection.
pub struct StatisticField<'field> {
	/// Owning class.
	pub class_id: ClassId,
	/// Stable field identity.
	pub field_id: SchemaId,
	/// Runtime property dictionary key.
	pub property_key: PropertyKeyId,
	/// Declared value type.
	pub value_type: &'field Type,
}

/// Database-global exact counters and Base histograms.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct BaseStatistics {
	/// Database revision represented by exact counters.
	pub revision: u64,
	/// Revision represented by bulk histograms.
	pub analyzed_revision: u64,
	/// Changed rows since bulk analysis.
	pub dirty_rows: u64,
	/// Exact live atom count by class.
	pub classes: BTreeMap<ClassId, u64>,
	/// Base property histograms.
	pub property_histograms: BTreeMap<PropertyHistogramKey, PropertyHistogram>,
}

impl BaseStatistics {
	/// Marks bulk distributions stale after a committed base-row change.
	pub fn mark_dirty(&mut self, changed_rows: u64, revision: u64) -> FerrosGrynnResult<()> {
		self.dirty_rows = checked_add(self.dirty_rows, changed_rows, "base dirty-row count")?;
		self.revision = revision;
		Ok(())
	}
}

/// Exact counters and bulk distributions for one graph revision.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct GraphStatistics {
	/// Graph revision represented by exact counters.
	pub revision: u64,
	/// Graph revision represented by bulk distributions.
	pub analyzed_revision: u64,
	/// Changed occurrences since bulk analysis.
	pub dirty_rows: u64,
	/// Exact node-occurrence count.
	pub node_count: u64,
	/// Exact edge-occurrence count.
	pub edge_count: u64,
	/// Exact outgoing incidence total.
	pub total_out_degree: u64,
	/// Exact incoming incidence total.
	pub total_in_degree: u64,
	/// Exact undirected incidence total.
	pub total_undirected_degree: u64,
	/// Exact or conservative outgoing maximum.
	pub maximum_out_degree: u64,
	/// Exact or conservative incoming maximum.
	pub maximum_in_degree: u64,
	/// Exact label counters keyed by independent node/edge namespaces.
	pub labels: BTreeMap<LabelStatisticKey, LabelStatistic>,
	/// Exact graph-local class occurrence counts.
	pub classes: BTreeMap<ClassId, u64>,
	/// Bulk degree distributions.
	pub degree_histograms: BTreeMap<DegreeHistogramKey, DegreeHistogram>,
	/// Effective property distributions.
	pub property_histograms: BTreeMap<PropertyHistogramKey, PropertyHistogram>,
}

impl GraphStatistics {
	/// Returns the exact mean outgoing degree.
	pub fn average_out_degree(&self) -> f64 {
		average(self.total_out_degree, self.node_count)
	}

	/// Returns the exact mean incoming degree.
	pub fn average_in_degree(&self) -> f64 {
		average(self.total_in_degree, self.node_count)
	}

	/// Marks bulk distributions stale after committed graph-local work.
	pub fn mark_dirty(&mut self, changed_rows: u64, revision: u64) -> FerrosGrynnResult<()> {
		self.dirty_rows = checked_add(self.dirty_rows, changed_rows, "graph dirty-row count")?;
		self.revision = revision;
		Ok(())
	}
}

fn average(total: u64, count: u64) -> f64 {
	if count == 0 {
		0.0
	} else {
		total as f64 / count as f64
	}
}

fn checked_add(left: u64, right: u64, name: &str) -> FerrosGrynnResult<u64> {
	left.checked_add(right).ok_or_else(|| {
		FerrosGrynnError::invalid_storage_file(format!("statistics {name} overflows u64"))
	})
}
