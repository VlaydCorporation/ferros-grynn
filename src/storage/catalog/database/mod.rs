//! Exact `FGDC v14` logical model and codec.

/// Retained source policies and database origin for durable resume.
mod changefeed;
mod codec;
/// Versioned event definitions with bounded source descriptors.
mod event;
/// Authoritative explicit function source descriptor bytes.
mod function;
pub(in crate::storage) use codec::Decoder as SourceDecoder;
pub(in crate::storage) use event::{decode as decode_event_source, encode as encode_event_source};
pub(in crate::storage) use function::{
	decode_source as decode_function_source,
	encode_source as encode_function_source,
};
/// Checked graph-owned definition descriptors.
mod graph_index;
/// Checked references shared by source history and private LIVE history.
mod history_segment;
/// Canonical subscription recipes, options and confirmed watermarks.
mod live;

/// Checked reactive definitions, exact values and bounded fault summaries.
mod live_binding;
/// Shared concrete function versions and historical record source owners.
mod live_capture;
/// Capture epochs and private physical references for computed LIVE history.
mod live_history;
/// Checked independent JOIN scope identities and last-success name operands.
mod live_scope;
mod type_codec;
mod validate;

#[cfg(test)]
/// Regression tests for database behavior.
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
	/// Nfc case of analyzer filter catalog entry.
	Nfc,
	/// Nfkc case of analyzer filter catalog entry.
	Nfkc,
	/// Ascii case of analyzer filter catalog entry.
	Ascii,
	/// Lowercase case of analyzer filter catalog entry.
	Lowercase,
	/// Uppercase case of analyzer filter catalog entry.
	Uppercase,
	/// Edge ngram case of analyzer filter catalog entry.
	EdgeNgram {
		/// Min associated with this analyzer filter catalog entry.
		min: u16,
		/// Max associated with this analyzer filter catalog entry.
		max: u16,
	},
	/// Ngram case of analyzer filter catalog entry.
	Ngram {
		/// Min associated with this analyzer filter catalog entry.
		min: u16,
		/// Max associated with this analyzer filter catalog entry.
		max: u16,
	},
	/// Snowball case of analyzer filter catalog entry.
	Snowball(
		/// Snowball case of analyzer filter catalog entry.
		crate::datastore::AnalyzerLanguage,
	),
	/// Canonical `StorageValue::Object` bytes.
	Mapper(
		/// Canonical `StorageValue::Object` bytes.
		Vec<u8>,
	),
}

/// One durable text-analyzer definition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::storage) struct AnalyzerCatalogEntry {
	/// Identifier used to locate this value within its owning subsystem.
	pub(in crate::storage) id: AnalyzerId,
	/// Version associated with this analyzer catalog entry.
	pub(in crate::storage) version: SchemaVersion,
	/// Published at associated with this analyzer catalog entry.
	pub(in crate::storage) published_at: u64,
	/// Name associated with this analyzer catalog entry.
	pub(in crate::storage) name: Text,
	/// Tokenizers associated with this analyzer catalog entry.
	pub(in crate::storage) tokenizers: std::collections::BTreeSet<AnalyzerTokenizer>,
	/// Filters associated with this analyzer catalog entry.
	pub(in crate::storage) filters: Vec<AnalyzerFilterCatalogEntry>,
	/// Function associated with this analyzer catalog entry.
	pub(in crate::storage) function: Option<(FunctionId, SchemaVersion)>,
	/// Comment associated with this analyzer catalog entry.
	pub(in crate::storage) comment: Option<Text>,
}

/// Exact physical root of one immutable index generation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::storage) struct PhysicalIndexBinding {
	/// File id associated with this physical index binding.
	pub(in crate::storage) file_id: DataFileId,
	/// Root associated with this physical index binding.
	pub(in crate::storage) root: PageNo,
}

/// Database-global AtomStore generation and its non-singleton roots.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::storage) struct AtomStoreCatalogBinding {
	/// Generation identifying this version of the data.
	pub(in crate::storage) generation: u64,
	/// Primary associated with this atom store catalog binding.
	pub(in crate::storage) primary: PhysicalIndexBinding,
	/// Reverse membership associated with this atom store catalog binding.
	pub(in crate::storage) reverse_membership: PhysicalIndexBinding,
}

/// One versioned field belonging to a class definition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::storage) struct ClassFieldCatalogEntry {
	/// Identifier used to locate this value within its owning subsystem.
	pub(in crate::storage) id: SchemaId,
	/// Version associated with this class field catalog entry.
	pub(in crate::storage) version: SchemaVersion,
	/// Name associated with this class field catalog entry.
	pub(in crate::storage) name: Text,
	/// Value type associated with this class field catalog entry.
	pub(in crate::storage) value_type: Type,
	/// Property key associated with this class field catalog entry.
	pub(in crate::storage) property_key: PropertyKeyId,
}

/// One versioned base or effective class-index definition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::storage) struct ClassIndexCatalogEntry {
	/// Identifier used to locate this value within its owning subsystem.
	pub(in crate::storage) id: IndexId,
	/// Version associated with this class index catalog entry.
	pub(in crate::storage) version: SchemaVersion,
	/// Name associated with this class index catalog entry.
	pub(in crate::storage) name: Text,
	/// Kind associated with this class index catalog entry.
	pub(in crate::storage) kind: ClassIndexKind,
	/// State associated with this class index catalog entry.
	pub(in crate::storage) state: ClassIndexState,
	/// Maintenance associated with this class index catalog entry.
	pub(in crate::storage) maintenance: IndexMaintenanceMode,
	/// Build revision associated with this class index catalog entry.
	pub(in crate::storage) build_revision: u64,
	/// Scope associated with this class index catalog entry.
	pub(in crate::storage) scope: IndexScope,
	/// Fields associated with this class index catalog entry.
	pub(in crate::storage) fields: Vec<PropertyKeyId>,
	/// Options associated with this class index catalog entry.
	pub(in crate::storage) options: ClassIndexOptions,
	/// Physical associated with this class index catalog entry.
	pub(in crate::storage) physical: Option<PhysicalIndexBinding>,
	/// Delta associated with this class index catalog entry.
	pub(in crate::storage) delta: Option<PhysicalIndexBinding>,
}

/// One durable `RecordId` namespace definition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::storage) struct ClassCatalogEntry {
	/// Identifier used to locate this value within its owning subsystem.
	pub(in crate::storage) id: ClassId,
	/// Version associated with this class catalog entry.
	pub(in crate::storage) version: SchemaVersion,
	/// Published at associated with this class catalog entry.
	pub(in crate::storage) published_at: u64,
	/// Name associated with this class catalog entry.
	pub(in crate::storage) name: Text,
	/// Kind associated with this class catalog entry.
	pub(in crate::storage) kind: AtomKind,
	/// Mode associated with this class catalog entry.
	pub(in crate::storage) mode: ClassSchemaMode,
	/// Fields associated with this class catalog entry.
	pub(in crate::storage) fields: Vec<ClassFieldCatalogEntry>,
	/// Indexes associated with this class catalog entry.
	pub(in crate::storage) indexes: Vec<ClassIndexCatalogEntry>,
}

/// One named graph definition and its graph-local generation bindings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::storage) struct GraphCatalogEntry {
	/// Durable incidence dependency revision; unrelated COW/checkpoint does not
	/// change it.
	pub(in crate::storage) tensor_revision: u64,
	/// Identifier used to locate this value within its owning subsystem.
	pub(in crate::storage) id: GraphId,
	/// Version associated with this graph catalog entry.
	pub(in crate::storage) version: SchemaVersion,
	/// Published at associated with this graph catalog entry.
	pub(in crate::storage) published_at: u64,
	/// Generation identifying this version of the data.
	pub(in crate::storage) generation: u64,
	/// Name associated with this graph catalog entry.
	pub(in crate::storage) name: Text,
	/// Kind associated with this graph catalog entry.
	pub(in crate::storage) kind: GraphKind,
	/// Storage mode associated with this graph catalog entry.
	pub(in crate::storage) storage_mode: GraphStorageMode,
	/// Topology constraints associated with this graph catalog entry.
	pub(in crate::storage) topology_constraints: GraphTopologyConstraints,
	/// Label index associated with this graph catalog entry.
	pub(in crate::storage) label_index: (IndexId, SchemaVersion),
	/// Adjacency index associated with this graph catalog entry.
	pub(in crate::storage) adjacency_index: (IndexId, SchemaVersion),
	/// Optional engine capability and its snapshot-exact synchronous side-file.
	pub(in crate::storage) edge_endpoint_index: Option<(IndexId, SchemaVersion)>,
	/// Edge endpoint associated with this graph catalog entry.
	pub(in crate::storage) edge_endpoint: Option<PhysicalIndexBinding>,
	/// User snapshot definitions in stable identity order.
	pub(in crate::storage) indexes: Vec<GraphIndex>,
	/// Exact immutable user artifact bindings, separate from public
	/// definitions.
	pub(in crate::storage) index_bindings:
		std::collections::BTreeMap<IndexId, PhysicalIndexBinding>,
	/// Membership associated with this graph catalog entry.
	pub(in crate::storage) membership: PhysicalIndexBinding,
	/// Owner root associated with this graph catalog entry.
	pub(in crate::storage) owner_root: Option<GraphId>,
	/// Root associated with this graph catalog entry.
	pub(in crate::storage) root: Option<Vec<u8>>,
	/// Properties associated with this graph catalog entry.
	pub(in crate::storage) properties: Vec<GraphPropertyCatalogEntry>,
	/// Arena associated with this graph catalog entry.
	pub(in crate::storage) arena: GraphArenaBindings,
}

/// One catalog-owned graph property encoded through the canonical value codec.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::storage) struct GraphPropertyCatalogEntry {
	/// Key associated with this graph property catalog entry.
	pub(in crate::storage) key: PropertyKeyId,
	/// Value associated with this graph property catalog entry.
	pub(in crate::storage) value: Vec<u8>,
}

/// One encoded global parameter; value bytes use the canonical value codec.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::storage) struct ParameterCatalogEntry {
	/// Identifier used to locate this value within its owning subsystem.
	pub(in crate::storage) id: SchemaId,
	/// Version associated with this parameter catalog entry.
	pub(in crate::storage) version: SchemaVersion,
	/// Published at associated with this parameter catalog entry.
	pub(in crate::storage) published_at: u64,
	/// Name associated with this parameter catalog entry.
	pub(in crate::storage) name: Text,
	/// Value associated with this parameter catalog entry.
	pub(in crate::storage) value: Vec<u8>,
	/// Comment associated with this parameter catalog entry.
	pub(in crate::storage) comment: Option<Text>,
}

/// One parameter in a durable named-function signature.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::storage) struct FunctionParameterCatalogEntry {
	/// Name associated with this function parameter catalog entry.
	pub(in crate::storage) name: Text,
	/// Value type associated with this function parameter catalog entry.
	pub(in crate::storage) value_type: Type,
}

/// One durable named-function overload. The body is persisted as UTF-8 source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::storage) struct FunctionCatalogEntry {
	/// Identifier used to locate this value within its owning subsystem.
	pub(in crate::storage) id: FunctionId,
	/// Version associated with this function catalog entry.
	pub(in crate::storage) version: SchemaVersion,
	/// Published at associated with this function catalog entry.
	pub(in crate::storage) published_at: u64,
	/// Namespace associated with this function catalog entry.
	pub(in crate::storage) namespace: Text,
	/// Name associated with this function catalog entry.
	pub(in crate::storage) name: Text,
	/// Parameters associated with this function catalog entry.
	pub(in crate::storage) parameters: Vec<FunctionParameterCatalogEntry>,
	/// Result associated with this function catalog entry.
	pub(in crate::storage) result: Type,
	/// Effect associated with this function catalog entry.
	pub(in crate::storage) effect: FunctionEffect,
	/// Cost associated with this function catalog entry.
	pub(in crate::storage) cost: bool,
	/// Max depth associated with this function catalog entry.
	pub(in crate::storage) max_depth: Option<u8>,
	/// Body associated with this function catalog entry.
	pub(in crate::storage) body: Text,
	/// Comment associated with this function catalog entry.
	pub(in crate::storage) comment: Option<Text>,
}

/// Canonical `FGDC v12` payload independent of physical catalog pages.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::storage) struct DatabaseCatalog {
	/// Independent correspondence heap and lookup-tree roots.
	pub(in crate::storage) bridges: Option<crate::storage::BridgeCatalogBinding>,
	/// Authoritative ordered checkpoint/delta segments for accepted
	/// invocations.
	pub(in crate::storage) event_outbox_segments: Vec<crate::storage::HistorySegment>,
	/// Published source-owned events in stable schema identity order.
	pub(in crate::storage) events: Vec<crate::datastore::EventDefinition>,
	/// Durable non-nil database origin.
	pub(in crate::storage) database_uuid: uuid::Uuid,
	/// Nondecreasing UTC publication time in epoch nanoseconds.
	pub(in crate::storage) committed_at_ns: i128,
	/// Retained source metadata independent of surviving owner schemas.
	pub(in crate::storage) changefeeds: Vec<ChangefeedCatalogEntry>,
	/// Reactive definitions survive consumer detach and generation replacement.
	pub(in crate::storage) live_queries: Vec<LiveCatalogEntry>,
	/// Independently versioned reactive parameters, including retained
	/// removals.
	pub(in crate::storage) live_bindings: Vec<LiveBindingCatalogEntry>,
	/// Private result history is not exposed by user SHOW CHANGES.
	pub(in crate::storage) live_history: Vec<LiveHistoryCatalogEntry>,
	/// Next graph id associated with this database catalog.
	pub(in crate::storage) next_graph_id: u64,
	/// Next schema id associated with this database catalog.
	pub(in crate::storage) next_schema_id: u64,
	/// Time zones associated with this database catalog.
	pub(in crate::storage) time_zones: Vec<Text>,
	/// Atom store associated with this database catalog.
	pub(in crate::storage) atom_store: AtomStoreCatalogBinding,
	/// Classes associated with this database catalog.
	pub(in crate::storage) classes: Vec<ClassCatalogEntry>,
	/// Graphs associated with this database catalog.
	pub(in crate::storage) graphs: Vec<GraphCatalogEntry>,
	/// Parameters associated with this database catalog.
	pub(in crate::storage) parameters: Vec<ParameterCatalogEntry>,
	/// Functions associated with this database catalog.
	pub(in crate::storage) functions: Vec<FunctionCatalogEntry>,
	/// Analyzers associated with this database catalog.
	pub(in crate::storage) analyzers: Vec<AnalyzerCatalogEntry>,
}

/// One private computed-history binding with independently trusted bounds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::storage) struct LiveHistoryCatalogEntry {
	/// Exact subscription incarnation, never a source alias.
	pub(in crate::storage) id: crate::live::LiveId,
	/// Final capture epochs and purge floor survive missing segment bytes.
	pub(in crate::storage) metadata: crate::datastore::LiveHistoryMetadata,
	/// Immutable snapshot ceilings in the common physical transport.
	pub(in crate::storage) segments: Vec<crate::storage::HistorySegment>,
}

/// One retained feed descriptor, independent of a surviving source schema.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::storage) struct ChangefeedCatalogEntry {
	/// Stable retained source incarnation.
	pub(in crate::storage) id: crate::changefeed::ChangefeedId,
	/// Stable owning class or graph identity.
	pub(in crate::storage) source: crate::changefeed::ChangefeedSource,
	/// Historical owner name.
	pub(in crate::storage) name: Text,
	/// Latest validated capture policy.
	pub(in crate::storage) config: crate::changefeed::ChangefeedConfig,
	/// Active, stopped, or retired owner state.
	pub(in crate::storage) state: crate::changefeed::ChangefeedState,
	/// Checked descriptor revision.
	pub(in crate::storage) version: SchemaVersion,
	/// Commit creating this retained incarnation.
	pub(in crate::storage) defined_at: u64,
	/// Commit publishing the latest capture policy.
	pub(in crate::storage) published_at: u64,
	/// Latest recorded history commit, or zero.
	pub(in crate::storage) last_stamp: u64,
	/// Durable unavailable-prefix floor, or zero.
	pub(in crate::storage) purged_through: u64,
	/// Trusted UTC time of the final removed commit, or zero before purging.
	pub(in crate::storage) purged_time_ns: i128,
	/// First live group page in the first retained segment, or zero.
	pub(in crate::storage) retained_page: u64,
	/// Expired groups preceding that page, or zero.
	pub(in crate::storage) retained_groups: u64,
	/// Commit stopping capture or removing its owner.
	pub(in crate::storage) closed_at: Option<u64>,
	/// UTC time closing this source; absent while active.
	pub(in crate::storage) closed_time_ns: Option<i128>,
	/// Historical policies sorted by inclusive start stamp.
	pub(in crate::storage) epochs: Vec<crate::changefeed::ChangefeedCaptureEpoch>,
	/// Sorted immutable segment references, including trusted stamp/time
	/// bounds.
	pub(in crate::storage) segments: Vec<crate::storage::HistorySegment>,
}

/// One restorable subscription incarnation in the same durable catalog.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::storage) struct LiveCatalogEntry {
	/// Query-owned graph checkpoint, independent of optional source capture.
	pub(in crate::storage) graph_checkpoint: Option<crate::storage::HistorySegment>,
	/// Stable incarnation retained independently of attached handles.
	pub(in crate::storage) id: crate::live::LiveId,
	/// Nonzero logical definition version.
	pub(in crate::storage) version: crate::live::LiveVersion,
	/// Published lifecycle; Pending is never encoded.
	pub(in crate::storage) state: crate::live::LiveState,
	/// Registration commit timestamp.
	pub(in crate::storage) defined_at: u64,
	/// Resolved exclusive replay and initial ACK floor.
	pub(in crate::storage) starting_after: u64,
	/// Latest lifecycle publication timestamp.
	pub(in crate::storage) published_at: u64,
	/// Confirmed whole-commit watermark.
	pub(in crate::storage) acknowledged: u64,
	/// Validated queue and execution policy.
	pub(in crate::storage) options: crate::live::LiveOptions,
	/// Owned logical query and fixed captures.
	pub(in crate::storage) query: std::sync::Arc<crate::live::LiveQuery>,
	/// Canonical descriptor validated by the bounded recovery reader.
	pub(in crate::storage) descriptor: std::sync::Arc<[u8]>,
	/// Captured class owner, including an original record namespace.
	pub(in crate::storage) source_class: Option<ClassId>,
	/// Last successful qualified capture owner; recovery never evaluates
	/// recipes.
	pub(in crate::storage) capture_source: crate::changefeed::ChangefeedSource,
	/// Durable string operand of a successfully resolved dynamic graph scope.
	pub(in crate::storage) capture_scope_name: Option<Text>,
	/// Last successful String scopes of independently owned root JOIN sources.
	pub(in crate::storage) rhs_scope_anchors: Vec<crate::datastore::LiveRhsScopeAnchor>,
	/// Captured PURE overload identities and definition versions.
	pub(in crate::storage) functions: Vec<(FunctionId, SchemaVersion)>,
	/// Historical class incarnations for nested exact record sources.
	pub(in crate::storage) record_sources: Vec<(Text, ClassId)>,
	/// Bounded original status code and static fault classification.
	pub(in crate::storage) diagnostic: Option<(Text, Text)>,
}

/// One independently restorable reactive binding in the common catalog.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::storage) struct LiveBindingCatalogEntry {
	/// Stable non-nil reactive identity.
	pub(in crate::storage) id: crate::live::LiveBindingId,
	/// Expression replacement version.
	pub(in crate::storage) version: crate::live::LiveVersion,
	/// Published lifecycle; Pending is never durable.
	pub(in crate::storage) state: crate::live::LiveState,
	/// First successful publication stamp.
	pub(in crate::storage) defined_at: u64,
	/// Latest definition/state publication stamp.
	pub(in crate::storage) published_at: u64,
	/// Latest successful exact value stamp.
	pub(in crate::storage) value_at: u64,
	/// Checked evaluation limits restored before decoding descriptors.
	pub(in crate::storage) options: crate::live::LiveBindingOptions,
	/// Logical expression reconstructed from canonical descriptor bytes.
	pub(in crate::storage) expression: std::sync::Arc<crate::live::LiveExpr>,
	/// Frozen ordinary input values, independent of primary records.
	pub(in crate::storage) parameters: std::sync::Arc<crate::values::Object>,
	/// Last successful exact value retained while paused.
	pub(in crate::storage) value: std::sync::Arc<crate::values::Value>,
	/// Canonical logical recipe with independent dictionaries.
	pub(in crate::storage) descriptor: std::sync::Arc<[u8]>,
	/// Canonical literal historical value with empty captures.
	pub(in crate::storage) value_descriptor: std::sync::Arc<[u8]>,
	/// Sorted distinct direct reactive identities.
	pub(in crate::storage) dependencies: Vec<crate::live::LiveBindingId>,
	/// Sorted unique PURE function identities with concrete versions.
	pub(in crate::storage) functions: Vec<(FunctionId, SchemaVersion)>,
	/// Canonical record source names retain their original class incarnation.
	pub(in crate::storage) record_sources: Vec<(Text, crate::datastore::ClassId)>,
	/// Bounded original status code and static classification; no technical
	/// sources.
	pub(in crate::storage) diagnostic: Option<(Text, Text)>,
}

impl DatabaseCatalog {
	/// Encodes the only supported database-catalog version.
	pub(in crate::storage) fn encode(&self) -> crate::gql_status::FerrosGrynnResult<Vec<u8>> {
		codec::encode(self)
	}

	/// Decodes and validates only exact `FGDC v12` bytes.
	pub(in crate::storage) fn decode(bytes: &[u8]) -> crate::gql_status::FerrosGrynnResult<Self> {
		codec::decode(bytes)
	}
}
