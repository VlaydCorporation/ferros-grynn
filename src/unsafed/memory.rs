use super::error::{NativeMemoryAllocationRefusedRepr, Result};
use std::alloc::{Layout, alloc, dealloc};
use std::ptr;
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};

pub fn page_size() -> usize {
	#[cfg(unix)]
	{
		use nix::libc::{_SC_PAGESIZE, sysconf};

		let page_size = unsafe { sysconf(_SC_PAGESIZE) };
		if page_size < 0 {
			4096
		} else {
			page_size as usize
		}
	}

	#[cfg(windows)]
	{
		use windows::Win32::System::SystemInformation::{GetSystemInfo, SYSTEM_INFO};

		let mut info = SYSTEM_INFO::default();
		unsafe { GetSystemInfo(&mut info) };
		info.dwPageSize as usize
	}
}

/// Allocates a block of memory of the given size in bytes.
pub fn allocate_memory(size: usize) -> Result<*mut u8> {
	let layout = Layout::from_size_align(size, 1).unwrap();
	let ptr = unsafe { alloc(layout) };
	if ptr.is_null() {
		Err(NativeMemoryAllocationRefusedRepr {
			size,
			already_allocated: 0, // This is hard to track without a global allocator
		}
		.into())
	} else {
		Ok(ptr)
	}
}

/// Frees a block of memory allocated with `allocate_memory`.
pub unsafe fn free_memory(ptr: *mut u8, size: usize) {
	let layout = Layout::from_size_align(size, 1).unwrap();
	unsafe {
		dealloc(ptr, layout);
	}
}

/// Sets a block of memory to a given value.
pub unsafe fn set_memory(ptr: *mut u8, bytes: usize, value: u8) {
	unsafe {
		ptr::write_bytes(ptr, value, bytes);
	}
}

/// Copies a block of memory from a source to a destination.
pub unsafe fn copy_memory(src: *const u8, dest: *mut u8, bytes: usize) {
	unsafe {
		ptr::copy_nonoverlapping(src, dest, bytes);
	}
}

// Atomic operations
pub fn compare_and_swap_u64(target: &AtomicU64, expected: u64, update: u64) -> bool {
	target.compare_exchange(expected, update, Ordering::SeqCst, Ordering::SeqCst).is_ok()
}

pub fn compare_and_swap_i64(target: &AtomicI64, expected: i64, update: i64) -> bool {
	target.compare_exchange(expected, update, Ordering::SeqCst, Ordering::SeqCst).is_ok()
}

pub fn get_and_add_i64(target: &AtomicI64, delta: i64) -> i64 {
	target.fetch_add(delta, Ordering::SeqCst)
}

pub fn get_and_set_i64(target: &AtomicI64, value: i64) -> i64 {
	target.swap(value, Ordering::SeqCst)
}

// Byte-wise operations for unaligned access

pub unsafe fn get_short_byte_wise_little_endian(p: *const u8) -> i16 {
	let mut bytes = [0u8; 2];
	unsafe {
		ptr::copy_nonoverlapping(p, bytes.as_mut_ptr(), 2);
	}
	i16::from_le_bytes(bytes)
}

pub unsafe fn put_short_byte_wise_little_endian(p: *mut u8, value: i16) {
	let bytes = value.to_le_bytes();
	unsafe {
		ptr::copy_nonoverlapping(bytes.as_ptr(), p, 2);
	}
}

pub unsafe fn get_int_byte_wise_little_endian(p: *const u8) -> i32 {
	let mut bytes = [0u8; 4];
	unsafe {
		ptr::copy_nonoverlapping(p, bytes.as_mut_ptr(), 4);
	}
	i32::from_le_bytes(bytes)
}

pub unsafe fn put_int_byte_wise_little_endian(p: *mut u8, value: i32) {
	let bytes = value.to_le_bytes();
	unsafe {
		ptr::copy_nonoverlapping(bytes.as_ptr(), p, 4);
	}
}

pub unsafe fn get_long_byte_wise_little_endian(p: *const u8) -> i64 {
	let mut bytes = [0u8; 8];
	unsafe {
		ptr::copy_nonoverlapping(p, bytes.as_mut_ptr(), 8);
	}
	i64::from_le_bytes(bytes)
}

pub unsafe fn put_long_byte_wise_little_endian(p: *mut u8, value: i64) {
	let bytes = value.to_le_bytes();
	unsafe {
		ptr::copy_nonoverlapping(bytes.as_ptr(), p, 8);
	}
}
