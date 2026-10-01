//! Public versioned class, graph, index, and parameter definitions.

/// Durable text-analyzer definitions.
mod analyzer;
/// Catalog identities, graphs, parameters, and system indexes.
mod catalog;
/// Atom-class definitions.
mod class;
/// Class field definitions.
mod field;
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
pub use field::ClassField;
pub(crate) use index::ClassIndexOptions;
pub use index::{
	ClassIndex,
	ClassIndexKind,
	ClassIndexState,
	DiskAnnIndexOptions,
	FulltextIndexOptions,
	HnswIndexOptions,
	IndexMaintenanceMode,
	IndexScope,
	VectorDistance,
};
