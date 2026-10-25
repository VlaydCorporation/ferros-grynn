//! Sealed mapping from persisted file identities to owned Direct-I/O handles.

use std::{
	collections::BTreeSet,
	fs,
	path::PathBuf,
	sync::{Arc, OnceLock, Weak},
};

use parking_lot::Mutex;
use rapidhash::{HashMapExt, HashSetExt, RapidHashMap, RapidHashSet};

use super::{
	direct::{DirectFile, DirectFileOpenMode},
	direct_io::validate_file_length,
	types::{DataFileId, DataFileKind, PageKey},
};
use crate::{
	datastore::DataSpaceId,
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
};

/// Checkpoint behavior assigned to one registered file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum DataFileRole {
	/// Transactional pages copied from committed WAL frames.
	WalBackfilled,
	/// Data-space metadata replaced atomically by checkpoint.
	Superblock,
	/// Metadata replaced through the checksummed metadata-COW protocol.
	MetadataCow,
	/// Immutable auxiliary file outside positional page addressing.
	AuxiliaryImmutable,
}

/// Bootstrap description supplied by the local database opener.
#[derive(Debug, Clone)]
pub(super) struct DataFileRegistration {
	/// Database or graph address space.
	pub(super) space_id: DataSpaceId,
	/// Stable file identity persisted in WAL.
	pub(super) file_id: DataFileId,
	/// Checkpoint ownership contract.
	pub(super) role: DataFileRole,
	/// Exact local path resolved by the trusted database layout.
	pub(super) path: PathBuf,
}

/// One opened file retained for the manager lifetime.
pub(super) struct RegisteredDataFile {
	/// Checkpoint ownership contract.
	role: DataFileRole,
	/// Exact trusted local path.
	path: PathBuf,
	/// Shared Direct-I/O handle used by reads and checkpoint writes.
	///
	/// Checkpoint-owned superblocks keep no handle because Windows replacement
	/// requires the old file object to be closed.
	file: OnceLock<Arc<DirectFile>>,
	/// Serializes only the first native open attempt.
	open_lock: Mutex<()>,
	/// Native handle behavior used when the data file is first demanded.
	open_mode: DirectFileOpenMode,
	/// Validated home-file length in pages.
	page_count: u64,
}

impl RegisteredDataFile {
	/// Returns the checkpoint ownership contract.
	pub(super) const fn role(&self) -> DataFileRole {
		self.role
	}

	/// Returns the trusted local path.
	pub(super) fn path(&self) -> &std::path::Path {
		&self.path
	}

	/// Returns the shared Direct-I/O handle.
	pub(super) fn file(&self) -> FerrosGrynnResult<Arc<DirectFile>> {
		if self.role != DataFileRole::WalBackfilled {
			return Err(FerrosGrynnError::invalid_storage_file(
				"metadata and auxiliary files have no positional data handle",
			));
		}
		if let Some(file) = self.file.get() {
			return Ok(file.clone());
		}
		let _open = self.open_lock.lock();
		if let Some(file) = self.file.get() {
			return Ok(file.clone());
		}
		let opened = Arc::new(DirectFile::open_read_write_with_mode(&self.path, self.open_mode)?);
		match self.file.set(opened.clone()) {
			Ok(()) => Ok(opened),
			Err(_opened) => self.file.get().cloned().ok_or_else(|| {
				FerrosGrynnError::invalid_storage_file(
					"lazy data-file handle publication lost its initialized value",
				)
			}),
		}
	}

	/// Returns whether this registration currently owns a native data handle.
	pub(super) fn is_open(&self) -> bool {
		self.file.get().is_some()
	}

	/// Returns the validated number of addressable home pages.
	pub(super) const fn page_count(&self) -> u64 {
		self.page_count
	}
}

/// Immutable registry sealed after all trusted paths are opened.
pub(super) struct FileRegistry {
	/// Files keyed by their complete persisted identity.
	files: RapidHashMap<(DataSpaceId, DataFileId), Arc<RegisteredDataFile>>,
}

/// Trusted physical identity retained without extending a retired file's life.
pub(super) type RetiredFileMarker = ((DataSpaceId, DataFileId), Weak<RegisteredDataFile>);

impl FileRegistry {
	/// Opens and seals one trusted set of physical data files.
	///
	/// # Errors
	///
	/// Returns an error for duplicate identities, a role/kind mismatch, an
	/// invalid graph-superblock length, or a required Direct-I/O open failure.
	/// Cataloged secondary indexes in `degradable` are omitted when their
	/// physical file cannot be opened so callers can retain schema and fall
	/// back to a primary scan.
	pub(super) fn open(
		registrations: Vec<DataFileRegistration>,
		io_mode: DirectFileOpenMode,
		degradable: &RapidHashSet<(DataSpaceId, DataFileId)>,
	) -> FerrosGrynnResult<Self> {
		Self::open_with_reuse(registrations, io_mode, degradable, None)
	}

	/// Reuses the exact file owner across catalog refreshes so snapshots and
	/// checkpoint share native handles, cached lengths and retirement markers.
	///
	/// # Errors
	///
	/// Returns the same validation errors as [`Self::open`], or an error when
	/// a previously registered identity changes its physical contract.
	pub(super) fn open_with_reuse(
		registrations: Vec<DataFileRegistration>,
		io_mode: DirectFileOpenMode,
		degradable: &RapidHashSet<(DataSpaceId, DataFileId)>,
		previous: Option<&Self>,
	) -> FerrosGrynnResult<Self> {
		let mut files = RapidHashMap::with_capacity(registrations.len());
		let mut seen = RapidHashSet::with_capacity(registrations.len());
		for registration in registrations {
			let key = (registration.space_id, registration.file_id);
			if !seen.insert(key) {
				return Err(FerrosGrynnError::invalid_storage_file(
					"duplicate data-file identity in sealed registry",
				));
			}
			validate_registration(registration.space_id, registration.file_id, registration.role)?;
			let old = previous.and_then(|registry| registry.files.get(&key));
			if let Some(old) = old
				&& (old.path != registration.path
					|| old.role != registration.role
					|| old.open_mode != io_mode)
			{
				return Err(FerrosGrynnError::invalid_storage_file(
					"registered identity changed its path, role or I/O mode",
				));
			}
			match open_registration(registration, io_mode) {
				Ok(file) => {
					let file = old.map_or_else(|| Arc::new(file), Arc::clone);
					files.insert(key, file);
				}
				Err(_error) if degradable.contains(&key) => {}
				Err(error) => return Err(error),
			}
		}
		Ok(Self {
			files,
		})
	}

	/// Reports whether one complete physical identity remains available.
	pub(super) fn contains(&self, space_id: DataSpaceId, file_id: DataFileId) -> bool {
		self.files.contains_key(&(space_id, file_id))
	}

	/// Resolves a page key without deriving or opening a path from WAL bytes.
	///
	/// # Errors
	///
	/// Returns fatal storage corruption when the persisted identity is absent.
	pub(super) fn resolve(&self, key: PageKey) -> FerrosGrynnResult<Arc<RegisteredDataFile>> {
		self.resolve_file(key.space_id(), key.file_id())
	}

	/// Resolves one complete physical file identity.
	///
	/// # Errors
	///
	/// Returns fatal storage corruption when the identity is absent.
	pub(super) fn resolve_file(
		&self,
		space_id: DataSpaceId,
		file_id: DataFileId,
	) -> FerrosGrynnResult<Arc<RegisteredDataFile>> {
		self.files.get(&(space_id, file_id)).cloned().ok_or_else(|| {
			FerrosGrynnError::invalid_storage_file(format!(
				"unregistered data file space={} id={:#018x}",
				space_id.raw(),
				file_id.get()
			))
		})
	}

	/// Iterates over the sealed registry without permitting mutation.
	pub(super) fn snapshot(&self) -> Vec<((DataSpaceId, DataFileId), Arc<RegisteredDataFile>)> {
		self.files.iter().map(|(key, file)| (*key, file.clone())).collect()
	}

	/// Remembers every removed registration, including files whose directory
	/// stays reachable through an independently retained index.
	pub(super) fn markers_except(
		&self,
		retained: &BTreeSet<(DataSpaceId, DataFileId)>,
	) -> Vec<RetiredFileMarker> {
		self.files
			.iter()
			.filter(|(key, _file)| !retained.contains(key))
			.map(|(key, file)| (*key, Arc::downgrade(file)))
			.collect()
	}

	/// Returns `(registrations, open positional handles)`.
	pub(super) fn handle_counts(&self) -> (usize, usize) {
		(self.files.len(), self.files.values().filter(|file| file.is_open()).count())
	}
}

/// Validates one trusted registration without opening a persistent handle.
fn open_registration(
	registration: DataFileRegistration,
	io_mode: DirectFileOpenMode,
) -> FerrosGrynnResult<RegisteredDataFile> {
	validate_registration(registration.space_id, registration.file_id, registration.role)?;
	let length = fs::metadata(&registration.path)
		.map_err(|source| {
			FerrosGrynnError::storage_io_failure("read registered data-file metadata")
				.with_technical_source(source)
		})?
		.len();
	validate_file_length(length)?;
	if registration.role == DataFileRole::Superblock
		&& length != crate::storage::page::PAGE_SIZE as u64
	{
		return Err(FerrosGrynnError::invalid_storage_file(
			"registered checkpoint-owned superblock must contain exactly one page",
		));
	}
	let file = OnceLock::new();
	let page_count = length / crate::storage::page::PAGE_SIZE as u64;
	Ok(RegisteredDataFile {
		role: registration.role,
		path: registration.path,
		file,
		open_lock: Mutex::new(()),
		open_mode: io_mode,
		page_count,
	})
}

/// Validates that a checkpoint-owned superblock belongs to its data-space
/// family.
fn validate_registration(
	space_id: DataSpaceId,
	file_id: DataFileId,
	role: DataFileRole,
) -> FerrosGrynnResult<()> {
	validate_role(file_id, role)?;
	match (space_id, file_id.kind()) {
		(DataSpaceId::Database, Some(DataFileKind::GraphSuperblock))
		| (DataSpaceId::Graph(_), Some(DataFileKind::AtomStoreSuperblock)) => Err(
			FerrosGrynnError::invalid_storage_file("superblock kind does not match its data space"),
		),
		_ => Ok(()),
	}
}

/// Validates that checkpoint ownership agrees with the persisted file kind.
fn validate_role(file_id: DataFileId, role: DataFileRole) -> FerrosGrynnResult<()> {
	let expected = match file_id.kind() {
		Some(DataFileKind::GraphSuperblock | DataFileKind::AtomStoreSuperblock) => {
			DataFileRole::Superblock
		}
		Some(DataFileKind::JobCatalog) => DataFileRole::MetadataCow,
		Some(DataFileKind::ImportQuarantine) => DataFileRole::AuxiliaryImmutable,
		Some(_) => DataFileRole::WalBackfilled,
		None => {
			return Err(FerrosGrynnError::invalid_storage_file(
				"data-file identity has an unknown format-v1 kind",
			));
		}
	};
	if expected != role {
		return Err(FerrosGrynnError::invalid_storage_file(
			"data-file role does not match its format-v1 kind",
		));
	}
	Ok(())
}

#[cfg(test)]
/// Regression tests for file registry behavior.
mod tests {
	use super::*;
	use crate::graph::GraphId;
	#[cfg(test)]
	use crate::test_assertions::AssertOption as _;

	/// Registry role validation rejects transactional graph metadata.
	#[test]
	fn role_must_match_closed_file_kind() {
		let graph = DataFileId::for_kind(DataFileKind::GraphSuperblock);
		let atoms = DataFileId::for_kind(DataFileKind::AtomStoreSuperblock);
		let data = DataFileId::for_kind(DataFileKind::NodesHot);
		assert!(validate_role(graph, DataFileRole::WalBackfilled).is_err());
		assert!(validate_role(data, DataFileRole::Superblock).is_err());
		assert!(validate_role(graph, DataFileRole::Superblock).is_ok());
		assert!(validate_role(atoms, DataFileRole::Superblock).is_ok());
		let graph_space = DataSpaceId::Graph(GraphId::new(1).assert_some());
		assert!(
			validate_registration(DataSpaceId::Database, graph, DataFileRole::Superblock).is_err()
		);
		assert!(validate_registration(graph_space, atoms, DataFileRole::Superblock).is_err());
	}

	/// Verifies ordinary handle opens only on first use.
	#[test]
	fn ordinary_handle_opens_only_on_first_use() {
		use crate::test_assertions::AssertResult as _;

		let path =
			std::env::temp_dir().join(format!("fg-registry-{}.fgb", crate::values::Uuid::now_v7()));
		let created = DirectFile::create_new(&path).assert_ok();
		created.resize_pages(1).assert_ok();
		drop(created);
		let file_id = DataFileId::for_kind(DataFileKind::NodesHot);
		let registration = DataFileRegistration {
			space_id: DataSpaceId::Database,
			file_id,
			role: DataFileRole::WalBackfilled,
			path: path.clone(),
		};
		let registry = FileRegistry::open(
			vec![registration.clone()],
			DirectFileOpenMode::Synchronous,
			&RapidHashSet::default(),
		)
		.assert_ok();
		assert_eq!(registry.handle_counts(), (1, 0));
		drop(registry.resolve_file(DataSpaceId::Database, file_id).assert_ok().file().assert_ok());
		assert_eq!(registry.handle_counts(), (1, 1));
		let next = FileRegistry::open_with_reuse(
			vec![registration.clone()],
			DirectFileOpenMode::Synchronous,
			&RapidHashSet::default(),
			Some(&registry),
		)
		.assert_ok();
		assert!(Arc::ptr_eq(
			&registry.resolve_file(DataSpaceId::Database, file_id).assert_ok(),
			&next.resolve_file(DataSpaceId::Database, file_id).assert_ok()
		));
		assert_eq!(next.handle_counts(), (1, 1));
		let mut changed = registration;
		changed.path = path.with_extension("missing");
		let mut degradable = RapidHashSet::default();
		degradable.insert((DataSpaceId::Database, file_id));
		assert!(
			FileRegistry::open_with_reuse(
				vec![changed],
				DirectFileOpenMode::Synchronous,
				&degradable,
				Some(&registry)
			)
			.is_err()
		);
		drop(next);
		drop(registry);
		drop(std::fs::remove_file(path));
	}
}
