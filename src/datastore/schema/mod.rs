//! Public versioned class, graph, index, and parameter definitions.

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

pub use catalog::{
	ClassId,
	GraphSchema,
	IndexId,
	SchemaCatalog,
	SchemaId,
	SchemaParameter,
	SchemaVersion,
	SystemIndex,
	SystemIndexKind,
};
pub use class::{ClassSchema, ClassSchemaMode};
pub use field::ClassField;
pub use index::{ClassIndex, ClassIndexKind, ClassIndexState, IndexScope};
