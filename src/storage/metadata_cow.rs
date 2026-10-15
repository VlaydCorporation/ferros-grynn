//! Shared tmp/main/prev protocol for checkpoint-published metadata pages.

use std::{
	fs::{self, File},
	io::{self, Read, Write},
	path::{Path, PathBuf},
};

use super::{
	direct::DirectFile,
	page::{AlignedPage, PAGE_SIZE, PageNo},
};
use crate::gql_status::{FerrosGrynnError, FerrosGrynnResult};

/// Suffix of the never-authoritative write-in-progress candidate.
const TEMP_SUFFIX: &str = ".tmp";
/// Suffix of the retained previous authoritative candidate.
const PREVIOUS_SUFFIX: &str = ".prev";

/// Decode and authority state of one metadata candidate.
enum Candidate<T> {
	/// No file exists at the candidate path.
	Missing,
	/// The file exists but is malformed or not accepted by authority.
	Invalid,
	/// The file contains a valid, authority-approved metadata page.
	Valid {
		/// Decoded metadata used for comparison and return.
		value: T,
		/// Exact page bytes retained with the decoded value.
		page: Box<AlignedPage>,
	},
}

/// Decode and authority state of one variable-length metadata candidate.
enum BytesCandidate<T> {
	/// Missing case of bytes candidate.
	Missing,
	/// Invalid case of bytes candidate.
	Invalid,
	/// Valid case of bytes candidate.
	Valid {
		/// Value associated with this bytes candidate.
		value: T,
		/// Bytes associated with this bytes candidate.
		bytes: Vec<u8>,
	},
}

/// Writes a checkpoint metadata page through the canonical tmp/main/prev
/// sequence.
///
/// The caller must publish the database checkpoint authority only after this
/// function succeeds. A leftover `.tmp` is never selected by recovery.
pub(super) fn write_checkpoint_page(target: &Path, page: &AlignedPage) -> FerrosGrynnResult<()> {
	let temporary = derived_path(target, TEMP_SUFFIX);
	let previous = derived_path(target, PREVIOUS_SUFFIX);
	remove_if_present(&temporary, "remove stale metadata temporary file")?;
	let file = DirectFile::create_new(&temporary)?;
	file.write_page(PageNo::new(0), page)?;
	// Make the temporary payload durable before either authoritative candidate
	// name changes.
	file.sync_all()?;
	drop(file);

	if target.exists() {
		remove_if_present(&previous, "remove obsolete metadata previous file")?;
		rename_durable(target, &previous)?;
	}
	rename_durable(&temporary, target)
}

/// Writes an arbitrary checksummed metadata payload through the same
/// tmp/main/prev authority protocol as checkpoint metadata pages.
pub(super) fn write_metadata_bytes(target: &Path, bytes: &[u8]) -> FerrosGrynnResult<()> {
	let temporary = derived_path(target, TEMP_SUFFIX);
	let previous = derived_path(target, PREVIOUS_SUFFIX);
	remove_if_present(&temporary, "remove stale metadata temporary file")?;
	let mut file = File::create_new(&temporary)
		.map_err(|source| io_failure("create metadata temporary file", source))?;
	file.write_all(bytes).map_err(|source| io_failure("write metadata temporary file", source))?;
	file.sync_all().map_err(|source| io_failure("synchronize metadata temporary file", source))?;
	drop(file);

	if target.exists() {
		remove_if_present(&previous, "remove obsolete metadata previous file")?;
		rename_durable(target, &previous)?;
	}
	rename_durable(&temporary, target)
}

/// Selects the newest valid variable-length metadata candidate from main and
/// previous files. A neighboring temporary file is never authoritative.
pub(super) fn select_metadata_bytes<T, Key>(
	target: &Path,
	max_bytes: usize,
	decode: impl Fn(&[u8]) -> FerrosGrynnResult<T> + Copy,
	key_of: impl Fn(&T) -> Key + Copy,
	kind: &str,
) -> FerrosGrynnResult<T>
where
	T: Clone,
	Key: Copy + Ord,
{
	let main = read_bytes_candidate(target, max_bytes, decode)?;
	let previous = read_bytes_candidate(&derived_path(target, PREVIOUS_SUFFIX), max_bytes, decode)?;
	match (main, previous) {
		(
			BytesCandidate::Valid {
				value,
				..
			},
			BytesCandidate::Missing | BytesCandidate::Invalid,
		)
		| (
			BytesCandidate::Missing | BytesCandidate::Invalid,
			BytesCandidate::Valid {
				value,
				..
			},
		) => Ok(value),
		(
			BytesCandidate::Valid {
				value: main_value,
				bytes: main_bytes,
			},
			BytesCandidate::Valid {
				value: previous_value,
				bytes: previous_bytes,
			},
		) => {
			let main_key = key_of(&main_value);
			let previous_key = key_of(&previous_value);
			if main_key == previous_key && main_bytes != previous_bytes {
				return Err(invalid(format!(
					"{kind} main and previous files have equal clocks but different bytes"
				)));
			}
			if main_key >= previous_key {
				Ok(main_value)
			} else {
				Ok(previous_value)
			}
		}
		(
			BytesCandidate::Missing | BytesCandidate::Invalid,
			BytesCandidate::Missing | BytesCandidate::Invalid,
		) => Err(invalid(format!("no valid {kind} main/previous file exists"))),
	}
}

/// Selects the newest checksummed main/prev page accepted by database
/// checkpoint authority.
///
/// The neighboring `.tmp` is intentionally never opened or promoted.
pub(super) fn select_checkpoint_page<T, Key>(
	target: &Path,
	decode: impl Fn(&AlignedPage) -> FerrosGrynnResult<T> + Copy,
	accepted: impl Fn(&T) -> bool + Copy,
	key_of: impl Fn(&T) -> Key + Copy,
	kind: &str,
) -> FerrosGrynnResult<(T, Box<AlignedPage>)>
where
	T: Clone,
	Key: Copy + Ord,
{
	let main = read_candidate(target, decode, accepted)?;
	let previous = read_candidate(&derived_path(target, PREVIOUS_SUFFIX), decode, accepted)?;
	match (main, previous) {
		(
			Candidate::Valid {
				value,
				page,
			},
			Candidate::Missing | Candidate::Invalid,
		)
		| (
			Candidate::Missing | Candidate::Invalid,
			Candidate::Valid {
				value,
				page,
			},
		) => Ok((value, page)),
		(
			Candidate::Valid {
				value: main_value,
				page: main_page,
			},
			Candidate::Valid {
				value: previous_value,
				page: previous_page,
			},
		) => {
			let main_key = key_of(&main_value);
			let previous_key = key_of(&previous_value);
			if main_key == previous_key && main_page.as_bytes() != previous_page.as_bytes() {
				return Err(invalid(format!(
					"{kind} main and previous pages have equal clocks but different bytes"
				)));
			}
			if main_key >= previous_key {
				Ok((main_value, main_page))
			} else {
				Ok((previous_value, previous_page))
			}
		}
		(Candidate::Missing | Candidate::Invalid, Candidate::Missing | Candidate::Invalid) => Err(
			invalid(format!("no valid {kind} main/previous page satisfies checkpoint authority")),
		),
	}
}

/// Reads, decodes, and classifies one main or previous metadata candidate.
fn read_candidate<T>(
	path: &Path,
	decode: impl Fn(&AlignedPage) -> FerrosGrynnResult<T>,
	accepted: impl Fn(&T) -> bool,
) -> FerrosGrynnResult<Candidate<T>> {
	let mut file = match File::open(path) {
		Ok(file) => file,
		Err(source) if source.kind() == io::ErrorKind::NotFound => return Ok(Candidate::Missing),
		Err(source) => return Err(io_failure("open metadata checkpoint candidate", source)),
	};
	if file
		.metadata()
		.map_err(|source| io_failure("inspect metadata checkpoint candidate", source))?
		.len()
		!= PAGE_SIZE as u64
	{
		return Ok(Candidate::Invalid);
	}
	let mut page = Box::new(AlignedPage::zeroed());
	match file.read_exact(page.as_bytes_mut()) {
		Ok(()) => {}
		Err(source) if source.kind() == io::ErrorKind::UnexpectedEof => {
			return Ok(Candidate::Invalid);
		}
		Err(source) => return Err(io_failure("read metadata checkpoint candidate", source)),
	}
	match decode(page.as_ref()) {
		Ok(value) if accepted(&value) => Ok(Candidate::Valid {
			value,
			page,
		}),
		Ok(_) | Err(_) => Ok(Candidate::Invalid),
	}
}

/// Reads one bounded variable-length metadata candidate.
fn read_bytes_candidate<T>(
	path: &Path,
	max_bytes: usize,
	decode: impl Fn(&[u8]) -> FerrosGrynnResult<T>,
) -> FerrosGrynnResult<BytesCandidate<T>> {
	let mut file = match File::open(path) {
		Ok(file) => file,
		Err(source) if source.kind() == io::ErrorKind::NotFound => {
			return Ok(BytesCandidate::Missing);
		}
		Err(source) => return Err(io_failure("open metadata candidate", source)),
	};
	let length =
		file.metadata().map_err(|source| io_failure("inspect metadata candidate", source))?.len();
	let length = match usize::try_from(length) {
		Ok(length) if length <= max_bytes => length,
		Ok(_) | Err(_) => return Ok(BytesCandidate::Invalid),
	};
	let mut bytes = vec![0; length];
	match file.read_exact(&mut bytes) {
		Ok(()) => {}
		Err(source) if source.kind() == io::ErrorKind::UnexpectedEof => {
			return Ok(BytesCandidate::Invalid);
		}
		Err(source) => return Err(io_failure("read metadata candidate", source)),
	}
	match decode(&bytes) {
		Ok(value) => Ok(BytesCandidate::Valid {
			value,
			bytes,
		}),
		Err(_) => Ok(BytesCandidate::Invalid),
	}
}

/// Derives a sibling metadata path by appending a fixed suffix.
fn derived_path(target: &Path, suffix: &str) -> PathBuf {
	let mut path = target.as_os_str().to_os_string();
	path.push(suffix);
	PathBuf::from(path)
}

/// Removes a stale candidate while treating absence as success.
fn remove_if_present(path: &Path, operation: &str) -> FerrosGrynnResult<()> {
	match fs::remove_file(path) {
		Ok(()) => Ok(()),
		Err(source) if source.kind() == io::ErrorKind::NotFound => Ok(()),
		Err(source) => Err(io_failure(operation, source)),
	}
}

#[cfg(windows)]
/// Atomically renames a metadata candidate with write-through semantics.
fn rename_durable(source: &Path, target: &Path) -> FerrosGrynnResult<()> {
	use std::{iter, os::windows::ffi::OsStrExt};

	use windows::{
		Win32::Storage::FileSystem::{
			MOVEFILE_REPLACE_EXISTING,
			MOVEFILE_WRITE_THROUGH,
			MoveFileExW,
		},
		core::PCWSTR,
	};

	let source = source.as_os_str().encode_wide().chain(iter::once(0)).collect::<Vec<_>>();
	let target = target.as_os_str().encode_wide().chain(iter::once(0)).collect::<Vec<_>>();
	// SAFETY: both buffers are NUL-terminated UTF-16 paths retained for the
	// synchronous call; the destination is absent and no handle is transferred.
	unsafe {
		MoveFileExW(
			PCWSTR(source.as_ptr()),
			PCWSTR(target.as_ptr()),
			MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
		)
	}
	.map_err(|source| {
		FerrosGrynnError::storage_io_failure("rename checkpoint metadata")
			.with_technical_source(source)
	})
}

#[cfg(not(windows))]
/// Renames a metadata candidate and synchronizes its containing directory.
fn rename_durable(source: &Path, target: &Path) -> FerrosGrynnResult<()> {
	fs::rename(source, target)
		.map_err(|source| io_failure("rename checkpoint metadata", source))?;
	let parent = target
		.parent()
		.ok_or_else(|| invalid("checkpoint metadata path has no parent directory"))?;
	File::open(parent)
		.and_then(|directory| directory.sync_all())
		.map_err(|source| io_failure("synchronize checkpoint metadata directory", source))
}

/// Wraps an operating-system I/O error in the storage status boundary.
fn io_failure(operation: impl Into<String>, source: io::Error) -> FerrosGrynnError {
	FerrosGrynnError::storage_io_failure(operation).with_technical_source(source)
}

/// Creates the typed invalid-metadata error used by recovery.
fn invalid(reason: impl Into<String>) -> FerrosGrynnError {
	FerrosGrynnError::invalid_storage_file(reason)
}

#[cfg(test)]
/// Returns the canonical temporary path for recovery tests.
pub(super) fn temporary_path(target: &Path) -> PathBuf {
	derived_path(target, TEMP_SUFFIX)
}

#[cfg(test)]
/// Returns the canonical previous-candidate path for recovery tests.
pub(super) fn previous_path(target: &Path) -> PathBuf {
	derived_path(target, PREVIOUS_SUFFIX)
}
