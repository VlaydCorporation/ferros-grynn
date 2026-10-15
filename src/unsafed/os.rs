use lazy_static::lazy_static;
use parking_lot::Mutex;
use sysinfo::System;

lazy_static! {
	static ref SYSTEM: Mutex<System> = Mutex::new(System::new_all());
}

/// Looks up total physical memory for this module.
pub fn get_total_physical_memory() -> u64 {
	SYSTEM.lock().total_memory()
}

/// Looks up free physical memory for this module.
pub fn get_free_physical_memory() -> u64 {
	SYSTEM.lock().free_memory()
}

/// Looks up total swap space for this module.
pub fn get_total_swap_space() -> u64 {
	SYSTEM.lock().total_swap()
}

/// Looks up free swap space for this module.
pub fn get_free_swap_space() -> u64 {
	SYSTEM.lock().free_swap()
}

// Committed virtual memory is not directly available in a cross-platform way
// with sysinfo. This will return available memory as a proxy.
/// Looks up committed virtual memory for this module.
pub fn get_committed_virtual_memory() -> u64 {
	SYSTEM.lock().available_memory()
}

/// Looks up max file descriptors for this module.
pub fn get_max_file_descriptors() -> Option<u64> {
	#[cfg(unix)]
	{
		use nix::libc::{RLIMIT_NOFILE, getrlimit, rlimit};

		let mut limit = rlimit {
			rlim_cur: 0,
			rlim_max: 0,
		};
		unsafe {
			if getrlimit(RLIMIT_NOFILE, &mut limit) {
				Some(limit.rlim_max as u64)
			} else {
				None
			}
		}
	}

	#[cfg(windows)]
	{
		None
	}
}

/// Looks up open file descriptors for this module.
pub fn get_open_file_descriptors() -> Option<u64> {
	#[cfg(unix)]
	{
		use std::fs;

		match fs::read_dir("/proc/self/fd") {
			Ok(entries) => Some(entries.count() as u64),
			Err(_) => None,
		}
	}

	#[cfg(windows)]
	{
		None
	}
}
