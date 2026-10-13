//! Public versioned class, graph, index, and parameter definitions.

/// Durable text-analyzer definitions.
mod analyzer;
/// Catalog identities, graphs, parameters, and system indexes.
mod catalog;
/// Atom-class definitions.
mod class;
/// Durable class/graph event definitions and checked execution policies.
mod event;
/// Atomic event catalog installation and replay.
mod event_catalog;
/// Class field definitions.
mod field;
/// Graph-owned snapshot index definitions.
mod graph_index;
/// Class index definitions.
mod index;
/// Atomic replacement and removal of published schema definitions.
mod mutation;

pub use analyzer::{
	AnalyzerFilter,
	AnalyzerId,
	AnalyzerLanguage,
	AnalyzerTokenizer,
	SchemaAnalyzer,
};
pub use catalog::{
	ClassId,
	FunctionEffect,
	FunctionId,
	GraphSchema,
	IndexId,
	SchemaCatalog,
	SchemaFunction,
	SchemaFunctionParameter,
	SchemaId,
	SchemaParameter,
	SchemaVersion,
	SystemIndex,
	SystemIndexKind,
};
pub use class::{ClassSchema, ClassSchemaMode};
pub use event::{EventAsyncOptions, EventDefinition, EventExecution, EventId, EventTarget};
pub use field::ClassField;
pub use graph_index::{
	GraphIndex,
	GraphIndexCost,
	GraphIndexDefinition,
	NeighbourhoodAlgorithm,
	PathIndexAlgorithm,
	ReachabilityAlgorithm,
	TensorCacheFormat,
};
pub(crate) use index::ClassIndexOptions;
pub use index::{
	ClassIndex,
	ClassIndexKind,
	ClassIndexState,
	DiskAnnIndexOptions,
	FulltextIndexOptions,
	GeometryIndexOptions,
	HnswIndexOptions,
	IndexMaintenanceMode,
	IndexScope,
	VectorDistance,
};
