#![cfg(feature = "allocation-tracking")]

use std::{
	alloc::{GlobalAlloc, Layout, System},
	cell::Cell,
	sync::atomic::{AtomicI64, Ordering},
};

#[cfg(test)]
use super::registry;

/// Signed sum of allocation deltas already flushed by participating threads.
static GLOBAL_TOTAL_BYTES: AtomicI64 = AtomicI64::new(0);

/// Magnitude of a thread-local delta that triggers a global-counter flush.
const BATCH_THRESHOLD: i64 = 12 * 1024;

/// Maximum nested tracking depth admitted before skipping deeper updates.
const MAX_DEPTH: u32 = 3;

thread_local! {
	/// Per-thread accumulation buffer for batched updates
	static THREAD_STATE: ThreadState = const { ThreadState::new() };

	/// Recursion depth counter to prevent infinite recursion with nested allocations
	static RECURSION_DEPTH: Cell<u32> = const { Cell::new(0) };
}

/// Thread-local allocation delta waiting to be flushed into global accounting.
struct ThreadState {
	/// Thread-local allocation delta not yet flushed to the global counter.
	local_bytes: Cell<i64>,
}

impl ThreadState {
	/// Constructs `ThreadState` from the supplied initial state.
	const fn new() -> Self {
		Self {
			local_bytes: Cell::new(0),
		}
	}

	/// Flushes to global for `ThreadState`.
	fn flush_to_global(&self) {
		let delta = self.local_bytes.get();
		if delta != 0 {
			// This counter observes byte totals; it does not publish or
			// synchronize allocation contents.
			GLOBAL_TOTAL_BYTES.fetch_add(delta, Ordering::Relaxed);
			self.local_bytes.set(0);
		}
	}
}

/// Returns flushed allocation deltas plus registered external memory reports.
#[cfg(test)]
pub(super) fn memory_allocated() -> usize {
	// Get the heap memory allocated
	let heap_memory = GLOBAL_TOTAL_BYTES.load(Ordering::Relaxed).max(0) as usize;
	// Get the external memory allocated
	let external_memory = registry::memory_reporters_allocated_total();
	// Return the total memory allocated
	heap_memory + external_memory
}

/// Flushes this thread's pending allocation delta to the global counter.
#[cfg(test)]
pub fn flush_local_allocations() {
	THREAD_STATE.with(|state| {
		state.flush_to_global();
	});
}

/// This structure implements a wrapper around the system allocator,
/// or around a user-specified allocator. It tracks the current memory
/// which is allocated, allowing the memory use to be checked at runtime.
///
/// # Important Note on Thread Pools
///
/// This allocator automatically batches thread allocations and syncs to
/// the global counter after a threshold is reached. Threads are tracked
/// using thread-local storage, and the global counter is updated atomically.
///
/// While ThreadState does not implement Drop (due to Rust's restriction
/// that "the global allocator may not use TLS with destructors"), unflushed
/// thread-local bytes are periodically synced via the batch threshold
/// mechanism. At thread termination, any remaining unflushed bytes may not be
/// reflected in the global counter, so reported bytes may differ from live
/// allocations. The connected crate currently exposes these observations only
/// to tests, which call `flush_local_allocations` before sampling the counter.
/// The production allocator still applies the same per-thread thresholds;
/// no worker shutdown hook is installed by this wrapper.
///
/// # Design Features
///
/// - Lock-free operations for zero contention
/// - Batched updates to reduce atomic operations
/// - Recursion depth tracking prevents infinite recursion while tracking nested
///   allocations
/// - O(1) usage queries regardless of thread count
pub struct TrackAlloc<Alloc = System> {
	/// Underlying allocator to which allocation requests are forwarded.
	alloc: Alloc,
}

impl<A> TrackAlloc<A> {
	/// Constructs `TrackAlloc<A>` from the supplied initial state.
	#[inline]
	pub const fn new(alloc: A) -> Self {
		Self {
			alloc,
		}
	}
}

impl<A: GlobalAlloc> TrackAlloc<A> {
	/// Adds a successful allocation to this thread's batched byte delta.
	fn add(size: usize) {
		// Track the current recursion depth
		let depth = RECURSION_DEPTH.with(|d| {
			let current = d.get();
			if current >= MAX_DEPTH {
				return MAX_DEPTH;
			}
			d.set(current + 1);
			current
		});
		// Don't recursively track too deep
		if depth >= MAX_DEPTH {
			return;
		}
		// Update the tracked byte count
		THREAD_STATE.with(|state| {
			let bytes = state.local_bytes.get() + size as i64;
			state.local_bytes.set(bytes);
			if bytes >= BATCH_THRESHOLD {
				state.flush_to_global();
			}
		});
		// Decrement the recursion depth
		RECURSION_DEPTH.set(RECURSION_DEPTH.get().saturating_sub(1));
	}

	/// Subtracts released bytes from this thread's batched byte delta.
	fn sub(size: usize) {
		// Track the current recursion depth
		let depth = RECURSION_DEPTH.with(|d| {
			let current = d.get();
			if current >= MAX_DEPTH {
				return MAX_DEPTH;
			}
			d.set(current + 1);
			current
		});
		// Don't recursively track too deep
		if depth >= MAX_DEPTH {
			return;
		}
		// Update the tracked byte count
		THREAD_STATE.with(|state| {
			let bytes = state.local_bytes.get() - size as i64;
			state.local_bytes.set(bytes);
			if bytes <= -BATCH_THRESHOLD {
				state.flush_to_global();
			}
		});
		// Decrement the recursion depth
		RECURSION_DEPTH.set(RECURSION_DEPTH.get().saturating_sub(1));
	}
}

// SAFETY: Allocation validity comes from `A`: pointers, layouts and results
// pass through unchanged to the same allocator. Tracking only updates
// thread-local cells and an atomic counter; it never accesses allocated data.
unsafe impl<A: GlobalAlloc> GlobalAlloc for TrackAlloc<A> {
	/// Allocates memory through the underlying allocator and updates allocation
	/// accounting.
	unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
		// SAFETY: The GlobalAlloc caller supplies a nonzero allocation layout;
		// this wrapper forwards that layout unchanged to its underlying
		// allocator.
		let ret = unsafe { self.alloc.alloc(layout) };
		if !ret.is_null() {
			Self::add(layout.size());
		}
		ret
	}

	/// Releases memory through the underlying allocator and updates allocation
	/// accounting.
	unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
		// SAFETY: The caller supplies a live allocation and its matching
		// layout. Both are forwarded unchanged to the allocator that
		// created the allocation.
		unsafe { self.alloc.dealloc(ptr, layout) };
		Self::sub(layout.size());
	}

	/// Allocates zero-initialized memory through the underlying allocator and
	/// updates allocation accounting.
	unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
		// SAFETY: The GlobalAlloc caller supplies a nonzero allocation layout;
		// forwarding it unchanged preserves the wrapped allocator's zeroing
		// contract.
		let ret = unsafe { self.alloc.alloc_zeroed(layout) };
		if !ret.is_null() {
			Self::add(layout.size());
		}
		ret
	}

	/// Resizes memory through the underlying allocator and updates allocation
	/// accounting.
	unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
		// SAFETY: The caller supplies a live allocation, its matching layout
		// and a new size satisfying GlobalAlloc's bounds. Forwarding
		// these arguments unchanged preserves allocator identity and
		// the original alignment.
		let ret = unsafe { self.alloc.realloc(ptr, layout, new_size) };
		if !ret.is_null() {
			Self::sub(layout.size());
			Self::add(new_size);
		}
		ret
	}
}
