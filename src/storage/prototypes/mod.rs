//! Test-only storage-engine prototypes for the stage 3.1 experiment.
//!
//! Nothing in this module is reachable from the production storage path.

mod atomic_arena;
mod harness;
mod policy;
mod prefetch;
#[cfg(all(windows, feature = "native-async-io"))]
mod windows_transport;
#[cfg(windows)]
mod windows_vm;

use super::{
	buffer_pool::{PageCacheKey, PageCacheVersion},
	types::{DataFileId, GraphId},
};

/// Stable identity of one prototype page source independent of its version.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct SourceIdentity {
	/// Graph address space that owns the file.
	graph_id: GraphId,
	/// Never-reused physical file identity.
	file_id: DataFileId,
}

impl SourceIdentity {
	/// Extracts source identity from an exact cache key.
	const fn from_key(key: PageCacheKey) -> Self {
		let page_key = key.page_key();
		Self {
			graph_id: page_key.graph_id(),
			file_id: page_key.file_id(),
		}
	}
}

/// Exact immutable epoch of one prototype page source.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct SourceEpoch(PageCacheVersion);

impl SourceEpoch {
	/// Extracts the source epoch from an exact cache key.
	const fn from_key(key: PageCacheKey) -> Self {
		Self(key.version())
	}
}
