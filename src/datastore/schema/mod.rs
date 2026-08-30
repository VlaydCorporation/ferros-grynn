//! Public table-schema definitions and graph-name constraints.

/// Table field definitions.
mod field;
/// Named-graph schema constraints.
mod graph;
/// Scalar index definitions.
mod index;
/// Table definitions.
mod table;

pub use field::TableField;
pub(crate) use graph::validate_graph_name;
pub use index::{TableIndex, TableIndexKind};
pub use table::{TableKind, TableSchema};
