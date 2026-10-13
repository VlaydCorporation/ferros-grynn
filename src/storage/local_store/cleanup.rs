//! Removal of unpublished UUID generations left by failed imports or crashes.

use std::{
	collections::{BTreeMap, BTreeSet},
	fs,
	path::{Path, PathBuf},
	sync::Weak,
};

use super::layout::invalid;
use crate::{
	datastore::DatabaseRoot,
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	storage::{
		catalog::FileCatalog,
		file_registry::{FileRegistry, RegisteredDataFile},
		types::{DataFileId, DataFileKind},
	},
	values::Uuid,
};

/// Individual append-only file whose catalog reference was durably removed.
pub(super) struct RetiredChangefeedFile {
	/// Exact logical file identity used to test old binding-map pins.
	file: DataFileId,
	/// Canonical catalog path below the database directory.
	path: PathBuf,
}

impl RetiredChangefeedFile {
	/// Admits complete removal metadata before the WAL publication boundary.
	pub(super) fn between(
		root: &Path,
		previous: &FileCatalog,
		current: &FileCatalog,
	) -> FerrosGrynnResult<Vec<Self>> {
		let missing = |entry: &&crate::storage::catalog::FileCatalogEntry| {
			matches!(
				entry.file_id.kind(),
				Some(DataFileKind::Changefeed | DataFileKind::EventOutbox)
			) && current
				.files
				.binary_search_by_key(&(entry.space_id, entry.file_id), |candidate| {
					(candidate.space_id, candidate.file_id)
				})
				.is_err()
		};
		let count = previous.files.iter().filter(missing).count();
		let bytes =
			count.checked_mul(2048).ok_or_else(|| invalid("feed retirement metadata overflows"))?;
		let maximum = crate::storage::external_sort_run_budget().min(256 * 1024 * 1024);
		if bytes > maximum {
			return Err(FerrosGrynnError::storage_decode_limit_exceeded(
				"feed retirement metadata",
				bytes as u64,
				maximum as u64,
			));
		}
		let mut result = Vec::new();
		result
			.try_reserve_exact(count)
			.map_err(|_error| invalid("feed retirement allocation failed"))?;
		for entry in previous.files.iter().filter(missing) {
			result.push(Self {
				file: entry.file_id,
				path: entry.path.resolve(root),
			});
		}
		Ok(result)
	}

	/// Deletes only after every earlier binding map releases this file.
	pub(super) fn sweep(
		&self,
		pinned: &[Weak<BTreeMap<DataFileId, std::sync::Arc<RegisteredDataFile>>>],
	) -> FerrosGrynnResult<bool> {
		if pinned
			.iter()
			.any(|map| map.upgrade().is_some_and(|files| files.contains_key(&self.file)))
		{
			return Ok(false);
		}
		let metadata = match fs::symlink_metadata(&self.path) {
			Ok(metadata) => metadata,
			Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(true),
			Err(error) => return Err(io_failure("inspect retired feed", error)),
		};
		if !metadata.is_file() || metadata.file_type().is_symlink() {
			return Err(invalid("retired feed path is not a physical file"));
		}
		fs::remove_file(&self.path).map_err(|error| io_failure("remove retired feed", error))?;
		Ok(true)
	}
}

/// One catalog generation that is no longer current but may still be pinned
/// by an exact physical query snapshot.
pub(super) struct RetiredGeneration {
	/// UUID directories reachable only from the replaced catalog.
	directories: Vec<PathBuf>,
	/// Non-owning markers for handles from the replaced registry.
	markers: Vec<Weak<RegisteredDataFile>>,
}

impl RetiredGeneration {
	/// Accounts pinned retirement metadata before admission of another
	/// generation. Paths and file markers remain owned until cleanup.
	pub(super) fn heap_bytes(&self) -> FerrosGrynnResult<usize> {
		let fixed = self
			.directories
			.capacity()
			.checked_mul(std::mem::size_of::<PathBuf>())
			.and_then(|bytes| {
				bytes.checked_add(
					self.markers
						.capacity()
						.checked_mul(std::mem::size_of::<Weak<RegisteredDataFile>>())?,
				)
			})
			.ok_or_else(|| invalid("generation retirement metadata overflows"))?;
		self.directories.iter().try_fold(fixed, |bytes, path| {
			bytes
				.checked_add(path.capacity())
				.ok_or_else(|| invalid("generation retirement paths overflow"))
		})
	}

	/// Captures the directories retired by one durable catalog replacement.
	pub(super) fn between(
		root: &Path,
		previous: &FileCatalog,
		current: &FileCatalog,
		registry: &FileRegistry,
	) -> FerrosGrynnResult<Option<Self>> {
		let previous = generation_directories(root, previous)?;
		let current = generation_directories(root, current)?;
		let directories = previous.difference(&current).cloned().collect::<Vec<_>>();
		if directories.is_empty() {
			return Ok(None);
		}
		let markers = registry.directory_markers(&directories);
		Ok(Some(Self {
			directories,
			markers,
		}))
	}

	/// Returns the number of directories awaiting removal.
	pub(super) const fn directory_count(&self) -> usize {
		self.directories.len()
	}

	/// Removes every unpinned directory and returns the number of failed
	/// removals. A failure remains pending for the next maintenance sweep.
	pub(super) fn sweep(&mut self) -> u64 {
		if self.markers.iter().any(|marker| marker.strong_count() != 0) {
			return 0;
		}
		self.markers.clear();
		let mut failures = 0_u64;
		self.directories.retain(|directory| match remove_generation_directory(directory) {
			Ok(()) => false,
			Err(_error) => {
				failures = failures.saturating_add(1);
				true
			}
		});
		failures
	}

	/// Reports whether every retired directory has been removed.
	pub(super) fn is_empty(&self) -> bool {
		self.directories.is_empty()
	}
}

/// Deletes a generation on unwind or error until its catalog commit is known
/// to be durable.
pub(super) struct UnpublishedGeneration {
	/// AtomStore and graph-generation directories owned by the pending commit.
	directories: Vec<PathBuf>,
	/// Whether `Drop` must remove the directories.
	armed: bool,
}

impl UnpublishedGeneration {
	/// Arms cleanup for one independent immutable mapping generation.
	pub(super) fn for_mapping(root: &Path, directory: uuid::Uuid) -> Self {
		Self {
			directories: vec![root.join("atoms").join(directory.to_string())],
			armed: true,
		}
	}

	/// Arms cleanup before the first streamed side-file is created.
	pub(super) fn for_database(root: &Path, database: &DatabaseRoot, generation: &str) -> Self {
		let mut directories = vec![root.join("atoms").join(generation)];
		directories.extend(database.schema.all_graphs().map(|graph| {
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

	/// Keeps files after an uncertain WAL append/fsync outcome. Recovered
	/// catalog reachability, rather than an error return, decides their
	/// cleanup.
	pub(super) fn preserve_for_recovery(&mut self) {
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

/// Owns only newly created feed files before the WAL call. Appended old files
/// are never removed; their old committed ceilings remain authoritative.
pub(super) struct PendingChangefeedFiles {
	/// Canonical new segment paths belonging to this private publication.
	paths: Vec<PathBuf>,
	/// Known failures clean up; uncertain WAL outcomes defer to recovery.
	armed: bool,
}

impl PendingChangefeedFiles {
	/// Admits cleanup metadata before any new segment file is created.
	pub(super) fn new(root: &Path, images: &[super::layout::FileImage]) -> FerrosGrynnResult<Self> {
		let new_file = |image: &&super::layout::FileImage| {
			matches!(
				image.entry.file_id.kind(),
				Some(DataFileKind::Changefeed | DataFileKind::EventOutbox)
			) && image.page_numbers.get(1).is_some_and(|number| number.get() == 1)
		};
		let count = images.iter().filter(new_file).count();
		let bytes = root
			.as_os_str()
			.len()
			.checked_add(256)
			.and_then(|bytes| bytes.checked_mul(3))
			.and_then(|bytes| bytes.checked_mul(count))
			.ok_or_else(|| invalid("pending feed cleanup metadata overflows"))?;
		if bytes > 16 * 1024 * 1024 {
			return Err(FerrosGrynnError::storage_decode_limit_exceeded(
				"pending feed cleanup bytes",
				bytes as u64,
				16 * 1024 * 1024,
			));
		}
		let paths =
			images.iter().filter(new_file).map(|image| image.entry.path.resolve(root)).collect();
		Ok(Self {
			paths,
			armed: true,
		})
	}

	/// Once append/fsync begins, only recovery may prove a file unreachable.
	pub(super) fn preserve_for_recovery(&mut self) {
		self.armed = false;
	}
}

impl Drop for PendingChangefeedFiles {
	fn drop(&mut self) {
		if self.armed {
			for path in &self.paths {
				drop(fs::remove_file(path));
			}
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

/// Removes immutable quarantine files that are not reachable from the
/// recovered file catalog.
pub(super) fn cleanup_orphan_quarantines(
	root: &Path,
	catalog: &FileCatalog,
) -> FerrosGrynnResult<()> {
	let directory = root.join("admin").join("quarantine");
	if !directory.exists() {
		return Ok(());
	}
	let reachable = catalog
		.files
		.iter()
		.filter(|entry| entry.file_id.kind() == Some(DataFileKind::ImportQuarantine))
		.map(|entry| entry.path.resolve(root))
		.collect::<BTreeSet<_>>();
	for entry in fs::read_dir(&directory)
		.map_err(|source| io_failure("read quarantine directory", source))?
	{
		let entry = entry.map_err(|source| io_failure("read quarantine entry", source))?;
		let file_type =
			entry.file_type().map_err(|source| io_failure("inspect quarantine entry", source))?;
		if !file_type.is_file() || file_type.is_symlink() {
			continue;
		}
		let path = entry.path();
		let canonical_name = path
			.file_name()
			.and_then(|name| name.to_str())
			.and_then(|name| name.strip_suffix(".fgq"))
			.is_some_and(|id| id.parse::<Uuid>().is_ok());
		if canonical_name && !reachable.contains(&path) {
			match fs::remove_file(&path) {
				Ok(()) => {}
				Err(source) if source.kind() == std::io::ErrorKind::NotFound => {}
				Err(source) => return Err(io_failure("remove orphan quarantine", source)),
			}
		}
	}
	Ok(())
}

/// Removes only canonical feed files proven unreachable by the recovered
/// catalog. This runs after recovery establishes the WAL commit outcome.
pub(super) fn cleanup_orphan_changefeeds(
	root: &Path,
	catalog: &FileCatalog,
) -> FerrosGrynnResult<()> {
	cleanup_orphan_history(root, catalog, DataFileKind::Changefeed, "changefeed", ".fcf")
}

/// Removes unpublished invocation journals only after strict recovery succeeds.
pub(super) fn cleanup_orphan_events(root: &Path, catalog: &FileCatalog) -> FerrosGrynnResult<()> {
	cleanup_orphan_history(root, catalog, DataFileKind::EventOutbox, "events", ".feo")
}

/// The shared physical-directory checks protect both append-only namespaces.
fn cleanup_orphan_history(
	root: &Path,
	catalog: &FileCatalog,
	file_kind: DataFileKind,
	name: &str,
	suffix: &str,
) -> FerrosGrynnResult<()> {
	let directory = root.join(name);
	let metadata = match fs::symlink_metadata(&directory) {
		Ok(metadata) => metadata,
		Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
		Err(error) => return Err(io_failure("inspect feed directory", error)),
	};
	if !metadata.is_dir() || metadata.file_type().is_symlink() {
		return Err(invalid("feed directory is not a physical directory"));
	}
	let reachable = catalog
		.files
		.iter()
		.filter(|entry| entry.file_id.kind() == Some(file_kind))
		.map(|entry| entry.path.resolve(root))
		.collect::<BTreeSet<_>>();
	for entry in
		fs::read_dir(&directory).map_err(|error| io_failure("read feed directories", error))?
	{
		let entry = entry.map_err(|error| io_failure("read feed directory", error))?;
		let kind =
			entry.file_type().map_err(|error| io_failure("inspect feed directory", error))?;
		let canonical = entry.file_name().to_str().is_some_and(canonical_uuid);
		if !kind.is_dir() || kind.is_symlink() || !canonical {
			continue;
		}
		for file in
			fs::read_dir(entry.path()).map_err(|error| io_failure("read feed segments", error))?
		{
			let file = file.map_err(|error| io_failure("read feed segment", error))?;
			let kind =
				file.file_type().map_err(|error| io_failure("inspect feed segment", error))?;
			let canonical = file
				.file_name()
				.to_str()
				.and_then(|name| name.strip_suffix(suffix))
				.is_some_and(canonical_uuid);
			if !kind.is_file()
				|| kind.is_symlink()
				|| !canonical
				|| reachable.contains(&file.path())
			{
				continue;
			}
			match fs::remove_file(file.path()) {
				Ok(()) => {}
				Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
				Err(error) => return Err(io_failure("remove orphan feed segment", error)),
			}
		}
	}
	Ok(())
}

/// Recognizes exactly the non-nil lowercase UUID spelling used by the catalog.
fn canonical_uuid(name: &str) -> bool {
	name.parse::<uuid::Uuid>().is_ok_and(|id| !id.is_nil() && id.to_string() == name)
}

/// Removes transient query runs left by an interrupted process before the
/// database accepts sessions.
pub(super) fn cleanup_query_runs(root: &Path) -> FerrosGrynnResult<()> {
	let path = root.join("tmp").join("query");
	match fs::remove_dir_all(path) {
		Ok(()) => Ok(()),
		Err(source) if source.kind() == std::io::ErrorKind::NotFound => Ok(()),
		Err(source) => Err(io_failure("remove stale query-run scratch", source)),
	}
}

/// Derives the exact reachable generation directories from the catalog.
fn generation_directories(
	root: &Path,
	catalog: &FileCatalog,
) -> FerrosGrynnResult<BTreeSet<PathBuf>> {
	let mut directories = BTreeSet::new();
	for file in &catalog.files {
		if file.file_id.kind() == Some(DataFileKind::SystemCatalog)
			|| matches!(
				file.file_id.kind(),
				Some(
					DataFileKind::JobCatalog
						| DataFileKind::ImportQuarantine
						| DataFileKind::Changefeed
						| DataFileKind::EventOutbox
				)
			) {
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
			remove_generation_directory(&path)?;
		}
	}
	Ok(())
}

/// Removes one generation directory. Absence is success so a partially
/// completed Windows removal can be retried safely.
fn remove_generation_directory(path: &Path) -> FerrosGrynnResult<()> {
	#[cfg(test)]
	if take_injected_cleanup_failure() {
		return Err(io_failure(
			"remove retired generation",
			std::io::Error::other("injected retired-generation cleanup failure"),
		));
	}
	match fs::remove_dir_all(path) {
		Ok(()) => Ok(()),
		Err(source) if source.kind() == std::io::ErrorKind::NotFound => Ok(()),
		Err(source) => Err(io_failure("remove retired generation", source)),
	}
}

#[cfg(test)]
thread_local! {
	static INJECTED_CLEANUP_FAILURES: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// Injects deterministic post-publication cleanup failures for local-store
/// lifecycle tests.
#[cfg(test)]
pub(super) fn inject_cleanup_failures(count: u64) {
	INJECTED_CLEANUP_FAILURES.set(count);
}

/// Consumes one deterministic cleanup failure.
#[cfg(test)]
fn take_injected_cleanup_failure() -> bool {
	INJECTED_CLEANUP_FAILURES.with(|remaining| {
		let count = remaining.get();
		if count == 0 {
			false
		} else {
			remaining.set(count - 1);
			true
		}
	})
}

/// Wraps a filesystem failure in the project storage status.
fn io_failure(operation: impl Into<String>, source: std::io::Error) -> FerrosGrynnError {
	FerrosGrynnError::storage_io_failure(operation).with_technical_source(source)
}

#[cfg(test)]
mod query_run_tests {
	use super::*;

	#[test]
	fn startup_cleanup_removes_orphan_query_runs_idempotently() {
		let root = std::env::temp_dir()
			.join(format!("fg-query-cleanup-{}", crate::values::Uuid::now_v7()));
		let session = root.join("tmp").join("query").join("orphan");
		fs::create_dir_all(&session).unwrap();
		fs::write(session.join("0000000000000000.run"), b"orphan").unwrap();

		cleanup_query_runs(&root).unwrap();
		assert!(!root.join("tmp").join("query").exists());
		cleanup_query_runs(&root).unwrap();
		drop(fs::remove_dir_all(root));
	}
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
	fn startup_cleanup_removes_only_unreachable_quarantines() -> FerrosGrynnResult<()> {
		let root = std::env::temp_dir().join(format!("fg-quarantine-cleanup-{}", Uuid::now_v7()));
		let directory = root.join("admin").join("quarantine");
		fs::create_dir_all(&directory)
			.map_err(|source| io_failure("create quarantine fixture directory", source))?;
		let reachable = Uuid::now_v7();
		let orphan = Uuid::now_v7();
		let reachable_path = directory.join(format!("{reachable}.fgq"));
		let orphan_path = directory.join(format!("{orphan}.fgq"));
		let unrelated = directory.join("keep-me");
		for path in [&reachable_path, &orphan_path, &unrelated] {
			fs::write(path, b"fixture")
				.map_err(|source| io_failure("write quarantine fixture", source))?;
		}
		let mut catalog = FileCatalog::bootstrap()?;
		catalog.files.push(FileCatalogEntry {
			space_id: DataSpaceId::Database,
			file_id: DataFileId::new(1, DataFileKind::ImportQuarantine)
				.ok_or_else(|| invalid("test quarantine identity is invalid"))?,
			companion_of: None,
			role: DataFileRole::AuxiliaryImmutable,
			path: CatalogPath::parse(&format!("admin/quarantine/{reachable}.fgq"))?,
		});

		cleanup_orphan_quarantines(&root, &catalog)?;
		assert!(reachable_path.is_file());
		assert!(!orphan_path.exists());
		assert!(unrelated.is_file());
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
