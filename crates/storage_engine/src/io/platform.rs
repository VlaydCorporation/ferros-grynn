use cfg_if::cfg_if;
use std::io::{Error, Result};
use std::path::Path;
use std::sync::OnceLock;

static ALIGNMENT: OnceLock<usize> = OnceLock::new();

cfg_if!(
	if #[cfg(unix)] {
		mod platform {
			use super::*;
			use std::os::fd::RawFd;

			use nix::fcntl::{OFlag, open};
			use nix::sys::stat::Mode;
			use nix::sys::uio::{pread, pwrite};
			use nix::unistd::{sysconf, SysconfVar, read, write, close};

			pub type FileHandle = RawFd;

			pub fn get_alignment() -> usize {
				*ALIGNMENT.get_or_init(|| {
					unsafe { sysconf(SysconfVar::PAGE_SIZE) }.unwrap().unwrap() as usize
				})
			}

			pub fn open_file<P: AsRef<Path>>(path: P, write: bool) -> Result<FileHandle> {
				let flags = if write {
					OFlag::O_WRONLY | OFlag::O_CREAT | OFlag::O_TRUNC | OFlag::O_DIRECT
				} else {
					OFlag::O_RDONLY | OFlag::O_DIRECT
				};
				open(path.as_ref(), flags, Mode::S_IRUSR | Mode::S_IWUSR)
				.map_err(Error::from)
			}

			pub fn read_file(fd: FileHandle, buf: &mut [u8]) -> Result<usize> {
				unsafe { read(fd, buf) }.map_err(Error::from)
			}

			pub fn read_at(fd: FileHandle, buf: &mut [u8], offset: i64) -> Result<usize> {
				pread(fd, buf, offset).map_err(Error::from)
			}

			pub fn write_file(fd: FileHandle, buf: &[u8]) -> Result<usize> {
				unsafe { write(fd, buf) }.map_err(Error::from)
			}

			pub fn write_at(fd: FileHandle, buf: &[u8], offset: i64) -> Result<usize> {
				pwrite(fd, buf, offset).map_err(Error::from)
			}

			pub fn close_file(fd: FileHandle) -> Result<()> {
				unsafe { close(fd) }.map_err(Error::from)
			}
		}
	} else if #[cfg(windows)] {
		mod platform {
			use super::*;
			use windows::Win32::System::SystemInformation::{GetSystemInfo, SYSTEM_INFO};
			use windows::Win32::Storage::FileSystem::{CREATE_ALWAYS, OPEN_EXISTING, CreateFileW, FILE_SHARE_READ, FILE_SHARE_WRITE, FILE_FLAG_NO_BUFFERING, ReadFile, WriteFile};
			use windows::Win32::Foundation::{GENERIC_READ, GENERIC_WRITE, HANDLE, CloseHandle};
			use windows::Win32::System::IO::OVERLAPPED;
			use windows::core::HSTRING;

			pub type FileHandle = HANDLE;

			pub fn get_alignment() -> usize {
				*ALIGNMENT.get_or_init(|| {
					let mut info = SYSTEM_INFO::default();
					unsafe { GetSystemInfo(&mut info) };
					info.dwPageSize as usize
				})
			}

			pub fn open_file<P: AsRef<Path>>(path: P, write: bool) -> Result<FileHandle> {
				let path = path.as_ref();
				let access = if write { GENERIC_WRITE } else { GENERIC_READ };
				let disposition = if write { CREATE_ALWAYS } else { OPEN_EXISTING };

				unsafe {
					CreateFileW(
						&HSTRING::from(path),
						access.0,
						FILE_SHARE_READ | FILE_SHARE_WRITE,
						None,
						disposition,
						FILE_FLAG_NO_BUFFERING,
						None,
					)
				}.map_err(Error::from)
			}

			/// For Windows, reading/writing at an offset requires the OVERLAPPED structure.
			pub unsafe fn create_overlapped(offset: u64) -> OVERLAPPED {
				let mut overlapped = OVERLAPPED::default();
				overlapped.Anonymous.Anonymous.Offset = (offset & 0xFFFFFFFF) as u32;
				overlapped.Anonymous.Anonymous.OffsetHigh = (offset >> 32) as u32;
				overlapped
			}

			pub fn read_file(handle: FileHandle, buf: &mut [u8]) -> Result<usize> {
				let mut bytes_read = 0;
				unsafe {
					ReadFile(
						handle,
						Some(buf),
						Some(&mut bytes_read),
						None,
					)
				}.map_err(Error::from)?;
				Ok(bytes_read as usize)
			}

			pub fn read_at(handle: FileHandle, buf: &mut [u8], offset: u64) -> Result<usize> {
				let mut overlapped = unsafe { create_overlapped(offset) };
				let mut bytes_read = 0;
				unsafe {
					ReadFile(
						handle,
						Some(buf),
						Some(&mut bytes_read),
						Some(&mut overlapped),
					)
				}.map_err(Error::from)?;
				Ok(bytes_read as usize)
			}

			pub fn write_file(handle: FileHandle, buf: &[u8]) -> Result<usize> {
				let mut bytes_written = 0;
				unsafe {
					WriteFile(
						handle,
						Some(buf),
						Some(&mut bytes_written),
						None,
					)
				}.map_err(Error::from)?;
				Ok(bytes_written as usize)
			}

			pub fn write_at(handle: FileHandle, buf: &[u8], offset: u64) -> Result<usize> {
				let mut overlapped = unsafe { create_overlapped(offset) };
                let mut bytes_written = 0;
				unsafe {
					WriteFile(
						handle,
						Some(buf),
						Some(&mut bytes_written),
						Some(&mut overlapped),
					)
				}.map_err(Error::from)?;
				Ok(bytes_written as usize)
			}

			pub fn close_file(handle: FileHandle) -> Result<()> {
				unsafe { CloseHandle(handle) }.map_err(Error::from)
			}
		}
	}
);

pub(crate) use platform::*;
