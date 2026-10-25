#[allow(
	clippy::wildcard_imports,
	reason = "platform shims use the parent Direct-I/O contract"
)]
use super::*;

/// Adds `FILE_FLAG_NO_BUFFERING` without `FILE_FLAG_WRITE_THROUGH`.
#[cfg(windows)]
#[allow(
	clippy::unnecessary_wraps,
	reason = "all platform shims share the fallible signature used by unsupported targets"
)]
pub(super) fn configure_direct_io(
	options: &mut OpenOptions,
	io_mode: DirectFileOpenMode,
) -> FerrosGrynnResult<()> {
	use std::os::windows::fs::OpenOptionsExt;

	use windows::Win32::Storage::FileSystem::{
		FILE_FLAG_NO_BUFFERING,
		FILE_FLAG_OVERLAPPED,
		FILE_SHARE_DELETE,
		FILE_SHARE_READ,
		FILE_SHARE_WRITE,
	};

	let flags = match io_mode {
		DirectFileOpenMode::Synchronous => FILE_FLAG_NO_BUFFERING,
		DirectFileOpenMode::Overlapped => FILE_FLAG_NO_BUFFERING | FILE_FLAG_OVERLAPPED,
	};
	options
		.custom_flags(flags.0)
		.share_mode((FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE).0);
	Ok(())
}

/// Performs one positional read on Windows.
#[cfg(windows)]
pub(super) fn read_at(file: &File, buffer: &mut [u8], offset: u64) -> io::Result<usize> {
	use std::os::windows::fs::FileExt;

	file.seek_read(buffer, offset)
}

/// Performs one positional write on Windows.
#[cfg(windows)]
pub(super) fn write_at(file: &File, buffer: &[u8], offset: u64) -> io::Result<usize> {
	use std::os::windows::fs::FileExt;

	file.seek_write(buffer, offset)
}
