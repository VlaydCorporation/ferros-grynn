use super::LocalOptions;

/// Read-only snapshot of the effective local storage budgets and pressure
/// counters owned by one datastore.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct LocalStorageStatus {
	/// Validated options that own the local budgets.
	pub(crate) options: LocalOptions,
	/// Exact resident-frame budget.
	pub(crate) resident_capacity_pages: usize,
	/// Exact page versions currently resident.
	pub(crate) resident_pages: usize,
	/// Frames awaiting read-epoch reclamation.
	pub(crate) retired_resident_pages: usize,
	/// Shared cache-bypass transport budget.
	pub(crate) transient_capacity_pages: usize,
	/// Pages in either reusable scatter buffer.
	pub(crate) scatter_extent_pages: usize,
	/// Maximum frame-IoRing sparse window.
	pub(crate) sparse_window_pages: usize,
	/// Fixed arena metadata excluding dynamic bucket overhead.
	pub(crate) fixed_metadata_bytes: usize,
	/// Exact home source versions retained by locator ownership.
	pub(crate) home_source_versions: usize,
	/// Live page-to-frame locator entries.
	pub(crate) locator_entries: usize,
	/// Exact physical file registrations in the current generation.
	pub(crate) registered_files: usize,
	/// Registrations that currently own an open native positional handle.
	pub(crate) open_file_handles: usize,
	/// Cumulative resident misses.
	pub(crate) cache_misses: u64,
	/// Cumulative coalesced miss waiters.
	pub(crate) coalesced_misses: u64,
	/// Cumulative resident evictions.
	pub(crate) evictions: u64,
	/// Cumulative capacity waits.
	pub(crate) capacity_waits: u64,
	/// Committed pages not yet copied to home files.
	pub(crate) wal_unbackfilled_pages: usize,
	/// Replaced UUID directories awaiting safe deletion.
	pub(crate) pending_retired_generation_directories: usize,
	/// Cumulative post-publication removal failures.
	pub(crate) generation_cleanup_failures: u64,
}

impl LocalStorageStatus {
	/// Returns the validated options that own every effective local budget.
	pub const fn options(&self) -> LocalOptions {
		self.options
	}

	/// Returns the exact number of 16-KiB resident frames.
	pub const fn resident_capacity_pages(&self) -> usize {
		self.resident_capacity_pages
	}

	/// Returns the number of resident exact page versions.
	pub const fn resident_pages(&self) -> usize {
		self.resident_pages
	}

	/// Returns frames awaiting bounded read-epoch reclamation.
	pub const fn retired_resident_pages(&self) -> usize {
		self.retired_resident_pages
	}

	/// Returns the cache-bypassing transport budget in 16-KiB pages.
	pub const fn transient_capacity_pages(&self) -> usize {
		self.transient_capacity_pages
	}

	/// Returns the size of either reusable scatter buffer in pages.
	pub const fn scatter_extent_pages(&self) -> usize {
		self.scatter_extent_pages
	}

	/// Returns the maximum sparse transport window in pages.
	pub const fn sparse_window_pages(&self) -> usize {
		self.sparse_window_pages
	}

	/// Returns fixed arena metadata bytes, excluding allocator and hash-table
	/// bucket overhead.
	pub const fn fixed_metadata_bytes(&self) -> usize {
		self.fixed_metadata_bytes
	}

	/// Returns exact home-file source versions retained by locator ownership.
	pub const fn home_source_versions(&self) -> usize {
		self.home_source_versions
	}

	/// Returns live page-to-frame locator entries.
	pub const fn locator_entries(&self) -> usize {
		self.locator_entries
	}

	/// Returns physical files registered by the current catalog generation.
	pub const fn registered_files(&self) -> usize {
		self.registered_files
	}

	/// Returns registrations that currently own a native positional handle.
	pub const fn open_file_handles(&self) -> usize {
		self.open_file_handles
	}

	/// Returns cumulative cache misses.
	pub const fn cache_misses(&self) -> u64 {
		self.cache_misses
	}

	/// Returns cumulative callers coalesced onto an existing miss.
	pub const fn coalesced_misses(&self) -> u64 {
		self.coalesced_misses
	}

	/// Returns cumulative resident evictions.
	pub const fn evictions(&self) -> u64 {
		self.evictions
	}

	/// Returns cumulative waits caused by pinned or loading capacity.
	pub const fn capacity_waits(&self) -> u64 {
		self.capacity_waits
	}

	/// Returns committed pages not yet copied from WAL into home files.
	pub const fn wal_unbackfilled_pages(&self) -> usize {
		self.wal_unbackfilled_pages
	}

	/// Returns old UUID-generation directories awaiting snapshot release or a
	/// successful cleanup retry.
	pub const fn pending_retired_generation_directories(&self) -> usize {
		self.pending_retired_generation_directories
	}

	/// Returns cumulative post-publication directory-removal failures.
	pub const fn generation_cleanup_failures(&self) -> u64 {
		self.generation_cleanup_failures
	}
}
