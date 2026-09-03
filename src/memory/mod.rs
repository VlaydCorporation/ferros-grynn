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
