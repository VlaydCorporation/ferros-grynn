#[allow(
	clippy::wildcard_imports,
	reason = "platform shims use the parent Direct-I/O contract"
)]
use super::*;

/// Adds the Linux `O_DIRECT` flag without per-write synchronization.
#[cfg(target_os = "linux")]
#[allow(
	clippy::unnecessary_wraps,
	reason = "all platform shims share the fallible signature used by unsupported targets"
)]
pub(super) fn configure_direct_io(
	options: &mut OpenOptions,
	_io_mode: DirectFileOpenMode,
) -> FerrosGrynnResult<()> {
	use std::os::unix::fs::OpenOptionsExt;

	options.custom_flags(nix::libc::O_DIRECT | nix::libc::O_CLOEXEC);
	Ok(())
}

/// Performs one positional read on Linux.
#[cfg(target_os = "linux")]
pub(super) fn read_at(file: &File, buffer: &mut [u8], offset: u64) -> io::Result<usize> {
	use std::os::unix::fs::FileExt;

	file.read_at(buffer, offset)
}

/// Performs one positional write on Linux.
#[cfg(target_os = "linux")]
pub(super) fn write_at(file: &File, buffer: &[u8], offset: u64) -> io::Result<usize> {
	use std::os::unix::fs::FileExt;

	file.write_at(buffer, offset)
}
