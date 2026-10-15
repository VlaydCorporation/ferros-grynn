//! WAL-backed publication and recovery for graph-local occurrence images.

mod bindings;
mod checkpoint;
mod durable;
mod reverse;

#[cfg(test)]
mod tests;

pub(super) use bindings::{GraphFileSet, ReverseMembershipFile};
#[cfg(test)]
pub(super) use checkpoint::backfill_graph_checkpoint;
pub(super) use checkpoint::{GraphCheckpointAuthority, write_graph_checkpoint_page};
#[cfg(test)]
pub(super) use durable::stage_durable_graph_image;
pub(super) use durable::{
	build_durable_graph_image,
	build_import_durable_graph_image,
	open_durable_graph_image,
};
#[cfg(test)]
pub(super) use reverse::{backfill_reverse_membership_checkpoint, stage_reverse_membership_image};
pub(super) use reverse::{
	build_import_reverse_membership_image,
	build_reverse_membership_image,
	open_reverse_membership_image,
	validate_reverse_memberships,
};
