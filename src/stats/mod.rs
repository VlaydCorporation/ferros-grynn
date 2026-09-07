//! Transactional planner statistics shared by datastore and local storage.

use std::collections::{BTreeMap, BTreeSet};

use crate::{
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	graph::{EdgeRead, EdgeTopology, GraphRead, NodeRead},
	values::{
		PropertyKeyId,
		RecordId,
		Text,
		Type,
		Value,
		graph::{AtomId, AtomKind, MaterializedGraph},
		storage::{EncodeContext, TypeTag, encode_index_key},
	},
};

/// Maximum number of equi-depth buckets stored for one distribution.
pub const HISTOGRAM_BUCKETS: usize = 100;

/// A graph-local label identity; node and edge label namespaces stay distinct.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct LabelStatisticKey {
	/// Atom namespace containing the label.
	pub kind: AtomKind,
	/// Interned label text.
	pub label: Text,
}

/// Exact label cardinality and the aggregate property count used for its mean.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LabelStatistic {
	/// Exact number of labeled atoms.
	pub atom_count: u64,
	/// Exact aggregate number of properties on those atoms.
	pub property_count: u64,
}

/// One direction represented by a degree histogram.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[repr(u8)]
pub enum DegreeDirection {
	Out = 0,
	In = 1,
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

/// Global or per-label degree distribution identity.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct DegreeHistogramKey {
	/// Optional node label; `None` identifies the whole graph.
	pub label: Option<LabelStatisticKey>,
	/// Degree direction represented by the distribution.
	pub direction: DegreeDirection,
}

/// One exact frequency range in an equi-depth degree histogram.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DegreeBucket {
	/// Inclusive lower degree bound.
	pub minimum: u32,
	/// Inclusive upper degree bound.
	pub maximum: u32,
	/// Number of nodes represented by the bucket.
	pub node_count: u64,
}

/// A bounded equi-depth degree histogram.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DegreeHistogram {
	/// Ordered equi-depth buckets.
	pub buckets: Vec<DegreeBucket>,
}

/// Stable schema identity of one property distribution.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct PropertyHistogramKey {
	/// Stable table schema identity.
	pub table_id: u32,
	/// Stable field schema identity.
	pub field_id: u32,
}

/// One exact frequency range over order-preserving encoded values.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PropertyBucket {
	/// Inclusive lower encoded value.
	pub lower: Vec<u8>,
	/// Inclusive upper encoded value.
	pub upper: Vec<u8>,
	/// Number of non-null values represented by the bucket.
	pub frequency: u64,
	/// Exact number of distinct encoded values in the bucket.
	pub distinct_count: u64,
}

/// Exact bulk statistics for one scalar property.
#[derive(Debug, Clone, PartialEq)]
pub struct PropertyHistogram {
	/// Format-v1 storage type tag.
	pub type_tag: u8,
	/// Exact distinct count at the analyzed revision.
	pub distinct_count: u64,
	/// Fraction of absent, `NONE`, or `NULL` table values.
	pub null_fraction: f32,
	/// Ordered equi-depth buckets for non-null values.
	pub buckets: Vec<PropertyBucket>,
}

/// One schema field eligible for scalar histogram collection.
pub struct StatisticField<'a> {
	/// Stable table schema identity.
	pub table_id: u32,
	/// Stable field schema identity.
	pub field_id: u32,
	/// Runtime property dictionary identity.
	pub property_key: PropertyKeyId,
	/// Declared schema type used by the index-key codec.
	pub value_type: &'a Type,
}

/// Planner statistics for one exact graph revision.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct GraphStatistics {
	/// Exact graph revision represented by simple counters.
	pub revision: u64,
	/// Graph revision at which bulk distributions were rebuilt.
	pub analyzed_revision: u64,
	/// Number of changed rows accumulated since bulk analysis.
	pub dirty_rows: u64,
	/// Exact live node count.
	pub node_count: u64,
	/// Exact live edge count.
	pub edge_count: u64,
	/// Exact aggregate outgoing degree.
	pub total_out_degree: u64,
	/// Exact aggregate incoming degree.
	pub total_in_degree: u64,
	/// Exact aggregate undirected degree.
	pub total_undirected_degree: u64,
	/// Exact or conservative upper bound for outgoing degree.
	pub maximum_out_degree: u64,
	/// Exact or conservative upper bound for incoming degree.
	pub maximum_in_degree: u64,
	/// Exact cardinality and property totals per label.
	pub labels: BTreeMap<LabelStatisticKey, LabelStatistic>,
	/// Exact cardinality per table membership.
	pub tables: BTreeMap<u32, u64>,
	/// Bulk degree distributions indexed by label and direction.
	pub degree_histograms: BTreeMap<DegreeHistogramKey, DegreeHistogram>,
	/// Bulk property distributions indexed by table and field.
	pub property_histograms: BTreeMap<PropertyHistogramKey, PropertyHistogram>,
}

impl GraphStatistics {
	/// Returns the exact mean out-degree represented by the integer total.
	pub fn average_out_degree(&self) -> f64 {
		average(self.total_out_degree, self.node_count)
	}

	/// Returns the exact mean in-degree represented by the integer total.
	pub fn average_in_degree(&self) -> f64 {
		average(self.total_in_degree, self.node_count)
	}

	/// Marks bulk distributions stale after a committed row change.
	pub fn mark_dirty(&mut self, changed_rows: u64, revision: u64) -> FerrosGrynnResult<()> {
		self.dirty_rows = self.dirty_rows.checked_add(changed_rows).ok_or_else(|| {
			FerrosGrynnError::invalid_storage_file("statistics dirty-row count overflows")
		})?;
		self.revision = revision;
		Ok(())
	}

	/// Applies one exact before/after label observation.
	///
	/// # Errors
	///
	/// Returns an error if an exact counter would overflow or underflow.
	pub fn replace_label(
		&mut self,
		kind: AtomKind,
		before: Option<(&Text, usize)>,
		after: Option<(&Text, usize)>,
	) -> FerrosGrynnResult<()> {
		if let Some((label, properties)) = before {
			self.adjust_label(kind, label, properties, false)?;
		}
		if let Some((label, properties)) = after {
			self.adjust_label(kind, label, properties, true)?;
		}
		Ok(())
	}

	/// Applies one labeled atom contribution to exact counters.
	fn adjust_label(
		&mut self,
		kind: AtomKind,
		label: &Text,
		properties: usize,
		add: bool,
	) -> FerrosGrynnResult<()> {
		let key = LabelStatisticKey {
			kind,
			label: label.clone(),
		};
		let property_count = u64::try_from(properties).map_err(|_error| {
			FerrosGrynnError::invalid_storage_file("label property count exceeds u64")
		})?;
		if add {
			let entry = self.labels.entry(key).or_default();
			entry.atom_count = entry.atom_count.checked_add(1).ok_or_else(|| {
				FerrosGrynnError::invalid_storage_file("label atom count overflows")
			})?;
			entry.property_count =
				entry.property_count.checked_add(property_count).ok_or_else(|| {
					FerrosGrynnError::invalid_storage_file("label property count overflows")
				})?;
			return Ok(());
		}
		let entry = self.labels.get_mut(&key).ok_or_else(|| {
			FerrosGrynnError::invalid_storage_file(
				"label statistics delta references an absent label",
			)
		})?;
		entry.atom_count = entry
			.atom_count
			.checked_sub(1)
			.ok_or_else(|| FerrosGrynnError::invalid_storage_file("label atom count underflows"))?;
		entry.property_count =
			entry.property_count.checked_sub(property_count).ok_or_else(|| {
				FerrosGrynnError::invalid_storage_file("label property count underflows")
			})?;
		// Keep zero-count entries so point deltas never need to truncate the
		// append-only statistics file. A later full ANALYZE drops dead labels.
		Ok(())
	}
}

/// MVCC-owned graph statistics catalog used by future cost planning.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct StatisticsStore {
	/// Statistics indexed by exact graph identity.
	graphs: BTreeMap<RecordId, GraphStatistics>,
}

impl StatisticsStore {
	/// Returns statistics for one graph.
	pub fn graph(&self, graph: &RecordId) -> Option<&GraphStatistics> {
		self.graphs.get(graph)
	}

	/// Inserts or replaces statistics for one graph.
	pub fn insert(&mut self, graph: RecordId, statistics: GraphStatistics) {
		self.graphs.insert(graph, statistics);
	}

	/// Returns mutable statistics for one graph.
	pub fn graph_mut(&mut self, graph: &RecordId) -> Option<&mut GraphStatistics> {
		self.graphs.get_mut(graph)
	}

	/// Removes statistics for one graph.
	pub fn remove(&mut self, graph: &RecordId) {
		self.graphs.remove(graph);
	}

	/// Retains only statistics for live graph identities.
	#[allow(clippy::mutable_key_type, reason = "RecordId is the datastore's stable graph key")]
	pub fn retain_graphs(&mut self, graphs: &BTreeSet<RecordId>) {
		self.graphs.retain(|graph, _statistics| graphs.contains(graph));
	}
}

/// Builds the reference context required by order-preserving property keys.
pub fn graph_encode_context<R: GraphRead>(
	graph: &R,
	dictionary: &crate::values::PropertyDictionary,
) -> FerrosGrynnResult<EncodeContext> {
	let mut context = EncodeContext::new(dictionary.clone());
	for id in graph.node_ids()?.chain(graph.edge_ids()?) {
		let id = id?;
		let reference = graph.atom_ref(id)?.ok_or_else(|| {
			FerrosGrynnError::invalid_storage_file(
				"atom disappeared while preparing statistics context",
			)
		})?;
		let disk = crate::values::storage::DiskAtomRef::from_parts(
			id.kind().ok_or_else(|| {
				FerrosGrynnError::invalid_storage_file("statistics atom kind is invalid")
			})?,
			id.slot(),
		)?;
		context.register_atom(reference, disk)?;
	}
	Ok(context)
}

/// Accumulated directed and undirected degree totals for one node.
#[derive(Clone, Copy, Default)]
struct DegreeCounts {
	/// Outgoing degree.
	out: u64,
	/// Incoming degree.
	incoming: u64,
	/// Undirected degree.
	undirected: u64,
}

/// Bulk property observations collected before histogram construction.
struct PropertyAccumulator {
	/// Format-v1 storage type tag.
	type_tag: u8,
	/// Number of table rows observed.
	row_count: u64,
	/// Number of absent, `NONE`, or `NULL` values.
	null_count: u64,
	/// Encoded non-null values awaiting sort.
	values: Vec<Vec<u8>>,
	/// Whether every observed value was encodable.
	valid: bool,
}

/// Performs one exact bulk scan and replaces all distributions for a graph.
pub fn analyze_graph<R, Membership>(
	graph: &R,
	revision: u64,
	fields: &[StatisticField<'_>],
	mut membership: Membership,
	context: &mut EncodeContext,
) -> FerrosGrynnResult<GraphStatistics>
where
	R: GraphRead,
	Membership: FnMut(AtomId) -> Option<u32>,
{
	let node_ids = graph.node_ids()?.collect::<FerrosGrynnResult<Vec<_>>>()?;
	let edge_ids = graph.edge_ids()?.collect::<FerrosGrynnResult<Vec<_>>>()?;
	let mut degrees = node_ids
		.iter()
		.copied()
		.map(|id| (id, DegreeCounts::default()))
		.collect::<BTreeMap<_, _>>();
	let fields_by_table = fields_by_table(fields);
	let mut statistics = GraphStatistics {
		revision,
		analyzed_revision: revision,
		node_count: checked_len(node_ids.len(), "node count")?,
		edge_count: checked_len(edge_ids.len(), "edge count")?,
		..GraphStatistics::default()
	};
	let mut properties = BTreeMap::<PropertyHistogramKey, PropertyAccumulator>::new();

	for id in &node_ids {
		let node = graph.node(*id)?.ok_or_else(|| {
			FerrosGrynnError::invalid_storage_file("node disappeared during statistics scan")
		})?;
		let values = node.properties()?;
		record_atom(
			&mut statistics,
			&mut properties,
			*id,
			node.label_text()?.as_deref(),
			&values,
			membership(*id),
			&fields_by_table,
			context,
		)?;
	}
	for id in &edge_ids {
		let edge = graph.edge(*id)?.ok_or_else(|| {
			FerrosGrynnError::invalid_storage_file("edge disappeared during statistics scan")
		})?;
		let values = edge.properties()?;
		record_atom(
			&mut statistics,
			&mut properties,
			*id,
			edge.label_text()?.as_deref(),
			&values,
			membership(*id),
			&fields_by_table,
			context,
		)?;
		let topology = edge.topology()?;
		record_topology(&mut degrees, topology.as_ref())?;
	}
	finish_degrees(&mut statistics, &degrees, graph)?;
	for (key, mut accumulator) in properties {
		if !accumulator.valid {
			continue;
		}
		accumulator.values.sort_unstable();
		let distinct_count =
			checked_len(distinct_sorted(&accumulator.values), "property distinct count")?;
		let null_fraction = if accumulator.row_count == 0 {
			0.0
		} else {
			accumulator.null_count as f32 / accumulator.row_count as f32
		};
		statistics.property_histograms.insert(key, PropertyHistogram {
			type_tag: accumulator.type_tag,
			distinct_count,
			null_fraction,
			buckets: property_buckets(&accumulator.values)?,
		});
	}
	Ok(statistics)
}

/// Applies exact point-DML counters while retaining bulk distributions as
/// stale.
pub fn update_graph_statistics(
	previous: &GraphStatistics,
	before: &MaterializedGraph,
	after: &MaterializedGraph,
	dirty: &BTreeSet<AtomId>,
) -> FerrosGrynnResult<GraphStatistics> {
	let mut statistics = previous.clone();
	let mut added_out = 0_u64;
	let mut added_in = 0_u64;
	for id in dirty {
		match id.kind() {
			Some(AtomKind::Node) => {
				let before = node_label_observation(before, *id)?;
				let after = node_label_observation(after, *id)?;
				adjust_count(
					&mut statistics.node_count,
					before.is_some(),
					after.is_some(),
					"node",
				)?;
				statistics.replace_label(
					AtomKind::Node,
					before.as_ref().and_then(LabelObservation::as_labeled),
					after.as_ref().and_then(LabelObservation::as_labeled),
				)?;
			}
			Some(AtomKind::Edge) => {
				let before = edge_observation(before, *id)?;
				let after = edge_observation(after, *id)?;
				adjust_count(
					&mut statistics.edge_count,
					before.is_some(),
					after.is_some(),
					"edge",
				)?;
				statistics.replace_label(
					AtomKind::Edge,
					before.as_ref().and_then(|value| value.label.as_labeled()),
					after.as_ref().and_then(|value| value.label.as_labeled()),
				)?;
				if let Some(before) = before {
					subtract_degrees(&mut statistics, before.degrees)?;
				}
				if let Some(after) = after {
					add_degrees(&mut statistics, after.degrees)?;
					added_out = added_out.checked_add(after.degrees.out).ok_or_else(|| {
						FerrosGrynnError::invalid_storage_file(
							"incremental out-degree bound overflows",
						)
					})?;
					added_in = added_in.checked_add(after.degrees.incoming).ok_or_else(|| {
						FerrosGrynnError::invalid_storage_file(
							"incremental in-degree bound overflows",
						)
					})?;
				}
			}
			None => {
				return Err(FerrosGrynnError::invalid_storage_file(
					"statistics delta contains an invalid atom kind",
				));
			}
		}
	}
	statistics.maximum_out_degree =
		statistics.maximum_out_degree.saturating_add(added_out).min(statistics.total_out_degree);
	statistics.maximum_in_degree =
		statistics.maximum_in_degree.saturating_add(added_in).min(statistics.total_in_degree);
	statistics
		.mark_dirty(checked_len(dirty.len(), "statistics changed-row count")?, after.revision())?;
	Ok(statistics)
}

/// Before/after label and property count for one atom.
struct LabelObservation {
	/// Optional atom label.
	label: Option<Text>,
	/// Exact number of properties.
	property_count: usize,
}

impl LabelObservation {
	/// Returns the observation only when it has a label.
	fn as_labeled(&self) -> Option<(&Text, usize)> {
		self.label.as_ref().map(|label| (label, self.property_count))
	}
}

/// Before/after edge state needed by incremental statistics.
struct EdgeObservation {
	/// Edge label and property observation.
	label: LabelObservation,
	/// Edge endpoint contribution to degree totals.
	degrees: DegreeCounts,
}

/// Reads the label and property count of a live materialized node.
fn node_label_observation(
	graph: &MaterializedGraph,
	id: AtomId,
) -> FerrosGrynnResult<Option<LabelObservation>> {
	let Some(node) = GraphRead::node(graph, id)? else {
		return Ok(None);
	};
	Ok(Some(LabelObservation {
		label: NodeRead::label_text(&node)?.map(std::borrow::Cow::into_owned),
		property_count: NodeRead::properties(&node)?.len(),
	}))
}

/// Reads the label, properties, and degree contribution of a live edge.
fn edge_observation(
	graph: &MaterializedGraph,
	id: AtomId,
) -> FerrosGrynnResult<Option<EdgeObservation>> {
	let Some(edge) = GraphRead::edge(graph, id)? else {
		return Ok(None);
	};
	Ok(Some(EdgeObservation {
		label: LabelObservation {
			label: EdgeRead::label_text(&edge)?.map(std::borrow::Cow::into_owned),
			property_count: EdgeRead::properties(&edge)?.len(),
		},
		degrees: topology_degrees(EdgeRead::topology(&edge)?.as_ref())?,
	}))
}

/// Counts the degree contribution of one edge topology.
fn topology_degrees(topology: &EdgeTopology) -> FerrosGrynnResult<DegreeCounts> {
	let mut degrees = DegreeCounts::default();
	let mut count = |endpoints: &[crate::graph::EdgeEndpoint], out, incoming, undirected| {
		for endpoint in endpoints {
			if endpoint.local_id().is_none_or(|id| id.kind() != Some(AtomKind::Node)) {
				continue;
			}
			if out {
				degrees.out = degrees.out.checked_add(1).ok_or_else(|| {
					FerrosGrynnError::invalid_storage_file("out-degree delta overflows")
				})?;
			}
			if incoming {
				degrees.incoming = degrees.incoming.checked_add(1).ok_or_else(|| {
					FerrosGrynnError::invalid_storage_file("in-degree delta overflows")
				})?;
			}
			if undirected {
				degrees.undirected = degrees.undirected.checked_add(1).ok_or_else(|| {
					FerrosGrynnError::invalid_storage_file("undirected-degree delta overflows")
				})?;
			}
		}
		Ok::<_, FerrosGrynnError>(())
	};
	match topology {
		EdgeTopology::Directed {
			source,
			target,
		} => {
			count(source, true, false, false)?;
			count(target, false, true, false)?;
		}
		EdgeTopology::Bidirectional {
			source,
			target,
		} => {
			count(source, true, true, false)?;
			count(target, true, true, false)?;
		}
		EdgeTopology::Undirected {
			endpoints,
		} => count(endpoints, false, false, true)?,
	}
	Ok(degrees)
}

/// Applies one presence transition to an exact cardinality.
fn adjust_count(
	count: &mut u64,
	was_present: bool,
	is_present: bool,
	name: &'static str,
) -> FerrosGrynnResult<()> {
	match (was_present, is_present) {
		(false, true) => {
			*count = count.checked_add(1).ok_or_else(|| {
				FerrosGrynnError::invalid_storage_file(format!("{name} count overflows"))
			})?;
		}
		(true, false) => {
			*count = count.checked_sub(1).ok_or_else(|| {
				FerrosGrynnError::invalid_storage_file(format!("{name} count underflows"))
			})?;
		}
		_ => {}
	}
	Ok(())
}

/// Removes one edge contribution from exact degree totals.
fn subtract_degrees(
	statistics: &mut GraphStatistics,
	degrees: DegreeCounts,
) -> FerrosGrynnResult<()> {
	statistics.total_out_degree = statistics
		.total_out_degree
		.checked_sub(degrees.out)
		.ok_or_else(|| FerrosGrynnError::invalid_storage_file("out-degree total underflows"))?;
	statistics.total_in_degree = statistics
		.total_in_degree
		.checked_sub(degrees.incoming)
		.ok_or_else(|| FerrosGrynnError::invalid_storage_file("in-degree total underflows"))?;
	statistics.total_undirected_degree =
		statistics.total_undirected_degree.checked_sub(degrees.undirected).ok_or_else(|| {
			FerrosGrynnError::invalid_storage_file("undirected-degree total underflows")
		})?;
	Ok(())
}

/// Adds one edge contribution to exact degree totals.
fn add_degrees(statistics: &mut GraphStatistics, degrees: DegreeCounts) -> FerrosGrynnResult<()> {
	statistics.total_out_degree = statistics
		.total_out_degree
		.checked_add(degrees.out)
		.ok_or_else(|| FerrosGrynnError::invalid_storage_file("out-degree total overflows"))?;
	statistics.total_in_degree = statistics
		.total_in_degree
		.checked_add(degrees.incoming)
		.ok_or_else(|| FerrosGrynnError::invalid_storage_file("in-degree total overflows"))?;
	statistics.total_undirected_degree =
		statistics.total_undirected_degree.checked_add(degrees.undirected).ok_or_else(|| {
			FerrosGrynnError::invalid_storage_file("undirected-degree total overflows")
		})?;
	Ok(())
}

/// Groups histogram-eligible schema fields by stable table identity.
fn fields_by_table<'a>(
	fields: &'a [StatisticField<'a>],
) -> BTreeMap<u32, Vec<&'a StatisticField<'a>>> {
	let mut result = BTreeMap::<u32, Vec<_>>::new();
	for field in fields {
		if histogram_type_tag(field.value_type).is_some() {
			result.entry(field.table_id).or_default().push(field);
		}
	}
	result
}

#[allow(
	clippy::too_many_arguments,
	reason = "one bulk observation carries its graph, schema, and output accumulators"
)]
/// Records one atom in the bulk cardinality and property accumulators.
fn record_atom(
	statistics: &mut GraphStatistics,
	properties: &mut BTreeMap<PropertyHistogramKey, PropertyAccumulator>,
	id: AtomId,
	label: Option<&Text>,
	values: &crate::values::PropertyMap,
	table_id: Option<u32>,
	fields: &BTreeMap<u32, Vec<&StatisticField<'_>>>,
	context: &mut EncodeContext,
) -> FerrosGrynnResult<()> {
	let kind = id.kind().ok_or_else(|| {
		FerrosGrynnError::invalid_storage_file("statistics scan found an invalid atom kind")
	})?;
	if let Some(label) = label {
		statistics.replace_label(kind, None, Some((label, values.len())))?;
	}
	let Some(table_id) = table_id else {
		return Ok(());
	};
	let count = statistics.tables.entry(table_id).or_default();
	*count = count.checked_add(1).ok_or_else(|| {
		FerrosGrynnError::invalid_storage_file("table statistics count overflows")
	})?;
	for field in fields.get(&table_id).into_iter().flatten() {
		let key = PropertyHistogramKey {
			table_id,
			field_id: field.field_id,
		};
		let type_tag = histogram_type_tag(field.value_type).ok_or_else(|| {
			FerrosGrynnError::invalid_storage_file("unsupported field entered histogram scan")
		})?;
		let accumulator = properties.entry(key).or_insert_with(|| PropertyAccumulator {
			type_tag,
			row_count: 0,
			null_count: 0,
			values: Vec::new(),
			valid: true,
		});
		if !accumulator.valid {
			continue;
		}
		accumulator.row_count = accumulator.row_count.checked_add(1).ok_or_else(|| {
			FerrosGrynnError::invalid_storage_file("histogram row count overflows")
		})?;
		let value = values.get(field.property_key);
		if value.is_none_or(|value| matches!(value, Value::None | Value::Null)) {
			accumulator.null_count = accumulator.null_count.checked_add(1).ok_or_else(|| {
				FerrosGrynnError::invalid_storage_file("histogram null count overflows")
			})?;
			continue;
		}
		let value = value.ok_or_else(|| {
			FerrosGrynnError::invalid_storage_file("histogram property unexpectedly disappeared")
		})?;
		match encode_index_key(&[(value, field.value_type)], context) {
			Ok(value) => accumulator.values.push(value),
			Err(_error) => {
				accumulator.valid = false;
				accumulator.values.clear();
			}
		}
	}
	Ok(())
}

/// Applies one edge topology to the per-node degree accumulators.
fn record_topology(
	degrees: &mut BTreeMap<AtomId, DegreeCounts>,
	topology: &EdgeTopology,
) -> FerrosGrynnResult<()> {
	let mut increment = |endpoint: &crate::graph::EdgeEndpoint,
	                     out: bool,
	                     incoming: bool,
	                     undirected: bool|
	 -> FerrosGrynnResult<()> {
		let Some(id) = endpoint.local_id().filter(|id| id.kind() == Some(AtomKind::Node)) else {
			return Ok(());
		};
		let degree = degrees.get_mut(&id).ok_or_else(|| {
			FerrosGrynnError::invalid_storage_file("edge references an unknown node")
		})?;
		if out {
			degree.out = degree
				.out
				.checked_add(1)
				.ok_or_else(|| FerrosGrynnError::invalid_storage_file("out-degree overflows"))?;
		}
		if incoming {
			degree.incoming = degree
				.incoming
				.checked_add(1)
				.ok_or_else(|| FerrosGrynnError::invalid_storage_file("in-degree overflows"))?;
		}
		if undirected {
			degree.undirected = degree.undirected.checked_add(1).ok_or_else(|| {
				FerrosGrynnError::invalid_storage_file("undirected degree overflows")
			})?;
		}
		Ok(())
	};
	match topology {
		EdgeTopology::Directed {
			source,
			target,
		} => {
			for endpoint in source {
				increment(endpoint, true, false, false)?;
			}
			for endpoint in target {
				increment(endpoint, false, true, false)?;
			}
		}
		EdgeTopology::Bidirectional {
			source,
			target,
		} => {
			for endpoint in source.iter().chain(target) {
				increment(endpoint, true, true, false)?;
			}
		}
		EdgeTopology::Undirected {
			endpoints,
		} => {
			for endpoint in endpoints {
				increment(endpoint, false, false, true)?;
			}
		}
	}
	Ok(())
}

/// Finalizes exact degree totals and global/per-label histograms.
fn finish_degrees<R: GraphRead>(
	statistics: &mut GraphStatistics,
	degrees: &BTreeMap<AtomId, DegreeCounts>,
	graph: &R,
) -> FerrosGrynnResult<()> {
	let mut global = [Vec::new(), Vec::new(), Vec::new()];
	let mut labels = BTreeMap::<LabelStatisticKey, [Vec<u32>; 3]>::new();
	for (id, degree) in degrees {
		let values = [
			checked_degree(degree.out)?,
			checked_degree(degree.incoming)?,
			checked_degree(degree.undirected)?,
		];
		statistics.total_out_degree = statistics
			.total_out_degree
			.checked_add(degree.out)
			.ok_or_else(|| FerrosGrynnError::invalid_storage_file("out-degree total overflows"))?;
		statistics.total_in_degree = statistics
			.total_in_degree
			.checked_add(degree.incoming)
			.ok_or_else(|| FerrosGrynnError::invalid_storage_file("in-degree total overflows"))?;
		statistics.total_undirected_degree =
			statistics.total_undirected_degree.checked_add(degree.undirected).ok_or_else(|| {
				FerrosGrynnError::invalid_storage_file("undirected-degree total overflows")
			})?;
		statistics.maximum_out_degree = statistics.maximum_out_degree.max(degree.out);
		statistics.maximum_in_degree = statistics.maximum_in_degree.max(degree.incoming);
		for (target, value) in global.iter_mut().zip(values) {
			target.push(value);
		}
		let node = graph.node(*id)?.ok_or_else(|| {
			FerrosGrynnError::invalid_storage_file("node disappeared during degree scan")
		})?;
		if let Some(label) = node.label_text()? {
			let entry = labels
				.entry(LabelStatisticKey {
					kind: AtomKind::Node,
					label: label.into_owned(),
				})
				.or_insert_with(|| [Vec::new(), Vec::new(), Vec::new()]);
			for (target, value) in entry.iter_mut().zip(values) {
				target.push(value);
			}
		}
	}
	for (index, values) in global.into_iter().enumerate() {
		insert_degree_histogram(statistics, None, direction(index)?, values)?;
	}
	for (label, values) in labels {
		for (index, values) in values.into_iter().enumerate() {
			insert_degree_histogram(statistics, Some(label.clone()), direction(index)?, values)?;
		}
	}
	Ok(())
}

/// Sorts one degree sample and inserts its bounded histogram.
fn insert_degree_histogram(
	statistics: &mut GraphStatistics,
	label: Option<LabelStatisticKey>,
	direction: DegreeDirection,
	mut values: Vec<u32>,
) -> FerrosGrynnResult<()> {
	if values.is_empty() {
		return Ok(());
	}
	values.sort_unstable();
	statistics.degree_histograms.insert(
		DegreeHistogramKey {
			label,
			direction,
		},
		DegreeHistogram {
			buckets: degree_buckets(&values)?,
		},
	);
	Ok(())
}

/// Builds balanced equi-depth buckets from sorted degrees.
fn degree_buckets(values: &[u32]) -> FerrosGrynnResult<Vec<DegreeBucket>> {
	let mut buckets = Vec::new();
	for range in equi_depth_ranges(values.len()) {
		let values = &values[range];
		buckets.push(DegreeBucket {
			minimum: values[0],
			maximum: values[values.len() - 1],
			node_count: checked_len(values.len(), "degree bucket count")?,
		});
	}
	Ok(buckets)
}

/// Builds balanced equi-depth buckets from sorted encoded values.
fn property_buckets(values: &[Vec<u8>]) -> FerrosGrynnResult<Vec<PropertyBucket>> {
	let mut buckets = Vec::new();
	for range in equi_depth_ranges(values.len()) {
		let values = &values[range];
		let distinct_count =
			checked_len(distinct_sorted(values), "property bucket distinct count")?;
		buckets.push(PropertyBucket {
			lower: values[0].clone(),
			upper: values[values.len() - 1].clone(),
			frequency: checked_len(values.len(), "property bucket frequency")?,
			distinct_count,
		});
	}
	Ok(buckets)
}

/// Counts adjacent distinct values in an already sorted slice.
fn distinct_sorted<T: Eq>(values: &[T]) -> usize {
	usize::from(!values.is_empty()) + values.windows(2).filter(|pair| pair[0] != pair[1]).count()
}

/// Partitions a length into at most [`HISTOGRAM_BUCKETS`] balanced ranges.
fn equi_depth_ranges(length: usize) -> impl Iterator<Item = std::ops::Range<usize>> {
	let bucket_count = length.min(HISTOGRAM_BUCKETS);
	let bucket_size = length / bucket_count.max(1);
	let larger_buckets = length % bucket_count.max(1);
	(0..bucket_count).map(move |index| {
		let start = index * bucket_size + index.min(larger_buckets);
		let end = start + bucket_size + usize::from(index < larger_buckets);
		start..end
	})
}

/// Returns the storage tag for a histogram-safe schema type.
fn histogram_type_tag(value_type: &Type) -> Option<u8> {
	let tag = match value_type {
		Type::None => TypeTag::None,
		Type::Null => TypeTag::Null,
		Type::Bool => TypeTag::False,
		Type::Int => TypeTag::Int,
		Type::Float => TypeTag::Float,
		Type::Decimal => TypeTag::Decimal,
		Type::String => TypeTag::String,
		Type::Duration => TypeTag::Duration,
		Type::DateTime => TypeTag::DateTime,
		Type::Ulid => TypeTag::Ulid,
		Type::Uuid => TypeTag::Uuid,
		Type::Array(inner) => {
			histogram_type_tag(inner)?;
			TypeTag::Array
		}
		Type::Set(inner) => {
			histogram_type_tag(inner)?;
			TypeTag::Set
		}
		Type::Bytes => TypeTag::Bytes,
		Type::Record => TypeTag::RecordId,
		Type::Range(inner) => {
			histogram_type_tag(inner)?;
			TypeTag::Range
		}
		Type::Atom(_) => TypeTag::AtomRef,
		Type::Option(inner) => return histogram_type_tag(inner),
		Type::Any
		| Type::Number
		| Type::Object
		| Type::Geometry
		| Type::Vector(_)
		| Type::Regex
		| Type::Path
		| Type::Graph(_)
		| Type::Union(_)
		| Type::RecordShape(_)
		| Type::Closure {
			..
		} => return None,
	};
	Some(tag.byte())
}

/// Maps the fixed accumulator index to a degree direction.
fn direction(index: usize) -> FerrosGrynnResult<DegreeDirection> {
	match index {
		0 => Ok(DegreeDirection::Out),
		1 => Ok(DegreeDirection::In),
		2 => Ok(DegreeDirection::Undirected),
		_ => Err(FerrosGrynnError::invalid_storage_file(
			"degree histogram direction is out of range",
		)),
	}
}

/// Narrows a persisted degree bucket bound.
fn checked_degree(value: u64) -> FerrosGrynnResult<u32> {
	u32::try_from(value)
		.map_err(|_error| FerrosGrynnError::invalid_storage_file("degree exceeds u32"))
}

/// Converts an in-memory collection length to its persisted width.
fn checked_len(value: usize, name: &'static str) -> FerrosGrynnResult<u64> {
	u64::try_from(value)
		.map_err(|_error| FerrosGrynnError::invalid_storage_file(format!("{name} exceeds u64")))
}

/// Computes a finite mean, returning zero for an empty population.
fn average(total: u64, count: u64) -> f64 {
	if count == 0 {
		0.0
	} else {
		total as f64 / count as f64
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::{
		graph::{Graph, GraphMut, Mutable},
		values::{PropertyDictionary, PropertyMap},
	};

	#[test]
	fn bulk_analysis_and_point_delta_preserve_exact_counters() -> FerrosGrynnResult<()> {
		let ranges = equi_depth_ranges(101).collect::<Vec<_>>();
		assert_eq!(ranges.len(), HISTOGRAM_BUCKETS);
		assert_eq!(ranges.first(), Some(&(0..2)));
		assert_eq!(ranges.last(), Some(&(100..101)));

		let mut dictionary = PropertyDictionary::new();
		let age = dictionary.intern("age")?;
		let mut graph = Graph::<Mutable>::new();
		let mut first_properties = PropertyMap::new();
		first_properties.insert(age, 10_i64);
		let first = graph.add_node(Some("Person".into()), first_properties)?;
		let second = graph.add_node(Some("Person".into()), PropertyMap::new())?;
		let mut third_properties = PropertyMap::new();
		third_properties.insert(age, 30_i64);
		let third = graph.add_node(Some("Other".into()), third_properties)?;
		let edge = graph.add_edge(
			EdgeTopology::directed([first], [second]),
			Some("KNOWS".into()),
			PropertyMap::new(),
		)?;
		let before: MaterializedGraph = graph.freeze().into();
		let fields = [
			StatisticField {
				table_id: 7,
				field_id: 9,
				property_key: age,
				value_type: &Type::Int,
			},
			StatisticField {
				table_id: 7,
				field_id: 10,
				property_key: age,
				value_type: &Type::String,
			},
		];
		let mut context = graph_encode_context(&before, &dictionary)?;
		let statistics = analyze_graph(
			&before,
			before.revision(),
			&fields,
			|id| [first, second, third].contains(&id).then_some(7),
			&mut context,
		)?;
		assert_eq!((statistics.node_count, statistics.edge_count), (3, 1));
		assert_eq!(
			(
				statistics.total_out_degree,
				statistics.total_in_degree,
				statistics.total_undirected_degree,
			),
			(1, 1, 0)
		);
		assert_eq!(statistics.tables.get(&7), Some(&3));
		let property = statistics
			.property_histograms
			.get(&PropertyHistogramKey {
				table_id: 7,
				field_id: 9,
			})
			.ok_or_else(|| FerrosGrynnError::invalid_storage_file("test histogram is absent"))?;
		assert_eq!(property.distinct_count, 2);
		assert!((property.null_fraction - 1.0 / 3.0).abs() < 0.000_001);
		assert_eq!(property.buckets.iter().map(|bucket| bucket.frequency).sum::<u64>(), 2);
		assert!(!statistics.property_histograms.contains_key(&PropertyHistogramKey {
			table_id: 7,
			field_id: 10,
		}));

		let bulk_histograms = statistics.property_histograms.clone();
		let analyzed_revision = statistics.analyzed_revision;
		let mut changed = before.clone().into_general()?.thaw();
		*GraphMut::node_label_mut(&mut changed, first)? = Some("Admin".into());
		GraphMut::remove_atom(&mut changed, edge)?;
		let after: MaterializedGraph = changed.freeze().into();
		let dirty = BTreeSet::from([first, edge]);
		let statistics = update_graph_statistics(&statistics, &before, &after, &dirty)?;
		assert_eq!((statistics.node_count, statistics.edge_count), (3, 0));
		assert_eq!((statistics.total_out_degree, statistics.total_in_degree), (0, 0));
		assert_eq!(statistics.analyzed_revision, analyzed_revision);
		assert_eq!(statistics.dirty_rows, 2);
		assert_eq!(statistics.property_histograms, bulk_histograms);
		assert_eq!(
			statistics
				.labels
				.get(&LabelStatisticKey {
					kind: AtomKind::Node,
					label: "Admin".into(),
				})
				.map(|value| value.atom_count),
			Some(1)
		);
		assert_eq!(
			statistics
				.labels
				.get(&LabelStatisticKey {
					kind: AtomKind::Edge,
					label: "KNOWS".into(),
				})
				.map(|value| value.atom_count),
			Some(0)
		);
		Ok(())
	}
}
