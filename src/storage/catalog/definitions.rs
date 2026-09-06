#[allow(clippy::wildcard_imports, reason = "catalog layers use their facade format contract")]
use super::*;

/// Closed logical catalog stored in a [`PageType::SchemaRoot`] chain.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub(super) enum CatalogKind {
	/// Durable graph identities and creation metadata.
	Database = 0x01,
	/// Exact physical-file registrations.
	Files = 0x02,
}

impl CatalogKind {
	/// Returns the assigned format byte.
	pub(super) const fn get(self) -> u8 {
		self as u8
	}

	/// Decodes only assigned format-v1 kinds.
	pub(super) const fn from_raw(value: u8) -> Option<Self> {
		match value {
			value if value == Self::Database.get() => Some(Self::Database),
			value if value == Self::Files.get() => Some(Self::Files),
			_ => None,
		}
	}
}

/// Closed recursive type tags used by durable table schemas.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub(super) enum SchemaTypeTag {
	Any = 0x00,
	None = 0x01,
	Null = 0x02,
	Bool = 0x03,
	Number = 0x04,
	Int = 0x05,
	Float = 0x06,
	Decimal = 0x07,
	String = 0x08,
	Duration = 0x09,
	DateTime = 0x0a,
	Ulid = 0x0b,
	Uuid = 0x0c,
	Array = 0x0d,
	Set = 0x0e,
	Object = 0x0f,
	Geometry = 0x10,
	Vector = 0x11,
	Bytes = 0x12,
	Record = 0x13,
	Regex = 0x14,
	Range = 0x15,
	Atom = 0x16,
	Path = 0x17,
	Graph = 0x18,
	Option = 0x19,
	Union = 0x1a,
	RecordShape = 0x1b,
	Closure = 0x1c,
}

impl SchemaTypeTag {
	/// Returns the assigned format byte.
	pub(super) const fn get(self) -> u8 {
		self as u8
	}

	/// Decodes only assigned format-v1 schema tags.
	pub(super) const fn from_raw(value: u8) -> Option<Self> {
		match value {
			0x00 => Some(Self::Any),
			0x01 => Some(Self::None),
			0x02 => Some(Self::Null),
			0x03 => Some(Self::Bool),
			0x04 => Some(Self::Number),
			0x05 => Some(Self::Int),
			0x06 => Some(Self::Float),
			0x07 => Some(Self::Decimal),
			0x08 => Some(Self::String),
			0x09 => Some(Self::Duration),
			0x0a => Some(Self::DateTime),
			0x0b => Some(Self::Ulid),
			0x0c => Some(Self::Uuid),
			0x0d => Some(Self::Array),
			0x0e => Some(Self::Set),
			0x0f => Some(Self::Object),
			0x10 => Some(Self::Geometry),
			0x11 => Some(Self::Vector),
			0x12 => Some(Self::Bytes),
			0x13 => Some(Self::Record),
			0x14 => Some(Self::Regex),
			0x15 => Some(Self::Range),
			0x16 => Some(Self::Atom),
			0x17 => Some(Self::Path),
			0x18 => Some(Self::Graph),
			0x19 => Some(Self::Option),
			0x1a => Some(Self::Union),
			0x1b => Some(Self::RecordShape),
			0x1c => Some(Self::Closure),
			_ => None,
		}
	}
}

/// One durable graph-catalog entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::storage) struct GraphCatalogEntry {
	/// Storage address-space identity.
	pub(in crate::storage) graph_id: GraphId,
	/// Stable schema identity for a named root; zero for nested address spaces.
	pub(in crate::storage) schema_id: u32,
	/// Semantic graph-definition version; zero for nested address spaces.
	pub(in crate::storage) schema_version: u32,
	/// Engine-owned label-index definition for a named root.
	pub(in crate::storage) label_index: SystemIndexCatalogEntry,
	/// Engine-owned adjacency-index definition for a named root.
	pub(in crate::storage) adjacency_index: SystemIndexCatalogEntry,
	/// Owning graph for an internal nested graph; absent for named roots.
	pub(in crate::storage) parent_graph_id: Option<GraphId>,
	/// Root-arena subgraph slot used by hot pages and qualified endpoints.
	pub(in crate::storage) subgraph_slot: Option<u32>,
	/// Container node or edge in the parent graph.
	pub(in crate::storage) parent_atom: Option<crate::values::storage::DiskAtomRef>,
	/// External graph `RecordId` in its canonical display form.
	pub(in crate::storage) record_id: String,
	/// User-visible graph name.
	pub(in crate::storage) name: String,
	/// Runtime graph-family contract.
	pub(in crate::storage) kind: GraphKind,
	/// Commit marker that made the graph visible.
	pub(in crate::storage) created_lsn: Lsn,
	/// Stable runtime graph identity preserved across reopen.
	pub(in crate::storage) identity: [u8; 16],
	/// Last published runtime graph revision.
	pub(in crate::storage) revision: u64,
	/// Root page of the graph's immutable primary-index generation.
	pub(in crate::storage) primary_index_root: PageNo,
	/// Exact physical file containing the primary index.
	pub(in crate::storage) primary_index_file_id: DataFileId,
	/// Canonical graph-level property map.
	pub(in crate::storage) properties: Option<PropertyLocation>,
}

/// Durable identity and version of one engine-owned index capability.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::storage) struct SystemIndexCatalogEntry {
	/// Stable schema identity; zero only for an internal nested address space.
	pub(in crate::storage) schema_id: u32,
	/// Semantic definition version; zero only for an internal nested address
	/// space.
	pub(in crate::storage) schema_version: u32,
}

/// One durable table field using the database property-key namespace.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::storage) struct TableFieldCatalogEntry {
	/// Stable field-definition identity.
	pub(in crate::storage) schema_id: u32,
	/// Semantic field-definition version.
	pub(in crate::storage) schema_version: u32,
	/// User-visible field name.
	pub(in crate::storage) name: String,
	/// Runtime type contract.
	pub(in crate::storage) value_type: Type,
	/// Dense property-key identifier.
	pub(in crate::storage) property_key: u32,
}

/// One durable scalar B+tree definition and physical root.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::storage) struct TableIndexCatalogEntry {
	/// Stable index-definition identity.
	pub(in crate::storage) schema_id: u32,
	/// Semantic index-definition version.
	pub(in crate::storage) schema_version: u32,
	/// Table-local index name.
	pub(in crate::storage) name: String,
	/// Standard or unique leaf contract.
	pub(in crate::storage) kind: TableIndexKind,
	/// Composite fields as database property-key identifiers.
	pub(in crate::storage) fields: Vec<u32>,
	/// Exact physical index file.
	pub(in crate::storage) file_id: DataFileId,
	/// Root page of the immutable B+tree generation.
	pub(in crate::storage) root: PageNo,
}

/// One durable table schema.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::storage) struct TableCatalogEntry {
	/// Stable table-definition identity.
	pub(in crate::storage) schema_id: u32,
	/// Semantic table-definition version.
	pub(in crate::storage) schema_version: u32,
	/// Unique database table name.
	pub(in crate::storage) name: String,
	/// Owning durable graph identity.
	pub(in crate::storage) graph_id: GraphId,
	/// Node/relation applicability.
	pub(in crate::storage) kind: TableKind,
	/// Fields in query-visible order.
	pub(in crate::storage) fields: Vec<TableFieldCatalogEntry>,
	/// Scalar B+trees in strict name order.
	pub(in crate::storage) indexes: Vec<TableIndexCatalogEntry>,
	/// Engine-owned primary-index definition.
	pub(in crate::storage) primary_index: SystemIndexCatalogEntry,
}

/// One canonical global-parameter definition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::storage) struct ParameterCatalogEntry {
	/// Stable parameter-definition identity.
	pub(in crate::storage) schema_id: u32,
	/// Semantic parameter-definition version.
	pub(in crate::storage) schema_version: u32,
	/// Parameter name without the query-language `$` prefix.
	pub(in crate::storage) name: String,
	/// Canonical `StorageValue` bytes.
	pub(in crate::storage) value: Vec<u8>,
	/// RecordId table dictionary used by the canonical value.
	pub(in crate::storage) table_names: Vec<String>,
	/// IANA time-zone dictionary used by the canonical value.
	pub(in crate::storage) time_zones: Vec<String>,
}

/// Durable graph catalog owned by the system file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::storage) struct DatabaseCatalog {
	/// First never-assigned graph identity.
	pub(in crate::storage) next_graph_id: u64,
	/// First never-assigned schema identity.
	pub(in crate::storage) next_schema_id: u64,
	/// Entries in strictly increasing graph-id order.
	pub(in crate::storage) graphs: Vec<GraphCatalogEntry>,
	/// Database-wide property names in dense identifier order.
	pub(in crate::storage) property_keys: Vec<String>,
	/// Tables in strict name order.
	pub(in crate::storage) tables: Vec<TableCatalogEntry>,
	/// Global parameters in strict name order.
	pub(in crate::storage) parameters: Vec<ParameterCatalogEntry>,
}

impl DatabaseCatalog {
	/// Empty catalog used by a newly created database.
	pub(in crate::storage) const fn empty() -> Self {
		Self {
			next_graph_id: 1,
			next_schema_id: 1,
			graphs: Vec::new(),
			property_keys: Vec::new(),
			tables: Vec::new(),
			parameters: Vec::new(),
		}
	}

	/// Encodes the canonical database-catalog payload.
	pub(super) fn encode(&self) -> FerrosGrynnResult<Vec<u8>> {
		validate_graph_catalog(self)?;
		let graph_count = u32::try_from(self.graphs.len()).map_err(|_error| {
			invalid_catalog("database catalog contains more than u32::MAX graphs")
		})?;
		let property_count = u32::try_from(self.property_keys.len()).map_err(|_error| {
			invalid_catalog("database catalog contains more than u32::MAX properties")
		})?;
		let table_count = u32::try_from(self.tables.len()).map_err(|_error| {
			invalid_catalog("database catalog contains more than u32::MAX tables")
		})?;
		let parameter_count = u32::try_from(self.parameters.len()).map_err(|_error| {
			invalid_catalog("database catalog contains more than u32::MAX parameters")
		})?;
		let mut output = Vec::new();
		output.extend_from_slice(&DATABASE_CATALOG_MAGIC);
		output.extend_from_slice(&DATABASE_CATALOG_PAYLOAD_VERSION.to_le_bytes());
		output.extend_from_slice(&0_u16.to_le_bytes());
		output.extend_from_slice(&self.next_graph_id.to_le_bytes());
		output.extend_from_slice(&self.next_schema_id.to_le_bytes());
		output.extend_from_slice(&graph_count.to_le_bytes());
		output.extend_from_slice(&property_count.to_le_bytes());
		output.extend_from_slice(&table_count.to_le_bytes());
		output.extend_from_slice(&parameter_count.to_le_bytes());
		for property in &self.property_keys {
			write_catalog_string(&mut output, property, "property name")?;
		}
		for graph in &self.graphs {
			let record_len = u16::try_from(graph.record_id.len())
				.map_err(|_error| invalid_catalog("graph RecordId exceeds u16"))?;
			let name_len = u16::try_from(graph.name.len())
				.map_err(|_error| invalid_catalog("graph name exceeds u16"))?;
			output.extend_from_slice(&graph.graph_id.get().to_le_bytes());
			output.extend_from_slice(&graph.schema_id.to_le_bytes());
			output.extend_from_slice(&graph.schema_version.to_le_bytes());
			output.extend_from_slice(&graph.label_index.schema_id.to_le_bytes());
			output.extend_from_slice(&graph.label_index.schema_version.to_le_bytes());
			output.extend_from_slice(&graph.adjacency_index.schema_id.to_le_bytes());
			output.extend_from_slice(&graph.adjacency_index.schema_version.to_le_bytes());
			output.push(graph_kind_byte(graph.kind));
			output.extend_from_slice(&[0_u8; 7]);
			output.extend_from_slice(
				&graph.parent_graph_id.map_or(GraphId::SYSTEM.get(), GraphId::get).to_le_bytes(),
			);
			output.extend_from_slice(&graph.subgraph_slot.unwrap_or(u32::MAX).to_le_bytes());
			output.extend_from_slice(&0_u32.to_le_bytes());
			output.extend_from_slice(&graph.parent_atom.map_or(0, |atom| atom.raw()).to_le_bytes());
			output.extend_from_slice(&graph.created_lsn.get().to_le_bytes());
			output.extend_from_slice(&graph.identity);
			output.extend_from_slice(&graph.revision.to_le_bytes());
			output.extend_from_slice(&graph.primary_index_root.get().to_le_bytes());
			output.extend_from_slice(&graph.primary_index_file_id.get().to_le_bytes());
			encode_property_location(&mut output, graph.properties)?;
			output.extend_from_slice(&record_len.to_le_bytes());
			output.extend_from_slice(&name_len.to_le_bytes());
			output.extend_from_slice(&0_u32.to_le_bytes());
			output.extend_from_slice(graph.record_id.as_bytes());
			output.extend_from_slice(graph.name.as_bytes());
		}
		for table in &self.tables {
			write_catalog_string(&mut output, &table.name, "table name")?;
			output.extend_from_slice(&table.schema_id.to_le_bytes());
			output.extend_from_slice(&table.schema_version.to_le_bytes());
			output.extend_from_slice(&table.graph_id.get().to_le_bytes());
			output.push(table_kind_byte(table.kind));
			output.extend_from_slice(&[0_u8; 3]);
			output.extend_from_slice(&table.primary_index.schema_id.to_le_bytes());
			output.extend_from_slice(&table.primary_index.schema_version.to_le_bytes());
			output.extend_from_slice(
				&u32::try_from(table.fields.len())
					.map_err(|_error| invalid_catalog("table field count exceeds u32"))?
					.to_le_bytes(),
			);
			output.extend_from_slice(
				&u32::try_from(table.indexes.len())
					.map_err(|_error| invalid_catalog("table index count exceeds u32"))?
					.to_le_bytes(),
			);
			output.extend_from_slice(&0_u32.to_le_bytes());
			for field in &table.fields {
				write_catalog_string(&mut output, &field.name, "field name")?;
				output.extend_from_slice(&field.schema_id.to_le_bytes());
				output.extend_from_slice(&field.schema_version.to_le_bytes());
				output.extend_from_slice(&field.property_key.to_le_bytes());
				encode_type(&mut output, &field.value_type, 0)?;
			}
			for index in &table.indexes {
				write_catalog_string(&mut output, &index.name, "index name")?;
				output.extend_from_slice(&index.schema_id.to_le_bytes());
				output.extend_from_slice(&index.schema_version.to_le_bytes());
				output.push(table_index_kind_byte(index.kind));
				output.extend_from_slice(&[0_u8; 3]);
				output.extend_from_slice(
					&u32::try_from(index.fields.len())
						.map_err(|_error| invalid_catalog("index field count exceeds u32"))?
						.to_le_bytes(),
				);
				output.extend_from_slice(&index.file_id.get().to_le_bytes());
				output.extend_from_slice(&index.root.get().to_le_bytes());
				for field in &index.fields {
					output.extend_from_slice(&field.to_le_bytes());
				}
			}
		}
		for parameter in &self.parameters {
			write_catalog_string(&mut output, &parameter.name, "parameter name")?;
			output.extend_from_slice(&parameter.schema_id.to_le_bytes());
			output.extend_from_slice(&parameter.schema_version.to_le_bytes());
			output.extend_from_slice(
				&u32::try_from(parameter.value.len())
					.map_err(|_error| invalid_catalog("parameter value exceeds u32"))?
					.to_le_bytes(),
			);
			output.extend_from_slice(
				&u32::try_from(parameter.table_names.len())
					.map_err(|_error| invalid_catalog("parameter table dictionary exceeds u32"))?
					.to_le_bytes(),
			);
			output.extend_from_slice(
				&u32::try_from(parameter.time_zones.len())
					.map_err(|_error| {
						invalid_catalog("parameter time-zone dictionary exceeds u32")
					})?
					.to_le_bytes(),
			);
			output.extend_from_slice(&0_u32.to_le_bytes());
			for table in &parameter.table_names {
				write_catalog_string(&mut output, table, "parameter table name")?;
			}
			for zone in &parameter.time_zones {
				write_catalog_string(&mut output, zone, "parameter time-zone name")?;
			}
			output.extend_from_slice(&parameter.value);
		}
		validate_catalog_size(output.len())?;
		Ok(output)
	}

	/// Decodes a bounded canonical database-catalog payload.
	fn decode(bytes: &[u8]) -> FerrosGrynnResult<Self> {
		let mut decoder = CatalogDecoder::new(bytes);
		decoder.expect_magic(DATABASE_CATALOG_MAGIC, "database catalog")?;
		decoder.expect_version(DATABASE_CATALOG_PAYLOAD_VERSION)?;
		let next_graph_id = decoder.read_u64()?;
		let next_schema_id = decoder.read_u64()?;
		let graph_count = decoder.read_u32()?;
		let property_count = decoder.read_u32()?;
		let table_count = decoder.read_u32()?;
		let parameter_count = decoder.read_u32()?;
		let property_count = usize::try_from(property_count)
			.map_err(|_error| invalid_catalog("property count does not fit usize"))?;
		let mut property_keys = Vec::with_capacity(property_count);
		for _ in 0..property_count {
			property_keys.push(read_catalog_string(&mut decoder, "property name")?);
		}
		let count = usize::try_from(graph_count)
			.map_err(|_error| invalid_catalog("graph count does not fit usize"))?;
		let minimum = count
			.checked_mul(GRAPH_CATALOG_FIXED_BYTES)
			.and_then(|bytes| decoder.position.checked_add(bytes))
			.ok_or_else(|| invalid_catalog("database catalog entry area overflows usize"))?;
		if minimum > bytes.len() {
			return Err(invalid_catalog("database catalog count exceeds its payload"));
		}
		let mut graphs = Vec::with_capacity(count);
		for _ in 0..count {
			let graph_id = GraphId::new(decoder.read_u64()?);
			let schema_id = decoder.read_u32()?;
			let schema_version = decoder.read_u32()?;
			let label_index = SystemIndexCatalogEntry {
				schema_id: decoder.read_u32()?,
				schema_version: decoder.read_u32()?,
			};
			let adjacency_index = SystemIndexCatalogEntry {
				schema_id: decoder.read_u32()?,
				schema_version: decoder.read_u32()?,
			};
			let kind = decode_graph_kind(decoder.read_u8()?)?;
			decoder.expect_zero(7, "graph catalog entry padding")?;
			let raw_parent = GraphId::new(decoder.read_u64()?);
			let raw_subgraph_slot = decoder.read_u32()?;
			decoder.expect_zero(size_of::<u32>(), "graph parent reserved bytes")?;
			let raw_parent_atom = decoder.read_u64()?;
			let parent_graph_id = (raw_parent != GraphId::SYSTEM).then_some(raw_parent);
			let subgraph_slot = (raw_subgraph_slot != u32::MAX).then_some(raw_subgraph_slot);
			let parent_atom = if parent_graph_id.is_some() {
				Some(crate::values::storage::DiskAtomRef::from_raw(raw_parent_atom)?)
			} else {
				if raw_parent_atom != 0 {
					return Err(invalid_catalog("root graph contains a nested parent atom"));
				}
				None
			};
			let created_lsn = Lsn::new(decoder.read_u64()?);
			let identity: [u8; 16] = decoder.read_exact(16)?.try_into().map_err(|source| {
				invalid_catalog(format!("graph UUID width mismatch: {source}"))
			})?;
			let revision = decoder.read_u64()?;
			let primary_index_root = PageNo::new(decoder.read_u64()?);
			let primary_index_file_id =
				DataFileId::from_raw(decoder.read_u64()?).ok_or_else(|| {
					invalid_catalog("graph catalog contains an unknown primary-index file id")
				})?;
			let properties = decode_property_location(&mut decoder)?;
			let record_len = usize::from(decoder.read_u16()?);
			let name_len = usize::from(decoder.read_u16()?);
			decoder.expect_zero(size_of::<u32>(), "graph catalog entry reserved bytes")?;
			if record_len > MAX_GRAPH_RECORD_ID_BYTES || name_len > MAX_GRAPH_NAME_BYTES {
				return Err(invalid_catalog("graph catalog string exceeds its format limit"));
			}
			let record_id = decoder.read_utf8(record_len, "graph RecordId")?.to_owned();
			let name = decoder.read_utf8(name_len, "graph name")?.to_owned();
			graphs.push(GraphCatalogEntry {
				graph_id,
				schema_id,
				schema_version,
				label_index,
				adjacency_index,
				parent_graph_id,
				subgraph_slot,
				parent_atom,
				record_id,
				name,
				kind,
				created_lsn,
				identity,
				revision,
				primary_index_root,
				primary_index_file_id,
				properties,
			});
		}
		let table_count = usize::try_from(table_count)
			.map_err(|_error| invalid_catalog("table count does not fit usize"))?;
		let mut tables = Vec::with_capacity(table_count);
		for _ in 0..table_count {
			let name = read_catalog_string(&mut decoder, "table name")?;
			let schema_id = decoder.read_u32()?;
			let schema_version = decoder.read_u32()?;
			let graph_id = GraphId::new(decoder.read_u64()?);
			let kind = decode_table_kind(decoder.read_u8()?)?;
			decoder.expect_zero(3, "table catalog padding")?;
			let primary_index = SystemIndexCatalogEntry {
				schema_id: decoder.read_u32()?,
				schema_version: decoder.read_u32()?,
			};
			let field_count = usize::try_from(decoder.read_u32()?)
				.map_err(|_error| invalid_catalog("field count does not fit usize"))?;
			let index_count = usize::try_from(decoder.read_u32()?)
				.map_err(|_error| invalid_catalog("index count does not fit usize"))?;
			decoder.expect_zero(size_of::<u32>(), "table catalog reserved bytes")?;
			if field_count > MAX_TYPE_MEMBERS {
				return Err(invalid_catalog("table field count exceeds its format limit"));
			}
			let mut fields = Vec::with_capacity(field_count);
			for _ in 0..field_count {
				fields.push(TableFieldCatalogEntry {
					name: read_catalog_string(&mut decoder, "field name")?,
					schema_id: decoder.read_u32()?,
					schema_version: decoder.read_u32()?,
					property_key: decoder.read_u32()?,
					value_type: decode_type(&mut decoder, 0)?,
				});
			}
			if index_count > MAX_TYPE_MEMBERS {
				return Err(invalid_catalog("table index count exceeds its format limit"));
			}
			let mut indexes = Vec::with_capacity(index_count);
			for _ in 0..index_count {
				let name = read_catalog_string(&mut decoder, "index name")?;
				let schema_id = decoder.read_u32()?;
				let schema_version = decoder.read_u32()?;
				let kind = decode_table_index_kind(decoder.read_u8()?)?;
				decoder.expect_zero(3, "table index padding")?;
				let field_count = usize::try_from(decoder.read_u32()?)
					.map_err(|_error| invalid_catalog("index field count does not fit usize"))?;
				if field_count == 0 || field_count > MAX_TYPE_MEMBERS {
					return Err(invalid_catalog("index field count exceeds its format limit"));
				}
				let file_id = DataFileId::from_raw(decoder.read_u64()?)
					.ok_or_else(|| invalid_catalog("table index contains an unknown DataFileId"))?;
				let root = PageNo::new(decoder.read_u64()?);
				let mut index_fields = Vec::with_capacity(field_count);
				for _ in 0..field_count {
					index_fields.push(decoder.read_u32()?);
				}
				indexes.push(TableIndexCatalogEntry {
					name,
					schema_id,
					schema_version,
					kind,
					fields: index_fields,
					file_id,
					root,
				});
			}
			tables.push(TableCatalogEntry {
				name,
				schema_id,
				schema_version,
				graph_id,
				kind,
				fields,
				indexes,
				primary_index,
			});
		}
		let parameter_count = usize::try_from(parameter_count)
			.map_err(|_error| invalid_catalog("parameter count does not fit usize"))?;
		let mut parameters = Vec::with_capacity(parameter_count);
		for _ in 0..parameter_count {
			let name = read_catalog_string(&mut decoder, "parameter name")?;
			let schema_id = decoder.read_u32()?;
			let schema_version = decoder.read_u32()?;
			let value_len = usize::try_from(decoder.read_u32()?)
				.map_err(|_error| invalid_catalog("parameter value length does not fit usize"))?;
			let table_count = usize::try_from(decoder.read_u32()?)
				.map_err(|_error| invalid_catalog("parameter table count does not fit usize"))?;
			let zone_count = usize::try_from(decoder.read_u32()?)
				.map_err(|_error| invalid_catalog("parameter zone count does not fit usize"))?;
			decoder.expect_zero(size_of::<u32>(), "parameter reserved bytes")?;
			if table_count > MAX_TYPE_MEMBERS || zone_count > MAX_TYPE_MEMBERS {
				return Err(invalid_catalog("parameter dictionary exceeds its format limit"));
			}
			let mut table_names = Vec::with_capacity(table_count);
			for _ in 0..table_count {
				table_names.push(read_catalog_string(&mut decoder, "parameter table name")?);
			}
			let mut time_zones = Vec::with_capacity(zone_count);
			for _ in 0..zone_count {
				time_zones.push(read_catalog_string(&mut decoder, "parameter time-zone name")?);
			}
			parameters.push(ParameterCatalogEntry {
				schema_id,
				schema_version,
				name,
				value: decoder.read_exact(value_len)?.to_vec(),
				table_names,
				time_zones,
			});
		}
		decoder.finish()?;
		let catalog = Self {
			next_graph_id,
			next_schema_id,
			graphs,
			property_keys,
			tables,
			parameters,
		};
		validate_graph_catalog(&catalog)?;
		Ok(catalog)
	}
}

/// Validated relative path stored in the physical-file catalog.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(in crate::storage) struct CatalogPath(Box<str>);

impl CatalogPath {
	/// Parses a canonical slash-separated relative path.
	///
	/// # Errors
	///
	/// Returns an invalid-storage status for an empty, absolute, ambiguous, or
	/// traversal path.
	pub(in crate::storage) fn parse(value: &str) -> FerrosGrynnResult<Self> {
		if value.is_empty()
			|| value.len() > MAX_CATALOG_PATH_BYTES
			|| value.chars().any(|character| matches!(character, '\\' | ':' | '\0'))
			|| value.split('/').any(invalid_path_component)
		{
			return Err(invalid_catalog("file catalog contains a non-canonical relative path"));
		}
		let path = Path::new(value);
		if path.is_absolute()
			|| path.components().any(|component| !matches!(component, Component::Normal(_)))
		{
			return Err(invalid_catalog("file catalog path escapes the database directory"));
		}
		Ok(Self(value.into()))
	}

	/// Returns the canonical persisted path.
	pub(in crate::storage) fn as_str(&self) -> &str {
		&self.0
	}

	/// Resolves the already validated components below a trusted root.
	pub(in crate::storage) fn resolve(&self, root: &Path) -> PathBuf {
		self.0.split('/').fold(root.to_path_buf(), |path, component| path.join(component))
	}
}

/// One exact file-registry entry persisted by the system catalog.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::storage) struct FileCatalogEntry {
	/// Graph or system address space.
	pub(in crate::storage) graph_id: GraphId,
	/// Stable physical-file identity.
	pub(in crate::storage) file_id: DataFileId,
	/// Checkpoint ownership contract.
	pub(in crate::storage) role: DataFileRole,
	/// Validated relative database path.
	pub(in crate::storage) path: CatalogPath,
}

/// Durable sealed-registry description.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::storage) struct FileCatalog {
	/// First never-assigned 56-bit physical-file instance identity.
	pub(in crate::storage) next_file_instance_id: u64,
	/// Entries in strict `(GraphId, DataFileId)` order.
	pub(in crate::storage) files: Vec<FileCatalogEntry>,
}

impl FileCatalog {
	/// Bootstrap catalog containing only the system file itself.
	pub(in crate::storage) fn bootstrap() -> FerrosGrynnResult<Self> {
		Ok(Self {
			next_file_instance_id: 1,
			files: vec![FileCatalogEntry {
				graph_id: GraphId::SYSTEM,
				file_id: SYSTEM_CATALOG_FILE_ID,
				role: DataFileRole::WalBackfilled,
				path: CatalogPath::parse(SYSTEM_CATALOG_PATH)?,
			}],
		})
	}

	/// Encodes the canonical physical-file catalog payload.
	pub(super) fn encode(&self) -> FerrosGrynnResult<Vec<u8>> {
		validate_file_catalog(self)?;
		let count = u32::try_from(self.files.len())
			.map_err(|_error| invalid_catalog("file catalog contains more than u32::MAX files"))?;
		let mut output = Vec::new();
		output.extend_from_slice(&FILE_CATALOG_MAGIC);
		output.extend_from_slice(&FILE_CATALOG_PAYLOAD_VERSION.to_le_bytes());
		output.extend_from_slice(&0_u16.to_le_bytes());
		output.extend_from_slice(&count.to_le_bytes());
		output.extend_from_slice(&0_u32.to_le_bytes());
		output.extend_from_slice(&self.next_file_instance_id.to_le_bytes());
		for file in &self.files {
			let path_len = u16::try_from(file.path.as_str().len())
				.map_err(|_error| invalid_catalog("file catalog path exceeds u16"))?;
			output.extend_from_slice(&file.graph_id.get().to_le_bytes());
			output.extend_from_slice(&file.file_id.get().to_le_bytes());
			output.push(role_byte(file.role));
			output.extend_from_slice(&[0_u8; 7]);
			output.extend_from_slice(&path_len.to_le_bytes());
			output.extend_from_slice(&[0_u8; 6]);
			output.extend_from_slice(file.path.as_str().as_bytes());
		}
		validate_catalog_size(output.len())?;
		Ok(output)
	}

	/// Decodes a bounded canonical physical-file catalog payload.
	fn decode(bytes: &[u8]) -> FerrosGrynnResult<Self> {
		let mut decoder = CatalogDecoder::new(bytes);
		decoder.expect_magic(FILE_CATALOG_MAGIC, "file catalog")?;
		decoder.expect_version(FILE_CATALOG_PAYLOAD_VERSION)?;
		let count = decoder.read_u32()?;
		decoder.expect_zero(size_of::<u32>(), "file catalog reserved bytes")?;
		let next_file_instance_id = decoder.read_u64()?;
		let count = usize::try_from(count)
			.map_err(|_error| invalid_catalog("file count does not fit usize"))?;
		let minimum = count
			.checked_mul(32)
			.and_then(|bytes| decoder.position.checked_add(bytes))
			.ok_or_else(|| invalid_catalog("file catalog entry area overflows usize"))?;
		if minimum > bytes.len() {
			return Err(invalid_catalog("file catalog count exceeds its payload"));
		}
		let mut files = Vec::with_capacity(count);
		for _ in 0..count {
			let graph_id = GraphId::new(decoder.read_u64()?);
			let file_id = DataFileId::from_raw(decoder.read_u64()?)
				.ok_or_else(|| invalid_catalog("file catalog contains an unknown DataFileId"))?;
			let role = decode_role(decoder.read_u8()?)?;
			decoder.expect_zero(7, "file catalog role padding")?;
			let path_len = usize::from(decoder.read_u16()?);
			decoder.expect_zero(6, "file catalog path padding")?;
			if path_len > MAX_CATALOG_PATH_BYTES {
				return Err(invalid_catalog("file catalog path exceeds its format limit"));
			}
			let path = CatalogPath::parse(decoder.read_utf8(path_len, "file path")?)?;
			files.push(FileCatalogEntry {
				graph_id,
				file_id,
				role,
				path,
			});
		}
		decoder.finish()?;
		let catalog = Self {
			next_file_instance_id,
			files,
		};
		validate_file_catalog(&catalog)?;
		Ok(catalog)
	}

	/// Converts persisted trusted paths into registry bootstrap values.
	pub(in crate::storage) fn registrations(&self, root: &Path) -> Vec<DataFileRegistration> {
		self.files
			.iter()
			.map(|file| DataFileRegistration {
				graph_id: file.graph_id,
				file_id: file.file_id,
				role: file.role,
				path: file.path.resolve(root),
			})
			.collect()
	}
}

/// Encoded initial catalogs and their roots in the shared system file.
pub(in crate::storage) struct BootstrapCatalogPages {
	/// Database-catalog root persisted in `db.fgb`.
	pub(in crate::storage) database_root: PageNo,
	/// File-catalog root persisted in `db.fgb`.
	pub(in crate::storage) file_root: PageNo,
	/// Complete initialized system-file page extent.
	pub(in crate::storage) pages: Vec<AlignedPage>,
}

/// Copy-on-write catalog pages prepared for one transactional head switch.
pub(in crate::storage) struct CatalogUpdatePages {
	/// Database catalog root followed by fresh continuation pages.
	pub(in crate::storage) database_pages: Vec<(PageNo, AlignedPage)>,
	/// File catalog root followed by fresh continuation pages.
	pub(in crate::storage) file_pages: Vec<(PageNo, AlignedPage)>,
	/// File length required before the root frames may commit.
	pub(in crate::storage) required_page_count: u64,
}

/// Encodes both initial catalogs into one contiguous system-file extent.
pub(in crate::storage) fn encode_bootstrap_catalogs(
	database: &DatabaseCatalog,
	files: &FileCatalog,
) -> FerrosGrynnResult<BootstrapCatalogPages> {
	let database_payload = database.encode()?;
	let database_root = PageNo::new(0);
	let file_root = PageNo::new(1);
	let mut database_pages = encode_page_chain(
		CatalogKind::Database,
		database_root,
		PageNo::new(2),
		&database_payload,
		PageLsn::ZERO,
	)?;
	let file_tail = next_tail_page(PageNo::new(2), database_pages.len().saturating_sub(1))?;
	let file_payload = files.encode()?;
	let mut file_pages =
		encode_page_chain(CatalogKind::Files, file_root, file_tail, &file_payload, PageLsn::ZERO)?;
	database_pages.append(&mut file_pages);
	database_pages.sort_unstable_by_key(|(page_no, _page)| *page_no);
	let mut pages = Vec::with_capacity(database_pages.len());
	for (index, (page_no, page)) in database_pages.into_iter().enumerate() {
		let expected = u64::try_from(index)
			.map_err(|_error| invalid_catalog("bootstrap catalog page index exceeds u64"))?;
		if page_no.get() != expected {
			return Err(invalid_catalog("bootstrap catalog pages are not contiguous"));
		}
		pages.push(page);
	}
	Ok(BootstrapCatalogPages {
		database_root,
		file_root,
		pages,
	})
}

/// Builds immutable catalog tails and the two fixed transactional head pages.
///
/// Existing continuation pages are never overwritten. A crash before the WAL
/// head switch therefore leaves only unreachable, checksummed tail pages.
pub(in crate::storage) fn encode_catalog_update(
	database: &DatabaseCatalog,
	files: &FileCatalog,
	database_root: PageNo,
	file_root: PageNo,
	first_free_page: PageNo,
) -> FerrosGrynnResult<CatalogUpdatePages> {
	if database_root == PageNo::NULL
		|| file_root == PageNo::NULL
		|| database_root == file_root
		|| first_free_page <= database_root
		|| first_free_page <= file_root
	{
		return Err(invalid_catalog("catalog update roots or allocation boundary are invalid"));
	}
	let database_pages = encode_page_chain(
		CatalogKind::Database,
		database_root,
		first_free_page,
		&database.encode()?,
		PageLsn::ZERO,
	)?;
	let file_tail = next_tail_page(first_free_page, database_pages.len().saturating_sub(1))?;
	let file_pages = encode_page_chain(
		CatalogKind::Files,
		file_root,
		file_tail,
		&files.encode()?,
		PageLsn::ZERO,
	)?;
	let file_tail_count = file_pages.len().saturating_sub(1);
	let required_page_count = file_tail
		.get()
		.checked_add(
			u64::try_from(file_tail_count)
				.map_err(|_error| invalid_catalog("catalog tail page count exceeds u64"))?,
		)
		.ok_or_else(|| invalid_catalog("catalog file length overflows u64"))?;
	Ok(CatalogUpdatePages {
		database_pages,
		file_pages,
		required_page_count,
	})
}

/// Reads and validates both catalog chains from an opened system file.
pub(in crate::storage) fn read_catalogs(
	file: &DirectFile,
	database_root: PageNo,
	file_root: PageNo,
) -> FerrosGrynnResult<(DatabaseCatalog, FileCatalog)> {
	read_catalogs_with(
		|page_no, page| file.read_page(page_no, page).map(|_header| ()),
		database_root,
		file_root,
	)
}

/// Reads both catalog chains through a caller-selected WAL/home page source.
pub(in crate::storage) fn read_catalogs_with(
	mut read_page: impl FnMut(PageNo, &mut AlignedPage) -> FerrosGrynnResult<()>,
	database_root: PageNo,
	file_root: PageNo,
) -> FerrosGrynnResult<(DatabaseCatalog, FileCatalog)> {
	if database_root == PageNo::NULL || file_root == PageNo::NULL {
		return Err(invalid_catalog("database superblock does not reference durable catalogs"));
	}
	let database = decode_page_chain(&mut read_page, database_root, CatalogKind::Database)?;
	let files = decode_page_chain(&mut read_page, file_root, CatalogKind::Files)?;
	Ok((DatabaseCatalog::decode(&database)?, FileCatalog::decode(&files)?))
}
