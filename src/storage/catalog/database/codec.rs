//! Canonical little-endian encoding for `FGDC v7`.

use super::{
	AtomStoreCatalogBinding,
	ClassCatalogEntry,
	ClassFieldCatalogEntry,
	ClassIndexCatalogEntry,
	DatabaseCatalog,
	GraphCatalogEntry,
	ParameterCatalogEntry,
	PhysicalIndexBinding,
	type_codec,
	validate::validate,
};
use crate::{
	datastore::{
		ClassId,
		ClassIndexKind,
		ClassIndexState,
		ClassSchemaMode,
		IndexId,
		IndexScope,
		SchemaId,
		SchemaVersion,
	},
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	graph::{AtomId, AtomKind, GraphId, GraphStorageMode},
	storage::{
		graph_pages::GraphArenaBindings,
		page::PageNo,
		types::{DataFileId, DataFileKind},
	},
	values::{PropertyKeyId, Text},
};

const MAGIC: [u8; 4] = *b"FGDC";
const VERSION: u16 = 7;
const HEADER_BYTES: usize = 80;
const MAX_BYTES: usize = 16 * 1024 * 1024;
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
	output.extend_from_slice(&catalog.atom_store.generation.to_le_bytes());
	write_binding(&mut output, catalog.atom_store.primary);
	write_binding(&mut output, catalog.atom_store.reverse_membership);

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
	if output.len() > MAX_BYTES {
		return Err(FerrosGrynnError::storage_decode_limit_exceeded(
			"database catalog bytes",
			MAX_BYTES as u64,
			output.len() as u64,
		));
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
	let generation = decoder.read_u64()?;
	let primary = read_binding(&mut decoder)?;
	let reverse_membership = read_binding(&mut decoder)?;

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
	decoder.finish()?;
	let catalog = DatabaseCatalog {
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
			output.push(0);
			output.extend_from_slice(&0_u64.to_le_bytes());
		}
		IndexScope::Effective {
			graph,
		} => {
			output.push(1);
			output.push(0);
			output.extend_from_slice(&graph.get().to_le_bytes());
		}
	}
	write_u16(output, index.fields.len(), "index field count")?;
	output.extend_from_slice(&0_u16.to_le_bytes());
	write_optional_binding(output, index.physical);
	write_string(output, index.name.as_str(), "index name")?;
	for field in &index.fields {
		output.extend_from_slice(&field.get().to_le_bytes());
	}
	Ok(())
}

fn decode_index(decoder: &mut Decoder<'_>) -> FerrosGrynnResult<ClassIndexCatalogEntry> {
	let id = IndexId::from_storage(decoder.read_u32()?)?;
	let version = SchemaVersion::from_storage(decoder.read_u32()?)?;
	let kind = decode_index_kind(decoder.read_u8()?)?;
	let state = decode_index_state(decoder.read_u8()?)?;
	let scope_kind = decoder.read_u8()?;
	decoder.expect_zero(1, "index scope reserved byte")?;
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
	decoder.expect_zero(2, "index entry reserved bytes")?;
	let physical = read_optional_binding(decoder)?;
	let name = read_string(decoder, "index name")?.into();
	let mut fields = Vec::with_capacity(field_count);
	for _ in 0..field_count {
		fields.push(PropertyKeyId::new(decoder.read_u32()?));
	}
	Ok(ClassIndexCatalogEntry {
		id,
		version,
		name,
		kind,
		state,
		scope,
		fields,
		physical,
	})
}

fn encode_graph(output: &mut Vec<u8>, graph: &GraphCatalogEntry) -> FerrosGrynnResult<()> {
	output.extend_from_slice(&graph.id.get().to_le_bytes());
	output.extend_from_slice(&graph.version.get().to_le_bytes());
	output.extend_from_slice(&graph.published_at.to_le_bytes());
	output.extend_from_slice(&graph.generation.to_le_bytes());
	output.push(type_codec::graph_kind(graph.kind));
	output.push(storage_mode(graph.storage_mode));
	output.extend_from_slice(&0_u16.to_le_bytes());
	output.extend_from_slice(&graph.label_index.0.get().to_le_bytes());
	output.extend_from_slice(&graph.label_index.1.get().to_le_bytes());
	output.extend_from_slice(&graph.adjacency_index.0.get().to_le_bytes());
	output.extend_from_slice(&graph.adjacency_index.1.get().to_le_bytes());
	write_binding(output, graph.membership);
	write_u32(output, graph.arena.graph_by_slot.len(), "graph arena slot count")?;
	write_string(output, graph.name.as_str(), "graph name")?;
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
	decoder.expect_zero(2, "graph entry reserved bytes")?;
	let label_index = (
		IndexId::from_storage(decoder.read_u32()?)?,
		SchemaVersion::from_storage(decoder.read_u32()?)?,
	);
	let adjacency_index = (
		IndexId::from_storage(decoder.read_u32()?)?,
		SchemaVersion::from_storage(decoder.read_u32()?)?,
	);
	let membership = read_binding(decoder)?;
	let slot_count = decoder.read_count("graph arena slot count")?;
	let name = read_string(decoder, "graph name")?.into();
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
	Ok(GraphCatalogEntry {
		id,
		version,
		published_at,
		generation,
		name,
		kind,
		storage_mode,
		label_index,
		adjacency_index,
		membership,
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
	write_string(output, parameter.name.as_str(), "parameter name")?;
	output.extend_from_slice(&parameter.value);
	Ok(())
}

fn decode_parameter(decoder: &mut Decoder<'_>) -> FerrosGrynnResult<ParameterCatalogEntry> {
	let id = SchemaId::from_storage(decoder.read_u32()?)?;
	let version = SchemaVersion::from_storage(decoder.read_u32()?)?;
	let published_at = decoder.read_u64()?;
	let value_len = decoder.read_count("parameter value length")?;
	let name = read_string(decoder, "parameter name")?.into();
	let value = decoder.read_exact(value_len)?.to_vec();
	Ok(ParameterCatalogEntry {
		id,
		version,
		published_at,
		name,
		value,
	})
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
	let raw_file = decoder.read_u64()?;
	let root = PageNo::new(decoder.read_u64()?);
	if raw_file == 0 && root == PageNo::NULL {
		return Ok(None);
	}
	let file_id = DataFileId::from_raw(raw_file)
		.ok_or_else(|| invalid("optional index binding contains an unknown file identity"))?;
	if file_id.kind() != Some(DataFileKind::Index) || root == PageNo::NULL {
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
	Ok(value.to_owned())
}

fn write_u16(output: &mut Vec<u8>, value: usize, field: &str) -> FerrosGrynnResult<()> {
	output.extend_from_slice(
		&u16::try_from(value)
			.map_err(|_error| invalid(format!("{field} exceeds u16")))?
			.to_le_bytes(),
	);
	Ok(())
}

fn write_u32(output: &mut Vec<u8>, value: usize, field: &str) -> FerrosGrynnResult<()> {
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
	}
}

fn decode_index_kind(value: u8) -> FerrosGrynnResult<ClassIndexKind> {
	match value {
		0 => Ok(ClassIndexKind::Standard),
		1 => Ok(ClassIndexKind::Unique),
		2 => Ok(ClassIndexKind::Count),
		_ => Err(invalid("index contains an unknown kind")),
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

pub(super) struct Decoder<'a> {
	bytes: &'a [u8],
	position: usize,
}

impl<'a> Decoder<'a> {
	fn new(bytes: &'a [u8]) -> Self {
		Self {
			bytes,
			position: 0,
		}
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

	fn read_count(&mut self, field: &str) -> FerrosGrynnResult<usize> {
		usize::try_from(self.read_u32()?)
			.map_err(|_error| invalid(format!("{field} does not fit usize")))
	}

	fn expect_zero(&mut self, count: usize, field: &str) -> FerrosGrynnResult<()> {
		if self.read_exact(count)?.iter().any(|byte| *byte != 0) {
			return Err(invalid(format!("{field} are non-zero")));
		}
		Ok(())
	}

	fn finish(self) -> FerrosGrynnResult<()> {
		if self.position != self.bytes.len() {
			return Err(invalid("database catalog contains trailing bytes"));
		}
		Ok(())
	}
}

pub(super) fn invalid(reason: impl Into<String>) -> FerrosGrynnError {
	FerrosGrynnError::invalid_storage_file(reason)
}
