//! Database-wide passive/restart checkpoint publication.

use std::collections::BTreeMap;

use super::{
	LocalState,
	layout::{DATABASE_CATALOG_ROOT, FILE_CATALOG_ROOT, invalid},
};
use crate::{
	datastore::{CheckpointMode, DataSpaceId},
	gql_status::FerrosGrynnResult,
	storage::{
		atom_store_image::{AtomStoreCheckpointAuthority, write_atom_store_checkpoint_page},
		atom_store_superblock::AtomStoreSuperblock,
		database_superblock::DatabaseSuperblock,
		file_registry::DataFileRole,
		graph_store::{GraphCheckpointAuthority, write_graph_checkpoint_page},
		graph_superblock::GraphSuperblock,
		image_io::backfill_pinned_pages,
		local::write_database_superblock,
		page::{AlignedPage, PageNo},
		types::PageKey,
		wal::{CheckpointTicket, FrameWal},
	},
};

/// Returns the checkpoint result.
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
	let metadata = checkpoint_metadata(wal, state, &ticket)?;
	let files = state
		.registry
		.snapshot()
		.into_iter()
		.chain(
			state
				.retired_generations
				.iter()
				.flat_map(super::cleanup::RetiredGeneration::pinned_files),
		)
		.filter(|(_key, file)| file.role() == DataFileRole::WalBackfilled)
		.collect::<BTreeMap<_, _>>();
	backfill_pinned_pages(wal, &files, &ticket)?;
	let committed = wal.finish_checkpoint(ticket)?;
	let authority = AtomStoreCheckpointAuthority::new(
		committed.generation(),
		committed.target(),
		committed.checkpoint_record_lsn(),
	);
	let mut next_atom = state.atom_superblock;
	if let Some(logical) = metadata.atoms {
		write_atom_store_checkpoint_page(
			&state.registry,
			state.bindings.atoms,
			logical,
			authority,
		)?;
		next_atom.checkpoint_generation = committed.generation();
		next_atom.backfilled_lsn = committed.target();
		next_atom.checkpoint_record_lsn = committed.checkpoint_record_lsn();
	}
	let graph_authority = GraphCheckpointAuthority::new(
		committed.generation(),
		committed.target(),
		committed.checkpoint_record_lsn(),
	);
	let mut next_graphs = state.graph_superblocks.clone();
	for binding in &state.bindings.graphs {
		if let Some(logical) = metadata.graphs.get(&binding.graph_id) {
			write_graph_checkpoint_page(&state.registry, binding.files, *logical, graph_authority)?;
			let latest = next_graphs
				.get_mut(&binding.graph_id)
				.ok_or_else(|| invalid("checkpoint graph has no logical superblock"))?;
			latest.checkpoint_generation = committed.generation();
			latest.backfilled_lsn = committed.target();
			latest.checkpoint_record_lsn = committed.checkpoint_record_lsn();
		}
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
	// Runtime ceilings describe the latest root, not necessarily the older
	// ticket version written home. Install only the newly authorized tuple.
	state.atom_superblock = next_atom;
	state.graph_superblocks = next_graphs;
	state.database_superblock = next;
	wal.publish_checkpoint(&committed)?;
	Ok(true)
}

/// Only exact ticket frames may replace subordinate metadata. An absent frame
/// leaves its old authorized COW home intact, including after-target bindings.
struct CheckpointMetadata {
	/// AtomStore logical frame selected at or before the checkpoint target.
	atoms: Option<AtomStoreSuperblock>,
	/// Current graph bindings with a logical frame selected by this ticket.
	graphs: BTreeMap<crate::graph::GraphId, GraphSuperblock>,
}

/// Decodes stamped logical metadata before the ticket is consumed or pruned.
fn checkpoint_metadata(
	wal: &FrameWal,
	state: &LocalState,
	ticket: &CheckpointTicket,
) -> FerrosGrynnResult<CheckpointMetadata> {
	let atom = PageKey::new(DataSpaceId::Database, state.bindings.atoms.superblock, PageNo::new(0));
	let graph_files = state
		.bindings
		.graphs
		.iter()
		.map(|binding| {
			(
				PageKey::new(
					DataSpaceId::Graph(binding.graph_id),
					binding.files.superblock,
					PageNo::new(0),
				),
				binding.graph_id,
			)
		})
		.collect::<BTreeMap<_, _>>();
	let mut metadata = CheckpointMetadata {
		atoms: None,
		graphs: BTreeMap::new(),
	};
	let mut reader = wal.frame_read_scope();
	for frame in ticket.frames() {
		let key = frame.key();
		if key != atom && !graph_files.contains_key(&key) {
			continue;
		}
		let mut page = AlignedPage::zeroed();
		reader.read_frame(*frame, &mut page)?;
		if key == atom {
			metadata.atoms = Some(AtomStoreSuperblock::decode(&page)?);
		} else if let Some(graph) = graph_files.get(&key) {
			let logical = GraphSuperblock::decode(&page)?;
			if logical.graph_id != *graph {
				return Err(invalid("checkpoint graph frame belongs to another graph"));
			}
			metadata.graphs.insert(*graph, logical);
		}
	}
	Ok(metadata)
}
