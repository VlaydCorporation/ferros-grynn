//! Exact `FGDC v7` logical model and codec.

mod codec;
mod type_codec;
mod validate;

#[cfg(test)]
mod tests;

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
	graph::{AtomKind, GraphId, GraphKind, GraphStorageMode},
	storage::{graph_pages::GraphArenaBindings, page::PageNo, types::DataFileId},
	values::{PropertyKeyId, Text, Type},
};

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
	pub(in crate::storage) scope: IndexScope,
	pub(in crate::storage) fields: Vec<PropertyKeyId>,
	pub(in crate::storage) physical: Option<PhysicalIndexBinding>,
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
	pub(in crate::storage) label_index: (IndexId, SchemaVersion),
	pub(in crate::storage) adjacency_index: (IndexId, SchemaVersion),
	pub(in crate::storage) membership: PhysicalIndexBinding,
	pub(in crate::storage) arena: GraphArenaBindings,
}

/// One encoded global parameter; value bytes use the canonical value codec.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::storage) struct ParameterCatalogEntry {
	pub(in crate::storage) id: SchemaId,
	pub(in crate::storage) version: SchemaVersion,
	pub(in crate::storage) published_at: u64,
	pub(in crate::storage) name: Text,
	pub(in crate::storage) value: Vec<u8>,
}

/// Canonical `FGDC v7` payload independent of physical catalog pages.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::storage) struct DatabaseCatalog {
	pub(in crate::storage) next_graph_id: u64,
	pub(in crate::storage) next_schema_id: u64,
	pub(in crate::storage) time_zones: Vec<Text>,
	pub(in crate::storage) atom_store: AtomStoreCatalogBinding,
	pub(in crate::storage) classes: Vec<ClassCatalogEntry>,
	pub(in crate::storage) graphs: Vec<GraphCatalogEntry>,
	pub(in crate::storage) parameters: Vec<ParameterCatalogEntry>,
}

impl DatabaseCatalog {
	/// Encodes the only supported database-catalog version.
	pub(in crate::storage) fn encode(&self) -> crate::gql_status::FerrosGrynnResult<Vec<u8>> {
		codec::encode(self)
	}

	/// Decodes and validates only exact `FGDC v7` bytes.
	pub(in crate::storage) fn decode(bytes: &[u8]) -> crate::gql_status::FerrosGrynnResult<Self> {
		codec::decode(bytes)
	}
}
