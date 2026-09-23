//! Sealed mapping from persisted file identities to owned Direct-I/O handles.

use std::{path::PathBuf, sync::Arc};

use rapidhash::{HashMapExt, HashSetExt, RapidHashMap, RapidHashSet};

use super::{
	direct::{DirectFile, DirectFileOpenMode},
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
	file: Option<Arc<DirectFile>>,
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
	pub(super) fn file(&self) -> FerrosGrynnResult<&Arc<DirectFile>> {
		self.file.as_ref().ok_or_else(|| {
			FerrosGrynnError::invalid_storage_file(
				"checkpoint-owned metadata has no positional data handle",
			)
		})
	}
}

/// Immutable registry sealed after all trusted paths are opened.
pub(super) struct FileRegistry {
	/// Files keyed by their complete persisted identity.
	files: RapidHashMap<(DataSpaceId, DataFileId), Arc<RegisteredDataFile>>,
}

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
			match open_registration(registration, io_mode) {
				Ok(file) => {
					files.insert(key, Arc::new(file));
				}
				Err(_error) if degradable.contains(&key) => {}
				Err(error) => return Err(error),
			}
		}
		Ok(Self {
			files,
		})
	}

	/// Adds a fully created layout after its files are durable but before the
	/// transaction can stage frames for them.
	pub(super) fn insert(
		&mut self,
		registrations: Vec<DataFileRegistration>,
		io_mode: DirectFileOpenMode,
	) -> FerrosGrynnResult<()> {
		let mut opened = Vec::with_capacity(registrations.len());
		let mut keys = RapidHashMap::with_capacity(registrations.len());
		for registration in registrations {
			let key = (registration.space_id, registration.file_id);
			if self.files.contains_key(&key) || keys.insert(key, ()).is_some() {
				return Err(FerrosGrynnError::invalid_storage_file(
					"duplicate data-file identity in registry extension",
				));
			}
			opened.push((key, Arc::new(open_registration(registration, io_mode)?)));
		}
		for (key, file) in opened {
			self.files.insert(key, file);
		}
		Ok(())
	}

	/// Removes registrations that never became reachable from a durable
	/// catalog root.
	pub(super) fn remove(&mut self, keys: &[(DataSpaceId, DataFileId)]) {
		for key in keys {
			self.files.remove(key);
		}
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
}

/// Opens one trusted registration without publishing a partial registry.
fn open_registration(
	registration: DataFileRegistration,
	io_mode: DirectFileOpenMode,
) -> FerrosGrynnResult<RegisteredDataFile> {
	validate_registration(registration.space_id, registration.file_id, registration.role)?;
	let opened = DirectFile::open_read_write_with_mode(&registration.path, io_mode)?;
	let file = if registration.role == DataFileRole::Superblock {
		if opened.page_count() != 1 {
			return Err(FerrosGrynnError::invalid_storage_file(
				"registered checkpoint-owned superblock must contain exactly one page",
			));
		}
		drop(opened);
		None
	} else {
		Some(Arc::new(opened))
	};
	Ok(RegisteredDataFile {
		role: registration.role,
		path: registration.path,
		file,
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
	let metadata = matches!(
		file_id.kind(),
		Some(DataFileKind::GraphSuperblock | DataFileKind::AtomStoreSuperblock)
	);
	if metadata != (role == DataFileRole::Superblock) {
		return Err(FerrosGrynnError::invalid_storage_file(
			"data-file role does not match its format-v1 kind",
		));
	}
	Ok(())
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::graph::GraphId;

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
		let graph_space = DataSpaceId::Graph(GraphId::new(1).unwrap());
		assert!(
			validate_registration(DataSpaceId::Database, graph, DataFileRole::Superblock).is_err()
		);
		assert!(validate_registration(graph_space, atoms, DataFileRole::Superblock).is_err());
	}
}
