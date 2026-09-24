//! WAL-backed publication and recovery for graph-local occurrence images.

mod bindings;
mod checkpoint;
mod durable;
mod reverse;

#[cfg(test)]
mod tests;

pub(super) use bindings::{GraphFileSet, ReverseMembershipFile};
pub(super) use checkpoint::{
	GraphCheckpointAuthority,
	backfill_graph_checkpoint,
	write_graph_checkpoint_page,
};
pub(super) use durable::{
	build_durable_graph_image,
	build_import_durable_graph_image,
	open_durable_graph_image,
	stage_durable_graph_image,
	stage_graph_superblock_if_changed,
};
pub(super) use reverse::{
	backfill_reverse_membership_checkpoint,
	build_reverse_membership_image,
	build_import_reverse_membership_image,
	open_reverse_membership_image,
	stage_reverse_membership_image,
	validate_reverse_memberships,
};
