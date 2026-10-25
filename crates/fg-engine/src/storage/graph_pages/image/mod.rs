//! Complete immutable graph-occurrence image construction and reopen.

mod arena;
/// Shared immutable canonical maps used by catalog and physical owners.
mod bindings;
mod build;
mod builders;
mod decode_topology;
mod open;
/// Borrowed route validation is independent of arena slot planning.
mod qualified;
mod readers;
mod topology;

#[cfg(test)]
/// Regression tests for image behavior.
mod tests;

pub(in crate::storage) use bindings::GraphArenaBindings;
pub(in crate::storage) use build::{build_graph_image, build_import_graph_image};
pub(in crate::storage) use open::open_graph_image;

use crate::{
	graph::GraphRef,
	storage::{
		graph_index::BuiltPrimaryIndex,
		graph_superblock::GraphSuperblock,
		page::AlignedPage,
	},
};

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
