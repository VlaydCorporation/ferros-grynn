/// Owned-heap estimates and reusable estimator caches.
mod measurable;
pub use measurable::{
	HeapEstimatorCache,
	Measurable,
	NO_HEAP_ESTIMATOR_CACHE_INSTANCE,
	NoHeapEstimatorCache,
};
/// Allocator forwarding when allocation tracking is disabled.
mod notrack;
/// Weak registration and aggregation of external memory reporters.
#[cfg(test)]
mod registry;
/// Optional allocator wrapper with thread-local batched byte tracking.
mod track;

#[cfg(test)]
pub use registry::{
	MemoryReporter,
	cleanup_memory_reporters,
	memory_reporters_allocated_by_name,
	memory_reporters_allocated_total,
	register_memory_reporter,
};
/// Flushes this test thread's pending allocation delta.
#[cfg(all(test, feature = "allocation-tracking"))]
pub use track::flush_local_allocations;

/// Returns allocator-tracked bytes, or zero when tracking is disabled.
#[cfg(test)]
pub fn allocated_bytes() -> usize {
	#[cfg(feature = "allocation-tracking")]
	{
		track::memory_allocated()
	}
	#[cfg(not(feature = "allocation-tracking"))]
	{
		0
	}
}

/// Reserves complete initial B-tree nodes and sparse node occupancy.
/// The fixed reserve covers root/split scratch; four slots per entry cover
/// current std node occupancy and pointer arrays.
///
/// # Errors
///
/// Returns typed overflow when the complete reserve is not addressable.
pub fn tree_allocation_bytes(
	count: usize,
	entry_bytes: usize,
) -> crate::gql_status::FerrosGrynnResult<usize> {
	if count == 0 {
		return Ok(0);
	}
	tree_slot_bytes(count, entry_bytes).and_then(|bytes| bytes.checked_add(4096)).ok_or_else(|| {
		crate::gql_status::FerrosGrynnError::operation_overflow("runtime tree allocation")
	})
}

/// Estimates retained tree storage separately from temporary split scratch.
/// Rust 1.98 B-trees hold eleven entries per node and at least five per
/// non-root node. Below eleven total entries only one retained leaf is
/// possible; an empty map may still own that leaf. Recheck this bound when
/// updating the toolchain. Entry is the key/value tuple, or the set element.
///
/// # Errors
///
/// Returns typed overflow when the retained storage is not addressable.
pub fn tree_retained_bytes<Entry>(count: usize) -> crate::gql_status::FerrosGrynnResult<usize> {
	let nodes = if count < 11 {
		1
	} else {
		(count - 1) / 5 + 1
	};
	// Parent metadata, twelve child pointers and two alignment boundaries.
	11_usize
		.checked_mul(size_of::<Entry>())
		.and_then(|bytes| bytes.checked_add(128))
		.and_then(|bytes| bytes.checked_add(align_of::<Entry>().checked_mul(2)?))
		.and_then(|bytes| bytes.checked_mul(nodes))
		.ok_or_else(|| {
			crate::gql_status::FerrosGrynnError::operation_overflow("runtime retained tree")
		})
}

/// Applies the shared conservative sparse-node slot and pointer allowance.
fn tree_slot_bytes(count: usize, entry_bytes: usize) -> Option<usize> {
	entry_bytes
		.checked_add(32)
		.and_then(|bytes| bytes.checked_mul(4))
		.and_then(|bytes| bytes.checked_mul(count))
}

#[cfg(all(
	feature = "allocation-tracking",
	any(unix, windows),
	not(all(
		any(target_arch = "x86_64", target_arch = "x86"),
		any(target_os = "linux", target_os = "macos"),
		not(target_env = "msvc"),
	)),
))]
#[global_allocator]
/// Mimalloc-backed global allocator with batched allocation tracking.
pub static ALLOC: track::TrackAlloc<mimalloc_pprof::MiMalloc> =
	track::TrackAlloc::new(mimalloc_pprof::MiMalloc);

#[cfg(all(
	feature = "allocation-tracking",
	all(
		any(target_arch = "x86_64", target_arch = "x86"),
		any(target_os = "linux", target_os = "macos"),
		not(target_env = "msvc"),
	)
))]
#[global_allocator]
/// Jemalloc-backed global allocator with batched allocation tracking.
pub static ALLOC: track::TrackAlloc<tikv_jemallocator::Jemalloc> =
	track::TrackAlloc::new(tikv_jemallocator::Jemalloc);

#[cfg(all(
	not(feature = "allocation-tracking"),
	any(unix, windows),
	not(all(
		any(target_arch = "x86_64", target_arch = "x86"),
		any(target_os = "linux", target_os = "macos"),
		not(target_env = "msvc"),
	))
))]
#[global_allocator]
/// Mimalloc-backed global allocator without allocation tracking.
pub static ALLOC: notrack::NotrackAlloc<mimalloc_pprof::MiMalloc> =
	notrack::NotrackAlloc::new(mimalloc_pprof::MiMalloc);

#[cfg(all(
	not(feature = "allocation-tracking"),
	all(
		any(target_arch = "x86_64", target_arch = "x86"),
		any(target_os = "linux", target_os = "macos"),
		not(target_env = "msvc"),
	)
))]
#[global_allocator]
/// Jemalloc-backed global allocator without allocation tracking.
pub static ALLOC: notrack::NotrackAlloc<tikv_jemallocator::Jemalloc> =
	notrack::NotrackAlloc::new(tikv_jemallocator::Jemalloc);

/// Contract checks for the disabled-tracking observation path.
#[cfg(test)]
#[cfg(not(feature = "allocation-tracking"))]
mod tests {
	/// A normal allocator build must not report estimated or process memory as
	/// tracked bytes.
	#[test]
	fn inspection_reports_zero_without_tracking() {
		assert_eq!(super::allocated_bytes(), 0);
	}
}
