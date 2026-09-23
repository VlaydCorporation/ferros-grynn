//! Eager statistics-root validation without reading auxiliary payloads.

use super::layout::invalid;
use crate::{
	datastore::DataSpaceId,
	gql_status::FerrosGrynnResult,
	storage::{
		base_statistics::decode_base_statistics_root,
		catalog::FileCatalog,
		database_superblock::DatabaseSuperblock,
		file_registry::FileRegistry,
		graph_pages::decode_statistics_root,
		hybrid_engine::HybridReadEngine,
		image_io::{read_page_cached, visible_file_page_count},
		page::{AlignedPage, PageNo},
		types::{DataFileId, DataFileKind, PageKey, StorageSnapshot},
		wal::FrameWal,
	},
};

/// Reads only the Base and per-graph directory pages required at reopen.
pub(super) fn validate_statistics_roots(
	wal: &FrameWal,
	registry: &FileRegistry,
	engine: &HybridReadEngine,
	snapshot: StorageSnapshot,
	authority: &DatabaseSuperblock,
	files: &FileCatalog,
) -> FerrosGrynnResult<()> {
	let base_classes = singleton(files, DataSpaceId::Database, DataFileKind::ClassStatistics)?;
	let base_properties =
		singleton(files, DataSpaceId::Database, DataFileKind::PropertyStatistics)?;
	let mut root = AlignedPage::zeroed();
	read_page_cached(
		wal,
		registry,
		engine,
		snapshot,
		authority.generation,
		authority.system_backfilled_lsn,
		PageKey::new(DataSpaceId::Database, base_classes, PageNo::new(0)),
		&mut root,
	)?;
	decode_base_statistics_root(
		&root,
		visible_file_page_count(wal, registry, snapshot, DataSpaceId::Database, base_classes)?,
		visible_file_page_count(wal, registry, snapshot, DataSpaceId::Database, base_properties)?,
	)?;

	for space in files
		.files
		.iter()
		.map(|entry| entry.space_id)
		.filter(|space| matches!(space, DataSpaceId::Graph(_)))
		.collect::<std::collections::BTreeSet<_>>()
	{
		let graph = singleton(files, space, DataFileKind::Statistics)?;
		let labels = singleton(files, space, DataFileKind::LabelStatistics)?;
		let properties = singleton(files, space, DataFileKind::PropertyStatistics)?;
		read_page_cached(
			wal,
			registry,
			engine,
			snapshot,
			authority.generation,
			authority.checkpoint_lsn,
			PageKey::new(space, graph, PageNo::new(0)),
			&mut root,
		)?;
		decode_statistics_root(
			&root,
			visible_file_page_count(wal, registry, snapshot, space, graph)?,
			visible_file_page_count(wal, registry, snapshot, space, labels)?,
			visible_file_page_count(wal, registry, snapshot, space, properties)?,
		)?;
	}
	Ok(())
}

fn singleton(
	files: &FileCatalog,
	space: DataSpaceId,
	kind: DataFileKind,
) -> FerrosGrynnResult<DataFileId> {
	let mut matches = files
		.files
		.iter()
		.filter(|entry| entry.space_id == space && entry.file_id.kind() == Some(kind));
	let file = matches
		.next()
		.ok_or_else(|| invalid(format!("{space:?} has no {kind:?} statistics file")))?;
	if matches.next().is_some() {
		return Err(invalid(format!("{space:?} has duplicate {kind:?} statistics files")));
	}
	Ok(file.file_id)
}
