//! Shared WAL-first transport for immutable data-space page images.

use std::{
	collections::{BTreeMap, BTreeSet},
	sync::Arc,
};

#[cfg(test)]
use super::wal::TxHandle;
use super::{
	file_registry::{FileRegistry, RegisteredDataFile},
	hybrid_engine::HybridReadEngine,
	page::{AlignedPage, PageNo},
	page_cache::{PageCacheKey, PageCacheVersion},
	types::{CheckpointGeneration, DataFileId, Lsn, PageKey, StorageSnapshot},
	wal::{CheckpointTicket, FrameWal},
};
use crate::{
	datastore::DataSpaceId,
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
};

/// Returns the stage pages result.
#[cfg(test)]
pub(super) fn stage_pages(
	tx: &mut TxHandle,
	space: DataSpaceId,
	file_id: DataFileId,
	pages: &[AlignedPage],
) -> FerrosGrynnResult<()> {
	for (index, page) in pages.iter().enumerate() {
		let page_no = PageNo::new(
			u64::try_from(index).map_err(|_error| invalid("staged page index exceeds u64"))?,
		);
		tx.stage_page(PageKey::new(space, file_id, page_no), page)?;
	}
	Ok(())
}

/// Reads pages.
pub(super) fn read_pages(
	wal: &FrameWal,
	registry: &FileRegistry,
	snapshot: StorageSnapshot,
	space: DataSpaceId,
	file_id: DataFileId,
	page_count: u64,
) -> FerrosGrynnResult<Vec<AlignedPage>> {
	let home = registry.resolve_file(space, file_id)?;
	let home = home.file()?;
	let visible_page_count = visible_file_page_count(wal, registry, snapshot, space, file_id)?;
	if page_count > visible_page_count {
		return Err(invalid("snapshot-visible page ceiling exceeds its physical and WAL image"));
	}
	let count = usize::try_from(page_count)
		.map_err(|_error| invalid("snapshot-visible page count exceeds usize"))?;
	let mut keys = Vec::new();
	keys.try_reserve_exact(count)
		.map_err(|_error| invalid("snapshot-visible page-key allocation failed"))?;
	for index in 0..page_count {
		keys.push(PageKey::new(space, file_id, PageNo::new(index)));
	}
	let visible = wal.visible_frames(&keys, snapshot);
	let mut pages = Vec::new();
	pages
		.try_reserve_exact(count)
		.map_err(|_error| invalid("snapshot-visible page allocation failed"))?;
	for (key, frame) in keys.into_iter().zip(visible.frames()) {
		let mut page = AlignedPage::zeroed();
		if let Some(frame) = frame {
			visible.read_frame(*frame, &mut page)?;
		} else {
			home.read_page(key.page_no(), &mut page)?;
		}
		pages.push(page);
	}
	Ok(pages)
}

/// Loads a snapshot-visible extent through the production exact-version
/// resident arena.
pub(super) fn read_pages_cached(
	wal: &FrameWal,
	registry: &FileRegistry,
	engine: &HybridReadEngine,
	snapshot: StorageSnapshot,
	home_generation: CheckpointGeneration,
	home_ceiling: Lsn,
	space: DataSpaceId,
	file_id: DataFileId,
	page_count: u64,
) -> FerrosGrynnResult<Vec<AlignedPage>> {
	let home = registry.resolve_file(space, file_id)?;
	let home = home.file()?;
	let visible_page_count = visible_file_page_count(wal, registry, snapshot, space, file_id)?;
	if page_count > visible_page_count {
		return Err(invalid("snapshot-visible page ceiling exceeds its physical and WAL image"));
	}
	let count = usize::try_from(page_count)
		.map_err(|_error| invalid("snapshot-visible page count exceeds usize"))?;
	let keys = (0..page_count)
		.map(|page| PageKey::new(space, file_id, PageNo::new(page)))
		.collect::<Vec<_>>();
	let visible = wal.visible_frames(&keys, snapshot);
	let mut pages = Vec::new();
	pages
		.try_reserve_exact(count)
		.map_err(|_error| invalid("snapshot-visible page allocation failed"))?;
	for (key, frame) in keys.into_iter().zip(visible.frames()) {
		let version = frame.map_or(
			PageCacheVersion::Home {
				generation: home_generation,
				ceiling: home_ceiling,
			},
			|frame| PageCacheVersion::Wal(super::page::PageLsn::new(frame.frame_lsn().get())),
		);
		let cache_key = PageCacheKey::new(key, version);
		let page = engine.get_or_load(cache_key, |output| match frame {
			Some(frame) => visible.read_frame(*frame, output),
			None => home.read_page(key.page_no(), output),
		})?;
		let mut owned = AlignedPage::zeroed();
		owned.as_bytes_mut().copy_from_slice(page.as_bytes());
		pages.push(owned);
	}
	Ok(pages)
}

/// Loads one arbitrary snapshot-visible page through the exact-version arena.
pub(super) fn read_page_cached(
	wal: &FrameWal,
	registry: &FileRegistry,
	engine: &HybridReadEngine,
	snapshot: StorageSnapshot,
	home_generation: CheckpointGeneration,
	home_ceiling: Lsn,
	key: PageKey,
	output: &mut AlignedPage,
) -> FerrosGrynnResult<()> {
	let home = registry.resolve(key)?;
	read_pinned_page_cached(
		wal,
		&home,
		engine,
		snapshot,
		home_generation,
		home_ceiling,
		key,
		output,
	)
}

/// Reads from an exact registered-file pin after its catalog generation
/// retires.
pub(super) fn read_pinned_page_cached(
	wal: &FrameWal,
	home: &RegisteredDataFile,
	engine: &HybridReadEngine,
	snapshot: StorageSnapshot,
	home_generation: CheckpointGeneration,
	home_ceiling: Lsn,
	key: PageKey,
	output: &mut AlignedPage,
) -> FerrosGrynnResult<()> {
	let visible = wal.visible_frames(std::slice::from_ref(&key), snapshot);
	let frame = visible.frames()[0];
	let version = frame.map_or(
		PageCacheVersion::Home {
			generation: home_generation,
			ceiling: home_ceiling,
		},
		|frame| PageCacheVersion::Wal(super::page::PageLsn::new(frame.frame_lsn().get())),
	);
	let page = engine.get_or_load(PageCacheKey::new(key, version), |destination| match frame {
		Some(frame) => visible.read_frame(frame, destination),
		None => home.file()?.read_page(key.page_no(), destination),
	})?;
	output.as_bytes_mut().copy_from_slice(page.as_bytes());
	Ok(())
}

/// Returns the exact snapshot-visible length of one ordinary data file.
pub(super) fn visible_file_page_count(
	wal: &FrameWal,
	registry: &FileRegistry,
	snapshot: StorageSnapshot,
	space: DataSpaceId,
	file_id: DataFileId,
) -> FerrosGrynnResult<u64> {
	let home_pages = registry.resolve_file(space, file_id)?.file()?.page_count();
	Ok(home_pages.max(wal.visible_file_page_count(snapshot, space, file_id).unwrap_or(0)))
}

/// Copies selected WAL frames for ordinary files into their registered homes.
#[cfg(test)]
pub(super) fn backfill_pages(
	wal: &FrameWal,
	registry: &FileRegistry,
	space: DataSpaceId,
	files: &[DataFileId],
	ticket: &CheckpointTicket,
) -> FerrosGrynnResult<()> {
	let files = files
		.iter()
		.map(|file| Ok(((space, *file), registry.resolve_file(space, *file)?)))
		.collect::<FerrosGrynnResult<BTreeMap<_, _>>>()?;
	backfill_pinned_pages(wal, &files, ticket)
}

/// Backfills current and retired trusted handles before their WAL versions
/// are pruned. The owned pins keep retired files alive through synchronization.
pub(super) fn backfill_pinned_pages(
	wal: &FrameWal,
	files: &BTreeMap<(DataSpaceId, DataFileId), Arc<RegisteredDataFile>>,
	ticket: &CheckpointTicket,
) -> FerrosGrynnResult<()> {
	let mut touched = BTreeSet::new();
	let mut reader = wal.frame_read_scope();
	for frame in ticket.frames() {
		let key = frame.key();
		let identity = (key.space_id(), key.file_id());
		let Some(registered) = files.get(&identity) else {
			continue;
		};
		if registered.role() != super::file_registry::DataFileRole::WalBackfilled {
			return Err(invalid("checkpoint positional map contains a metadata file"));
		}
		let mut page = AlignedPage::zeroed();
		reader.read_frame(*frame, &mut page)?;
		registered.file()?.write_page(key.page_no(), &page)?;
		touched.insert(identity);
	}
	for file in touched {
		files
			.get(&file)
			.ok_or_else(|| invalid("checkpoint lost its registered file"))?
			.file()?
			.sync_data()?;
	}
	Ok(())
}

/// Returns the invalid result.
fn invalid(reason: impl Into<String>) -> FerrosGrynnError {
	FerrosGrynnError::invalid_storage_file(reason)
}
