//! Windows-first local database ownership and atomic superblock replacement.

use std::{
	fs::{self, File},
	io::{self, Read},
	path::{Path, PathBuf},
};

use super::{
	direct::DirectFile,
	page::{AlignedPage, PAGE_SIZE, PageNo},
	superblock::{DatabaseSuperblock, GraphSuperblock},
};
use crate::gql_status::{FerrosGrynnError, FerrosGrynnResult};

/// Suffix appended to the complete target filename for atomic replacement.
const SUPERBLOCK_TEMP_SUFFIX: &str = ".tmp";

/// Exclusive process ownership of one local database directory.
pub(super) struct DatabaseLock {
	/// Non-shareable Windows handle retained for the database lifetime.
	#[cfg(windows)]
	_file: File,
}

impl DatabaseLock {
	/// Acquires the persistent `db.lock` ownership file.
	///
	/// # Errors
	///
	/// Returns `LocalDatabaseBusy` when another Windows handle owns the file,
	/// an I/O status for other native failures, and an unsupported status on
	/// deferred non-Windows targets.
	#[cfg(windows)]
	pub(super) fn acquire(database_directory: &Path) -> FerrosGrynnResult<Self> {
		use std::os::windows::fs::OpenOptionsExt;

		use windows::Win32::Foundation::{ERROR_LOCK_VIOLATION, ERROR_SHARING_VIOLATION};

		let path = database_directory.join("db.lock");
		let mut options = fs::OpenOptions::new();
		options.read(true).write(true).create(true).share_mode(0);
		let file = options.open(path).map_err(|source| {
			let sharing = source.raw_os_error().is_some_and(|code| {
				code == ERROR_SHARING_VIOLATION.0 as i32 || code == ERROR_LOCK_VIOLATION.0 as i32
			});
			if sharing {
				FerrosGrynnError::local_database_busy().with_technical_source(source)
			} else {
				io_failure("acquire local database lock", source)
			}
		})?;
		Ok(Self {
			_file: file,
		})
	}

	/// Rejects local database ownership until the Unix runtime port is tested.
	///
	/// # Errors
	///
	/// Always returns `DirectIoUnavailable` on non-Windows targets.
	#[cfg(not(windows))]
	pub(super) fn acquire(_database_directory: &Path) -> FerrosGrynnResult<Self> {
		Err(FerrosGrynnError::direct_io_unavailable(
			"local database ownership is Windows-only in storage slice 3b",
		))
	}
}

/// Writes one page to a durable neighbor and atomically replaces the target.
///
/// # Errors
///
/// Returns an error for temporary-file cleanup, Direct I/O, synchronization,
/// path encoding, or native atomic replacement failure.
pub(super) fn write_superblock_atomic(target: &Path, page: &AlignedPage) -> FerrosGrynnResult<()> {
	let temporary = temporary_path(target);
	remove_if_present(&temporary, "remove stale superblock temporary file")?;
	let file = DirectFile::create_new(&temporary)?;
	file.write_page(PageNo::new(0), page)?;
	file.sync_all()?;
	drop(file);
	atomic_replace(&temporary, target)
}

/// Selects, promotes, and decodes the authoritative database superblock.
///
/// # Errors
///
/// Returns an error when no valid candidate exists, equal clocks have
/// different bytes, promotion fails, or stale temporary cleanup fails.
pub(super) fn select_database_superblock(path: &Path) -> FerrosGrynnResult<DatabaseSuperblock> {
	select_superblock(
		path,
		DatabaseSuperblock::decode,
		|value| (value.generation, value.checkpoint_record_lsn),
		"DatabaseSuperblock",
	)
}

/// Selects, promotes, and decodes the authoritative graph superblock.
///
/// # Errors
///
/// Returns the same candidate-selection errors as
/// [`select_database_superblock`].
pub(super) fn select_graph_superblock(path: &Path) -> FerrosGrynnResult<GraphSuperblock> {
	select_superblock(
		path,
		GraphSuperblock::decode,
		|value| (value.generation, value.checkpoint_record_lsn),
		"GraphSuperblock",
	)
}

/// Candidate validity after exact-size read and typed decoding.
enum Candidate<T> {
	/// Path does not exist.
	Missing,
	/// Bytes exist but fail exact-size or codec validation.
	Invalid,
	/// Complete validated value and its canonical page bytes.
	Valid {
		/// Decoded typed superblock.
		value: T,
		/// Page retained for equal-clock byte comparison.
		page: Box<AlignedPage>,
	},
}

/// Filesystem action chosen after comparing validated candidates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Selection {
	/// Keep the current main file.
	Main,
	/// Promote the neighboring temporary file.
	Temporary,
}

/// Applies the shared two-candidate selection protocol without a public trait.
fn select_superblock<T, Key, Decode, KeyOf>(
	path: &Path,
	decode: Decode,
	key_of: KeyOf,
	kind: &str,
) -> FerrosGrynnResult<T>
where
	T: Clone,
	Key: Copy + Ord,
	Decode: Fn(&AlignedPage) -> FerrosGrynnResult<T> + Copy,
	KeyOf: Fn(&T) -> Key,
{
	let temporary = temporary_path(path);
	let main = read_candidate(path, decode)?;
	let temp = read_candidate(&temporary, decode)?;
	let (selection, value) = match (&main, &temp) {
		(
			Candidate::Valid {
				value,
				..
			},
			Candidate::Missing | Candidate::Invalid,
		) => (Selection::Main, value.clone()),
		(
			Candidate::Missing | Candidate::Invalid,
			Candidate::Valid {
				value,
				..
			},
		) => (Selection::Temporary, value.clone()),
		(
			Candidate::Valid {
				value: main_value,
				page: main_page,
			},
			Candidate::Valid {
				value: temp_value,
				page: temp_page,
			},
		) => {
			let main_key = key_of(main_value);
			let temp_key = key_of(temp_value);
			if main_key == temp_key && main_page.as_bytes() != temp_page.as_bytes() {
				return Err(FerrosGrynnError::invalid_storage_file(format!(
					"{kind} main and temporary candidates have equal clocks but different bytes"
				)));
			}
			if temp_key > main_key {
				(Selection::Temporary, temp_value.clone())
			} else {
				(Selection::Main, main_value.clone())
			}
		}
		(Candidate::Missing | Candidate::Invalid, Candidate::Missing | Candidate::Invalid) => {
			return Err(FerrosGrynnError::invalid_storage_file(format!(
				"no valid {kind} main or temporary candidate"
			)));
		}
	};

	match selection {
		Selection::Main => remove_if_present(&temporary, "remove stale superblock candidate")?,
		Selection::Temporary => atomic_replace(&temporary, path)?,
	}
	Ok(value)
}

/// Reads one exact page and converts codec failures into invalid candidates.
fn read_candidate<T, Decode>(path: &Path, decode: Decode) -> FerrosGrynnResult<Candidate<T>>
where
	Decode: Fn(&AlignedPage) -> FerrosGrynnResult<T>,
{
	let mut file = match File::open(path) {
		Ok(file) => file,
		Err(source) if source.kind() == io::ErrorKind::NotFound => return Ok(Candidate::Missing),
		Err(source) => return Err(io_failure("open superblock candidate", source)),
	};
	let length =
		file.metadata().map_err(|source| io_failure("inspect superblock candidate", source))?.len();
	if length != PAGE_SIZE as u64 {
		return Ok(Candidate::Invalid);
	}
	let mut page = Box::new(AlignedPage::zeroed());
	match file.read_exact(page.as_bytes_mut()) {
		Ok(()) => {}
		Err(source) if source.kind() == io::ErrorKind::UnexpectedEof => {
			return Ok(Candidate::Invalid);
		}
		Err(source) => return Err(io_failure("read superblock candidate", source)),
	}
	match decode(page.as_ref()) {
		Ok(value) => Ok(Candidate::Valid {
			value,
			page,
		}),
		Err(_error) => Ok(Candidate::Invalid),
	}
}

/// Appends `.tmp` to the complete target filename.
fn temporary_path(target: &Path) -> PathBuf {
	let mut path = target.as_os_str().to_os_string();
	path.push(SUPERBLOCK_TEMP_SUFFIX);
	PathBuf::from(path)
}

/// Removes one exact neighboring temporary file when present.
fn remove_if_present(path: &Path, operation: &str) -> FerrosGrynnResult<()> {
	match fs::remove_file(path) {
		Ok(()) => Ok(()),
		Err(source) if source.kind() == io::ErrorKind::NotFound => Ok(()),
		Err(source) => Err(io_failure(operation, source)),
	}
}

/// Replaces a path through the durable Windows rename primitive.
#[cfg(windows)]
fn atomic_replace(source: &Path, target: &Path) -> FerrosGrynnResult<()> {
	use std::{iter, os::windows::ffi::OsStrExt};

	use windows::{
		Win32::Storage::FileSystem::{
			MOVEFILE_REPLACE_EXISTING,
			MOVEFILE_WRITE_THROUGH,
			MoveFileExW,
		},
		core::PCWSTR,
	};

	let source_wide = source.as_os_str().encode_wide().chain(iter::once(0)).collect::<Vec<_>>();
	let target_wide = target.as_os_str().encode_wide().chain(iter::once(0)).collect::<Vec<_>>();
	// SAFETY: both vectors are NUL-terminated UTF-16 paths and remain alive for
	// the complete synchronous call. Flags request only documented replacement
	// and write-through behavior; no handle ownership crosses the boundary.
	unsafe {
		MoveFileExW(
			PCWSTR(source_wide.as_ptr()),
			PCWSTR(target_wide.as_ptr()),
			MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
		)
	}
	.map_err(|source| {
		FerrosGrynnError::storage_io_failure(format!(
			"atomically replace superblock {}",
			target.display()
		))
		.with_technical_source(source)
	})
}

/// Rejects atomic replacement until a Unix implementation is tested.
#[cfg(not(windows))]
fn atomic_replace(_source: &Path, _target: &Path) -> FerrosGrynnResult<()> {
	Err(FerrosGrynnError::direct_io_unavailable(
		"atomic superblock replacement is Windows-only in storage slice 3b",
	))
}

/// Preserves a native I/O source behind the project status boundary.
fn io_failure(operation: impl Into<String>, source: io::Error) -> FerrosGrynnError {
	FerrosGrynnError::storage_io_failure(operation).with_technical_source(source)
}

#[cfg(test)]
#[allow(
	clippy::panic_in_result_fn,
	reason = "fallible filesystem fixtures return Result while assertions report contract failures"
)]
mod tests {
	use std::{
		error::Error,
		io::Write,
		sync::atomic::{AtomicU64, Ordering},
	};

	use super::*;
	use crate::storage::{
		page::PageNo,
		superblock::DatabaseSuperblock,
		types::{CheckpointGeneration, CommitTs, Lsn, TxId},
	};

	/// Monotonic suffix for collision-resistant local-storage test directories.
	static NEXT_TEST_DIRECTORY: AtomicU64 = AtomicU64::new(0);

	/// Temporary directory removed on success and unwind.
	struct TestDirectory(PathBuf);

	impl TestDirectory {
		/// Creates one unique empty directory in the workspace.
		fn new() -> io::Result<Self> {
			let suffix = NEXT_TEST_DIRECTORY.fetch_add(1, Ordering::Relaxed);
			let path = std::env::current_dir()?
				.join(format!(".ferros-grynn-local-{}-{suffix}", std::process::id()));
			fs::create_dir(&path)?;
			Ok(Self(path))
		}
	}

	impl Drop for TestDirectory {
		/// Removes only the uniquely named test directory.
		fn drop(&mut self) {
			drop(fs::remove_dir_all(&self.0));
		}
	}

	/// Creates one valid candidate with configurable checkpoint clocks.
	fn database(generation: u64, checkpoint_record_lsn: u64, name: &str) -> DatabaseSuperblock {
		DatabaseSuperblock {
			db_name: name.into(),
			generation: CheckpointGeneration::new(generation),
			checkpoint_lsn: Lsn::new(3),
			checkpoint_record_lsn: Lsn::new(checkpoint_record_lsn),
			durable_visible_lsn: Lsn::new(3),
			next_lsn: Lsn::new(checkpoint_record_lsn + 1),
			commit_ts: CommitTs::new(1),
			next_tx_id: TxId::new(2),
			next_file_instance_id: 1,
			system_backfilled_lsn: Lsn::new(3),
			graph_catalog_root_page: PageNo::NULL,
			file_catalog_root_page: PageNo::NULL,
			graph_count: 0,
		}
	}

	/// Writes exact candidate bytes without invoking the selection protocol.
	fn write_candidate(path: &Path, page: &AlignedPage) -> io::Result<()> {
		let mut file = File::create(path)?;
		file.write_all(page.as_bytes())?;
		file.sync_all()
	}

	/// Windows share-mode zero provides exclusive process ownership.
	#[cfg(windows)]
	#[test]
	fn database_lock_is_exclusive_for_its_lifetime() -> Result<(), Box<dyn Error>> {
		let directory = TestDirectory::new()?;
		let first = DatabaseLock::acquire(&directory.0)?;
		let second = DatabaseLock::acquire(&directory.0);
		assert_eq!(
			second.err().map(|error| error.status().code().to_string()),
			Some("FG-51I03".into())
		);
		drop(first);
		assert!(DatabaseLock::acquire(&directory.0).is_ok());
		Ok(())
	}

	/// A newer valid temporary candidate is promoted to the main path.
	#[cfg(windows)]
	#[test]
	fn newer_temporary_superblock_is_promoted() -> Result<(), Box<dyn Error>> {
		let directory = TestDirectory::new()?;
		let path = directory.0.join("db.fgb");
		write_candidate(&path, &database(1, 4, "old").encode()?)?;
		let temporary = temporary_path(&path);
		write_candidate(&temporary, &database(2, 8, "new").encode()?)?;

		let selected = select_database_superblock(&path)?;
		assert_eq!(selected.db_name, "new");
		assert!(!temporary.exists());
		assert_eq!(select_database_superblock(&path)?.generation, CheckpointGeneration::new(2));
		Ok(())
	}

	/// Equal authority clocks with different valid bytes are corruption.
	#[cfg(windows)]
	#[test]
	fn equal_candidate_clocks_require_identical_bytes() -> Result<(), Box<dyn Error>> {
		let directory = TestDirectory::new()?;
		let path = directory.0.join("db.fgb");
		write_candidate(&path, &database(2, 8, "left").encode()?)?;
		write_candidate(&temporary_path(&path), &database(2, 8, "right").encode()?)?;
		assert!(select_database_superblock(&path).is_err());
		Ok(())
	}

	/// A corrupt temporary candidate never displaces a valid main page.
	#[cfg(windows)]
	#[test]
	fn corrupt_temporary_is_removed_after_main_selection() -> Result<(), Box<dyn Error>> {
		let directory = TestDirectory::new()?;
		let path = directory.0.join("db.fgb");
		write_candidate(&path, &database(2, 8, "main").encode()?)?;
		let temporary = temporary_path(&path);
		let mut file = File::create(&temporary)?;
		file.write_all(b"partial")?;
		file.sync_all()?;
		drop(file);
		assert_eq!(select_database_superblock(&path)?.db_name, "main");
		assert!(!temporary.exists());
		Ok(())
	}

	/// A valid temporary candidate recovers a missing main file.
	#[cfg(windows)]
	#[test]
	fn temporary_only_candidate_is_promoted() -> Result<(), Box<dyn Error>> {
		let directory = TestDirectory::new()?;
		let path = directory.0.join("db.fgb");
		let temporary = temporary_path(&path);
		write_candidate(&temporary, &database(1, 4, "temp").encode()?)?;
		assert_eq!(select_database_superblock(&path)?.db_name, "temp");
		assert!(path.exists());
		assert!(!temporary.exists());
		Ok(())
	}

	/// A corrupt main file cannot displace a valid temporary candidate.
	#[cfg(windows)]
	#[test]
	fn valid_temporary_replaces_corrupt_main() -> Result<(), Box<dyn Error>> {
		let directory = TestDirectory::new()?;
		let path = directory.0.join("db.fgb");
		let mut main = File::create(&path)?;
		main.write_all(b"partial")?;
		main.sync_all()?;
		drop(main);
		write_candidate(&temporary_path(&path), &database(2, 8, "recovered").encode()?)?;
		assert_eq!(select_database_superblock(&path)?.db_name, "recovered");
		Ok(())
	}

	/// Two invalid candidates provide no checkpoint authority.
	#[cfg(windows)]
	#[test]
	fn two_invalid_candidates_are_fatal() -> Result<(), Box<dyn Error>> {
		let directory = TestDirectory::new()?;
		let path = directory.0.join("db.fgb");
		let temporary = temporary_path(&path);
		for candidate in [&path, &temporary] {
			let mut file = File::create(candidate)?;
			file.write_all(b"partial")?;
			file.sync_all()?;
		}
		assert!(select_database_superblock(&path).is_err());
		Ok(())
	}

	/// A valid but older temporary candidate is removed after main selection.
	#[cfg(windows)]
	#[test]
	fn stale_valid_temporary_is_removed() -> Result<(), Box<dyn Error>> {
		let directory = TestDirectory::new()?;
		let path = directory.0.join("db.fgb");
		write_candidate(&path, &database(3, 12, "main").encode()?)?;
		let temporary = temporary_path(&path);
		write_candidate(&temporary, &database(2, 8, "stale").encode()?)?;
		assert_eq!(select_database_superblock(&path)?.db_name, "main");
		assert!(!temporary.exists());
		Ok(())
	}
}
