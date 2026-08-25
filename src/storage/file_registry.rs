//! Sealed mapping from persisted file identities to owned Direct-I/O handles.

use std::{path::PathBuf, sync::Arc};

use rapidhash::{HashMapExt, RapidHashMap};

use super::{
	direct::DirectFile,
	types::{DataFileId, DataFileKind, GraphId, PageKey},
};
use crate::gql_status::{FerrosGrynnError, FerrosGrynnResult};

/// Checkpoint behavior assigned to one registered file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum DataFileRole {
	/// Transactional pages copied from committed WAL frames.
	WalBackfilled,
	/// Graph metadata replaced atomically by checkpoint.
	GraphSuperblock,
}

/// Bootstrap description supplied by the local database opener.
pub(super) struct DataFileRegistration {
	/// Graph or system address space.
	pub(super) graph_id: GraphId,
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
	files: RapidHashMap<(GraphId, DataFileId), RegisteredDataFile>,
}

impl FileRegistry {
	/// Opens and seals one trusted set of physical data files.
	///
	/// # Errors
	///
	/// Returns an error for duplicate identities, a role/kind mismatch, an
	/// invalid graph-superblock length, or any Direct-I/O open failure.
	pub(super) fn open(registrations: Vec<DataFileRegistration>) -> FerrosGrynnResult<Self> {
		let mut files = RapidHashMap::with_capacity(registrations.len());
		for registration in registrations {
			validate_role(registration.file_id, registration.role)?;
			let opened = DirectFile::open_read_write(&registration.path)?;
			let file = if registration.role == DataFileRole::GraphSuperblock {
				if opened.page_count() != 1 {
					return Err(FerrosGrynnError::invalid_storage_file(
						"registered GraphSuperblock file must contain exactly one page",
					));
				}
				drop(opened);
				None
			} else {
				Some(Arc::new(opened))
			};
			let key = (registration.graph_id, registration.file_id);
			if files
				.insert(key, RegisteredDataFile {
					role: registration.role,
					path: registration.path,
					file,
				})
				.is_some()
			{
				return Err(FerrosGrynnError::invalid_storage_file(
					"duplicate data-file identity in sealed registry",
				));
			}
		}
		Ok(Self {
			files,
		})
	}

	/// Resolves a page key without deriving or opening a path from WAL bytes.
	///
	/// # Errors
	///
	/// Returns fatal storage corruption when the persisted identity is absent.
	pub(super) fn resolve(&self, key: PageKey) -> FerrosGrynnResult<&RegisteredDataFile> {
		self.resolve_file(key.graph_id(), key.file_id())
	}

	/// Resolves one complete physical file identity.
	///
	/// # Errors
	///
	/// Returns fatal storage corruption when the identity is absent.
	pub(super) fn resolve_file(
		&self,
		graph_id: GraphId,
		file_id: DataFileId,
	) -> FerrosGrynnResult<&RegisteredDataFile> {
		self.files.get(&(graph_id, file_id)).ok_or_else(|| {
			FerrosGrynnError::invalid_storage_file(format!(
				"unregistered data file graph={} id={:#018x}",
				graph_id.get(),
				file_id.get()
			))
		})
	}

	/// Iterates over the sealed registry without permitting mutation.
	pub(super) fn iter(
		&self,
	) -> impl Iterator<Item = ((GraphId, DataFileId), &RegisteredDataFile)> {
		self.files.iter().map(|(key, file)| (*key, file))
	}
}

/// Validates that checkpoint ownership agrees with the persisted file kind.
fn validate_role(file_id: DataFileId, role: DataFileRole) -> FerrosGrynnResult<()> {
	let graph_metadata = file_id.kind() == Some(DataFileKind::GraphSuperblock);
	if graph_metadata != (role == DataFileRole::GraphSuperblock) {
		return Err(FerrosGrynnError::invalid_storage_file(
			"data-file role does not match its format-v1 kind",
		));
	}
	Ok(())
}

#[cfg(test)]
mod tests {
	use super::*;

	/// Registry role validation rejects transactional graph metadata.
	#[test]
	fn role_must_match_closed_file_kind() {
		let graph = DataFileId::for_kind(DataFileKind::GraphSuperblock);
		let data = DataFileId::for_kind(DataFileKind::NodesHot);
		assert!(validate_role(graph, DataFileRole::WalBackfilled).is_err());
		assert!(validate_role(data, DataFileRole::GraphSuperblock).is_err());
		assert!(validate_role(graph, DataFileRole::GraphSuperblock).is_ok());
	}
}
