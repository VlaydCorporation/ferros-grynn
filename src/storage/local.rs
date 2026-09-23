//! Exclusive local-directory ownership and database-superblock COW.

use std::{fs, io, path::Path};

use super::{
	database_superblock::DatabaseSuperblock,
	metadata_cow::{select_checkpoint_page, write_checkpoint_page},
};
use crate::gql_status::{FerrosGrynnError, FerrosGrynnResult};

/// Process-lifetime exclusive ownership of one local database directory.
pub(super) struct DatabaseLock {
	#[cfg(windows)]
	_file: fs::File,
}

impl DatabaseLock {
	/// Opens `db.lock` without Windows sharing permissions.
	pub(super) fn acquire(database_directory: &Path) -> FerrosGrynnResult<Self> {
		#[cfg(windows)]
		{
			use std::os::windows::fs::OpenOptionsExt;

			use windows::Win32::Foundation::{ERROR_LOCK_VIOLATION, ERROR_SHARING_VIOLATION};

			let mut options = fs::OpenOptions::new();
			options.read(true).write(true).create(true).share_mode(0);
			let file = options.open(database_directory.join("db.lock")).map_err(|source| {
				let busy = source.raw_os_error().is_some_and(|code| {
					code == ERROR_SHARING_VIOLATION.0 as i32
						|| code == ERROR_LOCK_VIOLATION.0 as i32
				});
				if busy {
					FerrosGrynnError::local_database_busy().with_technical_source(source)
				} else {
					io_failure("acquire local database lock", source)
				}
			})?;
			Ok(Self {
				_file: file,
			})
		}
		#[cfg(not(windows))]
		{
			let _ = database_directory;
			Err(FerrosGrynnError::direct_io_unavailable(
				"local database ownership is currently supported only on Windows",
			))
		}
	}
}

/// Selects the newest valid database checkpoint authority.
pub(super) fn select_database_superblock(path: &Path) -> FerrosGrynnResult<DatabaseSuperblock> {
	select_checkpoint_page(
		path,
		DatabaseSuperblock::decode,
		|_value| true,
		|value| (value.generation, value.checkpoint_record_lsn),
		"DatabaseSuperblock",
	)
	.map(|(value, _page)| value)
}

/// Publishes database checkpoint authority after all subordinate metadata is
/// durable.
pub(super) fn write_database_superblock(
	path: &Path,
	value: &DatabaseSuperblock,
) -> FerrosGrynnResult<()> {
	write_checkpoint_page(path, &value.encode()?)
}

fn io_failure(operation: impl Into<String>, source: io::Error) -> FerrosGrynnError {
	FerrosGrynnError::storage_io_failure(operation).with_technical_source(source)
}

#[cfg(test)]
mod tests {
	use super::*;

	#[cfg(windows)]
	#[test]
	fn database_lock_is_exclusive() -> FerrosGrynnResult<()> {
		let root = std::env::temp_dir().join(format!(
			"fg-lock-{:016x}",
			crate::values::Uuid::now_v7().as_inner().as_u128() as u64
		));
		fs::create_dir(&root).map_err(|source| io_failure("create lock test directory", source))?;
		let first = DatabaseLock::acquire(&root)?;
		assert!(DatabaseLock::acquire(&root).is_err());
		drop(first);
		assert!(DatabaseLock::acquire(&root).is_ok());
		drop(fs::remove_dir_all(root));
		Ok(())
	}
}
