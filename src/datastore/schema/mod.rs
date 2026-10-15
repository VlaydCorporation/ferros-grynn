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
pub use index::{
	ClassIndex,
	ClassIndexKind,
	ClassIndexOptions,
	ClassIndexState,
	DiskAnnIndexOptions,
	FulltextIndexOptions,
	GeometryIndexOptions,
	HnswIndexOptions,
	IndexMaintenanceMode,
	IndexScope,
	VectorDistance,
};

/// A requested change to one optional catalog or graph-local field.
///
/// The enclosing `Option<FieldUpdate<T>>` is absent when the field is
/// unchanged. A present update explicitly clears the field or replaces its
/// value.
#[derive(Clone, Copy)]
pub enum FieldUpdate<T> {
	/// Removes the current optional value.
	Clear,
	/// Replaces the current optional value with this owned payload.
	Set(
		/// Replaces the current optional value with this owned payload.
		T,
	),
}

impl<T> FieldUpdate<T> {
	/// Converts an explicitly supplied optional value into a clear or set
	/// update.
	pub(crate) fn from_optional(value: Option<T>) -> Self {
		match value {
			None => Self::Clear,
			Some(value) => Self::Set(value),
		}
	}

	/// Consumes a requested update into the optional value stored by the owner.
	pub(crate) fn into_optional(self) -> Option<T> {
		match self {
			Self::Clear => None,
			Self::Set(value) => Some(value),
		}
	}
}

impl<T: std::fmt::Debug> std::fmt::Debug for FieldUpdate<T> {
	/// Preserves the existing optional-payload representation in IR
	/// diagnostics.
	///
	/// # Errors
	///
	/// Returns an error reported by the supplied f.
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		match self {
			Self::Clear => std::fmt::Debug::fmt(&None::<&T>, f),
			Self::Set(value) => std::fmt::Debug::fmt(&Some(value), f),
		}
	}
}
