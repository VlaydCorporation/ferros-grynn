//! Removal of unpublished UUID generations left by failed imports or crashes.

use std::{
	collections::BTreeSet,
	fs,
	path::{Path, PathBuf},
};

use super::layout::invalid;
use crate::{
	datastore::DatabaseRoot,
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	storage::{catalog::FileCatalog, types::DataFileKind},
	values::Uuid,
};

/// Deletes an import generation on unwind or error until its catalog commit is
/// known to be durable.
pub(super) struct UnpublishedGeneration {
	/// AtomStore and graph-generation directories owned by the import.
	directories: Vec<PathBuf>,
	/// Whether `Drop` must remove the directories.
	armed: bool,
}

impl UnpublishedGeneration {
	/// Arms cleanup before the first streamed side-file is created.
	pub(super) fn for_database(root: &Path, database: &DatabaseRoot, generation: &str) -> Self {
		let mut directories = vec![root.join("atoms").join(generation)];
		directories.extend(database.schema.graphs().map(|graph| {
			root.join("graphs").join(format!("{:016x}", graph.id().get())).join(generation)
		}));
		Self {
			directories,
			armed: true,
		}
	}

	/// Disarms cleanup after the catalog/WAL publication is durable.
	pub(super) fn publish(&mut self) {
		self.armed = false;
	}
}

impl Drop for UnpublishedGeneration {
	fn drop(&mut self) {
		if !self.armed {
			return;
		}
		for directory in &self.directories {
			drop(fs::remove_dir_all(directory));
		}
	}
}

/// Removes UUID generation directories that are not reachable from the
/// recovered file catalog.
pub(super) fn cleanup_orphan_generations(
	root: &Path,
	catalog: &FileCatalog,
) -> FerrosGrynnResult<()> {
	let reachable = generation_directories(root, catalog)?;
	cleanup_generation_children(&root.join("atoms"), &reachable)?;
	let graphs = root.join("graphs");
	if !graphs.exists() {
		return Ok(());
	}
	for entry in
		fs::read_dir(&graphs).map_err(|source| io_failure("read graph directories", source))?
	{
		let entry = entry.map_err(|source| io_failure("read graph directory entry", source))?;
		if entry
			.file_type()
			.map_err(|source| io_failure("inspect graph directory entry", source))?
			.is_dir()
		{
			cleanup_generation_children(&entry.path(), &reachable)?;
		}
	}
	Ok(())
}

/// Derives the exact reachable generation directories from the catalog.
fn generation_directories(
	root: &Path,
	catalog: &FileCatalog,
) -> FerrosGrynnResult<BTreeSet<PathBuf>> {
	let mut directories = BTreeSet::new();
	for file in &catalog.files {
		if file.file_id.kind() == Some(DataFileKind::SystemCatalog) {
			continue;
		}
		let components = file.path.as_str().split('/').collect::<Vec<_>>();
		let relative = match components.as_slice() {
			["atoms", generation, ..] if generation.parse::<Uuid>().is_ok() => {
				PathBuf::from("atoms").join(generation)
			}
			["graphs", graph, generation, ..]
				if graph.len() == 16
					&& graph.bytes().all(|byte| byte.is_ascii_hexdigit())
					&& generation.parse::<Uuid>().is_ok() =>
			{
				PathBuf::from("graphs").join(graph).join(generation)
			}
			_ => return Err(invalid("file catalog contains an invalid generation path")),
		};
		directories.insert(root.join(relative));
	}
	Ok(directories)
}

/// Removes UUID-named child directories absent from the reachable set.
fn cleanup_generation_children(
	parent: &Path,
	reachable: &BTreeSet<PathBuf>,
) -> FerrosGrynnResult<()> {
	if !parent.exists() {
		return Ok(());
	}
	for entry in
		fs::read_dir(parent).map_err(|source| io_failure("read generation directory", source))?
	{
		let entry = entry.map_err(|source| io_failure("read generation entry", source))?;
		let file_type =
			entry.file_type().map_err(|source| io_failure("inspect generation entry", source))?;
		if !file_type.is_dir() || file_type.is_symlink() {
			continue;
		}
		let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
			continue;
		};
		if name.parse::<Uuid>().is_err() {
			continue;
		}
		let path = entry.path();
		if !reachable.contains(&path) {
			fs::remove_dir_all(&path)
				.map_err(|source| io_failure("remove orphan generation", source))?;
		}
	}
	Ok(())
}

/// Wraps a filesystem failure in the project storage status.
fn io_failure(operation: impl Into<String>, source: std::io::Error) -> FerrosGrynnError {
	FerrosGrynnError::storage_io_failure(operation).with_technical_source(source)
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::{
		datastore::DataSpaceId,
		storage::{
			catalog::{CatalogPath, FileCatalogEntry},
			file_registry::DataFileRole,
			types::{DataFileId, DataFileKind},
		},
	};

	#[test]
	fn startup_cleanup_removes_only_unreachable_uuid_generations() -> FerrosGrynnResult<()> {
		let root = std::env::temp_dir().join(format!("fg-gc-{}", Uuid::now_v7()));
		let reachable = Uuid::now_v7().to_string();
		let orphan = Uuid::now_v7().to_string();
		let reachable_path = root.join("atoms").join(&reachable);
		let orphan_path = root.join("atoms").join(&orphan);
		let unrelated = root.join("atoms").join("keep-me");
		fs::create_dir_all(&reachable_path)
			.map_err(|source| io_failure("create reachable fixture", source))?;
		fs::create_dir_all(&orphan_path)
			.map_err(|source| io_failure("create orphan fixture", source))?;
		fs::create_dir_all(&unrelated)
			.map_err(|source| io_failure("create unrelated fixture", source))?;
		let mut catalog = FileCatalog::bootstrap()?;
		catalog.files.push(FileCatalogEntry {
			space_id: DataSpaceId::Database,
			file_id: DataFileId::new(1, DataFileKind::AtomsHot).unwrap(),
			companion_of: None,
			role: DataFileRole::WalBackfilled,
			path: CatalogPath::parse(&format!("atoms/{reachable}/atoms_hot.fgb"))?,
		});

		cleanup_orphan_generations(&root, &catalog)?;
		assert!(reachable_path.is_dir());
		assert!(!orphan_path.exists());
		assert!(unrelated.is_dir());
		drop(fs::remove_dir_all(root));
		Ok(())
	}

	#[test]
	fn unpublished_generation_drop_removes_every_owned_directory() -> FerrosGrynnResult<()> {
		let root = std::env::temp_dir().join(format!("fg-import-drop-{}", Uuid::now_v7()));
		let first = root.join("atoms").join(Uuid::now_v7().to_string());
		let second = root.join("graphs").join("0000000000000001").join(Uuid::now_v7().to_string());
		fs::create_dir_all(&first)
			.map_err(|source| io_failure("create first unpublished generation", source))?;
		fs::create_dir_all(&second)
			.map_err(|source| io_failure("create second unpublished generation", source))?;
		let generation = UnpublishedGeneration {
			directories: vec![first.clone(), second.clone()],
			armed: true,
		};
		drop(generation);

		assert!(!first.exists());
		assert!(!second.exists());
		drop(fs::remove_dir_all(root));
		Ok(())
	}
}
