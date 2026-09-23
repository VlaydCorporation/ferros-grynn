use std::collections::BTreeMap;

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
	PropertyBucket,
	PropertyHistogram,
	PropertyHistogramKey,
	StatisticField,
	StatisticsScope,
	StatisticsStore,
};
use crate::{
	datastore::{
		DatabaseRoot,
		GraphOccurrence,
		GraphRoot,
		OccurrenceState,
		OverlayValue,
		index_context,
	},
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	graph::{AtomId, AtomKind, EdgeEndpoint, EdgeTopology, GraphId},
	values::{
		PropertyKeyId,
		PropertyMap,
		Type,
		Value,
		storage::{EncodeContext, TypeTag, encode_index_key},
	},
};

#[derive(Default, Clone, Copy)]
struct DegreeCounts {
	out: u64,
	incoming: u64,
	undirected: u64,
}

struct PropertyAccumulator {
	property_key: PropertyKeyId,
	value_type: Type,
	type_tag: u8,
	row_count: u64,
	null_count: u64,
	values: Vec<Vec<u8>>,
	valid: bool,
}

/// Performs one exact full scan shared by RAM and durable rebuilds.
pub(crate) fn analyze_database(database: &DatabaseRoot) -> FerrosGrynnResult<StatisticsStore> {
	let fields = statistic_fields(database)?;
	let mut context = index_context(&database.schema, &database.dictionary)?;
	let mut result = StatisticsStore::default();
	*result.base_mut() = analyze_base(database, &fields, &mut context)?;
	for graph in database.graphs.keys().copied() {
		result.replace_graph(graph, analyze_graph(database, graph, &fields, &mut context)?);
	}
	Ok(result)
}

fn analyze_base(
	database: &DatabaseRoot,
	fields: &[StatisticField<'_>],
	context: &mut EncodeContext,
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
	let mut properties = property_accumulators(fields, StatisticsScope::Base)?;
	for atom in database.atoms.atoms() {
		increment(statistics.classes.entry(atom.class_id()).or_default(), 1, "base class count")?;
		record_properties(
			&mut properties,
			atom.class_id(),
			|key| atom.properties().get(key),
			context,
		)?;
	}
	statistics.property_histograms = finish_properties(properties)?;
	Ok(statistics)
}

fn analyze_graph(
	database: &DatabaseRoot,
	graph_id: GraphId,
	fields: &[StatisticField<'_>],
	context: &mut EncodeContext,
) -> FerrosGrynnResult<GraphStatistics> {
	let graph = database
		.graph(graph_id)
		.ok_or_else(|| FerrosGrynnError::graph_does_not_exist(graph_id.to_string()))?;
	let mut statistics = GraphStatistics {
		revision: database.commit_ts,
		analyzed_revision: database.commit_ts,
		..GraphStatistics::default()
	};
	let mut properties = property_accumulators(fields, StatisticsScope::Effective {
		graph: graph_id,
	})?;
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
		match occurrence.state() {
			OccurrenceState::Node {
				..
			} => {
				increment(&mut statistics.node_count, 1, "node count")?;
				degrees.insert(occurrence.local_id(), DegreeCounts::default());
			}
			OccurrenceState::Edge {
				..
			} => increment(&mut statistics.edge_count, 1, "edge count")?,
		}
		record_properties(
			&mut properties,
			atom.class_id(),
			|key| occurrence.effective_value(atom.properties().get(key), key),
			context,
		)?;
	}
	for occurrence in graph.occurrences() {
		if let OccurrenceState::Edge {
			topology,
			..
		} = occurrence.state()
		{
			record_topology(&mut degrees, topology)?;
		}
	}
	finish_degrees(&mut statistics, &degrees, graph)?;
	statistics.property_histograms = finish_properties(properties)?;
	Ok(statistics)
}

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

fn property_accumulators(
	fields: &[StatisticField<'_>],
	scope: StatisticsScope,
) -> FerrosGrynnResult<BTreeMap<PropertyHistogramKey, PropertyAccumulator>> {
	fields
		.iter()
		.map(|field| {
			let type_tag = histogram_type_tag(field.value_type)
				.ok_or_else(|| invalid("unsupported field entered histogram builder"))?;
			Ok((
				PropertyHistogramKey {
					class_id: field.class_id,
					field_id: field.field_id,
					scope,
				},
				PropertyAccumulator {
					property_key: field.property_key,
					value_type: field.value_type.clone(),
					type_tag,
					row_count: 0,
					null_count: 0,
					values: Vec::new(),
					valid: true,
				},
			))
		})
		.collect()
}

fn record_properties<'value>(
	properties: &mut BTreeMap<PropertyHistogramKey, PropertyAccumulator>,
	class: crate::datastore::ClassId,
	mut value: impl FnMut(PropertyKeyId) -> Option<&'value Value>,
	context: &mut EncodeContext,
) -> FerrosGrynnResult<()> {
	for (_key, accumulator) in properties.iter_mut().filter(|(key, _)| key.class_id == class) {
		accumulator.row_count = accumulator
			.row_count
			.checked_add(1)
			.ok_or_else(|| invalid("histogram row count overflows u64"))?;
		if !accumulator.valid {
			continue;
		}
		let Some(value) = value(accumulator.property_key) else {
			increment(&mut accumulator.null_count, 1, "histogram null count")?;
			continue;
		};
		if matches!(value, Value::None | Value::Null) {
			increment(&mut accumulator.null_count, 1, "histogram null count")?;
			continue;
		}
		match encode_index_key(&[(value, &accumulator.value_type)], context) {
			Ok(encoded) => accumulator.values.push(encoded),
			Err(_error) => {
				accumulator.valid = false;
				accumulator.values.clear();
			}
		}
	}
	Ok(())
}

fn finish_properties(
	properties: BTreeMap<PropertyHistogramKey, PropertyAccumulator>,
) -> FerrosGrynnResult<BTreeMap<PropertyHistogramKey, PropertyHistogram>> {
	let mut result = BTreeMap::new();
	for (key, mut accumulator) in properties {
		let Ok(sample_count) = u32::try_from(accumulator.row_count) else {
			continue;
		};
		if !accumulator.valid {
			continue;
		}
		accumulator.values.sort_unstable();
		let distinct_count = u64::try_from(distinct_sorted(&accumulator.values))
			.map_err(|_error| invalid("histogram distinct count exceeds u64"))?;
		let null_fraction = if accumulator.row_count == 0 {
			0.0
		} else {
			accumulator.null_count as f32 / accumulator.row_count as f32
		};
		result.insert(key, PropertyHistogram {
			type_tag: accumulator.type_tag,
			method: HistogramMethod::Exact,
			sample_count,
			distinct_count,
			null_fraction,
			buckets: property_buckets(&accumulator.values)?,
		});
	}
	Ok(result)
}

fn effective_property_count(
	base: &PropertyMap,
	occurrence: &GraphOccurrence,
) -> FerrosGrynnResult<usize> {
	let inherited = base
		.iter()
		.filter(|(key, value)| occurrence.effective_value(Some(value), *key).is_some())
		.count();
	let additional = occurrence
		.overlay()
		.into_iter()
		.flat_map(crate::datastore::PropertyOverlay::iter)
		.filter(|(key, value)| !base.contains_key(*key) && matches!(value, OverlayValue::Value(_)))
		.count();
	inherited
		.checked_add(additional)
		.ok_or_else(|| invalid("effective property count overflows usize"))
}

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
	}
	Ok(())
}

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

fn property_buckets(values: &[Vec<u8>]) -> FerrosGrynnResult<Vec<PropertyBucket>> {
	equi_depth_ranges(values.len())
		.map(|range| {
			let values = &values[range];
			Ok(PropertyBucket {
				lower: values[0].clone(),
				upper: values[values.len() - 1].clone(),
				frequency: u64::try_from(values.len())
					.map_err(|_error| invalid("property bucket frequency exceeds u64"))?,
				distinct_count: u64::try_from(distinct_sorted(values))
					.map_err(|_error| invalid("property bucket distinct count exceeds u64"))?,
			})
		})
		.collect()
}

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

fn distinct_sorted<T: Eq>(values: &[T]) -> usize {
	usize::from(!values.is_empty()) + values.windows(2).filter(|pair| pair[0] != pair[1]).count()
}

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
		_ => return None,
	};
	Some(tag.byte())
}

fn direction(index: usize) -> FerrosGrynnResult<DegreeDirection> {
	match index {
		0 => Ok(DegreeDirection::Out),
		1 => Ok(DegreeDirection::In),
		2 => Ok(DegreeDirection::Undirected),
		_ => Err(invalid("degree direction index is outside its domain")),
	}
}

fn u32_degree(value: u64) -> FerrosGrynnResult<u32> {
	u32::try_from(value).map_err(|_error| invalid("degree exceeds u32"))
}

fn increment(target: &mut u64, amount: u64, name: &str) -> FerrosGrynnResult<()> {
	*target = target
		.checked_add(amount)
		.ok_or_else(|| invalid(format!("statistics {name} overflows u64")))?;
	Ok(())
}

fn invalid(reason: impl Into<String>) -> FerrosGrynnError {
	FerrosGrynnError::invalid_storage_file(reason)
}
