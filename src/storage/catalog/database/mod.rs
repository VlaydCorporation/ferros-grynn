//! Exact `FGDC v10` logical model and codec.

mod codec;
/// Checked graph-owned definition descriptors.
mod graph_index;
mod type_codec;
mod validate;

#[cfg(test)]
mod tests;

use crate::{
	datastore::{
		AnalyzerId,
		AnalyzerTokenizer,
		ClassId,
		ClassIndexKind,
		ClassIndexOptions,
		ClassIndexState,
		ClassSchemaMode,
		FunctionEffect,
		FunctionId,
		GraphIndex,
		IndexId,
		IndexMaintenanceMode,
		IndexScope,
		SchemaId,
		SchemaVersion,
	},
	graph::{AtomKind, GraphId, GraphKind, GraphStorageMode, GraphTopologyConstraints},
	storage::{graph_pages::GraphArenaBindings, page::PageNo, types::DataFileId},
	values::{PropertyKeyId, Text, Type},
};

/// One ordered analyzer filter in the durable catalog.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::storage) enum AnalyzerFilterCatalogEntry {
	Nfc,
	Nfkc,
	Ascii,
	Lowercase,
	Uppercase,
	EdgeNgram {
		min: u16,
		max: u16,
	},
	Ngram {
		min: u16,
		max: u16,
	},
	Snowball(crate::datastore::AnalyzerLanguage),
	/// Canonical `StorageValue::Object` bytes.
	Mapper(Vec<u8>),
}

/// One durable text-analyzer definition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::storage) struct AnalyzerCatalogEntry {
	pub(in crate::storage) id: AnalyzerId,
	pub(in crate::storage) version: SchemaVersion,
	pub(in crate::storage) published_at: u64,
	pub(in crate::storage) name: Text,
	pub(in crate::storage) tokenizers: std::collections::BTreeSet<AnalyzerTokenizer>,
	pub(in crate::storage) filters: Vec<AnalyzerFilterCatalogEntry>,
	pub(in crate::storage) function: Option<(FunctionId, SchemaVersion)>,
	pub(in crate::storage) comment: Option<Text>,
}

/// Exact physical root of one immutable index generation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::storage) struct PhysicalIndexBinding {
	pub(in crate::storage) file_id: DataFileId,
	pub(in crate::storage) root: PageNo,
}

/// Database-global AtomStore generation and its non-singleton roots.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::storage) struct AtomStoreCatalogBinding {
	pub(in crate::storage) generation: u64,
	pub(in crate::storage) primary: PhysicalIndexBinding,
	pub(in crate::storage) reverse_membership: PhysicalIndexBinding,
}

/// One versioned field belonging to a class definition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::storage) struct ClassFieldCatalogEntry {
	pub(in crate::storage) id: SchemaId,
	pub(in crate::storage) version: SchemaVersion,
	pub(in crate::storage) name: Text,
	pub(in crate::storage) value_type: Type,
	pub(in crate::storage) property_key: PropertyKeyId,
}

/// One versioned base or effective class-index definition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::storage) struct ClassIndexCatalogEntry {
	pub(in crate::storage) id: IndexId,
	pub(in crate::storage) version: SchemaVersion,
	pub(in crate::storage) name: Text,
	pub(in crate::storage) kind: ClassIndexKind,
	pub(in crate::storage) state: ClassIndexState,
	pub(in crate::storage) maintenance: IndexMaintenanceMode,
	pub(in crate::storage) build_revision: u64,
	pub(in crate::storage) scope: IndexScope,
	pub(in crate::storage) fields: Vec<PropertyKeyId>,
	pub(in crate::storage) options: ClassIndexOptions,
	pub(in crate::storage) physical: Option<PhysicalIndexBinding>,
	pub(in crate::storage) delta: Option<PhysicalIndexBinding>,
}

/// One durable `RecordId` namespace definition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::storage) struct ClassCatalogEntry {
	pub(in crate::storage) id: ClassId,
	pub(in crate::storage) version: SchemaVersion,
	pub(in crate::storage) published_at: u64,
	pub(in crate::storage) name: Text,
	pub(in crate::storage) kind: AtomKind,
	pub(in crate::storage) mode: ClassSchemaMode,
	pub(in crate::storage) fields: Vec<ClassFieldCatalogEntry>,
	pub(in crate::storage) indexes: Vec<ClassIndexCatalogEntry>,
}

/// One named graph definition and its graph-local generation bindings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::storage) struct GraphCatalogEntry {
	pub(in crate::storage) id: GraphId,
	pub(in crate::storage) version: SchemaVersion,
	pub(in crate::storage) published_at: u64,
	pub(in crate::storage) generation: u64,
	pub(in crate::storage) name: Text,
	pub(in crate::storage) kind: GraphKind,
	pub(in crate::storage) storage_mode: GraphStorageMode,
	pub(in crate::storage) topology_constraints: GraphTopologyConstraints,
	pub(in crate::storage) label_index: (IndexId, SchemaVersion),
	pub(in crate::storage) adjacency_index: (IndexId, SchemaVersion),
	/// Optional engine capability and its snapshot-exact synchronous side-file.
	pub(in crate::storage) edge_endpoint_index: Option<(IndexId, SchemaVersion)>,
	pub(in crate::storage) edge_endpoint: Option<PhysicalIndexBinding>,
	/// User snapshot definitions in stable identity order.
	pub(in crate::storage) indexes: Vec<GraphIndex>,
	/// Exact immutable user artifact bindings, separate from public
	/// definitions.
	pub(in crate::storage) index_bindings:
		std::collections::BTreeMap<IndexId, PhysicalIndexBinding>,
	pub(in crate::storage) membership: PhysicalIndexBinding,
	pub(in crate::storage) owner_root: Option<GraphId>,
	pub(in crate::storage) root: Option<Vec<u8>>,
	pub(in crate::storage) properties: Vec<GraphPropertyCatalogEntry>,
	pub(in crate::storage) arena: GraphArenaBindings,
}

/// One catalog-owned graph property encoded through the canonical value codec.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::storage) struct GraphPropertyCatalogEntry {
	pub(in crate::storage) key: PropertyKeyId,
	pub(in crate::storage) value: Vec<u8>,
}

/// One encoded global parameter; value bytes use the canonical value codec.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::storage) struct ParameterCatalogEntry {
	pub(in crate::storage) id: SchemaId,
	pub(in crate::storage) version: SchemaVersion,
	pub(in crate::storage) published_at: u64,
	pub(in crate::storage) name: Text,
	pub(in crate::storage) value: Vec<u8>,
	pub(in crate::storage) comment: Option<Text>,
}

/// One parameter in a durable named-function signature.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::storage) struct FunctionParameterCatalogEntry {
	pub(in crate::storage) name: Text,
	pub(in crate::storage) value_type: Type,
}

/// One durable named-function overload. The body is persisted as UTF-8 source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::storage) struct FunctionCatalogEntry {
	pub(in crate::storage) id: FunctionId,
	pub(in crate::storage) version: SchemaVersion,
	pub(in crate::storage) published_at: u64,
	pub(in crate::storage) namespace: Text,
	pub(in crate::storage) name: Text,
	pub(in crate::storage) parameters: Vec<FunctionParameterCatalogEntry>,
	pub(in crate::storage) result: Type,
	pub(in crate::storage) effect: FunctionEffect,
	pub(in crate::storage) cost: bool,
	pub(in crate::storage) max_depth: Option<u8>,
	pub(in crate::storage) body: Text,
	pub(in crate::storage) comment: Option<Text>,
}

/// Canonical `FGDC v10` payload independent of physical catalog pages.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::storage) struct DatabaseCatalog {
	pub(in crate::storage) next_graph_id: u64,
	pub(in crate::storage) next_schema_id: u64,
	pub(in crate::storage) time_zones: Vec<Text>,
	pub(in crate::storage) atom_store: AtomStoreCatalogBinding,
	pub(in crate::storage) classes: Vec<ClassCatalogEntry>,
	pub(in crate::storage) graphs: Vec<GraphCatalogEntry>,
	pub(in crate::storage) parameters: Vec<ParameterCatalogEntry>,
	pub(in crate::storage) functions: Vec<FunctionCatalogEntry>,
	pub(in crate::storage) analyzers: Vec<AnalyzerCatalogEntry>,
}

impl DatabaseCatalog {
	/// Encodes the only supported database-catalog version.
	pub(in crate::storage) fn encode(&self) -> crate::gql_status::FerrosGrynnResult<Vec<u8>> {
		codec::encode(self)
	}

	/// Decodes and validates only exact `FGDC v10` bytes.
	pub(in crate::storage) fn decode(bytes: &[u8]) -> crate::gql_status::FerrosGrynnResult<Self> {
		codec::decode(bytes)
	}
}
