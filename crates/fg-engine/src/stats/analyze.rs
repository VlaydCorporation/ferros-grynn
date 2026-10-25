use std::{collections::BTreeMap, num::NonZeroU32, path::Path};

use fg_runtime::__internal::GeometryInternal as _;
use rapidhash::v3::rapidhash_v3;

use super::{
	BaseStatistics,
	DegreeBucket,
	DegreeDirection,
	DegreeHistogram,
	DegreeHistogramKey,
	GraphStatistics,
	HISTOGRAM_BUCKETS,
	HistogramMethod,
	LabelStatisticKey,
	PropertyHistogram,
	PropertyHistogramKey,
	StatisticField,
	StatisticsScope,
	StatisticsStore,
	bounded_histogram::{HistogramInput, finish_histograms},
	effective_property_count,
};
use crate::{
	datastore::{DatabaseRoot, GraphOccurrence, GraphRoot, OccurrenceStateRef, index_context},
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	graph::{AtomId, AtomKind, EdgeEndpoint, EdgeTopology, GraphId},
	math::HyperLogLog,
	storage::{ExternalIndexSorter, SortedIndexRecords},
	values::{
		PropertyKeyId,
		Type,
		Value,
		storage::{EncodeContext, TypeTag, encode_index_key},
	},
};

#[derive(Default, Clone, Copy)]
/// Degree counts state.
struct DegreeCounts {
	/// Out associated with this degree counts.
	out: u64,
	/// Incoming associated with this degree counts.
	incoming: u64,
	/// Undirected associated with this degree counts.
	undirected: u64,
}

/// Property accumulator state.
struct PropertyAccumulator {
	/// Ordinal associated with this property accumulator.
	ordinal: u32,
	/// Property key associated with this property accumulator.
	property_key: PropertyKeyId,
	/// Value type associated with this property accumulator.
	value_type: Type,
	/// Type tag associated with this property accumulator.
	type_tag: u8,
	/// Row count associated with this property accumulator.
	row_count: u64,
	/// Null count associated with this property accumulator.
	null_count: u64,
	/// Value count associated with this property accumulator.
	value_count: usize,
	/// Valid associated with this property accumulator.
	valid: bool,
	/// Sampled rows associated with this property accumulator.
	sampled_rows: u32,
	/// Sampled nulls associated with this property accumulator.
	sampled_nulls: u32,
	/// Hll associated with this property accumulator.
	hll: Option<HyperLogLog>,
	/// Geometry sample associated with this property accumulator.
	geometry_sample: BTreeMap<(u64, u64), [f64; 4]>,
	/// Geometry population associated with this property accumulator.
	geometry_population: u64,
}

/// Histogram construction policy selected by public `ANALYZE`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnalyzeMethod {
	/// Full case of analyze method.
	Full,
	/// Approximate case of analyze method.
	Approximate {
		/// Sample count associated with this analyze method.
		sample_count: NonZeroU32,
	},
}

/// Performs one exact full scan shared by RAM and durable rebuilds.
#[cfg(test)]
pub fn analyze_database(database: &DatabaseRoot) -> FerrosGrynnResult<StatisticsStore> {
	let scratch_root = std::env::temp_dir().join("ferros-grynn-statistics");
	analyze_database_at(database, &scratch_root)
}

/// Performs the same full scan with one shared 256 MiB external-sort budget
/// for all property histograms in each scope.
pub fn analyze_database_at(
	database: &DatabaseRoot,
	scratch_root: &Path,
) -> FerrosGrynnResult<StatisticsStore> {
	analyze_database_with_method_at(database, scratch_root, AnalyzeMethod::Full)
}

/// Performs a full statistics scan while selecting exact or sampled property
/// distributions.
pub fn analyze_database_with_method_at(
	database: &DatabaseRoot,
	scratch_root: &Path,
	method: AnalyzeMethod,
) -> FerrosGrynnResult<StatisticsStore> {
	let fields = statistic_fields(database)?;
	let mut context = index_context(&database.schema, &database.dictionary)?;
	let mut result = StatisticsStore::default();
	*result.base_mut() = analyze_base(database, &fields, &mut context, scratch_root, method)?;
	for graph in database.graphs.keys().copied() {
		result.replace_graph(
			graph,
			analyze_graph(database, graph, &fields, &mut context, scratch_root, method)?,
		);
	}
	Ok(result)
}

/// Rebuilds the complete Base statistics scope while preserving graph-local
/// scopes in the caller-owned store.
pub fn analyze_base_with_method_at(
	database: &DatabaseRoot,
	scratch_root: &Path,
	method: AnalyzeMethod,
) -> FerrosGrynnResult<BaseStatistics> {
	let fields = statistic_fields(database)?;
	let mut context = index_context(&database.schema, &database.dictionary)?;
	analyze_base(database, &fields, &mut context, scratch_root, method)
}

/// Rebuilds one complete graph-effective statistics scope.
pub fn analyze_graph_with_method_at(
	database: &DatabaseRoot,
	graph: GraphId,
	scratch_root: &Path,
	method: AnalyzeMethod,
) -> FerrosGrynnResult<GraphStatistics> {
	let fields = statistic_fields(database)?;
	let mut context = index_context(&database.schema, &database.dictionary)?;
	analyze_graph(database, graph, &fields, &mut context, scratch_root, method)
}

/// Analyzes base.
fn analyze_base(
	database: &DatabaseRoot,
	fields: &[StatisticField<'_>],
	context: &mut EncodeContext,
	scratch_root: &Path,
	method: AnalyzeMethod,
) -> FerrosGrynnResult<BaseStatistics> {
	let mut statistics = BaseStatistics {
		revision: database.commit_ts,
		analyzed_revision: database.commit_ts,
		..BaseStatistics::default()
	};
	for class in database.schema.classes() {
		statistics
			.classes
			.insert(class.id().ok_or_else(|| invalid("published class has no ClassId"))?, 0);
	}
	let mut properties = property_accumulators(fields, StatisticsScope::Base, method)?;
	let mut sorter = ExternalIndexSorter::new(scratch_root);
	let mut sequence = 0_u64;
	for atom in database.atoms.atoms() {
		increment(statistics.classes.entry(atom.class_id()).or_default(), 1, "base class count")?;
		record_properties(
			&mut properties,
			atom.class_id(),
			|key| atom.properties().get(key),
			context,
			&mut sorter,
			&mut sequence,
			method,
		)?;
	}
	statistics.property_histograms =
		finish_properties(properties, sorter.finish()?, scratch_root, method)?;
	Ok(statistics)
}

/// Analyzes graph.
fn analyze_graph(
	database: &DatabaseRoot,
	graph_id: GraphId,
	fields: &[StatisticField<'_>],
	context: &mut EncodeContext,
	scratch_root: &Path,
	method: AnalyzeMethod,
) -> FerrosGrynnResult<GraphStatistics> {
	let graph = database
		.graph(graph_id)
		.ok_or_else(|| FerrosGrynnError::graph_does_not_exist(graph_id.to_string()))?;
	let mut statistics = GraphStatistics {
		revision: database.commit_ts,
		analyzed_revision: database.commit_ts,
		..GraphStatistics::default()
	};
	let mut properties = property_accumulators(
		fields,
		StatisticsScope::Effective {
			graph: graph_id,
		},
		method,
	)?;
	let mut sorter = ExternalIndexSorter::new(scratch_root);
	let mut sequence = 0_u64;
	let mut degrees = BTreeMap::<AtomId, DegreeCounts>::new();
	for occurrence in graph.occurrences() {
		let atom = database
			.atom(occurrence.global_id())
			.ok_or_else(|| invalid("graph statistics found an absent global atom"))?;
		increment(statistics.classes.entry(atom.class_id()).or_default(), 1, "graph class count")?;
		let property_count = effective_property_count(atom.properties(), occurrence)?;
		if let Some(label) = occurrence.label() {
			let label = statistics
				.labels
				.entry(LabelStatisticKey {
					kind: atom.kind(),
					label: label.clone(),
				})
				.or_default();
			increment(&mut label.atom_count, 1, "label atom count")?;
			increment(
				&mut label.property_count,
				u64::try_from(property_count)
					.map_err(|_error| invalid("effective property count exceeds u64"))?,
				"label property count",
			)?;
		}
		match occurrence.state().view() {
			OccurrenceStateRef::Node {
				..
			} => {
				increment(&mut statistics.node_count, 1, "node count")?;
				degrees.insert(occurrence.local_id(), DegreeCounts::default());
			}
			OccurrenceStateRef::Edge {
				..
			} => increment(&mut statistics.edge_count, 1, "edge count")?,
		}
		record_properties(
			&mut properties,
			atom.class_id(),
			|key| occurrence.effective_value(atom.properties().get(key), key),
			context,
			&mut sorter,
			&mut sequence,
			method,
		)?;
	}
	for occurrence in graph.occurrences() {
		if let OccurrenceStateRef::Edge {
			topology,
			..
		} = occurrence.state().view()
		{
			record_topology(&mut degrees, topology)?;
		}
	}
	finish_degrees(&mut statistics, &degrees, graph)?;
	statistics.property_histograms =
		finish_properties(properties, sorter.finish()?, scratch_root, method)?;
	Ok(statistics)
}

/// Returns the statistic fields result.
fn statistic_fields(database: &DatabaseRoot) -> FerrosGrynnResult<Vec<StatisticField<'_>>> {
	let mut result = Vec::new();
	for class in database.schema.classes() {
		let class_id = class.id().ok_or_else(|| invalid("published class has no ClassId"))?;
		for field in class.fields() {
			if histogram_type_tag(field.value_type()).is_none() {
				continue;
			}
			result.push(StatisticField {
				class_id,
				field_id: field.id().ok_or_else(|| invalid("published field has no SchemaId"))?,
				property_key: field
					.key
					.ok_or_else(|| invalid("published field has no property key"))?,
				value_type: field.value_type(),
			});
		}
	}
	Ok(result)
}

/// Returns the property accumulators result.
fn property_accumulators(
	fields: &[StatisticField<'_>],
	scope: StatisticsScope,
	method: AnalyzeMethod,
) -> FerrosGrynnResult<BTreeMap<PropertyHistogramKey, PropertyAccumulator>> {
	fields
		.iter()
		.enumerate()
		.map(|(ordinal, field)| {
			let type_tag = histogram_type_tag(field.value_type)
				.ok_or_else(|| invalid("unsupported field entered histogram builder"))?;
			let ordinal = u32::try_from(ordinal)
				.map_err(|_error| invalid("property histogram ordinal exceeds u32"))?;
			Ok((
				PropertyHistogramKey {
					class_id: field.class_id,
					field_id: field.field_id,
					scope,
				},
				PropertyAccumulator {
					ordinal,
					property_key: field.property_key,
					value_type: field.value_type.clone(),
					type_tag,
					row_count: 0,
					null_count: 0,
					value_count: 0,
					valid: true,
					sampled_rows: 0,
					sampled_nulls: 0,
					hll: match method {
						AnalyzeMethod::Full => None,
						AnalyzeMethod::Approximate {
							..
						} => Some(HyperLogLog::new(12)?),
					},
					geometry_sample: BTreeMap::new(),
					geometry_population: 0,
				},
			))
		})
		.collect()
}

/// Records properties.
fn record_properties<'value>(
	properties: &mut BTreeMap<PropertyHistogramKey, PropertyAccumulator>,
	class: crate::datastore::ClassId,
	mut value: impl FnMut(PropertyKeyId) -> Option<&'value Value>,
	context: &mut EncodeContext,
	sorter: &mut ExternalIndexSorter,
	sequence: &mut u64,
	method: AnalyzeMethod,
) -> FerrosGrynnResult<()> {
	for (_key, accumulator) in properties.iter_mut().filter(|(key, _)| key.class_id == class) {
		accumulator.row_count = accumulator
			.row_count
			.checked_add(1)
			.ok_or_else(|| invalid("histogram row count overflows u64"))?;
		if !accumulator.valid {
			continue;
		}
		if accumulator.type_tag == 0x27 {
			match value(accumulator.property_key) {
				None | Some(Value::None | Value::Null) => {
					increment(&mut accumulator.null_count, 1, "geometry null count")?;
				}
				Some(Value::Geometry(geometry)) => {
					if let Some(mbr) = geometry.mbr()? {
						increment(&mut accumulator.geometry_population, 1, "geometry population")?;
						let mut identity = [0_u8; 12];
						identity[..4].copy_from_slice(&accumulator.ordinal.to_le_bytes());
						identity[4..].copy_from_slice(&sequence.to_le_bytes());
						accumulator
							.geometry_sample
							.insert((rapidhash_v3(&identity), *sequence), mbr);
						if accumulator.geometry_sample.len() > 1024 {
							accumulator.geometry_sample.pop_last();
						}
					}
				}
				Some(_) => {
					accumulator.valid = false;
				}
			}
			*sequence = sequence
				.checked_add(1)
				.ok_or_else(|| invalid("geometry sample sequence overflows"))?;
			continue;
		}
		let encoded = match value(accumulator.property_key) {
			None | Some(Value::None | Value::Null) => {
				increment(&mut accumulator.null_count, 1, "histogram null count")?;
				None
			}
			Some(value) => match encode_index_key(&[(value, &accumulator.value_type)], context) {
				Ok(encoded) => Some(encoded),
				Err(_error) => {
					accumulator.valid = false;
					continue;
				}
			},
		};
		match method {
			AnalyzeMethod::Full => {
				if let Some(encoded) = encoded {
					let mut key = Vec::with_capacity(4_usize.saturating_add(encoded.len()));
					key.extend_from_slice(&accumulator.ordinal.to_be_bytes());
					key.extend_from_slice(&encoded);
					sorter.push_raw(key, *sequence)?;
					accumulator.value_count = accumulator
						.value_count
						.checked_add(1)
						.ok_or_else(|| invalid("property histogram value count overflows usize"))?;
				}
			}
			AnalyzeMethod::Approximate {
				..
			} => {
				if let (Some(hll), Some(encoded)) = (accumulator.hll.as_mut(), encoded.as_deref()) {
					hll.insert(encoded);
				}
				let mut priority_input = Vec::with_capacity(
					13_usize.saturating_add(encoded.as_ref().map_or(0, Vec::len)),
				);
				priority_input.extend_from_slice(&accumulator.ordinal.to_be_bytes());
				priority_input.extend_from_slice(&sequence.to_be_bytes());
				priority_input.push(u8::from(encoded.is_some()));
				if let Some(encoded) = &encoded {
					priority_input.extend_from_slice(encoded);
				}
				let priority = rapidhash_v3(&priority_input);
				let mut key = Vec::with_capacity(priority_input.len());
				key.extend_from_slice(&accumulator.ordinal.to_be_bytes());
				key.extend_from_slice(&priority.to_be_bytes());
				key.push(u8::from(encoded.is_some()));
				if let Some(encoded) = encoded {
					key.extend_from_slice(&encoded);
				}
				sorter.push_raw(key, *sequence)?;
			}
		}
		*sequence = sequence
			.checked_add(1)
			.ok_or_else(|| invalid("property histogram sequence overflows u64"))?;
	}
	Ok(())
}

/// Finishes properties.
fn finish_properties(
	mut properties: BTreeMap<PropertyHistogramKey, PropertyAccumulator>,
	records: SortedIndexRecords,
	scratch_root: &Path,
	method: AnalyzeMethod,
) -> FerrosGrynnResult<BTreeMap<PropertyHistogramKey, PropertyHistogram>> {
	let records = match method {
		AnalyzeMethod::Full => records,
		AnalyzeMethod::Approximate {
			sample_count,
		} => select_property_sample(&mut properties, &records, scratch_root, sample_count)?,
	};
	let inputs = properties
		.values()
		.map(|accumulator| HistogramInput {
			ordinal: accumulator.ordinal,
			value_count: accumulator.value_count,
		})
		.collect::<Vec<_>>();
	let mut finished = finish_histograms(&records, &inputs)?;
	let mut result = BTreeMap::new();
	for (key, accumulator) in properties {
		if !accumulator.valid {
			continue;
		}
		let histogram = finished
			.remove(&accumulator.ordinal)
			.ok_or_else(|| invalid("property histogram sort result is absent"))?;
		if accumulator.type_tag == 0x27 {
			result.insert(key, PropertyHistogram {
				type_tag: 0x27,
				method: HistogramMethod::Exact,
				sample_count: u32::try_from(accumulator.row_count).map_err(
					|_conversion_error| invalid("geometry histogram population exceeds u32"),
				)?,
				distinct_count: 0,
				null_fraction: fraction(accumulator.null_count, accumulator.row_count),
				buckets: Vec::new(),
				mbr_sample: accumulator.geometry_sample.into_values().collect(),
				mbr_population: accumulator.geometry_population,
			});
			continue;
		}
		let (method, sample_count, distinct_count, null_fraction) = match method {
			AnalyzeMethod::Full => {
				let Ok(sample_count) = u32::try_from(accumulator.row_count) else {
					continue;
				};
				let null_fraction = fraction(accumulator.null_count, accumulator.row_count);
				(HistogramMethod::Exact, sample_count, histogram.distinct_count, null_fraction)
			}
			AnalyzeMethod::Approximate {
				..
			} => {
				let distinct_count = accumulator.hll.as_ref().map_or(0, HyperLogLog::estimate);
				let null_fraction = fraction(
					u64::from(accumulator.sampled_nulls),
					u64::from(accumulator.sampled_rows),
				);
				(
					HistogramMethod::Approximate,
					accumulator.sampled_rows,
					distinct_count,
					null_fraction,
				)
			}
		};
		result.insert(key, PropertyHistogram {
			type_tag: accumulator.type_tag,
			method,
			sample_count,
			distinct_count,
			null_fraction,
			buckets: histogram.buckets,
			mbr_sample: accumulator.geometry_sample.into_values().collect(),
			mbr_population: 0,
		});
	}
	Ok(result)
}

/// Returns the select property sample result.
fn select_property_sample(
	properties: &mut BTreeMap<PropertyHistogramKey, PropertyAccumulator>,
	records: &SortedIndexRecords,
	scratch_root: &Path,
	sample_count: NonZeroU32,
) -> FerrosGrynnResult<SortedIndexRecords> {
	let mut by_ordinal = properties
		.values_mut()
		.map(|accumulator| (accumulator.ordinal, accumulator))
		.collect::<BTreeMap<_, _>>();
	let mut sampled = ExternalIndexSorter::new(scratch_root);
	records.visit_raw(|key, sequence| {
		if key.len() < 13 {
			return Err(invalid("sample selection key is truncated"));
		}
		let ordinal = u32::from_be_bytes(
			key[..4]
				.try_into()
				.map_err(|_error| invalid("sample ordinal has an invalid length"))?,
		);
		let accumulator = by_ordinal
			.get_mut(&ordinal)
			.ok_or_else(|| invalid("sample refers to an unknown histogram ordinal"))?;
		if accumulator.sampled_rows >= sample_count.get() {
			return Ok(());
		}
		accumulator.sampled_rows = accumulator
			.sampled_rows
			.checked_add(1)
			.ok_or_else(|| invalid("sample row count overflows u32"))?;
		match key[12] {
			0 if key.len() == 13 => {
				accumulator.sampled_nulls = accumulator
					.sampled_nulls
					.checked_add(1)
					.ok_or_else(|| invalid("sample null count overflows u32"))?;
			}
			1 if key.len() > 13 => {
				let mut histogram_key = Vec::with_capacity(key.len() - 9);
				histogram_key.extend_from_slice(&ordinal.to_be_bytes());
				histogram_key.extend_from_slice(&key[13..]);
				sampled.push_raw(histogram_key, sequence)?;
				accumulator.value_count = accumulator
					.value_count
					.checked_add(1)
					.ok_or_else(|| invalid("sample value count overflows usize"))?;
			}
			_ => return Err(invalid("sample selection marker is not canonical")),
		}
		Ok(())
	})?;
	sampled.finish()
}

/// Returns the fraction result.
fn fraction(numerator: u64, denominator: u64) -> f32 {
	if denominator == 0 {
		0.0
	} else {
		numerator as f32 / denominator as f32
	}
}

/// Records topology.
fn record_topology(
	degrees: &mut BTreeMap<AtomId, DegreeCounts>,
	topology: &EdgeTopology,
) -> FerrosGrynnResult<()> {
	match topology {
		EdgeTopology::Directed {
			source,
			target,
		} => {
			for endpoint in source {
				increment_degree(degrees, endpoint, DegreeDirection::Out)?;
			}
			for endpoint in target {
				increment_degree(degrees, endpoint, DegreeDirection::In)?;
			}
		}
		EdgeTopology::Bidirectional {
			source,
			target,
		} => {
			for endpoint in source.iter().chain(target) {
				increment_degree(degrees, endpoint, DegreeDirection::Out)?;
				increment_degree(degrees, endpoint, DegreeDirection::In)?;
			}
		}
		EdgeTopology::Undirected {
			endpoints,
		} => {
			for endpoint in endpoints {
				increment_degree(degrees, endpoint, DegreeDirection::Undirected)?;
			}
		}

		_ => {
			return Err(crate::gql_status::FerrosGrynnError::invalid_argument(
				"Unsupported runtime variant at the workspace boundary.",
			));
		}
	}
	Ok(())
}

/// Returns the increment degree result.
fn increment_degree(
	degrees: &mut BTreeMap<AtomId, DegreeCounts>,
	endpoint: &EdgeEndpoint,
	direction: DegreeDirection,
) -> FerrosGrynnResult<()> {
	let Some(id) = endpoint.local_id().filter(|id| id.kind() == Some(AtomKind::Node)) else {
		return Ok(());
	};
	let degree = degrees
		.get_mut(&id)
		.ok_or_else(|| invalid("edge references an unknown graph-local node"))?;
	let target = match direction {
		DegreeDirection::Out => &mut degree.out,
		DegreeDirection::In => &mut degree.incoming,
		DegreeDirection::Undirected => &mut degree.undirected,
	};
	increment(target, 1, "node degree")
}

/// Finishes degrees.
fn finish_degrees(
	statistics: &mut GraphStatistics,
	degrees: &BTreeMap<AtomId, DegreeCounts>,
	graph: &GraphRoot,
) -> FerrosGrynnResult<()> {
	let mut global = [Vec::new(), Vec::new(), Vec::new()];
	let mut labels = BTreeMap::<LabelStatisticKey, [Vec<u32>; 3]>::new();
	for (id, degree) in degrees {
		let values =
			[u32_degree(degree.out)?, u32_degree(degree.incoming)?, u32_degree(degree.undirected)?];
		increment(&mut statistics.total_out_degree, degree.out, "total out degree")?;
		increment(&mut statistics.total_in_degree, degree.incoming, "total in degree")?;
		increment(
			&mut statistics.total_undirected_degree,
			degree.undirected,
			"total undirected degree",
		)?;
		statistics.maximum_out_degree = statistics.maximum_out_degree.max(degree.out);
		statistics.maximum_in_degree = statistics.maximum_in_degree.max(degree.incoming);
		for (target, value) in global.iter_mut().zip(values) {
			target.push(value);
		}
		if let Some(label) = graph.occurrence(*id).and_then(GraphOccurrence::label) {
			let entry = labels
				.entry(LabelStatisticKey {
					kind: AtomKind::Node,
					label: label.clone(),
				})
				.or_insert_with(|| [Vec::new(), Vec::new(), Vec::new()]);
			for (target, value) in entry.iter_mut().zip(values) {
				target.push(value);
			}
		}
	}
	for (index, values) in global.into_iter().enumerate() {
		insert_degree(statistics, None, direction(index)?, values)?;
	}
	for (label, values) in labels {
		for (index, values) in values.into_iter().enumerate() {
			insert_degree(statistics, Some(label.clone()), direction(index)?, values)?;
		}
	}
	Ok(())
}

/// Inserts degree.
fn insert_degree(
	statistics: &mut GraphStatistics,
	label: Option<LabelStatisticKey>,
	direction: DegreeDirection,
	mut values: Vec<u32>,
) -> FerrosGrynnResult<()> {
	if values.is_empty() {
		return Ok(());
	}
	values.sort_unstable();
	let buckets = equi_depth_ranges(values.len())
		.map(|range| {
			let values = &values[range];
			Ok(DegreeBucket {
				minimum: values[0],
				maximum: values[values.len() - 1],
				node_count: u64::try_from(values.len())
					.map_err(|_error| invalid("degree bucket count exceeds u64"))?,
			})
		})
		.collect::<FerrosGrynnResult<Vec<_>>>()?;
	statistics.degree_histograms.insert(
		DegreeHistogramKey {
			label,
			direction,
		},
		DegreeHistogram {
			buckets,
		},
	);
	Ok(())
}

/// Returns the equi depth ranges result.
fn equi_depth_ranges(length: usize) -> impl Iterator<Item = std::ops::Range<usize>> {
	let buckets = length.min(HISTOGRAM_BUCKETS);
	let size = length / buckets.max(1);
	let larger = length % buckets.max(1);
	(0..buckets).map(move |index| {
		let start = index * size + index.min(larger);
		let end = start + size + usize::from(index < larger);
		start..end
	})
}

/// Returns the histogram type tag result.
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
		Type::Bytes => TypeTag::Bytes,
		Type::Record => TypeTag::RecordId,
		Type::Array(inner) => {
			histogram_type_tag(inner)?;
			TypeTag::Array
		}
		Type::Set(inner) => {
			histogram_type_tag(inner)?;
			TypeTag::Set
		}
		Type::Range(inner) => {
			histogram_type_tag(inner)?;
			TypeTag::Range
		}
		Type::Atom(_) => TypeTag::AtomRef,
		Type::Option(inner) => return histogram_type_tag(inner),
		Type::Geometry => return Some(0x27),
		_ => return None,
	};
	Some(tag.byte())
}

/// Returns the direction result.
fn direction(index: usize) -> FerrosGrynnResult<DegreeDirection> {
	match index {
		0 => Ok(DegreeDirection::Out),
		1 => Ok(DegreeDirection::In),
		2 => Ok(DegreeDirection::Undirected),
		_ => Err(invalid("degree direction index is outside its domain")),
	}
}

/// Returns the u32 degree result.
fn u32_degree(value: u64) -> FerrosGrynnResult<u32> {
	u32::try_from(value).map_err(|_error| invalid("degree exceeds u32"))
}

/// Returns the increment result.
fn increment(target: &mut u64, amount: u64, name: &str) -> FerrosGrynnResult<()> {
	*target = target
		.checked_add(amount)
		.ok_or_else(|| invalid(format!("statistics {name} overflows u64")))?;
	Ok(())
}

/// Returns the invalid result.
fn invalid(reason: impl Into<String>) -> FerrosGrynnError {
	FerrosGrynnError::invalid_storage_file(reason)
}
