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
	entry_bytes
		.checked_add(32)
		.and_then(|bytes| bytes.checked_mul(4))
		.and_then(|bytes| bytes.checked_mul(count))
		.and_then(|bytes| bytes.checked_add(4096))
		.ok_or_else(|| {
			crate::gql_status::FerrosGrynnError::operation_overflow("runtime tree allocation")
		})
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
