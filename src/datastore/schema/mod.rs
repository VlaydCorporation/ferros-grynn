//! Public versioned schema catalog and definition types.

/// Versioned catalog identities, graphs, parameters, and system indexes.
mod catalog;

/// Table field definitions.
mod field;
/// Named-graph schema constraints.
mod graph;
/// Scalar index definitions.
mod index;
/// Table definitions.
mod table;

pub(crate) use catalog::AtomTableMemberships;
pub use catalog::{
	GraphSchema,
	SchemaCatalog,
	SchemaId,
	SchemaParameter,
	SchemaVersion,
	SystemIndex,
	SystemIndexKind,
};
pub use field::TableField;
pub(crate) use graph::validate_graph_name;
pub use index::{TableIndex, TableIndexKind};
pub use table::{TableKind, TableSchema};
