//! Database-wide passive/restart checkpoint publication.

use std::collections::{BTreeMap, BTreeSet};

use super::{
	LocalState,
	layout::{DATABASE_CATALOG_ROOT, FILE_CATALOG_ROOT, invalid},
};
use crate::{
	datastore::{CheckpointMode, DataSpaceId},
	gql_status::FerrosGrynnResult,
	storage::{
		atom_store_image::{AtomStoreCheckpointAuthority, write_atom_store_checkpoint_page},
		database_superblock::DatabaseSuperblock,
		file_registry::DataFileRole,
		graph_store::{GraphCheckpointAuthority, write_graph_checkpoint_page},
		image_io::backfill_pages,
		local::write_database_superblock,
		types::DataFileId,
		wal::FrameWal,
	},
};

pub(super) fn checkpoint(
	wal: &FrameWal,
	state: &mut LocalState,
	mode: CheckpointMode,
) -> FerrosGrynnResult<bool> {
	let target = match mode {
		CheckpointMode::Passive => wal.capture_checkpoint_target(),
		CheckpointMode::Restart => wal.capture_restart_checkpoint_target(),
	};
	let Some(target) = target else {
		return Ok(false);
	};
	let ticket = wal.begin_checkpoint(target, state.database_superblock.generation)?;
	let mut files = BTreeMap::<DataSpaceId, BTreeSet<DataFileId>>::new();
	for ((space, file_id), file) in state.registry.snapshot() {
		if file.role() == DataFileRole::WalBackfilled {
			files.entry(space).or_default().insert(file_id);
		}
	}
	for (space, files) in files {
		backfill_pages(
			wal,
			&state.registry,
			space,
			&files.into_iter().collect::<Vec<_>>(),
			&ticket,
		)?;
	}
	let committed = wal.finish_checkpoint(ticket)?;
	let authority = AtomStoreCheckpointAuthority::new(
		committed.generation(),
		committed.target(),
		committed.checkpoint_record_lsn(),
	);
	state.atom_superblock = write_atom_store_checkpoint_page(
		&state.registry,
		state.bindings.atoms,
		state.atom_superblock,
		authority,
	)?;
	let graph_authority = GraphCheckpointAuthority::new(
		committed.generation(),
		committed.target(),
		committed.checkpoint_record_lsn(),
	);
	for binding in &state.bindings.graphs {
		let logical = state
			.graph_superblocks
			.get(&binding.graph_id)
			.copied()
			.ok_or_else(|| invalid("checkpoint graph has no logical superblock"))?;
		let checkpointed =
			write_graph_checkpoint_page(&state.registry, binding.files, logical, graph_authority)?;
		state.graph_superblocks.insert(binding.graph_id, checkpointed);
	}
	let clocks = committed.clocks();
	let next_file_instance_id =
		clocks.next_file_instance_id.max(state.file_catalog.next_file_instance_id);
	let next = DatabaseSuperblock {
		name: state.database_superblock.name.clone(),
		generation: committed.generation(),
		checkpoint_lsn: committed.target(),
		checkpoint_record_lsn: committed.checkpoint_record_lsn(),
		durable_visible_lsn: clocks.durable_visible_lsn,
		next_lsn: clocks.next_lsn,
		commit_ts: clocks.commit_ts,
		next_tx_id: clocks.next_tx_id,
		next_file_instance_id,
		system_backfilled_lsn: committed.target(),
		database_catalog_root: DATABASE_CATALOG_ROOT,
		file_catalog_root: FILE_CATALOG_ROOT,
		graph_count: u32::try_from(state.bindings.graphs.len())
			.map_err(|_error| invalid("checkpoint graph count exceeds u32"))?,
	};
	write_database_superblock(&state.database_superblock_path, &next)?;
	state.database_superblock = next;
	wal.publish_checkpoint(&committed)?;
	Ok(true)
}
