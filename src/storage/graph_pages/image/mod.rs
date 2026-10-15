//! Complete immutable graph-occurrence image construction and reopen.

mod arena;
mod build;
mod builders;
mod decode_topology;
mod open;
mod readers;
mod topology;

#[cfg(test)]
/// Regression tests for image behavior.
mod tests;

use std::collections::BTreeMap;

pub(in crate::storage) use build::{build_graph_image, build_import_graph_image};
pub(in crate::storage) use open::open_graph_image;

use crate::{
	graph::{GraphId, GraphRef},
	storage::{
		graph_index::BuiltPrimaryIndex,
		graph_superblock::GraphSuperblock,
		page::AlignedPage,
	},
};

/// Runtime catalog bindings accompanying one graph image.
///
/// The durable form belongs to `FGDC`; the page image stores only compact
/// root-arena slots.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(in crate::storage) struct GraphArenaBindings {
	/// Graph by slot associated with this graph arena bindings.
	pub(in crate::storage) graph_by_slot: BTreeMap<u32, GraphId>,
	/// Scope by slot associated with this graph arena bindings.
	pub(in crate::storage) scope_by_slot: BTreeMap<u32, Box<[crate::graph::AtomId]>>,
}

/// Complete immutable graph-local generation used by WAL publication.
pub(in crate::storage) struct BuiltGraphImage<Membership = BuiltPrimaryIndex> {
	/// Reference associated with this built graph image.
	pub(in crate::storage) reference: GraphRef,
	/// Superblock associated with this built graph image.
	pub(in crate::storage) superblock: GraphSuperblock,
	/// Nodes associated with this built graph image.
	pub(in crate::storage) nodes: Vec<AlignedPage>,
	/// Edges associated with this built graph image.
	pub(in crate::storage) edges: Vec<AlignedPage>,
	/// Adjacency associated with this built graph image.
	pub(in crate::storage) adjacency: Vec<AlignedPage>,
	/// Cross level adjacency associated with this built graph image.
	pub(in crate::storage) cross_level_adjacency: Vec<AlignedPage>,
	/// Overlays associated with this built graph image.
	pub(in crate::storage) overlays: Vec<AlignedPage>,
	/// Subgraphs associated with this built graph image.
	pub(in crate::storage) subgraphs: Vec<AlignedPage>,
	/// Labels associated with this built graph image.
	pub(in crate::storage) labels: Vec<AlignedPage>,
	/// Membership associated with this built graph image.
	pub(in crate::storage) membership: Membership,
	/// Arena associated with this built graph image.
	pub(in crate::storage) arena: GraphArenaBindings,
}
