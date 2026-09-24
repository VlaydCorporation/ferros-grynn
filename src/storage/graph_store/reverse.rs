//! Database-global reverse graph-membership index publication and validation.

use std::{collections::BTreeMap, path::Path};

use super::ReverseMembershipFile;
use crate::{
	datastore::{DataSpaceId, DatabaseRoot, GlobalAtomId},
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	graph::GraphId,
	storage::{
		file_registry::FileRegistry,
		graph_index::{BuiltPrimaryIndex, SortedIndexRecords, scan_primary_index},
		image_io::{backfill_pages, read_pages, read_pages_cached, stage_pages},
		membership_index::{build_reverse_membership_index, sort_reverse_membership_index},
		page::{AlignedPage, PageLsn, PageNo},
		types::StorageSnapshot,
		wal::{CheckpointTicket, FrameWal, TxHandle},
	},
	values::storage::DiskAtomRef,
};

/// Builds the database-global `memberships.fgidx` image.
pub(in crate::storage) fn build_reverse_membership_image(
	database: &DatabaseRoot,
) -> FerrosGrynnResult<BuiltPrimaryIndex> {
	build_reverse_membership_index(&database.atoms, PageLsn::ZERO)
}

/// Builds bounded reverse-membership input for atomic import.
pub(in crate::storage) fn build_import_reverse_membership_image(
	database: &DatabaseRoot,
	scratch_root: &Path,
) -> FerrosGrynnResult<SortedIndexRecords> {
	sort_reverse_membership_index(&database.atoms, scratch_root)
}

/// Stages the reverse membership image in the caller's database WAL
/// transaction.
pub(in crate::storage) fn stage_reverse_membership_image(
	tx: &mut TxHandle,
	binding: ReverseMembershipFile,
	image: &BuiltPrimaryIndex,
) -> FerrosGrynnResult<()> {
	validate_root(binding, image)?;
	stage_pages(tx, DataSpaceId::Database, binding.file, &image.pages)
}

/// Reopens the complete snapshot-visible reverse membership B+tree.
pub(in crate::storage) fn open_reverse_membership_image(
	wal: &FrameWal,
	registry: &FileRegistry,
	engine: Option<&crate::storage::hybrid_engine::HybridReadEngine>,
	snapshot: StorageSnapshot,
	home_generation: crate::storage::types::CheckpointGeneration,
	home_ceiling: crate::storage::types::Lsn,
	binding: ReverseMembershipFile,
) -> FerrosGrynnResult<BuiltPrimaryIndex> {
	let home_pages =
		registry.resolve_file(DataSpaceId::Database, binding.file)?.file()?.page_count();
	let count = home_pages.max(
		wal.visible_file_page_count(snapshot, DataSpaceId::Database, binding.file).unwrap_or(0),
	);
	if count == 0 || binding.root.get() >= count {
		return Err(invalid("reverse membership root exceeds its snapshot-visible file"));
	}
	let pages = match engine {
		Some(engine) => read_pages_cached(
			wal,
			registry,
			engine,
			snapshot,
			home_generation,
			home_ceiling,
			DataSpaceId::Database,
			binding.file,
			count,
		)?,
		None => read_pages(wal, registry, snapshot, DataSpaceId::Database, binding.file, count)?,
	};
	Ok(BuiltPrimaryIndex {
		root: binding.root,
		pages,
	})
}

/// Backfills the database-global reverse membership index selected by a
/// checkpoint.
pub(in crate::storage) fn backfill_reverse_membership_checkpoint(
	wal: &FrameWal,
	registry: &FileRegistry,
	binding: ReverseMembershipFile,
	ticket: &CheckpointTicket,
) -> FerrosGrynnResult<()> {
	backfill_pages(wal, registry, DataSpaceId::Database, &[binding.file], ticket)
}

/// Confirms exact agreement between both durable membership directions and RAM
/// state.
pub(in crate::storage) fn validate_reverse_memberships(
	image: &BuiltPrimaryIndex,
	database: &DatabaseRoot,
) -> FerrosGrynnResult<()> {
	let mut actual = BTreeMap::new();
	for entry in scan_primary_index::<DiskAtomRef>(
		|page_no, output| copy_page(&image.pages, page_no, output),
		image.root,
	)? {
		let (global, graph) = decode_key(&entry.key)?;
		if actual.insert((global, graph), entry.atom).is_some() {
			return Err(invalid("reverse membership index contains a duplicate key"));
		}
	}

	let mut expected = BTreeMap::new();
	for atom in database.atoms.atoms() {
		if let Some(memberships) = database.atoms.memberships(atom.id()) {
			for (graph, local) in memberships {
				let kind =
					local.kind().ok_or_else(|| invalid("RAM membership contains a null atom"))?;
				let disk = DiskAtomRef::from_parts(kind, local.slot())?;
				expected.insert((atom.id(), *graph), disk);
			}
		}
	}
	if actual != expected {
		return Err(invalid("reverse membership index disagrees with graph occurrences"));
	}
	Ok(())
}

/// Decodes the fixed `(GlobalAtomId, GraphId)` key.
fn decode_key(bytes: &[u8]) -> FerrosGrynnResult<(GlobalAtomId, GraphId)> {
	let bytes: [u8; 16] = bytes
		.try_into()
		.map_err(|_error| invalid("reverse membership key is not exactly sixteen bytes"))?;
	let mut global = [0_u8; 8];
	global.copy_from_slice(&bytes[..8]);
	let mut graph = [0_u8; 8];
	graph.copy_from_slice(&bytes[8..]);
	Ok((
		GlobalAtomId::from_storage(u64::from_be_bytes(global))?,
		GraphId::new(u64::from_be_bytes(graph))
			.ok_or_else(|| invalid("reverse membership key contains graph identity zero"))?,
	))
}

/// Copies one page out of an immutable index image.
fn copy_page(
	pages: &[AlignedPage],
	page_no: PageNo,
	output: &mut AlignedPage,
) -> FerrosGrynnResult<()> {
	let index = usize::try_from(page_no.get())
		.map_err(|_error| invalid("reverse membership page exceeds usize"))?;
	let page = pages.get(index).ok_or_else(|| invalid("reverse membership page is absent"))?;
	output.as_bytes_mut().copy_from_slice(page.as_bytes());
	Ok(())
}

/// Checks that the immutable image matches its catalog root binding.
fn validate_root(
	binding: ReverseMembershipFile,
	image: &BuiltPrimaryIndex,
) -> FerrosGrynnResult<()> {
	let count = u64::try_from(image.pages.len())
		.map_err(|_error| invalid("reverse membership page count exceeds u64"))?;
	if count == 0 || image.root != binding.root || image.root.get() >= count {
		return Err(invalid("reverse membership root disagrees with its physical binding"));
	}
	Ok(())
}

fn invalid(reason: impl Into<String>) -> FerrosGrynnError {
	FerrosGrynnError::invalid_storage_file(reason)
}
