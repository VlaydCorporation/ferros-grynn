//! Restores accepted graph checkpoints without querying current primary data.

use std::sync::Arc;

use super::layout::invalid;
use crate::{
	datastore::{DataSpaceId, DatabaseRoot},
	gql_status::FerrosGrynnResult,
	live::LivePayload,
	storage::{
		catalog::DatabaseCatalog,
		database_superblock::DatabaseSuperblock,
		file_registry::FileRegistry,
		history_codec::{
			CodecBudget,
			pages::{decode_header, decode_locator, decode_pages},
		},
		hybrid_engine::HybridReadEngine,
		image_io::read_page_cached,
		live_history,
		page::{AlignedPage, PAGE_SIZE, PageNo},
		types::{PageKey, StorageSnapshot},
		wal::FrameWal,
	},
};

/// Restores one admitted image per query before exposing the recovered root.
#[allow(clippy::too_many_arguments, reason = "shares the existing recovery authority tuple")]
pub(super) fn recover(
	root: &mut DatabaseRoot,
	catalog: &DatabaseCatalog,
	registry: &FileRegistry,
	wal: &FrameWal,
	engine: &HybridReadEngine,
	snapshot: StorageSnapshot,
	authority: &DatabaseSuperblock,
) -> FerrosGrynnResult<()> {
	for entry in &catalog.live_queries {
		let Some(segment) = entry.graph_checkpoint else {
			continue;
		};
		registry.resolve_file(DataSpaceId::Database, segment.file)?;
		// Retained images and the one transient decoder have independent caps.
		let maximum = entry.options.scratch_bytes().min(256 * 1024 * 1024);
		let mut budget = CodecBudget::new(maximum, PAGE_SIZE)?;
		let mut read = |number, output: &mut AlignedPage| {
			read_page_cached(
				wal,
				registry,
				engine,
				snapshot,
				authority.generation,
				authority.system_backfilled_lsn,
				PageKey::new(DataSpaceId::Database, segment.file, number),
				output,
			)
		};
		let mut page = AlignedPage::zeroed();
		read(PageNo::new(0), &mut page)?;
		if decode_header(&page)? != segment.header {
			return Err(invalid("LIVE graph checkpoint header differs from its catalog"));
		}
		read(PageNo::new(1), &mut page)?;
		let locator = decode_locator(&page, PageNo::new(1), segment.header.pages)?;
		if locator.stamp != segment.header.last_stamp
			|| 1 + u64::from(locator.pages) != segment.header.pages
		{
			return Err(invalid("LIVE graph checkpoint contains another commit interval"));
		}
		let bytes = decode_pages(locator, segment.header.pages, &mut read, &mut budget)?;
		let info = live_history::inspect(&bytes, bytes.len() as u64)?;
		let LivePayload::Graph(batch) = live_history::decode_delivery_bounded(&bytes, &mut budget)?
		else {
			return Err(invalid("LIVE graph checkpoint contains another payload kind"));
		};
		let recipe = entry
			.query
			.graph_result()
			.ok_or_else(|| invalid("graph checkpoint belongs to a row recipe"))?;
		if recipe.family() != batch.delta().after().family()
			|| recipe.validators() != batch.delta().after().validators()
		{
			return Err(invalid("graph checkpoint family differs from its authoritative recipe"));
		}
		if batch.id() != entry.id
			|| batch.version() != entry.version
			|| batch.versionstamp().get() != locator.stamp
			|| info.time != segment.header.last_time
		{
			return Err(invalid(
				"LIVE graph checkpoint image differs from its authoritative bounds",
			));
		}
		Arc::make_mut(root.live_catalog_mut()).replace_graph_checkpoint(entry.id, batch)?;
	}
	Ok(())
}
