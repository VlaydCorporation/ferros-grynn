//! Removal of unpublished UUID generations left by failed imports or crashes.

use std::{
	collections::{BTreeMap, BTreeSet},
	fs,
	path::{Path, PathBuf},
	sync::{Arc, Weak},
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
		remove_retired_file(&self.path)?;
		Ok(true)
	}
}

/// One catalog generation that is no longer current but may still be pinned
/// by an exact physical query snapshot.
pub(super) struct RetiredGeneration {
	/// UUID directories reachable only from the replaced catalog.
	directories: Vec<PathBuf>,
	/// Selected owned files whose directory still holds other live bindings.
	files: Vec<PathBuf>,
	/// Non-owning markers for handles from the replaced registry.
	markers: Vec<crate::storage::file_registry::RetiredFileMarker>,
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
				bytes
					.checked_add(self.files.capacity().checked_mul(std::mem::size_of::<PathBuf>())?)
			})
			.and_then(|bytes| {
				bytes.checked_add(self.markers.capacity().checked_mul(std::mem::size_of::<
					crate::storage::file_registry::RetiredFileMarker,
				>())?)
			})
			.ok_or_else(|| invalid("generation retirement metadata overflows"))?;
		self.directories.iter().chain(&self.files).try_fold(fixed, |bytes, path| {
			bytes
				.checked_add(path.capacity())
				.ok_or_else(|| invalid("generation retirement paths overflow"))
		})
	}

	/// Captures retired directories, selected indexes and snapshot markers.
	pub(super) fn between(
		root: &Path,
		previous: &FileCatalog,
		current: &FileCatalog,
		registry: &FileRegistry,
	) -> FerrosGrynnResult<Option<Self>> {
		let retained = current
			.files
			.iter()
			.map(|entry| (entry.space_id, entry.file_id))
			.collect::<BTreeSet<_>>();
		let markers = registry.markers_except(&retained);
		let previous_directories = generation_directories(root, previous)?;
		let current_directories = generation_directories(root, current)?;
		let files = previous
			.files
			.iter()
			.filter_map(|entry| {
				if retained.contains(&(entry.space_id, entry.file_id)) {
					return None;
				}
				let path = entry.path.resolve(root);
				let name = path.file_name()?.to_str()?;
				let parent = path.parent()?;
				let scalar = entry.file_id.kind() == Some(DataFileKind::Index)
					&& is_selected_index_name(
						name,
						matches!(entry.space_id, crate::datastore::DataSpaceId::Graph(_)),
					)
					&& parent.file_name().is_some_and(|name| name == "idx")
					&& parent
						.parent()
						.is_some_and(|directory| current_directories.contains(directory));
				let root_index = entry.file_id.kind() == Some(DataFileKind::Index)
					&& is_root_index_name(
						name,
						matches!(entry.space_id, crate::datastore::DataSpaceId::Graph(_)),
					)
					&& match entry.space_id {
						crate::datastore::DataSpaceId::Database => {
							parent.parent() == Some(root.join("atoms").as_path())
						}
						crate::datastore::DataSpaceId::Graph(graph) => {
							parent.parent()
								== Some(
									root.join("graphs")
										.join(format!("{:016x}", graph.get()))
										.as_path(),
								)
						}
					}
					&& current_directories.contains(parent);
				let metadata = matches!(entry.space_id, crate::datastore::DataSpaceId::Graph(_))
					&& match entry.file_id.kind() {
						Some(DataFileKind::Labels) => {
							name == "labels.fgb" && current_directories.contains(parent)
						}
						Some(DataFileKind::Statistics | DataFileKind::LabelStatistics) => {
							matches!(
								(entry.file_id.kind(), name),
								(Some(DataFileKind::Statistics), "graph_stats.fgb")
									| (Some(DataFileKind::LabelStatistics), "label_stats.fgb")
							) && parent.file_name().is_some_and(|name| name == "stats")
								&& parent.parent().is_some_and(|directory| {
									current_directories.contains(directory)
								})
						}
						_ => false,
					};
				(scalar || root_index || metadata).then_some(path)
			})
			.collect::<Vec<_>>();
		let directories =
			previous_directories.difference(&current_directories).cloned().collect::<Vec<_>>();
		if directories.is_empty() && files.is_empty() && markers.is_empty() {
			return Ok(None);
		}
		Ok(Some(Self {
			directories,
			files,
			markers,
		}))
	}

	/// Returns the number of directories awaiting removal.
	pub(super) const fn directory_count(&self) -> usize {
		self.directories.len()
	}

	/// Pins trusted retired files while checkpoint moves their snapshot pages
	/// from WAL into home; weak markers alone protect only against unlink.
	pub(super) fn pinned_files(
		&self,
	) -> impl Iterator<
		Item = ((crate::datastore::DataSpaceId, DataFileId), std::sync::Arc<RegisteredDataFile>),
	> + '_ {
		self.markers.iter().filter_map(|(key, marker)| marker.upgrade().map(|file| (*key, file)))
	}

	/// Removes every unpinned file/directory and returns the number of failed
	/// removals. A failure remains pending for the next maintenance sweep.
	pub(super) fn sweep(&mut self, pinned: &[Arc<RegisteredDataFile>]) -> u64 {
		self.markers.retain(|(_key, marker)| marker.strong_count() != 0);
		let mut failures = 0_u64;
		self.files.retain(|path| {
			if pinned.iter().any(|file| file.path() == path) {
				return true;
			}
			match remove_retired_file(path) {
				Ok(()) => false,
				Err(_error) => {
					failures = failures.saturating_add(1);
					true
				}
			}
		});
		self.directories.retain(|directory| {
			// A retained artifact can delay directory retirement until a later
			// batch; readers from every earlier batch still protect its files.
			if pinned.iter().any(|file| file.path().starts_with(directory)) {
				return true;
			}
			match remove_generation_directory(directory) {
				Ok(()) => false,
				Err(_error) => {
					failures = failures.saturating_add(1);
					true
				}
			}
		});
		failures
	}

	/// Reports whether every retired path and snapshot marker was released.
	pub(super) fn is_empty(&self) -> bool {
		self.directories.is_empty() && self.files.is_empty() && self.markers.is_empty()
	}
}

/// Collects only handle pins, without cloning paths or resurrecting dead files.
pub(super) fn pin_retired_files(
	generations: &[RetiredGeneration],
) -> FerrosGrynnResult<Vec<Arc<RegisteredDataFile>>> {
	let count = generations.iter().try_fold(0_usize, |count, generation| {
		count
			.checked_add(generation.markers.len())
			.ok_or_else(|| invalid("retired file pin count overflows"))
	})?;
	let mut pinned = Vec::new();
	pinned
		.try_reserve_exact(count)
		.map_err(|_error| invalid("retired file pin allocation failed"))?;
	pinned.extend(
		generations.iter().flat_map(RetiredGeneration::pinned_files).map(|(_key, file)| file),
	);
	Ok(pinned)
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
	/// Runs cleanup for this `UnpublishedGeneration` before its fields are
	/// released.
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
	/// Runs cleanup for this `PendingChangefeedFiles` before its fields are
	/// released.
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
	let paths = catalog.files.iter().map(|entry| entry.path.resolve(root)).collect::<BTreeSet<_>>();
	for directory in &reachable {
		let atoms = directory.parent() == Some(root.join("atoms").as_path());
		cleanup_orphan_index_files(&directory.join("idx"), &paths, |name| {
			is_selected_index_name(name, !atoms)
		})?;
		if atoms {
			cleanup_orphan_index_files(directory, &paths, |name| is_root_index_name(name, false))?;
		} else {
			cleanup_orphan_index_files(directory, &paths, |name| is_root_index_name(name, true))?;
			cleanup_orphan_index_files(directory, &paths, |name| name == "labels.fgb")?;
			cleanup_orphan_index_files(&directory.join("stats"), &paths, |name| {
				matches!(name, "graph_stats.fgb" | "label_stats.fgb")
			})?;
		}
	}
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

/// Root membership names are distinct from scalar/class arrays below `idx`.
fn is_root_index_name(name: &str, graph: bool) -> bool {
	if graph {
		name == "membership.fgidx"
	} else {
		matches!(name, "primary.fgidx" | "memberships.fgidx")
	}
}

/// Recognizes owned basenames; only graph spaces own label/class arrays.
fn is_selected_index_name(name: &str, graph: bool) -> bool {
	if graph && name == "ee_endpoint.fgidx" {
		return true;
	}
	let Some(stem) = name.strip_suffix(".fgidx") else {
		return false;
	};
	if let Some(ordinal) = stem.strip_prefix("std_").or_else(|| stem.strip_prefix("uniq_")) {
		return ordinal.len() >= 4
			&& (ordinal.len() == 4 || !ordinal.starts_with('0'))
			&& ordinal.bytes().all(|byte| byte.is_ascii_digit())
			&& ordinal.parse::<u32>().is_ok();
	}
	if graph && let Some(id) = stem.strip_prefix("cls_") {
		return !id.starts_with('0')
			&& id.parse::<u32>().is_ok_and(|id| id != 0)
			&& id.bytes().all(|byte| byte.is_ascii_digit());
	}
	if graph
		&& let Some(id) = stem.strip_prefix("lbl_node_").or_else(|| stem.strip_prefix("lbl_edge_"))
	{
		return !id.is_empty()
			&& (id.len() == 1 || !id.starts_with('0'))
			&& id.bytes().all(|byte| byte.is_ascii_digit())
			&& id.parse::<u32>().is_ok();
	}
	false
}

/// Recovery reachability also governs selected index files in retained
/// directories; a crash may interrupt their post-publication retirement.
fn cleanup_orphan_index_files(
	directory: &Path,
	reachable: &BTreeSet<PathBuf>,
	owned_name: impl Fn(&str) -> bool,
) -> FerrosGrynnResult<()> {
	let metadata = match fs::symlink_metadata(directory) {
		Ok(metadata) => metadata,
		Err(source) if source.kind() == std::io::ErrorKind::NotFound => return Ok(()),
		Err(source) => return Err(io_failure("inspect scalar index directory", source)),
	};
	if !metadata.is_dir() || metadata.file_type().is_symlink() {
		return Ok(());
	}
	for entry in fs::read_dir(directory)
		.map_err(|source| io_failure("read scalar index directory", source))?
	{
		let entry = entry.map_err(|source| io_failure("read scalar index entry", source))?;
		let kind =
			entry.file_type().map_err(|source| io_failure("inspect scalar index entry", source))?;
		if kind.is_file()
			&& !kind.is_symlink()
			&& entry.file_name().to_str().is_some_and(&owned_name)
			&& !reachable.contains(&entry.path())
		{
			remove_retired_file(&entry.path())?;
		}
	}
	Ok(())
}

/// Unlinks a trusted retired regular file; absence makes cleanup idempotent.
fn remove_retired_file(path: &Path) -> FerrosGrynnResult<()> {
	#[cfg(test)]
	if take_injected_cleanup_failure() {
		return Err(io_failure(
			"remove retired file",
			std::io::Error::other("injected retired-file cleanup failure"),
		));
	}
	let metadata = match fs::symlink_metadata(path) {
		Ok(metadata) => metadata,
		Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
		Err(error) => return Err(io_failure("inspect retired file", error)),
	};
	if !metadata.is_file() || metadata.file_type().is_symlink() {
		return Err(invalid("retired path is not a physical file"));
	}
	fs::remove_file(path).map_err(|error| io_failure("remove retired file", error))
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
	/// Cleanup failures still to inject on this test thread after publication succeeds.
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
/// Regression tests for cleanup behavior.
mod query_run_tests {
	use super::*;
	#[cfg(test)]
	use crate::test_assertions::AssertResult as _;

	/// Verifies startup cleanup removes orphan query runs idempotently.
	#[test]
	fn startup_cleanup_removes_orphan_query_runs_idempotently() {
		let root = std::env::temp_dir()
			.join(format!("fg-query-cleanup-{}", crate::values::Uuid::now_v7()));
		let session = root.join("tmp").join("query").join("orphan");
		fs::create_dir_all(&session).assert_ok();
		fs::write(session.join("0000000000000000.run"), b"orphan").assert_ok();

		cleanup_query_runs(&root).assert_ok();
		assert!(!root.join("tmp").join("query").exists());
		cleanup_query_runs(&root).assert_ok();
		drop(fs::remove_dir_all(root));
	}
}

#[cfg(test)]
/// Regression tests for cleanup behavior.
mod tests {
	use super::*;
	#[cfg(test)]
	use crate::test_assertions::AssertOption as _;
	use crate::{
		datastore::DataSpaceId,
		storage::{
			catalog::{CatalogPath, FileCatalogEntry},
			file_registry::DataFileRole,
			types::{DataFileId, DataFileKind},
		},
	};

	/// Only the exact engine-owned endpoint basename belongs to graph cleanup.
	#[test]
	fn selected_endpoint_cleanup_name_is_exact_and_graph_only() {
		assert!(is_selected_index_name("ee_endpoint.fgidx", true));
		assert!(!is_selected_index_name("ee_endpoint.fgidx", false));
		for name in
			["ee_endpoint.fgidx.bak", "ee_endpoint_1.fgidx", "EE_endpoint.fgidx", "ee_endpoint.fgb"]
		{
			assert!(!is_selected_index_name(name, true));
		}
	}

	/// Only canonical bounded decimal label identities belong to cleanup.
	#[test]
	fn selected_label_cleanup_names_are_exact() {
		for name in ["lbl_node_0.fgidx", "lbl_edge_42.fgidx", "lbl_node_4294967295.fgidx"] {
			assert!(is_selected_index_name(name, true));
			assert!(!is_selected_index_name(name, false));
		}
		for name in [
			"lbl_node_.fgidx",
			"lbl_node_00.fgidx",
			"lbl_node_-1.fgidx",
			"lbl_edge_4294967296.fgidx",
			"lbl_node_1.fgidx.bak",
			"lbl_other_1.fgidx",
		] {
			assert!(!is_selected_index_name(name, true));
		}
	}

	/// Class arrays use positive canonical ClassId values, only in graph
	/// spaces.
	#[test]
	fn selected_class_cleanup_names_are_exact() {
		for name in ["cls_1.fgidx", "cls_4294967295.fgidx"] {
			assert!(is_selected_index_name(name, true));
			assert!(!is_selected_index_name(name, false));
		}
		for name in [
			"cls_0.fgidx",
			"cls_01.fgidx",
			"cls_+1.fgidx",
			"cls_4294967296.fgidx",
			"cls_.fgidx",
			"cls_1.fgidx.bak",
		] {
			assert!(!is_selected_index_name(name, true));
		}
	}

	/// Runtime and recovery share exact scoped membership/class ownership.
	#[test]
	fn selected_membership_cleanup_checks_scope_kind_and_reachability() {
		use crate::{storage::direct::DirectFileOpenMode, test_assertions::AssertResult as _};
		let root =
			std::env::temp_dir().join(format!("fg-membership-retirement-{}", Uuid::now_v7()));
		let registry = FileRegistry::open(
			Vec::new(),
			DirectFileOpenMode::Synchronous,
			&rapidhash::RapidHashSet::default(),
		)
		.assert_ok();
		for graph in [false, true] {
			let (space, prefix) = if graph {
				(
					DataSpaceId::Graph(crate::graph::GraphId::new(1).assert_some()),
					format!("graphs/0000000000000001/{}", Uuid::now_v7()),
				)
			} else {
				(DataSpaceId::Database, format!("atoms/{}", Uuid::now_v7()))
			};
			let directory = root.join(&prefix);
			fs::create_dir_all(directory.join("idx")).assert_ok();
			for name in [
				"membership.fgidx",
				"memberships.fgidx",
				"membership.fgidx.bak",
				"idx/cls_1.fgidx",
				"idx/cls_01.fgidx",
				"idx/ee_endpoint.fgidx",
				"idx/ee_endpoint.fgidx.bak",
			] {
				fs::write(directory.join(name), b"fixture").assert_ok();
			}
			let mut retained = FileCatalog::bootstrap().assert_ok();
			retained.next_file_instance_id = 3;
			retained.files.push(FileCatalogEntry {
				space_id: space,
				file_id: DataFileId::new(1, DataFileKind::Index).assert_some(),
				companion_of: None,
				role: DataFileRole::WalBackfilled,
				path: CatalogPath::parse(&format!("{prefix}/idx/std_0000.fgidx")).assert_ok(),
			});
			for kind in [DataFileKind::Index, DataFileKind::AtomsHot] {
				for name in [
					"membership.fgidx",
					"memberships.fgidx",
					"idx/cls_1.fgidx",
					"idx/cls_01.fgidx",
					"idx/ee_endpoint.fgidx",
					"idx/ee_endpoint.fgidx.bak",
				] {
					let mut previous = retained.clone();
					previous.files.push(FileCatalogEntry {
						space_id: space,
						file_id: DataFileId::new(2, kind).assert_some(),
						companion_of: None,
						role: DataFileRole::WalBackfilled,
						path: CatalogPath::parse(&format!("{prefix}/{name}")).assert_ok(),
					});
					let result = RetiredGeneration::between(&root, &previous, &retained, &registry)
						.assert_ok();
					let owned = kind == DataFileKind::Index
						&& if graph {
							matches!(
								name,
								"membership.fgidx" | "idx/cls_1.fgidx" | "idx/ee_endpoint.fgidx"
							)
						} else {
							name == "memberships.fgidx"
						};
					if owned {
						assert_eq!(result.assert_some().files, [directory.join(name)]);
					} else {
						assert!(result.is_none(), "{graph} {kind:?} {name}");
					}
				}
			}
			for _ in 0..2 {
				cleanup_orphan_generations(&root, &retained).assert_ok();
			}
			assert_eq!(directory.join("membership.fgidx").exists(), !graph);
			assert_eq!(directory.join("memberships.fgidx").exists(), graph);
			assert_eq!(directory.join("idx/cls_1.fgidx").exists(), !graph);
			assert_eq!(directory.join("idx/ee_endpoint.fgidx").exists(), !graph);
			assert!(directory.join("idx/ee_endpoint.fgidx.bak").is_file());
			assert!(directory.join("membership.fgidx.bak").is_file());
			assert!(directory.join("idx/cls_01.fgidx").is_file());
		}
		drop(fs::remove_dir_all(root));
	}

	/// A later directory retirement honors an earlier markers-only batch.
	#[test]
	fn later_retirement_honors_earlier_file_pins() {
		use crate::{
			storage::{
				direct::{DirectFile, DirectFileOpenMode},
				file_registry::DataFileRegistration,
			},
			test_assertions::AssertResult as _,
		};
		let root = std::env::temp_dir().join(format!("fg-retirement-pins-{}", Uuid::now_v7()));
		let directory = root.join("atoms").join(Uuid::now_v7().to_string());
		fs::create_dir_all(&directory).assert_ok();
		let path = directory.join("atoms_hot.fgb");
		let native = DirectFile::create_new(&path).assert_ok();
		native.resize_pages(1).assert_ok();
		drop(native);
		let id = DataFileId::new(1, DataFileKind::AtomsHot).assert_some();
		let registry = FileRegistry::open(
			vec![DataFileRegistration {
				space_id: DataSpaceId::Database,
				file_id: id,
				role: DataFileRole::WalBackfilled,
				path,
			}],
			DirectFileOpenMode::Synchronous,
			&rapidhash::RapidHashSet::default(),
		)
		.assert_ok();
		let file = registry.resolve_file(DataSpaceId::Database, id).assert_ok();
		let early = RetiredGeneration {
			directories: Vec::new(),
			files: Vec::new(),
			markers: vec![((DataSpaceId::Database, id), Arc::downgrade(&file))],
		};
		let mut later = RetiredGeneration {
			directories: vec![directory.clone()],
			files: vec![file.path().to_path_buf()],
			markers: Vec::new(),
		};
		drop(registry);
		let pinned = pin_retired_files(&[early]).assert_ok();
		assert_eq!(later.sweep(&pinned), 0);
		assert!(directory.is_dir());
		drop(pinned);
		drop(file);
		assert_eq!(later.sweep(&[]), 0);
		assert!(later.is_empty());
		assert!(!directory.exists());
		drop(fs::remove_dir_all(root));
	}

	/// A failed individual unlink remains pending and retries without changing
	/// the already published catalog or removing another file.
	#[test]
	fn individual_file_cleanup_retries_known_failure() {
		use crate::test_assertions::AssertResult as _;
		let root = std::env::temp_dir().join(format!("fg-file-retry-{}", Uuid::now_v7()));
		fs::create_dir_all(&root).assert_ok();
		let path = root.join("std_0000.fgidx");
		fs::write(&path, b"retired").assert_ok();
		let mut retired = RetiredGeneration {
			directories: Vec::new(),
			files: vec![path.clone()],
			markers: Vec::new(),
		};
		inject_cleanup_failures(1);
		assert_eq!(retired.sweep(&[]), 1);
		assert!(path.is_file());
		assert!(!retired.is_empty());
		assert_eq!(retired.sweep(&[]), 0);
		assert!(!path.exists());
		assert!(retired.is_empty());
		drop(fs::remove_dir_all(root));
	}

	/// Only canonical unreferenced scalar files in reachable directories are
	/// owned by startup cleanup; unknown names and nested directories survive.
	#[test]
	fn startup_scalar_cleanup_preserves_referenced_and_unknown_files() {
		use crate::test_assertions::AssertResult as _;
		let root = std::env::temp_dir().join(format!("fg-scalar-orphans-{}", Uuid::now_v7()));
		let generation = Uuid::now_v7().to_string();
		let relative = format!("atoms/{generation}/idx");
		let directory = root.join(&relative);
		fs::create_dir_all(&directory).assert_ok();
		for name in [
			"std_0000.fgidx",
			"uniq_0001.fgidx",
			"point_0000000000000001.fgidx",
			"std_0002.fgidx",
			"std_00002.fgidx",
			"user.fgidx",
			"lbl_node_0.fgidx",
			"lbl_edge_1.fgidx",
		] {
			fs::write(directory.join(name), b"fixture").assert_ok();
		}
		fs::create_dir(directory.join("uniq_0003.fgidx")).assert_ok();
		let mut catalog = FileCatalog::bootstrap().assert_ok();
		catalog.next_file_instance_id = 2;
		catalog.files.push(FileCatalogEntry {
			space_id: DataSpaceId::Database,
			file_id: DataFileId::new(1, DataFileKind::Index).assert_some(),
			companion_of: None,
			role: DataFileRole::WalBackfilled,
			path: CatalogPath::parse(&format!("{relative}/std_0002.fgidx")).assert_ok(),
		});
		for _ in 0..2 {
			cleanup_orphan_generations(&root, &catalog).assert_ok();
		}
		for name in ["std_0000.fgidx", "uniq_0001.fgidx"] {
			assert!(!directory.join(name).exists());
		}
		for name in [
			"std_0002.fgidx",
			"std_00002.fgidx",
			"user.fgidx",
			"uniq_0003.fgidx",
			"point_0000000000000001.fgidx",
			"lbl_node_0.fgidx",
			"lbl_edge_1.fgidx",
		] {
			assert!(directory.join(name).exists());
		}
		drop(fs::remove_dir_all(root));
	}

	/// Graph cleanup removes only unreferenced canonical label arrays.
	#[test]
	fn startup_graph_label_cleanup_preserves_referenced_and_unknown_files() {
		use crate::test_assertions::AssertResult as _;
		let root = std::env::temp_dir().join(format!("fg-label-orphans-{}", Uuid::now_v7()));
		let relative = format!("graphs/0000000000000001/{}/idx", Uuid::now_v7());
		let directory = root.join(&relative);
		fs::create_dir_all(&directory).assert_ok();
		for name in [
			"lbl_node_0.fgidx",
			"lbl_edge_1.fgidx",
			"lbl_node_1.fgidx",
			"lbl_edge_01.fgidx",
			"lbl_node_4294967296.fgidx",
			"user.fgidx",
		] {
			fs::write(directory.join(name), b"fixture").assert_ok();
		}
		fs::create_dir(directory.join("lbl_node_2.fgidx")).assert_ok();
		let mut catalog = FileCatalog::bootstrap().assert_ok();
		catalog.next_file_instance_id = 2;
		catalog.files.push(FileCatalogEntry {
			space_id: DataSpaceId::Graph(crate::graph::GraphId::new(1).assert_some()),
			file_id: DataFileId::new(1, DataFileKind::Index).assert_some(),
			companion_of: None,
			role: DataFileRole::WalBackfilled,
			path: CatalogPath::parse(&format!("{relative}/lbl_node_1.fgidx")).assert_ok(),
		});
		for _ in 0..2 {
			cleanup_orphan_generations(&root, &catalog).assert_ok();
		}
		for name in ["lbl_node_0.fgidx", "lbl_edge_1.fgidx"] {
			assert!(!directory.join(name).exists());
		}
		for name in [
			"lbl_node_1.fgidx",
			"lbl_edge_01.fgidx",
			"lbl_node_4294967296.fgidx",
			"lbl_node_2.fgidx",
			"user.fgidx",
		] {
			assert!(directory.join(name).exists());
		}
		drop(fs::remove_dir_all(root));
	}

	/// Startup removes only canonical unreferenced graph metadata, preserving
	/// lookalikes.
	#[test]
	fn startup_namespace_metadata_cleanup_preserves_referenced_unknown_and_atom_files() {
		use crate::test_assertions::AssertResult as _;
		let root = std::env::temp_dir().join(format!("fg-namespace-orphans-{}", Uuid::now_v7()));
		let graph = format!("graphs/0000000000000001/{}", Uuid::now_v7());
		let atoms = format!("atoms/{}", Uuid::now_v7());
		for relative in [&graph, &atoms] {
			fs::create_dir_all(root.join(relative).join("stats")).assert_ok();
			for name in [
				"labels.fgb",
				"labels.fgb.bak",
				"stats/graph_stats.fgb",
				"stats/label_stats.fgb",
				"stats/prop_stats.fgb",
				"stats/user.fgb",
			] {
				fs::write(root.join(relative).join(name), b"fixture").assert_ok();
			}
		}
		let mut catalog = FileCatalog::bootstrap().assert_ok();
		catalog.next_file_instance_id = 3;
		for (instance, space, kind, path) in [
			(
				1,
				DataSpaceId::Graph(crate::graph::GraphId::new(1).assert_some()),
				DataFileKind::Labels,
				format!("{graph}/labels.fgb"),
			),
			(2, DataSpaceId::Database, DataFileKind::AtomsHot, format!("{atoms}/atoms_hot.fgb")),
		] {
			catalog.files.push(FileCatalogEntry {
				space_id: space,
				file_id: DataFileId::new(instance, kind).assert_some(),
				companion_of: None,
				role: DataFileRole::WalBackfilled,
				path: CatalogPath::parse(&path).assert_ok(),
			});
		}
		for _ in 0..2 {
			cleanup_orphan_generations(&root, &catalog).assert_ok();
		}
		for name in ["stats/graph_stats.fgb", "stats/label_stats.fgb"] {
			assert!(!root.join(&graph).join(name).exists());
		}
		for name in ["labels.fgb", "labels.fgb.bak", "stats/prop_stats.fgb", "stats/user.fgb"] {
			assert!(root.join(&graph).join(name).exists());
		}
		for name in [
			"labels.fgb",
			"labels.fgb.bak",
			"stats/graph_stats.fgb",
			"stats/label_stats.fgb",
			"stats/prop_stats.fgb",
			"stats/user.fgb",
		] {
			assert!(root.join(&atoms).join(name).exists());
		}
		drop(fs::remove_dir_all(root));
	}

	/// Selected metadata retirement requires exact kind, basename and graph
	/// ownership.
	#[test]
	fn selected_namespace_metadata_retirement_checks_kind_name_and_owner() {
		use crate::{storage::direct::DirectFileOpenMode, test_assertions::AssertResult as _};
		let root = std::env::temp_dir().join(format!("fg-namespace-retirement-{}", Uuid::now_v7()));
		let registry = FileRegistry::open(
			Vec::new(),
			DirectFileOpenMode::Synchronous,
			&rapidhash::RapidHashSet::default(),
		)
		.assert_ok();
		for graph in [false, true] {
			for (kind, name, owned) in [
				(DataFileKind::Labels, "labels.fgb", true),
				(DataFileKind::Statistics, "stats/graph_stats.fgb", true),
				(DataFileKind::LabelStatistics, "stats/label_stats.fgb", true),
				(DataFileKind::Statistics, "stats/label_stats.fgb", false),
				(DataFileKind::LabelStatistics, "stats/graph_stats.fgb", false),
				(DataFileKind::PropertyStatistics, "stats/prop_stats.fgb", false),
				(DataFileKind::Labels, "labels.fgb.bak", false),
			] {
				let (space, prefix) = if graph {
					(
						DataSpaceId::Graph(crate::graph::GraphId::new(1).assert_some()),
						format!("graphs/0000000000000001/{}", Uuid::now_v7()),
					)
				} else {
					(DataSpaceId::Database, format!("atoms/{}", Uuid::now_v7()))
				};
				let mut previous = FileCatalog::bootstrap().assert_ok();
				previous.next_file_instance_id = 3;
				for (instance, kind, name) in
					[(1, DataFileKind::Index, "idx/std_0000.fgidx"), (2, kind, name)]
				{
					previous.files.push(FileCatalogEntry {
						space_id: space,
						file_id: DataFileId::new(instance, kind).assert_some(),
						companion_of: None,
						role: DataFileRole::WalBackfilled,
						path: CatalogPath::parse(&format!("{prefix}/{name}")).assert_ok(),
					});
				}
				let mut current = previous.clone();
				current.files.retain(|entry| entry.file_id.instance_id() != 2);
				let retired =
					RetiredGeneration::between(&root, &previous, &current, &registry).assert_ok();
				if graph && owned {
					let retired = retired.assert_some();
					assert_eq!(retired.files, [root.join(&prefix).join(name)]);
					assert!(retired.directories.is_empty());
				} else {
					assert!(retired.is_none());
				}
			}
		}
	}

	/// Individual retirement uses the same graph-only ownership as recovery.
	#[test]
	fn selected_label_retirement_requires_graph_ownership() {
		use crate::{
			storage::{direct::DirectFileOpenMode, file_registry::FileRegistry},
			test_assertions::AssertResult as _,
		};
		let root = std::env::temp_dir().join(format!("fg-label-retirement-{}", Uuid::now_v7()));
		let registry = FileRegistry::open(
			Vec::new(),
			DirectFileOpenMode::Synchronous,
			&rapidhash::RapidHashSet::default(),
		)
		.assert_ok();
		for graph in [false, true] {
			let (space, relative) = if graph {
				(
					DataSpaceId::Graph(crate::graph::GraphId::new(1).assert_some()),
					format!("graphs/0000000000000001/{}/idx", Uuid::now_v7()),
				)
			} else {
				(DataSpaceId::Database, format!("atoms/{}/idx", Uuid::now_v7()))
			};
			let mut previous = FileCatalog::bootstrap().assert_ok();
			previous.next_file_instance_id = 3;
			for (instance, name) in [(1, "std_0000.fgidx"), (2, "lbl_node_0.fgidx")] {
				previous.files.push(FileCatalogEntry {
					space_id: space,
					file_id: DataFileId::new(instance, DataFileKind::Index).assert_some(),
					companion_of: None,
					role: DataFileRole::WalBackfilled,
					path: CatalogPath::parse(&format!("{relative}/{name}")).assert_ok(),
				});
			}
			let mut current = previous.clone();
			current.files.retain(|entry| entry.file_id.instance_id() != 2);
			let retired =
				RetiredGeneration::between(&root, &previous, &current, &registry).assert_ok();
			if graph {
				let retired = retired.assert_some();
				assert_eq!(retired.files, [root.join(relative).join("lbl_node_0.fgidx")]);
				assert!(retired.directories.is_empty());
			} else {
				assert!(retired.is_none());
			}
		}
	}

	/// Primary retirement preserves referenced files and unknown neighbours.
	#[test]
	fn startup_primary_cleanup_preserves_referenced_and_unknown_files() {
		use crate::test_assertions::AssertResult as _;
		let root = std::env::temp_dir().join(format!("fg-primary-orphans-{}", Uuid::now_v7()));
		let relative = format!("atoms/{}", Uuid::now_v7());
		let directory = root.join(&relative);
		fs::create_dir_all(&directory).assert_ok();
		for name in ["primary.fgidx", "primary-backup.fgidx", "user.fgidx"] {
			fs::write(directory.join(name), b"fixture").assert_ok();
		}
		let mut catalog = FileCatalog::bootstrap().assert_ok();
		catalog.next_file_instance_id = 3;
		for (instance, kind, name) in [
			(1, DataFileKind::AtomsHot, "atoms_hot.fgb"),
			(2, DataFileKind::Index, "primary.fgidx"),
		] {
			catalog.files.push(FileCatalogEntry {
				space_id: DataSpaceId::Database,
				file_id: DataFileId::new(instance, kind).assert_some(),
				companion_of: None,
				role: DataFileRole::WalBackfilled,
				path: CatalogPath::parse(&format!("{relative}/{name}")).assert_ok(),
			});
		}
		cleanup_orphan_generations(&root, &catalog).assert_ok();
		assert!(directory.join("primary.fgidx").is_file());
		catalog.files.retain(|entry| entry.file_id.kind() != Some(DataFileKind::Index));
		for _ in 0..2 {
			cleanup_orphan_generations(&root, &catalog).assert_ok();
		}
		assert!(!directory.join("primary.fgidx").exists());
		assert!(directory.join("primary-backup.fgidx").is_file());
		assert!(directory.join("user.fgidx").is_file());
		drop(fs::remove_dir_all(root));
	}

	/// Verifies startup cleanup removes only unreachable uuid generations.
	#[test]
	fn startup_cleanup_removes_only_unreachable_uuid_generations() {
		use crate::test_assertions::AssertResult as _;

		let root = std::env::temp_dir().join(format!("fg-gc-{}", Uuid::now_v7()));
		let reachable = Uuid::now_v7().to_string();
		let orphan = Uuid::now_v7().to_string();
		let reachable_path = root.join("atoms").join(&reachable);
		let orphan_path = root.join("atoms").join(&orphan);
		let unrelated = root.join("atoms").join("keep-me");
		fs::create_dir_all(&reachable_path)
			.map_err(|source| io_failure("create reachable fixture", source))
			.assert_ok();
		fs::create_dir_all(&orphan_path)
			.map_err(|source| io_failure("create orphan fixture", source))
			.assert_ok();
		fs::create_dir_all(&unrelated)
			.map_err(|source| io_failure("create unrelated fixture", source))
			.assert_ok();
		let mut catalog = FileCatalog::bootstrap().assert_ok();
		catalog.files.push(FileCatalogEntry {
			space_id: DataSpaceId::Database,
			file_id: DataFileId::new(1, DataFileKind::AtomsHot).assert_some(),
			companion_of: None,
			role: DataFileRole::WalBackfilled,
			path: CatalogPath::parse(&format!("atoms/{reachable}/atoms_hot.fgb")).assert_ok(),
		});

		cleanup_orphan_generations(&root, &catalog).assert_ok();
		assert!(reachable_path.is_dir());
		assert!(!orphan_path.exists());
		assert!(unrelated.is_dir());
		drop(fs::remove_dir_all(root));
	}

	/// Verifies startup cleanup removes only unreachable quarantines.
	#[test]
	fn startup_cleanup_removes_only_unreachable_quarantines() {
		use crate::test_assertions::AssertResult as _;

		let root = std::env::temp_dir().join(format!("fg-quarantine-cleanup-{}", Uuid::now_v7()));
		let directory = root.join("admin").join("quarantine");
		fs::create_dir_all(&directory)
			.map_err(|source| io_failure("create quarantine fixture directory", source))
			.assert_ok();
		let reachable = Uuid::now_v7();
		let orphan = Uuid::now_v7();
		let reachable_path = directory.join(format!("{reachable}.fgq"));
		let orphan_path = directory.join(format!("{orphan}.fgq"));
		let unrelated = directory.join("keep-me");
		for path in [&reachable_path, &orphan_path, &unrelated] {
			fs::write(path, b"fixture")
				.map_err(|source| io_failure("write quarantine fixture", source))
				.assert_ok();
		}
		let mut catalog = FileCatalog::bootstrap().assert_ok();
		catalog.files.push(FileCatalogEntry {
			space_id: DataSpaceId::Database,
			file_id: DataFileId::new(1, DataFileKind::ImportQuarantine)
				.ok_or_else(|| invalid("test quarantine identity is invalid"))
				.assert_ok(),
			companion_of: None,
			role: DataFileRole::AuxiliaryImmutable,
			path: CatalogPath::parse(&format!("admin/quarantine/{reachable}.fgq")).assert_ok(),
		});

		cleanup_orphan_quarantines(&root, &catalog).assert_ok();
		assert!(reachable_path.is_file());
		assert!(!orphan_path.exists());
		assert!(unrelated.is_file());
		drop(fs::remove_dir_all(root));
	}

	/// Verifies unpublished generation drop removes every owned directory.
	#[test]
	fn unpublished_generation_drop_removes_every_owned_directory() {
		use crate::test_assertions::AssertResult as _;

		let root = std::env::temp_dir().join(format!("fg-import-drop-{}", Uuid::now_v7()));
		let first = root.join("atoms").join(Uuid::now_v7().to_string());
		let second = root.join("graphs").join("0000000000000001").join(Uuid::now_v7().to_string());
		fs::create_dir_all(&first)
			.map_err(|source| io_failure("create first unpublished generation", source))
			.assert_ok();
		fs::create_dir_all(&second)
			.map_err(|source| io_failure("create second unpublished generation", source))
			.assert_ok();
		let generation = UnpublishedGeneration {
			directories: vec![first.clone(), second.clone()],
			armed: true,
		};
		drop(generation);

		assert!(!first.exists());
		assert!(!second.exists());
		drop(fs::remove_dir_all(root));
	}
}
