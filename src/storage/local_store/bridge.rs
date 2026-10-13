//! Correspondence generations use ordinary catalog/WAL publication and
//! recovery.

use std::sync::Arc;

use super::layout::{FileImage, LayoutBuilder, invalid};
use crate::{
	datastore::{DataSpaceId, DatabaseRoot},
	gql_status::FerrosGrynnResult,
	storage::{
		BridgeCatalogBinding,
		bridge_mapping,
		catalog::{
			CatalogPath,
			DatabaseCatalog,
			FileCatalog,
			FileCatalogEntry,
			PhysicalIndexBinding,
		},
		database_superblock::DatabaseSuperblock,
		file_registry::{DataFileRole, FileRegistry},
		hybrid_engine::HybridReadEngine,
		image_io::read_page_cached,
		page::PageNo,
		types::{DataFileKind, PageKey, StorageSnapshot},
		wal::FrameWal,
	},
};

/// Retains unchanged roots or prepares independent immutable mapping files.
pub(super) fn prepare_layout(
	database: &DatabaseRoot,
	layout: &mut LayoutBuilder,
	root: Option<&std::path::Path>,
) -> FerrosGrynnResult<(Option<BridgeCatalogBinding>, Option<super::cleanup::UnpublishedGeneration>)>
{
	if database.bridges.databases.is_empty() {
		return Ok((None, None));
	}
	if let Some(binding) = database
		.bridges
		.physical
		.filter(|binding| binding.generation == database.bridges.generation)
	{
		for (name, root) in roots(binding) {
			layout.retain(entry(binding, name, root)?)?;
		}
		return Ok((Some(binding), None));
	}
	let directory = uuid::Uuid::now_v7();
	if let Some(root) = root {
		let cleanup = super::cleanup::UnpublishedGeneration::for_mapping(root, directory);
		let built = bridge_mapping::build_streamed(
			&database.bridges,
			database.database_uuid,
			root,
			directory,
		)?;
		let mut add = |name: &str,
		               index: crate::storage::graph_index::StreamedIndex|
		 -> FerrosGrynnResult<PhysicalIndexBinding> {
			let file_id = layout.add_prewritten(
				DataSpaceId::Database,
				DataFileKind::BridgeMappings,
				DataFileRole::WalBackfilled,
				format!("atoms/{directory}/bridges/{name}"),
				index.page_count,
			)?;
			Ok(PhysicalIndexBinding {
				file_id,
				root: index.root,
			})
		};
		let [heap, primary, direct, reverse] = built;
		let binding = BridgeCatalogBinding {
			generation: database.bridges.generation,
			directory,
			count: database.bridges.databases.len() as u64 + database.bridges.rows().count() as u64,
			heap: add("records.fgb", heap)?,
			primary: add("primary.fgidx", primary)?,
			direct: add("local.fgidx", direct)?,
			reverse: add("external.fgidx", reverse)?,
		};
		return Ok((Some(binding), Some(cleanup)));
	}
	let built = bridge_mapping::build(&database.bridges, database.database_uuid)?;
	let mut add = |name: &str, root, pages| -> FerrosGrynnResult<PhysicalIndexBinding> {
		let file_id = layout.add(
			DataSpaceId::Database,
			DataFileKind::BridgeMappings,
			DataFileRole::WalBackfilled,
			format!("atoms/{directory}/bridges/{name}"),
			None,
			pages,
		)?;
		Ok(PhysicalIndexBinding {
			file_id,
			root,
		})
	};
	Ok((
		Some(BridgeCatalogBinding {
			generation: database.bridges.generation,
			directory,
			count: database.bridges.databases.len() as u64 + database.bridges.rows().count() as u64,
			heap: add("records.fgb", PageNo::new(0), built.heap)?,
			primary: add("primary.fgidx", built.primary.root, built.primary.pages)?,
			direct: add("local.fgidx", built.direct.root, built.direct.pages)?,
			reverse: add("external.fgidx", built.reverse.root, built.reverse.pages)?,
		}),
		None,
	))
}

/// Pure confirmations retain every unrelated atom/graph/index file.
pub(super) fn prepare_catalog(
	database: &DatabaseRoot,
	files: &mut FileCatalog,
	catalog: &mut DatabaseCatalog,
	images: &mut Vec<FileImage>,
	root: &std::path::Path,
) -> FerrosGrynnResult<Option<super::cleanup::UnpublishedGeneration>> {
	let mut layout = LayoutBuilder::new(files.next_file_instance_id);
	let (binding, cleanup) = prepare_layout(database, &mut layout, Some(root))?;
	let (prepared, additional) = layout.finish()?;
	files.files.retain(|entry| entry.file_id.kind() != Some(DataFileKind::BridgeMappings));
	files.files.extend(
		prepared
			.files
			.into_iter()
			.filter(|entry| entry.file_id.kind() == Some(DataFileKind::BridgeMappings)),
	);
	files.files.sort_unstable_by_key(|entry| (entry.space_id, entry.file_id));
	files.next_file_instance_id = prepared.next_file_instance_id;
	images.extend(
		additional
			.into_iter()
			.filter(|image| image.entry.file_id.kind() == Some(DataFileKind::BridgeMappings)),
	);
	catalog.bridges = binding;
	Ok(cleanup)
}

/// Restores authority and verifies both index directions before ordinary
/// validation.
#[allow(clippy::too_many_arguments, reason = "recovery shares the existing exact WAL authority")]
pub(super) fn recover(
	database: &mut DatabaseRoot,
	catalog: &DatabaseCatalog,
	files: &FileCatalog,
	registry: &FileRegistry,
	wal: &FrameWal,
	engine: &HybridReadEngine,
	snapshot: StorageSnapshot,
	superblock: &DatabaseSuperblock,
) -> FerrosGrynnResult<()> {
	let Some(binding) = catalog.bridges else {
		return Ok(());
	};
	for (name, root) in roots(binding) {
		let expected = entry(binding, name, root)?;
		let actual = files
			.files
			.iter()
			.find(|value| value.file_id == root.file_id && value.space_id == DataSpaceId::Database)
			.ok_or_else(|| invalid("mapping physical file is absent"))?;
		if actual != &expected {
			return Err(invalid("mapping physical registration mismatch"));
		}
	}
	let mappings =
		bridge_mapping::recover(database.database_uuid, binding, |root, page, output| {
			read_page_cached(
				wal,
				registry,
				engine,
				snapshot,
				superblock.generation,
				superblock.system_backfilled_lsn,
				PageKey::new(DataSpaceId::Database, root.file_id, page),
				output,
			)
		})?;
	for descriptor in mappings.databases.values() {
		if u64::from(descriptor.reference.id.get()) >= catalog.next_schema_id {
			return Err(invalid("mapping database identity exceeds allocation clock"));
		}
	}
	for mapping in
		mappings.rows().filter(|row| row.state != crate::bridge::BridgeMappingState::Tombstoned)
	{
		let atom = database
			.atom_by_record(mapping.subject.record())
			.ok_or_else(|| invalid("mapping local subject is absent"))?;
		if let crate::bridge::BridgeMappingSubject::Relation {
			graph,
			..
		} = &mapping.subject
		{
			if database.membership(*graph, atom.id()).is_none()
				|| atom.kind() != crate::graph::AtomKind::Edge
			{
				return Err(invalid("mapping edge occurrence is absent"));
			}
		}
	}
	database.bridges = Arc::new(mappings);
	let registry = database.bridges.clone();
	for descriptor in registry.databases.values() {
		for role in
			[crate::bridge::BridgeMappingRole::Atom, crate::bridge::BridgeMappingRole::Relation]
		{
			database.mark_version(
				crate::datastore::ConflictKey::BridgeMappings(descriptor.reference.id, role),
				database.commit_ts,
			);
		}
	}
	database.mark_version(crate::datastore::ConflictKey::BridgeDatabases, database.commit_ts);
	Ok(())
}

/// Lists the four exact physical roots belonging to one mapping generation.
fn roots(binding: BridgeCatalogBinding) -> [(&'static str, PhysicalIndexBinding); 4] {
	[
		("records.fgb", binding.heap),
		("primary.fgidx", binding.primary),
		("local.fgidx", binding.direct),
		("external.fgidx", binding.reverse),
	]
}
/// Builds the canonical database-space file registration for one root.
fn entry(
	binding: BridgeCatalogBinding,
	name: &str,
	root: PhysicalIndexBinding,
) -> FerrosGrynnResult<FileCatalogEntry> {
	Ok(FileCatalogEntry {
		space_id: DataSpaceId::Database,
		file_id: root.file_id,
		companion_of: None,
		role: DataFileRole::WalBackfilled,
		path: CatalogPath::parse(&format!("atoms/{}/bridges/{name}", binding.directory))?,
	})
}
