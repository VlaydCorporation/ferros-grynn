mod measurable;
mod notrack;
mod registry;
mod track;

pub use registry::{
	MemoryReporter,
	cleanup_memory_reporters,
	memory_reporters_allocated_by_name,
	memory_reporters_allocated_total,
	register_memory_reporter,
};

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
pub static ALLOC: notrack::NotrackAlloc<tikv_jemallocator::Jemalloc> =
	notrack::NotrackAlloc::new(tikv_jemallocator::Jemalloc);
