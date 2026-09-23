//! Standalone storage-engine prototypes for the stage 3.1 experiment.
//!
//! Nothing in this crate is reachable from the production storage path.

#[cfg(test)]
mod atomic_arena;
#[cfg(test)]
mod harness;
#[cfg(test)]
mod hybrid_candidate;
#[cfg(test)]
mod policy;
#[cfg(test)]
mod prefetch;
#[cfg(all(test, windows, feature = "native-async-io"))]
mod windows_transport;
#[cfg(all(test, windows))]
mod windows_vm;

#[cfg(test)]
use ferros_grynn::storage_engine_prototype_api::{
	buffer_pool::{PageCacheKey, PageCacheVersion},
	types::DataFileId,
};

/// Stable identity of one prototype page source independent of its version.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg(test)]
struct SourceIdentity {
	/// Database-global or graph-local address space that owns the file.
	space_id: u64,
	/// Never-reused physical file identity.
	file_id: DataFileId,
}

#[cfg(test)]
impl SourceIdentity {
	/// Extracts source identity from an exact cache key.
	const fn from_key(key: PageCacheKey) -> Self {
		let page_key = key.page_key();
		Self {
			space_id: page_key.data_space_raw(),
			file_id: page_key.file_id(),
		}
	}
}

/// Exact immutable epoch of one prototype page source.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg(test)]
struct SourceEpoch(PageCacheVersion);

#[cfg(test)]
impl SourceEpoch {
	/// Extracts the source epoch from an exact cache key.
	const fn from_key(key: PageCacheKey) -> Self {
		Self(key.version())
	}
}
