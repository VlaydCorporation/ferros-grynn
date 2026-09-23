//! Shared WAL-first transport for immutable data-space page images.

use std::collections::BTreeSet;

use super::{
	file_registry::FileRegistry,
	hybrid_engine::HybridReadEngine,
	page::{AlignedPage, PageNo},
	page_cache::{PageCacheKey, PageCacheVersion},
	types::{CheckpointGeneration, DataFileId, Lsn, PageKey, StorageSnapshot},
	wal::{CheckpointTicket, FrameWal, TxHandle},
};
use crate::{
	datastore::DataSpaceId,
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
};

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
pub(super) fn backfill_pages(
	wal: &FrameWal,
	registry: &FileRegistry,
	space: DataSpaceId,
	files: &[DataFileId],
	ticket: &CheckpointTicket,
) -> FerrosGrynnResult<()> {
	let mut touched = BTreeSet::new();
	for frame in ticket.frames() {
		let key = frame.key();
		if key.space_id() != space || !files.contains(&key.file_id()) {
			continue;
		}
		let registered = registry.resolve(key)?;
		let mut page = AlignedPage::zeroed();
		wal.read_frame(*frame, &mut page)?;
		registered.file()?.write_page(key.page_no(), &page)?;
		touched.insert(key.file_id());
	}
	for file in touched {
		registry.resolve_file(space, file)?.file()?.sync_data()?;
	}
	Ok(())
}

fn invalid(reason: impl Into<String>) -> FerrosGrynnError {
	FerrosGrynnError::invalid_storage_file(reason)
}
