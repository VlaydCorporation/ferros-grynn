//! Durable database and physical-file catalog pages.

use std::{
	collections::{BTreeMap, BTreeSet},
	path::{Component, Path, PathBuf},
};

use super::{
	direct::DirectFile,
	file_registry::{DataFileRegistration, DataFileRole},
	graph_pages::{LARGE_PROPERTY_SLOT, PropertyLocation},
	page::{AlignedPage, PAGE_HEADER_SIZE, PAGE_SIZE, PageFlags, PageLsn, PageNo, PageType},
	types::{DataFileId, DataFileKind, GraphId, Lsn},
};
use crate::{
	datastore::{TableIndexKind, TableKind},
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	values::runtime::{
		Type,
		VectorKind,
		graph::{AtomKind, GraphKind},
	},
};

/// Trusted relative path of the format-v1 system catalog.
pub(super) const SYSTEM_CATALOG_PATH: &str = "schema/catalog.fgb";
/// Stable singleton identity of the system catalog file.
pub(super) const SYSTEM_CATALOG_FILE_ID: DataFileId =
	DataFileId::for_kind(DataFileKind::SystemCatalog);

/// Catalog-page marker stored after the common page header.
const CATALOG_PAGE_MAGIC: [u8; 4] = *b"FGC1";
/// Catalog-page codec version.
const CATALOG_PAGE_VERSION: u8 = 1;
/// Catalog kind byte offset.
const CATALOG_KIND_OFFSET: usize = PAGE_HEADER_SIZE + CATALOG_PAGE_MAGIC.len();
/// Catalog codec-version byte offset.
const CATALOG_VERSION_OFFSET: usize = CATALOG_KIND_OFFSET + size_of::<u8>();
/// Reserved bytes after the catalog version.
const CATALOG_RESERVED_A_OFFSET: usize = CATALOG_VERSION_OFFSET + size_of::<u8>();
/// Next page in the same catalog chain.
const CATALOG_NEXT_PAGE_OFFSET: usize = CATALOG_RESERVED_A_OFFSET + size_of::<u16>();
/// Bytes used by this page's payload chunk.
const CATALOG_CHUNK_LEN_OFFSET: usize = CATALOG_NEXT_PAGE_OFFSET + size_of::<u64>();
/// Complete logical catalog length repeated on every page.
const CATALOG_TOTAL_LEN_OFFSET: usize = CATALOG_CHUNK_LEN_OFFSET + size_of::<u32>();
/// Reserved bytes before the payload chunk.
const CATALOG_RESERVED_B_OFFSET: usize = CATALOG_TOTAL_LEN_OFFSET + size_of::<u32>();
/// First byte of a catalog payload chunk.
const CATALOG_PAYLOAD_OFFSET: usize = CATALOG_RESERVED_B_OFFSET + size_of::<u64>();
/// Bytes available to one catalog payload chunk.
const CATALOG_PAYLOAD_BYTES: usize = PAGE_SIZE - CATALOG_PAYLOAD_OFFSET;
/// Maximum logical catalog accepted before allocation.
const MAX_CATALOG_BYTES: usize = 16 * 1024 * 1024;

/// Database-catalog payload marker.
const DATABASE_CATALOG_MAGIC: [u8; 4] = *b"FGDC";
/// File-catalog payload marker.
const FILE_CATALOG_MAGIC: [u8; 4] = *b"FGFC";
/// Payload codec version shared by both catalog values.
const CATALOG_PAYLOAD_VERSION: u16 = 3;
/// Maximum persisted graph name in UTF-8 bytes.
const MAX_GRAPH_NAME_BYTES: usize = 128;
/// Maximum canonical `RecordId` text retained for a graph identity.
const MAX_GRAPH_RECORD_ID_BYTES: usize = 4096;
/// Bytes before the two variable UTF-8 fields in one graph entry.
const GRAPH_CATALOG_FIXED_BYTES: usize = 112;
/// Maximum trusted relative file path in UTF-8 bytes.
const MAX_CATALOG_PATH_BYTES: usize = 1024;
/// Maximum one catalog-owned schema name in UTF-8 bytes.
const MAX_SCHEMA_NAME_BYTES: usize = 1024;
/// Maximum recursive type nesting accepted from storage.
const MAX_TYPE_DEPTH: usize = 64;
/// Maximum number of members in one schema type node.
const MAX_TYPE_MEMBERS: usize = 4096;

/// Closed logical catalog stored in a [`PageType::SchemaRoot`] chain.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
enum CatalogKind {
	/// Durable graph identities and creation metadata.
	Database = 0x01,
	/// Exact physical-file registrations.
	Files = 0x02,
}

impl CatalogKind {
	/// Returns the assigned format byte.
	const fn get(self) -> u8 {
		self as u8
	}

	/// Decodes only assigned format-v1 kinds.
	const fn from_raw(value: u8) -> Option<Self> {
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
enum SchemaTypeTag {
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
	const fn get(self) -> u8 {
		self as u8
	}

	/// Decodes only assigned format-v1 schema tags.
	const fn from_raw(value: u8) -> Option<Self> {
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
pub(super) struct GraphCatalogEntry {
	/// Storage address-space identity.
	pub(super) graph_id: GraphId,
	/// Owning graph for an internal nested graph; absent for named roots.
	pub(super) parent_graph_id: Option<GraphId>,
	/// Root-arena subgraph slot used by hot pages and qualified endpoints.
	pub(super) subgraph_slot: Option<u32>,
	/// Container node or edge in the parent graph.
	pub(super) parent_atom: Option<crate::values::storage::DiskAtomRef>,
	/// External graph `RecordId` in its canonical display form.
	pub(super) record_id: String,
	/// User-visible graph name.
	pub(super) name: String,
	/// Runtime graph-family contract.
	pub(super) kind: GraphKind,
	/// Commit marker that made the graph visible.
	pub(super) created_lsn: Lsn,
	/// Stable runtime graph identity preserved across reopen.
	pub(super) identity: [u8; 16],
	/// Last published runtime graph revision.
	pub(super) revision: u64,
	/// Root page of the graph's immutable primary-index generation.
	pub(super) primary_index_root: PageNo,
	/// Exact physical file containing the primary index.
	pub(super) primary_index_file_id: DataFileId,
	/// Canonical graph-level property map.
	pub(super) properties: Option<PropertyLocation>,
}

/// One durable table field using the database property-key namespace.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct TableFieldCatalogEntry {
	/// User-visible field name.
	pub(super) name: String,
	/// Runtime type contract.
	pub(super) value_type: Type,
	/// Dense property-key identifier.
	pub(super) property_key: u32,
}

/// One durable scalar B+tree definition and physical root.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct TableIndexCatalogEntry {
	/// Table-local index name.
	pub(super) name: String,
	/// Standard or unique leaf contract.
	pub(super) kind: TableIndexKind,
	/// Composite fields as database property-key identifiers.
	pub(super) fields: Vec<u32>,
	/// Exact physical index file.
	pub(super) file_id: DataFileId,
	/// Root page of the immutable B+tree generation.
	pub(super) root: PageNo,
}

/// One durable table schema.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct TableCatalogEntry {
	/// Unique database table name.
	pub(super) name: String,
	/// Owning durable graph identity.
	pub(super) graph_id: GraphId,
	/// Node/relation applicability.
	pub(super) kind: TableKind,
	/// Fields in query-visible order.
	pub(super) fields: Vec<TableFieldCatalogEntry>,
	/// Scalar B+trees in strict name order.
	pub(super) indexes: Vec<TableIndexCatalogEntry>,
}

/// Durable graph catalog owned by the system file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct DatabaseCatalog {
	/// First never-assigned graph identity.
	pub(super) next_graph_id: u64,
	/// Entries in strictly increasing graph-id order.
	pub(super) graphs: Vec<GraphCatalogEntry>,
	/// Database-wide property names in dense identifier order.
	pub(super) property_keys: Vec<String>,
	/// Tables in strict name order.
	pub(super) tables: Vec<TableCatalogEntry>,
}

impl DatabaseCatalog {
	/// Empty catalog used by a newly created database.
	pub(super) const fn empty() -> Self {
		Self {
			next_graph_id: 1,
			graphs: Vec::new(),
			property_keys: Vec::new(),
			tables: Vec::new(),
		}
	}

	/// Encodes the canonical database-catalog payload.
	fn encode(&self) -> FerrosGrynnResult<Vec<u8>> {
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
		let mut output = Vec::new();
		output.extend_from_slice(&DATABASE_CATALOG_MAGIC);
		output.extend_from_slice(&CATALOG_PAYLOAD_VERSION.to_le_bytes());
		output.extend_from_slice(&0_u16.to_le_bytes());
		output.extend_from_slice(&self.next_graph_id.to_le_bytes());
		output.extend_from_slice(&graph_count.to_le_bytes());
		output.extend_from_slice(&property_count.to_le_bytes());
		output.extend_from_slice(&table_count.to_le_bytes());
		output.extend_from_slice(&0_u32.to_le_bytes());
		for property in &self.property_keys {
			write_catalog_string(&mut output, property, "property name")?;
		}
		for graph in &self.graphs {
			let record_len = u16::try_from(graph.record_id.len())
				.map_err(|_error| invalid_catalog("graph RecordId exceeds u16"))?;
			let name_len = u16::try_from(graph.name.len())
				.map_err(|_error| invalid_catalog("graph name exceeds u16"))?;
			output.extend_from_slice(&graph.graph_id.get().to_le_bytes());
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
			output.extend_from_slice(&table.graph_id.get().to_le_bytes());
			output.push(table_kind_byte(table.kind));
			output.extend_from_slice(&[0_u8; 3]);
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
				output.extend_from_slice(&field.property_key.to_le_bytes());
				encode_type(&mut output, &field.value_type, 0)?;
			}
			for index in &table.indexes {
				write_catalog_string(&mut output, &index.name, "index name")?;
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
		validate_catalog_size(output.len())?;
		Ok(output)
	}

	/// Decodes a bounded canonical database-catalog payload.
	fn decode(bytes: &[u8]) -> FerrosGrynnResult<Self> {
		let mut decoder = CatalogDecoder::new(bytes);
		decoder.expect_magic(DATABASE_CATALOG_MAGIC, "database catalog")?;
		decoder.expect_version()?;
		let next_graph_id = decoder.read_u64()?;
		let graph_count = decoder.read_u32()?;
		let property_count = decoder.read_u32()?;
		let table_count = decoder.read_u32()?;
		decoder.expect_zero(size_of::<u32>(), "database catalog reserved bytes")?;
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
			let graph_id = GraphId::new(decoder.read_u64()?);
			let kind = decode_table_kind(decoder.read_u8()?)?;
			decoder.expect_zero(3, "table catalog padding")?;
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
					kind,
					fields: index_fields,
					file_id,
					root,
				});
			}
			tables.push(TableCatalogEntry {
				name,
				graph_id,
				kind,
				fields,
				indexes,
			});
		}
		decoder.finish()?;
		let catalog = Self {
			next_graph_id,
			graphs,
			property_keys,
			tables,
		};
		validate_graph_catalog(&catalog)?;
		Ok(catalog)
	}
}

/// Validated relative path stored in the physical-file catalog.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(super) struct CatalogPath(Box<str>);

impl CatalogPath {
	/// Parses a canonical slash-separated relative path.
	///
	/// # Errors
	///
	/// Returns an invalid-storage status for an empty, absolute, ambiguous, or
	/// traversal path.
	pub(super) fn parse(value: &str) -> FerrosGrynnResult<Self> {
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
	pub(super) fn as_str(&self) -> &str {
		&self.0
	}

	/// Resolves the already validated components below a trusted root.
	pub(super) fn resolve(&self, root: &Path) -> PathBuf {
		self.0.split('/').fold(root.to_path_buf(), |path, component| path.join(component))
	}
}

/// One exact file-registry entry persisted by the system catalog.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct FileCatalogEntry {
	/// Graph or system address space.
	pub(super) graph_id: GraphId,
	/// Stable physical-file identity.
	pub(super) file_id: DataFileId,
	/// Checkpoint ownership contract.
	pub(super) role: DataFileRole,
	/// Validated relative database path.
	pub(super) path: CatalogPath,
}

/// Durable sealed-registry description.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct FileCatalog {
	/// First never-assigned 56-bit physical-file instance identity.
	pub(super) next_file_instance_id: u64,
	/// Entries in strict `(GraphId, DataFileId)` order.
	pub(super) files: Vec<FileCatalogEntry>,
}

impl FileCatalog {
	/// Bootstrap catalog containing only the system file itself.
	pub(super) fn bootstrap() -> FerrosGrynnResult<Self> {
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
	fn encode(&self) -> FerrosGrynnResult<Vec<u8>> {
		validate_file_catalog(self)?;
		let count = u32::try_from(self.files.len())
			.map_err(|_error| invalid_catalog("file catalog contains more than u32::MAX files"))?;
		let mut output = Vec::new();
		output.extend_from_slice(&FILE_CATALOG_MAGIC);
		output.extend_from_slice(&CATALOG_PAYLOAD_VERSION.to_le_bytes());
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
		decoder.expect_version()?;
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
	pub(super) fn registrations(&self, root: &Path) -> Vec<DataFileRegistration> {
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
pub(super) struct BootstrapCatalogPages {
	/// Database-catalog root persisted in `db.fgb`.
	pub(super) database_root: PageNo,
	/// File-catalog root persisted in `db.fgb`.
	pub(super) file_root: PageNo,
	/// Complete initialized system-file page extent.
	pub(super) pages: Vec<AlignedPage>,
}

/// Copy-on-write catalog pages prepared for one transactional head switch.
pub(super) struct CatalogUpdatePages {
	/// Database catalog root followed by fresh continuation pages.
	pub(super) database_pages: Vec<(PageNo, AlignedPage)>,
	/// File catalog root followed by fresh continuation pages.
	pub(super) file_pages: Vec<(PageNo, AlignedPage)>,
	/// File length required before the root frames may commit.
	pub(super) required_page_count: u64,
}

/// Encodes both initial catalogs into one contiguous system-file extent.
pub(super) fn encode_bootstrap_catalogs(
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
pub(super) fn encode_catalog_update(
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
pub(super) fn read_catalogs(
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
pub(super) fn read_catalogs_with(
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

/// Builds a sealed checksummed page chain for one logical payload.
fn encode_page_chain(
	kind: CatalogKind,
	root: PageNo,
	continuation_start: PageNo,
	payload: &[u8],
	page_lsn: PageLsn,
) -> FerrosGrynnResult<Vec<(PageNo, AlignedPage)>> {
	validate_catalog_size(payload.len())?;
	let page_count = payload.len().div_ceil(CATALOG_PAYLOAD_BYTES).max(1);
	let total_len = u32::try_from(payload.len())
		.map_err(|_error| invalid_catalog("catalog payload exceeds u32"))?;
	let mut pages = Vec::with_capacity(page_count);
	for index in 0..page_count {
		let index_u64 = u64::try_from(index)
			.map_err(|_error| invalid_catalog("catalog page index exceeds u64"))?;
		let page_no = if index == 0 {
			root
		} else {
			continuation_start
				.get()
				.checked_add(index_u64 - 1)
				.map(PageNo::new)
				.ok_or_else(|| invalid_catalog("catalog page number overflows u64"))?
		};
		let next_page = if index + 1 == page_count {
			PageNo::NULL
		} else {
			continuation_start
				.get()
				.checked_add(index_u64)
				.map(PageNo::new)
				.ok_or_else(|| invalid_catalog("catalog next-page number overflows u64"))?
		};
		let start = index
			.checked_mul(CATALOG_PAYLOAD_BYTES)
			.ok_or_else(|| invalid_catalog("catalog chunk offset overflows usize"))?;
		let end = start.saturating_add(CATALOG_PAYLOAD_BYTES).min(payload.len());
		let chunk = &payload[start..end];
		let chunk_len = u32::try_from(chunk.len())
			.map_err(|_error| invalid_catalog("catalog chunk exceeds u32"))?;
		let mut page =
			AlignedPage::new(PageType::SchemaRoot, PageFlags::empty(), page_lsn, page_no);
		let bytes = page.as_bytes_mut();
		bytes[PAGE_HEADER_SIZE..CATALOG_KIND_OFFSET].copy_from_slice(&CATALOG_PAGE_MAGIC);
		bytes[CATALOG_KIND_OFFSET] = kind.get();
		bytes[CATALOG_VERSION_OFFSET] = CATALOG_PAGE_VERSION;
		bytes[CATALOG_NEXT_PAGE_OFFSET..CATALOG_CHUNK_LEN_OFFSET]
			.copy_from_slice(&next_page.get().to_le_bytes());
		bytes[CATALOG_CHUNK_LEN_OFFSET..CATALOG_TOTAL_LEN_OFFSET]
			.copy_from_slice(&chunk_len.to_le_bytes());
		bytes[CATALOG_TOTAL_LEN_OFFSET..CATALOG_RESERVED_B_OFFSET]
			.copy_from_slice(&total_len.to_le_bytes());
		bytes[CATALOG_PAYLOAD_OFFSET..CATALOG_PAYLOAD_OFFSET + chunk.len()].copy_from_slice(chunk);
		page.seal()?;
		pages.push((page_no, page));
	}
	Ok(pages)
}

/// Returns the first page after `count` consecutive continuation pages.
fn next_tail_page(first: PageNo, count: usize) -> FerrosGrynnResult<PageNo> {
	first
		.get()
		.checked_add(
			u64::try_from(count)
				.map_err(|_error| invalid_catalog("catalog page count exceeds u64"))?,
		)
		.map(PageNo::new)
		.ok_or_else(|| invalid_catalog("catalog continuation range overflows u64"))
}

/// Loads a bounded page chain without trusting persisted lengths or links.
fn decode_page_chain(
	read_page: &mut impl FnMut(PageNo, &mut AlignedPage) -> FerrosGrynnResult<()>,
	root: PageNo,
	expected_kind: CatalogKind,
) -> FerrosGrynnResult<Vec<u8>> {
	let mut page_no = root;
	let mut visited = BTreeSet::new();
	let mut output = Vec::new();
	let mut expected_total = None;
	loop {
		if page_no == PageNo::NULL || !visited.insert(page_no) {
			return Err(invalid_catalog("catalog page chain is null or cyclic"));
		}
		let mut page = AlignedPage::zeroed();
		read_page(page_no, &mut page)?;
		let header = page.validate(page_no)?;
		if header.page_type() != PageType::SchemaRoot || !header.flags().is_empty() {
			return Err(invalid_catalog("catalog chain contains an invalid page type or flags"));
		}
		let bytes = page.as_bytes();
		if bytes[PAGE_HEADER_SIZE..CATALOG_KIND_OFFSET] != CATALOG_PAGE_MAGIC
			|| CatalogKind::from_raw(bytes[CATALOG_KIND_OFFSET]) != Some(expected_kind)
			|| bytes[CATALOG_VERSION_OFFSET] != CATALOG_PAGE_VERSION
		{
			return Err(invalid_catalog("catalog page marker, kind, or version is invalid"));
		}
		validate_zero(
			&bytes[CATALOG_RESERVED_A_OFFSET..CATALOG_NEXT_PAGE_OFFSET],
			"catalog page reserved bytes",
		)?;
		validate_zero(
			&bytes[CATALOG_RESERVED_B_OFFSET..CATALOG_PAYLOAD_OFFSET],
			"catalog page reserved bytes",
		)?;
		let next_page = PageNo::new(read_u64(bytes, CATALOG_NEXT_PAGE_OFFSET));
		let chunk_len = usize::try_from(read_u32(bytes, CATALOG_CHUNK_LEN_OFFSET))
			.map_err(|_error| invalid_catalog("catalog chunk length does not fit usize"))?;
		let total_len = usize::try_from(read_u32(bytes, CATALOG_TOTAL_LEN_OFFSET))
			.map_err(|_error| invalid_catalog("catalog total length does not fit usize"))?;
		validate_catalog_size(total_len)?;
		if chunk_len > CATALOG_PAYLOAD_BYTES
			|| expected_total.is_some_and(|value| value != total_len)
		{
			return Err(invalid_catalog("catalog page contains inconsistent lengths"));
		}
		if expected_total.is_none() {
			output.reserve(total_len);
			expected_total = Some(total_len);
		}
		let remaining = total_len.saturating_sub(output.len());
		if chunk_len > remaining {
			return Err(invalid_catalog("catalog chunks exceed the declared total length"));
		}
		output
			.extend_from_slice(&bytes[CATALOG_PAYLOAD_OFFSET..CATALOG_PAYLOAD_OFFSET + chunk_len]);
		validate_zero(&bytes[CATALOG_PAYLOAD_OFFSET + chunk_len..], "catalog page trailing bytes")?;
		if next_page == PageNo::NULL {
			break;
		}
		if output.len() == total_len {
			return Err(invalid_catalog("catalog page chain continues after its declared payload"));
		}
		page_no = next_page;
	}
	if output.len() != expected_total.unwrap_or(0) {
		return Err(invalid_catalog("catalog page chain ended before its declared payload"));
	}
	Ok(output)
}

/// Appends one compact property location used by graph-catalog metadata.
fn encode_property_location(
	output: &mut Vec<u8>,
	location: Option<PropertyLocation>,
) -> FerrosGrynnResult<()> {
	let (page, slot, length) = match location {
		None => (PageNo::NULL, 0, 0),
		Some(PropertyLocation::Inline {
			page_no,
			slot,
			length,
		}) => (page_no, u32::from(slot), length),
		Some(PropertyLocation::Large {
			first_page,
			length,
		}) => (first_page, LARGE_PROPERTY_SLOT, length),
	};
	if (location.is_some() && (page == PageNo::NULL || length == 0))
		|| (location.is_none() && (page != PageNo::NULL || slot != 0 || length != 0))
	{
		return Err(invalid_catalog("graph property location is invalid"));
	}
	output.extend_from_slice(&page.get().to_le_bytes());
	output.extend_from_slice(&slot.to_le_bytes());
	output.extend_from_slice(&length.to_le_bytes());
	Ok(())
}

/// Reads one compact graph property location.
fn decode_property_location(
	decoder: &mut CatalogDecoder<'_>,
) -> FerrosGrynnResult<Option<PropertyLocation>> {
	let page = PageNo::new(decoder.read_u64()?);
	let slot = decoder.read_u32()?;
	let length = decoder.read_u32()?;
	if page == PageNo::NULL {
		return if slot == 0 && length == 0 {
			Ok(None)
		} else {
			Err(invalid_catalog("null graph property page has non-zero fields"))
		};
	}
	if length == 0 {
		return Err(invalid_catalog("graph property record has zero length"));
	}
	if slot == LARGE_PROPERTY_SLOT {
		Ok(Some(PropertyLocation::Large {
			first_page: page,
			length,
		}))
	} else {
		Ok(Some(PropertyLocation::Inline {
			page_no: page,
			slot: u16::try_from(slot)
				.map_err(|_error| invalid_catalog("graph property slot exceeds u16"))?,
			length,
		}))
	}
}

/// Appends one bounded UTF-8 schema string.
fn write_catalog_string(output: &mut Vec<u8>, value: &str, field: &str) -> FerrosGrynnResult<()> {
	if value.is_empty() || value.contains('\0') || value.len() > MAX_SCHEMA_NAME_BYTES {
		return Err(invalid_catalog(format!("{field} violates its format bound")));
	}
	let length = u16::try_from(value.len())
		.map_err(|_error| invalid_catalog(format!("{field} exceeds u16")))?;
	output.extend_from_slice(&length.to_le_bytes());
	output.extend_from_slice(value.as_bytes());
	Ok(())
}

/// Reads one bounded UTF-8 schema string.
fn read_catalog_string(decoder: &mut CatalogDecoder<'_>, field: &str) -> FerrosGrynnResult<String> {
	let length = usize::from(decoder.read_u16()?);
	if length == 0 || length > MAX_SCHEMA_NAME_BYTES {
		return Err(invalid_catalog(format!("{field} violates its format bound")));
	}
	let value = decoder.read_utf8(length, field)?;
	if value.contains('\0') {
		return Err(invalid_catalog(format!("{field} contains NUL")));
	}
	Ok(value.to_owned())
}

/// Encodes one recursive runtime type without a serialization dependency.
fn encode_type(output: &mut Vec<u8>, value: &Type, depth: usize) -> FerrosGrynnResult<()> {
	if depth > MAX_TYPE_DEPTH {
		return Err(invalid_catalog("table type nesting exceeds its format limit"));
	}
	let tag = match value {
		Type::Any => SchemaTypeTag::Any,
		Type::None => SchemaTypeTag::None,
		Type::Null => SchemaTypeTag::Null,
		Type::Bool => SchemaTypeTag::Bool,
		Type::Number => SchemaTypeTag::Number,
		Type::Int => SchemaTypeTag::Int,
		Type::Float => SchemaTypeTag::Float,
		Type::Decimal => SchemaTypeTag::Decimal,
		Type::String => SchemaTypeTag::String,
		Type::Duration => SchemaTypeTag::Duration,
		Type::DateTime => SchemaTypeTag::DateTime,
		Type::Ulid => SchemaTypeTag::Ulid,
		Type::Uuid => SchemaTypeTag::Uuid,
		Type::Array(_) => SchemaTypeTag::Array,
		Type::Set(_) => SchemaTypeTag::Set,
		Type::Object => SchemaTypeTag::Object,
		Type::Geometry => SchemaTypeTag::Geometry,
		Type::Vector(_) => SchemaTypeTag::Vector,
		Type::Bytes => SchemaTypeTag::Bytes,
		Type::Record => SchemaTypeTag::Record,
		Type::Regex => SchemaTypeTag::Regex,
		Type::Range(_) => SchemaTypeTag::Range,
		Type::Atom(_) => SchemaTypeTag::Atom,
		Type::Path => SchemaTypeTag::Path,
		Type::Graph(_) => SchemaTypeTag::Graph,
		Type::Option(_) => SchemaTypeTag::Option,
		Type::Union(_) => SchemaTypeTag::Union,
		Type::RecordShape(_) => SchemaTypeTag::RecordShape,
		Type::Closure {
			..
		} => SchemaTypeTag::Closure,
	};
	output.push(tag.get());
	match value {
		Type::Array(inner) | Type::Set(inner) | Type::Range(inner) | Type::Option(inner) => {
			encode_type(output, inner, depth + 1)?;
		}
		Type::Vector(kind) => output.push(vector_kind_byte(*kind)),
		Type::Atom(kind) => output.push(atom_kind_byte(*kind)),
		Type::Graph(kind) => output.push(graph_kind_byte(*kind)),
		Type::Union(values) => {
			write_type_member_count(output, values.len())?;
			for value in values {
				encode_type(output, value, depth + 1)?;
			}
		}
		Type::RecordShape(fields) => {
			write_type_member_count(output, fields.len())?;
			for (name, value) in fields {
				write_catalog_string(output, name.as_str(), "record-shape field")?;
				encode_type(output, value, depth + 1)?;
			}
		}
		Type::Closure {
			parameters,
			result,
		} => {
			write_type_member_count(output, parameters.len())?;
			for parameter in parameters {
				encode_type(output, parameter, depth + 1)?;
			}
			encode_type(output, result, depth + 1)?;
		}
		_ => {}
	}
	Ok(())
}

/// Decodes one recursive runtime type with bounds checked before allocation.
fn decode_type(decoder: &mut CatalogDecoder<'_>, depth: usize) -> FerrosGrynnResult<Type> {
	if depth > MAX_TYPE_DEPTH {
		return Err(invalid_catalog("table type nesting exceeds its format limit"));
	}
	let tag = SchemaTypeTag::from_raw(decoder.read_u8()?)
		.ok_or_else(|| invalid_catalog("table schema contains an unknown type tag"))?;
	Ok(match tag {
		SchemaTypeTag::Any => Type::Any,
		SchemaTypeTag::None => Type::None,
		SchemaTypeTag::Null => Type::Null,
		SchemaTypeTag::Bool => Type::Bool,
		SchemaTypeTag::Number => Type::Number,
		SchemaTypeTag::Int => Type::Int,
		SchemaTypeTag::Float => Type::Float,
		SchemaTypeTag::Decimal => Type::Decimal,
		SchemaTypeTag::String => Type::String,
		SchemaTypeTag::Duration => Type::Duration,
		SchemaTypeTag::DateTime => Type::DateTime,
		SchemaTypeTag::Ulid => Type::Ulid,
		SchemaTypeTag::Uuid => Type::Uuid,
		SchemaTypeTag::Array => Type::Array(Box::new(decode_type(decoder, depth + 1)?)),
		SchemaTypeTag::Set => Type::Set(Box::new(decode_type(decoder, depth + 1)?)),
		SchemaTypeTag::Object => Type::Object,
		SchemaTypeTag::Geometry => Type::Geometry,
		SchemaTypeTag::Vector => Type::Vector(decode_vector_kind(decoder.read_u8()?)?),
		SchemaTypeTag::Bytes => Type::Bytes,
		SchemaTypeTag::Record => Type::Record,
		SchemaTypeTag::Regex => Type::Regex,
		SchemaTypeTag::Range => Type::Range(Box::new(decode_type(decoder, depth + 1)?)),
		SchemaTypeTag::Atom => Type::Atom(decode_atom_kind(decoder.read_u8()?)?),
		SchemaTypeTag::Path => Type::Path,
		SchemaTypeTag::Graph => Type::Graph(decode_graph_kind(decoder.read_u8()?)?),
		SchemaTypeTag::Option => Type::Option(Box::new(decode_type(decoder, depth + 1)?)),
		SchemaTypeTag::Union => {
			let count = read_type_member_count(decoder)?;
			let mut values = Vec::with_capacity(count);
			for _ in 0..count {
				values.push(decode_type(decoder, depth + 1)?);
			}
			Type::Union(values)
		}
		SchemaTypeTag::RecordShape => {
			let count = read_type_member_count(decoder)?;
			let mut fields = Vec::with_capacity(count);
			for _ in 0..count {
				fields.push((
					read_catalog_string(decoder, "record-shape field")?.into(),
					decode_type(decoder, depth + 1)?,
				));
			}
			Type::RecordShape(fields)
		}
		SchemaTypeTag::Closure => {
			let count = read_type_member_count(decoder)?;
			let mut parameters = Vec::with_capacity(count);
			for _ in 0..count {
				parameters.push(decode_type(decoder, depth + 1)?);
			}
			Type::Closure {
				parameters,
				result: Box::new(decode_type(decoder, depth + 1)?),
			}
		}
	})
}

fn write_type_member_count(output: &mut Vec<u8>, count: usize) -> FerrosGrynnResult<()> {
	if count > MAX_TYPE_MEMBERS {
		return Err(invalid_catalog("table type member count exceeds its format limit"));
	}
	output.extend_from_slice(
		&u16::try_from(count)
			.map_err(|_error| invalid_catalog("table type member count exceeds u16"))?
			.to_le_bytes(),
	);
	Ok(())
}

fn read_type_member_count(decoder: &mut CatalogDecoder<'_>) -> FerrosGrynnResult<usize> {
	let count = usize::from(decoder.read_u16()?);
	if count > MAX_TYPE_MEMBERS {
		return Err(invalid_catalog("table type member count exceeds its format limit"));
	}
	Ok(count)
}

/// Validates ordering and graph-id clock invariants.
fn validate_graph_catalog(catalog: &DatabaseCatalog) -> FerrosGrynnResult<()> {
	if catalog.next_graph_id == 0 {
		return Err(invalid_catalog("graph identity zero is reserved for system metadata"));
	}
	let mut previous = GraphId::SYSTEM;
	let mut graph_ids = BTreeSet::new();
	let mut graph_roots = BTreeMap::new();
	let mut graph_records = BTreeSet::new();
	let mut graph_names = BTreeSet::new();
	let mut nested_slots = BTreeSet::new();
	for graph in &catalog.graphs {
		let root_graph_id = match (graph.parent_graph_id, graph.subgraph_slot, graph.parent_atom) {
			(None, None, None) => Some(graph.graph_id),
			(Some(parent), Some(slot), Some(_atom)) if slot != 0 && parent < graph.graph_id => {
				graph_roots.get(&parent).copied().filter(|root| nested_slots.insert((*root, slot)))
			}
			_ => None,
		}
		.ok_or_else(|| invalid_catalog("graph catalog entry violates a format-v1 bound"))?;
		if graph.graph_id == GraphId::SYSTEM || graph.graph_id <= previous {
			return Err(invalid_catalog("graph catalog entries are not in strict id order"));
		}
		if graph.graph_id.get() >= catalog.next_graph_id
			|| graph.record_id.is_empty()
			|| graph.record_id.len() > MAX_GRAPH_RECORD_ID_BYTES
			|| graph.name.is_empty()
			|| graph.name.len() > MAX_GRAPH_NAME_BYTES
			|| graph.name.contains('\0')
			|| !graph_ids.insert(graph.graph_id)
			|| !graph_records.insert(graph.record_id.as_str())
			|| (graph.parent_graph_id.is_none() && !graph_names.insert(graph.name.as_str()))
			|| graph.primary_index_file_id.kind() != Some(DataFileKind::Index)
			|| graph.primary_index_root == PageNo::NULL
		{
			return Err(invalid_catalog("graph catalog entry violates a format-v1 bound"));
		}
		graph_roots.insert(graph.graph_id, root_graph_id);
		previous = graph.graph_id;
	}
	let mut property_names = BTreeSet::new();
	for property in &catalog.property_keys {
		if property.is_empty()
			|| property.contains('\0')
			|| property.len() > MAX_SCHEMA_NAME_BYTES
			|| !property_names.insert(property.as_str())
		{
			return Err(invalid_catalog("property dictionary violates a format-v1 bound"));
		}
	}
	let mut previous_table = None;
	for table in &catalog.tables {
		if table.name.is_empty()
			|| table.name.contains('\0')
			|| table.name.len() > MAX_SCHEMA_NAME_BYTES
			|| previous_table.is_some_and(|previous: &str| previous >= table.name.as_str())
			|| !graph_ids.contains(&table.graph_id)
			|| table.fields.len() > MAX_TYPE_MEMBERS
			|| table.indexes.len() > MAX_TYPE_MEMBERS
		{
			return Err(invalid_catalog("table catalog entry violates a format-v1 bound"));
		}
		let mut field_names = BTreeSet::new();
		for field in &table.fields {
			let property_index = usize::try_from(field.property_key)
				.map_err(|_error| invalid_catalog("property key does not fit usize"))?;
			if field.name.is_empty()
				|| field.name.contains('\0')
				|| field.name.len() > MAX_SCHEMA_NAME_BYTES
				|| !field_names.insert(field.name.as_str())
				|| catalog.property_keys.get(property_index).map(String::as_str)
					!= Some(field.name.as_str())
			{
				return Err(invalid_catalog("table field violates its property-key contract"));
			}
		}
		let mut previous_index = None;
		for index in &table.indexes {
			if index.name.is_empty()
				|| index.name.contains('\0')
				|| index.name.len() > MAX_SCHEMA_NAME_BYTES
				|| previous_index.is_some_and(|previous: &str| previous >= index.name.as_str())
				|| index.fields.is_empty()
				|| index.fields.len() > MAX_TYPE_MEMBERS
				|| index.file_id.kind() != Some(DataFileKind::Index)
				|| index.root == PageNo::NULL
			{
				return Err(invalid_catalog("table index violates a format-v1 bound"));
			}
			let mut index_fields = BTreeSet::new();
			for property_key in &index.fields {
				if !index_fields.insert(*property_key)
					|| !table.fields.iter().any(|field| field.property_key == *property_key)
				{
					return Err(invalid_catalog(
						"table index references an unknown or duplicate field",
					));
				}
			}
			previous_index = Some(index.name.as_str());
		}
		previous_table = Some(table.name.as_str());
	}
	Ok(())
}

/// Validates ordering, roles, and the mandatory system-file registration.
fn validate_file_catalog(catalog: &FileCatalog) -> FerrosGrynnResult<()> {
	if catalog.next_file_instance_id == 0
		|| catalog.next_file_instance_id > DataFileId::EXHAUSTED_INSTANCE_ID
	{
		return Err(invalid_catalog("file instance clock is outside its 56-bit domain"));
	}
	let mut previous = None;
	let mut system_catalogs = 0_u8;
	for file in &catalog.files {
		let key = (file.graph_id, file.file_id);
		if previous.is_some_and(|previous| previous >= key) {
			return Err(invalid_catalog("file catalog entries are not in strict identity order"));
		}
		let graph_metadata = file.file_id.kind() == Some(DataFileKind::GraphSuperblock);
		if graph_metadata != (file.role == DataFileRole::GraphSuperblock) {
			return Err(invalid_catalog("file catalog role does not match DataFileKind"));
		}
		if file.file_id == SYSTEM_CATALOG_FILE_ID {
			if file.graph_id != GraphId::SYSTEM
				|| file.role != DataFileRole::WalBackfilled
				|| file.path.as_str() != SYSTEM_CATALOG_PATH
			{
				return Err(invalid_catalog("system catalog registration is not canonical"));
			}
			system_catalogs = system_catalogs.saturating_add(1);
		}
		previous = Some(key);
		if file.file_id.instance_id() >= catalog.next_file_instance_id {
			return Err(invalid_catalog("file catalog would reuse a physical-file identity"));
		}
	}
	if system_catalogs != 1 {
		return Err(invalid_catalog("file catalog must contain exactly one system catalog"));
	}
	Ok(())
}

/// Maps runtime graph families to stable catalog bytes.
const fn graph_kind_byte(kind: GraphKind) -> u8 {
	match kind {
		GraphKind::Graph => 0,
		GraphKind::Tree => 1,
		GraphKind::RootedTree => 2,
		GraphKind::Forest => 3,
		GraphKind::Dag => 4,
		GraphKind::Hypergraph => 5,
		GraphKind::Metagraph => 6,
		GraphKind::Subgraph => 7,
	}
}

/// Rejects catalog graph-family bytes not assigned by format v1.
fn decode_graph_kind(value: u8) -> FerrosGrynnResult<GraphKind> {
	match value {
		0 => Ok(GraphKind::Graph),
		1 => Ok(GraphKind::Tree),
		2 => Ok(GraphKind::RootedTree),
		3 => Ok(GraphKind::Forest),
		4 => Ok(GraphKind::Dag),
		5 => Ok(GraphKind::Hypergraph),
		6 => Ok(GraphKind::Metagraph),
		7 => Ok(GraphKind::Subgraph),
		_ => Err(invalid_catalog("graph catalog contains an unknown GraphKind")),
	}
}

/// Maps table applicability to stable catalog bytes.
const fn table_kind_byte(kind: TableKind) -> u8 {
	match kind {
		TableKind::Any => 0,
		TableKind::Normal => 1,
		TableKind::Relation => 2,
	}
}

/// Rejects unassigned table applicability bytes.
fn decode_table_kind(value: u8) -> FerrosGrynnResult<TableKind> {
	match value {
		0 => Ok(TableKind::Any),
		1 => Ok(TableKind::Normal),
		2 => Ok(TableKind::Relation),
		_ => Err(invalid_catalog("table catalog contains an unknown TableKind")),
	}
}

/// Maps scalar-index uniqueness to stable catalog bytes.
const fn table_index_kind_byte(kind: TableIndexKind) -> u8 {
	match kind {
		TableIndexKind::Standard => 0,
		TableIndexKind::Unique => 1,
	}
}

/// Rejects unassigned scalar-index kind bytes.
fn decode_table_index_kind(value: u8) -> FerrosGrynnResult<TableIndexKind> {
	match value {
		0 => Ok(TableIndexKind::Standard),
		1 => Ok(TableIndexKind::Unique),
		_ => Err(invalid_catalog("table catalog contains an unknown TableIndexKind")),
	}
}

/// Maps atom kinds to stable schema bytes.
const fn atom_kind_byte(kind: AtomKind) -> u8 {
	match kind {
		AtomKind::Node => 0,
		AtomKind::Edge => 1,
	}
}

/// Rejects unassigned atom-kind bytes.
fn decode_atom_kind(value: u8) -> FerrosGrynnResult<AtomKind> {
	match value {
		0 => Ok(AtomKind::Node),
		1 => Ok(AtomKind::Edge),
		_ => Err(invalid_catalog("table schema contains an unknown AtomKind")),
	}
}

/// Maps vector element kinds to stable schema bytes.
const fn vector_kind_byte(kind: VectorKind) -> u8 {
	match kind {
		VectorKind::F64 => 0,
		VectorKind::F32 => 1,
		VectorKind::I64 => 2,
		VectorKind::I32 => 3,
		VectorKind::I16 => 4,
	}
}

/// Rejects unassigned vector-kind bytes.
fn decode_vector_kind(value: u8) -> FerrosGrynnResult<VectorKind> {
	match value {
		0 => Ok(VectorKind::F64),
		1 => Ok(VectorKind::F32),
		2 => Ok(VectorKind::I64),
		3 => Ok(VectorKind::I32),
		4 => Ok(VectorKind::I16),
		_ => Err(invalid_catalog("table schema contains an unknown VectorKind")),
	}
}

/// Stable file-role byte.
const fn role_byte(role: DataFileRole) -> u8 {
	match role {
		DataFileRole::WalBackfilled => 0x01,
		DataFileRole::GraphSuperblock => 0x02,
	}
}

/// Rejects file-role bytes not assigned by format v1.
fn decode_role(value: u8) -> FerrosGrynnResult<DataFileRole> {
	match value {
		0x01 => Ok(DataFileRole::WalBackfilled),
		0x02 => Ok(DataFileRole::GraphSuperblock),
		_ => Err(invalid_catalog("file catalog contains an unknown role")),
	}
}

/// Returns whether one slash-separated component is ambiguous on Windows.
fn invalid_path_component(component: &str) -> bool {
	if component.is_empty()
		|| component == "."
		|| component == ".."
		|| component.ends_with(['.', ' '])
	{
		return true;
	}
	let stem = component.split('.').next().unwrap_or(component);
	let upper = stem.to_ascii_uppercase();
	matches!(upper.as_str(), "CON" | "PRN" | "AUX" | "NUL")
		|| (upper.len() == 4
			&& (upper.starts_with("COM") || upper.starts_with("LPT"))
			&& upper.as_bytes()[3].is_ascii_digit()
			&& upper.as_bytes()[3] != b'0')
}

/// Validates a catalog allocation bound before reserving memory.
fn validate_catalog_size(length: usize) -> FerrosGrynnResult<()> {
	if length > MAX_CATALOG_BYTES {
		return Err(FerrosGrynnError::storage_decode_limit_exceeded(
			"catalog bytes",
			MAX_CATALOG_BYTES as u64,
			length as u64,
		));
	}
	Ok(())
}

/// Rejects non-zero bytes in format-reserved regions.
fn validate_zero(bytes: &[u8], field: &str) -> FerrosGrynnResult<()> {
	if bytes.iter().any(|byte| *byte != 0) {
		return Err(invalid_catalog(format!("{field} are non-zero")));
	}
	Ok(())
}

/// Reads one little-endian `u32` from a checked fixed offset.
fn read_u32(bytes: &[u8], offset: usize) -> u32 {
	let mut value = [0_u8; size_of::<u32>()];
	value.copy_from_slice(&bytes[offset..offset + size_of::<u32>()]);
	u32::from_le_bytes(value)
}

/// Reads one little-endian `u64` from a checked fixed offset.
fn read_u64(bytes: &[u8], offset: usize) -> u64 {
	let mut value = [0_u8; size_of::<u64>()];
	value.copy_from_slice(&bytes[offset..offset + size_of::<u64>()]);
	u64::from_le_bytes(value)
}

/// Bounded cursor used by both catalog payload decoders.
struct CatalogDecoder<'a> {
	/// Untrusted logical payload.
	bytes: &'a [u8],
	/// Next unread byte.
	position: usize,
}

impl<'a> CatalogDecoder<'a> {
	/// Starts at the first logical payload byte.
	const fn new(bytes: &'a [u8]) -> Self {
		Self {
			bytes,
			position: 0,
		}
	}

	/// Validates one exact payload marker.
	fn expect_magic(&mut self, expected: [u8; 4], kind: &str) -> FerrosGrynnResult<()> {
		if self.read_exact(expected.len())? != expected {
			return Err(invalid_catalog(format!("invalid {kind} marker")));
		}
		Ok(())
	}

	/// Validates the shared version and reserved bytes.
	fn expect_version(&mut self) -> FerrosGrynnResult<()> {
		if self.read_u16()? != CATALOG_PAYLOAD_VERSION {
			return Err(invalid_catalog("unsupported catalog payload version"));
		}
		self.expect_zero(size_of::<u16>(), "catalog payload reserved bytes")
	}

	/// Reads one byte.
	fn read_u8(&mut self) -> FerrosGrynnResult<u8> {
		Ok(self.read_exact(1)?[0])
	}

	/// Reads one little-endian `u16`.
	fn read_u16(&mut self) -> FerrosGrynnResult<u16> {
		let bytes: [u8; 2] = self
			.read_exact(size_of::<u16>())?
			.try_into()
			.map_err(|source| invalid_catalog(format!("catalog u16 width mismatch: {source}")))?;
		Ok(u16::from_le_bytes(bytes))
	}

	/// Reads one little-endian `u32`.
	fn read_u32(&mut self) -> FerrosGrynnResult<u32> {
		let bytes: [u8; 4] = self
			.read_exact(size_of::<u32>())?
			.try_into()
			.map_err(|source| invalid_catalog(format!("catalog u32 width mismatch: {source}")))?;
		Ok(u32::from_le_bytes(bytes))
	}

	/// Reads one little-endian `u64`.
	fn read_u64(&mut self) -> FerrosGrynnResult<u64> {
		let bytes: [u8; 8] = self
			.read_exact(size_of::<u64>())?
			.try_into()
			.map_err(|source| invalid_catalog(format!("catalog u64 width mismatch: {source}")))?;
		Ok(u64::from_le_bytes(bytes))
	}

	/// Reads one bounded UTF-8 field.
	fn read_utf8(&mut self, length: usize, field: &str) -> FerrosGrynnResult<&'a str> {
		std::str::from_utf8(self.read_exact(length)?)
			.map_err(|source| invalid_catalog(format!("{field} is not UTF-8: {source}")))
	}

	/// Requires a fixed region to be canonical zero padding.
	fn expect_zero(&mut self, length: usize, field: &str) -> FerrosGrynnResult<()> {
		validate_zero(self.read_exact(length)?, field)
	}

	/// Returns one checked byte range without allocating.
	fn read_exact(&mut self, length: usize) -> FerrosGrynnResult<&'a [u8]> {
		let end = self
			.position
			.checked_add(length)
			.ok_or_else(|| invalid_catalog("catalog decoder offset overflows usize"))?;
		let bytes = self
			.bytes
			.get(self.position..end)
			.ok_or_else(|| invalid_catalog("catalog payload ends inside a field"))?;
		self.position = end;
		Ok(bytes)
	}

	/// Rejects trailing bytes after the declared entries.
	fn finish(self) -> FerrosGrynnResult<()> {
		if self.position != self.bytes.len() {
			return Err(invalid_catalog("catalog payload contains trailing bytes"));
		}
		Ok(())
	}
}

/// Creates the project-wide storage corruption error.
fn invalid_catalog(reason: impl Into<String>) -> FerrosGrynnError {
	FerrosGrynnError::invalid_storage_file(reason)
}

#[cfg(test)]
mod tests {
	use std::{
		fs,
		path::{Path, PathBuf},
		sync::atomic::{AtomicU64, Ordering},
	};

	use super::*;

	/// Unique suffix for catalog filesystem fixtures.
	static NEXT_DIRECTORY: AtomicU64 = AtomicU64::new(0);

	/// Exact temporary directory removed during unwind as well as success.
	struct TestDirectory(PathBuf);

	impl TestDirectory {
		/// Creates one empty fixture directory below the current workspace.
		fn new(label: &str) -> FerrosGrynnResult<Self> {
			let suffix = NEXT_DIRECTORY.fetch_add(1, Ordering::Relaxed);
			let path = std::env::current_dir()
				.map_err(|source| test_io("resolve catalog test directory", source))?
				.join(format!(".ferros-grynn-{label}-{}-{suffix}", std::process::id()));
			fs::create_dir(&path)
				.map_err(|source| test_io("create catalog test directory", source))?;
			Ok(Self(path))
		}

		/// Returns the exact fixture path.
		fn path(&self) -> &Path {
			&self.0
		}
	}

	impl Drop for TestDirectory {
		/// Best-effort cleanup owns only the uniquely created fixture
		/// directory.
		fn drop(&mut self) {
			drop(fs::remove_dir_all(&self.0));
		}
	}

	/// Preserves one native test I/O source behind the storage status boundary.
	fn test_io(operation: &str, source: std::io::Error) -> FerrosGrynnError {
		FerrosGrynnError::storage_io_failure(operation).with_technical_source(source)
	}

	/// Both logical catalogs round-trip through checksummed chained pages.
	#[test]
	fn bootstrap_catalogs_round_trip_through_direct_file() -> FerrosGrynnResult<()> {
		let directory = TestDirectory::new("catalog-roundtrip")?;
		let path = directory.path().join("catalog.fgb");
		let database = DatabaseCatalog {
			next_graph_id: 2,
			graphs: vec![GraphCatalogEntry {
				graph_id: GraphId::new(1),
				parent_graph_id: None,
				subgraph_slot: None,
				parent_atom: None,
				record_id: "graph:people".to_owned(),
				name: "people".to_owned(),
				kind: GraphKind::Graph,
				created_lsn: Lsn::new(3),
				identity: [7; 16],
				revision: 9,
				primary_index_root: PageNo::new(4),
				primary_index_file_id: DataFileId::for_kind(DataFileKind::Index),
				properties: Some(PropertyLocation::Inline {
					page_no: PageNo::new(2),
					slot: 1,
					length: 17,
				}),
			}],
			property_keys: vec!["name".to_owned(), "tags".to_owned()],
			tables: vec![TableCatalogEntry {
				name: "person".to_owned(),
				graph_id: GraphId::new(1),
				kind: TableKind::Normal,
				fields: vec![
					TableFieldCatalogEntry {
						name: "name".to_owned(),
						value_type: Type::String,
						property_key: 0,
					},
					TableFieldCatalogEntry {
						name: "tags".to_owned(),
						value_type: Type::Option(Box::new(Type::Array(Box::new(Type::String)))),
						property_key: 1,
					},
				],
				indexes: Vec::new(),
			}],
		};
		let files = FileCatalog::bootstrap()?;
		let encoded = encode_bootstrap_catalogs(&database, &files)?;
		let file = DirectFile::create_new(&path)?;
		file.write_pages(PageNo::new(0), &encoded.pages)?;
		file.sync_data()?;

		let actual = read_catalogs(&file, encoded.database_root, encoded.file_root)?;
		assert_eq!(actual, (database, files));
		Ok(())
	}

	/// Catalog paths reject every form that can escape or alias the DB root.
	#[test]
	fn catalog_path_rejects_traversal_and_windows_aliases() {
		for invalid in [
			"",
			"/absolute",
			"../escape",
			"schema/../escape",
			"schema\\catalog.fgb",
			"C:/outside",
			"schema/NUL.fgb",
			"schema/catalog.fgb.",
		] {
			assert!(CatalogPath::parse(invalid).is_err(), "accepted {invalid:?}");
		}
		assert_eq!(
			CatalogPath::parse(SYSTEM_CATALOG_PATH).ok().map(|path| path.as_str().to_owned()),
			Some(SYSTEM_CATALOG_PATH.to_owned())
		);
	}

	/// Decode validates reserved bytes after checksum validation.
	#[test]
	fn catalog_page_rejects_non_zero_reserved_bytes() -> FerrosGrynnResult<()> {
		let files = FileCatalog::bootstrap()?;
		let mut pages = encode_page_chain(
			CatalogKind::Files,
			PageNo::new(0),
			PageNo::new(1),
			&files.encode()?,
			PageLsn::ZERO,
		)?;
		pages[0].1.as_bytes_mut()[CATALOG_RESERVED_B_OFFSET] = 1;
		pages[0].1.seal()?;
		let directory = TestDirectory::new("catalog-reserved")?;
		let path = directory.path().join("catalog.fgb");
		let file = DirectFile::create_new(path)?;
		let pages = pages.into_iter().map(|(_page_no, page)| page).collect::<Vec<_>>();
		file.write_pages(PageNo::new(0), &pages)?;
		let mut read_page =
			|page_no, page: &mut AlignedPage| file.read_page(page_no, page).map(|_header| ());
		assert!(decode_page_chain(&mut read_page, PageNo::new(0), CatalogKind::Files).is_err());
		Ok(())
	}

	/// Retaining both over-aligned roots in their page-chain vectors preserves
	/// complete headers and checksums across the update boundary.
	#[test]
	fn catalog_update_root_container_preserves_aligned_pages() -> FerrosGrynnResult<()> {
		let mut database = DatabaseCatalog::empty();
		database.property_keys.push("name".to_owned());
		let files = FileCatalog::bootstrap()?;
		let update = encode_catalog_update(
			&database,
			&files,
			PageNo::new(0),
			PageNo::new(1),
			PageNo::new(2),
		)?;
		for pages in [&update.database_pages, &update.file_pages] {
			let (page_no, page) = pages
				.first()
				.ok_or_else(|| invalid_catalog("catalog update test root is absent"))?;
			assert_eq!(page.validate(*page_no)?.page_type(), PageType::SchemaRoot);
		}
		Ok(())
	}

	#[test]
	fn nested_catalog_reserves_local_slot_and_rejects_root_wide_collisions() -> FerrosGrynnResult<()>
	{
		let root = nested_graph_entry(1, None, None)?;
		let slot_zero = DatabaseCatalog {
			next_graph_id: 3,
			graphs: vec![root.clone(), nested_graph_entry(2, Some(1), Some(0))?],
			property_keys: Vec::new(),
			tables: Vec::new(),
		};
		assert!(slot_zero.encode().is_err());

		let duplicate = DatabaseCatalog {
			next_graph_id: 4,
			graphs: vec![
				root,
				nested_graph_entry(2, Some(1), Some(1))?,
				nested_graph_entry(3, Some(2), Some(1))?,
			],
			property_keys: Vec::new(),
			tables: Vec::new(),
		};
		assert!(duplicate.encode().is_err());
		Ok(())
	}

	fn nested_graph_entry(
		graph_id: u64,
		parent_graph_id: Option<u64>,
		subgraph_slot: Option<u32>,
	) -> FerrosGrynnResult<GraphCatalogEntry> {
		Ok(GraphCatalogEntry {
			graph_id: GraphId::new(graph_id),
			parent_graph_id: parent_graph_id.map(GraphId::new),
			subgraph_slot,
			parent_atom: parent_graph_id
				.map(|_| {
					crate::values::storage::DiskAtomRef::from_parts(
						crate::values::runtime::graph::AtomKind::Node,
						0,
					)
				})
				.transpose()?,
			record_id: format!("graph:{graph_id}"),
			name: format!("graph-{graph_id}"),
			kind: GraphKind::Graph,
			created_lsn: Lsn::new(1),
			identity: [0; 16],
			revision: 0,
			primary_index_root: PageNo::new(1),
			primary_index_file_id: DataFileId::for_kind(DataFileKind::Index),
			properties: None,
		})
	}
}
