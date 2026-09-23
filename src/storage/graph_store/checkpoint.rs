//! Checkpoint backfill and metadata COW for graph data spaces.

use super::GraphFileSet;
use crate::{
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	storage::{
		file_registry::FileRegistry,
		graph_superblock::GraphSuperblock,
		image_io::backfill_pages,
		metadata_cow::write_checkpoint_page,
		page::{AlignedPage, PageNo},
		types::{CheckpointGeneration, Lsn},
		wal::{CheckpointTicket, FrameWal},
	},
};

/// Database-authorized checkpoint tuple for one graph metadata page.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::storage) struct GraphCheckpointAuthority {
	/// Highest authorized checkpoint generation.
	pub(super) generation: CheckpointGeneration,
	/// Highest authorized home-file backfill boundary.
	pub(super) backfilled_lsn: Lsn,
	/// Matching durable checkpoint record boundary.
	pub(super) checkpoint_record_lsn: Lsn,
}

impl GraphCheckpointAuthority {
	/// Authority before the first durable database checkpoint.
	pub(super) const EMPTY: Self = Self {
		generation: CheckpointGeneration::ZERO,
		backfilled_lsn: Lsn::ZERO,
		checkpoint_record_lsn: Lsn::ZERO,
	};

	/// Creates authority from the database checkpoint commit point.
	pub(in crate::storage) const fn new(
		generation: CheckpointGeneration,
		backfilled_lsn: Lsn,
		checkpoint_record_lsn: Lsn,
	) -> Self {
		Self {
			generation,
			backfilled_lsn,
			checkpoint_record_lsn,
		}
	}

	/// Accepts a logical WAL version covered by database authority.
	pub(super) fn accepts_logical(self, value: GraphSuperblock) -> bool {
		value.checkpoint_generation <= self.generation
			&& value.backfilled_lsn <= self.backfilled_lsn
			&& value.checkpoint_record_lsn <= self.checkpoint_record_lsn
	}

	/// Accepts a COW home candidate produced by the declared checkpoint.
	pub(super) fn accepts_checkpoint(self, value: GraphSuperblock) -> bool {
		self.accepts_logical(value) && value.page_lsn.get() <= value.backfilled_lsn.get()
	}
}

/// Backfills graph data pages and returns the selected logical superblock.
pub(in crate::storage) fn backfill_graph_checkpoint(
	wal: &FrameWal,
	registry: &FileRegistry,
	files: GraphFileSet,
	ticket: &CheckpointTicket,
) -> FerrosGrynnResult<Option<GraphSuperblock>> {
	let regular_files = files.regular_files();
	let mut logical_superblock = None;
	for frame in ticket.frames() {
		let key = frame.key();
		if key.space_id() != files.space() {
			continue;
		}
		if key.file_id() == files.superblock {
			if key.page_no() != PageNo::new(0) || logical_superblock.is_some() {
				return Err(invalid("checkpoint selected an invalid GraphSuperblock frame"));
			}
			let mut page = AlignedPage::zeroed();
			wal.read_frame(*frame, &mut page)?;
			logical_superblock = Some(GraphSuperblock::decode(&page)?);
			continue;
		}
	}
	backfill_pages(wal, registry, files.space(), &regular_files, ticket)?;
	Ok(logical_superblock)
}

/// Publishes a checkpointed GraphSuperblock using metadata COW.
pub(in crate::storage) fn write_graph_checkpoint_page(
	registry: &FileRegistry,
	files: GraphFileSet,
	logical: GraphSuperblock,
	authority: GraphCheckpointAuthority,
) -> FerrosGrynnResult<GraphSuperblock> {
	if logical.graph_id != files.graph_id {
		return Err(invalid("checkpoint GraphSuperblock belongs to another graph"));
	}
	let checkpointed = logical.at_checkpoint(
		authority.generation,
		authority.backfilled_lsn,
		authority.checkpoint_record_lsn,
	)?;
	let path = registry.resolve_file(files.space(), files.superblock)?.path().to_path_buf();
	write_checkpoint_page(&path, &checkpointed.encode()?)?;
	Ok(checkpointed)
}

fn invalid(reason: impl Into<String>) -> FerrosGrynnError {
	FerrosGrynnError::invalid_storage_file(reason)
}
