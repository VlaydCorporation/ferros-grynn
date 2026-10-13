//! Canonical little-endian encoding for `FGDC v14`.

use std::collections::BTreeMap;

use super::{
	AnalyzerCatalogEntry,
	AnalyzerFilterCatalogEntry,
	AtomStoreCatalogBinding,
	ClassCatalogEntry,
	ClassFieldCatalogEntry,
	ClassIndexCatalogEntry,
	DatabaseCatalog,
	FunctionCatalogEntry,
	GraphCatalogEntry,
	GraphPropertyCatalogEntry,
	ParameterCatalogEntry,
	PhysicalIndexBinding,
	type_codec,
	validate::validate,
};
use crate::{
	datastore::{
		AnalyzerId,
		AnalyzerLanguage,
		AnalyzerTokenizer,
		ClassId,
		ClassIndexKind,
		ClassIndexOptions,
		ClassIndexState,
		ClassSchemaMode,
		DiskAnnIndexOptions,
		FulltextIndexOptions,
		FunctionId,
		HnswIndexOptions,
		IndexId,
		IndexMaintenanceMode,
		IndexScope,
		SchemaId,
		SchemaVersion,
		VectorDistance,
	},
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	graph::{AtomId, AtomKind, GraphId, GraphStorageMode, GraphTopologyConstraints},
	storage::{
		graph_pages::GraphArenaBindings,
		page::PageNo,
		types::{DataFileId, DataFileKind},
	},
	values::{PropertyKeyId, VectorKind},
};

const MAGIC: [u8; 4] = *b"FGDC";
/// Current development layout; older catalogs are deliberately rejected.
const VERSION: u16 = 14;
/// Fixed header including the source-only EVENT descriptor count.
const HEADER_BYTES: usize = 144;
/// Independent cap for the complete logical database catalog.
pub(super) const MAX_BYTES: usize = 16 * 1024 * 1024;
const MAX_STRING_BYTES: usize = 4096;

pub(super) fn encode(catalog: &DatabaseCatalog) -> FerrosGrynnResult<Vec<u8>> {
	validate(catalog)?;
	let mut output = Vec::with_capacity(HEADER_BYTES);
	output.extend_from_slice(&MAGIC);
	output.extend_from_slice(&VERSION.to_le_bytes());
	output.extend_from_slice(&0_u16.to_le_bytes());
	output.extend_from_slice(&catalog.next_graph_id.to_le_bytes());
	output.extend_from_slice(&catalog.next_schema_id.to_le_bytes());
	write_u32(&mut output, catalog.time_zones.len(), "time-zone count")?;
	write_u32(&mut output, catalog.classes.len(), "class count")?;
	write_u32(&mut output, catalog.graphs.len(), "graph count")?;
	write_u32(&mut output, catalog.parameters.len(), "parameter count")?;
	write_u32(&mut output, catalog.functions.len(), "function count")?;
	write_u32(&mut output, catalog.analyzers.len(), "analyzer count")?;
	output.extend_from_slice(&catalog.atom_store.generation.to_le_bytes());
	write_binding(&mut output, catalog.atom_store.primary);
	write_binding(&mut output, catalog.atom_store.reverse_membership);
	output.extend_from_slice(catalog.database_uuid.as_bytes());
	output.extend_from_slice(&catalog.committed_at_ns.to_le_bytes());
	write_u32(&mut output, catalog.changefeeds.len(), "changefeed count")?;
	write_u32(&mut output, catalog.live_queries.len(), "LIVE query count")?;
	write_u32(&mut output, catalog.live_bindings.len(), "LIVE binding count")?;
	write_u32(&mut output, catalog.live_history.len(), "LIVE history count")?;
	write_u32(&mut output, catalog.events.len(), "EVENT count")?;
	write_u32(&mut output, catalog.event_outbox_segments.len(), "EVENT outbox segment count")?;

	for name in &catalog.time_zones {
		write_string(&mut output, name.as_str(), "time-zone name")?;
	}
	for class in &catalog.classes {
		encode_class(&mut output, class)?;
	}
	for graph in &catalog.graphs {
		encode_graph(&mut output, graph)?;
	}
	for parameter in &catalog.parameters {
		encode_parameter(&mut output, parameter)?;
	}
	for function in &catalog.functions {
		super::function::encode(&mut output, function)?;
	}
	for analyzer in &catalog.analyzers {
		encode_analyzer(&mut output, analyzer)?;
	}
	for entry in &catalog.changefeeds {
		super::changefeed::encode(&mut output, entry)?;
	}
	for entry in &catalog.live_queries {
		super::live::encode(&mut output, entry)?;
	}
	for entry in &catalog.live_bindings {
		super::live_binding::encode(&mut output, entry)?;
	}
	for entry in &catalog.live_history {
		super::live_history::encode(&mut output, entry)?;
	}
	for event in &catalog.events {
		super::event::encode(&mut output, event)?;
	}
	for segment in &catalog.event_outbox_segments {
		super::history_segment::encode(&mut output, *segment);
	}
	if output.len() > MAX_BYTES {
		return Err(FerrosGrynnError::storage_decode_limit_exceeded(
			"database catalog bytes",
			MAX_BYTES as u64,
			output.len() as u64,
		));
	}
	if !catalog.live_queries.is_empty() || !catalog.live_bindings.is_empty() {
		// Admission must match the recovery reader before any catalog pages/WAL
		// are written. Empty registries keep the existing publication fast
		// path.
		let _recoverable = decode(&output)?;
	}
	Ok(output)
}

pub(super) fn decode(bytes: &[u8]) -> FerrosGrynnResult<DatabaseCatalog> {
	if bytes.len() > MAX_BYTES {
		return Err(FerrosGrynnError::storage_decode_limit_exceeded(
			"database catalog bytes",
			MAX_BYTES as u64,
			bytes.len() as u64,
		));
	}
	let mut decoder = Decoder::new(bytes);
	if decoder.read_exact(4)? != MAGIC {
		return Err(invalid("invalid database catalog marker"));
	}
	let version = decoder.read_u16()?;
	if version != VERSION {
		return Err(FerrosGrynnError::unsupported_storage_format(
			u64::from(version),
			u64::from(VERSION),
		));
	}
	decoder.expect_zero(2, "database catalog header reserved bytes")?;
	let next_graph_id = decoder.read_u64()?;
	let next_schema_id = decoder.read_u64()?;
	let time_zone_count = decoder.read_count("time-zone count")?;
	let class_count = decoder.read_count("class count")?;
	let graph_count = decoder.read_count("graph count")?;
	let parameter_count = decoder.read_count("parameter count")?;
	let function_count = decoder.read_count("function count")?;
	let analyzer_count = decoder.read_count("analyzer count")?;
	let generation = decoder.read_u64()?;
	let primary = read_binding(&mut decoder)?;
	let reverse_membership = read_binding(&mut decoder)?;
	let database_uuid = uuid::Uuid::from_slice(decoder.read_exact(16)?).map_err(|error| {
		invalid("database origin UUID is malformed").with_technical_source(error)
	})?;
	let committed_at_ns =
		i128::from_le_bytes(decoder.read_exact(16)?.try_into().map_err(|error| {
			invalid("database commit time is truncated").with_technical_source(error)
		})?);
	let changefeed_count = decoder.read_count("changefeed count")?;
	let live_count = decoder.read_count("LIVE query count")?;
	let binding_count = decoder.read_count("LIVE binding count")?;
	let history_count = decoder.read_count("LIVE history count")?;
	let event_count = decoder.read_count("EVENT count")?;
	let outbox_count = decoder.read_count("EVENT outbox segment count")?;
	if changefeed_count > bytes.len().saturating_sub(HEADER_BYTES) / super::changefeed::MIN_BYTES {
		return Err(invalid("changefeed count exceeds its encoded payload"));
	}
	let section_counts = [
		time_zone_count,
		class_count,
		graph_count,
		parameter_count,
		function_count,
		analyzer_count,
	];
	let minimum_bytes = section_counts
		.into_iter()
		.try_fold(0_usize, |total, count| total.checked_add(count.checked_mul(4)?))
		.and_then(|total| {
			total.checked_add(changefeed_count.checked_mul(super::changefeed::MIN_BYTES)?)
		})
		.and_then(|total| total.checked_add(live_count.checked_mul(super::live::MIN_BYTES)?))
		.and_then(|total| {
			total.checked_add(binding_count.checked_mul(super::live_binding::MIN_BYTES)?)
		})
		.and_then(|total| {
			total.checked_add(history_count.checked_mul(super::live_history::MIN_BYTES)?)
		})
		.and_then(|total| total.checked_add(event_count.checked_mul(super::event::MIN_BYTES)?))
		.ok_or_else(|| invalid("catalog section counts overflow"))?;
	if minimum_bytes > bytes.len().saturating_sub(HEADER_BYTES) {
		return Err(invalid("catalog section counts exceed available encoded bytes"));
	}

	let mut time_zones = Vec::with_capacity(time_zone_count);
	for _ in 0..time_zone_count {
		time_zones.push(read_string(&mut decoder, "time-zone name")?.into());
	}
	let mut classes = Vec::with_capacity(class_count);
	for _ in 0..class_count {
		classes.push(decode_class(&mut decoder)?);
	}
	let mut graphs = Vec::with_capacity(graph_count);
	for _ in 0..graph_count {
		graphs.push(decode_graph(&mut decoder)?);
	}
	let mut parameters = Vec::with_capacity(parameter_count);
	for _ in 0..parameter_count {
		parameters.push(decode_parameter(&mut decoder)?);
	}
	let mut functions = Vec::with_capacity(function_count);
	for _ in 0..function_count {
		functions.push(super::function::decode(&mut decoder)?);
	}
	let mut analyzers = Vec::with_capacity(analyzer_count);
	for _ in 0..analyzer_count {
		analyzers.push(decode_analyzer(&mut decoder)?);
	}
	if changefeed_count
		> decoder.bytes.len().saturating_sub(decoder.position) / super::changefeed::MIN_BYTES
	{
		return Err(invalid("changefeed count exceeds remaining catalog bytes"));
	}
	let mut changefeeds = Vec::with_capacity(changefeed_count);
	for _ in 0..changefeed_count {
		changefeeds.push(super::changefeed::decode(&mut decoder)?);
	}
	if live_count > decoder.bytes.len().saturating_sub(decoder.position) / super::live::MIN_BYTES {
		return Err(invalid("LIVE query count exceeds remaining catalog bytes"));
	}
	let mut budget =
		crate::storage::history_codec::CodecBudget::new(256 * 1024 * 1024, bytes.len())?;
	budget.admit(
		live_count
			.checked_mul(std::mem::size_of::<super::LiveCatalogEntry>())
			.ok_or_else(|| invalid("LIVE catalog allocation length overflows"))?,
	)?;
	let mut live_queries = Vec::new();
	live_queries
		.try_reserve_exact(live_count)
		.map_err(|_error| invalid("LIVE catalog allocation failed"))?;
	for _ in 0..live_count {
		live_queries.push(super::live::decode(&mut decoder, &mut budget)?);
	}
	if binding_count
		> decoder.bytes.len().saturating_sub(decoder.position) / super::live_binding::MIN_BYTES
	{
		return Err(invalid("LIVE binding count exceeds remaining catalog bytes"));
	}
	budget.admit(
		binding_count
			.checked_mul(std::mem::size_of::<super::LiveBindingCatalogEntry>())
			.ok_or_else(|| invalid("LIVE binding allocation length overflows"))?,
	)?;
	let mut live_bindings = Vec::new();
	live_bindings
		.try_reserve_exact(binding_count)
		.map_err(|_error| invalid("LIVE binding allocation failed"))?;
	for _ in 0..binding_count {
		live_bindings.push(super::live_binding::decode(&mut decoder, &mut budget)?);
	}
	if history_count > decoder.remaining_bytes() / super::live_history::MIN_BYTES {
		return Err(invalid("LIVE history count exceeds remaining bytes"));
	}
	budget.admit(
		history_count
			.checked_mul(std::mem::size_of::<super::LiveHistoryCatalogEntry>())
			.ok_or_else(|| invalid("LIVE history allocation overflows"))?,
	)?;
	let mut live_history = Vec::new();
	live_history
		.try_reserve_exact(history_count)
		.map_err(|_error| invalid("LIVE history allocation failed"))?;
	for _ in 0..history_count {
		live_history.push(super::live_history::decode(&mut decoder, &mut budget)?);
	}
	if event_count > decoder.remaining_bytes() / super::event::MIN_BYTES {
		return Err(invalid("EVENT count exceeds remaining catalog bytes"));
	}
	budget.admit(
		event_count
			.checked_mul(std::mem::size_of::<crate::datastore::EventDefinition>())
			.ok_or_else(|| invalid("EVENT catalog allocation overflows"))?,
	)?;
	let mut events = Vec::new();
	events
		.try_reserve_exact(event_count)
		.map_err(|error| invalid("EVENT catalog allocation failed").with_technical_source(error))?;
	for _ in 0..event_count {
		events.push(super::event::decode(&mut decoder, &mut budget)?);
	}
	if outbox_count > decoder.remaining_bytes() / 128 {
		return Err(invalid("EVENT outbox segment count exceeds encoded bytes"));
	}
	budget.admit(
		outbox_count
			.checked_mul(std::mem::size_of::<crate::storage::HistorySegment>())
			.ok_or_else(|| invalid("EVENT outbox segment bytes overflow"))?,
	)?;
	let mut event_outbox_segments = Vec::new();
	event_outbox_segments.try_reserve_exact(outbox_count).map_err(|error| {
		invalid("EVENT outbox segment allocation failed").with_technical_source(error)
	})?;
	for _ in 0..outbox_count {
		event_outbox_segments.push(super::history_segment::decode(&mut decoder)?);
	}
	decoder.finish()?;
	let catalog = DatabaseCatalog {
		event_outbox_segments,
		events,
		database_uuid,
		committed_at_ns,
		changefeeds,
		live_queries,
		live_bindings,
		live_history,
		next_graph_id,
		next_schema_id,
		time_zones,
		atom_store: AtomStoreCatalogBinding {
			generation,
			primary,
			reverse_membership,
		},
		classes,
		graphs,
		parameters,
		functions,
		analyzers,
	};
	validate(&catalog)?;
	Ok(catalog)
}

fn encode_class(output: &mut Vec<u8>, class: &ClassCatalogEntry) -> FerrosGrynnResult<()> {
	output.extend_from_slice(&class.id.get().to_le_bytes());
	output.extend_from_slice(&class.version.get().to_le_bytes());
	output.extend_from_slice(&class.published_at.to_le_bytes());
	output.push(atom_kind(class.kind));
	output.push(class_mode(class.mode));
	output.extend_from_slice(&0_u16.to_le_bytes());
	write_u16(output, class.fields.len(), "class field count")?;
	write_u16(output, class.indexes.len(), "class index count")?;
	write_string(output, class.name.as_str(), "class name")?;
	for field in &class.fields {
		output.extend_from_slice(&field.id.get().to_le_bytes());
		output.extend_from_slice(&field.version.get().to_le_bytes());
		output.extend_from_slice(&field.property_key.get().to_le_bytes());
		write_string(output, field.name.as_str(), "class field name")?;
		type_codec::encode(output, &field.value_type, 0)?;
	}
	for index in &class.indexes {
		encode_index(output, index)?;
	}
	Ok(())
}

fn decode_class(decoder: &mut Decoder<'_>) -> FerrosGrynnResult<ClassCatalogEntry> {
	let id = ClassId::from_storage(decoder.read_u32()?)?;
	let version = SchemaVersion::from_storage(decoder.read_u32()?)?;
	let published_at = decoder.read_u64()?;
	let kind = decode_atom_kind(decoder.read_u8()?)?;
	let mode = decode_class_mode(decoder.read_u8()?)?;
	decoder.expect_zero(2, "class entry reserved bytes")?;
	let field_count = usize::from(decoder.read_u16()?);
	let index_count = usize::from(decoder.read_u16()?);
	let name = read_string(decoder, "class name")?.into();
	let mut fields = Vec::with_capacity(field_count);
	for _ in 0..field_count {
		fields.push(ClassFieldCatalogEntry {
			id: SchemaId::from_storage(decoder.read_u32()?)?,
			version: SchemaVersion::from_storage(decoder.read_u32()?)?,
			property_key: PropertyKeyId::new(decoder.read_u32()?),
			name: read_string(decoder, "class field name")?.into(),
			value_type: type_codec::decode(decoder, 0)?,
		});
	}
	let mut indexes = Vec::with_capacity(index_count);
	for _ in 0..index_count {
		indexes.push(decode_index(decoder)?);
	}
	Ok(ClassCatalogEntry {
		id,
		version,
		published_at,
		name,
		kind,
		mode,
		fields,
		indexes,
	})
}

fn encode_index(output: &mut Vec<u8>, index: &ClassIndexCatalogEntry) -> FerrosGrynnResult<()> {
	output.extend_from_slice(&index.id.get().to_le_bytes());
	output.extend_from_slice(&index.version.get().to_le_bytes());
	output.push(index_kind(index.kind));
	output.push(index_state(index.state));
	match index.scope {
		IndexScope::Base => {
			output.push(0);
			output.push(index_maintenance(index.maintenance));
			output.extend_from_slice(&0_u64.to_le_bytes());
		}
		IndexScope::Effective {
			graph,
		} => {
			output.push(1);
			output.push(index_maintenance(index.maintenance));
			output.extend_from_slice(&graph.get().to_le_bytes());
		}
	}
	write_u16(output, index.fields.len(), "index field count")?;
	let options = encode_index_options(index.options)?;
	write_u16(output, options.len(), "index options length")?;
	output.extend_from_slice(&index.build_revision.to_le_bytes());
	write_optional_binding(output, index.physical);
	write_optional_binding(output, index.delta);
	write_string(output, index.name.as_str(), "index name")?;
	for field in &index.fields {
		output.extend_from_slice(&field.get().to_le_bytes());
	}
	output.extend_from_slice(&options);
	Ok(())
}

fn decode_index(decoder: &mut Decoder<'_>) -> FerrosGrynnResult<ClassIndexCatalogEntry> {
	let id = IndexId::from_storage(decoder.read_u32()?)?;
	let version = SchemaVersion::from_storage(decoder.read_u32()?)?;
	let kind = decode_index_kind(decoder.read_u8()?)?;
	let state = decode_index_state(decoder.read_u8()?)?;
	let scope_kind = decoder.read_u8()?;
	let maintenance = decode_index_maintenance(decoder.read_u8()?)?;
	let scope_graph = decoder.read_u64()?;
	let scope = match (scope_kind, scope_graph) {
		(0, 0) => IndexScope::Base,
		(1, graph) => IndexScope::Effective {
			graph: GraphId::new(graph)
				.ok_or_else(|| invalid("effective index uses graph identity zero"))?,
		},
		_ => return Err(invalid("index scope encoding is not canonical")),
	};
	let field_count = usize::from(decoder.read_u16()?);
	let options_len = usize::from(decoder.read_u16()?);
	let build_revision = decoder.read_u64()?;
	let physical = read_optional_binding(decoder)?;
	let delta = read_optional_binding(decoder)?;
	let name = read_string(decoder, "index name")?.into();
	let mut fields = Vec::with_capacity(field_count);
	for _ in 0..field_count {
		fields.push(PropertyKeyId::new(decoder.read_u32()?));
	}
	let mut options_decoder = Decoder::new(decoder.read_exact(options_len)?);
	let options = decode_index_options(kind, &mut options_decoder)?;
	options_decoder.finish()?;
	Ok(ClassIndexCatalogEntry {
		id,
		version,
		name,
		kind,
		state,
		maintenance,
		build_revision,
		scope,
		fields,
		options,
		physical,
		delta,
	})
}

fn encode_index_options(options: ClassIndexOptions) -> FerrosGrynnResult<Vec<u8>> {
	let mut output = Vec::new();
	match options {
		ClassIndexOptions::Scalar => {}
		ClassIndexOptions::Geometry(options) => {
			output.push(options.geometry_type().map_or(0, |kind| match kind {
				crate::values::GeometryKind::Point => 1,
				crate::values::GeometryKind::MultiPoint => 2,
				crate::values::GeometryKind::LineString => 3,
				crate::values::GeometryKind::MultiLineString => 4,
				crate::values::GeometryKind::Polygon => 5,
				crate::values::GeometryKind::MultiPolygon => 6,
				crate::values::GeometryKind::Collection => 7,
			}));
			output.extend_from_slice(&[1, 0, 0, 0, 0, 0, 0]);
		}
		ClassIndexOptions::Fulltext(options) => {
			output.extend_from_slice(&options.analyzer.get().to_le_bytes());
			output.extend_from_slice(&options.analyzer_version.get().to_le_bytes());
			output.extend_from_slice(&options.k1_bits.to_le_bytes());
			output.extend_from_slice(&options.b_bits.to_le_bytes());
			output.push(u8::from(options.highlights));
			output.extend_from_slice(&[0; 7]);
		}
		ClassIndexOptions::Hnsw(options) => {
			output.push(vector_kind(options.element));
			output.extend_from_slice(&[0; 3]);
			output.extend_from_slice(&options.dimension.get().to_le_bytes());
			output.extend_from_slice(&options.m.get().to_le_bytes());
			output.extend_from_slice(&options.ef_construction.get().to_le_bytes());
			output.extend_from_slice(&options.m0.get().to_le_bytes());
			encode_vector_distance(&mut output, options.distance);
		}
		ClassIndexOptions::DiskAnn(options) => {
			output.push(vector_kind(options.element));
			output.push(u8::from(options.hashed_vector));
			output.extend_from_slice(&options.dimension.get().to_le_bytes());
			output.extend_from_slice(&options.degree.get().to_le_bytes());
			output.extend_from_slice(&options.build_list_size.get().to_le_bytes());
			output.extend_from_slice(&options.alpha_bits.to_le_bytes());
			encode_vector_distance(&mut output, options.distance);
		}
	}
	Ok(output)
}

fn decode_index_options(
	kind: ClassIndexKind,
	decoder: &mut Decoder<'_>,
) -> FerrosGrynnResult<ClassIndexOptions> {
	match kind {
		ClassIndexKind::Geometry => {
			let kind = match decoder.read_u8()? {
				0 => None,
				1 => Some(crate::values::GeometryKind::Point),
				2 => Some(crate::values::GeometryKind::MultiPoint),
				3 => Some(crate::values::GeometryKind::LineString),
				4 => Some(crate::values::GeometryKind::MultiLineString),
				5 => Some(crate::values::GeometryKind::Polygon),
				6 => Some(crate::values::GeometryKind::MultiPolygon),
				7 => Some(crate::values::GeometryKind::Collection),
				_ => return Err(invalid("unknown geometry TYPE")),
			};
			if decoder.read_u8()? != 1 {
				return Err(invalid("unknown geometry model"));
			}
			decoder.expect_zero(6, "geometry options reserved bytes")?;
			Ok(ClassIndexOptions::Geometry(crate::datastore::GeometryIndexOptions::new(kind)))
		}
		ClassIndexKind::Standard | ClassIndexKind::Unique | ClassIndexKind::Count => {
			Ok(ClassIndexOptions::Scalar)
		}
		ClassIndexKind::Fulltext => {
			let analyzer = AnalyzerId::from_storage(decoder.read_u32()?)?;
			let analyzer_version = SchemaVersion::from_storage(decoder.read_u32()?)?;
			let k1 = f64::from_bits(decoder.read_u64()?);
			let b = f64::from_bits(decoder.read_u64()?);
			let highlights = match decoder.read_u8()? {
				0 => false,
				1 => true,
				_ => return Err(invalid("Fulltext highlight flag is invalid")),
			};
			decoder.expect_zero(7, "Fulltext options reserved bytes")?;
			Ok(ClassIndexOptions::Fulltext(FulltextIndexOptions::new(
				analyzer,
				analyzer_version,
				k1,
				b,
				highlights,
			)?))
		}
		ClassIndexKind::Hnsw => {
			let element = decode_vector_kind(decoder.read_u8()?)?;
			decoder.expect_zero(3, "HNSW options reserved bytes")?;
			let dimension = nonzero_u16(decoder.read_u16()?, "HNSW dimension")?;
			let m = nonzero_u16(decoder.read_u16()?, "HNSW M")?;
			let ef_construction = nonzero_u16(decoder.read_u16()?, "HNSW EFC")?;
			let m0 = nonzero_u16(decoder.read_u16()?, "HNSW M0")?;
			let distance = decode_vector_distance(decoder)?;
			distance.validate()?;
			Ok(ClassIndexOptions::Hnsw(HnswIndexOptions {
				element,
				dimension,
				distance,
				m,
				ef_construction,
				m0,
			}))
		}
		ClassIndexKind::DiskAnn => {
			let element = decode_vector_kind(decoder.read_u8()?)?;
			let hashed_vector = match decoder.read_u8()? {
				0 => false,
				1 => true,
				_ => return Err(invalid("DiskANN HASHED_VECTOR flag is invalid")),
			};
			let dimension = nonzero_u16(decoder.read_u16()?, "DiskANN dimension")?;
			let degree = nonzero_u16(decoder.read_u16()?, "DiskANN degree")?;
			let build_list_size = nonzero_u16(decoder.read_u16()?, "DiskANN build list")?;
			let alpha_bits = decoder.read_u32()?;
			let alpha = f32::from_bits(alpha_bits);
			if !alpha.is_finite() || alpha <= 0.0 {
				return Err(invalid("DiskANN alpha must be finite and positive"));
			}
			let distance = decode_vector_distance(decoder)?;
			distance.validate()?;
			Ok(ClassIndexOptions::DiskAnn(DiskAnnIndexOptions {
				element,
				dimension,
				distance,
				degree,
				build_list_size,
				alpha_bits,
				hashed_vector,
			}))
		}
	}
}

fn encode_vector_distance(output: &mut Vec<u8>, distance: VectorDistance) {
	let (tag, function, version, parameter) = match distance {
		VectorDistance::Euclidean => (0, 0, 0, 0),
		VectorDistance::Cosine => (1, 0, 0, 0),
		VectorDistance::Manhattan => (2, 0, 0, 0),
		VectorDistance::Minkowski(exponent) => (3, 0, 0, exponent.to_bits()),
		VectorDistance::InnerProduct => (4, 0, 0, 0),
		VectorDistance::CosineNormalized => (5, 0, 0, 0),
		VectorDistance::Custom {
			function,
			version,
		} => (6, function.get(), version.get(), 0),
	};
	output.push(tag);
	output.extend_from_slice(&[0; 3]);
	output.extend_from_slice(&function.to_le_bytes());
	output.extend_from_slice(&version.to_le_bytes());
	output.extend_from_slice(&parameter.to_le_bytes());
}

fn decode_vector_distance(decoder: &mut Decoder<'_>) -> FerrosGrynnResult<VectorDistance> {
	let tag = decoder.read_u8()?;
	decoder.expect_zero(3, "vector distance reserved bytes")?;
	let function = decoder.read_u32()?;
	let version = decoder.read_u32()?;
	let parameter = decoder.read_u64()?;
	match (tag, function, version, parameter) {
		(0, 0, 0, 0) => Ok(VectorDistance::Euclidean),
		(1, 0, 0, 0) => Ok(VectorDistance::Cosine),
		(2, 0, 0, 0) => Ok(VectorDistance::Manhattan),
		(3, 0, 0, bits) => Ok(VectorDistance::Minkowski(f64::from_bits(bits))),
		(4, 0, 0, 0) => Ok(VectorDistance::InnerProduct),
		(5, 0, 0, 0) => Ok(VectorDistance::CosineNormalized),
		(6, function, version, 0) => Ok(VectorDistance::Custom {
			function: FunctionId::from_storage(function)?,
			version: SchemaVersion::from_storage(version)?,
		}),
		_ => Err(invalid("vector distance encoding is not canonical")),
	}
}

fn nonzero_u16(value: u16, field: &str) -> FerrosGrynnResult<std::num::NonZeroU16> {
	std::num::NonZeroU16::new(value).ok_or_else(|| invalid(format!("{field} is zero")))
}

fn encode_graph(output: &mut Vec<u8>, graph: &GraphCatalogEntry) -> FerrosGrynnResult<()> {
	output.extend_from_slice(&graph.id.get().to_le_bytes());
	output.extend_from_slice(&graph.version.get().to_le_bytes());
	output.extend_from_slice(&graph.published_at.to_le_bytes());
	output.extend_from_slice(&graph.generation.to_le_bytes());
	output.push(type_codec::graph_kind(graph.kind));
	output.push(storage_mode(graph.storage_mode));
	output.extend_from_slice(&graph.topology_constraints.bits().to_le_bytes());
	output.extend_from_slice(&graph.label_index.0.get().to_le_bytes());
	output.extend_from_slice(&graph.label_index.1.get().to_le_bytes());
	output.extend_from_slice(&graph.adjacency_index.0.get().to_le_bytes());
	output.extend_from_slice(&graph.adjacency_index.1.get().to_le_bytes());
	write_binding(output, graph.membership);
	output.extend_from_slice(&graph.owner_root.map_or(0, GraphId::get).to_le_bytes());
	write_u32(output, graph.root.as_ref().map_or(0, Vec::len), "graph root value bytes")?;
	write_u32(output, graph.properties.len(), "graph property count")?;
	write_u32(output, graph.arena.graph_by_slot.len(), "graph arena slot count")?;
	write_u32(output, graph.indexes.len(), "graph user index count")?;
	let (endpoint_id, endpoint_version) =
		graph.edge_endpoint_index.map_or((0, 0), |(id, version)| (id.get(), version.get()));
	output.extend_from_slice(&endpoint_id.to_le_bytes());
	output.extend_from_slice(&endpoint_version.to_le_bytes());
	write_optional_binding(output, graph.edge_endpoint);
	write_string(output, graph.name.as_str(), "graph name")?;
	if let Some(root) = &graph.root {
		output.extend_from_slice(root);
	}
	for property in &graph.properties {
		output.extend_from_slice(&property.key.get().to_le_bytes());
		write_u32(output, property.value.len(), "graph property value bytes")?;
		output.extend_from_slice(&property.value);
	}
	for (slot, child) in &graph.arena.graph_by_slot {
		let scope = graph
			.arena
			.scope_by_slot
			.get(slot)
			.ok_or_else(|| invalid("graph arena slot has no scope path"))?;
		output.extend_from_slice(&slot.to_le_bytes());
		output.extend_from_slice(&child.get().to_le_bytes());
		write_u16(output, scope.len(), "graph scope depth")?;
		output.extend_from_slice(&0_u16.to_le_bytes());
		for atom in scope.iter() {
			output.extend_from_slice(&atom.raw().to_le_bytes());
		}
	}
	for index in &graph.indexes {
		super::graph_index::encode(output, index)?;
		write_optional_binding(
			output,
			index.id().and_then(|id| graph.index_bindings.get(&id).copied()),
		);
	}
	output.extend_from_slice(&graph.tensor_revision.to_le_bytes());
	Ok(())
}

fn decode_graph(decoder: &mut Decoder<'_>) -> FerrosGrynnResult<GraphCatalogEntry> {
	let id = GraphId::new(decoder.read_u64()?)
		.ok_or_else(|| invalid("graph catalog uses identity zero"))?;
	let version = SchemaVersion::from_storage(decoder.read_u32()?)?;
	let published_at = decoder.read_u64()?;
	let generation = decoder.read_u64()?;
	let kind = type_codec::decode_graph_kind(decoder.read_u8()?)?;
	let storage_mode = decode_storage_mode(decoder.read_u8()?)?;
	let topology_constraints = GraphTopologyConstraints::from_bits(decoder.read_u16()?)
		.ok_or_else(|| invalid("graph entry contains unknown topology constraint bits"))?;
	let label_index = (
		IndexId::from_storage(decoder.read_u32()?)?,
		SchemaVersion::from_storage(decoder.read_u32()?)?,
	);
	let adjacency_index = (
		IndexId::from_storage(decoder.read_u32()?)?,
		SchemaVersion::from_storage(decoder.read_u32()?)?,
	);
	let membership = read_binding(decoder)?;
	let owner_root = match decoder.read_u64()? {
		0 => None,
		value => Some(
			GraphId::new(value).ok_or_else(|| invalid("graph owner root uses identity zero"))?,
		),
	};
	let root_len = decoder.read_count("graph root value bytes")?;
	let property_count = decoder.read_count("graph property count")?;
	let slot_count = decoder.read_count("graph arena slot count")?;
	let index_count = decoder.read_count("graph user index count")?;
	if index_count > 4096 {
		return Err(invalid("graph user index count exceeds its bound"));
	}
	let edge_endpoint_index = match (decoder.read_u32()?, decoder.read_u32()?) {
		(0, 0) => None,
		(id, version) => Some((IndexId::from_storage(id)?, SchemaVersion::from_storage(version)?)),
	};
	let edge_endpoint = read_optional_binding(decoder)?;
	let name = read_string(decoder, "graph name")?.into();
	let root = match root_len {
		0 => None,
		length => Some(decoder.read_exact(length)?.to_vec()),
	};
	let mut properties = Vec::with_capacity(property_count);
	for _ in 0..property_count {
		let key = PropertyKeyId::new(decoder.read_u32()?);
		let value_len = decoder.read_count("graph property value bytes")?;
		let value = decoder.read_exact(value_len)?.to_vec();
		properties.push(GraphPropertyCatalogEntry {
			key,
			value,
		});
	}
	let mut graph_by_slot = std::collections::BTreeMap::new();
	let mut scope_by_slot = std::collections::BTreeMap::new();
	for _ in 0..slot_count {
		let slot = decoder.read_u32()?;
		let child = GraphId::new(decoder.read_u64()?)
			.ok_or_else(|| invalid("graph arena contains identity zero"))?;
		let depth = usize::from(decoder.read_u16()?);
		decoder.expect_zero(2, "graph arena reserved bytes")?;
		let mut scope = Vec::with_capacity(depth);
		for _ in 0..depth {
			let atom = AtomId::from_raw(decoder.read_u64()?);
			if atom.kind().is_none() {
				return Err(invalid("graph arena scope contains a null atom"));
			}
			scope.push(atom);
		}
		if graph_by_slot.insert(slot, child).is_some()
			|| scope_by_slot.insert(slot, scope.into_boxed_slice()).is_some()
		{
			return Err(invalid("graph arena contains duplicate slots"));
		}
	}
	let mut indexes = Vec::new();
	let mut index_bindings = BTreeMap::new();
	for _ in 0..index_count {
		let index = super::graph_index::decode(decoder)?;
		let kind = if matches!(
			index.definition(),
			crate::datastore::GraphIndexDefinition::Tensor { .. }
		) {
			DataFileKind::TensorCache
		} else {
			DataFileKind::Index
		};
		if let Some(binding) = read_optional_binding_kind(decoder, kind)? {
			let id = index
				.id()
				.ok_or_else(|| invalid("graph artifact binding has no definition identity"))?;
			if index_bindings.insert(id, binding).is_some() {
				return Err(invalid("graph artifact bindings are duplicated"));
			}
		}
		indexes.push(index);
	}
	let tensor_revision = decoder.read_u64()?;
	if tensor_revision > generation {
		return Err(invalid("graph incidence revision exceeds its publication"));
	}
	Ok(GraphCatalogEntry {
		tensor_revision,
		id,
		version,
		published_at,
		generation,
		name,
		kind,
		storage_mode,
		topology_constraints,
		label_index,
		adjacency_index,
		edge_endpoint_index,
		edge_endpoint,
		indexes,
		index_bindings,
		membership,
		owner_root,
		root,
		properties,
		arena: GraphArenaBindings {
			graph_by_slot,
			scope_by_slot,
		},
	})
}

fn encode_parameter(
	output: &mut Vec<u8>,
	parameter: &ParameterCatalogEntry,
) -> FerrosGrynnResult<()> {
	output.extend_from_slice(&parameter.id.get().to_le_bytes());
	output.extend_from_slice(&parameter.version.get().to_le_bytes());
	output.extend_from_slice(&parameter.published_at.to_le_bytes());
	write_u32(output, parameter.value.len(), "parameter value length")?;
	write_optional_string(
		output,
		parameter.comment.as_ref().map(|value| value.as_str()),
		"parameter comment",
	)?;
	write_string(output, parameter.name.as_str(), "parameter name")?;
	output.extend_from_slice(&parameter.value);
	Ok(())
}

fn decode_parameter(decoder: &mut Decoder<'_>) -> FerrosGrynnResult<ParameterCatalogEntry> {
	let id = SchemaId::from_storage(decoder.read_u32()?)?;
	let version = SchemaVersion::from_storage(decoder.read_u32()?)?;
	let published_at = decoder.read_u64()?;
	let value_len = decoder.read_count("parameter value length")?;
	let comment = read_optional_string(decoder, "parameter comment")?.map(Into::into);
	let name = read_string(decoder, "parameter name")?.into();
	let value = decoder.read_exact(value_len)?.to_vec();
	Ok(ParameterCatalogEntry {
		id,
		version,
		published_at,
		name,
		value,
		comment,
	})
}

fn encode_analyzer(output: &mut Vec<u8>, analyzer: &AnalyzerCatalogEntry) -> FerrosGrynnResult<()> {
	output.extend_from_slice(&analyzer.id.get().to_le_bytes());
	output.extend_from_slice(&analyzer.version.get().to_le_bytes());
	output.extend_from_slice(&analyzer.published_at.to_le_bytes());
	output.push(tokenizer_bits(&analyzer.tokenizers));
	output.push(u8::from(analyzer.function.is_some()));
	write_u16(output, analyzer.filters.len(), "analyzer filter count")?;
	match analyzer.function {
		Some((id, version)) => {
			output.extend_from_slice(&id.get().to_le_bytes());
			output.extend_from_slice(&version.get().to_le_bytes());
		}
		None => output.extend_from_slice(&0_u64.to_le_bytes()),
	}
	write_string(output, analyzer.name.as_str(), "analyzer name")?;
	write_optional_string(
		output,
		analyzer.comment.as_ref().map(|comment| comment.as_str()),
		"analyzer comment",
	)?;
	for filter in &analyzer.filters {
		encode_analyzer_filter(output, filter)?;
	}
	Ok(())
}

fn decode_analyzer(decoder: &mut Decoder<'_>) -> FerrosGrynnResult<AnalyzerCatalogEntry> {
	let id = AnalyzerId::from_storage(decoder.read_u32()?)?;
	let version = SchemaVersion::from_storage(decoder.read_u32()?)?;
	let published_at = decoder.read_u64()?;
	let tokenizers = decode_tokenizer_bits(decoder.read_u8()?)?;
	let has_function = match decoder.read_u8()? {
		0 => false,
		1 => true,
		_ => return Err(invalid("analyzer function flag is invalid")),
	};
	let filter_count = usize::from(decoder.read_u16()?);
	let function_id = decoder.read_u32()?;
	let function_version = decoder.read_u32()?;
	let function = match (has_function, function_id, function_version) {
		(false, 0, 0) => None,
		(true, id, version) => {
			Some((FunctionId::from_storage(id)?, SchemaVersion::from_storage(version)?))
		}
		_ => return Err(invalid("analyzer function reference is not canonical")),
	};
	let name = read_string(decoder, "analyzer name")?.into();
	let comment = read_optional_string(decoder, "analyzer comment")?.map(Into::into);
	let mut filters = Vec::with_capacity(filter_count);
	for _ in 0..filter_count {
		filters.push(decode_analyzer_filter(decoder)?);
	}
	Ok(AnalyzerCatalogEntry {
		id,
		version,
		published_at,
		name,
		tokenizers,
		filters,
		function,
		comment,
	})
}

fn encode_analyzer_filter(
	output: &mut Vec<u8>,
	filter: &AnalyzerFilterCatalogEntry,
) -> FerrosGrynnResult<()> {
	match filter {
		AnalyzerFilterCatalogEntry::Nfc => output.push(0),
		AnalyzerFilterCatalogEntry::Nfkc => output.push(1),
		AnalyzerFilterCatalogEntry::Ascii => output.push(2),
		AnalyzerFilterCatalogEntry::Lowercase => output.push(3),
		AnalyzerFilterCatalogEntry::Uppercase => output.push(4),
		AnalyzerFilterCatalogEntry::EdgeNgram {
			min,
			max,
		} => {
			output.push(5);
			output.extend_from_slice(&min.to_le_bytes());
			output.extend_from_slice(&max.to_le_bytes());
		}
		AnalyzerFilterCatalogEntry::Ngram {
			min,
			max,
		} => {
			output.push(6);
			output.extend_from_slice(&min.to_le_bytes());
			output.extend_from_slice(&max.to_le_bytes());
		}
		AnalyzerFilterCatalogEntry::Snowball(language) => {
			output.push(7);
			output.push(analyzer_language(*language));
		}
		AnalyzerFilterCatalogEntry::Mapper(value) => {
			output.push(8);
			write_u32(output, value.len(), "analyzer mapper bytes")?;
			output.extend_from_slice(value);
		}
	}
	Ok(())
}

fn decode_analyzer_filter(
	decoder: &mut Decoder<'_>,
) -> FerrosGrynnResult<AnalyzerFilterCatalogEntry> {
	match decoder.read_u8()? {
		0 => Ok(AnalyzerFilterCatalogEntry::Nfc),
		1 => Ok(AnalyzerFilterCatalogEntry::Nfkc),
		2 => Ok(AnalyzerFilterCatalogEntry::Ascii),
		3 => Ok(AnalyzerFilterCatalogEntry::Lowercase),
		4 => Ok(AnalyzerFilterCatalogEntry::Uppercase),
		5 => Ok(AnalyzerFilterCatalogEntry::EdgeNgram {
			min: decoder.read_u16()?,
			max: decoder.read_u16()?,
		}),
		6 => Ok(AnalyzerFilterCatalogEntry::Ngram {
			min: decoder.read_u16()?,
			max: decoder.read_u16()?,
		}),
		7 => decode_analyzer_language(decoder.read_u8()?).map(AnalyzerFilterCatalogEntry::Snowball),
		8 => {
			let count = decoder.read_count("analyzer mapper bytes")?;
			Ok(AnalyzerFilterCatalogEntry::Mapper(decoder.read_exact(count)?.to_vec()))
		}
		_ => Err(invalid("analyzer filter tag is invalid")),
	}
}

fn tokenizer_bits(tokenizers: &std::collections::BTreeSet<AnalyzerTokenizer>) -> u8 {
	tokenizers.iter().fold(0, |bits, tokenizer| {
		bits | match tokenizer {
			AnalyzerTokenizer::Blank => 1,
			AnalyzerTokenizer::Camel => 2,
			AnalyzerTokenizer::Class => 4,
			AnalyzerTokenizer::Punct => 8,
		}
	})
}

fn decode_tokenizer_bits(
	bits: u8,
) -> FerrosGrynnResult<std::collections::BTreeSet<AnalyzerTokenizer>> {
	if bits & !0x0f != 0 {
		return Err(invalid("analyzer tokenizer bits are invalid"));
	}
	let mut result = std::collections::BTreeSet::new();
	for (bit, tokenizer) in [
		(1, AnalyzerTokenizer::Blank),
		(2, AnalyzerTokenizer::Camel),
		(4, AnalyzerTokenizer::Class),
		(8, AnalyzerTokenizer::Punct),
	] {
		if bits & bit != 0 {
			result.insert(tokenizer);
		}
	}
	Ok(result)
}

const fn analyzer_language(language: AnalyzerLanguage) -> u8 {
	match language {
		AnalyzerLanguage::Arabic => 0,
		AnalyzerLanguage::Danish => 1,
		AnalyzerLanguage::Dutch => 2,
		AnalyzerLanguage::English => 3,
		AnalyzerLanguage::Finnish => 4,
		AnalyzerLanguage::French => 5,
		AnalyzerLanguage::German => 6,
		AnalyzerLanguage::Greek => 7,
		AnalyzerLanguage::Hungarian => 8,
		AnalyzerLanguage::Italian => 9,
		AnalyzerLanguage::Norwegian => 10,
		AnalyzerLanguage::Portuguese => 11,
		AnalyzerLanguage::Romanian => 12,
		AnalyzerLanguage::Russian => 13,
		AnalyzerLanguage::Spanish => 14,
		AnalyzerLanguage::Swedish => 15,
		AnalyzerLanguage::Tamil => 16,
		AnalyzerLanguage::Turkish => 17,
	}
}

fn decode_analyzer_language(value: u8) -> FerrosGrynnResult<AnalyzerLanguage> {
	match value {
		0 => Ok(AnalyzerLanguage::Arabic),
		1 => Ok(AnalyzerLanguage::Danish),
		2 => Ok(AnalyzerLanguage::Dutch),
		3 => Ok(AnalyzerLanguage::English),
		4 => Ok(AnalyzerLanguage::Finnish),
		5 => Ok(AnalyzerLanguage::French),
		6 => Ok(AnalyzerLanguage::German),
		7 => Ok(AnalyzerLanguage::Greek),
		8 => Ok(AnalyzerLanguage::Hungarian),
		9 => Ok(AnalyzerLanguage::Italian),
		10 => Ok(AnalyzerLanguage::Norwegian),
		11 => Ok(AnalyzerLanguage::Portuguese),
		12 => Ok(AnalyzerLanguage::Romanian),
		13 => Ok(AnalyzerLanguage::Russian),
		14 => Ok(AnalyzerLanguage::Spanish),
		15 => Ok(AnalyzerLanguage::Swedish),
		16 => Ok(AnalyzerLanguage::Tamil),
		17 => Ok(AnalyzerLanguage::Turkish),
		_ => Err(invalid("analyzer Snowball language is invalid")),
	}
}

pub(super) fn write_optional_string(
	output: &mut Vec<u8>,
	value: Option<&str>,
	field: &str,
) -> FerrosGrynnResult<()> {
	match value {
		None => output.extend_from_slice(&u16::MAX.to_le_bytes()),
		Some(value) => write_string(output, value, field)?,
	}
	Ok(())
}

pub(super) fn read_optional_string<'a>(
	decoder: &mut Decoder<'a>,
	field: &str,
) -> FerrosGrynnResult<Option<&'a str>> {
	let length = decoder.read_u16()?;
	if length == u16::MAX {
		return Ok(None);
	}
	let bytes = decoder.read_exact(usize::from(length))?;
	let value =
		std::str::from_utf8(bytes).map_err(|_error| invalid(format!("{field} is not UTF-8")))?;
	if value.len() > MAX_STRING_BYTES {
		return Err(invalid(format!("{field} exceeds its format limit")));
	}
	if value.contains('\0') {
		return Err(invalid(format!("{field} contains NUL")));
	}
	decoder.admit_heap(value.len().saturating_mul(2).saturating_add(64))?;
	Ok(Some(value))
}

fn write_binding(output: &mut Vec<u8>, binding: PhysicalIndexBinding) {
	output.extend_from_slice(&binding.file_id.get().to_le_bytes());
	output.extend_from_slice(&binding.root.get().to_le_bytes());
}

fn read_binding(decoder: &mut Decoder<'_>) -> FerrosGrynnResult<PhysicalIndexBinding> {
	let file_id = DataFileId::from_raw(decoder.read_u64()?)
		.ok_or_else(|| invalid("catalog binding contains an unknown file identity"))?;
	let root = PageNo::new(decoder.read_u64()?);
	if file_id.kind() != Some(DataFileKind::Index) || root == PageNo::NULL {
		return Err(invalid("catalog index binding is invalid"));
	}
	Ok(PhysicalIndexBinding {
		file_id,
		root,
	})
}

fn write_optional_binding(output: &mut Vec<u8>, binding: Option<PhysicalIndexBinding>) {
	match binding {
		Some(binding) => write_binding(output, binding),
		None => {
			output.extend_from_slice(&0_u64.to_le_bytes());
			output.extend_from_slice(&PageNo::NULL.get().to_le_bytes());
		}
	}
}

fn read_optional_binding(
	decoder: &mut Decoder<'_>,
) -> FerrosGrynnResult<Option<PhysicalIndexBinding>> {
	read_optional_binding_kind(decoder, DataFileKind::Index)
}

/// Decodes the owning family's physical kind without weakening ordinary
/// indexes.
fn read_optional_binding_kind(
	decoder: &mut Decoder<'_>,
	kind: DataFileKind,
) -> FerrosGrynnResult<Option<PhysicalIndexBinding>> {
	let raw_file = decoder.read_u64()?;
	let root = PageNo::new(decoder.read_u64()?);
	if raw_file == 0 && root == PageNo::NULL {
		return Ok(None);
	}
	let file_id = DataFileId::from_raw(raw_file)
		.ok_or_else(|| invalid("optional index binding contains an unknown file identity"))?;
	if file_id.kind() != Some(kind) || root == PageNo::NULL {
		return Err(invalid("optional index binding is incomplete"));
	}
	Ok(Some(PhysicalIndexBinding {
		file_id,
		root,
	}))
}

pub(super) fn write_string(
	output: &mut Vec<u8>,
	value: &str,
	field: &str,
) -> FerrosGrynnResult<()> {
	if value.is_empty() || value.contains('\0') || value.len() > MAX_STRING_BYTES {
		return Err(invalid(format!("{field} violates its string bound")));
	}
	let length =
		u16::try_from(value.len()).map_err(|_error| invalid(format!("{field} exceeds u16")))?;
	output.extend_from_slice(&length.to_le_bytes());
	output.extend_from_slice(value.as_bytes());
	Ok(())
}

pub(super) fn read_string(decoder: &mut Decoder<'_>, field: &str) -> FerrosGrynnResult<String> {
	let length = usize::from(decoder.read_u16()?);
	if length == 0 || length > MAX_STRING_BYTES {
		return Err(invalid(format!("{field} violates its string bound")));
	}
	let value = std::str::from_utf8(decoder.read_exact(length)?)
		.map_err(|_error| invalid(format!("{field} is not UTF-8")))?;
	if value.contains('\0') {
		return Err(invalid(format!("{field} contains NUL")));
	}
	decoder.admit_heap(value.len().saturating_mul(2).saturating_add(64))?;
	Ok(value.to_owned())
}

/// Writes a checked catalog length without silently truncating the source
/// descriptor.
pub(super) fn write_u16(output: &mut Vec<u8>, value: usize, field: &str) -> FerrosGrynnResult<()> {
	output.extend_from_slice(
		&u16::try_from(value)
			.map_err(|_error| invalid(format!("{field} exceeds u16")))?
			.to_le_bytes(),
	);
	Ok(())
}

/// Writes a checked catalog length without silently truncating the source
/// descriptor.
pub(super) fn write_u32(output: &mut Vec<u8>, value: usize, field: &str) -> FerrosGrynnResult<()> {
	output.extend_from_slice(
		&u32::try_from(value)
			.map_err(|_error| invalid(format!("{field} exceeds u32")))?
			.to_le_bytes(),
	);
	Ok(())
}

const fn atom_kind(value: AtomKind) -> u8 {
	match value {
		AtomKind::Node => 0,
		AtomKind::Edge => 1,
	}
}

fn decode_atom_kind(value: u8) -> FerrosGrynnResult<AtomKind> {
	match value {
		0 => Ok(AtomKind::Node),
		1 => Ok(AtomKind::Edge),
		_ => Err(invalid("class contains an unknown AtomKind")),
	}
}

const fn class_mode(value: ClassSchemaMode) -> u8 {
	match value {
		ClassSchemaMode::Schemaless => 0,
		ClassSchemaMode::Schemafull => 1,
	}
}

fn decode_class_mode(value: u8) -> FerrosGrynnResult<ClassSchemaMode> {
	match value {
		0 => Ok(ClassSchemaMode::Schemaless),
		1 => Ok(ClassSchemaMode::Schemafull),
		_ => Err(invalid("class contains an unknown schema mode")),
	}
}

const fn index_kind(value: ClassIndexKind) -> u8 {
	match value {
		ClassIndexKind::Standard => 0,
		ClassIndexKind::Unique => 1,
		ClassIndexKind::Count => 2,
		ClassIndexKind::Fulltext => 3,
		ClassIndexKind::Hnsw => 4,
		ClassIndexKind::DiskAnn => 5,
		ClassIndexKind::Geometry => 6,
	}
}

fn decode_index_kind(value: u8) -> FerrosGrynnResult<ClassIndexKind> {
	match value {
		0 => Ok(ClassIndexKind::Standard),
		1 => Ok(ClassIndexKind::Unique),
		2 => Ok(ClassIndexKind::Count),
		3 => Ok(ClassIndexKind::Fulltext),
		4 => Ok(ClassIndexKind::Hnsw),
		5 => Ok(ClassIndexKind::DiskAnn),
		6 => Ok(ClassIndexKind::Geometry),
		_ => Err(invalid("index contains an unknown kind")),
	}
}

const fn vector_kind(value: VectorKind) -> u8 {
	match value {
		VectorKind::F64 => 0,
		VectorKind::F32 => 1,
		VectorKind::F16 => 2,
		VectorKind::I64 => 3,
		VectorKind::I32 => 4,
		VectorKind::I16 => 5,
		VectorKind::I8 => 6,
		VectorKind::U8 => 7,
	}
}

fn decode_vector_kind(value: u8) -> FerrosGrynnResult<VectorKind> {
	match value {
		0 => Ok(VectorKind::F64),
		1 => Ok(VectorKind::F32),
		2 => Ok(VectorKind::F16),
		3 => Ok(VectorKind::I64),
		4 => Ok(VectorKind::I32),
		5 => Ok(VectorKind::I16),
		6 => Ok(VectorKind::I8),
		7 => Ok(VectorKind::U8),
		_ => Err(invalid("index contains an unknown vector element kind")),
	}
}

const fn index_state(value: ClassIndexState) -> u8 {
	match value {
		ClassIndexState::Ready => 0,
		ClassIndexState::Stale => 1,
		ClassIndexState::PreparedForRemoval => 2,
	}
}

fn decode_index_state(value: u8) -> FerrosGrynnResult<ClassIndexState> {
	match value {
		0 => Ok(ClassIndexState::Ready),
		1 => Ok(ClassIndexState::Stale),
		2 => Ok(ClassIndexState::PreparedForRemoval),
		_ => Err(invalid("index contains an unknown state")),
	}
}

const fn index_maintenance(value: IndexMaintenanceMode) -> u8 {
	match value {
		IndexMaintenanceMode::Immediate => 0,
		IndexMaintenanceMode::Deferred => 1,
	}
}

fn decode_index_maintenance(value: u8) -> FerrosGrynnResult<IndexMaintenanceMode> {
	match value {
		0 => Ok(IndexMaintenanceMode::Immediate),
		1 => Ok(IndexMaintenanceMode::Deferred),
		_ => Err(invalid("class index has an unknown maintenance mode")),
	}
}

const fn storage_mode(value: GraphStorageMode) -> u8 {
	match value {
		GraphStorageMode::InMemory => 0,
		GraphStorageMode::DiskBacked => 1,
		GraphStorageMode::Hybrid => 2,
	}
}

fn decode_storage_mode(value: u8) -> FerrosGrynnResult<GraphStorageMode> {
	match value {
		0 => Ok(GraphStorageMode::InMemory),
		1 => Ok(GraphStorageMode::DiskBacked),
		2 => Ok(GraphStorageMode::Hybrid),
		_ => Err(invalid("graph contains an unknown storage mode")),
	}
}

/// Consumes a bounded source descriptor without compatibility or executable-IR
/// decoding.
pub(in crate::storage) struct Decoder<'a> {
	bytes: &'a [u8],
	position: usize,
	/// Frozen source decoding owns a ledger; ordinary catalog callers retain
	/// their contract.
	admission: Option<crate::storage::history_codec::CodecBudget>,
}

impl<'a> Decoder<'a> {
	/// Bounds section allocations against the unread input.
	pub(super) fn remaining_bytes(&self) -> usize {
		self.bytes.len().saturating_sub(self.position)
	}

	/// Starts a checked decoder at the beginning of its owned descriptor slice.
	pub(in crate::storage) fn new(bytes: &'a [u8]) -> Self {
		Self {
			bytes,
			position: 0,
			admission: None,
		}
	}

	/// Bounds every owned source allocation by the enclosing codec's remaining
	/// budget.
	pub(super) fn bounded(bytes: &'a [u8], available: usize) -> FerrosGrynnResult<Self> {
		let mut decoder = Self::new(bytes);
		decoder.admission = Some(crate::storage::history_codec::CodecBudget::new(available, 0)?);
		Ok(decoder)
	}

	/// Admits before strings, boxes or member vectors allocate.
	pub(super) fn admit_heap(&mut self, bytes: usize) -> FerrosGrynnResult<()> {
		match &mut self.admission {
			Some(budget) => budget.admit(bytes),
			None => Ok(()),
		}
	}

	/// Transfers the private source reservation into the outer payload ledger.
	pub(super) fn admitted_bytes(&self) -> usize {
		self.admission
			.as_ref()
			.map_or(0, crate::storage::history_codec::CodecBudget::admitted_bytes)
	}

	pub(super) fn read_exact(&mut self, count: usize) -> FerrosGrynnResult<&'a [u8]> {
		let end = self
			.position
			.checked_add(count)
			.ok_or_else(|| invalid("database catalog offset overflows usize"))?;
		let value = self
			.bytes
			.get(self.position..end)
			.ok_or_else(|| invalid("database catalog payload is truncated"))?;
		self.position = end;
		Ok(value)
	}

	pub(super) fn read_u8(&mut self) -> FerrosGrynnResult<u8> {
		Ok(self.read_exact(1)?[0])
	}

	pub(super) fn read_u16(&mut self) -> FerrosGrynnResult<u16> {
		Ok(u16::from_le_bytes(
			self.read_exact(2)?
				.try_into()
				.map_err(|_error| invalid("database catalog u16 is truncated"))?,
		))
	}

	pub(super) fn read_u32(&mut self) -> FerrosGrynnResult<u32> {
		Ok(u32::from_le_bytes(
			self.read_exact(4)?
				.try_into()
				.map_err(|_error| invalid("database catalog u32 is truncated"))?,
		))
	}

	pub(super) fn read_u64(&mut self) -> FerrosGrynnResult<u64> {
		Ok(u64::from_le_bytes(
			self.read_exact(8)?
				.try_into()
				.map_err(|_error| invalid("database catalog u64 is truncated"))?,
		))
	}

	pub(super) fn read_count(&mut self, field: &str) -> FerrosGrynnResult<usize> {
		usize::try_from(self.read_u32()?)
			.map_err(|_error| invalid(format!("{field} does not fit usize")))
	}

	pub(super) fn expect_zero(&mut self, count: usize, field: &str) -> FerrosGrynnResult<()> {
		if self.read_exact(count)?.iter().any(|byte| *byte != 0) {
			return Err(invalid(format!("{field} are non-zero")));
		}
		Ok(())
	}

	/// Requires exact descriptor consumption so trailing bytes cannot hide
	/// another record.
	pub(in crate::storage) fn finish(self) -> FerrosGrynnResult<()> {
		if self.position != self.bytes.len() {
			return Err(invalid("database catalog contains trailing bytes"));
		}
		Ok(())
	}
}

pub(super) fn invalid(reason: impl Into<String>) -> FerrosGrynnError {
	FerrosGrynnError::invalid_storage_file(reason)
}
