#![cfg(not(feature = "allocation-tracking"))]

use std::alloc::{GlobalAlloc, Layout, System};

/// This structure implements a wrapper around the system allocator,
/// or a user-specified allocator. It is a thin shim, enabling some
/// functions, but without tracking the current memory allocated.
pub struct NotrackAlloc<Alloc = System> {
	/// Underlying allocator to which allocation operations are forwarded.
	alloc: Alloc,
}

impl<A> NotrackAlloc<A> {
	#[inline]
	/// Creates a new `NotrackAlloc<A>` value.
	pub const fn new(alloc: A) -> Self {
		Self {
			alloc,
		}
	}
}

// SAFETY: Every operation preserves the pointer, layout and allocator identity
// by forwarding directly to the same inner GlobalAlloc implementation.
unsafe impl<A: GlobalAlloc> GlobalAlloc for NotrackAlloc<A> {
	/// Allocates memory through the underlying allocator without updating
	/// allocation accounting.
	unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
		// SAFETY: We are forwarding to the underlying allocator with the same
		// layout
		unsafe { self.alloc.alloc(layout) }
	}

	/// Allocates zero-initialized memory through the underlying allocator
	/// without updating allocation accounting.
	unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
		// SAFETY: We are forwarding to the underlying allocator with the same
		// layout
		unsafe { self.alloc.alloc_zeroed(layout) }
	}

	/// Releases memory through the underlying allocator without updating
	/// allocation accounting.
	unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
		// SAFETY: We are forwarding to the underlying allocator with the same
		// ptr and layout
		unsafe { self.alloc.dealloc(ptr, layout) };
	}

	/// Resizes memory through the underlying allocator without updating
	/// allocation accounting.
	unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
		// SAFETY: We are forwarding to the underlying allocator with the same
		// arguments
		unsafe { self.alloc.realloc(ptr, layout, new_size) }
	}
}
